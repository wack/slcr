use std::{
    ffi::OsStr,
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use clap::Args;
use miette::{Diagnostic, Result};
use thiserror::Error;

use crate::Terminal;
use crate::fs::FileSystem;
use crate::spec::file::RequirementsFile;
use crate::spec::graph::SlcrRequirementsDocument;
use crate::spec::text::{InvalidSpecName, InvalidTitle, SpecName, Title};

/// Where `slcr init` creates the specification when no file is given: a
/// well-known name, like `Makefile` or `LICENSE`.
pub const DEFAULT_FILE: &str = "SPEC.slcr.yml";

/// The stem of the well-known file name, whatever its extension. Its
/// specification is named after the directory that holds it.
const WELL_KNOWN_STEM: &str = "SPEC.slcr";

/// Suffixes that mark a file's stem as a specification's, dropped when the
/// specification is named after the file: `todo-api.spec.yaml` and
/// `todo-api.slcr.yml` both name `todo-api`.
const STEM_SUFFIXES: [&str; 2] = [".slcr", ".spec"];

/// The arguments to `slcr init`.
#[derive(Args, Clone, Debug)]
pub struct InitArgs {
    /// The requirements file to create: `.yaml`, `.yml`, `.json`, or `.toml`.
    /// It must not exist yet.
    #[arg(value_name = "FILE", default_value = DEFAULT_FILE)]
    file: PathBuf,

    /// The specification's name, e.g. `todo-api`. Defaults to the file's
    /// name without its extension and any `.slcr` or `.spec`, or, for
    /// `SPEC.slcr.*`, to the name of its directory.
    #[arg(long, value_parser = parse_spec_name)]
    name: Option<SpecName>,

    /// The root Section's title. Defaults to the specification's name.
    #[arg(long, value_parser = parse_title)]
    title: Option<Title>,
}

fn parse_spec_name(value: &str) -> Result<SpecName, InvalidSpecName> {
    SpecName::try_from(value.to_owned())
}

fn parse_title(value: &str) -> Result<Title, InvalidTitle> {
    Title::try_from(value.to_owned())
}

/// Create a specification with its root and Glossary sections.
///
/// The file's extension selects its format, as it does for `slcr check`.
/// The command never overwrites anything: it fails if the file exists, and
/// it doesn't create a missing directory.
pub struct Init {
    terminal: Terminal,
    args: InitArgs,
}

impl Init {
    pub fn new(terminal: Terminal, args: InitArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let InitArgs {
            file: path,
            name,
            title,
        } = self.args;
        let file = RequirementsFile::new(path.clone())?;
        let name = match name {
            Some(name) => name,
            None => derived_name(&path)?,
        };
        let title = title.unwrap_or_else(|| Title::from(&name));
        tracing::debug!(path = %path.display(), %name, "creating a requirements file");
        let document = SlcrRequirementsDocument::new(name, title);
        FileSystem::new()?.create_file(&file, &document)?;
        let created = Created {
            path: &path,
            name: document.spec().name(),
        };
        self.terminal.write_stdout_line(&created.to_string())
    }
}

/// The name a specification takes from its file's path when none is given.
fn derived_name(path: &Path) -> Result<SpecName, UnnamedSpec> {
    let candidate = name_candidate(path);
    candidate
        .clone()
        .and_then(|candidate| SpecName::try_from(candidate).ok())
        .ok_or_else(|| UnnamedSpec {
            path: path.to_path_buf(),
            candidate,
        })
}

/// What a specification would be named after its file's path, valid or
/// not: the file's stem without any `.slcr` or `.spec`, or, for the
/// well-known `SPEC.slcr.*`, the name of the directory that holds it.
fn name_candidate(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(OsStr::to_str)?;
    if stem == WELL_KNOWN_STEM {
        return directory_name(path);
    }
    let name = STEM_SUFFIXES
        .iter()
        .find_map(|suffix| stem.strip_suffix(suffix))
        .unwrap_or(stem);
    Some(name.to_owned())
}

/// The name of the directory that holds `path`, which need not exist.
fn directory_name(path: &Path) -> Option<String> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        // A bare file name is in the working directory.
        _ => Path::new("."),
    };
    // Resolving the directory turns `.` and `..` into real names. A
    // directory that doesn't exist can't be resolved, but its own name
    // will do: creating the file fails later anyway, and says why.
    let resolved = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    resolved
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
}

/// The error returned when no `--name` is given and the file's path doesn't
/// yield a valid specification name.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("could not name the specification in {}", .path.display())]
pub struct UnnamedSpec {
    path: PathBuf,
    /// The name the path suggested, if any.
    candidate: Option<String>,
}

impl Diagnostic for UnnamedSpec {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        let pass_one = "pass one with `--name`, e.g. `--name todo-api`";
        Some(Box::new(match &self.candidate {
            Some(candidate) => format!(
                "{candidate:?} is not a valid specification name, which is lowercase letters \
                 and digits in dash-separated words; {pass_one}"
            ),
            None => format!("the path does not suggest a name; {pass_one}"),
        }))
    }
}

/// The status `slcr init` reports, e.g.
/// `SPEC.slcr.yml: created specification todo-api`.
struct Created<'a> {
    path: &'a Path,
    name: &'a SpecName,
}

impl Display for Created<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: created specification {}",
            self.path.display(),
            self.name
        )
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use clap::Parser;
    use figment::Jail;

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;
    use crate::fs::AlreadyExists;
    use crate::spec::file::UnsupportedFormat;
    use crate::terminal::render_plain;

    /// What `slcr init` writes for `todo-api`, in each format.
    const TODO_API_YAML: &str = "\
$schema: https://slcr.io/reference/schemas/spec/v1.schema.json
formatVersion: \"1\"
spec:
  name: todo-api
root:
  kind: section
  id: SEC-001
  title: todo-api
glossary:
  kind: section
  id: SEC-002
  title: Glossary
";

    const TODO_API_JSON: &str = r#"{
  "$schema": "https://slcr.io/reference/schemas/spec/v1.schema.json",
  "formatVersion": "1",
  "spec": {
    "name": "todo-api"
  },
  "root": {
    "kind": "section",
    "id": "SEC-001",
    "title": "todo-api"
  },
  "glossary": {
    "kind": "section",
    "id": "SEC-002",
    "title": "Glossary"
  }
}
"#;

    const TODO_API_TOML: &str = r#""$schema" = "https://slcr.io/reference/schemas/spec/v1.schema.json"
formatVersion = "1"

[spec]
name = "todo-api"

[root]
kind = "section"
id = "SEC-001"
title = "todo-api"

[glossary]
kind = "section"
id = "SEC-002"
title = "Glossary"
"#;

    /// The arguments `slcr` parses `init` and then `args` into.
    fn parse(args: &[&str]) -> Result<InitArgs, clap::Error> {
        let argv = ["slcr", "init"].iter().chain(args);
        match Cli::try_parse_from(argv)?.cmd() {
            Some(SlcrCommand::Init(args)) => Ok(args.clone()),
            _ => panic!("expected `init`"),
        }
    }

    /// Run `slcr init` with `args`.
    fn run(args: &[&str]) -> Result<()> {
        let argv = ["slcr", "--enable-colors", "never", "init"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn read(jail: &Jail, path: &str) -> String {
        std::fs::read_to_string(jail.directory().join(path)).unwrap()
    }

    fn spec_name(value: &str) -> SpecName {
        SpecName::try_from(value.to_owned()).unwrap()
    }

    fn load(path: &str) -> SlcrRequirementsDocument {
        let fs = FileSystem::new().unwrap();
        fs.load_file(RequirementsFile::new(path).unwrap()).unwrap()
    }

    #[test]
    fn the_file_defaults_to_the_well_known_name() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.file, PathBuf::from("SPEC.slcr.yml"));
        assert_eq!(args.file, PathBuf::from(DEFAULT_FILE));
        assert!(args.name.is_none());
        assert!(args.title.is_none());
    }

    #[test]
    fn the_well_known_name_is_a_supported_requirements_file() {
        assert!(RequirementsFile::new(DEFAULT_FILE).is_ok());
        assert_eq!(
            Path::new(DEFAULT_FILE).file_stem().and_then(OsStr::to_str),
            Some(WELL_KNOWN_STEM)
        );
    }

    #[test]
    fn init_accepts_a_file_a_name_and_a_title() {
        let args = parse(&[
            "specs/api.json",
            "--name",
            "todo-api",
            "--title",
            "TodoList API",
            "--enable-colors",
            "never",
        ])
        .unwrap();
        assert_eq!(args.file, PathBuf::from("specs/api.json"));
        assert_eq!(args.name, Some(spec_name("todo-api")));
        assert_eq!(args.title.unwrap().as_str(), "TodoList API");
    }

    #[test]
    fn init_accepts_only_one_file() {
        assert!(parse(&["a.yaml", "b.yaml"]).is_err());
    }

    #[test]
    fn invalid_names_are_rejected_when_parsing() {
        for name in ["", "Todo API", "todo_api", "todo--api", "-todo", "tödo"] {
            // `--name=` keeps clap from reading `-todo` as a flag.
            let err = parse(&[&format!("--name={name}")]).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{name:?}"
            );
            assert!(
                err.to_string()
                    .contains("is not a valid specification name"),
                "{err}"
            );
        }
    }

    #[test]
    fn invalid_titles_are_rejected_when_parsing() {
        for title in ["", " leading", "trailing ", "two\nlines"] {
            let err = parse(&[&format!("--title={title}")]).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{title:?}"
            );
            assert!(err.to_string().contains("is not a valid title"), "{err}");
        }
    }

    #[test]
    fn names_derive_from_the_file_stem() {
        for (path, name) in [
            ("todo-api.yaml", "todo-api"),
            ("todo-api.spec.yaml", "todo-api"),
            ("todo-api.slcr.yml", "todo-api"),
            ("todo-api.spec.json", "todo-api"),
            ("specs/billing.slcr.toml", "billing"),
            ("spec.yaml", "spec"),
            ("slcr.yml", "slcr"),
            // Only the uppercase stem is the well-known one.
            ("spec.slcr.yml", "spec"),
            ("v2.json", "v2"),
        ] {
            assert_eq!(derived_name(Path::new(path)), Ok(spec_name(name)), "{path}");
        }
    }

    #[test]
    fn only_one_suffix_is_dropped_from_the_stem() {
        // `todo-api.slcr.spec` drops `.spec` and keeps `.slcr`, which is not
        // part of a valid name.
        let err = derived_name(Path::new("todo-api.slcr.spec.yml")).unwrap_err();
        assert_eq!(err.candidate.as_deref(), Some("todo-api.slcr"));
    }

    #[test]
    fn stems_that_are_not_valid_names_are_rejected() {
        for (path, candidate) in [
            ("My Spec.yaml", "My Spec"),
            ("todo_api.toml", "todo_api"),
            ("Todo.json", "Todo"),
            (".spec.yaml", ""),
            ("SPEC.yml", "SPEC"),
        ] {
            let err = derived_name(Path::new(path)).unwrap_err();
            assert_eq!(
                err,
                UnnamedSpec {
                    path: PathBuf::from(path),
                    candidate: Some(candidate.to_owned()),
                },
                "{path}"
            );
        }
    }

    #[test]
    fn the_well_known_file_is_named_after_its_directory() {
        Jail::expect_with(|jail| {
            jail.create_dir("todo-api")?;
            jail.create_dir("billing")?;
            let derived = |path: &str| derived_name(Path::new(path)).map(String::from);
            assert_eq!(derived("todo-api/SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("todo-api/SPEC.slcr.json").unwrap(), "todo-api");
            assert_eq!(derived("todo-api/./SPEC.slcr.toml").unwrap(), "todo-api");
            assert_eq!(
                derived("todo-api/../billing/SPEC.slcr.yaml").unwrap(),
                "billing"
            );

            jail.change_dir("todo-api")?;
            assert_eq!(derived("SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("./SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("../billing/SPEC.slcr.yml").unwrap(), "billing");
            Ok(())
        });
    }

    #[test]
    fn a_missing_directory_still_names_the_well_known_file() {
        Jail::expect_with(|_| {
            let name = derived_name(Path::new("absent/SPEC.slcr.yml")).unwrap();
            assert_eq!(name, spec_name("absent"));
            Ok(())
        });
    }

    #[test]
    fn a_directory_that_is_not_a_valid_name_is_rejected() {
        Jail::expect_with(|jail| {
            jail.create_dir("My Project")?;
            let err = derived_name(Path::new("My Project/SPEC.slcr.yml")).unwrap_err();
            assert_eq!(err.candidate.as_deref(), Some("My Project"));
            Ok(())
        });
    }

    #[test]
    fn the_filesystem_root_does_not_name_a_specification() {
        let err = derived_name(Path::new("/SPEC.slcr.yml")).unwrap_err();
        assert_eq!(err.candidate, None);
    }

    #[test]
    fn init_creates_the_well_known_file_by_default() {
        Jail::expect_with(|jail| {
            jail.create_dir("todo-api")?;
            jail.change_dir("todo-api")?;
            run(&[]).unwrap();
            assert_eq!(read(jail, "todo-api/SPEC.slcr.yml"), TODO_API_YAML);
            Ok(())
        });
    }

    #[test]
    fn init_writes_each_format_exactly() {
        Jail::expect_with(|jail| {
            for (path, expected) in [
                ("todo-api.spec.yaml", TODO_API_YAML),
                ("todo-api.slcr.yml", TODO_API_YAML),
                ("todo-api.spec.json", TODO_API_JSON),
                ("todo-api.spec.toml", TODO_API_TOML),
            ] {
                run(&[path]).unwrap();
                assert_eq!(read(jail, path), expected, "{path}");
            }
            Ok(())
        });
    }

    #[test]
    fn the_created_file_loads_and_passes_the_strictest_check() {
        Jail::expect_with(|_| {
            let paths = ["new.yaml", "new.yml", "new.json", "new.toml"];
            for path in paths {
                run(&[path, "--name", "todo-api"]).unwrap();
                let document = load(path);
                let expected = SlcrRequirementsDocument::new(
                    spec_name("todo-api"),
                    Title::try_from("todo-api".to_owned()).unwrap(),
                );
                assert_eq!(document, expected, "{path}");
                assert!(document.warnings().is_empty(), "{path}");
            }
            let argv = [
                "slcr",
                "--enable-colors",
                "never",
                "check",
                "--deny-warnings",
            ]
            .into_iter()
            .chain(paths);
            let cli = Cli::parse_from(argv);
            let terminal = Terminal::new(&cli);
            cli.cmd().clone().unwrap().dispatch(terminal).unwrap();
            Ok(())
        });
    }

    #[test]
    fn the_name_and_title_can_be_given() {
        Jail::expect_with(|_| {
            run(&[
                "My Spec.json",
                "--name",
                "billing",
                "--title",
                "Billing Service",
            ])
            .unwrap();
            let document = load("My Spec.json");
            assert_eq!(document.spec().name().as_str(), "billing");
            assert_eq!(document.root().title().as_str(), "Billing Service");
            assert_eq!(document.glossary().title().as_str(), "Glossary");
            Ok(())
        });
    }

    #[test]
    fn the_title_defaults_to_the_name() {
        Jail::expect_with(|_| {
            run(&["spec.toml", "--name", "billing"]).unwrap();
            let document = load("spec.toml");
            assert_eq!(document.root().title().as_str(), "billing");
            Ok(())
        });
    }

    #[test]
    fn an_unnameable_specification_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["My Spec.yaml"]).unwrap_err();
            let unnamed = err.downcast_ref::<UnnamedSpec>().expect("unnamed");
            assert_eq!(unnamed.candidate.as_deref(), Some("My Spec"));
            assert!(!jail.directory().join("My Spec.yaml").exists());
            Ok(())
        });
    }

    #[test]
    fn an_unsupported_format_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["SPEC.md", "--name", "todo-api"]).unwrap_err();
            assert!(err.downcast_ref::<UnsupportedFormat>().is_some(), "{err}");
            assert!(!jail.directory().join("SPEC.md").exists());
            Ok(())
        });
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        Jail::expect_with(|jail| {
            jail.create_file("todo-api.yaml", "keep: me\n")?;
            let err = run(&["todo-api.yaml"]).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(read(jail, "todo-api.yaml"), "keep: me\n");
            Ok(())
        });
    }

    #[test]
    fn init_fails_the_second_time() {
        Jail::expect_with(|jail| {
            run(&["todo-api.json"]).unwrap();
            let err = run(&["todo-api.json", "--title", "Changed"]).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(read(jail, "todo-api.json"), TODO_API_JSON);
            Ok(())
        });
    }

    #[test]
    fn a_missing_directory_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["specs/todo-api.yaml"]).unwrap_err();
            assert_eq!(err.to_string(), "could not write specs/todo-api.yaml");
            assert_eq!(
                err.help().map(|help| help.to_string()).as_deref(),
                Some("create the directory specs first")
            );
            assert!(!jail.directory().join("specs").exists());
            Ok(())
        });
    }

    #[test]
    fn the_status_names_the_file_and_the_specification() {
        let name = spec_name("todo-api");
        let created = Created {
            path: Path::new("SPEC.slcr.yml"),
            name: &name,
        };
        assert_eq!(
            created.to_string(),
            "SPEC.slcr.yml: created specification todo-api"
        );
    }

    #[test]
    fn an_unnamed_specification_explains_the_invalid_name() {
        let err = UnnamedSpec {
            path: PathBuf::from("My Project/SPEC.slcr.yml"),
            candidate: Some("My Project".to_owned()),
        };
        assert_eq!(
            render_plain(&err),
            "  × could not name the specification in My Project/SPEC.slcr.yml\n  \
             help: \"My Project\" is not a valid specification name, which is lowercase\n        \
             letters and digits in dash-separated words; pass one with `--name`,\n        \
             e.g. `--name todo-api`\n"
        );
    }

    #[test]
    fn an_unnamed_specification_without_a_candidate_asks_for_a_name() {
        let err = UnnamedSpec {
            path: PathBuf::from("/SPEC.slcr.yml"),
            candidate: None,
        };
        assert_eq!(
            err.help().unwrap().to_string(),
            "the path does not suggest a name; pass one with `--name`, e.g. `--name todo-api`"
        );
    }
}
