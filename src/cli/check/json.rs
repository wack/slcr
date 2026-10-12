//! `slcr check --format json`: one JSON document describing everything
//! `check` did, for agents and CI to consume.
//!
//! ```json
//! {
//!   "passed": false,
//!   "interrupted": false,
//!   "descriptors": [
//!     { "tool": "cargo-nextest", "status": "already-installed" },
//!     { "tool": "jev", "status": "installed", "pin": "blake3:3aa3…" }
//!   ],
//!   "vet": { "passed": true, "project": {}, "files": [], "validators": [] },
//!   "validators": [
//!     {
//!       "file": "src/items/.validators.slcr.toml",
//!       "position": 1,
//!       "requirement": "todo-api/REQ-002",
//!       "tool": "cargo-nextest",
//!       "outcome": "red",
//!       "reason": { "code": "exit-code" },
//!       "exitCode": 100,
//!       "durationMs": 2310,
//!       "stdout": { "text": "…", "bytes": 1520, "truncated": false, "cutOff": false },
//!       "stderr": { "text": "…", "bytes": 88, "truncated": false, "cutOff": false }
//!     }
//!   ],
//!   "specs": [
//!     {
//!       "name": "todo-api",
//!       "file": "SPEC.slcr.toml",
//!       "passed": false,
//!       "summary": {
//!         "satisfied": 6, "unsatisfied": 1, "unknown": 3,
//!         "violations": 1, "warnings": 0, "informational": 0, "coverageGaps": 2
//!       },
//!       "requirements": [{ "id": "REQ-001", "status": "unknown" }],
//!       "findings": [
//!         {
//!           "code": "unsatisfied",
//!           "requirement": "REQ-002",
//!           "modality": "MUST",
//!           "severity": "violation",
//!           "message": "REQ-002, a MUST requirement, is unsatisfied"
//!         }
//!       ],
//!       "coverageGaps": ["REQ-004", "REQ-006"]
//!     }
//!   ]
//! }
//! ```
//!
//! `passed` is the command's verdict. Each part is present once `check`
//! got that far:
//!
//! - `descriptors`, unless `--offline` or the manifest couldn't be read,
//!   has every declared tool: its `status` is `already-installed`,
//!   `installed` (with the `pin`, and what it `replaced`, `corrupted` or
//!   `unreadable`, if the store held a damaged copy), or `failed`, with an
//!   `error` shaped like one of `vet`'s. If one failed, the document ends
//!   there.
//! - `vet` is the document `slcr vet --format json` writes for the
//!   project. If it didn't pass (warnings aside), the document ends there.
//! - `validators` has every validator, by validator file, then by
//!   `position` in the file, from 1, with its `criterion` if it has one.
//!   Its `outcome` is `green`, `red`, `could-not-run` (with an `error`), or
//!   `interrupted`. A red one has a `reason`, whose `code` is `exit-code`,
//!   `terminated` (with the `signal`, where known), `timed-out`, or `lost`
//!   (with a `detail`). A run has an `exitCode` if it exited with one, its
//!   `durationMs`, and all of each output stream slcr kept: its last
//!   64 KiB, as `text`, with bytes that aren't UTF-8 replaced; `bytes`
//!   counts all it wrote, `truncated` says earlier output was dropped, and
//!   `cutOff` that the stream was still open when slcr stopped reading it.
//! - `specs`, absent if `check` was interrupted, has every specification:
//!   each requirement's `status` (`satisfied`, `unsatisfied`, or
//!   `unknown`) in document order, its findings, each with a kebab-case
//!   `code` (`unsatisfied` or `absorbed`, with `absorbedBy`) and the fields
//!   that go with it, its `severity` (`violation`, `warning`, or
//!   `informational`), `message`, and any `help`, and the leaves no
//!   validator checks, its `coverageGaps`.

use serde::Serialize;

use super::schedule::{Finished, Plan};
use super::verdict::Evaluated;
use crate::cli::vet::json::{Document as VetDocument, ErrorJson};
use crate::project::QualifiedRequirementId;
use crate::run::process::{Captured, Outcome, Red};
use crate::satisfaction::{Finding, Severity, Status, Summary};
use crate::spec::id::{CriterionId, RequirementId};
use crate::spec::text::SpecName;
use crate::tools::ToolKey;
use crate::tools::install::{Damage, Installed, Status as InstallStatus};

/// Everything `check` did, as far as it got.
#[derive(Default, Serialize)]
pub(super) struct Document<'a> {
    pub(super) passed: bool,
    pub(super) interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) descriptors: Option<Vec<DescriptorJson<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) vet: Option<VetDocument<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) validators: Option<Vec<ValidatorJson<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) specs: Option<Vec<SpecJson<'a>>>,
}

impl Document<'_> {
    pub(super) fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("what `check` did serializes to JSON")
    }
}

/// What installing one declared tool's descriptor did.
#[derive(Serialize)]
pub(super) struct DescriptorJson<'a> {
    tool: &'a ToolKey,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replaced: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorJson>,
}

/// What installing each declared tool's descriptor did.
pub(super) fn descriptors(installed: &Installed) -> Vec<DescriptorJson<'_>> {
    installed
        .tools()
        .map(|(tool, outcome)| {
            let mut json = DescriptorJson {
                tool,
                status: "installed",
                pin: None,
                replaced: None,
                error: None,
            };
            match outcome {
                Ok(InstallStatus::AlreadyInstalled) => json.status = "already-installed",
                Ok(InstallStatus::Installed { pin, replaced }) => {
                    json.pin = Some(pin.to_string());
                    json.replaced = replaced.map(|damage| match damage {
                        Damage::Corrupted => "corrupted",
                        Damage::Unreadable => "unreadable",
                    });
                }
                Err(error) => {
                    json.status = "failed";
                    json.error = Some(ErrorJson::new(error));
                }
            }
            json
        })
        .collect()
}

/// One validator, and how running it ended.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ValidatorJson<'a> {
    /// Relative to the working directory.
    file: String,
    position: usize,
    requirement: &'a QualifiedRequirementId,
    #[serde(skip_serializing_if = "Option::is_none")]
    criterion: Option<&'a CriterionId>,
    tool: &'a ToolKey,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<ReasonJson<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<StreamJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<StreamJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorJson>,
}

/// Why a validator is red.
#[derive(Serialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
enum ReasonJson<'a> {
    ExitCode,
    Terminated {
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
    TimedOut,
    Lost {
        detail: &'a str,
    },
}

impl<'a> ReasonJson<'a> {
    fn new(red: &'a Red) -> Self {
        match red {
            Red::ExitCode(_) => Self::ExitCode,
            Red::Terminated { signal } => Self::Terminated { signal: *signal },
            Red::TimedOut => Self::TimedOut,
            Red::Lost { reason } => Self::Lost { detail: reason },
        }
    }
}

/// What slcr kept of one of a run's output streams.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamJson {
    text: String,
    bytes: u64,
    truncated: bool,
    cut_off: bool,
}

impl StreamJson {
    fn new(captured: &Captured) -> Self {
        Self {
            text: String::from_utf8_lossy(captured.bytes()).into_owned(),
            bytes: captured.total(),
            truncated: captured.truncated(),
            cut_off: captured.cut_off(),
        }
    }
}

/// Every validator of `plan`, and how running it ended.
pub(super) fn validators<'a>(
    plan: &'a Plan,
    results: &'a [Option<Finished>],
) -> Vec<ValidatorJson<'a>> {
    plan.jobs()
        .zip(results)
        .map(|(job, finished)| {
            let validator = job.validator;
            let mut json = ValidatorJson {
                file: job.file.file().shown().display().to_string(),
                position: job.position,
                requirement: validator.requirement(),
                criterion: validator.criterion().as_ref(),
                tool: validator.tool(),
                outcome: "interrupted",
                reason: None,
                exit_code: None,
                duration_ms: None,
                stdout: None,
                stderr: None,
                error: None,
            };
            match finished {
                None => {}
                Some(Err(failure)) => {
                    json.outcome = "could-not-run";
                    json.error = Some(ErrorJson::of(failure));
                }
                Some(Ok(run)) => {
                    match run.outcome() {
                        Outcome::Green => json.outcome = "green",
                        Outcome::Red(red) => {
                            json.outcome = "red";
                            json.reason = Some(ReasonJson::new(red));
                        }
                    }
                    json.exit_code = *run.exit_code();
                    json.duration_ms = Some(run.duration().as_millis());
                    json.stdout = Some(StreamJson::new(run.stdout()));
                    json.stderr = Some(StreamJson::new(run.stderr()));
                }
            }
            json
        })
        .collect()
}

/// One specification, evaluated.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SpecJson<'a> {
    name: &'a SpecName,
    /// Relative to the working directory.
    file: String,
    passed: bool,
    summary: Summary,
    requirements: Vec<RequirementJson>,
    findings: Vec<FindingJson<'a>>,
    coverage_gaps: &'a [RequirementId],
}

#[derive(Serialize)]
struct RequirementJson {
    id: RequirementId,
    status: Status,
}

#[derive(Serialize)]
struct FindingJson<'a> {
    /// The `code` and the fields that go with it.
    #[serde(flatten)]
    finding: &'a Finding,
    severity: Severity,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
}

/// Every specification, evaluated.
pub(super) fn specs<'a>(evaluated: &'a [Evaluated<'a>], deny_warnings: bool) -> Vec<SpecJson<'a>> {
    evaluated
        .iter()
        .map(|spec| {
            let evaluation = &spec.evaluation;
            SpecJson {
                name: spec.name,
                file: spec.file.shown().display().to_string(),
                passed: evaluation.passes(deny_warnings),
                summary: evaluation.summary(),
                requirements: evaluation
                    .statuses()
                    .iter()
                    .map(|&(id, status)| RequirementJson { id, status })
                    .collect(),
                findings: evaluation
                    .findings()
                    .iter()
                    .map(|finding| FindingJson {
                        finding,
                        severity: finding.severity(),
                        message: finding.to_string(),
                        help: miette::Diagnostic::help(finding).map(|help| help.to_string()),
                    })
                    .collect(),
                coverage_gaps: evaluation.coverage_gaps(),
            }
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::super::schedule::{Ran, run_all, tests::plan};
    use super::*;
    use crate::cli::block_on;
    use crate::run::SpawnFailure;

    #[test]
    fn validators_say_how_each_run_ended() {
        let (_dir, plan) = plan(&["printf 'caf\\351'; exit 4", "true", "true"]);
        let plan = std::sync::Arc::new(plan);
        let Ran { mut results, .. } = block_on(run_all(
            plan.clone(),
            std::num::NonZeroUsize::MIN,
            std::future::pending(),
            |_, _| Ok::<_, std::convert::Infallible>(()),
        ))
        .unwrap()
        .unwrap();
        results[1] = Some(Err(SpawnFailure::ProgramNotFound {
            program: "sh".to_owned(),
        }));
        results[2] = None;
        let json: Value = serde_json::to_value(validators(&plan, &results)).unwrap();
        let mut red = json[0].clone();
        assert!(red["durationMs"].as_u64().unwrap() < Duration::from_secs(30).as_millis() as u64);
        red["durationMs"] = json!(0);
        assert_eq!(
            red,
            json!({
                "file": ".validators.slcr.toml",
                "position": 1,
                "requirement": "todo-api/REQ-001",
                "tool": "sh",
                "outcome": "red",
                "reason": { "code": "exit-code" },
                "exitCode": 4,
                "durationMs": 0,
                "stdout": { "text": "caf\u{fffd}", "bytes": 4, "truncated": false, "cutOff": false },
                "stderr": { "text": "", "bytes": 0, "truncated": false, "cutOff": false }
            })
        );
        assert_eq!(
            json[1],
            json!({
                "file": ".validators.slcr.toml",
                "position": 2,
                "requirement": "todo-api/REQ-001",
                "tool": "sh",
                "outcome": "could-not-run",
                "error": {
                    "message": "`sh` was not found on PATH",
                    "help": "install it, or put its directory on PATH; slcr searches the PATH the \
                             tool would see (the descriptor's `env.PATH`, if it sets one), but \
                             never an empty or relative entry, nor the current directory"
                }
            })
        );
        assert_eq!(json[2]["outcome"], "interrupted");
        assert!(json[2].get("durationMs").is_none());
    }

    #[test]
    fn every_reason_to_be_red_has_a_code() {
        let cases = [
            (Red::ExitCode(1), json!({ "code": "exit-code" })),
            (
                Red::Terminated { signal: Some(9) },
                json!({ "code": "terminated", "signal": 9 }),
            ),
            (
                Red::Terminated { signal: None },
                json!({ "code": "terminated" }),
            ),
            (Red::TimedOut, json!({ "code": "timed-out" })),
            (
                Red::Lost {
                    reason: "gone".to_owned(),
                },
                json!({ "code": "lost", "detail": "gone" }),
            ),
        ];
        for (red, expected) in cases {
            assert_eq!(
                serde_json::to_value(ReasonJson::new(&red)).unwrap(),
                expected
            );
        }
    }
}
