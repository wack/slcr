//! Running every validator of a project that vetted cleanly, at most `-j`
//! at a time, until they have all finished or Ctrl-C interrupts them.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    sync::Arc,
    task::Poll,
};

use serde_json::Map;
use tokio::task::JoinSet;

use crate::project::Layout;
use crate::run::process::{Groups, GroupsGuard, Run};
use crate::run::{Invocation, SpawnFailure, Target};
use crate::tools::{ToolKey, descriptor::Descriptor};
use crate::validators::{ValidatorFile, file::Validator};

/// How running one validator ended: green or red, or why it couldn't be
/// run.
pub(crate) type Finished = Result<Run, SpawnFailure>;

/// Every validator of a project that vetted cleanly, in the order `check`
/// reports them: by validator file, in the order of their paths, then by
/// position in the file.
#[derive(Debug)]
pub(crate) struct Plan {
    /// Where the project is: the validator files are in its SLCR
    /// directory, and the validators run against its project root.
    layout: Layout,
    files: Vec<ValidatorFile>,
    /// The descriptor of every tool a validator names.
    descriptors: BTreeMap<ToolKey, Descriptor>,
    /// Each validator, as the index of its file and its index in the file.
    order: Vec<(usize, usize)>,
    /// The `PATH` tools are found on and validators inherit, in place of
    /// the process's own.
    path: Option<OsString>,
}

/// One validator of a [Plan].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Job<'a> {
    pub(crate) file: &'a ValidatorFile,
    /// The validator's position in the file, from 1, as `vet` numbers it.
    pub(crate) position: usize,
    pub(crate) validator: &'a Validator,
}

impl Plan {
    /// Every validator in `files`. Vetting guarantees `descriptors` holds
    /// the descriptor of each one's tool.
    pub(crate) fn new(
        layout: Layout,
        files: Vec<ValidatorFile>,
        descriptors: BTreeMap<ToolKey, Descriptor>,
        path: Option<OsString>,
    ) -> Self {
        let order = files
            .iter()
            .enumerate()
            .flat_map(|(file, loaded)| (0..loaded.validators().len()).map(move |i| (file, i)))
            .collect();
        Self {
            layout,
            files,
            descriptors,
            order,
            path,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    /// The validator at `index` in the plan's order.
    pub(crate) fn job(&self, index: usize) -> Job<'_> {
        let (file, validator) = self.order[index];
        let file = &self.files[file];
        Job {
            file,
            position: validator + 1,
            validator: &file.validators()[validator],
        }
    }

    /// Every validator, in the plan's order.
    pub(crate) fn jobs(&self) -> impl Iterator<Item = Job<'_>> {
        (0..self.len()).map(|index| self.job(index))
    }

    /// Resolve the validator at `index` against slcr's environment as it
    /// is now, which is when `PATH` is searched, then run it.
    async fn run(&self, index: usize, groups: &Groups) -> Finished {
        self.invocation(index)?.run_in(groups).await
    }

    /// Resolve the validator at `index` against the project root and slcr's
    /// environment as it is now.
    fn invocation(&self, index: usize) -> Result<Invocation, SpawnFailure> {
        let job = self.job(index);
        let descriptor = self
            .descriptors
            .get(job.validator.tool())
            .expect("vetting guarantees a descriptor for every validator's tool");
        let empty = Map::new();
        Invocation::new(Target {
            descriptor,
            with: job.validator.with().as_ref().unwrap_or(&empty),
            project_root: self.layout.project_root(),
            validator_dir: &self.layout.validator_dir(job.file.file()),
            requirement: job.validator.requirement(),
            path: self.path.as_deref(),
        })
    }
}

/// What [run_all] came to.
#[derive(Debug)]
pub(crate) struct Ran {
    /// Each validator's result, in the plan's order: `None` for one that
    /// hadn't finished when the run was interrupted.
    pub(crate) results: Vec<Option<Finished>>,
    pub(crate) interrupted: bool,
}

/// Run every validator of `plan`, at most `jobs` at once, starting them in
/// the plan's order.
///
/// `report` is called once for each validator, in the plan's order
/// whatever order they finish in, as soon as it and every validator before
/// it have finished. If `interrupt` completes first, every run still going
/// is dropped, which kills its process group, and `report` is called for
/// each validator not yet reported, with `None` for those that hadn't
/// finished. Dropping the future before it completes kills the process
/// group of every run still going, synchronously, whether or not the
/// runtime ever polls their tasks again.
pub(crate) async fn run_all<E>(
    plan: Arc<Plan>,
    jobs: NonZeroUsize,
    interrupt: impl Future<Output = ()>,
    mut report: impl FnMut(usize, Option<&Finished>) -> Result<(), E>,
) -> Result<Ran, E> {
    // Dropping this future drops it, killing every validator's process
    // group still alive at once: the tasks running them are only told to
    // abort, and may never be polled again.
    let guard = GroupsGuard::default();
    let mut interrupt = pin!(interrupt);
    let mut results: Vec<Option<Finished>> = (0..plan.len()).map(|_| None).collect();
    let mut running = JoinSet::new();
    let (mut started, mut reported) = (0, 0);
    // Polling it once, before anything starts, is what installs a Ctrl-C
    // handler: without one, Ctrl-C would kill slcr and leave the
    // validators, in process groups of their own, running.
    let mut interrupted = poll_fn(|cx| Poll::Ready(interrupt.as_mut().poll(cx).is_ready())).await;
    while !interrupted {
        while started < plan.len() && running.len() < jobs.get() {
            let plan = Arc::clone(&plan);
            let index = started;
            let groups = guard.groups().clone();
            running.spawn(async move { (index, plan.run(index, &groups).await) });
            started += 1;
        }
        let next = poll_fn(|cx| {
            if interrupt.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            running.poll_join_next(cx).map(Some)
        })
        .await;
        match next {
            None => interrupted = true,
            // Nothing is running, so nothing is left to start.
            Some(None) => break,
            Some(Some(joined)) => {
                let (index, finished) =
                    joined.unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()));
                results[index] = Some(finished);
                while let Some(Some(finished)) = results.get(reported) {
                    report(reported, Some(finished))?;
                    reported += 1;
                }
            }
        }
    }
    if interrupted {
        // Abort every run, and wait until each has been dropped.
        running.shutdown().await;
        for (index, finished) in results.iter().enumerate().skip(reported) {
            report(index, finished.as_ref())?;
        }
    }
    Ok(Ran {
        results,
        interrupted,
    })
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use std::{convert::Infallible, ffi::OsString, path::Path};

    use tempfile::TempDir;

    use super::*;
    use crate::cli::block_on;
    use crate::project::ProjectFile;
    use crate::run::process::Outcome;

    /// A descriptor, `sh`, that runs `{with.script}` with `sh -c` in the
    /// project root.
    pub(crate) const SH: &str = r#"formatVersion = "1"
name = "sh"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["script"]
properties.script = { type = "string" }

[invoke]
argv = ["sh", "-c", "{with.script}"]
cwd = "project"
timeout = 30
"#;

    /// A project in a temporary directory whose validator file, at
    /// `.validators.slcr.toml`, runs each of `scripts` with [SH], against
    /// `todo-api/REQ-001`.
    pub(crate) fn plan(scripts: &[&str]) -> (TempDir, Plan) {
        let dir = TempDir::new().unwrap();
        let validators: String = scripts
            .iter()
            .map(|script| {
                format!(
                    "\n[[validator]]\nrequirement = \"todo-api/REQ-001\"\ntool = \"sh\"\n\
                     with = {{ script = {script:?} }}\n"
                )
            })
            .collect();
        let relative = Path::new(".validators.slcr.toml");
        std::fs::write(
            dir.path().join(relative),
            format!("formatVersion = \"1\"\n{validators}"),
        )
        .unwrap();
        let file = ProjectFile::new(dir.path(), dir.path(), relative.to_path_buf());
        let file = ValidatorFile::load(file).unwrap();
        let descriptor = Descriptor::parse("sh.toml", SH).unwrap();
        let key = ToolKey::try_from("sh".to_owned()).unwrap();
        let plan = Plan::new(
            Layout::single(dir.path().to_path_buf()),
            vec![file],
            BTreeMap::from([(key, descriptor)]),
            None,
        );
        (dir, plan)
    }

    #[test]
    fn validators_run_against_the_project_root_not_the_slcr_directory() {
        let dir = TempDir::new().unwrap();
        let (slcr_dir, repo) = (dir.path().join("app"), dir.path().join("repo"));
        std::fs::create_dir_all(slcr_dir.join("src")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        // Only the project root's Cargo.toml is in a `nearest` search.
        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        std::fs::write(slcr_dir.join("src/Cargo.toml"), "").unwrap();
        let tools = ["in-validator-dir", "in-project", "in-nearest"];
        // A validator file at the top of the SLCR directory, and one in
        // `src`, each running every tool.
        let files = [".validators.slcr.toml", "src/.validators.slcr.toml"].map(|relative| {
            let validators: String = tools
                .iter()
                .map(|tool| {
                    format!(
                        "\n[[validator]]\nrequirement = \"todo-api/REQ-001\"\ntool = \"{tool}\"\n"
                    )
                })
                .collect();
            std::fs::write(
                slcr_dir.join(relative),
                format!("formatVersion = \"1\"\n{validators}"),
            )
            .unwrap();
            let file = ProjectFile::new(&slcr_dir, dir.path(), relative.into());
            ValidatorFile::load(file).unwrap()
        });
        let descriptors = tools
            .iter()
            .zip([
                "\"validator-dir\"",
                "\"project\"",
                "{ nearest = \"Cargo.toml\" }",
            ])
            .map(|(tool, cwd)| {
                let source = format!(
                    "formatVersion = \"1\"\nname = \"{tool}\"\nversion = \"1.0.0\"\n\n\
                     [validator.schema]\ntype = \"object\"\n\n[invoke]\n\
                     argv = [\"sh\", \"{{slcr.project_root}}\", \"{{slcr.validator_dir}}\"]\n\
                     cwd = {cwd}\n"
                );
                let descriptor = Descriptor::parse("tool.toml", &source).unwrap();
                (ToolKey::try_from((*tool).to_owned()).unwrap(), descriptor)
            })
            .collect();
        let plan = Plan::new(
            Layout::new(slcr_dir, repo.clone()),
            files.into(),
            descriptors,
            None,
        );
        // Each argument and working directory, spelled exactly: `Path`'s
        // `==` would ignore a trailing separator.
        let spelled: Vec<(Vec<OsString>, OsString)> = (0..plan.len())
            .map(|i| {
                let invocation = plan.invocation(i).unwrap();
                let cwd = invocation.cwd().clone().into_os_string();
                (invocation.args().clone(), cwd)
            })
            .collect();
        let (root, src) = (
            repo.into_os_string(),
            dir.path().join("repo/src").into_os_string(),
        );
        let top = vec![root.clone(), root.clone()];
        let nested = vec![root.clone(), src.clone()];
        assert_eq!(
            spelled,
            [
                (top.clone(), root.clone()),
                (top.clone(), root.clone()),
                (top, root.clone()),
                (nested.clone(), src),
                (nested.clone(), root.clone()),
                (nested, root),
            ]
        );
    }

    /// Run `plan` `jobs` at a time until `interrupt`; also return the
    /// indices `report` was called with, in order.
    fn run(
        plan: Plan,
        jobs: usize,
        interrupt: impl Future<Output = ()>,
    ) -> (Ran, Vec<(usize, bool)>) {
        let mut reported = Vec::new();
        let ran = block_on(run_all(
            Arc::new(plan),
            NonZeroUsize::new(jobs).unwrap(),
            interrupt,
            |index, finished| {
                reported.push((index, finished.is_some()));
                Ok::<_, Infallible>(())
            },
        ))
        .unwrap()
        .unwrap();
        (ran, reported)
    }

    fn never() -> impl Future<Output = ()> {
        std::future::pending()
    }

    /// Each result's outcome: `Ok(green)`, or `Err` for a spawn failure.
    fn outcomes(ran: &Ran) -> Vec<Option<Result<bool, String>>> {
        ran.results
            .iter()
            .map(|finished| {
                finished.as_ref().map(|finished| match finished {
                    Ok(run) => Ok(*run.outcome() == Outcome::Green),
                    Err(failure) => Err(failure.to_string()),
                })
            })
            .collect()
    }

    #[test]
    fn results_are_reported_in_order_whatever_order_they_finish_in() {
        let (_dir, plan) = plan(&["sleep 0.5", "exit 1", "sleep 0.2", "true"]);
        let (ran, reported) = run(plan, 4, never());
        assert!(!ran.interrupted);
        assert_eq!(
            outcomes(&ran),
            [
                Some(Ok(true)),
                Some(Ok(false)),
                Some(Ok(true)),
                Some(Ok(true))
            ]
        );
        assert_eq!(reported, [(0, true), (1, true), (2, true), (3, true)]);
    }

    #[test]
    fn one_job_runs_the_validators_one_at_a_time_in_order() {
        // Each fails if another holds the lock, and logs its start.
        let scripts: Vec<String> = (1..=4)
            .map(|n| format!("mkdir lock || exit 9; echo {n} >> log; sleep 0.1; rmdir lock"))
            .collect();
        let scripts: Vec<&str> = scripts.iter().map(String::as_str).collect();
        let (dir, plan) = plan(&scripts);
        let (ran, _) = run(plan, 1, never());
        assert_eq!(outcomes(&ran), vec![Some(Ok(true)); 4]);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert_eq!(log, "1\n2\n3\n4\n");
    }

    #[test]
    fn jobs_run_at_once_up_to_the_limit() {
        // Each waits, for up to 10 s, until all four have started.
        let script = "touch started.$$; i=0; while [ $(ls | grep -c '^started') -lt 4 ]; do \
                      i=$((i+1)); [ $i -gt 200 ] && exit 3; sleep 0.05; done";
        let (_dir, plan) = plan(&[script; 4]);
        let (ran, _) = run(plan, 4, never());
        assert_eq!(outcomes(&ran), vec![Some(Ok(true)); 4]);
    }

    #[test]
    fn a_validator_that_cannot_be_run_has_its_spawn_failure() {
        let (_dir, mut plan) = plan(&["true", "true"]);
        let key = ToolKey::try_from("sh".to_owned()).unwrap();
        let missing = SH.replace("[\"sh\", ", "[\"slcr-no-such-program\", ");
        plan.descriptors
            .insert(key, Descriptor::parse("sh.toml", &missing).unwrap());
        let (ran, reported) = run(plan, 2, never());
        let failure = Err("`slcr-no-such-program` was not found on PATH".to_owned());
        assert_eq!(outcomes(&ran), [Some(failure.clone()), Some(failure)]);
        assert_eq!(reported, [(0, true), (1, true)]);
    }

    #[test]
    fn nothing_to_run_finishes_at_once() {
        let (_dir, mut plan) = plan(&["true"]);
        plan.order.clear();
        let (ran, reported) = run(plan, 1, never());
        assert!(ran.results.is_empty() && reported.is_empty() && !ran.interrupted);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_interrupt_kills_every_run_still_going() {
        use std::time::{Duration, Instant};

        // One at a time: the first finishes; the second starts a
        // grandchild, records its PID, and outlives the interrupt; the
        // third never starts.
        let (dir, plan) = plan(&[
            "true",
            "sleep 30 & echo $! > pid.tmp; mv pid.tmp pid; wait",
            "touch third",
        ]);
        let pid_file = dir.path().join("pid");
        let interrupt = {
            let pid_file = pid_file.clone();
            async move {
                while !pid_file.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let started = Instant::now();
        let (ran, reported) = run(plan, 1, interrupt);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(ran.interrupted);
        assert_eq!(outcomes(&ran)[1..], [None, None]);
        assert_eq!(reported, [(0, true), (1, false), (2, false)]);
        assert!(!dir.path().join("third").exists());

        let pid = std::fs::read_to_string(pid_file).unwrap();
        let stat = Path::new("/proc").join(pid.trim()).join("stat");
        // Killed, it is gone, or a zombie until something reaps it.
        let dead = || {
            std::fs::read_to_string(&stat).map_or(true, |stat| {
                matches!(
                    stat.rsplit_once(") ")
                        .and_then(|(_, rest)| rest.chars().next()),
                    Some('Z' | 'X')
                )
            })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dead() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(dead(), "the grandchild, {}, is still running", pid.trim());
    }

    /// Whether the process `pid` is gone, or a zombie until something
    /// reaps it.
    fn dead(pid: &str) -> bool {
        let ps = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&ps.stdout);
        matches!(state.trim_start().chars().next(), None | Some('Z' | 'X'))
    }

    #[test]
    fn dropping_the_run_kills_every_validator_at_once() {
        use std::time::{Duration, Instant};

        let (dir, plan) = plan(&["sleep 30 & echo $! > pid.tmp; mv pid.tmp pid; wait"]);
        // One thread, which polls nothing once `block_on` returns.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pid_file = dir.path().join("pid");
        let dropped = runtime.block_on(async {
            let run = run_all(Arc::new(plan), NonZeroUsize::MIN, never(), |_, _| {
                Ok::<_, Infallible>(())
            });
            let mut run = std::pin::pin!(run);
            // Run until the grandchild has started, then drop the run.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !pid_file.exists() && Instant::now() < deadline {
                let _ = tokio::time::timeout(Duration::from_millis(20), run.as_mut()).await;
            }
            pid_file.exists()
        });
        assert!(dropped, "the validator never started");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        // The runtime is still alive, and idle: its tasks are never polled.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dead(&pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            dead(&pid),
            "the grandchild, {}, is still running",
            pid.trim()
        );
        drop(runtime);
    }

    #[test]
    fn an_interrupt_before_anything_starts_runs_nothing() {
        let (dir, plan) = plan(&["touch ran"]);
        let (ran, reported) = run(plan, 1, async {});
        assert!(ran.interrupted);
        assert_eq!(reported, [(0, false)]);
        assert!(!dir.path().join("ran").exists());
    }
}
