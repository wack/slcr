use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use miette::{Diagnostic, IntoDiagnostic, Report as ErrorReport, Result};
use thiserror::Error;

use super::findings::{FileFindings, ValidatorFindings, counted, passes, tally};
use crate::Terminal;
use crate::fs::FileSystem;
use crate::project::{IndexedSpec, Problem, SpecIndex, discover};
use crate::spec::file::RequirementsFile;
use crate::spec::vet::Report;
use crate::tools::manifest::{ManifestFile, Project};
use crate::tools::{pinned, store::DescriptorStore};
use crate::validators::vet::{Context, vet as vet_validators};
use crate::validators::{Finding, ValidatorFile};

mod json;

/// The arguments to `slcr vet`.
#[derive(Args, Clone, Debug)]
pub struct VetArgs {
    /// The requirements files to vet: `.yaml`, `.yml`, `.json`, or `.toml`.
    /// Without any, vet the project: SLCR.toml and the descriptors its
    /// tools pin, every specification file its `specs` patterns match, and
    /// every validator file.
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Fail when a file has warnings, too.
    #[arg(long)]
    deny_warnings: bool,

    /// How to report the findings.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

/// How `slcr vet` reports its findings.
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
/// Every file is vetted, even after one fails. By default each finding is
/// written to stderr as a diagnostic, then each file's status is written to
/// stdout; `--format json` writes one JSON document to stdout instead. The
/// command fails if any file can't be loaded or breaks an invariant, or,
/// with `--deny-warnings`, has a warning.
///
/// Given no files, it vets the project that holds the working directory:
/// its manifest, which counts as one more file, every specification file
/// the manifest's `specs` patterns match, and every validator file. A
/// problem with the project as a whole, such as a pattern that matches
/// nothing, two specifications with the same name, or a declared tool
/// whose pinned descriptor isn't installed, fails the manifest. Either
/// way, nothing is run and the network is never used: descriptors are read
/// from the descriptor store only.
pub struct Vet {
    terminal: Terminal,
    args: VetArgs,
}

impl Vet {
    pub fn new(terminal: Terminal, args: VetArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let fs = FileSystem::new()?;
        let (project, outcomes) = if self.args.files.is_empty() {
            let cwd = std::env::current_dir().into_diagnostic()?;
            let store = DescriptorStore::new(&fs);
            let (project, outcomes) = vet_project(&fs, &store, &cwd)?;
            (Some(project), outcomes)
        } else {
            let outcomes = self
                .args
                .files
                .iter()
                .map(|path| vet_file(&fs, path))
                .collect();
            (None, outcomes)
        };
        let deny_warnings = self.args.deny_warnings;
        match self.args.format {
            OutputFormat::Human => self.write_human(project.as_ref(), &outcomes)?,
            OutputFormat::Json => {
                let document = json::document(project.as_ref(), &outcomes, deny_warnings);
                self.terminal.write_stdout_line(&document)?;
            }
        }
        verdict(project.as_ref(), &outcomes, deny_warnings)
    }

    /// Write the project's problems, then each file's findings, then each
    /// validator file's, to stderr, then the manifest's status and each
    /// file's to stdout.
    fn write_human(&self, project: Option<&ProjectOutcome>, outcomes: &[Outcome]) -> Result<()> {
        let deny_warnings = self.args.deny_warnings;
        match project.map(|project| &project.result) {
            Some(Err(error)) => self.terminal.write_diagnostic(error.as_ref())?,
            Some(Ok(problems)) => {
                for problem in problems {
                    self.terminal.write_diagnostic(problem)?;
                }
            }
            None => {}
        }
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
        let validators = project.map_or(&[][..], |project| &project.validators);
        for outcome in validators {
            match &outcome.result {
                Err(error) => self.terminal.write_diagnostic(error.as_ref())?,
                Ok(findings) if !findings.is_empty() => {
                    let findings = ValidatorFindings::new(&outcome.path, findings);
                    self.terminal.write_diagnostic(&findings)?;
                }
                Ok(_) => {}
            }
        }
        if let Some(project) = project {
            self.terminal
                .write_stdout_line(&project.status().to_string())?;
        }
        for outcome in outcomes {
            self.terminal
                .write_stdout_line(&outcome.status().to_string())?;
        }
        for outcome in validators {
            self.terminal
                .write_stdout_line(&outcome.status().to_string())?;
        }
        Ok(())
    }
}

/// Vet the project that holds `cwd`, an absolute path: read its manifest,
/// and check the descriptor each declared tool pins as `store` holds it;
/// discover its specification files and validator files; vet each
/// specification, check that their names are unique, then vet each
/// validator file against them.
fn vet_project(
    fs: &FileSystem,
    store: &DescriptorStore,
    cwd: &Path,
) -> Result<(ProjectOutcome, Vec<Outcome>)> {
    let project = Project::find_from(cwd)?;
    let manifest = project.shown_manifest().to_path_buf();
    tracing::debug!(manifest = %manifest.display(), "vetting a project");
    let file = match ManifestFile::load(&project) {
        Ok(file) => file,
        Err(error) => {
            let result = Err(error);
            let project = ProjectOutcome {
                manifest,
                result,
                validators: Vec::new(),
            };
            return Ok((project, Vec::new()));
        }
    };
    let discovery = discover(project.root(), cwd, file.manifest().specs());
    let mut problems = discovery.problems;
    let loaded: Vec<_> = discovery
        .files
        .into_iter()
        .map(|file| {
            tracing::debug!(path = %file.shown().display(), "vetting a requirements file");
            let document = file.load_spec(fs);
            (file, document)
        })
        .collect();
    let (index, duplicates) = SpecIndex::new(loaded.iter().filter_map(|(file, document)| {
        let document = document.as_ref().ok()?;
        Some(IndexedSpec { file, document })
    }));
    problems.extend(duplicates);

    let (descriptors, unusable) = pinned::load_all(fs, store, file.manifest().tools());
    problems.extend(unusable.into_iter().map(Problem::Descriptor));
    // Each loaded descriptor's schema compiled when it was checked.
    let schemas = descriptors
        .iter()
        .filter_map(|(key, descriptor)| {
            let schema = jsonschema::draft202012::new(descriptor.schema()).ok()?;
            Some((key.clone(), schema))
        })
        .collect();
    let cx = Context {
        specs: &index,
        complete: loaded.iter().all(|(_, document)| document.is_ok()),
        manifest: &file,
        schemas: &schemas,
    };
    let validators = discovery
        .validators
        .into_iter()
        .map(|file| {
            tracing::debug!(path = %file.shown().display(), "vetting a validator file");
            let path = file.shown().to_path_buf();
            let result = ValidatorFile::load(file).map(|file| vet_validators(&file, &cx));
            ValidatorOutcome { path, result }
        })
        .collect();

    let outcomes = loaded
        .into_iter()
        .map(|(file, document)| Outcome {
            path: file.shown().to_path_buf(),
            result: document.map(|document| document.report()),
        })
        .collect();
    let result = Ok(problems);
    let project = ProjectOutcome {
        manifest,
        result,
        validators,
    };
    Ok((project, outcomes))
}

/// What vetting the project as a whole found.
#[derive(Debug)]
struct ProjectOutcome {
    /// The manifest, relative to the working directory.
    manifest: PathBuf,
    /// Every problem with the project, or why the manifest couldn't be
    /// loaded.
    result: Result<Vec<Problem>, ErrorReport>,
    /// What vetting each validator file found, in the order of their
    /// paths; none if the manifest couldn't be loaded.
    validators: Vec<ValidatorOutcome>,
}

/// What vetting one validator file found.
#[derive(Debug)]
struct ValidatorOutcome {
    /// The file, relative to the working directory.
    path: PathBuf,
    /// Every problem with its validators, or why it couldn't be loaded.
    result: Result<Vec<Finding>, ErrorReport>,
}

impl ValidatorOutcome {
    /// Whether the file passes. Its problems are never warnings.
    fn passes(&self) -> bool {
        self.result.as_ref().is_ok_and(Vec::is_empty)
    }

    fn status(&self) -> ValidatorStatus<'_> {
        ValidatorStatus(self)
    }
}

/// A validator file's one-line status, e.g.
/// `src/.validators.slcr.toml: 2 problems`.
struct ValidatorStatus<'a>(&'a ValidatorOutcome);

impl Display for ValidatorStatus<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = self.0;
        write!(f, "{}: ", outcome.path.display())?;
        match &outcome.result {
            Err(_) => f.write_str("could not be loaded"),
            Ok(findings) if findings.is_empty() => f.write_str("ok"),
            Ok(findings) => f.write_str(&counted(findings.len(), "problem")),
        }
    }
}

impl ProjectOutcome {
    /// Whether the project passes. Its problems are never warnings.
    fn passes(&self) -> bool {
        self.result.as_ref().is_ok_and(Vec::is_empty)
    }

    fn status(&self) -> ProjectStatus<'_> {
        ProjectStatus(self)
    }
}

/// The manifest's one-line status, e.g. `SLCR.toml: 2 problems`.
struct ProjectStatus<'a>(&'a ProjectOutcome);

impl Display for ProjectStatus<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let project = self.0;
        write!(f, "{}: ", project.manifest.display())?;
        match &project.result {
            Err(_) => f.write_str("could not be loaded"),
            Ok(problems) => match problems.len() {
                0 => f.write_str("ok"),
                1 => f.write_str("1 problem"),
                count => write!(f, "{count} problems"),
            },
        }
    }
}

/// What vetting one file found.
#[derive(Debug)]
struct Outcome {
    /// The file, as given on the command line.
    path: PathBuf,
    /// Every finding, or why the file couldn't be loaded.
    result: Result<Report, ErrorReport>,
}

impl Outcome {
    /// Whether the file passes.
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

/// Load the file at `path` without vetting it, then vet it.
fn vet_file(fs: &FileSystem, path: &Path) -> Outcome {
    tracing::debug!(path = %path.display(), "vetting a requirements file");
    let result = RequirementsFile::new(path)
        .map_err(ErrorReport::from)
        .and_then(|file| fs.load_file(file.unvetted()))
        .map(|document| document.report());
    Outcome {
        path: path.to_path_buf(),
        result,
    }
}

/// Succeed if the project, when there is one, and every file pass, or fail
/// naming how many didn't. The project's manifest and its validator files
/// count as files.
fn verdict(
    project: Option<&ProjectOutcome>,
    outcomes: &[Outcome],
    deny_warnings: bool,
) -> Result<()> {
    let project_failed = project.is_some_and(|project| !project.passes());
    let validators = project.map_or(&[][..], |project| &project.validators);
    let failed = outcomes
        .iter()
        .filter(|outcome| !outcome.passes(deny_warnings))
        .count()
        + validators
            .iter()
            .filter(|outcome| !outcome.passes())
            .count()
        + usize::from(project_failed);
    if failed == 0 {
        return Ok(());
    }
    // A file that fails only because warnings are denied deserves a note.
    let warnings_denied = outcomes
        .iter()
        .any(|outcome| !outcome.passes(deny_warnings) && outcome.passes(false));
    Err(VetFailed {
        failed,
        total: outcomes.len() + validators.len() + usize::from(project.is_some()),
        warnings_denied,
    }
    .into())
}

/// The error `slcr vet` fails with when any file fails.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("vetting failed for {failed} of {total} {}", if *.total == 1 { "file" } else { "files" })]
struct VetFailed {
    failed: usize,
    total: usize,
    /// Whether a file failed only because its warnings are denied.
    warnings_denied: bool,
}

impl Diagnostic for VetFailed {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        self.warnings_denied.then(|| {
            Box::new("`--deny-warnings` makes a warning fail the file") as Box<dyn Display>
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
    use crate::spec::graph::UnvettedDocument;
    use crate::terminal::render_plain;
    use crate::tools::manifest::ProjectError;

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
        serde_json::from_value::<UnvettedDocument>(value)
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
        let argv = ["slcr", "--enable-colors", "never", "vet"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn vet_failed(result: Result<()>) -> VetFailed {
        let err = result.unwrap_err();
        let failed = err.downcast_ref::<VetFailed>().expect("vetting failed");
        failed.clone()
    }

    fn json(value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn vet_needs_no_file() {
        let cli = Cli::parse_from(["slcr", "vet", "--deny-warnings"]);
        let Some(SlcrCommand::Vet(args)) = cli.cmd() else {
            panic!("expected `vet`");
        };
        assert!(args.files.is_empty());
    }

    #[test]
    fn vet_accepts_many_files_and_its_flags() {
        let cli = Cli::parse_from([
            "slcr",
            "vet",
            "a.yaml",
            "--deny-warnings",
            "b.json",
            "--enable-colors",
            "never",
            "--log-level",
            "off",
        ]);
        let Some(SlcrCommand::Vet(args)) = cli.cmd() else {
            panic!("expected `vet`");
        };
        assert_eq!(
            args.files,
            [PathBuf::from("a.yaml"), PathBuf::from("b.json")]
        );
        assert!(args.deny_warnings);
    }

    #[test]
    fn warnings_are_allowed_by_default() {
        let cli = Cli::parse_from(["slcr", "vet", "a.yaml"]);
        let Some(SlcrCommand::Vet(args)) = cli.cmd() else {
            panic!("expected `vet`");
        };
        assert!(!args.deny_warnings);
    }

    #[test]
    fn the_output_format_defaults_to_human() {
        let cli = Cli::parse_from(["slcr", "vet", "a.yaml"]);
        let Some(SlcrCommand::Vet(args)) = cli.cmd() else {
            panic!("expected `vet`");
        };
        assert_eq!(args.format, OutputFormat::Human);
    }

    #[test]
    fn the_output_format_can_be_json() {
        let cli = Cli::parse_from(["slcr", "vet", "--format", "json", "a.yaml"]);
        let Some(SlcrCommand::Vet(args)) = cli.cmd() else {
            panic!("expected `vet`");
        };
        assert_eq!(args.format, OutputFormat::Json);
        assert!(Cli::try_parse_from(["slcr", "vet", "--format", "xml", "a.yaml"]).is_err());
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
                vet_failed(result),
                VetFailed {
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
    fn a_violation_fails_vetting() {
        Jail::expect_with(|jail| {
            let broken = json(&with_violation());
            let result = run(jail, &[("broken.json", &broken)], &["broken.json"]);
            assert_eq!(
                vet_failed(result),
                VetFailed {
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
                vet_failed(result),
                VetFailed {
                    failed: 1,
                    total: 1,
                    warnings_denied: true,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn every_file_is_vetted_after_one_fails() {
        Jail::expect_with(|jail| {
            let broken = json(&with_violation());
            let files = [("broken.json", broken.as_str()), ("ok.yaml", TODO_API_YAML)];
            let result = run(
                jail,
                &files,
                &["broken.json", "missing.yaml", "ok.yaml", "spec.md"],
            );
            assert_eq!(
                vet_failed(result),
                VetFailed {
                    failed: 3,
                    total: 4,
                    warnings_denied: false,
                }
            );
            Ok(())
        });
    }

    #[test]
    fn vet_file_reports_every_finding() {
        Jail::expect_with(|jail| {
            jail.create_file("both.json", &json(&with_both()))?;
            let fs = FileSystem::new().unwrap();
            let outcome = vet_file(&fs, Path::new("both.json"));
            assert_eq!(outcome.path, PathBuf::from("both.json"));
            let report = outcome.result.unwrap();
            assert_eq!(report.violations().len(), 1);
            assert_eq!(report.warnings().len(), 1);
            Ok(())
        });
    }

    #[test]
    fn vet_file_explains_why_a_file_could_not_be_loaded() {
        Jail::expect_with(|jail| {
            jail.create_file("malformed.yaml", "root: [unclosed")?;
            jail.create_file(
                "invalid.toml",
                &TODO_API_TOML.replacen("REQ-001", "REQ-1", 1),
            )?;
            let fs = FileSystem::new().unwrap();
            let error = |name: &str| vet_file(&fs, Path::new(name)).result.unwrap_err();

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
    fn the_verdict_passes_only_when_every_file_does() {
        let ok = || outcome("ok.json", todo_api());
        let warned = || outcome("warned.json", with_warning());
        assert!(verdict(None, &[ok(), warned()], false).is_ok());
        assert!(verdict(None, &[], false).is_ok());

        let failed = |result: Result<()>| vet_failed(result);
        assert_eq!(
            failed(verdict(None, &[ok(), warned()], true)),
            VetFailed {
                failed: 1,
                total: 2,
                warnings_denied: true,
            }
        );
        assert_eq!(
            failed(verdict(
                None,
                &[ok(), unloadable("missing.json"), warned()],
                false
            )),
            VetFailed {
                failed: 1,
                total: 3,
                warnings_denied: false,
            }
        );
    }

    fn project(result: Result<Vec<Problem>, ErrorReport>) -> ProjectOutcome {
        ProjectOutcome {
            manifest: PathBuf::from("SLCR.toml"),
            result,
            validators: Vec::new(),
        }
    }

    fn validator_file(path: &str, result: Result<Vec<Finding>, ErrorReport>) -> ValidatorOutcome {
        ValidatorOutcome {
            path: PathBuf::from(path),
            result,
        }
    }

    /// A problem with the first validator of a file.
    fn duplicate() -> Finding {
        let source = miette::NamedSource::new("v", String::new());
        Finding::new(
            2,
            crate::validators::Problem::DuplicateValidator { first: 1 },
            None,
            std::sync::Arc::new(source),
        )
    }

    #[test]
    fn validator_files_count_as_files_in_the_verdict() {
        let ok = || outcome("ok.json", todo_api());
        let mut clean = project(Ok(Vec::new()));
        clean.validators = vec![validator_file("a/.validators.slcr.toml", Ok(Vec::new()))];
        assert!(verdict(Some(&clean), &[ok()], true).is_ok());

        let mut failing = project(Ok(Vec::new()));
        failing.validators = vec![
            validator_file("a/.validators.slcr.toml", Ok(vec![duplicate()])),
            validator_file(
                "b/.validators.slcr.json",
                Err(miette::miette!("unreadable")),
            ),
            validator_file("c/.validators.slcr.yml", Ok(Vec::new())),
        ];
        assert_eq!(
            vet_failed(verdict(Some(&failing), &[ok()], false)),
            VetFailed {
                failed: 2,
                total: 5,
                warnings_denied: false,
            }
        );
    }

    #[test]
    fn validator_file_statuses_count_their_problems() {
        let cases = [
            (validator_file("v.toml", Ok(Vec::new())), "v.toml: ok"),
            (
                validator_file("v.toml", Ok(vec![duplicate()])),
                "v.toml: 1 problem",
            ),
            (
                validator_file("v.toml", Ok(vec![duplicate(), duplicate()])),
                "v.toml: 2 problems",
            ),
            (
                validator_file("v.toml", Err(miette::miette!("unreadable"))),
                "v.toml: could not be loaded",
            ),
        ];
        for (outcome, status) in cases {
            assert_eq!(outcome.status().to_string(), status);
        }
    }

    fn no_match(pattern: &str) -> Problem {
        Problem::NoMatch {
            pattern: pattern.to_owned(),
        }
    }

    #[test]
    fn the_manifest_counts_as_a_file_in_the_verdict() {
        let ok = || outcome("ok.json", todo_api());
        assert!(verdict(Some(&project(Ok(Vec::new()))), &[ok()], true).is_ok());
        assert_eq!(
            vet_failed(verdict(
                Some(&project(Ok(vec![no_match("a"), no_match("b")]))),
                &[ok(), unloadable("missing.json")],
                false
            )),
            VetFailed {
                failed: 2,
                total: 3,
                warnings_denied: false,
            }
        );
        assert_eq!(
            vet_failed(verdict(
                Some(&project(Err(miette::miette!("unparsable")))),
                &[],
                false
            )),
            VetFailed {
                failed: 1,
                total: 1,
                warnings_denied: false,
            }
        );
    }

    #[test]
    fn the_manifest_status_counts_its_problems() {
        let cases = [
            (project(Ok(Vec::new())), "SLCR.toml: ok"),
            (project(Ok(vec![no_match("a")])), "SLCR.toml: 1 problem"),
            (
                project(Ok(vec![no_match("a"), no_match("b")])),
                "SLCR.toml: 2 problems",
            ),
            (
                project(Err(miette::miette!("unparsable"))),
                "SLCR.toml: could not be loaded",
            ),
        ];
        for (project, status) in cases {
            assert_eq!(project.status().to_string(), status);
        }
    }

    /// Vet the project that holds `cwd`, with an empty descriptor store.
    fn vet_in(cwd: &Path) -> Result<(ProjectOutcome, Vec<Outcome>)> {
        let store = tempfile::tempdir().unwrap();
        let store = DescriptorStore::at(store.path().join("blake3"));
        vet_project(&FileSystem::new().unwrap(), &store, cwd)
    }

    /// The cargo-nextest example on the Schemas page.
    const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

    /// A manifest whose `specs` are `specs/*` and that declares
    /// cargo-nextest, pinned to NEXTEST.
    fn declaring_nextest() -> String {
        format!(
            "formatVersion = \"1\"\nspecs = [\"specs/*\"]\n\n[tools.cargo-nextest]\n\
             version = \"1.0.0\"\nsource = {{ path = \"nextest.toml\" }}\npin = \"blake3:{}\"\n",
            blake3::hash(NEXTEST.as_bytes()).to_hex()
        )
    }

    /// The messages of the problems with the project.
    fn problems(project: &ProjectOutcome) -> Vec<String> {
        let problems = project.result.as_ref().unwrap();
        problems.iter().map(ToString::to_string).collect()
    }

    /// The statuses of the project's validator files.
    fn validator_statuses(project: &ProjectOutcome) -> Vec<String> {
        project
            .validators
            .iter()
            .map(|outcome| outcome.status().to_string())
            .collect()
    }

    #[test]
    fn the_project_s_validator_files_are_vetted_against_it() {
        let dir = project_dir(&[
            ("SLCR.toml", &declaring_nextest()),
            ("specs/todo.yaml", TODO_API_YAML),
            (
                "src/.validators.slcr.toml",
                "formatVersion = \"1\"\n[[validator]]\nrequirement = \"todo-api/REQ-002\"\n\
                 tool = \"cargo-nextest\"\nwith = { test = \"a::b\" }\n",
            ),
            (
                "src/api/.validators.slcr.yml",
                "formatVersion: \"1\"\nvalidator:\n  - requirement: todo-api/REQ-099\n    \
                 tool: cargo-nextest\n    with: { test: 1 }\n",
            ),
            ("src/bad/.validators.slcr.json", "{"),
        ]);
        let store_dir = tempfile::tempdir().unwrap();
        let store = DescriptorStore::at(store_dir.path().join("blake3"));
        let fs = FileSystem::new().unwrap();
        store.install(&fs, NEXTEST).unwrap();
        let cwd = dir.path().join("src");
        let (project, _) = vet_project(&fs, &store, &cwd).unwrap();
        assert_eq!(problems(&project), [] as [&str; 0]);
        assert_eq!(
            validator_statuses(&project),
            [
                ".validators.slcr.toml: ok",
                "api/.validators.slcr.yml: 2 problems",
                "bad/.validators.slcr.json: could not be loaded",
            ]
        );
        let findings: Vec<String> = project.validators[1]
            .result
            .as_ref()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            findings,
            [
                "validator 1: `todo-api/REQ-099` names REQ-099, which the specification \
                 `todo-api` (../specs/todo.yaml) doesn't have",
                "validator 1: `with` at `/test` does not satisfy the validator schema of \
                 cargo-nextest: 1 is not of type \"string\"",
            ]
        );
    }

    #[test]
    fn descriptor_problems_count_against_the_manifest() {
        let dir = project_dir(&[
            ("SLCR.toml", &declaring_nextest()),
            ("specs/todo.yaml", TODO_API_YAML),
            (
                "src/.validators.slcr.toml",
                "formatVersion = \"1\"\n[[validator]]\nrequirement = \"todo-api/REQ-002\"\n\
                 tool = \"cargo-nextest\"\n",
            ),
            ("src/.validators.slcr.yaml", "formatVersion: \"1\"\n"),
        ]);
        let (project, _) = vet_in(dir.path()).unwrap();
        assert_eq!(
            problems(&project),
            [
                "src/.validators.slcr.toml and src/.validators.slcr.yaml are validator files in \
                 the same directory",
                "the descriptor tool cargo-nextest pins is not installed",
            ]
        );
        assert!(!project.passes());
        // Without its descriptor, a tool's `with` isn't checked.
        assert_eq!(
            validator_statuses(&project),
            [
                "src/.validators.slcr.toml: ok",
                "src/.validators.slcr.yaml: could not be loaded",
            ]
        );
    }

    /// A project in a temporary directory, holding `files`.
    fn project_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    /// The paths `vet_project` reports, run from `dir` in `project`.
    fn paths(outcomes: &[Outcome]) -> Vec<String> {
        outcomes
            .iter()
            .map(|outcome| outcome.path.display().to_string())
            .collect()
    }

    #[test]
    fn the_project_s_files_are_vetted_and_named_from_the_working_directory() {
        let billing = TODO_API_YAML.replace("name: todo-api", "name: billing");
        let dir = project_dir(&[
            (
                "SLCR.toml",
                "formatVersion = \"1\"\nspecs = [\"specs/*\"]\n",
            ),
            ("specs/todo.yaml", TODO_API_YAML),
            ("specs/billing.yaml", &billing),
            (
                "specs/broken.json",
                &json(&with_violation()).replace("todo-api", "broken"),
            ),
        ]);
        let cwd = dir.path().join("specs");
        let (project, outcomes) = vet_in(&cwd).unwrap();
        assert_eq!(project.manifest, PathBuf::from("../SLCR.toml"));
        assert!(project.passes(), "{:?}", project.result);
        assert_eq!(
            paths(&outcomes),
            ["billing.yaml", "broken.json", "todo.yaml"]
        );
        let passing: Vec<bool> = outcomes
            .iter()
            .map(|outcome| outcome.passes(false))
            .collect();
        assert_eq!(passing, [true, false, true]);
    }

    #[test]
    fn project_problems_are_collected() {
        let dir = project_dir(&[
            (
                "SLCR.toml",
                "formatVersion = \"1\"\nspecs = [\"*.yaml\", \"docs/*.yml\"]\n",
            ),
            ("a.yaml", TODO_API_YAML),
            ("b.yaml", TODO_API_YAML),
            ("c.yaml", "[unclosed\n"),
        ]);
        let (project, outcomes) = vet_in(dir.path()).unwrap();
        let problems: Vec<String> = project
            .result
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            problems,
            [
                "`specs` pattern `docs/*.yml` matches no file",
                "a.yaml and b.yaml both name their specification `todo-api`",
            ]
        );
        assert_eq!(paths(&outcomes), ["a.yaml", "b.yaml", "c.yaml"]);
        assert!(outcomes[2].result.is_err());
    }

    #[test]
    fn an_unloadable_manifest_vets_no_file() {
        let dir = project_dir(&[("SLCR.toml", "specs = 1\n"), ("a.yaml", TODO_API_YAML)]);
        let (project, outcomes) = vet_in(dir.path()).unwrap();
        let error = project.result.unwrap_err();
        assert!(error.downcast_ref::<ParseError>().is_some(), "{error:?}");
        assert!(outcomes.is_empty());
    }

    #[test]
    fn vetting_a_project_needs_one() {
        let dir = project_dir(&[]);
        let err = vet_in(dir.path()).unwrap_err();
        assert!(err.downcast_ref::<ProjectError>().is_some(), "{err:?}");
    }

    #[test]
    fn vet_failed_counts_files() {
        let one = VetFailed {
            failed: 1,
            total: 1,
            warnings_denied: false,
        };
        assert_eq!(one.to_string(), "vetting failed for 1 of 1 file");
        assert!(one.help().is_none());

        let some = VetFailed {
            failed: 2,
            total: 3,
            warnings_denied: true,
        };
        assert_eq!(some.to_string(), "vetting failed for 2 of 3 files");
        assert_eq!(
            some.help().unwrap().to_string(),
            "`--deny-warnings` makes a warning fail the file"
        );
    }

    #[test]
    fn vet_failed_renders_its_help() {
        let failed = VetFailed {
            failed: 1,
            total: 2,
            warnings_denied: true,
        };
        assert_eq!(
            render_plain(&failed),
            "  × vetting failed for 1 of 2 files\n  \
             help: `--deny-warnings` makes a warning fail the file\n"
        );
    }
}
