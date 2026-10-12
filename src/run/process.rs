//! Running an [Invocation]: a child process with stdin closed and its
//! output captured, killed if it outlives its timeout.
//!
//! On Unix the child leads a process group of its own, so that killing a
//! timed-out run kills what it spawned too (cargo's test binaries, say).
//! std can only signal the direct child, and the crate forbids
//! `unsafe_code`, so the group is signalled through `rustix`'s safe
//! `kill_process_group`. Elsewhere only the direct child is killed.
//!
//! A group of its own is also outside the terminal's foreground group, so
//! Ctrl-C no longer reaches the child. Dropping a run kills its group, but
//! a process killed by SIGINT drops nothing: whatever runs validators must
//! catch Ctrl-C and drop its runs before it exits.

use std::{
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use derive_getters::Getters;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinHandle,
    time,
};

use super::{Invocation, SpawnFailure};

/// How much of each output stream a [Run] keeps: its last 64 KiB.
const KEPT: usize = 64 * 1024;

/// How long, once the child has exited, to wait for its output streams to
/// close. Something the child left running may hold them open; slcr stops
/// reading then rather than wait for it.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// What one run of a validator came to.
#[derive(Clone, Debug, PartialEq, Eq, Getters)]
pub(crate) struct Run {
    outcome: Outcome,
    /// The exit code, if the child exited with one before any timeout.
    exit_code: Option<i32>,
    stdout: Captured,
    stderr: Captured,
    /// From starting the child to its exit.
    duration: Duration,
}

/// Whether the validator passed. Every run that starts is one or the
/// other: anything slcr can't read as a pass is a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Green,
    Red(Red),
}

/// Why a validator is red.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Red {
    /// It exited with a code its descriptor's `results.pass` doesn't list.
    ExitCode(i32),
    /// It was terminated without an exit code: by a signal, with its
    /// number where the platform reports one, such as on a crash.
    Terminated { signal: Option<i32> },
    /// It ran past its descriptor's `timeout`, and slcr killed it.
    TimedOut,
    /// slcr lost track of the child, so it fails closed.
    Lost { reason: String },
}

/// The end of one of a run's output streams.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Captured {
    /// Never more than [KEPT] bytes once read; more is trimmed lazily.
    tail: Vec<u8>,
    total: u64,
    open: bool,
}

impl Captured {
    /// The last bytes written, at most 64 KiB of them.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.tail[self.tail.len().saturating_sub(KEPT)..]
    }

    /// How many bytes were written in all.
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// Whether earlier bytes were dropped to keep only the last 64 KiB.
    pub(crate) fn truncated(&self) -> bool {
        self.total > self.bytes().len() as u64
    }

    /// Whether the stream was still open when slcr stopped reading it,
    /// because something the child started outlived it.
    pub(crate) fn cut_off(&self) -> bool {
        self.open
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        self.tail.extend_from_slice(chunk);
        // Trim only once there's as much again to drop, so trimming takes
        // amortized constant time per byte.
        if self.tail.len() >= 2 * KEPT {
            self.tail.drain(..self.tail.len() - KEPT);
        }
    }
}

impl Invocation {
    /// Run the validator, and read its outcome.
    ///
    /// This must be called within a tokio runtime, whose time and I/O
    /// drivers are enabled, and it is `Send`, so that `check` can run many
    /// validators at once, as tasks, each with its own timeout. Dropping
    /// the future kills the run.
    pub(crate) async fn run(&self) -> Result<Run, SpawnFailure> {
        let mut command = Command::new(&self.executable);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .envs(self.env.iter().map(|(name, value)| (name.as_str(), value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let started = Instant::now();
        let mut child = command.spawn().map_err(|source| SpawnFailure::Os {
            program: self.program.as_str().to_owned(),
            executable: self.executable.clone(),
            source,
        })?;
        let mut group = Group::led_by(&child);
        let stdout = Stream::read(child.stdout.take());
        let stderr = Stream::read(child.stderr.take());

        let waited = match self.timeout {
            None => Some(child.wait().await),
            Some(timeout) => time::timeout(timeout, child.wait()).await.ok(),
        };
        let (status, timed_out) = match waited {
            Some(status) => (status, false),
            None => {
                group.kill();
                // Where there are no groups to kill, this is what kills
                // the child.
                let _ = child.start_kill();
                (child.wait().await, true)
            }
        };
        // Reaped, or lost track of: either way the group's ID may no
        // longer be the child's.
        group.reaped();
        let duration = started.elapsed();

        let (outcome, exit_code) = match status {
            _ if timed_out => (Outcome::Red(Red::TimedOut), None),
            Ok(status) => (self.outcome(status), status.code()),
            Err(err) => (
                Outcome::Red(Red::Lost {
                    reason: err.to_string(),
                }),
                None,
            ),
        };
        let (stdout, stderr) = drain(stdout, stderr).await;
        Ok(Run {
            outcome,
            exit_code,
            stdout,
            stderr,
            duration,
        })
    }

    /// What `status`, from a child that exited before any timeout, means.
    fn outcome(&self, status: ExitStatus) -> Outcome {
        match status.code() {
            Some(code) if u8::try_from(code).is_ok_and(|code| self.pass.contains(&code)) => {
                Outcome::Green
            }
            Some(code) => Outcome::Red(Red::ExitCode(code)),
            None => Outcome::Red(Red::Terminated {
                signal: signal(status),
            }),
        }
    }
}

#[cfg(unix)]
fn signal(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
fn signal(_status: ExitStatus) -> Option<i32> {
    None
}

/// One of the child's output streams, read into a [Captured] as it comes,
/// so that a child that writes a lot never blocks on a full pipe.
struct Stream {
    captured: Arc<Mutex<Captured>>,
    reader: JoinHandle<()>,
}

impl Stream {
    fn read<R: AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Self {
        let captured = Arc::new(Mutex::new(Captured {
            open: true,
            ..Captured::default()
        }));
        let shared = Arc::clone(&captured);
        let reader = tokio::spawn(async move {
            if let Some(mut pipe) = pipe {
                let mut buffer = vec![0; 8 * 1024];
                // A read error ends the stream as end of file does.
                while let Ok(read @ 1..) = pipe.read(&mut buffer).await {
                    lock(&shared).push(&buffer[..read]);
                }
            }
            lock(&shared).open = false;
        });
        Self { captured, reader }
    }

    /// What was read, once reading has stopped.
    fn finish(self) -> Captured {
        self.reader.abort();
        std::mem::take(&mut *lock(&self.captured))
    }
}

fn lock(captured: &Mutex<Captured>) -> std::sync::MutexGuard<'_, Captured> {
    captured.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Both streams, read to their ends, or as far as they go in
/// [DRAIN_GRACE].
async fn drain(mut stdout: Stream, mut stderr: Stream) -> (Captured, Captured) {
    let _ = time::timeout(DRAIN_GRACE, async {
        let _ = (&mut stdout.reader).await;
        let _ = (&mut stderr.reader).await;
    })
    .await;
    (stdout.finish(), stderr.finish())
}

/// The child's process group, killed when the run ends early. slcr kills
/// it only while the child, its leader, is unreaped: until then the
/// group's ID can't name any other group.
struct Group {
    #[cfg(unix)]
    leader: Option<rustix::process::Pid>,
}

impl Group {
    fn led_by(child: &Child) -> Self {
        #[cfg(unix)]
        let group = Self {
            leader: child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(rustix::process::Pid::from_raw),
        };
        #[cfg(not(unix))]
        let group = {
            let _ = child;
            Self {}
        };
        group
    }

    /// Kill every process in the group. The child itself is reaped
    /// separately.
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(leader) = self.leader {
            // The group may already be gone; there's nothing more to do
            // either way.
            let _ = rustix::process::kill_process_group(leader, rustix::process::Signal::KILL);
        }
    }

    /// The leader has been reaped, so its ID is free for reuse.
    fn reaped(&mut self) {
        #[cfg(unix)]
        {
            self.leader = None;
        }
    }
}

impl Drop for Group {
    /// A run dropped before its child was reaped, such as when `check` is
    /// interrupted, leaves nothing behind.
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::*;
    use crate::cli::block_on;
    use crate::run::tests::{Project, descriptor};

    /// Run `script` with `sh -c` in a fresh project, with `invoke` and
    /// `results` lines added to the descriptor.
    fn sh(script: &str, invoke: &str, results: &str) -> Run {
        let project = Project::new();
        sh_in(&project, script, invoke, results)
    }

    fn sh_in(project: &Project, script: &str, invoke: &str, results: &str) -> Run {
        let descriptor = descriptor(r#"["sh", "-c", "{with.script}"]"#, invoke, results);
        run(project, &descriptor, json!({ "script": script }))
    }

    fn run(
        project: &Project,
        descriptor: &crate::tools::descriptor::Descriptor,
        with: Value,
    ) -> Run {
        let invocation = project.invocation(descriptor, &with).unwrap();
        block_on(invocation.run()).unwrap().unwrap()
    }

    fn text(captured: &Captured) -> &str {
        std::str::from_utf8(captured.bytes()).unwrap()
    }

    #[test]
    fn exit_zero_is_green() {
        let run = sh("exit 0", "", "");
        assert_eq!(*run.outcome(), Outcome::Green);
        assert_eq!(*run.exit_code(), Some(0));
    }

    #[test]
    fn any_other_exit_code_is_red() {
        let run = sh("exit 1", "", "");
        assert_eq!(*run.outcome(), Outcome::Red(Red::ExitCode(1)));
        assert_eq!(*run.exit_code(), Some(1));
    }

    #[test]
    fn results_pass_lists_the_green_codes() {
        let pass = "pass = [0, 3]";
        assert_eq!(*sh("exit 3", "", pass).outcome(), Outcome::Green);
        assert_eq!(*sh("exit 0", "", pass).outcome(), Outcome::Green);
        assert_eq!(
            *sh("exit 2", "", pass).outcome(),
            Outcome::Red(Red::ExitCode(2))
        );
        assert_eq!(
            *sh("exit 0", "", "pass = [1]").outcome(),
            Outcome::Red(Red::ExitCode(0))
        );
    }

    #[test]
    fn a_signal_is_red() {
        let run = sh("kill -TERM $$", "", "");
        assert_eq!(
            *run.outcome(),
            Outcome::Red(Red::Terminated { signal: Some(15) })
        );
        assert_eq!(*run.exit_code(), None);
    }

    #[test]
    fn stdin_is_closed() {
        // `cat` would wait for input on an open stdin.
        let project = Project::new();
        let run = run(
            &project,
            &descriptor(r#"["cat"]"#, "timeout = 10", ""),
            json!({}),
        );
        assert_eq!(*run.outcome(), Outcome::Green);
        assert_eq!(run.stdout().total(), 0);
    }

    #[test]
    fn the_descriptors_env_is_set() {
        let run = sh(
            r#"test "$GREETING" = "hello there" && test -n "$HOME""#,
            r#"env = { GREETING = "hello there" }"#,
            "",
        );
        assert_eq!(*run.outcome(), Outcome::Green);
    }

    #[test]
    fn the_child_runs_in_its_cwd() {
        let project = Project::new();
        let run = sh_in(&project, "pwd -P", "", "");
        let cwd = project.root().canonicalize().unwrap();
        assert_eq!(Path::new(text(run.stdout()).trim_end()), cwd);
    }

    #[test]
    fn output_is_captured() {
        let run = sh("echo out; echo err >&2; exit 4", "", "");
        assert_eq!(text(run.stdout()), "out\n");
        assert_eq!(text(run.stderr()), "err\n");
        assert!(!run.stdout().truncated());
        assert!(!run.stdout().cut_off());
        assert_eq!(*run.outcome(), Outcome::Red(Red::ExitCode(4)));
    }

    #[test]
    fn only_the_end_of_long_output_is_kept() {
        // About 600 KB on each stream at once, which would fill a pipe that
        // isn't read concurrently.
        let run = sh("seq 1 100000; seq 1 100000 >&2", "timeout = 30", "");
        assert_eq!(*run.outcome(), Outcome::Green);
        for captured in [run.stdout(), run.stderr()] {
            assert_eq!(captured.total(), 588_895);
            assert_eq!(captured.bytes().len(), KEPT);
            assert!(captured.truncated());
            assert!(text(captured).ends_with("\n99999\n100000\n"));
        }
    }

    #[test]
    fn a_timeout_kills_the_run() {
        let started = Instant::now();
        let run = sh("echo started; sleep 30", "timeout = 1", "");
        assert_eq!(*run.outcome(), Outcome::Red(Red::TimedOut));
        assert_eq!(*run.exit_code(), None);
        assert_eq!(text(run.stdout()), "started\n");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(*run.duration() >= Duration::from_secs(1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timeout_kills_what_the_child_spawned() {
        let project = Project::new();
        let pid_file = project.root().join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let started = Instant::now();
        let run = sh_in(&project, &script, "timeout = 1", "");
        assert_eq!(*run.outcome(), Outcome::Red(Red::TimedOut));
        // Had the grandchild survived, it would have held the output open
        // for the whole drain grace period.
        assert!(!run.stdout().cut_off());
        assert!(started.elapsed() < Duration::from_secs(1) + DRAIN_GRACE);

        let pid = std::fs::read_to_string(pid_file).unwrap();
        let stat = Path::new("/proc").join(pid.trim()).join("stat");
        // Killed, it is gone, or a zombie until something reaps it.
        let dead = || {
            std::fs::read_to_string(&stat)
                .map_or(true, |stat| matches!(state(&stat), Some('Z' | 'X')))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dead() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(dead(), "the grandchild, {}, is still running", pid.trim());
    }

    /// The state field of a `/proc/<pid>/stat` line, which follows the
    /// parenthesized command name.
    #[cfg(target_os = "linux")]
    fn state(stat: &str) -> Option<char> {
        stat.rsplit_once(") ")?.1.chars().next()
    }

    #[test]
    fn output_left_open_by_a_lingering_process_is_cut_off() {
        let started = Instant::now();
        // The background `sleep` keeps stdout open after `sh` exits.
        let run = sh("echo done; sleep 30 &", "", "");
        assert_eq!(*run.outcome(), Outcome::Green);
        assert_eq!(text(run.stdout()), "done\n");
        assert!(run.stdout().cut_off());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_program_the_os_refuses_is_a_spawn_failure() {
        use std::os::unix::fs::PermissionsExt;

        let project = Project::new();
        let bin = project.root().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // Executable, but in no format the kernel can run.
        let tool = bin.join("broken-tool");
        std::fs::write(&tool, [0x7f, b'E', b'L', b'F', 0, 0, 0, 0]).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let descriptor = descriptor(
            r#"["broken-tool"]"#,
            &format!("env = {{ PATH = {:?} }}", bin.to_str().unwrap()),
            "",
        );
        let invocation = project.invocation(&descriptor, &json!({})).unwrap();
        let err = block_on(invocation.run()).unwrap().unwrap_err();
        assert!(matches!(err, SpawnFailure::Os { .. }), "{err:?}");
        assert!(
            err.to_string()
                .starts_with("couldn't start `broken-tool`: ")
        );
    }

    #[test]
    fn runs_are_send_so_they_can_run_as_tasks() {
        fn assert_send<T: Send>(_: &T) {}
        let project = Project::new();
        let descriptor = descriptor(r#"["true"]"#, "", "");
        let invocation = project.invocation(&descriptor, &json!({})).unwrap();
        assert_send(&invocation.run());

        // Several at once, as `check` will run them.
        let outcomes = block_on(async {
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    let invocation = invocation.clone();
                    tokio::spawn(async move { invocation.run().await.unwrap() })
                })
                .collect();
            let mut outcomes = Vec::new();
            for task in tasks {
                outcomes.push(task.await.unwrap().outcome().clone());
            }
            outcomes
        })
        .unwrap();
        assert_eq!(outcomes, vec![Outcome::Green; 4]);
    }

    #[test]
    fn captured_output_keeps_the_last_bytes() {
        let mut captured = Captured::default();
        for _ in 0..5 {
            captured.push(&[b'a'; KEPT / 2]);
        }
        captured.push(b"end");
        assert_eq!(captured.total(), (5 * (KEPT / 2) + 3) as u64);
        assert_eq!(captured.bytes().len(), KEPT);
        assert!(captured.bytes().ends_with(b"aend"));
        assert!(captured.truncated());
        assert!(captured.tail.len() < 2 * KEPT);
    }
}
