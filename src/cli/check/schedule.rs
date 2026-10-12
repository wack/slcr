//! Running every validator of a project that vetted cleanly, at most `-j`
//! at a time, until they have all finished or Ctrl-C interrupts them.

use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    path::PathBuf,
    pin::pin,
    sync::Arc,
    task::Poll,
};

use serde_json::Map;
use tokio::task::JoinSet;

use crate::run::{Invocation, SpawnFailure, Target, process::Run};
use crate::tools::{ToolKey, descriptor::Descriptor};
use crate::validators::{ValidatorFile, file::Validator};

/// How running one validator ended: green or red, or why it couldn't be
/// run.
pub(super) type Finished = Result<Run, SpawnFailure>;

/// Every validator of a project that vetted cleanly, in the order `check`
/// reports them: by validator file, in the order of their paths, then by
/// position in the file.
#[derive(Debug)]
pub(super) struct Plan {
    /// The project root, as an absolute path.
    root: PathBuf,
    files: Vec<ValidatorFile>,
    /// The descriptor of every tool a validator names.
    descriptors: BTreeMap<ToolKey, Descriptor>,
    /// Each validator, as the index of its file and its index in the file.
    order: Vec<(usize, usize)>,
}

/// One validator of a [Plan].
#[derive(Clone, Copy, Debug)]
pub(super) struct Job<'a> {
    pub(super) file: &'a ValidatorFile,
    /// The validator's position in the file, from 1, as `vet` numbers it.
    pub(super) position: usize,
    pub(super) validator: &'a Validator,
}

impl Plan {
    /// Every validator in `files`. Vetting guarantees `descriptors` holds
    /// the descriptor of each one's tool.
    pub(super) fn new(
        root: PathBuf,
        files: Vec<ValidatorFile>,
        descriptors: BTreeMap<ToolKey, Descriptor>,
    ) -> Self {
        let order = files
            .iter()
            .enumerate()
            .flat_map(|(file, loaded)| (0..loaded.validators().len()).map(move |i| (file, i)))
            .collect();
        Self {
            root,
            files,
            descriptors,
            order,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.order.len()
    }

    /// The validator at `index` in the plan's order.
    pub(super) fn job(&self, index: usize) -> Job<'_> {
        let (file, validator) = self.order[index];
        let file = &self.files[file];
        Job {
            file,
            position: validator + 1,
            validator: &file.validators()[validator],
        }
    }

    /// Every validator, in the plan's order.
    pub(super) fn jobs(&self) -> impl Iterator<Item = Job<'_>> {
        (0..self.len()).map(|index| self.job(index))
    }

    /// Resolve the validator at `index` against slcr's environment as it
    /// is now, which is when `PATH` is searched, then run it.
    async fn run(&self, index: usize) -> Finished {
        let job = self.job(index);
        let descriptor = self
            .descriptors
            .get(job.validator.tool())
            .expect("vetting guarantees a descriptor for every validator's tool");
        let empty = Map::new();
        let invocation = Invocation::new(Target {
            descriptor,
            with: job.validator.with().as_ref().unwrap_or(&empty),
            project_root: &self.root,
            validator_file: job.file.file().absolute(),
            requirement: job.validator.requirement(),
        })?;
        invocation.run().await
    }
}

/// What [run_all] came to.
#[derive(Debug)]
pub(super) struct Ran {
    /// Each validator's result, in the plan's order: `None` for one that
    /// hadn't finished when the run was interrupted.
    pub(super) results: Vec<Option<Finished>>,
    pub(super) interrupted: bool,
}

/// Run every validator of `plan`, at most `jobs` at once, starting them in
/// the plan's order.
///
/// `report` is called once for each validator, in the plan's order
/// whatever order they finish in, as soon as it and every validator before
/// it have finished. If `interrupt` completes first, every run still going
/// is dropped, which kills its process group, and `report` is called for
/// each validator not yet reported, with `None` for those that hadn't
/// finished.
pub(super) async fn run_all<E>(
    plan: Arc<Plan>,
    jobs: NonZeroUsize,
    interrupt: impl Future<Output = ()>,
    mut report: impl FnMut(usize, Option<&Finished>) -> Result<(), E>,
) -> Result<Ran, E> {
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
            running.spawn(async move { (index, plan.run(index).await) });
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
pub(super) mod tests {
    use std::{
        convert::Infallible,
        path::Path,
        time::{Duration, Instant},
    };

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
            dir.path().to_path_buf(),
            vec![file],
            BTreeMap::from([(key, descriptor)]),
        );
        (dir, plan)
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

    #[test]
    fn an_interrupt_before_anything_starts_runs_nothing() {
        let (dir, plan) = plan(&["touch ran"]);
        let (ran, reported) = run(plan, 1, async {});
        assert!(ran.interrupted);
        assert_eq!(reported, [(0, false)]);
        assert!(!dir.path().join("ran").exists());
    }
}
