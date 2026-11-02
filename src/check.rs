//! The document `slcr check --format json` writes, read back: for a
//! program that stores what `slcr check` reported elsewhere, such as a
//! server that receives it from the sandbox that ran the validators.
//!
//! [Report] deserializes exactly the shape the CLI writes, field for field
//! (see `src/cli/check/json.rs`): an unknown field, a missing required one,
//! or a value of the wrong type fails. The `vet` document `check` embeds,
//! and the `error` of a validator that couldn't run or a descriptor that
//! couldn't be installed, are kept as JSON objects, unread.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::project::QualifiedRequirementId;
pub use crate::satisfaction::{Severity, Status};
use crate::spec::id::{CriterionId, RequirementId};
use crate::spec::node::Modality;
use crate::spec::text::SpecName;
use crate::tools::ToolKey;
use crate::tools::pin::Pin;

/// Everything `slcr check` did, as far as it got.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The command's verdict.
    pub passed: bool,
    pub interrupted: bool,
    /// What installing each declared tool's descriptor did, unless
    /// `--offline` or the manifest couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptors: Option<Vec<Descriptor>>,
    /// The document `slcr vet --format json` writes for the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vet: Option<Map<String, Value>>,
    /// Every validator, by validator file, then by position in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<Validator>>,
    /// Every specification, evaluated; absent if `check` was interrupted
    /// or stopped before running validators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specs: Option<Vec<Spec>>,
}

impl Report {
    /// Read `json` as the document `slcr check --format json` writes.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// What installing one declared tool's descriptor did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub tool: ToolKey,
    pub status: InstallStatus,
    /// The pin it was installed under, if it was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
    /// The damaged copy it replaced, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<Damage>,
    /// Why it couldn't be installed, shaped like an error of `vet`'s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Map<String, Value>>,
}

/// Whether a descriptor was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallStatus {
    AlreadyInstalled,
    Installed,
    Failed,
}

/// What was wrong with a descriptor the store held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Damage {
    Corrupted,
    Unreadable,
}

/// One validator, and how running it ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Validator {
    /// The validator file, relative to the working directory.
    pub file: String,
    /// Its position in the file, from 1.
    pub position: usize,
    pub requirement: QualifiedRequirementId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion: Option<CriterionId>,
    pub tool: ToolKey,
    pub outcome: RunOutcome,
    /// Why a red validator is red.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<Stream>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<Stream>,
    /// Why a validator that couldn't run couldn't, shaped like an error of
    /// `vet`'s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Map<String, Value>>,
}

/// How running a validator ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunOutcome {
    Green,
    Red,
    CouldNotRun,
    Interrupted,
}

/// Why a validator is red.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reason {
    ExitCode,
    Terminated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
    TimedOut,
    Lost {
        detail: String,
    },
}

/// What slcr kept of one of a run's output streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stream {
    /// The last 64 KiB, with bytes that aren't UTF-8 replaced.
    pub text: String,
    /// How many bytes the stream carried in all.
    pub bytes: u64,
    pub truncated: bool,
    pub cut_off: bool,
}

/// One specification, evaluated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Spec {
    pub name: SpecName,
    /// The specification's file, relative to the working directory.
    pub file: String,
    pub passed: bool,
    pub summary: Summary,
    /// Each requirement's status, in document order.
    pub requirements: Vec<RequirementStatus>,
    pub findings: Vec<Finding>,
    /// The leaves no validator checks.
    pub coverage_gaps: Vec<RequirementId>,
}

/// How many requirements have each status, and how many findings each
/// severity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Summary {
    pub satisfied: usize,
    pub unsatisfied: usize,
    pub unknown: usize,
    pub violations: usize,
    pub warnings: usize,
    pub informational: usize,
    pub coverage_gaps: usize,
}

/// One requirement's status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementStatus {
    pub id: RequirementId,
    pub status: Status,
}

/// An unsatisfied requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Finding {
    pub code: FindingCode,
    pub requirement: RequirementId,
    pub modality: Modality,
    /// What the requirement refines, all satisfied, for an `absorbed`
    /// finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorbed_by: Option<Vec<RequirementId>>,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// What kind of finding a [Finding] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingCode {
    Unsatisfied,
    Absorbed,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The stdout of a CLI snapshot, with its redacted durations put back
    /// as numbers.
    fn stdout(snapshot: &str) -> String {
        let start = snapshot.find("----- stdout -----\n").unwrap() + 19;
        let end = snapshot.find("----- stderr -----").unwrap();
        snapshot[start..end].replace("\"[DURATION]\"", "1234")
    }

    const SNAPSHOTS: [&str; 2] = [
        include_str!("../tests/cli/snapshots/cli__check__check_reports_in_json.snap"),
        include_str!(
            "../tests/cli/snapshots/cli__check__check_reports_a_vetting_failure_in_json.snap"
        ),
    ];

    #[test]
    fn what_the_cli_writes_reads_back_unchanged() {
        for snapshot in SNAPSHOTS {
            let written = stdout(snapshot);
            let report = Report::from_json(&written).unwrap();
            let original: Value = serde_json::from_str(&written).unwrap();
            assert_eq!(serde_json::to_value(&report).unwrap(), original);
        }
    }

    #[test]
    fn statuses_and_validators_are_read() {
        let report = Report::from_json(&stdout(SNAPSHOTS[0])).unwrap();
        let validators = report.validators.unwrap();
        assert_eq!(validators[0].requirement.to_string(), "shop/REQ-002");
        assert_eq!(validators[0].outcome, RunOutcome::Red);
        assert_eq!(validators[0].reason, Some(Reason::ExitCode));
        let specs = report.specs.unwrap();
        assert_eq!(specs[0].name.as_str(), "shop");
        assert!(
            specs[0]
                .requirements
                .iter()
                .any(|requirement| requirement.status == Status::Unsatisfied)
        );
    }

    #[test]
    fn other_documents_are_rejected() {
        let mut unknown = serde_json::from_str::<Value>(&stdout(SNAPSHOTS[0])).unwrap();
        unknown["extra"] = json!(true);
        let mut status = serde_json::from_str::<Value>(&stdout(SNAPSHOTS[0])).unwrap();
        status["specs"][0]["requirements"][0]["status"] = json!("green");
        let mut id = serde_json::from_str::<Value>(&stdout(SNAPSHOTS[0])).unwrap();
        id["validators"][0]["requirement"] = json!("REQ-002");
        for document in [
            json!({}),
            json!([]),
            json!({ "passed": true }),
            unknown,
            status,
            id,
        ] {
            assert!(
                serde_json::from_value::<Report>(document.clone()).is_err(),
                "{document}"
            );
        }
    }
}
