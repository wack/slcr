//! Satisfaction, from section 6 of "SLCR — Low-Level Design: Validators,
//! Lockfiles, and Materialization": how the verdicts of a specification's
//! validators combine, under strong Kleene logic, into each requirement's
//! [Status], and what an unsatisfied requirement is reported as.
//!
//! [evaluate] is pure: it runs nothing and reads nothing, and it reports
//! everything in document order.
//!
//! - A leaf, which nothing refines, is the Kleene AND of its validators'
//!   verdicts. A leaf with no validators is Unknown, never vacuously
//!   satisfied: a coverage gap, not a finding.
//! - Any other requirement combines the requirements that refine it with
//!   its `refinement`. AND leaves out MAY refiners, and is vacuously
//!   satisfied, with a warning, when that leaves none; OR and XOR count
//!   them. Its own validators, if any, join that result under AND.
//! - Containment plays no part: only `refines` edges do.
//! - An unsatisfied requirement is a violation, a warning, or informational
//!   by its modality, unless its failure is absorbed: it refines at least
//!   one requirement and every one of them is satisfied, so it is reported
//!   as a warning (a MAY stays informational).

use std::collections::{BTreeMap, HashMap, HashSet};

use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use crate::spec::graph::SlcrRequirementsDocument;
use crate::spec::id::RequirementId;
use crate::spec::node::{Modality, Refinement, Requirement};
use crate::spec::vet;

/// What one run of a validator reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Pass,
    Fail,
}

impl Outcome {
    fn status(self) -> Status {
        match self {
            Self::Pass => Status::Satisfied,
            Self::Fail => Status::Unsatisfied,
        }
    }
}

/// The outcomes of the validators reported for each requirement of one
/// specification. A requirement with none reported has no validators.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdicts(BTreeMap<RequirementId, Vec<Outcome>>);

impl Verdicts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the outcome of one validator of `requirement`.
    pub fn record(&mut self, requirement: RequirementId, outcome: Outcome) {
        self.0.entry(requirement).or_default().push(outcome);
    }

    /// The outcomes reported for `requirement`, in the order recorded.
    pub fn outcomes(&self, requirement: RequirementId) -> &[Outcome] {
        self.0.get(&requirement).map_or(&[], Vec::as_slice)
    }
}

impl FromIterator<(RequirementId, Outcome)> for Verdicts {
    fn from_iter<I: IntoIterator<Item = (RequirementId, Outcome)>>(iter: I) -> Self {
        let mut verdicts = Self::new();
        for (requirement, outcome) in iter {
            verdicts.record(requirement, outcome);
        }
        verdicts
    }
}

/// Whether a requirement holds, as far as its validators tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Satisfied,
    Unsatisfied,
    /// Not yet decided: some resolution of what no validator has settled
    /// would satisfy it, and another would not.
    Unknown,
}

impl Status {
    /// The Kleene AND of `statuses`: Unsatisfied if any is, Satisfied if
    /// all are, and so Satisfied if there are none.
    fn all(statuses: impl IntoIterator<Item = Self>) -> Self {
        let mut all = Self::Satisfied;
        for status in statuses {
            match status {
                Self::Unsatisfied => return Self::Unsatisfied,
                Self::Unknown => all = Self::Unknown,
                Self::Satisfied => {}
            }
        }
        all
    }

    /// The Kleene OR of `statuses`: Satisfied if any is, Unsatisfied if
    /// all are, and so Unsatisfied if there are none.
    fn any(statuses: impl IntoIterator<Item = Self>) -> Self {
        let mut any = Self::Unsatisfied;
        for status in statuses {
            match status {
                Self::Satisfied => return Self::Satisfied,
                Self::Unknown => any = Self::Unknown,
                Self::Unsatisfied => {}
            }
        }
        any
    }

    /// Whether exactly one of `statuses` is Satisfied, decided only when
    /// every resolution of the Unknowns agrees: Unsatisfied if two or more
    /// are Satisfied or all are Unsatisfied, Satisfied if one is and every
    /// other is Unsatisfied.
    fn one(statuses: impl IntoIterator<Item = Self>) -> Self {
        let (mut satisfied, mut unknown) = (0, 0);
        for status in statuses {
            match status {
                Self::Satisfied => satisfied += 1,
                Self::Unknown => unknown += 1,
                Self::Unsatisfied => {}
            }
        }
        match (satisfied, unknown) {
            (2.., _) | (0, 0) => Self::Unsatisfied,
            (1, 0) => Self::Satisfied,
            _ => Self::Unknown,
        }
    }
}

/// How much an unsatisfied requirement matters, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Never fails anything.
    Informational,
    /// Fails only when warnings are denied.
    Warning,
    Violation,
}

impl Severity {
    /// The severity of an unsatisfied requirement of `modality`: a negated
    /// modality is as severe as its positive form.
    fn of(modality: Modality) -> Self {
        match modality {
            Modality::Must | Modality::MustNot => Self::Violation,
            Modality::Should | Modality::ShouldNot => Self::Warning,
            Modality::May => Self::Informational,
        }
    }
}

/// Something evaluation found about one requirement.
///
/// It serializes like a [vet::Violation]: an object whose `code` names
/// the finding in kebab-case, alongside the variant's fields.
#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize)]
#[serde(
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Finding {
    /// Reported at its modality's severity.
    #[error("{requirement}, a {modality} requirement, is unsatisfied")]
    Unsatisfied {
        requirement: RequirementId,
        modality: Modality,
    },

    /// An unsatisfied requirement whose every parent is satisfied anyway.
    #[error(
        "{requirement}, a {modality} requirement, is unsatisfied, but every requirement it \
         refines is satisfied: {}",
        list(.absorbed_by)
    )]
    Absorbed {
        requirement: RequirementId,
        modality: Modality,
        /// What it refines, every one satisfied.
        absorbed_by: Vec<RequirementId>,
    },

    #[error("{requirement} is vacuously satisfied by its refinements")]
    VacuousAnd { requirement: RequirementId },
}

fn list(ids: &[RequirementId]) -> String {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    ids.join(", ")
}

impl Finding {
    pub fn requirement(&self) -> RequirementId {
        match self {
            Self::Unsatisfied { requirement, .. }
            | Self::Absorbed { requirement, .. }
            | Self::VacuousAnd { requirement } => *requirement,
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Self::Unsatisfied { modality, .. } => Severity::of(*modality),
            Self::Absorbed { modality, .. } => Severity::of(*modality).min(Severity::Warning),
            Self::VacuousAnd { .. } => Severity::Warning,
        }
    }
}

impl Diagnostic for Finding {
    fn severity(&self) -> Option<miette::Severity> {
        Some(match Finding::severity(self) {
            Severity::Informational => miette::Severity::Advice,
            Severity::Warning => miette::Severity::Warning,
            Severity::Violation => miette::Severity::Error,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        let help = match self {
            Self::Unsatisfied { .. } => return None,
            Self::Absorbed { .. } => "its failure is absorbed: what it refines holds without it",
            Self::VacuousAnd { .. } => {
                "it combines its refinements with AND, and every requirement refining it is MAY"
            }
        };
        Some(Box::new(help))
    }
}

/// Requirement and finding counts, for a summary line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub satisfied: usize,
    pub unsatisfied: usize,
    pub unknown: usize,
    pub violations: usize,
    pub warnings: usize,
    pub informational: usize,
    pub coverage_gaps: usize,
}

/// The satisfaction of one specification's requirements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    statuses: Vec<(RequirementId, Status)>,
    findings: Vec<Finding>,
    coverage_gaps: Vec<RequirementId>,
}

impl Evaluation {
    /// Every requirement's status, in document order.
    pub fn statuses(&self) -> &[(RequirementId, Status)] {
        &self.statuses
    }

    /// The status of `requirement`, if the specification has it.
    pub fn status(&self, requirement: RequirementId) -> Option<Status> {
        self.statuses
            .iter()
            .find(|(id, _)| *id == requirement)
            .map(|(_, status)| *status)
    }

    /// Every finding, in document order of its requirement.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// The leaves no validator was reported for, in document order. Each is
    /// Unknown, which is not a finding.
    pub fn coverage_gaps(&self) -> &[RequirementId] {
        &self.coverage_gaps
    }

    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            coverage_gaps: self.coverage_gaps.len(),
            ..Summary::default()
        };
        for (_, status) in &self.statuses {
            match status {
                Status::Satisfied => summary.satisfied += 1,
                Status::Unsatisfied => summary.unsatisfied += 1,
                Status::Unknown => summary.unknown += 1,
            }
        }
        for finding in &self.findings {
            match finding.severity() {
                Severity::Violation => summary.violations += 1,
                Severity::Warning => summary.warnings += 1,
                Severity::Informational => summary.informational += 1,
            }
        }
        summary
    }

    /// Whether the specification passes: no finding is a violation and, if
    /// warnings are denied, none is a warning. Unknown requirements never
    /// fail it.
    pub fn passes(&self, deny_warnings: bool) -> bool {
        let fails = if deny_warnings {
            Severity::Warning
        } else {
            Severity::Violation
        };
        self.findings
            .iter()
            .all(|finding| finding.severity() < fails)
    }
}

/// Evaluate every requirement of `document` from the `verdicts` of its
/// validators. Verdicts for requirements the document lacks are ignored.
pub fn evaluate(document: &SlcrRequirementsDocument, verdicts: &Verdicts) -> Evaluation {
    let requirements = document.requirements();
    let refiners = vet::refiners(&requirements);
    let refiners_of = |id: RequirementId| refiners.get(&id).map_or(&[][..], Vec::as_slice);
    let statuses = statuses(&requirements, &refiners_of, verdicts);
    let status = |id: &RequirementId| statuses.get(id).copied().unwrap_or(Status::Unknown);

    let mut findings = Vec::new();
    let mut coverage_gaps = Vec::new();
    for requirement in &requirements {
        let id = *requirement.id();
        let children = refiners_of(id);
        if children.is_empty() && verdicts.outcomes(id).is_empty() {
            coverage_gaps.push(id);
        }
        let vacuous = *requirement.refinement() == Some(Refinement::And)
            && !children.is_empty()
            && children.iter().all(|child| is_may(child));
        if vacuous {
            findings.push(Finding::VacuousAnd { requirement: id });
        }
        if status(&id) == Status::Unsatisfied {
            let parents = requirement.refines();
            let modality = *requirement.modality();
            let absorbed = !parents.is_empty()
                && parents
                    .iter()
                    .all(|parent| status(parent) == Status::Satisfied);
            findings.push(if absorbed {
                Finding::Absorbed {
                    requirement: id,
                    modality,
                    absorbed_by: parents.clone(),
                }
            } else {
                Finding::Unsatisfied {
                    requirement: id,
                    modality,
                }
            });
        }
    }

    Evaluation {
        statuses: requirements
            .iter()
            .map(|requirement| (*requirement.id(), status(requirement.id())))
            .collect(),
        findings,
        coverage_gaps,
    }
}

fn is_may(requirement: &Requirement) -> bool {
    *requirement.modality() == Modality::May
}

/// Every requirement's status, each computed once, after every requirement
/// that refines it.
///
/// Iterative rather than recursive, so a long chain of refinements can't
/// overflow the stack.
fn statuses<'a>(
    requirements: &[&'a Requirement],
    refiners_of: &impl Fn(RequirementId) -> &'a [&'a Requirement],
    verdicts: &Verdicts,
) -> HashMap<RequirementId, Status> {
    let mut statuses = HashMap::new();
    let mut expanded = HashSet::new();
    for requirement in requirements {
        let mut stack = vec![*requirement];
        while let Some(&top) = stack.last() {
            let id = *top.id();
            if statuses.contains_key(&id) {
                stack.pop();
                continue;
            }
            let pending: Vec<_> = refiners_of(id)
                .iter()
                .filter(|child| !statuses.contains_key(child.id()))
                .copied()
                .collect();
            // In an acyclic graph, every pending refiner has been computed
            // by the time an expanded requirement is back on top. Only a
            // cycle, which vetting rules out, leaves one pending; it reads
            // as Unknown rather than looping forever.
            if pending.is_empty() || !expanded.insert(id) {
                stack.pop();
                let status = status(top, refiners_of(id), &statuses, verdicts);
                statuses.insert(id, status);
            } else {
                stack.extend(pending);
            }
        }
    }
    statuses
}

/// The status of `requirement`, given those of its `children`, the
/// requirements that refine it.
fn status(
    requirement: &Requirement,
    children: &[&Requirement],
    statuses: &HashMap<RequirementId, Status>,
    verdicts: &Verdicts,
) -> Status {
    let outcomes = verdicts.outcomes(*requirement.id());
    let own = (!outcomes.is_empty()).then(|| Status::all(outcomes.iter().map(|o| o.status())));
    if children.is_empty() {
        return own.unwrap_or(Status::Unknown);
    }

    let child = |child: &&Requirement| statuses.get(child.id()).copied().unwrap_or(Status::Unknown);
    // Vetting guarantees a requirement with refiners has a `refinement`.
    let combined = match requirement.refinement().unwrap_or(Refinement::And) {
        Refinement::And => Status::all(children.iter().filter(|c| !is_may(c)).map(child)),
        Refinement::Or => Status::any(children.iter().map(child)),
        Refinement::Xor => Status::one(children.iter().map(child)),
    };
    own.map_or(combined, |own| Status::all([own, combined]))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::Outcome::{Fail, Pass};
    use super::Status::{Satisfied, Unknown, Unsatisfied};
    use super::*;

    const TODO_API: &str = include_str!("../tests/fixtures/todo-api.spec.json");

    fn rid(number: u32) -> RequirementId {
        RequirementId::new(number)
    }

    /// A requirement for [document]; its body states its modality.
    fn req(id: u32, modality: &str, refinement: Option<&str>, refines: &[u32]) -> Value {
        let mut value = json!({
            "kind": "requirement",
            "id": rid(id).to_string(),
            "title": format!("Requirement {id}"),
            "body": format!("It {modality} hold."),
            "modality": modality,
        });
        if let Some(refinement) = refinement {
            value["refinement"] = json!(refinement);
        }
        if !refines.is_empty() {
            let refines: Vec<String> = refines.iter().map(|id| rid(*id).to_string()).collect();
            value["refines"] = json!(refines);
        }
        value
    }

    /// A vetted document whose root Section holds `requirements`, side by
    /// side: containment plays no part in satisfaction.
    fn document(requirements: Vec<Value>) -> SlcrRequirementsDocument {
        serde_json::from_value(json!({
            "formatVersion": "1",
            "spec": { "name": "test" },
            "root": { "kind": "section", "id": "SEC-001", "title": "Root", "children": requirements },
            "glossary": { "kind": "section", "id": "SEC-002", "title": "Glossary" }
        }))
        .unwrap()
    }

    fn todo_api() -> SlcrRequirementsDocument {
        serde_json::from_str(TODO_API).unwrap()
    }

    fn verdicts<const N: usize>(outcomes: [(u32, Outcome); N]) -> Verdicts {
        outcomes.into_iter().map(|(id, o)| (rid(id), o)).collect()
    }

    /// The outcomes that make a leaf `status`: no validator for Unknown.
    fn outcomes_for(status: Status) -> &'static [Outcome] {
        match status {
            Satisfied => &[Pass],
            Unsatisfied => &[Fail],
            Unknown => &[],
        }
    }

    /// Every sequence of 1 to 3 statuses.
    fn combinations() -> Vec<Vec<Status>> {
        let mut all: Vec<Vec<Status>> = vec![vec![]];
        let mut combinations = Vec::new();
        for _ in 0..3 {
            all = all
                .into_iter()
                .flat_map(|prefix| {
                    [Satisfied, Unsatisfied, Unknown].map(|status| {
                        let mut next = prefix.clone();
                        next.push(status);
                        next
                    })
                })
                .collect();
            combinations.extend(all.clone());
        }
        combinations
    }

    /// Strong Kleene logic by definition: `op` over `statuses` is decided
    /// only when every resolution of the Unknowns agrees on it.
    fn kleene(statuses: &[Status], op: Operator) -> Status {
        let unknowns: Vec<usize> = (0..statuses.len())
            .filter(|i| statuses[*i] == Unknown)
            .collect();
        let results: HashSet<bool> = (0..1u32 << unknowns.len())
            .map(|resolution| {
                let values: Vec<bool> = statuses
                    .iter()
                    .enumerate()
                    .map(|(i, status)| match status {
                        Satisfied => true,
                        Unsatisfied => false,
                        Unknown => {
                            let bit = unknowns.iter().position(|u| *u == i).unwrap();
                            resolution & (1 << bit) != 0
                        }
                    })
                    .collect();
                op(&values)
            })
            .collect();
        match (results.contains(&true), results.contains(&false)) {
            (true, false) => Satisfied,
            (false, true) => Unsatisfied,
            _ => Unknown,
        }
    }

    /// A Boolean refinement operator, for [kleene].
    type Operator = fn(&[bool]) -> bool;

    const OPERATORS: [(&str, Operator); 3] = [
        ("AND", |values| values.iter().all(|v| *v)),
        ("OR", |values| values.iter().any(|v| *v)),
        ("XOR", |values| values.iter().filter(|v| **v).count() == 1),
    ];

    #[test]
    fn statuses_combine_by_strong_kleene_logic() {
        for statuses in combinations() {
            let [(_, and), (_, or), (_, xor)] = OPERATORS;
            assert_eq!(Status::all(statuses.clone()), kleene(&statuses, and));
            assert_eq!(Status::any(statuses.clone()), kleene(&statuses, or));
            assert_eq!(Status::one(statuses.clone()), kleene(&statuses, xor));
        }
    }

    #[test]
    fn refinements_combine_their_refiners_by_operator() {
        for (operator, op) in OPERATORS {
            for children in combinations() {
                let mut requirements = vec![req(1, "MUST", Some(operator), &[])];
                let mut outcomes = Verdicts::new();
                for (n, status) in children.iter().enumerate() {
                    let id = n as u32 + 2;
                    requirements.push(req(id, "MUST", None, &[1]));
                    for outcome in outcomes_for(*status) {
                        outcomes.record(rid(id), *outcome);
                    }
                }
                let evaluation = evaluate(&document(requirements), &outcomes);
                assert_eq!(
                    evaluation.status(rid(1)),
                    Some(kleene(&children, op)),
                    "{operator} over {children:?}"
                );
            }
        }
    }

    #[test]
    fn own_validators_join_the_refiners_under_and() {
        for (operator, _) in OPERATORS {
            for child in [Satisfied, Unsatisfied, Unknown] {
                for own in [Pass, Fail] {
                    let mut outcomes = verdicts([(1, own)]);
                    for outcome in outcomes_for(child) {
                        outcomes.record(rid(2), *outcome);
                    }
                    let document = document(vec![
                        req(1, "MUST", Some(operator), &[]),
                        req(2, "MUST", None, &[1]),
                    ]);
                    let expected = Status::all([own.status(), child]);
                    assert_eq!(
                        evaluate(&document, &outcomes).status(rid(1)),
                        Some(expected),
                        "{operator}: own {own:?}, child {child:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_failing_own_check_sinks_a_parent_whose_refiners_pass() {
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MUST", None, &[1]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(1, Fail), (2, Pass)]));
        assert_eq!(evaluation.status(rid(1)), Some(Unsatisfied));
        assert_eq!(evaluation.status(rid(2)), Some(Satisfied));
    }

    #[test]
    fn a_leaf_is_the_and_of_its_validators() {
        let document = document(vec![req(1, "MUST", None, &[])]);
        for (outcomes, expected) in [
            (vec![Pass], Satisfied),
            (vec![Pass, Pass], Satisfied),
            (vec![Fail], Unsatisfied),
            (vec![Pass, Fail], Unsatisfied),
        ] {
            let verdicts = outcomes.iter().map(|o| (rid(1), *o)).collect();
            let evaluation = evaluate(&document, &verdicts);
            assert_eq!(evaluation.status(rid(1)), Some(expected), "{outcomes:?}");
            assert!(evaluation.coverage_gaps().is_empty(), "{outcomes:?}");
        }
    }

    #[test]
    fn a_leaf_without_validators_is_unknown_and_a_coverage_gap() {
        let document = document(vec![req(1, "MUST", None, &[])]);
        let evaluation = evaluate(&document, &Verdicts::new());
        assert_eq!(evaluation.status(rid(1)), Some(Unknown));
        assert_eq!(evaluation.coverage_gaps(), [rid(1)]);
        assert!(evaluation.findings().is_empty());
        assert!(evaluation.passes(true));
    }

    #[test]
    fn a_refined_requirement_without_validators_is_not_a_coverage_gap() {
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MUST", None, &[1]),
        ]);
        let evaluation = evaluate(&document, &Verdicts::new());
        assert_eq!(evaluation.coverage_gaps(), [rid(2)]);
    }

    #[test]
    fn and_leaves_out_may_refiners() {
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MUST", None, &[1]),
            req(3, "MAY", None, &[1]),
        ]);
        for may in [Pass, Fail] {
            let evaluation = evaluate(&document, &verdicts([(2, Pass), (3, may)]));
            assert_eq!(evaluation.status(rid(1)), Some(Satisfied), "{may:?}");
        }
        let evaluation = evaluate(&document, &verdicts([(3, Pass)]));
        assert_eq!(evaluation.status(rid(1)), Some(Unknown));
    }

    #[test]
    fn or_and_xor_count_may_refiners() {
        for operator in ["OR", "XOR"] {
            let document = document(vec![
                req(1, "MUST", Some(operator), &[]),
                req(2, "MAY", None, &[1]),
                req(3, "MAY", None, &[1]),
            ]);
            let evaluation = evaluate(&document, &verdicts([(2, Pass), (3, Fail)]));
            assert_eq!(evaluation.status(rid(1)), Some(Satisfied), "{operator}");
            let evaluation = evaluate(&document, &verdicts([(2, Fail), (3, Fail)]));
            assert_eq!(evaluation.status(rid(1)), Some(Unsatisfied), "{operator}");
        }
    }

    #[test]
    fn xor_is_unsatisfied_when_no_refiner_is_satisfied() {
        let document = document(vec![
            req(1, "MUST", Some("XOR"), &[]),
            req(2, "MUST", None, &[1]),
            req(3, "MUST", None, &[1]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(2, Fail), (3, Fail)]));
        assert_eq!(evaluation.status(rid(1)), Some(Unsatisfied));
    }

    #[test]
    fn an_and_over_only_may_refiners_is_vacuously_satisfied_with_a_warning() {
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MAY", None, &[1]),
            req(3, "MAY", None, &[1]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(2, Fail)]));
        assert_eq!(evaluation.status(rid(1)), Some(Satisfied));
        assert_eq!(
            evaluation.findings(),
            [
                Finding::VacuousAnd {
                    requirement: rid(1)
                },
                Finding::Absorbed {
                    requirement: rid(2),
                    modality: Modality::May,
                    absorbed_by: vec![rid(1)],
                },
            ]
        );
        assert_eq!(evaluation.findings()[0].severity(), Severity::Warning);
        assert!(evaluation.passes(false));
        assert!(!evaluation.passes(true));
    }

    #[test]
    fn a_shared_refiner_counts_toward_every_requirement_it_refines() {
        // REQ-004 refines both REQ-002 and REQ-003, which both refine REQ-001.
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MUST", Some("AND"), &[1]),
            req(3, "MUST", Some("OR"), &[1]),
            req(4, "MUST", None, &[2, 3]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(4, Pass)]));
        assert_eq!(
            evaluation.statuses(),
            [1, 2, 3, 4].map(|id| (rid(id), Satisfied))
        );

        let evaluation = evaluate(&document, &verdicts([(4, Fail)]));
        assert_eq!(
            evaluation.statuses(),
            [1, 2, 3, 4].map(|id| (rid(id), Unsatisfied))
        );
    }

    #[test]
    fn a_long_chain_of_refinements_does_not_overflow_the_stack() {
        const LENGTH: u32 = 5_000;
        let requirements = (1..=LENGTH)
            .map(|id| {
                let refinement = (id < LENGTH).then_some("AND");
                let refines: &[u32] = if id == 1 { &[] } else { &[id - 1] };
                req(id, "MUST", refinement, refines)
            })
            .collect();
        let evaluation = evaluate(&document(requirements), &verdicts([(LENGTH, Pass)]));
        assert_eq!(evaluation.status(rid(1)), Some(Satisfied));
    }

    #[test]
    fn a_cycle_in_an_unvetted_document_still_terminates() {
        use crate::spec::graph::UnvettedDocument;

        let unvetted: UnvettedDocument = serde_json::from_value(json!({
            "formatVersion": "1",
            "spec": { "name": "test" },
            "root": { "kind": "section", "id": "SEC-001", "title": "Root", "children": [
                req(1, "MUST", Some("AND"), &[2]),
                req(2, "MUST", Some("AND"), &[1]),
            ] },
            "glossary": { "kind": "section", "id": "SEC-002", "title": "Glossary" }
        }))
        .unwrap();
        let evaluation = evaluate(unvetted.document(), &Verdicts::new());
        assert_eq!(
            evaluation.statuses(),
            [(rid(1), Unknown), (rid(2), Unknown)]
        );
    }

    #[test]
    fn an_unsatisfied_requirement_is_as_severe_as_its_modality() {
        for (modality, severity) in [
            ("MUST", Severity::Violation),
            ("MUST NOT", Severity::Violation),
            ("SHOULD", Severity::Warning),
            ("SHOULD NOT", Severity::Warning),
            ("MAY", Severity::Informational),
        ] {
            let document = document(vec![req(1, modality, None, &[])]);
            let evaluation = evaluate(&document, &verdicts([(1, Fail)]));
            let modality = serde_json::from_value(json!(modality)).unwrap();
            assert_eq!(
                evaluation.findings(),
                [Finding::Unsatisfied {
                    requirement: rid(1),
                    modality
                }]
            );
            assert_eq!(evaluation.findings()[0].severity(), severity, "{modality}");
        }
    }

    /// REQ-001 combines REQ-002 and REQ-003 with `operator`.
    fn two_refiners(operator: &str, modality: &str) -> SlcrRequirementsDocument {
        document(vec![
            req(1, "MUST", Some(operator), &[]),
            req(2, modality, None, &[1]),
            req(3, modality, None, &[1]),
        ])
    }

    #[test]
    fn a_failure_is_absorbed_when_a_satisfied_or_sibling_covers_it() {
        let evaluation = evaluate(
            &two_refiners("OR", "MUST"),
            &verdicts([(2, Pass), (3, Fail)]),
        );
        let absorbed = Finding::Absorbed {
            requirement: rid(3),
            modality: Modality::Must,
            absorbed_by: vec![rid(1)],
        };
        assert_eq!(absorbed.severity(), Severity::Warning);
        assert_eq!(evaluation.findings(), [absorbed]);
        assert!(evaluation.passes(false));
        assert!(!evaluation.passes(true));
    }

    #[test]
    fn a_failure_under_an_and_is_not_absorbed() {
        let evaluation = evaluate(
            &two_refiners("AND", "MUST"),
            &verdicts([(2, Pass), (3, Fail)]),
        );
        assert_eq!(
            evaluation.findings(),
            [
                Finding::Unsatisfied {
                    requirement: rid(1),
                    modality: Modality::Must
                },
                Finding::Unsatisfied {
                    requirement: rid(3),
                    modality: Modality::Must
                },
            ]
        );
        assert!(!evaluation.passes(false));
    }

    #[test]
    fn a_failure_that_refines_nothing_is_not_absorbed() {
        let document = document(vec![req(1, "MUST", None, &[])]);
        let evaluation = evaluate(&document, &verdicts([(1, Fail)]));
        assert_eq!(evaluation.findings()[0].severity(), Severity::Violation);
    }

    #[test]
    fn a_failure_is_not_absorbed_if_any_parent_is_unsatisfied() {
        // REQ-003 refines the OR REQ-001, which REQ-002 satisfies, and the
        // AND REQ-004, which its failure sinks.
        let document = document(vec![
            req(1, "MUST", Some("OR"), &[]),
            req(2, "MUST", None, &[1]),
            req(3, "MUST", None, &[1, 4]),
            req(4, "MUST", Some("AND"), &[]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(2, Pass), (3, Fail)]));
        assert_eq!(evaluation.status(rid(1)), Some(Satisfied));
        assert_eq!(
            evaluation.findings(),
            [
                Finding::Unsatisfied {
                    requirement: rid(3),
                    modality: Modality::Must
                },
                Finding::Unsatisfied {
                    requirement: rid(4),
                    modality: Modality::Must
                },
            ]
        );
    }

    #[test]
    fn a_failure_is_not_absorbed_if_a_parent_is_unknown() {
        let evaluation = evaluate(&two_refiners("OR", "MUST"), &verdicts([(3, Fail)]));
        assert_eq!(evaluation.status(rid(1)), Some(Unknown));
        assert_eq!(
            evaluation.findings(),
            [Finding::Unsatisfied {
                requirement: rid(3),
                modality: Modality::Must
            }]
        );
        assert!(!evaluation.passes(false));
    }

    #[test]
    fn an_absorbed_may_stays_informational() {
        let evaluation = evaluate(
            &two_refiners("XOR", "MAY"),
            &verdicts([(2, Pass), (3, Fail)]),
        );
        let absorbed = Finding::Absorbed {
            requirement: rid(3),
            modality: Modality::May,
            absorbed_by: vec![rid(1)],
        };
        assert_eq!(absorbed.severity(), Severity::Informational);
        assert_eq!(evaluation.findings(), [absorbed]);
        assert!(evaluation.passes(true));
    }

    #[test]
    fn the_canon_worked_example() {
        // The API-key validator passes and nothing validates OAuth; the POST
        // handler passes and nothing validates GET.
        let evaluation = evaluate(&todo_api(), &verdicts([(10, Pass), (2, Pass)]));
        assert_eq!(evaluation.status(rid(8)), Some(Satisfied));
        assert_eq!(evaluation.status(rid(9)), Some(Unknown));
        assert_eq!(evaluation.status(rid(1)), Some(Unknown));
        assert_eq!(evaluation.status(rid(3)), Some(Unknown));
        // REQ-004 refines POST, and nothing validates it either.
        assert_eq!(evaluation.status(rid(2)), Some(Unknown));
        assert_eq!(
            evaluation.coverage_gaps(),
            [4, 6, 7, 9].map(rid),
            "the leaves without validators"
        );
        assert!(evaluation.findings().is_empty());
        assert!(evaluation.passes(true));
    }

    #[test]
    fn statuses_are_in_document_order() {
        let evaluation = evaluate(&todo_api(), &Verdicts::new());
        let ids: Vec<u32> = evaluation
            .statuses()
            .iter()
            .map(|(id, _)| id.number())
            .collect();
        assert_eq!(ids, [1, 2, 4, 3, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn the_canon_example_is_satisfied_when_every_leaf_passes() {
        let leaves = [4, 6, 7, 9, 10].map(|id| (id, Pass));
        let evaluation = evaluate(&todo_api(), &verdicts(leaves));
        assert!(evaluation.statuses().iter().all(|(_, s)| *s == Satisfied));
        assert!(evaluation.coverage_gaps().is_empty());
        assert_eq!(
            evaluation.summary(),
            Summary {
                satisfied: 10,
                ..Summary::default()
            }
        );
    }

    #[test]
    fn the_summary_counts_statuses_findings_and_gaps() {
        // REQ-003 (SHOULD) fails under the AND REQ-001; REQ-004 (MAY) fails
        // alone; REQ-005 has no validator.
        let document = document(vec![
            req(1, "MUST", Some("AND"), &[]),
            req(2, "MUST", None, &[1]),
            req(3, "SHOULD", None, &[1]),
            req(4, "MAY", None, &[]),
            req(5, "MUST", None, &[]),
        ]);
        let evaluation = evaluate(&document, &verdicts([(2, Pass), (3, Fail), (4, Fail)]));
        assert_eq!(
            evaluation.summary(),
            Summary {
                satisfied: 1,
                unsatisfied: 3,
                unknown: 1,
                violations: 1,
                warnings: 1,
                informational: 1,
                coverage_gaps: 1,
            }
        );
        assert!(!evaluation.passes(false));
    }

    #[test]
    fn the_policy_fails_on_violations_and_denied_warnings() {
        for (modality, outcome, passes, passes_denying_warnings) in [
            ("MUST", Pass, true, true),
            ("MUST", Fail, false, false),
            ("MUST NOT", Fail, false, false),
            ("SHOULD", Fail, true, false),
            ("SHOULD NOT", Fail, true, false),
            ("MAY", Fail, true, true),
        ] {
            let document = document(vec![req(1, modality, None, &[])]);
            let evaluation = evaluate(&document, &verdicts([(1, outcome)]));
            assert_eq!(evaluation.passes(false), passes, "{modality} {outcome:?}");
            assert_eq!(
                evaluation.passes(true),
                passes_denying_warnings,
                "{modality} {outcome:?}"
            );
        }
    }

    #[test]
    fn verdicts_for_unknown_requirements_are_ignored() {
        let document = document(vec![req(1, "MUST", None, &[])]);
        let evaluation = evaluate(&document, &verdicts([(1, Pass), (9, Fail)]));
        assert_eq!(evaluation.statuses(), [(rid(1), Satisfied)]);
        assert!(evaluation.findings().is_empty());
    }

    #[test]
    fn findings_are_diagnostics_with_their_severity() {
        let cases = [
            (
                Finding::Unsatisfied {
                    requirement: rid(2),
                    modality: Modality::MustNot,
                },
                "REQ-002, a MUST NOT requirement, is unsatisfied",
                miette::Severity::Error,
            ),
            (
                Finding::Unsatisfied {
                    requirement: rid(2),
                    modality: Modality::Should,
                },
                "REQ-002, a SHOULD requirement, is unsatisfied",
                miette::Severity::Warning,
            ),
            (
                Finding::Absorbed {
                    requirement: rid(4),
                    modality: Modality::Must,
                    absorbed_by: vec![rid(2), rid(3)],
                },
                "REQ-004, a MUST requirement, is unsatisfied, but every requirement it refines \
                 is satisfied: REQ-002, REQ-003",
                miette::Severity::Warning,
            ),
            (
                Finding::Absorbed {
                    requirement: rid(9),
                    modality: Modality::May,
                    absorbed_by: vec![rid(8)],
                },
                "REQ-009, a MAY requirement, is unsatisfied, but every requirement it refines \
                 is satisfied: REQ-008",
                miette::Severity::Advice,
            ),
            (
                Finding::VacuousAnd {
                    requirement: rid(1),
                },
                "REQ-001 is vacuously satisfied by its refinements",
                miette::Severity::Warning,
            ),
        ];
        for (finding, message, severity) in cases {
            let diagnostic: &dyn Diagnostic = &finding;
            assert_eq!(diagnostic.to_string(), message);
            assert_eq!(diagnostic.severity(), Some(severity), "{message}");
            assert_eq!(
                diagnostic.help().is_some(),
                !matches!(finding, Finding::Unsatisfied { .. }),
                "{message}"
            );
        }
    }

    #[test]
    fn findings_serialize_with_a_code_and_their_fields() {
        let cases = [
            (
                Finding::Unsatisfied {
                    requirement: rid(2),
                    modality: Modality::MustNot,
                },
                json!({ "code": "unsatisfied", "requirement": "REQ-002", "modality": "MUST NOT" }),
            ),
            (
                Finding::Absorbed {
                    requirement: rid(9),
                    modality: Modality::May,
                    absorbed_by: vec![rid(8)],
                },
                json!({
                    "code": "absorbed",
                    "requirement": "REQ-009",
                    "modality": "MAY",
                    "absorbedBy": ["REQ-008"]
                }),
            ),
            (
                Finding::VacuousAnd {
                    requirement: rid(1),
                },
                json!({ "code": "vacuous-and", "requirement": "REQ-001" }),
            ),
        ];
        for (finding, expected) in cases {
            assert_eq!(serde_json::to_value(&finding).unwrap(), expected);
        }
    }
}
