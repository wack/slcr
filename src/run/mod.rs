//! Running one validator: its tool's descriptor, with the validator
//! entry's `with` substituted into `invoke.argv`, run as a child process
//! whose exit code is green or red.
//!
//! The rules are those of "SLCR — Tools: Manifest, Descriptors, and
//! Invocation" in Notion. `slcr check` runs many of these at once.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use derive_getters::Getters;
use miette::Diagnostic;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::project::QualifiedRequirementId;
use crate::tools::descriptor::{Descriptor, EnvName, ProgramName};

/// Resolving `invoke.cwd` to a directory.
mod cwd;
/// Running an [Invocation] and reading its outcome: a [process::Run].
pub(crate) mod process;
/// Finding a bare program name on `PATH`.
mod program;
/// Substituting placeholders into argument templates.
mod template;

/// The validator to run: its tool's descriptor and what the descriptor's
/// placeholders and `cwd` refer to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target<'a> {
    pub(crate) descriptor: &'a Descriptor,
    /// The validator entry's `with` object.
    pub(crate) with: &'a Map<String, Value>,
    /// The directory the project's validators check, as an absolute path:
    /// `{slcr.project_root}`, the `project` working directory, and the top
    /// of a `nearest` search.
    pub(crate) project_root: &'a Path,
    /// The directory the entry's validator file checks, under the project
    /// root, as an absolute path (see [Layout::validator_dir]):
    /// `{slcr.validator_dir}`, the `validator-dir` working directory, and
    /// where a `nearest` search starts.
    ///
    /// [Layout::validator_dir]: crate::project::Layout::validator_dir
    pub(crate) validator_dir: &'a Path,
    /// The requirement the validator verifies.
    pub(crate) requirement: &'a QualifiedRequirementId,
    /// The `PATH` the caller names, in place of slcr's own: what the
    /// program is found on, and what the child inherits, unless the
    /// descriptor's `env.PATH` overrides it. Without one, slcr's own is
    /// both.
    pub(crate) path: Option<&'a OsStr>,
}

/// A validator, resolved and ready to run: every argument substituted,
/// the working directory found, and the program found on `PATH`. Run it
/// with [Invocation::run].
#[derive(Clone, Debug, PartialEq, Eq, Getters)]
pub(crate) struct Invocation {
    /// The program as the descriptor names it.
    program: ProgramName,
    /// The absolute path `program` resolved to, which is what runs.
    executable: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    /// The `PATH` the child inherits in place of slcr's own, if the caller
    /// named one.
    path: Option<OsString>,
    /// Set or overridden in the environment the child inherits.
    env: BTreeMap<EnvName, String>,
    timeout: Option<Duration>,
    /// The exit codes that mean green.
    pass: Vec<u8>,
}

impl Invocation {
    /// Resolve `target` against the `PATH` it names, or else slcr's own,
    /// as the child will inherit it. Do this just before running it, so
    /// that the program found is the one `PATH` names then. Only without a
    /// `PATH` of the caller's is slcr's environment read.
    pub(crate) fn new(target: Target<'_>) -> Result<Self, SpawnFailure> {
        match target.path {
            Some(path) => Self::resolve(target, Some(path)),
            None => Self::resolve(target, std::env::var_os("PATH").as_deref()),
        }
    }

    /// Resolve `target` as if slcr's `PATH` were `inherited_path`.
    fn resolve(target: Target<'_>, inherited_path: Option<&OsStr>) -> Result<Self, SpawnFailure> {
        let invoke = target.descriptor.invoke();
        let validator_dir = target.validator_dir;
        let values = template::Values {
            with: target.with,
            project_root: target.project_root,
            validator_dir,
            requirement: target.requirement,
        };
        let args = invoke
            .argv()
            .args()
            .iter()
            .map(|arg| values.substitute(arg))
            .collect::<Result<_, _>>()?;
        let cwd = cwd::resolve(invoke.cwd(), target.project_root, validator_dir)?;
        // The descriptor's `env.PATH`, if it sets one, is the PATH the
        // child sees.
        let path = invoke
            .env()
            .iter()
            .find(|(name, _)| program::is_path_variable(name.as_str()))
            .map(|(_, value)| OsStr::new(value))
            .or(inherited_path);
        let program = invoke.argv().program();
        let executable = program::find(program, path)?;
        Ok(Self {
            program: program.clone(),
            executable,
            args,
            cwd,
            path: target.path.map(OsStr::to_os_string),
            env: invoke.env().clone(),
            timeout: invoke
                .timeout()
                .map(|seconds| Duration::from_secs(seconds.get())),
            pass: target.descriptor.results().pass().clone(),
        })
    }
}

/// Why a validator couldn't be run. A spawn failure is an error, not a
/// result: the validator is neither green nor red.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum SpawnFailure {
    #[error("`{{with.{field}}}` has no value: the validator's `with` has no `{field}`")]
    #[diagnostic(help(
        "give the validator a `with.{field}`; its tool's schema says which fields it takes"
    ))]
    MissingField { field: String },

    #[error("`{{with.{field}}}` can't be substituted: the validator's `with.{field}` is {found}")]
    #[diagnostic(help(
        "only a string, a number, or a boolean can be substituted into an argument"
    ))]
    NonScalarField { field: String, found: &'static str },

    #[error("{} holds `{file}`", searched(.start))]
    #[diagnostic(help(
        "the tool runs in the nearest directory that holds `{file}`, searching up from the \
         directory the validator file checks, at the validator file's own path under the \
         project root, and stopping at the project root"
    ))]
    NoNearest {
        file: String,
        /// The directory the validator file checks, relative to the
        /// project root.
        start: PathBuf,
    },

    #[error("the directory the validator file checks, {}, is outside the project root", shown(.dir))]
    OutsideProject {
        /// Relative to the project root.
        dir: PathBuf,
    },

    #[error("the working directory {} is not a directory", shown(.dir))]
    #[diagnostic(help("the tool's descriptor runs it there"))]
    MissingDirectory {
        /// Relative to the project root.
        dir: PathBuf,
    },

    #[error("`{program}` was not found on PATH")]
    #[diagnostic(help(
        "install it, or put its directory on PATH; slcr searches the PATH the tool would see \
         (the descriptor's `env.PATH`, if it sets one), but never an empty or relative entry, \
         nor the current directory"
    ))]
    ProgramNotFound { program: String },

    #[error("couldn't start `{program}`: {source}")]
    #[diagnostic(help("`{program}` was found on PATH, but the operating system couldn't run it"))]
    Os {
        program: String,
        /// The path `program` resolved to.
        executable: PathBuf,
        source: io::Error,
    },
}

/// `relative`, a path relative to the project root, as messages show it.
fn shown(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        "the project root".to_owned()
    } else {
        format!("`{}`", relative.display())
    }
}

/// The directories searched up from `start`, relative to the project root,
/// as messages describe them.
fn searched(start: &Path) -> String {
    if start.as_os_str().is_empty() {
        "not even the project root".to_owned()
    } else {
        format!(
            "no directory from `{}` up to the project root",
            start.display()
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::terminal::render_plain;

    /// A descriptor that runs `argv`, a TOML array, with `invoke` lines
    /// and `results` lines added. Its schema declares the `with` fields
    /// the tests substitute.
    pub(crate) fn descriptor(argv: &str, invoke: &str, results: &str) -> Descriptor {
        let source = format!(
            r#"formatVersion = "1"
name = "test-tool"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["name", "count", "ratio", "flag", "list", "table", "none", "script"]
properties.name = {{ type = "string" }}
properties.count = {{ type = "integer" }}
properties.ratio = {{ type = "number" }}
properties.flag = {{ type = "boolean" }}
properties.list = {{ type = "string" }}
properties.table = {{ type = "string" }}
properties.none = {{ type = "string" }}
properties.script = {{ type = "string" }}

[invoke]
argv = {argv}
cwd = "project"
{invoke}

[results]
{results}
"#
        );
        Descriptor::parse("test.descriptor.toml", &source).unwrap()
    }

    /// A project in a temporary directory, with a validator file in
    /// `specs/api`, whose project root is its SLCR directory.
    pub(crate) struct Project {
        pub(crate) dir: TempDir,
        pub(crate) requirement: QualifiedRequirementId,
    }

    impl Project {
        pub(crate) fn new() -> Self {
            let dir = TempDir::new().unwrap();
            fs::create_dir_all(dir.path().join("specs/api")).unwrap();
            Self {
                dir,
                requirement: "todo-api/REQ-002".parse().unwrap(),
            }
        }

        pub(crate) fn root(&self) -> &Path {
            self.dir.path()
        }

        /// The directory the validator file checks.
        pub(crate) fn validator_dir(&self) -> PathBuf {
            self.root().join("specs/api")
        }

        /// What running `descriptor` with `with` here resolves to, with
        /// slcr's `PATH`.
        pub(crate) fn invocation(
            &self,
            descriptor: &Descriptor,
            with: &Value,
        ) -> Result<Invocation, SpawnFailure> {
            Invocation::new(Target {
                descriptor,
                with: with.as_object().unwrap(),
                project_root: self.root(),
                validator_dir: &self.validator_dir(),
                requirement: &self.requirement,
                path: None,
            })
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_invocation_resolves_every_part() {
        let project = Project::new();
        let descriptor = descriptor(
            r#"["sh", "-c", "{with.script}", "{slcr.requirement}"]"#,
            "env = { GREETING = \"hi\" }\ntimeout = 5",
            "pass = [0, 3]",
        );
        let invocation = project
            .invocation(&descriptor, &json!({ "script": "exit 0" }))
            .unwrap();
        assert_eq!(invocation.program().as_str(), "sh");
        assert!(invocation.executable().is_absolute());
        assert!(invocation.executable().ends_with("sh"));
        assert_eq!(
            invocation.args(),
            &["-c", "exit 0", "todo-api/REQ-002"].map(OsString::from)
        );
        assert_eq!(invocation.cwd(), project.root());
        assert_eq!(invocation.env().len(), 1);
        assert_eq!(*invocation.timeout(), Some(Duration::from_secs(5)));
        assert_eq!(invocation.pass(), &[0, 3]);
    }

    #[cfg(unix)]
    #[test]
    fn the_descriptors_path_is_the_one_searched() {
        let project = Project::new();
        let bin = TempDir::new().unwrap();
        program::tests::executable(bin.path(), "only-here");
        let descriptor = descriptor(
            r#"["only-here"]"#,
            &format!("env = {{ PATH = {:?} }}", bin.path().to_str().unwrap()),
            "",
        );
        let target = Target {
            descriptor: &descriptor,
            with: &Map::new(),
            project_root: project.root(),
            validator_dir: &project.validator_dir(),
            requirement: &project.requirement,
            path: None,
        };
        let invocation = Invocation::resolve(target, Some(OsStr::new("/nonexistent"))).unwrap();
        assert_eq!(invocation.executable(), &bin.path().join("only-here"));

        // Without one, slcr's own is.
        let without = self::descriptor(r#"["only-here"]"#, "", "");
        let target = Target {
            descriptor: &without,
            ..target
        };
        let invocation = Invocation::resolve(target, Some(bin.path().as_os_str())).unwrap();
        assert_eq!(invocation.executable(), &bin.path().join("only-here"));
        let err = Invocation::resolve(target, Some(OsStr::new("/nonexistent"))).unwrap_err();
        assert!(matches!(err, SpawnFailure::ProgramNotFound { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn the_descriptors_path_wins_over_the_callers() {
        use std::os::unix::fs::PermissionsExt;

        let project = Project::new();
        let (theirs, callers) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        // Each directory has the program; only the descriptor's passes, and
        // only if the PATH it sees is the descriptor's.
        let check = format!("#!/bin/sh\n[ \"$PATH\" = '{}' ]\n", theirs.path().display());
        for (dir, script) in [
            (theirs.path(), check.as_str()),
            (callers.path(), "#!/bin/sh\nexit 7\n"),
        ] {
            let program = dir.join("both-have-it");
            fs::write(&program, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let descriptor = descriptor(
            r#"["both-have-it"]"#,
            &format!("env = {{ PATH = {:?} }}", theirs.path().to_str().unwrap()),
            "",
        );
        let invocation = Invocation::new(Target {
            descriptor: &descriptor,
            with: &Map::new(),
            project_root: project.root(),
            validator_dir: &project.validator_dir(),
            requirement: &project.requirement,
            path: Some(callers.path().as_os_str()),
        })
        .unwrap();
        assert_eq!(invocation.executable(), &theirs.path().join("both-have-it"));
        let run = crate::cli::block_on(invocation.run()).unwrap().unwrap();
        assert_eq!(*run.outcome(), process::Outcome::Green);
    }

    #[test]
    fn spawn_failures_say_what_to_do() {
        let project = Project::new();
        let missing = descriptor(r#"["slcr-no-such-program"]"#, "", "");
        let err = project.invocation(&missing, &json!({})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`slcr-no-such-program` was not found on PATH"
        );
        let help = err.help().unwrap().to_string();
        assert!(help.contains("never an empty or relative entry"), "{help}");

        // Arguments are substituted before the program is looked for.
        let missing = descriptor(r#"["slcr-no-such-program", "{with.name}"]"#, "", "");
        let err = project.invocation(&missing, &json!({})).unwrap_err();
        let message = render_plain(&err);
        assert!(
            message.contains("`{with.name}` has no value: the validator's `with` has no `name`")
        );
        assert!(message.contains("give the validator a `with.name`"));
    }
}
