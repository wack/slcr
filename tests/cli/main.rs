//! End-to-end tests of the `slcr` binary.
//!
//! Each test runs the real binary in a scratch [Workspace] and snapshots
//! what a user would see — exit status, stdout, and stderr — with
//! `insta-cmd`. Snapshots live in `tests/cli/snapshots/`; review changes to
//! them with `cargo insta review`.
//!
//! Flags that every subcommand shares (`--log-level`, `--enable-colors`)
//! are covered once, in [global]; each subcommand's module covers its own
//! arguments and flags.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// `insta_cmd::assert_cmd_snapshot!`, with the variables that place the
/// workspace's data directory redacted from the snapshot's header.
///
/// insta-cmd records every variable a command sets in the header, and
/// [Workspace] sets `XDG_DATA_HOME` and `HOME` to absolute paths in a
/// temporary directory, which differ on every run. Redacting them keeps
/// headers stable; it never redacts what `slcr` printed. Defined before the
/// modules, so they can all use it.
macro_rules! assert_cmd_snapshot {
    ($($arg:tt)*) => {{
        let mut settings = insta::Settings::clone_current();
        settings.add_redaction(".env.XDG_DATA_HOME", "[DATA_HOME]");
        settings.add_redaction(".env.HOME", "[HOME]");
        settings.bind(|| insta_cmd::assert_cmd_snapshot!($($arg)*));
    }};
}

mod global;
mod init;
mod query;
mod render;
mod slice;
mod tools;
mod version;
mod vet;

/// The canon `TodoListItem` example, in each format.
const TODO_API_YAML: &str = include_str!("../fixtures/todo-api.spec.yaml");
const TODO_API_JSON: &str = include_str!("../fixtures/todo-api.spec.json");
const TODO_API_TOML: &str = include_str!("../fixtures/todo-api.spec.toml");
/// The cargo-nextest descriptor on the Schemas page.
const NEXTEST_DESCRIPTOR: &str = include_str!("../fixtures/cargo-nextest.descriptor.toml");
/// What the canon example renders to.
const TODO_API_MD: &str = include_str!("../fixtures/todo-api.md");

/// Environment variables that change what `slcr` prints. They are removed
/// so that a developer's or CI's environment can't leak into a snapshot.
const AMBIENT: [&str; 4] = ["LOG_LEVEL", "CLICOLOR", "CLICOLOR_FORCE", "NO_COLOR"];

/// The canon example in YAML, with REQ-001's `refinement` removed though
/// REQ-002 and REQ-003 refine it: one violation.
fn with_violation() -> String {
    let refinement = "          modality: MUST\n          refinement: AND\n";
    assert!(TODO_API_YAML.contains(refinement));
    TODO_API_YAML.replacen(refinement, "          modality: MUST\n", 1)
}

/// The canon example in YAML, with REQ-003's body stripped of its
/// modality: one warning.
fn with_warning() -> String {
    let body = "The endpoint MUST accept GET";
    assert!(TODO_API_YAML.contains(body));
    TODO_API_YAML.replace(body, "The endpoint accepts GET")
}

/// The workspace's `XDG_DATA_HOME`, which `slcr` reads on Linux.
const DATA_HOME: &str = ".data";

/// The workspace's `HOME`, which `slcr` reads on macOS, where its data
/// directory is under `~/Library/Application Support`.
const HOME: &str = ".home";

/// Where in a [Workspace] `slcr` keeps its persistent data, such as the
/// descriptor store: its data directory, as the `directories` crate places
/// it on this platform given the workspace's `XDG_DATA_HOME` and `HOME`.
#[cfg(target_os = "macos")]
const DATA_DIR: &str = ".home/Library/Application Support/slcr";
#[cfg(not(target_os = "macos"))]
const DATA_DIR: &str = ".data/slcr";

/// Where the descriptor store files a descriptor, relative to the
/// workspace: under the BLAKE3 hash of its bytes.
fn stored(descriptor: &str) -> String {
    let hex = blake3::hash(descriptor.as_bytes()).to_hex();
    format!("{DATA_DIR}/descriptors/blake3/{hex}.toml")
}

/// A descriptor's pin, as a manifest records it.
fn pin(descriptor: &str) -> String {
    format!("blake3:{}", blake3::hash(descriptor.as_bytes()).to_hex())
}

/// A scratch directory that a test runs `slcr` in, deleted when dropped.
///
/// Commands are given paths relative to it, so the paths in their output
/// are the same on every run. `XDG_DATA_HOME` and `HOME` point inside it
/// too, so `slcr`'s data directory is [DATA_DIR] and no test touches the
/// real one.
struct Workspace {
    dir: TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    /// Create the file at `path` with `contents`, and any missing parent
    /// directories.
    fn file(&self, path: &str, contents: impl AsRef<[u8]>) -> &Self {
        let path = self.path(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the parent directory");
        }
        std::fs::write(path, contents).expect("the file");
        self
    }

    /// Create the directory at `path`, and any missing parents.
    fn dir(&self, path: &str) -> &Self {
        std::fs::create_dir_all(self.path(path)).expect("the directory");
        self
    }

    /// Install `descriptor` in the workspace's descriptor store, as
    /// `slcr tools add` would.
    fn install(&self, descriptor: &str) -> &Self {
        self.file(&stored(descriptor), descriptor)
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.path(path)).expect("a readable file")
    }

    /// Every file in the workspace, as a sorted list of `/`-separated
    /// paths relative to it.
    fn files(&self) -> Vec<String> {
        fn visit(root: &Path, dir: &Path, files: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("a readable directory") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    let relative = path.strip_prefix(root).expect("a path in the workspace");
                    let parts: Vec<_> =
                        relative.iter().map(|part| part.to_string_lossy()).collect();
                    files.push(parts.join("/"));
                }
            }
        }

        let mut files = Vec::new();
        visit(self.dir.path(), self.dir.path(), &mut files);
        files.sort();
        files
    }

    /// `slcr` with `args`, run in the workspace.
    fn slcr(&self, args: &[&str]) -> Command {
        self.slcr_in(".", args)
    }

    /// `slcr` with `args`, run in the workspace's directory `dir`.
    fn slcr_in(&self, dir: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slcr"));
        command.current_dir(self.path(dir)).args(args);
        for variable in AMBIENT {
            command.env_remove(variable);
        }
        // Absolute: the XDG Base Directory Specification says a relative
        // path is ignored.
        command
            .env("XDG_DATA_HOME", self.path(DATA_HOME))
            .env("HOME", self.path(HOME));
        command
    }
}
