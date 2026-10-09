use std::{
    fmt::{self, Display},
    path::Path,
};

use miette::{Diagnostic, Severity};

use crate::spec::check::{Finding, Report};

/// Whether a file with `report` passes: it breaks no invariant and, if
/// warnings are denied, has no warning.
pub(super) fn passes(report: &Report, deny_warnings: bool) -> bool {
    report.violations().is_empty() && (!deny_warnings || report.warnings().is_empty())
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
pub(super) fn tally(report: &Report) -> String {
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
pub(super) struct FileFindings<'a> {
    headline: String,
    severity: Severity,
    report: &'a Report,
}

impl<'a> FileFindings<'a> {
    pub(super) fn new(path: &Path, report: &'a Report, deny_warnings: bool) -> Self {
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::spec::graph::UncheckedDocument;
    use crate::terminal::render_plain;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");

    /// The canon example, with REQ-001's `refinement` removed though REQ-002
    /// and REQ-003 refine it.
    fn with_violation() -> Value {
        let mut value: Value = serde_json::from_str(TODO_API_JSON).unwrap();
        value["root"]["children"][0]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("refinement");
        value
    }

    /// The canon example, with REQ-003's body stripped of its modality.
    fn with_warning() -> Value {
        let mut value: Value = serde_json::from_str(TODO_API_JSON).unwrap();
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
    fn whether_a_report_passes_depends_on_its_findings() {
        let clean = Report::default();
        assert!(passes(&clean, false));
        assert!(passes(&clean, true));
        assert!(passes(&report(with_warning()), false));
        assert!(!passes(&report(with_warning()), true));
        assert!(!passes(&report(with_violation()), false));
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
}
