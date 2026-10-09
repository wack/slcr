//! `slcr check --format json`: one JSON document describing every file and
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

use miette::{Diagnostic, LabeledSpan, Report as ErrorReport};
use serde::Serialize;

use super::Outcome;
use crate::spec::check::Finding;

/// The JSON document describing `outcomes`.
pub(super) fn document(outcomes: &[Outcome], deny_warnings: bool) -> String {
    let files: Vec<File<'_>> = outcomes
        .iter()
        .map(|outcome| File::new(outcome, deny_warnings))
        .collect();
    let document = Document {
        passed: files.iter().all(|file| file.passed),
        files,
    };
    serde_json::to_string_pretty(&document).expect("check results serialize to JSON")
}

#[derive(Serialize)]
struct Document<'a> {
    /// Whether every file passed.
    passed: bool,
    files: Vec<File<'a>>,
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
            Ok(report) => FileResult::Checked {
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
    Checked {
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

/// Why a file couldn't be loaded.
#[derive(Serialize)]
struct ErrorJson {
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
    fn new(error: &ErrorReport) -> Self {
        let diagnostic: &dyn Diagnostic = error.as_ref();
        let labels = diagnostic
            .labels()
            .into_iter()
            .flatten()
            .filter_map(|label| LabelJson::new(diagnostic, &label))
            .collect();
        Self {
            message: error.to_string(),
            causes: error.chain().skip(1).map(ToString::to_string).collect(),
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
    use crate::cli::check::check_file;
    use crate::fs::FileSystem;
    use crate::spec::graph::UncheckedDocument;

    const TODO_API_JSON: &str = include_str!("../../../tests/fixtures/todo-api.spec.json");

    fn todo_api() -> Value {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    fn outcome(path: &str, value: Value) -> Outcome {
        let report = serde_json::from_value::<UncheckedDocument>(value)
            .unwrap()
            .report();
        Outcome {
            path: PathBuf::from(path),
            result: Ok(report),
        }
    }

    fn parsed(outcomes: &[Outcome], deny_warnings: bool) -> Value {
        serde_json::from_str(&document(outcomes, deny_warnings)).unwrap()
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
    fn an_empty_check_passes() {
        assert_eq!(parsed(&[], false), json!({ "passed": true, "files": [] }));
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
                .map(|name| check_file(&fs, Path::new(name)))
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
            let outcome = check_file(&fs, Path::new("bad.json"));
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
