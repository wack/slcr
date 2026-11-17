//! Projects on disk whose SLCR files are kept apart from their code, as a
//! desktop application keeps them: vetted, installed, and checked through
//! the library, and held to what `slcr` writes for the same project.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use slcr::check::{Options, Report, RunOutcome};
use slcr::{DescriptorStore, Project};
use tempfile::TempDir;

use super::TODO_API_YAML;
use super::https::HttpsServer;

/// A tool that runs `with.script` with `sh -c` in the directory its
/// validator file checks.
const SH: &str = r#"formatVersion = "1"
name = "sh"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["script"]
properties.script = { type = "string" }

[invoke]
argv = ["sh", "-c", "{with.script}"]
cwd = "validator-dir"
timeout = 60
"#;

/// Where `slcr`'s data directory is in a [Kept] workspace, given its
/// `XDG_DATA_HOME` and `HOME`.
#[cfg(target_os = "macos")]
const DATA_DIR: &str = ".home/Library/Application Support/slcr";
#[cfg(not(target_os = "macos"))]
const DATA_DIR: &str = ".data/slcr";

/// The BLAKE3 hash of `text`, in hexadecimal.
fn hex(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// A manifest whose `specs` is `SPEC.slcr.yml` and that declares `key`,
/// pinned to `descriptor`, from `source`, a TOML inline table.
fn declaring(key: &str, descriptor: &str, source: &str) -> String {
    format!(
        "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n[tools.{key}]\n\
         version = \"1.0.0\"\nsource = {source}\npin = \"blake3:{}\"\n",
        hex(descriptor)
    )
}

/// A validator file whose validators check `todo-api/REQ-002` with `tool`,
/// each running one of `scripts`.
fn validators(tool: &str, scripts: &[&str]) -> String {
    let validators: String = scripts
        .iter()
        .map(|script| {
            format!(
                "\n[[validator]]\nrequirement = \"todo-api/REQ-002\"\ntool = \"{tool}\"\n\
                 with = {{ script = {script:?} }}\n"
            )
        })
        .collect();
    format!("formatVersion = \"1\"\n{validators}")
}

/// A workspace holding a project kept outside its root: `app`, its SLCR
/// directory, with the canon specification and [SH] kept in it, and
/// `repo`, its project root, with a `src` directory. Its descriptor
/// store is in the workspace, where `slcr` run in it keeps its own.
struct Kept {
    /// Deleted when the workspace is dropped.
    _dir: TempDir,
    /// The workspace, canonical, as a working directory is.
    root: PathBuf,
}

impl Kept {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = Self { _dir: dir, root };
        kept.file(
            "app/SLCR.toml",
            &declaring("sh", SH, "{ path = \"tools/sh.toml\" }"),
        )
        .file("app/SPEC.slcr.yml", TODO_API_YAML)
        .file("app/tools/sh.toml", SH);
        std::fs::create_dir_all(kept.root.join("repo/src")).unwrap();
        kept
    }

    /// [Kept::new], with validators in `app/src` running `scripts`, and
    /// [SH] installed.
    fn validated(scripts: &[&str]) -> Self {
        let kept = Self::new();
        kept.file("app/src/.validators.slcr.toml", &validators("sh", scripts))
            .install(SH);
        kept
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn file(&self, path: &str, contents: &str) -> &Self {
        let path = self.path(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    /// The descriptor store `slcr` uses when run in the workspace.
    fn store(&self) -> DescriptorStore {
        DescriptorStore::at(self.path(DATA_DIR).join("descriptors/blake3"))
    }

    /// File `descriptor` in [Kept::store], as installing it would.
    fn install(&self, descriptor: &str) -> &Self {
        self.file(
            &format!("{DATA_DIR}/descriptors/blake3/{}.toml", hex(descriptor)),
            descriptor,
        )
    }

    fn project(&self) -> Project {
        Project::open(self.path("app"), self.path("repo")).unwrap()
    }

    /// The stdout of `slcr` with `args`, run in `app`, with the
    /// workspace's data directory.
    fn slcr(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_slcr"))
            .args(args)
            .current_dir(self.path("app"))
            .env_remove("LOG_LEVEL")
            .env("NO_COLOR", "1")
            .env("XDG_DATA_HOME", self.path(".data"))
            .env("HOME", self.path(".home"))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    /// What `slcr check --offline --format json --project-root ../repo`
    /// writes, without durations.
    fn checked_by_slcr(&self) -> Value {
        let written = self.slcr(&[
            "check",
            "--offline",
            "--format",
            "json",
            "--project-root",
            "../repo",
        ]);
        without_durations(serde_json::from_str(&written).unwrap())
    }

    /// Check the project with `options`, on a runtime of the test's.
    fn check(&self, options: &Options) -> Report {
        super::block_on(self.project().check(
            &self.store(),
            options,
            |_| {},
            std::future::pending(),
        ))
    }
}

/// `report` as JSON, with every validator's `durationMs` removed: what
/// two runs of the same check share.
fn without_durations(mut report: Value) -> Value {
    if let Some(validators) = report["validators"].as_array_mut() {
        for validator in validators {
            validator.as_object_mut().unwrap().remove("durationMs");
        }
    }
    report
}

fn json(report: &Report) -> Value {
    without_durations(serde_json::to_value(report).unwrap())
}

/// Options for a check `jobs` validators at a time.
fn jobs(jobs: usize) -> Options {
    let mut options = Options::default();
    options.jobs = jobs.try_into().unwrap();
    options
}

#[test]
fn projects_kept_anywhere_vet_as_slcr_vet_reports_them() {
    let ok = Kept::validated(&["true"]);
    let no_counterpart = Kept::validated(&["true"]);
    no_counterpart.file(
        "app/lib/.validators.slcr.toml",
        &validators("sh", &["true"]),
    );
    let uninstalled = Kept::new();
    uninstalled.file(
        "app/src/.validators.slcr.toml",
        &validators("sh", &["true"]),
    );
    for (name, kept) in [
        ("ok", &ok),
        ("no counterpart", &no_counterpart),
        ("uninstalled", &uninstalled),
    ] {
        let (project, outcomes) = kept.project().vet(&kept.store());
        let written = kept.slcr(&["vet", "--format", "json", "--project-root", "../repo"]);
        let report = slcr::vet::json(Some(&project), &outcomes, false);
        assert_eq!(format!("{report}\n"), written, "{name}");
        assert_eq!(project.passes(), name == "ok", "{name}");
    }
}

#[test]
fn installing_fills_the_store_it_is_given() {
    // Unique, so that nothing could have filed it before.
    let descriptor = format!("# {:?}\n{SH}", Instant::now());
    let kept = Kept::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let installed = runtime.block_on(async {
        let server = HttpsServer::start(&descriptor).await;
        let source = format!("{{ url = \"{}\" }}", server.url("/sh.toml"));
        kept.file("app/SLCR.toml", &declaring("sh", &descriptor, &source));
        let installed = kept
            .project()
            .install(&kept.store(), &server.fetcher())
            .await
            .unwrap();
        assert_eq!(server.requests(), 1);
        installed
    });
    let pin = format!("blake3:{}", hex(&descriptor));
    assert_eq!(
        installed.to_string(),
        format!("sh: installed {}…", &pin[..19])
    );
    assert!(installed.passed());
    let descriptors = installed.descriptors();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].pin.unwrap().to_string(), pin);
    let filed = kept
        .path(DATA_DIR)
        .join(format!("descriptors/blake3/{}.toml", hex(&descriptor)));
    assert_eq!(std::fs::read_to_string(filed).unwrap(), descriptor);
    // The store `slcr` uses by default never got it.
    let default = directories::ProjectDirs::from("", "", "slcr").map(|dirs| {
        dirs.data_dir()
            .join(format!("descriptors/blake3/{}.toml", hex(&descriptor)))
    });
    assert!(!default.is_some_and(|path| path.exists()));
    // Nor did the workspace's own manifest change.
    assert!(kept.path("app/SLCR.toml").is_file());
}

#[test]
fn checks_report_as_slcr_check_does() {
    let kept = Kept::validated(&["true", "echo out; echo err >&2; exit 3", "exit 0"]);
    let report = kept.check(&jobs(2));
    assert!(!report.passed);
    let outcomes: Vec<RunOutcome> = report
        .validators
        .iter()
        .flatten()
        .map(|validator| validator.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [RunOutcome::Green, RunOutcome::Red, RunOutcome::Green]
    );
    assert_eq!(json(&report), kept.checked_by_slcr());
}

#[test]
fn checks_run_on_their_caller_s_runtime() {
    let kept = Kept::validated(&["true", "exit 0"]);
    let (project, store) = (kept.project(), kept.store());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // A check that started a runtime of its own would panic here, inside
    // one already running.
    let report = runtime
        .block_on(async move {
            tokio::spawn(async move {
                project
                    .check(&store, &Options::default(), |_| {}, std::future::pending())
                    .await
            })
            .await
        })
        .unwrap();
    assert!(report.passed, "{report:?}");
    assert_eq!(json(&report), kept.checked_by_slcr());
}

#[test]
fn checks_report_statuses_as_slcr_check_writes_them() {
    let kept = Kept::validated(&["sleep 0.3", "exit 1", "true", "sleep 0.1; true"]);
    let mut statuses = Vec::new();
    super::block_on(kept.project().check(
        &kept.store(),
        &jobs(4),
        |status| statuses.push(status.to_string()),
        std::future::pending(),
    ));
    let written = kept.slcr(&["check", "--offline", "--project-root", "../repo", "-j", "4"]);
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(statuses.len(), 4);
    assert_eq!(statuses, lines[..4]);
    // The rest is the specification's summary.
    assert_eq!(lines.len(), 5, "{written}");
    assert!(lines[4].starts_with("todo-api: "), "{written}");
}

#[test]
fn checks_never_install() {
    // A server that would see any connection made to it.
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let port = server.local_addr().unwrap().port();
    let kept = Kept::new();
    let source = format!("{{ url = \"https://127.0.0.1:{port}/sh.toml\" }}");
    kept.file("app/SLCR.toml", &declaring("sh", SH, &source))
        .file(
            "app/src/.validators.slcr.toml",
            &validators("sh", &["touch ran"]),
        );
    let report = kept.check(&Options::default());
    assert!(!report.passed);
    assert_eq!(report.descriptors, None);
    assert_eq!(report.validators, None);
    let vet = report.vet.as_ref().unwrap();
    assert_eq!(vet["passed"], false);
    let problems = vet["project"]["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0]["message"]
            .as_str()
            .unwrap()
            .contains("is not installed"),
        "{problems:?}"
    );
    assert!(!kept.path("repo/src/ran").exists());
    let accepted = server.accept();
    assert!(
        matches!(&accepted, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "{accepted:?}"
    );
}

#[cfg(unix)]
#[test]
fn checks_stop_every_validator_when_cancelled() {
    // One at a time: the first finishes; the second starts a grandchild,
    // records its PID, and outlives the cancellation; the rest never
    // start.
    let kept = Kept::validated(&[
        "true",
        "sleep 30 & echo $! > pid.tmp; mv pid.tmp pid; wait",
        "touch third",
        "touch fourth",
        "touch fifth",
    ]);
    let pid_file = kept.path("repo/src/pid");
    let cancel = {
        let pid_file = pid_file.clone();
        async move {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    let started = Instant::now();
    let report = super::block_on(
        kept.project()
            .check(&kept.store(), &jobs(1), |_| {}, cancel),
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(report.interrupted && !report.passed);
    assert_eq!(report.specs, None);
    let outcomes: Vec<RunOutcome> = report
        .validators
        .iter()
        .flatten()
        .map(|validator| validator.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [
            RunOutcome::Green,
            RunOutcome::Interrupted,
            RunOutcome::Interrupted,
            RunOutcome::Interrupted,
            RunOutcome::Interrupted,
        ]
    );
    for never in ["third", "fourth", "fifth"] {
        assert!(!kept.path(&format!("repo/src/{never}")).exists(), "{never}");
    }

    let pid = std::fs::read_to_string(&pid_file).unwrap();
    // Killed, it is gone, or a zombie until something reaps it.
    let dead = || {
        let ps = Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&ps.stdout);
        matches!(state.trim_start().chars().next(), None | Some('Z' | 'X'))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dead() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(dead(), "the sleep, {}, is still running", pid.trim());
}

#[cfg(unix)]
#[test]
fn checks_find_tools_on_the_path_they_are_given() {
    use std::os::unix::fs::PermissionsExt;

    let kept = Kept::new();
    let bin = kept.path("bin");
    // Green only if the PATH it sees is exactly the one given.
    let program = format!("#!/bin/sh\n[ \"$PATH\" = '{}' ]\n", bin.display());
    kept.file("bin/only-here", &program);
    std::fs::set_permissions(
        bin.join("only-here"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let descriptor = SH
        .replace("name = \"sh\"", "name = \"only-here\"")
        .replace("[\"sh\", \"-c\", \"{with.script}\"]", "[\"only-here\"]");
    kept.file(
        "app/SLCR.toml",
        &declaring(
            "only-here",
            &descriptor,
            "{ path = \"tools/only-here.toml\" }",
        ),
    )
    .file("app/tools/only-here.toml", &descriptor)
    .file(
        "app/src/.validators.slcr.toml",
        &validators("only-here", &["unused"]),
    )
    .install(&descriptor);
    let process_path = std::env::var_os("PATH").unwrap_or_default();
    assert!(!std::env::split_paths(&process_path).any(|dir| dir == bin));

    let mut given = Options::default();
    given.path = Some(bin.clone().into_os_string());
    let report = kept.check(&given);
    let validator = &report.validators.as_ref().unwrap()[0];
    assert_eq!(validator.outcome, RunOutcome::Green, "{report:?}");

    let report = kept.check(&Options::default());
    let validator = &report.validators.as_ref().unwrap()[0];
    assert_eq!(validator.outcome, RunOutcome::CouldNotRun);
    let error = validator.error.as_ref().unwrap();
    assert_eq!(error["message"], "`only-here` was not found on PATH");
}

#[test]
fn projects_are_checked_at_once_as_they_are_alone() {
    // Each project's validators see their own project root: one finds
    // its own file there, and one looks for the other's.
    let first = Kept::validated(&["test -f first", "test -f second"]);
    first.file("repo/src/first", "");
    let second = Kept::validated(&["test -f second", "test -f first"]);
    second.file("repo/src/second", "");
    let cwd = std::env::current_dir().unwrap();
    for kept in [&first, &second] {
        assert!(!cwd.starts_with(&kept.root), "{}", cwd.display());
    }

    let alone = [json(&first.check(&jobs(2))), json(&second.check(&jobs(2)))];
    // Both on one runtime, as tasks that run at the same time.
    let check = |kept: &Kept| {
        let (project, store) = (kept.project(), kept.store());
        async move {
            project
                .check(&store, &jobs(2), |_| {}, std::future::pending())
                .await
        }
    };
    let (first_check, second_check) = (check(&first), check(&second));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let at_once = runtime.block_on(async {
        let first = tokio::spawn(first_check);
        let second = tokio::spawn(second_check);
        (first.await.unwrap(), second.await.unwrap())
    });
    assert_eq!([json(&at_once.0), json(&at_once.1)], alone);
    // Each is the other's mirror: green where it finds its own file.
    for (report, name) in [(&at_once.0, "first"), (&at_once.1, "second")] {
        let outcomes: Vec<RunOutcome> = report
            .validators
            .iter()
            .flatten()
            .map(|validator| validator.outcome)
            .collect();
        assert_eq!(outcomes, [RunOutcome::Green, RunOutcome::Red], "{name}");
    }
    // Nothing either did is in the working directory.
    assert!(!Path::new("first").exists() && !Path::new("second").exists());
}
