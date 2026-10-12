//! `slcr vet --format json`: one JSON document describing every file and
//! finding, for agents and CI to consume.
//!
//! ```json
//! {
//!   "passed": false,
//!   "files": [
//!     {
//!       "path": "todo-api.yaml",
//!       "passed": false,
//!       "violations": 1,
//!       "warnings": 0,
//!       "findings": [
//!         {
//!           "code": "dangling-reference",
//!           "from": "REQ-009",
//!           "relation": "refines",
//!           "to": "REQ-099",
//!           "severity": "error",
//!           "message": "REQ-009 names REQ-099 in `refines`, but no such node exists"
//!         }
//!       ]
//!     },
//!     {
//!       "path": "missing.json",
//!       "passed": false,
//!       "error": {
//!         "message": "could not read missing.json",
//!         "causes": ["No such file or directory (os error 2)"]
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! A finding's `code` names the invariant, and its other fields depend on
//! the code. A file that couldn't be loaded has an `error` instead of
//! findings; its `labels` locate the problem in the file, with 1-based
//! `line` and `column` and a byte `offset` and `length`.
//!
//! When `vet` is given no files and vets the project, the document also
//! has a `project` object, before `files`, describing the manifest and
//! every problem with the project as a whole. Like a finding, a problem
//! has a kebab-case `code` and the fields that go with it, then its
//! `message` and any `help`:
//!
//! ```json
//! {
//!   "passed": false,
//!   "project": {
//!     "manifest": "SLCR.toml",
//!     "passed": false,
//!     "problems": [
//!       {
//!         "code": "no-match",
//!         "pattern": "docs/*.yml",
//!         "message": "`specs` pattern `docs/*.yml` matches no file",
//!         "help": "check it for typos; a pattern never matches a file that a `.gitignore` ignores"
//!       }
//!     ]
//!   },
//!   "files": []
//! }
//! ```
//!
//! A manifest that couldn't be loaded has an `error`, shaped like a file's,
//! instead of `problems`, and `files` is empty.
//!
//! Two validator files in one directory (`duplicate-validator-file`) are a
//! problem with the project too, as is a declared tool whose pinned
//! descriptor can't be used: `descriptor-not-installed`,
//! `descriptor-corrupted`, `descriptor-unreadable`, and
//! `invalid-descriptor`, each with the `tool` and its `pin`, and
//! `descriptor-version-mismatch`.
//!
//! Project mode also adds a `validators` array, after `files`, describing
//! every validator file. Each of its problems names its `validator` by its
//! position in the file's `validator` array, from 1, then has a `code`
//! (`unknown-spec`, `unknown-requirement`, `unknown-criterion`,
//! `tool-not-declared`, `invalid-with`, or `duplicate-validator`) and the
//! fields that go with it, its `message` and any `help`, and `labels` that
//! locate the validator, as an error's do:
//!
//! ```json
//! {
//!   "passed": false,
//!   "project": { "manifest": "SLCR.toml", "passed": true, "problems": [] },
//!   "files": [],
//!   "validators": [
//!     {
//!       "path": "src/.validators.slcr.toml",
//!       "passed": false,
//!       "problems": [
//!         {
//!           "validator": 5,
//!           "code": "tool-not-declared",
//!           "manifest": "SLCR.toml",
//!           "key": "ast-grep",
//!           "declared": ["cargo-nextest"],
//!           "message": "SLCR.toml declares no tool named ast-grep",
//!           "help": "the tools it declares are: cargo-nextest",
//!           "labels": [{ "line": 26, "column": 8, "offset": 533, "length": 10 }]
//!         }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! A validator file that couldn't be loaded has an `error` instead of
//! `problems`. The array is absent when `vet` is given files, and empty
//! when the manifest couldn't be loaded.

use miette::{Diagnostic, LabeledSpan, Report as ErrorReport};
use serde::Serialize;

use super::{Outcome, ProjectOutcome, ValidatorOutcome};
use crate::project::Problem;
use crate::spec::vet::Finding;
use crate::validators::{Finding as ValidatorFinding, Problem as ValidatorProblem};

/// The JSON document describing `project`, if `vet` vetted one, and
/// `outcomes`.
pub(super) fn document(
    project: Option<&ProjectOutcome>,
    outcomes: &[Outcome],
    deny_warnings: bool,
) -> String {
    let document = Document::new(project, outcomes, deny_warnings);
    serde_json::to_string_pretty(&document).expect("vetting results serialize to JSON")
}

/// The JSON document `slcr vet` writes, which `slcr check --format json`
/// embeds.
#[derive(Serialize)]
pub(in crate::cli) struct Document<'a> {
    /// Whether the project, if any, and every file passed.
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<ProjectJson<'a>>,
    files: Vec<File<'a>>,
    /// The project's validator files, in project mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    validators: Option<Vec<ValidatorFileJson<'a>>>,
}

impl<'a> Document<'a> {
    /// The document describing `project`, if `vet` vetted one, and
    /// `outcomes`.
    pub(in crate::cli) fn new(
        project: Option<&'a ProjectOutcome>,
        outcomes: &'a [Outcome],
        deny_warnings: bool,
    ) -> Self {
        let validators: Option<Vec<ValidatorFileJson<'_>>> = project.map(|project| {
            project
                .validators
                .iter()
                .map(ValidatorFileJson::new)
                .collect()
        });
        let project = project.map(ProjectJson::new);
        let files: Vec<File<'_>> = outcomes
            .iter()
            .map(|outcome| File::new(outcome, deny_warnings))
            .collect();
        Self {
            passed: project.as_ref().is_none_or(|project| project.passed)
                && files.iter().all(|file| file.passed)
                && validators.iter().flatten().all(|file| file.passed),
            project,
            files,
            validators,
        }
    }
}

#[derive(Serialize)]
struct ValidatorFileJson<'a> {
    /// The path, relative to the working directory.
    path: String,
    passed: bool,
    #[serde(flatten)]
    result: ValidatorResult<'a>,
}

impl<'a> ValidatorFileJson<'a> {
    fn new(outcome: &'a ValidatorOutcome) -> Self {
        let result = match &outcome.result {
            Ok(findings) => ValidatorResult::Vetted {
                problems: findings.iter().map(ValidatorProblemJson::new).collect(),
            },
            Err(error) => ValidatorResult::Unloadable {
                error: ErrorJson::new(error),
            },
        };
        Self {
            path: outcome.path.display().to_string(),
            passed: outcome.passes(),
            result,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ValidatorResult<'a> {
    Vetted {
        problems: Vec<ValidatorProblemJson<'a>>,
    },
    Unloadable {
        error: ErrorJson,
    },
}

#[derive(Serialize)]
struct ValidatorProblemJson<'a> {
    /// The validator's position in the file, from 1.
    validator: usize,
    /// The `code` and the fields that go with it.
    #[serde(flatten)]
    problem: &'a ValidatorProblem,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<LabelJson>,
}

impl<'a> ValidatorProblemJson<'a> {
    fn new(finding: &'a ValidatorFinding) -> Self {
        let labels = finding
            .labels()
            .into_iter()
            .flatten()
            .filter_map(|label| LabelJson::new(finding, &label))
            .collect();
        Self {
            validator: finding.validator(),
            problem: finding.problem(),
            message: finding.problem().to_string(),
            help: finding.help().map(|help| help.to_string()),
            labels,
        }
    }
}

#[derive(Serialize)]
struct ProjectJson<'a> {
    /// The manifest's path, relative to the working directory.
    manifest: String,
    passed: bool,
    #[serde(flatten)]
    result: ProjectResult<'a>,
}

impl<'a> ProjectJson<'a> {
    fn new(project: &'a ProjectOutcome) -> Self {
        let result = match &project.result {
            Ok(problems) => ProjectResult::Vetted {
                problems: problems.iter().map(ProblemJson::new).collect(),
            },
            Err(error) => ProjectResult::Unloadable {
                error: ErrorJson::new(error),
            },
        };
        Self {
            manifest: project.manifest.display().to_string(),
            passed: project.passes(),
            result,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ProjectResult<'a> {
    Vetted { problems: Vec<ProblemJson<'a>> },
    Unloadable { error: ErrorJson },
}

#[derive(Serialize)]
struct ProblemJson<'a> {
    /// The `code` and the fields that go with it.
    #[serde(flatten)]
    problem: &'a Problem,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
}

impl<'a> ProblemJson<'a> {
    fn new(problem: &'a Problem) -> Self {
        Self {
            problem,
            message: problem.to_string(),
            help: problem.help().map(|help| help.to_string()),
        }
    }
}

#[derive(Serialize)]
struct File<'a> {
    /// The path, as given on the command line.
    path: String,
    passed: bool,
    #[serde(flatten)]
    result: FileResult<'a>,
}

impl<'a> File<'a> {
    fn new(outcome: &'a Outcome, deny_warnings: bool) -> Self {
        let result = match &outcome.result {
            Ok(report) => FileResult::Vetted {
                violations: report.violations().len(),
                warnings: report.warnings().len(),
                findings: report.findings().map(FindingJson::new).collect(),
            },
            Err(error) => FileResult::Unloadable {
                error: ErrorJson::new(error),
            },
        };
        Self {
            path: outcome.path.display().to_string(),
            passed: outcome.passes(deny_warnings),
            result,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum FileResult<'a> {
    Vetted {
        violations: usize,
        warnings: usize,
        findings: Vec<FindingJson<'a>>,
    },
    Unloadable {
        error: ErrorJson,
    },
}

#[derive(Serialize)]
struct FindingJson<'a> {
    /// The `code` and the fields that go with it.
    #[serde(flatten)]
    finding: Finding<'a>,
    severity: Severity,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
}

impl<'a> FindingJson<'a> {
    fn new(finding: Finding<'a>) -> Self {
        let diagnostic = finding.diagnostic();
        Self {
            finding,
            severity: match finding {
                Finding::Violation(_) => Severity::Error,
                Finding::Warning(_) => Severity::Warning,
            },
            message: diagnostic.to_string(),
            help: diagnostic.help().map(|help| help.to_string()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Error,
    Warning,
}

/// Why a file couldn't be loaded, or any other error, as JSON.
#[derive(Serialize)]
pub(in crate::cli) struct ErrorJson {
    message: String,
    /// The errors that caused this one, outermost first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    causes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<LabelJson>,
}

impl ErrorJson {
    pub(in crate::cli) fn new(error: &ErrorReport) -> Self {
        Self::of(error.as_ref())
    }

    /// `diagnostic`, with the errors that caused it.
    pub(in crate::cli) fn of(diagnostic: &dyn Diagnostic) -> Self {
        let labels = diagnostic
            .labels()
            .into_iter()
            .flatten()
            .filter_map(|label| LabelJson::new(diagnostic, &label))
            .collect();
        let causes = std::iter::successors(diagnostic.source(), |error| error.source());
        Self {
            message: diagnostic.to_string(),
            causes: causes.map(ToString::to_string).collect(),
            help: diagnostic.help().map(|help| help.to_string()),
            labels,
        }
    }
}

/// A place in a file's source that an error points at.
#[derive(Serialize)]
struct LabelJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// 1-based.
    line: usize,
    /// 1-based, in characters.
    column: usize,
    /// In bytes from the start of the file.
    offset: usize,
    /// In bytes.
    length: usize,
}

impl LabelJson {
    /// Locate `label` in the source of `diagnostic`, if it has any.
    fn new(diagnostic: &dyn Diagnostic, label: &LabeledSpan) -> Option<Self> {
        let source = diagnostic.source_code()?;
        let line = source.read_span(label.inner(), 0, 0).ok()?.line();
        // miette counts columns in bytes. To count characters, read from
        // the start of the line before the label's, or of the file, and
        // count those after the last line break before the label.
        let context = source.read_span(label.inner(), 1, 0).ok()?;
        let before = label.offset().checked_sub(context.span().offset())?;
        let before = std::str::from_utf8(context.data()).ok()?.get(..before)?;
        let column = before
            .rsplit(['\n', '\r'])
            .next()
            .map_or(0, |line| line.chars().count());
        Some(Self {
            message: label.label().map(str::to_owned),
            line: line + 1,
            column: column + 1,
            offset: label.offset(),
            length: label.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use std::path::{Path, PathBuf};

    use figment::Jail;
    use serde_json::{Value, json};

    use super::*;
    use crate::cli::vet::vet_file;
    use crate::fs::FileSystem;
    use crate::spec::graph::UnvettedDocument;

    const TODO_API_JSON: &str = include_str!("../../../tests/fixtures/todo-api.spec.json");

    fn todo_api() -> Value {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    fn outcome(path: &str, value: Value) -> Outcome {
        let report = serde_json::from_value::<UnvettedDocument>(value)
            .unwrap()
            .report();
        Outcome {
            path: PathBuf::from(path),
            result: Ok(report),
        }
    }

    fn parsed(outcomes: &[Outcome], deny_warnings: bool) -> Value {
        serde_json::from_str(&document(None, outcomes, deny_warnings)).unwrap()
    }

    fn parsed_project(project: &ProjectOutcome, outcomes: &[Outcome]) -> Value {
        serde_json::from_str(&document(Some(project), outcomes, false)).unwrap()
    }

    fn project(result: Result<Vec<Problem>, ErrorReport>) -> ProjectOutcome {
        ProjectOutcome {
            manifest: PathBuf::from("../SLCR.toml"),
            result,
            validators: Vec::new(),
        }
    }

    /// The canon example with REQ-009 refining a missing REQ-099 instead of
    /// REQ-008, and REQ-010's body stripped of its modality.
    fn broken() -> Value {
        let mut value = todo_api();
        let tokens = &mut value["root"]["children"][3]["children"][0]["children"];
        tokens[0]["refines"] = json!(["REQ-099"]);
        tokens[1]["body"] = json!("The API accepts API keys.");
        value
    }

    #[test]
    fn a_clean_file_passes_with_no_findings() {
        let document = parsed(&[outcome("todo-api.json", todo_api())], false);
        assert_eq!(
            document,
            json!({
                "passed": true,
                "files": [{
                    "path": "todo-api.json",
                    "passed": true,
                    "violations": 0,
                    "warnings": 0,
                    "findings": []
                }]
            })
        );
    }

    #[test]
    fn findings_carry_their_code_fields_severity_message_and_help() {
        let document = parsed(&[outcome("broken.json", broken())], false);
        assert_eq!(
            document,
            json!({
                "passed": false,
                "files": [{
                    "path": "broken.json",
                    "passed": false,
                    "violations": 1,
                    "warnings": 1,
                    "findings": [
                        {
                            "code": "dangling-reference",
                            "from": "REQ-009",
                            "relation": "refines",
                            "to": "REQ-099",
                            "severity": "error",
                            "message": "REQ-009 names REQ-099 in `refines`, but no such node exists"
                        },
                        {
                            "code": "modality-not-stated",
                            "requirement": "REQ-010",
                            "modality": "MAY",
                            "severity": "warning",
                            "message": "the body of REQ-010 does not contain its modality keyword, MAY"
                        }
                    ]
                }]
            })
        );
    }

    #[test]
    fn help_is_included_when_a_finding_has_it() {
        let mut value = todo_api();
        value["root"]["children"][0]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("refinement");
        let document = parsed(&[outcome("broken.json", value)], false);
        assert_eq!(
            document["files"][0]["findings"][0]["help"],
            "set `refinement` to AND, OR, or XOR"
        );
    }

    #[test]
    fn denied_warnings_fail_the_file_and_the_document() {
        let mut value = todo_api();
        value["root"]["children"][3]["children"][0]["children"][1]["body"] =
            json!("The API accepts API keys.");
        let outcomes = [outcome("warned.json", value)];
        assert_eq!(parsed(&outcomes, false)["passed"], true);
        let document = parsed(&outcomes, true);
        assert_eq!(document["passed"], false);
        assert_eq!(document["files"][0]["passed"], false);
        assert_eq!(document["files"][0]["warnings"], 1);
    }

    #[test]
    fn one_failing_file_fails_the_document() {
        let outcomes = [
            outcome("ok.json", todo_api()),
            outcome("broken.json", broken()),
        ];
        let document = parsed(&outcomes, false);
        assert_eq!(document["passed"], false);
        assert_eq!(document["files"][0]["passed"], true);
        assert_eq!(document["files"][1]["passed"], false);
    }

    #[test]
    fn vetting_nothing_passes() {
        assert_eq!(parsed(&[], false), json!({ "passed": true, "files": [] }));
    }

    #[test]
    fn a_clean_project_passes() {
        let document = parsed_project(&project(Ok(Vec::new())), &[outcome("a.json", todo_api())]);
        assert_eq!(
            document,
            json!({
                "passed": true,
                "project": {
                    "manifest": "../SLCR.toml",
                    "passed": true,
                    "problems": []
                },
                "files": [{
                    "path": "a.json",
                    "passed": true,
                    "violations": 0,
                    "warnings": 0,
                    "findings": []
                }],
                "validators": []
            })
        );
    }

    #[test]
    fn project_problems_carry_their_code_fields_message_and_help() {
        let problems = vec![Problem::NoMatch {
            pattern: "docs/*.yml".to_owned(),
        }];
        let document = parsed_project(&project(Ok(problems)), &[outcome("a.json", todo_api())]);
        assert_eq!(document["passed"], false);
        assert_eq!(document["files"][0]["passed"], true);
        assert_eq!(
            document["project"],
            json!({
                "manifest": "../SLCR.toml",
                "passed": false,
                "problems": [{
                    "code": "no-match",
                    "pattern": "docs/*.yml",
                    "message": "`specs` pattern `docs/*.yml` matches no file",
                    "help": "check it for typos; a pattern never matches a file that a \
                             `.gitignore` ignores"
                }]
            })
        );
    }

    #[test]
    fn validator_problems_name_their_validator_and_locate_it() {
        let source = "formatVersion = \"1\"\n[[validator]]\n";
        let source = std::sync::Arc::new(miette::NamedSource::new(
            "src/.validators.slcr.toml",
            source.to_owned(),
        ));
        let finding = ValidatorFinding::new(
            2,
            ValidatorProblem::InvalidWith {
                tool: crate::tools::ToolKey::try_from("jev".to_owned()).unwrap(),
                pointer: "/x".to_owned(),
                reason: "1 is not of type \"string\"".to_owned(),
            },
            Some(miette::SourceSpan::from((20, 13))),
            source,
        );
        let mut project = project(Ok(Vec::new()));
        project.validators = vec![
            ValidatorOutcome {
                path: PathBuf::from("src/.validators.slcr.toml"),
                result: Ok(vec![finding]),
            },
            ValidatorOutcome {
                path: PathBuf::from("src/api/.validators.slcr.json"),
                result: Err(miette::miette!("unreadable")),
            },
        ];
        let document = parsed_project(&project, &[]);
        assert_eq!(document["passed"], false);
        assert_eq!(document["project"]["passed"], true);
        assert_eq!(
            document["validators"],
            json!([
                {
                    "path": "src/.validators.slcr.toml",
                    "passed": false,
                    "problems": [{
                        "validator": 2,
                        "code": "invalid-with",
                        "tool": "jev",
                        "pointer": "/x",
                        "reason": "1 is not of type \"string\"",
                        "message": "`with` at `/x` does not satisfy the validator schema of jev: \
                                    1 is not of type \"string\"",
                        "help": "`slcr tools show jev` prints the schema",
                        "labels": [{ "line": 2, "column": 1, "offset": 20, "length": 13 }]
                    }]
                },
                {
                    "path": "src/api/.validators.slcr.json",
                    "passed": false,
                    "error": { "message": "unreadable" }
                }
            ])
        );
    }

    #[test]
    fn validators_are_left_out_without_a_project() {
        let document = parsed(&[outcome("a.json", todo_api())], false);
        assert!(document.get("validators").is_none(), "{document:#}");
    }

    #[test]
    fn an_unloadable_manifest_explains_why() {
        let document = parsed_project(&project(Err(miette::miette!("unreadable"))), &[]);
        assert_eq!(
            document,
            json!({
                "passed": false,
                "project": {
                    "manifest": "../SLCR.toml",
                    "passed": false,
                    "error": { "message": "unreadable" }
                },
                "files": [],
                "validators": []
            })
        );
    }

    #[test]
    fn unloadable_files_explain_why() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "malformed.json",
                "{\n  \"formatVersion\": \"1\"\n  \"spec\": {}\n}\n",
            )?;
            let fs = FileSystem::new().unwrap();
            let outcomes: Vec<Outcome> = ["malformed.json", "missing.yaml", "spec.md"]
                .into_iter()
                .map(|name| vet_file(&fs, Path::new(name)))
                .collect();
            let document = parsed(&outcomes, false);
            assert_eq!(document["passed"], false);
            assert_eq!(
                document["files"],
                json!([
                    {
                        "path": "malformed.json",
                        "passed": false,
                        "error": {
                            "message": "could not parse malformed.json as JSON",
                            "labels": [{
                                "message": "expected `,` or `}`",
                                "line": 3,
                                "column": 3,
                                "offset": 27,
                                "length": 1
                            }]
                        }
                    },
                    {
                        "path": "missing.yaml",
                        "passed": false,
                        "error": {
                            "message": "could not read missing.yaml",
                            "causes": ["No such file or directory (os error 2)"]
                        }
                    },
                    {
                        "path": "spec.md",
                        "passed": false,
                        "error": {
                            "message": "spec.md is not a YAML, JSON, or TOML file",
                            "help": "a requirements file's name must end in `.yaml`, `.yml`, `.json`, or `.toml`"
                        }
                    }
                ])
            );
            Ok(())
        });
    }

    #[test]
    fn label_columns_count_characters() {
        Jail::expect_with(|jail| {
            // `é` takes two bytes but one column.
            let source = r#"{"$schema": "é", "formatVersion": "1", "spec": 7}"#;
            jail.create_file("bad.json", source)?;
            let fs = FileSystem::new().unwrap();
            let outcome = vet_file(&fs, Path::new("bad.json"));
            let document = parsed(&[outcome], false);
            let label = &document["files"][0]["error"]["labels"][0];
            assert!(label["line"].as_u64().is_some(), "{document:#}");
            let (line, column, offset) = (
                label["line"].as_u64().unwrap(),
                label["column"].as_u64().unwrap(),
                label["offset"].as_u64().unwrap(),
            );
            assert_eq!(line, 1);
            let line_start: usize = source
                .split_inclusive('\n')
                .take(line as usize - 1)
                .map(str::len)
                .sum();
            let expected_column = source[line_start..offset as usize].chars().count() + 1;
            assert_eq!(column as usize, expected_column, "{document:#}");
            Ok(())
        });
    }
}
