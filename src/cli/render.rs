use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use clap::Args;
use miette::{Diagnostic, Result};
use thiserror::Error;

use super::findings::FileFindings;
use crate::Terminal;
use crate::fs::FileSystem;
use crate::render;
use crate::spec::file::{DEFAULT_FILE, RequirementsFile, base_name};
use crate::spec::text::SpecName;

/// The `--output` that names stdout.
const STDOUT: &str = "-";

/// The arguments to `slcr render`.
#[derive(Args, Clone, Debug)]
pub struct RenderArgs {
    /// The requirements file to render: `.yaml`, `.yml`, `.json`, or `.toml`.
    #[arg(value_name = "FILE", default_value = DEFAULT_FILE)]
    file: PathBuf,

    /// Where to write the Markdown, or `-` for stdout. Defaults to FILE's
    /// name without its extension and any `.slcr` or `.spec`, plus `.md`,
    /// beside it: `SPEC.slcr.yml` renders to `SPEC.md`.
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Replace the output even if it wasn't rendered by `slcr`.
    #[arg(long)]
    force: bool,

    /// Write nothing, and fail unless the output already holds exactly what
    /// FILE renders to.
    #[arg(long, conflicts_with = "force")]
    expect_no_diff: bool,
}

/// Where the rendered Markdown goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Output {
    Stdout,
    File(PathBuf),
}

impl Output {
    /// The output `--output` names, or else the file beside `input` that
    /// is named after it.
    fn new(input: &Path, output: Option<PathBuf>) -> Result<Self, NoOutputPath> {
        match output {
            Some(output) if output == Path::new(STDOUT) => Ok(Self::Stdout),
            Some(output) => Ok(Self::File(output)),
            None => default_output(input).map(Self::File),
        }
    }
}

impl Display for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => f.write_str("stdout"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// The Markdown file a requirements file renders to by default: its
/// [base_name] plus `.md`, in the same directory.
fn default_output(input: &Path) -> Result<PathBuf, NoOutputPath> {
    match base_name(input) {
        Some(base) if !base.is_empty() => Ok(input.with_file_name(format!("{base}.md"))),
        _ => Err(NoOutputPath {
            input: input.to_path_buf(),
        }),
    }
}

/// Render a specification as Markdown.
///
/// The specification must break none of its invariants; its findings,
/// warnings included, are reported on stderr as `slcr check` reports them.
/// The Markdown replaces the output only if `slcr` rendered what is there,
/// unless forced, and an output that is already up to date is left as it
/// is. With `--expect-no-diff`, nothing is written.
pub struct Render {
    terminal: Terminal,
    args: RenderArgs,
}

impl Render {
    pub fn new(terminal: Terminal, args: RenderArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let RenderArgs {
            file: path,
            output,
            force,
            expect_no_diff,
        } = self.args;
        let file = RequirementsFile::new(path.clone())?;
        let output = Output::new(&path, output)?;
        if expect_no_diff && output == Output::Stdout {
            return Err(NothingToCompare.into());
        }

        tracing::debug!(path = %path.display(), %output, "rendering a requirements file");
        let fs = FileSystem::new()?;
        let unchecked = fs.load_file(file.unchecked())?;
        let report = unchecked.report();
        if !report.is_clean() {
            let findings = FileFindings::new(&path, &report, false);
            self.terminal.write_diagnostic(&findings)?;
        }
        let Ok(document) = unchecked.into_checked() else {
            return Err(RenderFailed { path }.into());
        };
        let markdown = render::document(&document);

        let output = match output {
            Output::Stdout => return self.terminal.write_stdout(&markdown),
            Output::File(output) => output,
        };
        if is_same_file(&path, &output) {
            return Err(OutputIsInput { path: output }.into());
        }
        let existing = fs.read_bytes(&output)?;
        let up_to_date = existing.as_deref() == Some(markdown.as_bytes());
        let status = if up_to_date {
            Status::UpToDate
        } else if expect_no_diff {
            let difference = match &existing {
                None => Difference::Missing,
                Some(existing) => {
                    Difference::AtLine(first_different_line(markdown.as_bytes(), existing))
                }
            };
            return Err(UnexpectedDiff {
                path: output,
                difference,
            }
            .into());
        } else {
            if let Some(existing) = &existing
                && !force
                && !render::is_rendered(existing)
            {
                return Err(NotRendered { path: output }.into());
            }
            fs.replace_file(&output, &markdown)?;
            Status::Rendered
        };
        let rendered = Rendered {
            path: &output,
            name: document.spec().name(),
            status,
        };
        self.terminal.write_stdout_line(&rendered.to_string())
    }
}

/// Whether `a` and `b` name the same existing file.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The 1-based number of the first line on which `expected` and `actual`,
/// which differ, differ.
fn first_different_line(expected: &[u8], actual: &[u8]) -> usize {
    let common = expected
        .iter()
        .zip(actual)
        .take_while(|(expected, actual)| expected == actual)
        .count();
    let complete_lines = expected[..common]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    complete_lines + 1
}

/// What rendering did to the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    /// The output was written.
    Rendered,
    /// The output already held the rendering, so it was left as it is.
    UpToDate,
}

/// The status `slcr render` reports, e.g.
/// `SPEC.md: rendered todo-api`.
struct Rendered<'a> {
    path: &'a Path,
    name: &'a SpecName,
    status: Status,
}

impl Display for Rendered<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.path.display())?;
        match self.status {
            Status::Rendered => write!(f, "rendered {}", self.name),
            Status::UpToDate => f.write_str("up to date"),
        }
    }
}

/// The error returned when the specification breaks its invariants, which
/// have been reported.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("could not render {}, which breaks its invariants", .path.display())]
#[diagnostic(help("fix the violations reported above, then render it again"))]
pub struct RenderFailed {
    path: PathBuf,
}

/// The error returned when no `--output` is given and the requirements
/// file's name doesn't suggest one.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("could not name the Markdown file for {}", .input.display())]
#[diagnostic(help("pass one with `--output`, e.g. `--output SPEC.md`"))]
pub struct NoOutputPath {
    input: PathBuf,
}

/// The error returned when the output is the requirements file itself.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{} is the requirements file being rendered", .path.display())]
#[diagnostic(help("choose another file with `--output`"))]
pub struct OutputIsInput {
    path: PathBuf,
}

/// The error returned when the output exists but wasn't rendered by
/// `slcr`, so it may hold someone's work.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{} exists, and slcr did not render it", .path.display())]
#[diagnostic(help("pass `--force` to replace it, or choose another file with `--output`"))]
pub struct NotRendered {
    path: PathBuf,
}

/// The error returned when `--expect-no-diff` is asked to compare stdout.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`--expect-no-diff` compares a file, but the output is stdout")]
#[diagnostic(help("name a file with `--output`, or omit `--output` for the default"))]
pub struct NothingToCompare;

/// How the output differs from the rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Difference {
    /// There is no output.
    Missing,
    /// The output differs, first on this 1-based line.
    AtLine(usize),
}

/// The error returned under `--expect-no-diff` when the output isn't
/// exactly what the specification renders to.
#[derive(Debug, Error, PartialEq, Eq)]
pub struct UnexpectedDiff {
    path: PathBuf,
    difference: Difference,
}

impl Display for UnexpectedDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match self.difference {
            Difference::Missing => write!(f, "{path} does not exist"),
            Difference::AtLine(line) => write!(
                f,
                "{path} differs from the rendered specification, first on line {line}"
            ),
        }
    }
}

impl Diagnostic for UnexpectedDiff {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        let verb = match self.difference {
            Difference::Missing => "create",
            Difference::AtLine(_) => "update",
        };
        Some(Box::new(format!(
            "render again without `--expect-no-diff` to {verb} it"
        )))
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use clap::Parser;
    use figment::Jail;
    use serde_json::{Value, json};

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;
    use crate::fs::format::ParseError;
    use crate::fs::{ReadError, WriteError};
    use crate::spec::file::UnsupportedFormat;
    use crate::terminal::render_plain;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");
    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");
    /// What the canon example renders to.
    const TODO_API_MD: &str = include_str!("../../tests/fixtures/todo-api.md");

    /// The arguments `slcr` parses `render` and then `args` into.
    fn parse(args: &[&str]) -> Result<RenderArgs, clap::Error> {
        let argv = ["slcr", "render"].iter().chain(args);
        match Cli::try_parse_from(argv)?.cmd() {
            Some(SlcrCommand::Render(args)) => Ok(args.clone()),
            _ => panic!("expected `render`"),
        }
    }

    /// Run `slcr render` with `args`.
    fn run(args: &[&str]) -> Result<()> {
        let argv = ["slcr", "--enable-colors", "never", "render"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn read(jail: &Jail, path: &str) -> String {
        std::fs::read_to_string(jail.directory().join(path)).unwrap()
    }

    fn exists(jail: &Jail, path: &str) -> bool {
        jail.directory().join(path).exists()
    }

    /// The names of the entries in the jail's directory, sorted.
    fn entries(jail: &Jail) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(jail.directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// An identifier for the file at `path` that changes when the file is
    /// replaced, even by identical contents.
    #[cfg(unix)]
    fn identity(jail: &Jail, path: &str) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(jail.directory().join(path))
            .unwrap()
            .ino()
    }

    fn todo_api() -> Value {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    /// The canon example, with REQ-001's `refinement` removed though REQ-002
    /// and REQ-003 refine it.
    fn with_violation() -> String {
        let mut value = todo_api();
        value["root"]["children"][0]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("refinement");
        serde_json::to_string_pretty(&value).unwrap()
    }

    /// The canon example, with REQ-003's body stripped of its modality.
    fn with_warning() -> String {
        let mut value = todo_api();
        value["root"]["children"][0]["children"][0]["children"][1]["body"] =
            json!("A GET request reads an item.");
        serde_json::to_string_pretty(&value).unwrap()
    }

    fn unexpected_diff(result: Result<()>) -> UnexpectedDiff {
        let err = result.unwrap_err();
        let diff = err
            .downcast_ref::<UnexpectedDiff>()
            .unwrap_or_else(|| panic!("an unexpected diff, not {err:?}"));
        UnexpectedDiff {
            path: diff.path.clone(),
            difference: diff.difference,
        }
    }

    #[test]
    fn render_defaults_to_the_well_known_file() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.file, PathBuf::from(DEFAULT_FILE));
        assert_eq!(args.output, None);
        assert!(!args.force);
        assert!(!args.expect_no_diff);
    }

    #[test]
    fn render_accepts_a_file_an_output_and_its_flags() {
        let args = parse(&["specs/api.json", "--output", "docs/API.md", "--force"]).unwrap();
        assert_eq!(args.file, PathBuf::from("specs/api.json"));
        assert_eq!(args.output, Some(PathBuf::from("docs/API.md")));
        assert!(args.force);

        let args = parse(&["-o", "-", "--expect-no-diff"]).unwrap();
        assert_eq!(args.output, Some(PathBuf::from("-")));
        assert!(args.expect_no_diff);
    }

    #[test]
    fn render_accepts_only_one_file() {
        assert!(parse(&["a.yaml", "b.yaml"]).is_err());
    }

    #[test]
    fn expect_no_diff_conflicts_with_force() {
        let err = parse(&["--expect-no-diff", "--force"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn outputs_are_named_after_the_requirements_file() {
        for (input, output) in [
            ("SPEC.slcr.yml", "SPEC.md"),
            ("SPEC.slcr.json", "SPEC.md"),
            ("todo-api.spec.yaml", "todo-api.md"),
            ("todo-api.slcr.toml", "todo-api.md"),
            ("specs/billing.spec.json", "specs/billing.md"),
            ("api.yml", "api.md"),
            ("../up/spec.yaml", "../up/spec.md"),
        ] {
            assert_eq!(
                Output::new(Path::new(input), None),
                Ok(Output::File(PathBuf::from(output))),
                "{input}"
            );
        }
    }

    #[test]
    fn an_explicit_output_wins() {
        let input = Path::new("SPEC.slcr.yml");
        assert_eq!(
            Output::new(input, Some(PathBuf::from("docs/README.md"))),
            Ok(Output::File(PathBuf::from("docs/README.md")))
        );
        assert_eq!(
            Output::new(input, Some(PathBuf::from("-"))),
            Ok(Output::Stdout)
        );
        // Only a bare `-` names stdout.
        assert_eq!(
            Output::new(input, Some(PathBuf::from("./-"))),
            Ok(Output::File(PathBuf::from("./-")))
        );
    }

    #[test]
    fn outputs_display_as_where_they_go() {
        assert_eq!(Output::Stdout.to_string(), "stdout");
        assert_eq!(
            Output::File(PathBuf::from("docs/SPEC.md")).to_string(),
            "docs/SPEC.md"
        );
    }

    #[test]
    fn a_file_named_only_by_its_suffix_names_no_output() {
        for input in [".spec.yaml", "specs/.slcr.json"] {
            assert_eq!(
                Output::new(Path::new(input), None),
                Err(NoOutputPath {
                    input: PathBuf::from(input)
                })
            );
        }
    }

    #[test]
    fn render_writes_the_default_output_beside_the_well_known_file() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            run(&[]).unwrap();
            assert_eq!(read(jail, "SPEC.md"), TODO_API_MD);
            assert_eq!(entries(jail), ["SPEC.md", "SPEC.slcr.yml"]);
            Ok(())
        });
    }

    #[test]
    fn every_format_renders_the_same_markdown() {
        Jail::expect_with(|jail| {
            jail.create_dir("specs")?;
            for (input, contents, output) in [
                ("specs/a.spec.json", TODO_API_JSON, "specs/a.md"),
                ("specs/b.spec.yaml", TODO_API_YAML, "specs/b.md"),
                ("specs/c.slcr.yml", TODO_API_YAML, "specs/c.md"),
                ("specs/d.toml", TODO_API_TOML, "specs/d.md"),
            ] {
                jail.create_file(input, contents)?;
                run(&[input]).unwrap();
                assert_eq!(read(jail, output), TODO_API_MD, "{input}");
            }
            Ok(())
        });
    }

    #[test]
    fn render_writes_to_an_explicit_output() {
        Jail::expect_with(|jail| {
            jail.create_dir("docs")?;
            jail.create_file("todo-api.yaml", TODO_API_YAML)?;
            run(&["todo-api.yaml", "--output", "docs/API.md"]).unwrap();
            assert_eq!(read(jail, "docs/API.md"), TODO_API_MD);
            assert!(!exists(jail, "todo-api.md"));
            Ok(())
        });
    }

    #[test]
    fn render_writes_to_stdout_without_creating_a_file() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            run(&["-o", "-"]).unwrap();
            assert_eq!(entries(jail), ["SPEC.slcr.yml"]);
            Ok(())
        });
    }

    #[test]
    fn render_replaces_its_own_earlier_output() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            jail.create_file(
                "SPEC.md",
                "<!-- Generated by slcr from the todo-api specification. -->\n\nStale.\n",
            )?;
            run(&[]).unwrap();
            assert_eq!(read(jail, "SPEC.md"), TODO_API_MD);
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn an_up_to_date_output_is_left_as_it_is() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            run(&[]).unwrap();
            let before = identity(jail, "SPEC.md");
            run(&[]).unwrap();
            run(&["--force"]).unwrap();
            assert_eq!(identity(jail, "SPEC.md"), before);
            assert_eq!(read(jail, "SPEC.md"), TODO_API_MD);
            Ok(())
        });
    }

    #[test]
    fn a_file_slcr_did_not_render_is_never_replaced_unforced() {
        Jail::expect_with(|jail| {
            jail.create_file("README.yaml", TODO_API_YAML)?;
            jail.create_file("README.md", "# My project\n")?;
            let err = run(&["README.yaml"]).unwrap_err();
            assert_eq!(
                err.downcast_ref::<NotRendered>(),
                Some(&NotRendered {
                    path: PathBuf::from("README.md")
                })
            );
            assert_eq!(read(jail, "README.md"), "# My project\n");
            Ok(())
        });
    }

    #[test]
    fn force_replaces_a_file_slcr_did_not_render() {
        Jail::expect_with(|jail| {
            jail.create_file("README.yaml", TODO_API_YAML)?;
            std::fs::write(jail.directory().join("README.md"), [0xff, 0xfe]).unwrap();
            run(&["README.yaml", "--force"]).unwrap();
            assert_eq!(read(jail, "README.md"), TODO_API_MD);
            Ok(())
        });
    }

    #[test]
    fn the_output_is_never_the_requirements_file() {
        Jail::expect_with(|jail| {
            jail.create_dir("specs")?;
            jail.create_file("specs/todo-api.yaml", TODO_API_YAML)?;
            for output in ["specs/todo-api.yaml", "./specs/../specs/todo-api.yaml"] {
                let err = run(&["specs/todo-api.yaml", "-o", output, "--force"]).unwrap_err();
                assert_eq!(
                    err.downcast_ref::<OutputIsInput>(),
                    Some(&OutputIsInput {
                        path: PathBuf::from(output)
                    }),
                    "{output}"
                );
            }
            assert_eq!(read(jail, "specs/todo-api.yaml"), TODO_API_YAML);
            Ok(())
        });
    }

    #[test]
    fn a_missing_output_directory_is_not_created() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            let err = run(&["-o", "docs/SPEC.md"]).unwrap_err();
            assert!(err.downcast_ref::<WriteError>().is_some(), "{err:?}");
            assert_eq!(
                err.help().map(|help| help.to_string()).as_deref(),
                Some("create the directory docs first")
            );
            assert!(!exists(jail, "docs"));
            Ok(())
        });
    }

    #[test]
    fn an_output_that_is_a_directory_is_not_replaced() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            jail.create_dir("SPEC.md")?;
            let err = run(&["--force"]).unwrap_err();
            assert!(err.downcast_ref::<ReadError>().is_some(), "{err:?}");
            assert!(jail.directory().join("SPEC.md").is_dir());
            Ok(())
        });
    }

    #[test]
    fn a_specification_that_breaks_its_invariants_is_not_rendered() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.json", &with_violation())?;
            let stale = "<!-- Generated by slcr from the todo-api specification. -->\n";
            jail.create_file("SPEC.md", stale)?;
            let err = run(&["SPEC.slcr.json"]).unwrap_err();
            assert_eq!(
                err.downcast_ref::<RenderFailed>(),
                Some(&RenderFailed {
                    path: PathBuf::from("SPEC.slcr.json")
                })
            );
            assert_eq!(read(jail, "SPEC.md"), stale);
            Ok(())
        });
    }

    #[test]
    fn warnings_do_not_stop_rendering() {
        Jail::expect_with(|jail| {
            jail.create_file("todo-api.json", &with_warning())?;
            run(&["todo-api.json"]).unwrap();
            let rendered = read(jail, "todo-api.md");
            assert!(rendered.contains("A GET request reads an item."));
            Ok(())
        });
    }

    #[test]
    fn files_that_cannot_be_loaded_are_not_rendered() {
        Jail::expect_with(|jail| {
            jail.create_file("malformed.yaml", "root: [unclosed")?;
            let err = run(&["missing.yaml"]).unwrap_err();
            assert!(
                err.to_string().starts_with("could not read missing.yaml"),
                "{err}"
            );
            let err = run(&["malformed.yaml"]).unwrap_err();
            assert!(err.downcast_ref::<ParseError>().is_some(), "{err:?}");
            let err = run(&["SPEC.md"]).unwrap_err();
            assert!(err.downcast_ref::<UnsupportedFormat>().is_some(), "{err:?}");
            let err = run(&[".spec.yaml"]).unwrap_err();
            assert!(err.downcast_ref::<NoOutputPath>().is_some(), "{err:?}");
            assert_eq!(entries(jail), ["malformed.yaml"]);
            Ok(())
        });
    }

    #[test]
    fn a_new_specification_renders() {
        Jail::expect_with(|jail| {
            jail.create_dir("todo-api")?;
            jail.change_dir("todo-api")?;
            let cli = Cli::parse_from(["slcr", "--enable-colors", "never", "init"]);
            cli.cmd()
                .clone()
                .unwrap()
                .dispatch(Terminal::new(&cli))
                .unwrap();
            run(&[]).unwrap();
            assert_eq!(
                read(jail, "todo-api/SPEC.md"),
                "<!-- Generated by slcr from the todo-api specification. Do not edit: change the \
                 specification and run `slcr render`. -->\n\
                 \n\
                 <a id=\"SEC-001\"></a>\n\
                 # todo-api\n\
                 \n\
                 <a id=\"SEC-002\"></a>\n\
                 ## 1. Glossary\n"
            );
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_passes_on_an_up_to_date_output() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            jail.create_dir("docs")?;
            jail.create_file("docs/API.md", TODO_API_MD)?;
            run(&["-o", "docs/API.md"]).unwrap();
            run(&["-o", "docs/API.md", "--expect-no-diff"]).unwrap();
            assert_eq!(entries(jail), ["SPEC.slcr.yml", "docs"]);
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn expect_no_diff_never_writes() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            run(&[]).unwrap();
            let before = identity(jail, "SPEC.md");
            run(&["--expect-no-diff"]).unwrap();
            assert_eq!(identity(jail, "SPEC.md"), before);
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_fails_on_a_missing_output_without_creating_it() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            assert_eq!(
                unexpected_diff(run(&["--expect-no-diff"])),
                UnexpectedDiff {
                    path: PathBuf::from("SPEC.md"),
                    difference: Difference::Missing,
                }
            );
            assert!(!exists(jail, "SPEC.md"));
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_fails_on_a_stale_output_and_leaves_it() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            // The Models section's body changes after rendering.
            let stale = TODO_API_MD.replace("Persistence assumptions.", "Old assumptions.");
            jail.create_file("SPEC.md", &stale)?;
            let line = TODO_API_MD
                .lines()
                .position(|line| line == "Persistence assumptions.")
                .unwrap()
                + 1;
            assert_eq!(
                unexpected_diff(run(&["--expect-no-diff"])),
                UnexpectedDiff {
                    path: PathBuf::from("SPEC.md"),
                    difference: Difference::AtLine(line),
                }
            );
            assert_eq!(read(jail, "SPEC.md"), stale);
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_fails_on_a_file_slcr_did_not_render() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            jail.create_file("SPEC.md", "# Hand-written\n")?;
            assert_eq!(
                unexpected_diff(run(&["--expect-no-diff"])).difference,
                Difference::AtLine(1)
            );
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_fails_first_on_broken_invariants() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.json", &with_violation())?;
            let err = run(&["SPEC.slcr.json", "--expect-no-diff"]).unwrap_err();
            assert!(err.downcast_ref::<RenderFailed>().is_some(), "{err:?}");
            Ok(())
        });
    }

    #[test]
    fn expect_no_diff_cannot_compare_stdout() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            let err = run(&["-o", "-", "--expect-no-diff"]).unwrap_err();
            assert_eq!(
                err.downcast_ref::<NothingToCompare>(),
                Some(&NothingToCompare)
            );
            Ok(())
        });
    }

    #[test]
    fn the_first_different_line_is_found() {
        for (expected, actual, line) in [
            ("a\nb\n", "x\nb\n", 1),
            ("a\nb\nc\n", "a\nx\nc\n", 2),
            ("a\nb\nc\n", "a\nb\nx\n", 3),
            // A missing trailing newline is on the last line.
            ("a\nb\n", "a\nb", 2),
            // An extra line, or a missing one, follows the common lines.
            ("a\n", "a\nb\n", 2),
            ("a\nb\n", "a\n", 2),
            ("a\n", "", 1),
        ] {
            assert_eq!(
                first_different_line(expected.as_bytes(), actual.as_bytes()),
                line,
                "{expected:?} vs {actual:?}"
            );
        }
    }

    #[test]
    fn the_status_names_the_output() {
        let name = SpecName::try_from("todo-api".to_owned()).unwrap();
        let status = |status| {
            Rendered {
                path: Path::new("docs/SPEC.md"),
                name: &name,
                status,
            }
            .to_string()
        };
        assert_eq!(status(Status::Rendered), "docs/SPEC.md: rendered todo-api");
        assert_eq!(status(Status::UpToDate), "docs/SPEC.md: up to date");
    }

    #[test]
    fn errors_render_with_their_help() {
        let cases: [(&dyn Diagnostic, &str); 7] = [
            (
                &RenderFailed {
                    path: PathBuf::from("SPEC.slcr.yml"),
                },
                "  × could not render SPEC.slcr.yml, which breaks its invariants\n  \
                 help: fix the violations reported above, then render it again\n",
            ),
            (
                &NoOutputPath {
                    input: PathBuf::from(".spec.yaml"),
                },
                "  × could not name the Markdown file for .spec.yaml\n  \
                 help: pass one with `--output`, e.g. `--output SPEC.md`\n",
            ),
            (
                &OutputIsInput {
                    path: PathBuf::from("spec.yaml"),
                },
                "  × spec.yaml is the requirements file being rendered\n  \
                 help: choose another file with `--output`\n",
            ),
            (
                &NotRendered {
                    path: PathBuf::from("README.md"),
                },
                "  × README.md exists, and slcr did not render it\n  \
                 help: pass `--force` to replace it, or choose another file with `--output`\n",
            ),
            (
                &NothingToCompare,
                "  × `--expect-no-diff` compares a file, but the output is stdout\n  \
                 help: name a file with `--output`, or omit `--output` for the default\n",
            ),
            (
                &UnexpectedDiff {
                    path: PathBuf::from("SPEC.md"),
                    difference: Difference::Missing,
                },
                "  × SPEC.md does not exist\n  \
                 help: render again without `--expect-no-diff` to create it\n",
            ),
            (
                &UnexpectedDiff {
                    path: PathBuf::from("SPEC.md"),
                    difference: Difference::AtLine(12),
                },
                "  × SPEC.md differs from the rendered specification, first on line 12\n  \
                 help: render again without `--expect-no-diff` to update it\n",
            ),
        ];
        for (diagnostic, expected) in cases {
            assert_eq!(render_plain(diagnostic), expected);
        }
    }
}
