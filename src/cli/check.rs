use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use miette::{Diagnostic, Report as ErrorReport, Result, Severity};
use thiserror::Error;

use crate::Terminal;
use crate::fs::FileSystem;
use crate::spec::check::{Finding, Report};
use crate::spec::file::RequirementsFile;

mod json;

/// The arguments to `slcr check`.
#[derive(Args, Clone, Debug)]
pub struct CheckArgs {
    /// The requirements files to check: `.yaml`, `.yml`, `.json`, or `.toml`.
    #[arg(required = true, value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Fail when a file has warnings, too.
    #[arg(long)]
    deny_warnings: bool,

    /// How to report the findings.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

/// How `slcr check` reports its findings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Diagnostics on stderr, then each file's status on stdout.
    #[default]
    Human,
    /// One JSON document on stdout, describing every file and finding.
    Json,
}

/// Report the invariants a specification breaks.
///
/// Every file is checked, even after one fails. By default each finding is
/// written to stderr as a diagnostic, then each file's status is written to
/// stdout; `--format json` writes one JSON document to stdout instead. The
/// command fails if any file can't be loaded or breaks an invariant, or,
/// with `--deny-warnings`, has a warning.
pub struct Check {
    terminal: Terminal,
    args: CheckArgs,
}

impl Check {
    pub fn new(terminal: Terminal, args: CheckArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let fs = FileSystem::new()?;
        let outcomes: Vec<Outcome> = self
            .args
            .files
            .iter()
            .map(|path| check_file(&fs, path))
            .collect();
        let deny_warnings = self.args.deny_warnings;
        match self.args.format {
            OutputFormat::Human => self.write_human(&outcomes)?,
            OutputFormat::Json => {
                let document = json::document(&outcomes, deny_warnings);
                self.terminal.write_stdout_line(&document)?;
            }
        }
        verdict(&outcomes, deny_warnings)
    }

    /// Write each file's findings to stderr, then its status to stdout.
    fn write_human(&self, outcomes: &[Outcome]) -> Result<()> {
        let deny_warnings = self.args.deny_warnings;
        for outcome in outcomes {
            match &outcome.result {
                Err(error) => self.terminal.write_diagnostic(error.as_ref())?,
                Ok(report) if !report.is_clean() => {
                    let findings = FileFindings::new(&outcome.path, report, deny_warnings);
                    self.terminal.write_diagnostic(&findings)?;
                }
                Ok(_) => {}
            }
        }
        for outcome in outcomes {
            self.terminal
                .write_stdout_line(&outcome.status().to_string())?;
        }
        Ok(())
    }
}

/// What checking one file found.
#[derive(Debug)]
struct Outcome {
    /// The file, as given on the command line.
    path: PathBuf,
    /// Every finding, or why the file couldn't be loaded.
    result: Result<Report, ErrorReport>,
}

impl Outcome {
    /// Whether the file passes the check.
    fn passes(&self, deny_warnings: bool) -> bool {
        match &self.result {
            Ok(report) => passes(report, deny_warnings),
            Err(_) => false,
        }
    }

    fn status(&self) -> Status<'_> {
        Status(self)
    }
}

/// A file's one-line status, e.g. `todo-api.yaml: 2 violations, 1 warning`.
struct Status<'a>(&'a Outcome);

impl Display for Status<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = self.0;
        write!(f, "{}: ", outcome.path.display())?;
        match &outcome.result {
            Err(_) => f.write_str("could not be loaded"),
            Ok(report) if report.is_clean() => f.write_str("ok"),
            Ok(report) => f.write_str(&tally(report)),
        }
    }
}

/// Load the file at `path` without checking it, then check it.
fn check_file(fs: &FileSystem, path: &Path) -> Outcome {
    tracing::debug!(path = %path.display(), "checking a requirements file");
    let result = RequirementsFile::new(path)
        .map_err(ErrorReport::from)
        .and_then(|file| fs.load_file(file.unchecked()))
        .map(|document| document.report());
    Outcome {
        path: path.to_path_buf(),
        result,
    }
}

/// Whether a file with `report` passes: it breaks no invariant and, if
/// warnings are denied, has no warning.
fn passes(report: &Report, deny_warnings: bool) -> bool {
    report.violations().is_empty() && (!deny_warnings || report.warnings().is_empty())
}

/// Succeed if every file passes, or fail naming how many didn't.
fn verdict(outcomes: &[Outcome], deny_warnings: bool) -> Result<()> {
    let failed = outcomes
        .iter()
        .filter(|outcome| !outcome.passes(deny_warnings))
        .count();
    if failed == 0 {
        return Ok(());
    }
    // A file that fails only because warnings are denied deserves a note.
    let warnings_denied = outcomes
        .iter()
        .any(|outcome| !outcome.passes(deny_warnings) && outcome.passes(false));
    Err(CheckFailed {
        failed,
        total: outcomes.len(),
        warnings_denied,
    }
    .into())
}

/// `count` and `noun`, pluralized: "1 warning", "2 warnings".
fn counted(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// What a report found, e.g. "2 violations, 1 warning", omitting a kind it
/// found none of.
fn tally(report: &Report) -> String {
    let violations = report.violations().len();
    let warnings = report.warnings().len();
    match (violations, warnings) {
        (0, 0) => "no findings".to_owned(),
        (_, 0) => counted(violations, "violation"),
        (0, _) => counted(warnings, "warning"),
        _ => format!(
            "{}, {}",
            counted(violations, "violation"),
            counted(warnings, "warning")
        ),
    }
}

/// Everything a check found in one file, as one diagnostic whose related
/// diagnostics are the findings.
///
/// It is an error if the file breaks an invariant, or has warnings that are
/// denied, and a warning otherwise.
#[derive(Debug)]
struct FileFindings<'a> {
    headline: String,
    severity: Severity,
    report: &'a Report,
}

impl<'a> FileFindings<'a> {
    fn new(path: &Path, report: &'a Report, deny_warnings: bool) -> Self {
        Self {
            headline: format!("{}: {}", path.display(), tally(report)),
            severity: if passes(report, deny_warnings) {
                Severity::Warning
            } else {
                Severity::Error
            },
            report,
        }
    }
}

impl Display for FileFindings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.headline)
    }
}

impl std::error::Error for FileFindings<'_> {}

impl Diagnostic for FileFindings<'_> {
    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }

    fn related<'b>(&'b self) -> Option<Box<dyn Iterator<Item = &'b dyn Diagnostic> + 'b>> {
        Some(Box::new(self.report.findings().map(Finding::diagnostic)))
    }
}

/// The error `slcr check` fails with when any file fails the check.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("the check failed for {failed} of {total} {}", if *.total == 1 { "file" } else { "files" })]
struct CheckFailed {
    failed: usize,
    total: usize,
    /// Whether a file failed only because its warnings are denied.
    warnings_denied: bool,
}

impl Diagnostic for CheckFailed {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        self.warnings_denied.then(|| {
            Box::new("`--deny-warnings` makes a warning fail the check") as Box<dyn Display>
        })
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
    use crate::spec::file::UnsupportedFormat;
    use crate::spec::graph::UncheckedDocument;
    use crate::terminal::render_plain;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");
    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");

    fn todo_api() -> Value {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    /// The canon example, with REQ-001's `refinement` removed though REQ-002
    /// and REQ-003 refine it.
    fn with_violation() -> Value {
        let mut value = todo_api();
        value["root"]["children"][0]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("refinement");
        value
    }

    /// The canon example, with REQ-003's body stripped of its modality.
    fn with_warning() -> Value {
        let mut value = todo_api();
        value["root"]["children"][0]["children"][0]["children"][1]["body"] =
            json!("A GET request reads an item.");
        value
    }

    /// Both of the above.
    fn with_both() -> Value {
        let mut value = with_violation();
        value["root"]["children"][0]["children"][0]["children"][1]["body"] =
            json!("A GET request reads an item.");
        value
    }

    fn report(value: Value) -> Report {
        serde_json::from_value::<UncheckedDocument>(value)
            .unwrap()
            .report()
    }

    fn outcome(path: &str, value: Value) -> Outcome {
        Outcome {
            path: PathBuf::from(path),
            result: Ok(report(value)),
        }
    }

    fn unloadable(path: &str) -> Outcome {
        Outcome {
            path: PathBuf::from(path),
            result: Err(miette::miette!("unreadable")),
        }
    }

    /// Write each file into the jail, then run `slcr` with `args`.
    fn run(jail: &mut Jail, files: &[(&str, &str)], args: &[&str]) -> Result<()> {
        for (name, contents) in files {
            jail.create_file(name, contents).unwrap();
        }
        let argv = ["slcr", "--enable-colors", "never", "check"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn check_failed(result: Result<()>) -> CheckFailed {
        let err = result.unwrap_err();
        let failed = err.downcast_ref::<CheckFailed>().expect("the check failed");
        failed.clone()
    }

    fn json(value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn check_requires_a_file() {
        assert!(Cli::try_parse_from(["slcr", "check"]).is_err());
    }

    #[test]
    fn check_accepts_many_files_and_its_flags() {
        let cli = Cli::parse_from([
            "slcr",
            "check",
            "a.yaml",
            "--deny-warnings",
            "b.json",
            "--enable-colors",
            "never",
            "--log-level",
            "off",
        ]);
        let Some(SlcrCommand::Check(args)) = cli.cmd() else {
            panic!("expected `check`");
        };
        assert_eq!(
            args.files,
            [PathBuf::from("a.yaml"), PathBuf::from("b.json")]
        );
        assert!(args.deny_warnings);
    }

    #[test]
    fn warnings_are_allowed_by_default() {
        let cli = Cli::parse_from(["slcr", "check", "a.yaml"]);
        let Some(SlcrCommand::Check(args)) = cli.cmd() else {
            panic!("expected `check`");
        };
        assert!(!args.deny_warnings);
    }

    #[test]
    fn the_output_format_defaults_to_human() {
        let cli = Cli::parse_from(["slcr", "check", "a.yaml"]);
        let Some(SlcrCommand::Check(args)) = cli.cmd() else {
            panic!("expected `check`");
        };
        assert_eq!(args.format, OutputFormat::Human);
    }

    #[test]
    fn the_output_format_can_be_json() {
        let cli = Cli::parse_from(["slcr", "check", "--format", "json", "a.yaml"]);
        let Some(SlcrCommand::Check(args)) = cli.cmd() else {
            panic!("expected `check`");
        };
        assert_eq!(args.format, OutputFormat::Json);
        assert!(Cli::try_parse_from(["slcr", "check", "--format", "xml", "a.yaml"]).is_err());
    }

    #[test]
    fn json_output_fails_the_same_way() {
        Jail::expect_with(|jail| {
            let broken = json(&with_violation());
            let warned = json(&with_warning());
            let files = [
                ("broken.json", broken.as_str()),
                ("warned.json", warned.as_str()),
            ];
            let result = run(
                jail,
                &files,
                &["--format", "json", "broken.json", "warned.json"],
            );
            assert_eq!(
                check_failed(result),
                CheckFailed {
                    failed: 1,
                    total: 2,
                    warnings_denied: false,
                }
            );
            run(jail, &[], &["--format", "json", "warned.json"]).unwrap();
            Ok(())
        });
    }

    #[test]
    fn the_canon_example_passes_in_every_format() {
        Jail::expect_with(|jail| {
            let files = [
                ("todo-api.json", TODO_API_JSON),
                ("todo-api.yaml", TODO_API_YAML),
                ("todo-api.yml", TODO_API_YAML),
                ("todo-api.toml", TODO_API_TOML),
            ];
            let names: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
            run(jail, &files, &names).unwrap();
            // Even with warnings denied, since it has none.
            run(
                jail,
                &[],
                &[&["--deny-warnings"], names.as_slice()].concat(),
            )
            .unwrap();
            Ok(())
        });
    }

    #[test]
    fn a_violation_fails_the_check() {
        Jail::expect_with(|jail| {
            let broken = json(&with_violation());
            let result = run(jail, &[("broken.json", &broken)], &["broken.json"]);
            assert_eq!(
                check_failed(result),
                CheckFailed {
                    failed: 1,
                    total: 1,
                    warnings_denied: false,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn warnings_pass_unless_denied() {
        Jail::expect_with(|jail| {
            let warned = json(&with_warning());
            run(jail, &[("warned.json", &warned)], &["warned.json"]).unwrap();
            let result = run(jail, &[], &["--deny-warnings", "warned.json"]);
            assert_eq!(
                check_failed(result),
                CheckFailed {
                    failed: 1,
                    total: 1,
                    warnings_denied: true,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn every_file_is_checked_after_one_fails() {
        Jail::expect_with(|jail| {
            let broken = json(&with_violation());
            let files = [("broken.json", broken.as_str()), ("ok.yaml", TODO_API_YAML)];
            let result = run(
                jail,
                &files,
                &["broken.json", "missing.yaml", "ok.yaml", "spec.md"],
            );
            assert_eq!(
                check_failed(result),
                CheckFailed {
                    failed: 3,
                    total: 4,
                    warnings_denied: false,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn check_file_reports_every_finding() {
        Jail::expect_with(|jail| {
            jail.create_file("both.json", &json(&with_both()))?;
            let fs = FileSystem::new().unwrap();
            let outcome = check_file(&fs, Path::new("both.json"));
            assert_eq!(outcome.path, PathBuf::from("both.json"));
            let report = outcome.result.unwrap();
            assert_eq!(report.violations().len(), 1);
            assert_eq!(report.warnings().len(), 1);
            Ok(())
        });
    }

    #[test]
    fn check_file_explains_why_a_file_could_not_be_loaded() {
        Jail::expect_with(|jail| {
            jail.create_file("malformed.yaml", "root: [unclosed")?;
            jail.create_file(
                "invalid.toml",
                &TODO_API_TOML.replacen("REQ-001", "REQ-1", 1),
            )?;
            let fs = FileSystem::new().unwrap();
            let error = |name: &str| check_file(&fs, Path::new(name)).result.unwrap_err();

            assert!(
                error("spec.md")
                    .downcast_ref::<UnsupportedFormat>()
                    .is_some()
            );
            assert!(
                error("malformed.yaml")
                    .downcast_ref::<ParseError>()
                    .is_some()
            );
            let invalid = error("invalid.toml");
            let parse = invalid.downcast_ref::<ParseError>().expect("a parse error");
            assert_eq!(parse.message(), "`REQ-1` is not a valid requirement ID");
            assert!(parse.span().is_some());
            assert!(
                error("missing.json")
                    .to_string()
                    .starts_with("could not read missing.json")
            );
            Ok(())
        });
    }

    #[test]
    fn whether_a_file_passes_depends_on_its_findings() {
        let cases = [
            (outcome("ok.json", todo_api()), true, true),
            (outcome("warned.json", with_warning()), true, false),
            (outcome("broken.json", with_violation()), false, false),
            (outcome("both.json", with_both()), false, false),
            (unloadable("missing.json"), false, false),
        ];
        for (outcome, passes, passes_with_warnings_denied) in cases {
            let path = outcome.path.display().to_string();
            assert_eq!(outcome.passes(false), passes, "{path}");
            assert_eq!(outcome.passes(true), passes_with_warnings_denied, "{path}");
        }
    }

    #[test]
    fn statuses_summarize_each_file() {
        let cases = [
            (outcome("ok.json", todo_api()), "ok.json: ok"),
            (
                outcome("warned.json", with_warning()),
                "warned.json: 1 warning",
            ),
            (
                outcome("broken.json", with_violation()),
                "broken.json: 1 violation",
            ),
            (
                outcome("both.json", with_both()),
                "both.json: 1 violation, 1 warning",
            ),
            (
                unloadable("missing.json"),
                "missing.json: could not be loaded",
            ),
        ];
        for (outcome, status) in cases {
            assert_eq!(outcome.status().to_string(), status);
        }
    }

    #[test]
    fn counts_are_pluralized() {
        assert_eq!(counted(0, "warning"), "0 warnings");
        assert_eq!(counted(1, "warning"), "1 warning");
        assert_eq!(counted(2, "violation"), "2 violations");
    }

    #[test]
    fn a_clean_report_tallies_no_findings() {
        assert_eq!(tally(&Report::default()), "no findings");
    }

    #[test]
    fn the_verdict_passes_only_when_every_file_does() {
        let ok = || outcome("ok.json", todo_api());
        let warned = || outcome("warned.json", with_warning());
        assert!(verdict(&[ok(), warned()], false).is_ok());
        assert!(verdict(&[], false).is_ok());

        let failed = |result: Result<()>| check_failed(result);
        assert_eq!(
            failed(verdict(&[ok(), warned()], true)),
            CheckFailed {
                failed: 1,
                total: 2,
                warnings_denied: true,
            }
        );
        assert_eq!(
            failed(verdict(
                &[ok(), unloadable("missing.json"), warned()],
                false
            )),
            CheckFailed {
                failed: 1,
                total: 3,
                warnings_denied: false,
            }
        );
    }

    #[test]
    fn check_failed_counts_files() {
        let one = CheckFailed {
            failed: 1,
            total: 1,
            warnings_denied: false,
        };
        assert_eq!(one.to_string(), "the check failed for 1 of 1 file");
        assert!(one.help().is_none());

        let some = CheckFailed {
            failed: 2,
            total: 3,
            warnings_denied: true,
        };
        assert_eq!(some.to_string(), "the check failed for 2 of 3 files");
        assert_eq!(
            some.help().unwrap().to_string(),
            "`--deny-warnings` makes a warning fail the check"
        );
    }

    #[test]
    fn file_findings_are_errors_only_when_the_file_fails() {
        let path = Path::new("spec.json");
        let cases = [
            (with_violation(), false, Severity::Error),
            (with_both(), false, Severity::Error),
            (with_warning(), false, Severity::Warning),
            (with_warning(), true, Severity::Error),
        ];
        for (value, deny_warnings, severity) in cases {
            let report = report(value);
            let findings = FileFindings::new(path, &report, deny_warnings);
            assert_eq!(findings.severity(), Some(severity), "{findings}");
        }
    }

    #[test]
    fn file_findings_relate_violations_before_warnings() {
        let report = report(with_both());
        let findings = FileFindings::new(Path::new("both.json"), &report, false);
        let related: Vec<(String, Option<Severity>)> = findings
            .related()
            .unwrap()
            .map(|finding| (finding.to_string(), finding.severity()))
            .collect();
        assert_eq!(
            related,
            [
                (
                    "REQ-001 is refined by REQ-002 but has no `refinement`".to_owned(),
                    None
                ),
                (
                    "the body of REQ-003 does not contain its modality keyword, MUST".to_owned(),
                    Some(Severity::Warning)
                ),
            ]
        );
    }

    #[test]
    fn file_findings_render_each_finding_with_its_help() {
        let report = report(with_both());
        let findings = FileFindings::new(Path::new("specs/both.json"), &report, false);
        assert_eq!(
            render_plain(&findings),
            "  × specs/both.json: 1 violation, 1 warning\n\
             \n\
             Error: \n  \
             × REQ-001 is refined by REQ-002 but has no `refinement`\n  \
             help: set `refinement` to AND, OR, or XOR\n\
             \n\
             Warning: \n  \
             ⚠ the body of REQ-003 does not contain its modality keyword, MUST\n"
        );
    }

    #[test]
    fn warnings_alone_render_as_a_warning() {
        let report = report(with_warning());
        let findings = FileFindings::new(Path::new("warned.yaml"), &report, false);
        let rendered = render_plain(&findings);
        assert!(
            rendered.starts_with("  ⚠ warned.yaml: 1 warning\n"),
            "{rendered}"
        );
    }

    #[test]
    fn check_failed_renders_its_help() {
        let failed = CheckFailed {
            failed: 1,
            total: 2,
            warnings_denied: true,
        };
        assert_eq!(
            render_plain(&failed),
            "  × the check failed for 1 of 2 files\n  \
             help: `--deny-warnings` makes a warning fail the check\n"
        );
    }
}
