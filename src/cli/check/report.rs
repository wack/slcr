//! What `slcr check` writes for people: a status line per validator and a
//! summary line per specification on stdout, and the diagnostics of the
//! validators that couldn't be run or were red, and of unsatisfied
//! requirements, on stderr.

use std::fmt::{self, Display};

use miette::{Diagnostic, LabeledSpan, Severity, SourceCode};

use super::schedule::{Finished, Job};
use super::verdict::Evaluated;
use crate::cli::findings::counted;
use crate::run::SpawnFailure;
use crate::run::process::{Captured, Outcome, Red, Run};
use crate::satisfaction::{Finding, Severity as FindingSeverity};

/// How many of the last lines of each output stream of a red validator
/// are shown.
const TAIL: usize = 20;

/// A validator as `check` names it: its file, its position in the file,
/// what it checks, and its tool, e.g.
/// `src/.validators.slcr.toml validator 1 (todo-api/REQ-002, cargo-nextest)`.
pub(super) struct Name<'a>(pub(super) Job<'a>);

impl Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Job {
            file,
            position,
            validator,
        } = self.0;
        let path = file.file().shown().display();
        write!(
            f,
            "{path} validator {position} ({}",
            validator.requirement()
        )?;
        if let Some(criterion) = validator.criterion() {
            write!(f, " {criterion}")?;
        }
        write!(f, ", {})", validator.tool())
    }
}

/// A validator's status line: `green`, `red` and why, `could not be run`,
/// or, if it hadn't finished, `interrupted`.
pub(super) fn status(job: Job<'_>, finished: Option<&Finished>) -> String {
    let outcome = match finished {
        None => "interrupted".to_owned(),
        Some(Err(_)) => "could not be run".to_owned(),
        Some(Ok(run)) => match run.outcome() {
            Outcome::Green => "green".to_owned(),
            Outcome::Red(red) => format!("red ({})", why(red)),
        },
    };
    format!("{}: {outcome}", Name(job))
}

/// Why a validator is red, in a few words.
fn why(red: &Red) -> String {
    match red {
        Red::ExitCode(code) => format!("exit {code}"),
        Red::Terminated {
            signal: Some(signal),
        } => format!("killed by signal {signal}"),
        Red::Terminated { signal: None } => "terminated".to_owned(),
        Red::TimedOut => "timed out".to_owned(),
        Red::Lost { reason } => format!("lost track of it: {reason}"),
    }
}

/// A validator that couldn't be run, labeled in its validator file.
#[derive(Debug)]
pub(super) struct Unrunnable<'a> {
    job: Job<'a>,
    failure: &'a SpawnFailure,
}

impl<'a> Unrunnable<'a> {
    pub(super) fn new(job: Job<'a>, failure: &'a SpawnFailure) -> Self {
        Self { job, failure }
    }
}

impl Display for Unrunnable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} could not be run", Name(self.job))
    }
}

impl std::error::Error for Unrunnable<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.failure)
    }
}

impl Diagnostic for Unrunnable<'_> {
    fn help<'b>(&'b self) -> Option<Box<dyn Display + 'b>> {
        self.failure.help()
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&**self.job.file.source())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.job.file.spans(self.job.position - 1).entry?;
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            None, span,
        ))))
    }
}

/// A red validator. It is as severe as the finding on its requirement,
/// which a red validator always leaves unsatisfied.
#[derive(Debug)]
pub(super) struct RedValidator {
    headline: String,
    severity: Severity,
}

impl RedValidator {
    /// `severity` is that of the finding on the validator's requirement, if
    /// it was evaluated.
    pub(super) fn new(
        job: Job<'_>,
        red: &Red,
        severity: Option<FindingSeverity>,
        deny_warnings: bool,
    ) -> Self {
        Self {
            headline: format!("{}: red ({})", Name(job), why(red)),
            severity: match severity {
                Some(FindingSeverity::Violation) => Severity::Error,
                Some(FindingSeverity::Informational) => Severity::Advice,
                Some(FindingSeverity::Warning) | None if deny_warnings => Severity::Error,
                Some(FindingSeverity::Warning) | None => Severity::Warning,
            },
        }
    }
}

impl Display for RedValidator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.headline)
    }
}

impl std::error::Error for RedValidator {}

impl Diagnostic for RedValidator {
    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }
}

/// The end of a red validator's output: the last lines of its stderr,
/// then of its stdout, each saying what was left out, as lines of text to
/// write under its diagnostic. Empty if it wrote nothing.
pub(super) fn output(run: &Run) -> String {
    [("stderr", run.stderr()), ("stdout", run.stdout())]
        .into_iter()
        .filter_map(|(stream, captured)| tail(stream, captured))
        .collect()
}

/// The last [TAIL] lines of `captured`, from the output stream `stream`,
/// under a header, or none if nothing was written to it. Bytes that
/// aren't UTF-8 are replaced.
fn tail(stream: &str, captured: &Captured) -> Option<String> {
    if captured.total() == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(captured.bytes());
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let shown = &lines[lines.len().saturating_sub(TAIL)..];
    let header = if captured.truncated() {
        format!(
            "{stream}, last {} of its {} bytes:",
            counted(shown.len(), "line"),
            captured.total()
        )
    } else if shown.len() < lines.len() {
        format!("{stream}, last {} of {} lines:", shown.len(), lines.len())
    } else {
        format!("{stream}:")
    };
    let mut tail = format!("  {header}\n");
    for line in shown {
        tail.push_str("  │ ");
        tail.push_str(line.strip_suffix('\r').unwrap_or(line));
        tail.push('\n');
    }
    if captured.truncated() {
        tail.push_str("  (slcr keeps only the last 64 KiB of each stream)\n");
    }
    if captured.cut_off() {
        tail.push_str(
            "  (it was still open after the validator exited; slcr stopped reading it)\n",
        );
    }
    Some(tail)
}

/// Everything evaluation found in one specification, as one diagnostic
/// whose related diagnostics are the findings.
///
/// It is an error if the specification fails, a warning if it has
/// warnings that aren't denied, and advice otherwise.
#[derive(Debug)]
pub(super) struct SpecFindings<'a> {
    headline: String,
    severity: Severity,
    findings: &'a [Finding],
}

impl<'a> SpecFindings<'a> {
    pub(super) fn new(spec: &'a Evaluated<'a>, deny_warnings: bool) -> Self {
        let evaluation = &spec.evaluation;
        let summary = evaluation.summary();
        let counts = [
            (summary.violations, "violation"),
            (summary.warnings, "warning"),
            (summary.informational, "informational finding"),
        ];
        let counts: Vec<String> = counts
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, noun)| counted(count, noun))
            .collect();
        Self {
            headline: format!(
                "{} ({}): {}",
                spec.name,
                spec.file.shown().display(),
                counts.join(", ")
            ),
            severity: if !evaluation.passes(deny_warnings) {
                Severity::Error
            } else if summary.warnings > 0 {
                Severity::Warning
            } else {
                Severity::Advice
            },
            findings: evaluation.findings(),
        }
    }
}

impl Display for SpecFindings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.headline)
    }
}

impl std::error::Error for SpecFindings<'_> {}

impl Diagnostic for SpecFindings<'_> {
    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }

    fn related<'b>(&'b self) -> Option<Box<dyn Iterator<Item = &'b dyn Diagnostic> + 'b>> {
        Some(Box::new(
            self.findings
                .iter()
                .map(|finding| finding as &dyn Diagnostic),
        ))
    }
}

/// A specification's summary line, e.g.
/// `todo-api: 6 satisfied, 1 unsatisfied, 3 unknown (2 coverage gaps)`.
pub(super) fn summary(spec: &Evaluated<'_>) -> String {
    let summary = spec.evaluation.summary();
    let mut line = format!(
        "{}: {} satisfied, {} unsatisfied, {} unknown",
        spec.name, summary.satisfied, summary.unsatisfied, summary.unknown
    );
    if summary.coverage_gaps > 0 {
        line.push_str(&format!(
            " ({})",
            counted(summary.coverage_gaps, "coverage gap")
        ));
    }
    line
}

#[cfg(all(test, unix))]
mod tests {
    use std::{path::Path, sync::Arc};

    use super::super::schedule::{Plan, Ran, run_all, tests::plan};
    use super::super::verdict::{evaluate_all, verdicts};
    use super::*;
    use crate::cli::block_on;
    use crate::project::ProjectFile;
    use crate::spec::graph::UnvettedDocument;
    use crate::spec::id::RequirementId;
    use crate::terminal::render_plain;

    const TODO_API_JSON: &str = include_str!("../../../tests/fixtures/todo-api.spec.json");

    fn ran(plan: Plan) -> (Arc<Plan>, Ran) {
        let plan = Arc::new(plan);
        let ran = block_on(run_all(
            plan.clone(),
            std::num::NonZeroUsize::MIN,
            std::future::pending(),
            |_, _| Ok::<_, std::convert::Infallible>(()),
        ))
        .unwrap()
        .unwrap();
        (plan, ran)
    }

    #[test]
    fn each_validator_has_a_status_line() {
        let (_dir, plan) = plan(&["true", "exit 3", "kill -KILL $$"]);
        let (plan, ran) = ran(plan);
        let statuses: Vec<String> = plan
            .jobs()
            .zip(&ran.results)
            .map(|(job, finished)| status(job, finished.as_ref()))
            .collect();
        assert_eq!(
            statuses,
            [
                ".validators.slcr.toml validator 1 (todo-api/REQ-001, sh): green",
                ".validators.slcr.toml validator 2 (todo-api/REQ-001, sh): red (exit 3)",
                ".validators.slcr.toml validator 3 (todo-api/REQ-001, sh): red (killed by signal 9)",
            ]
        );
        assert_eq!(
            status(plan.job(0), None),
            ".validators.slcr.toml validator 1 (todo-api/REQ-001, sh): interrupted"
        );
    }

    #[test]
    fn every_reason_to_be_red_is_named() {
        let cases = [
            (Red::ExitCode(1), "exit 1"),
            (Red::Terminated { signal: Some(15) }, "killed by signal 15"),
            (Red::Terminated { signal: None }, "terminated"),
            (Red::TimedOut, "timed out"),
            (
                Red::Lost {
                    reason: "gone".to_owned(),
                },
                "lost track of it: gone",
            ),
        ];
        for (red, reason) in cases {
            assert_eq!(why(&red), reason);
        }
    }

    #[test]
    fn a_validator_that_cannot_be_run_is_labeled_in_its_file() {
        let (_dir, plan) = plan(&["true"]);
        let failure = SpawnFailure::ProgramNotFound {
            program: "sh".to_owned(),
        };
        let rendered = render_plain(&Unrunnable::new(plan.job(0), &failure));
        assert!(
            rendered.starts_with(
                "  × .validators.slcr.toml validator 1 (todo-api/REQ-001, sh) could not be run\n  \
                 ╰─▶ `sh` was not found on PATH\n"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("[.validators.slcr.toml:3:1]"),
            "{rendered}"
        );
        assert!(rendered.contains("help: install it"), "{rendered}");
    }

    #[test]
    fn a_red_validator_is_as_severe_as_its_requirement_s_finding() {
        let (_dir, plan) = plan(&["exit 1"]);
        let job = plan.job(0);
        let red = Red::ExitCode(1);
        let cases = [
            (Some(FindingSeverity::Violation), false, Severity::Error),
            (Some(FindingSeverity::Warning), false, Severity::Warning),
            (Some(FindingSeverity::Warning), true, Severity::Error),
            (Some(FindingSeverity::Informational), true, Severity::Advice),
            (None, false, Severity::Warning),
        ];
        for (finding, deny_warnings, severity) in cases {
            let diagnostic = RedValidator::new(job, &red, finding, deny_warnings);
            assert_eq!(diagnostic.severity(), Some(severity), "{finding:?}");
            assert_eq!(
                diagnostic.to_string(),
                ".validators.slcr.toml validator 1 (todo-api/REQ-001, sh): red (exit 1)"
            );
        }
    }

    fn lines(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn short_output_is_shown_whole() {
        let captured = Captured::of(&[b"one\r\n", b"two"], false);
        assert_eq!(
            tail("stderr", &captured).unwrap(),
            "  stderr:\n  │ one\n  │ two\n"
        );
        assert_eq!(tail("stdout", &Captured::default()), None);
    }

    #[test]
    fn only_the_last_lines_are_shown() {
        let captured = Captured::of(&[lines(25).as_bytes()], false);
        let tail = tail("stdout", &captured).unwrap();
        let expected: String = (6..=25).map(|n| format!("  │ line {n}\n")).collect();
        assert_eq!(tail, format!("  stdout, last 20 of 25 lines:\n{expected}"));
    }

    #[test]
    fn output_slcr_did_not_keep_or_finish_reading_is_noted() {
        // Far more than the 64 KiB slcr keeps.
        let output = lines(20_000);
        let captured = Captured::of(&[output.as_bytes()], true);
        let tail = tail("stderr", &captured).unwrap();
        let mut tail = tail.lines();
        assert_eq!(
            tail.next(),
            Some(format!("  stderr, last 20 lines of its {} bytes:", output.len()).as_str())
        );
        assert_eq!(tail.next(), Some("  │ line 19981"));
        let notes: Vec<&str> = tail.skip(19).collect();
        assert_eq!(
            notes,
            [
                "  (slcr keeps only the last 64 KiB of each stream)",
                "  (it was still open after the validator exited; slcr stopped reading it)",
            ]
        );
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let captured = Captured::of(&[b"caf\xe9\n"], false);
        assert_eq!(
            tail("stdout", &captured).unwrap(),
            "  stdout:\n  │ caf\u{fffd}\n"
        );
    }

    #[test]
    fn stderr_comes_before_stdout() {
        let (_dir, plan) = plan(&["echo out; echo err >&2; exit 1"]);
        let (_, ran) = ran(plan);
        let Some(Ok(run)) = &ran.results[0] else {
            panic!("{:?}", ran.results);
        };
        assert_eq!(output(run), "  stderr:\n  │ err\n  stdout:\n  │ out\n");
    }

    /// Check what evaluating the canon example finds after REQ-001's
    /// validators ran `scripts`.
    fn evaluated(scripts: &[&str], check: impl FnOnce(&[Evaluated<'_>])) {
        let (_dir, plan) = plan(scripts);
        let (plan, ran) = ran(plan);
        let file = ProjectFile::new(Path::new("/p"), Path::new("/p"), "SPEC.slcr.json".into());
        let document: UnvettedDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        let specs = [(file, document)];
        check(&evaluate_all(&specs, &verdicts(&plan, &ran.results)));
    }

    #[test]
    fn a_summary_line_counts_statuses_and_coverage_gaps() {
        // REQ-001 is refined, so its own validator alone can't satisfy it.
        evaluated(&["true"], |evaluated| {
            assert_eq!(
                summary(&evaluated[0]),
                "todo-api: 0 satisfied, 0 unsatisfied, 10 unknown (5 coverage gaps)"
            );
        });
        evaluated(&["exit 1"], |evaluated| {
            assert_eq!(
                summary(&evaluated[0]),
                "todo-api: 0 satisfied, 1 unsatisfied, 9 unknown (5 coverage gaps)"
            );
        });
    }

    #[test]
    fn a_summary_line_leaves_out_coverage_gaps_when_there_are_none() {
        let file = ProjectFile::new(Path::new("/p"), Path::new("/p"), "SPEC.slcr.json".into());
        let document: UnvettedDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        let name = document.document().spec().name().clone();
        // Every leaf of the canon example passes.
        let verdicts: crate::satisfaction::Verdicts = [4, 6, 7, 9, 10]
            .map(|id| (RequirementId::new(id), crate::satisfaction::Outcome::Pass))
            .into_iter()
            .collect();
        let specs = [(file, document)];
        let evaluated = evaluate_all(&specs, &[(name, verdicts)].into());
        assert_eq!(
            summary(&evaluated[0]),
            "todo-api: 10 satisfied, 0 unsatisfied, 0 unknown"
        );
    }

    #[test]
    fn spec_findings_are_one_diagnostic() {
        evaluated(&["exit 1"], |evaluated| {
            let findings = SpecFindings::new(&evaluated[0], false);
            assert_eq!(findings.severity(), Some(Severity::Error));
            assert_eq!(
                render_plain(&findings),
                "  × todo-api (SPEC.slcr.json): 1 violation\n\
                 \n\
                 Error: \n  \
                 × REQ-001, a MUST requirement, is unsatisfied\n"
            );
        });
    }
}
