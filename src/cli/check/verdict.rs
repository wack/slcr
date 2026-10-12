//! From the validators' results to each requirement's satisfaction, and
//! to whether `check` passes.

use std::{
    collections::BTreeMap,
    fmt::{self, Display},
};

use miette::Diagnostic;
use thiserror::Error;

use super::schedule::{Finished, Plan};
use crate::cli::findings::counted;
use crate::cli::vet::VetFailed;
use crate::project::{ProjectFile, QualifiedRequirementId};
use crate::run::process::Outcome as RunOutcome;
use crate::satisfaction::{Evaluation, Outcome, Severity, Verdicts, evaluate};
use crate::spec::graph::UnvettedDocument;
use crate::spec::id::RequirementId;
use crate::spec::text::SpecName;

/// The verdicts of each specification's validators: every green or red
/// run, under its validator's requirement, whichever criterion it checks.
/// A validator that couldn't be run, or hadn't finished, gives none.
pub(super) fn verdicts(plan: &Plan, results: &[Option<Finished>]) -> BTreeMap<SpecName, Verdicts> {
    let mut verdicts: BTreeMap<SpecName, Verdicts> = BTreeMap::new();
    for (job, finished) in plan.jobs().zip(results) {
        let Some(Ok(run)) = finished else {
            continue;
        };
        let outcome = match run.outcome() {
            RunOutcome::Green => Outcome::Pass,
            RunOutcome::Red(_) => Outcome::Fail,
        };
        let requirement = job.validator.requirement();
        verdicts
            .entry(requirement.spec().clone())
            .or_default()
            .record(*requirement.requirement(), outcome);
    }
    verdicts
}

/// One specification of the project, evaluated.
#[derive(Debug)]
pub(super) struct Evaluated<'a> {
    pub(super) file: &'a ProjectFile,
    pub(super) name: &'a SpecName,
    pub(super) evaluation: Evaluation,
}

impl Evaluated<'_> {
    /// The severity of the finding on `requirement`, if it has one.
    pub(super) fn severity(&self, requirement: RequirementId) -> Option<Severity> {
        self.evaluation
            .findings()
            .iter()
            .find(|finding| finding.requirement() == requirement)
            .map(|finding| finding.severity())
    }
}

/// The severity of the finding on `requirement`, if its specification is
/// among `evaluated` and it has one.
pub(super) fn severity(
    evaluated: &[Evaluated<'_>],
    requirement: &QualifiedRequirementId,
) -> Option<Severity> {
    let spec = evaluated
        .iter()
        .find(|spec| spec.name == requirement.spec())?;
    spec.severity(*requirement.requirement())
}

/// Evaluate each of `specs`, which vetted cleanly, from `verdicts`, in
/// order.
pub(super) fn evaluate_all<'a>(
    specs: &'a [(ProjectFile, UnvettedDocument)],
    verdicts: &BTreeMap<SpecName, Verdicts>,
) -> Vec<Evaluated<'a>> {
    let none = Verdicts::new();
    specs
        .iter()
        .map(|(file, document)| {
            let document = document.document();
            let name = document.spec().name();
            let verdicts = verdicts.get(name).unwrap_or(&none);
            Evaluated {
                file,
                name,
                evaluation: evaluate(document, verdicts),
            }
        })
        .collect()
}

/// What decides whether `check` passes, once vetting has and every
/// validator has finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Tally {
    /// Validators that couldn't be run.
    pub(super) unrunnable: usize,
    /// Unsatisfied MUST and MUST NOT requirements whose failure isn't
    /// absorbed.
    pub(super) violations: usize,
    /// Vetting's warnings and satisfaction's: unsatisfied SHOULD and
    /// SHOULD NOT requirements, and absorbed failures.
    pub(super) warnings: usize,
}

impl Tally {
    pub(super) fn new(
        vet_warnings: usize,
        results: &[Option<Finished>],
        evaluated: &[Evaluated<'_>],
    ) -> Self {
        let summaries = evaluated.iter().map(|spec| spec.evaluation.summary());
        let (violations, warnings) = summaries.fold((0, 0), |(violations, warnings), summary| {
            (violations + summary.violations, warnings + summary.warnings)
        });
        Self {
            unrunnable: results
                .iter()
                .filter(|finished| matches!(finished, Some(Err(_))))
                .count(),
            violations,
            warnings: vet_warnings + warnings,
        }
    }

    /// Succeed unless a validator couldn't be run, a requirement is a
    /// violation, or, if they are denied, there are warnings. Informational
    /// findings and unknown requirements never fail it.
    pub(super) fn verdict(self, deny_warnings: bool) -> Result<(), CheckFailed> {
        let warnings = if deny_warnings { self.warnings } else { 0 };
        if self.unrunnable == 0 && self.violations == 0 && warnings == 0 {
            return Ok(());
        }
        Err(CheckFailed {
            unrunnable: self.unrunnable,
            violations: self.violations,
            warnings,
        })
    }
}

/// The error `slcr check` fails with once every validator has finished.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(super) struct CheckFailed {
    unrunnable: usize,
    violations: usize,
    /// Warnings, counted only when they are denied.
    warnings: usize,
}

impl Display for CheckFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut reasons = Vec::new();
        if self.unrunnable > 0 {
            let validators = counted(self.unrunnable, "validator");
            reasons.push(format!("{validators} could not be run"));
        }
        if self.violations > 0 {
            reasons.push(counted(self.violations, "violation"));
        }
        if self.warnings > 0 {
            reasons.push(counted(self.warnings, "warning"));
        }
        write!(f, "check failed: {}", reasons.join(", "))
    }
}

impl Diagnostic for CheckFailed {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        (self.warnings > 0).then(|| {
            Box::new("`--deny-warnings` makes a warning fail the check") as Box<dyn Display>
        })
    }
}

/// The error `slcr check` fails with when Ctrl-C stops it.
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
#[error(
    "check was interrupted: {finished} of {total} {} finished",
    if *.total == 1 { "validator" } else { "validators" }
)]
#[diagnostic(help(
    "every validator still running was stopped, with what it had started; no requirement was \
     evaluated"
))]
pub(super) struct Interrupted {
    pub(super) finished: usize,
    pub(super) total: usize,
}

/// The error `slcr check` fails with when the project doesn't vet cleanly.
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("the project doesn't vet cleanly, so no validator was run")]
#[diagnostic(help("`slcr vet` reports the same problems; fix them, then check again"))]
pub(super) struct NotVetted {
    #[source]
    pub(super) failed: VetFailed,
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::super::schedule::{Ran, run_all, tests::plan};
    use super::*;
    use crate::cli::block_on;
    use crate::satisfaction::Status;
    use crate::terminal::render_plain;

    const TODO_API_JSON: &str = include_str!("../../../tests/fixtures/todo-api.spec.json");

    /// Run every validator of `plan`, uninterrupted.
    fn ran(plan: Plan) -> (std::sync::Arc<Plan>, Ran) {
        let plan = std::sync::Arc::new(plan);
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

    /// The canon example, named `name`.
    fn spec(name: &str) -> (ProjectFile, UnvettedDocument) {
        let mut value: Value = serde_json::from_str(TODO_API_JSON).unwrap();
        value["spec"]["name"] = json!(name);
        let file = ProjectFile::new(Path::new("/p"), Path::new("/p"), PathBuf::from(name));
        (file, serde_json::from_value(value).unwrap())
    }

    #[test]
    fn runs_are_grouped_by_their_validator_s_requirement() {
        // Every validator of `plan` names todo-api/REQ-001.
        let (_dir, plan) = plan(&["true", "exit 1", "true"]);
        let (plan, mut ran) = ran(plan);
        // The second is interrupted, as far as verdicts are concerned.
        ran.results[1] = None;
        let verdicts = verdicts(&plan, &ran.results);
        let name = SpecName::try_from("todo-api".to_owned()).unwrap();
        assert_eq!(
            verdicts[&name].outcomes(RequirementId::new(1)),
            [Outcome::Pass, Outcome::Pass]
        );
        assert_eq!(verdicts.len(), 1);
    }

    #[test]
    fn a_red_run_fails_its_requirement_in_its_own_specification_only() {
        let (_dir, plan) = plan(&["exit 1"]);
        let (plan, ran) = ran(plan);
        let verdicts = verdicts(&plan, &ran.results);
        let specs = [spec("todo-api"), spec("billing")];
        let evaluated = evaluate_all(&specs, &verdicts);
        let names: Vec<&str> = evaluated.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, ["todo-api", "billing"]);
        let req_001 = RequirementId::new(1);
        assert_eq!(
            evaluated[0].evaluation.status(req_001),
            Some(Status::Unsatisfied)
        );
        assert_eq!(evaluated[0].severity(req_001), Some(Severity::Violation));
        assert_eq!(
            evaluated[1].evaluation.status(req_001),
            Some(Status::Unknown)
        );
        assert_eq!(evaluated[1].severity(req_001), None);
        let qualified = |id: &str| id.parse::<QualifiedRequirementId>().unwrap();
        assert_eq!(
            severity(&evaluated, &qualified("todo-api/REQ-001")),
            Some(Severity::Violation)
        );
        assert_eq!(severity(&evaluated, &qualified("billing/REQ-001")), None);
        assert_eq!(severity(&evaluated, &qualified("other/REQ-001")), None);

        let tally = Tally::new(2, &ran.results, &evaluated);
        assert_eq!(
            tally,
            Tally {
                unrunnable: 0,
                violations: 1,
                warnings: 2,
            }
        );
    }

    #[test]
    fn the_verdict_fails_on_violations_unrunnable_validators_and_denied_warnings() {
        let tally = |unrunnable, violations, warnings| Tally {
            unrunnable,
            violations,
            warnings,
        };
        let failed = |unrunnable, violations, warnings| {
            Err(CheckFailed {
                unrunnable,
                violations,
                warnings,
            })
        };
        assert_eq!(tally(0, 0, 0).verdict(true), Ok(()));
        assert_eq!(tally(0, 0, 3).verdict(false), Ok(()));
        assert_eq!(tally(0, 0, 3).verdict(true), failed(0, 0, 3));
        assert_eq!(tally(0, 2, 3).verdict(false), failed(0, 2, 0));
        assert_eq!(tally(1, 0, 0).verdict(false), failed(1, 0, 0));
        assert_eq!(tally(2, 1, 1).verdict(true), failed(2, 1, 1));
    }

    #[test]
    fn check_failed_says_why() {
        let cases = [
            (
                CheckFailed {
                    unrunnable: 1,
                    violations: 0,
                    warnings: 0,
                },
                "check failed: 1 validator could not be run",
            ),
            (
                CheckFailed {
                    unrunnable: 2,
                    violations: 1,
                    warnings: 3,
                },
                "check failed: 2 validators could not be run, 1 violation, 3 warnings",
            ),
        ];
        for (failed, message) in cases {
            assert_eq!(failed.to_string(), message);
        }
        let denied = CheckFailed {
            unrunnable: 0,
            violations: 0,
            warnings: 1,
        };
        assert_eq!(
            render_plain(&denied),
            "  × check failed: 1 warning\n  help: `--deny-warnings` makes a warning fail the \
             check\n"
        );
    }

    #[test]
    fn interrupted_counts_what_finished() {
        let one = Interrupted {
            finished: 0,
            total: 1,
        };
        assert_eq!(
            one.to_string(),
            "check was interrupted: 0 of 1 validator finished"
        );
        let many = Interrupted {
            finished: 2,
            total: 5,
        };
        assert_eq!(
            many.to_string(),
            "check was interrupted: 2 of 5 validators finished"
        );
    }
}
