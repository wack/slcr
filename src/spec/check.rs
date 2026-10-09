//! Invariants of the specification graph that its JSON Schema cannot
//! express, from the schema's `x-checkInvariants` annotation.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::{self, Display},
    hash::Hash,
};

use miette::Diagnostic;
use thiserror::Error;

use super::graph::SpecGraph;
use super::id::{DependencyId, RequirementId, TermId};
use super::node::{Modality, Refinement, Requirement};

/// The invariants a specification graph breaks, in document order.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{}", list(.violations))]
pub struct Violations {
    #[related]
    violations: Vec<Violation>,
}

impl Violations {
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }
}

fn list(violations: &[Violation]) -> String {
    let mut message = String::from("the specification graph breaks its invariants:");
    for violation in violations {
        message.push_str("\n  - ");
        message.push_str(&violation.to_string());
    }
    message
}

/// One broken invariant.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum Violation {
    #[error("{0} identifies more than one node")]
    DuplicateId(String),

    #[error("{from} names {to} in `{relation}`, but no such node exists")]
    DanglingReference {
        from: String,
        relation: Relation,
        to: String,
    },

    #[error("`{relation}` forms a cycle: {}", arrows(.path))]
    Cycle {
        relation: Relation,
        /// The cycle's nodes, starting and ending with the same one.
        path: Vec<RequirementId>,
    },

    #[error("{requirement} is refined by {refiner} but has no `refinement`")]
    #[diagnostic(help("set `refinement` to AND, OR, or XOR"))]
    MissingRefinement {
        requirement: RequirementId,
        refiner: RequirementId,
    },

    #[error("{requirement} has a `refinement`, but no requirement refines it")]
    #[diagnostic(help("omit `refinement` on a leaf requirement"))]
    UnexpectedRefinement { requirement: RequirementId },

    #[error("glossary term {term} follows {previous}; terms must be serialized in ID order")]
    TermOutOfOrder { term: TermId, previous: TermId },
}

fn arrows(path: &[RequirementId]) -> String {
    let ids: Vec<String> = path.iter().map(ToString::to_string).collect();
    ids.join(" → ")
}

/// A cross edge between nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Refines,
    DependsOn,
    UsesTerm,
}

impl Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Refines => "refines",
            Self::DependsOn => "dependsOn",
            Self::UsesTerm => "usesTerm",
        })
    }
}

/// A finding that doesn't make the graph invalid.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum Warning {
    #[error("the body of {requirement} does not contain its modality keyword, {modality}")]
    #[diagnostic(severity(Warning))]
    ModalityNotStated {
        requirement: RequirementId,
        modality: Modality,
    },

    #[error("{requirement} is vacuously satisfied")]
    #[diagnostic(
        severity(Warning),
        help("it combines its refinements with AND, and every requirement refining it is MAY")
    )]
    VacuousAnd { requirement: RequirementId },
}

/// Check every invariant that is an error rather than a warning.
pub(super) fn invariants(graph: &SpecGraph) -> Result<(), Violations> {
    let requirements = graph.requirements();
    let violations: Vec<Violation> = [
        duplicate_ids(graph, &requirements),
        dangling_references(graph, &requirements),
        cycles(&requirements),
        refinement_presence(&requirements),
        term_order(graph),
    ]
    .into_iter()
    .flatten()
    .collect();

    if violations.is_empty() {
        Ok(())
    } else {
        Err(Violations { violations })
    }
}

/// Check the invariants that are only warnings.
pub(super) fn warnings(graph: &SpecGraph) -> Vec<Warning> {
    let requirements = graph.requirements();
    let refiners = refiners(&requirements);
    let mut warnings = Vec::new();
    for requirement in &requirements {
        let id = *requirement.id();
        if !states_modality(requirement.body().as_str(), *requirement.modality()) {
            warnings.push(Warning::ModalityNotStated {
                requirement: id,
                modality: *requirement.modality(),
            });
        }
        let vacuous = *requirement.refinement() == Some(Refinement::And)
            && refiners.get(&id).is_some_and(|refiners| {
                refiners
                    .iter()
                    .all(|refiner| *refiner.modality() == Modality::May)
            });
        if vacuous {
            warnings.push(Warning::VacuousAnd { requirement: id });
        }
    }
    warnings
}

/// Every ID is unique across the document. IDs of different kinds can't
/// collide, because the prefix is part of the ID.
fn duplicate_ids(graph: &SpecGraph, requirements: &[&Requirement]) -> Vec<Violation> {
    let sections = graph
        .sections()
        .into_iter()
        .map(|section| *section.id())
        .chain([*graph.glossary().id()]);
    let criteria = requirements
        .iter()
        .flat_map(|requirement| requirement.acceptance_criteria())
        .map(|criterion| *criterion.id());
    let terms = graph.glossary().terms().iter().map(|term| *term.id());

    let mut violations = repeated(sections);
    violations.extend(repeated(requirements.iter().map(|r| *r.id())));
    violations.extend(repeated(criteria));
    violations.extend(repeated(terms));
    violations
}

/// Report each ID that appears more than once, once, in order of its
/// second appearance.
fn repeated<T: Copy + Display + Eq + Hash>(ids: impl IntoIterator<Item = T>) -> Vec<Violation> {
    let mut seen = HashSet::new();
    let mut reported = HashSet::new();
    ids.into_iter()
        .filter(|id| !seen.insert(*id) && reported.insert(*id))
        .map(|id| Violation::DuplicateId(id.to_string()))
        .collect()
}

/// Every ID in `refines`, `dependsOn`, and `usesTerm` names an existing
/// node. The ID's type already guarantees the node is of an allowed kind.
fn dangling_references(graph: &SpecGraph, requirements: &[&Requirement]) -> Vec<Violation> {
    let sections = graph.sections();
    let section_ids: HashSet<_> = sections
        .iter()
        .map(|section| *section.id())
        .chain([*graph.glossary().id()])
        .collect();
    let requirement_ids: HashSet<_> = requirements.iter().map(|r| *r.id()).collect();
    let term_ids: HashSet<_> = graph.glossary().terms().iter().map(|t| *t.id()).collect();

    let dangling = |from: String, relation: Relation, to: String| Violation::DanglingReference {
        from,
        relation,
        to,
    };
    let mut violations = Vec::new();
    for section in &sections {
        for term in section.uses_term() {
            if !term_ids.contains(term) {
                let from = section.id().to_string();
                violations.push(dangling(from, Relation::UsesTerm, term.to_string()));
            }
        }
    }
    for requirement in requirements {
        let from = requirement.id().to_string();
        for target in requirement.refines() {
            if !requirement_ids.contains(target) {
                violations.push(dangling(
                    from.clone(),
                    Relation::Refines,
                    target.to_string(),
                ));
            }
        }
        for target in requirement.depends_on() {
            let exists = match target {
                DependencyId::Section(id) => section_ids.contains(id),
                DependencyId::Requirement(id) => requirement_ids.contains(id),
            };
            if !exists {
                violations.push(dangling(
                    from.clone(),
                    Relation::DependsOn,
                    target.to_string(),
                ));
            }
        }
        for term in requirement.uses_term() {
            if !term_ids.contains(term) {
                violations.push(dangling(from.clone(), Relation::UsesTerm, term.to_string()));
            }
        }
    }
    violations
}

/// `refines` is acyclic, and `dependsOn` is acyclic. Only Requirements have
/// outgoing edges, so only they can lie on a cycle.
fn cycles(requirements: &[&Requirement]) -> Vec<Violation> {
    let refines = edges(requirements, |requirement| requirement.refines().clone());
    let depends_on = edges(requirements, |requirement| {
        requirement
            .depends_on()
            .iter()
            .filter_map(|target| match target {
                DependencyId::Requirement(id) => Some(*id),
                DependencyId::Section(_) => None,
            })
            .collect()
    });

    [
        (Relation::Refines, refines),
        (Relation::DependsOn, depends_on),
    ]
    .into_iter()
    .filter_map(|(relation, edges)| {
        find_cycle(&edges).map(|path| Violation::Cycle { relation, path })
    })
    .collect()
}

/// The edges among requirements, keyed by source. Edges to IDs that name no
/// requirement are dropped; [dangling_references] reports them.
fn edges(
    requirements: &[&Requirement],
    targets: impl Fn(&Requirement) -> Vec<RequirementId>,
) -> BTreeMap<RequirementId, Vec<RequirementId>> {
    let mut edges: BTreeMap<_, Vec<_>> = requirements
        .iter()
        .map(|requirement| (*requirement.id(), Vec::new()))
        .collect();
    for requirement in requirements {
        let targets = targets(requirement);
        let known: Vec<_> = targets
            .into_iter()
            .filter(|target| edges.contains_key(target))
            .collect();
        edges.entry(*requirement.id()).or_default().extend(known);
    }
    edges
}

/// Find a cycle in a directed graph, or `None` if it is acyclic.
///
/// Repeatedly removes nodes with no outgoing edges. Every node left after
/// that has an edge to another node left, so walking those edges from any
/// of them must revisit a node, closing a cycle. Iterative, so a long chain
/// can't overflow the stack.
fn find_cycle(edges: &BTreeMap<RequirementId, Vec<RequirementId>>) -> Option<Vec<RequirementId>> {
    let mut predecessors: HashMap<RequirementId, Vec<RequirementId>> = HashMap::new();
    let mut out_degree: HashMap<RequirementId, usize> = HashMap::new();
    for (source, targets) in edges {
        out_degree.insert(*source, targets.len());
        for target in targets {
            predecessors.entry(*target).or_default().push(*source);
        }
    }

    let mut sinks: Vec<_> = out_degree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(node, _)| *node)
        .collect();
    while let Some(sink) = sinks.pop() {
        out_degree.remove(&sink);
        for predecessor in predecessors.get(&sink).into_iter().flatten() {
            if let Some(degree) = out_degree.get_mut(predecessor) {
                *degree -= 1;
                if *degree == 0 {
                    sinks.push(*predecessor);
                }
            }
        }
    }

    // Start from the smallest remaining ID so the report is deterministic.
    let start = edges.keys().find(|node| out_degree.contains_key(node))?;
    let mut path = vec![*start];
    let mut position = HashMap::from([(*start, 0)]);
    let mut node = *start;
    loop {
        node = *edges[&node]
            .iter()
            .find(|next| out_degree.contains_key(next))
            .expect("every remaining node has an edge to another remaining node");
        if let Some(&index) = position.get(&node) {
            let mut cycle = path.split_off(index);
            cycle.push(node);
            return Some(cycle);
        }
        position.insert(node, path.len());
        path.push(node);
    }
}

/// The requirements that refine each requirement, in document order.
fn refiners<'a>(requirements: &[&'a Requirement]) -> HashMap<RequirementId, Vec<&'a Requirement>> {
    let mut refiners: HashMap<_, Vec<_>> = HashMap::new();
    for requirement in requirements {
        for target in requirement.refines() {
            refiners.entry(*target).or_default().push(*requirement);
        }
    }
    refiners
}

/// `refinement` is present if and only if at least one requirement refines
/// this one.
fn refinement_presence(requirements: &[&Requirement]) -> Vec<Violation> {
    let refiners = refiners(requirements);
    requirements
        .iter()
        .filter_map(|requirement| {
            let id = *requirement.id();
            let refiner = refiners.get(&id).and_then(|refiners| refiners.first());
            match (requirement.refinement(), refiner) {
                (None, Some(refiner)) => Some(Violation::MissingRefinement {
                    requirement: id,
                    refiner: *refiner.id(),
                }),
                (Some(_), None) => Some(Violation::UnexpectedRefinement { requirement: id }),
                _ => None,
            }
        })
        .collect()
}

/// Glossary terms are serialized in ID order. A repeated ID is reported by
/// [duplicate_ids] instead.
fn term_order(graph: &SpecGraph) -> Vec<Violation> {
    graph
        .glossary()
        .terms()
        .windows(2)
        .filter(|pair| pair[1].id() < pair[0].id())
        .map(|pair| Violation::TermOutOfOrder {
            term: *pair[1].id(),
            previous: *pair[0].id(),
        })
        .collect()
}

/// Whether `body` states `modality` as a whole phrase: "MUST" alone does not
/// state MUST NOT, and "MUST NOT" does not state MUST.
fn states_modality(body: &str, modality: Modality) -> bool {
    let words: Vec<&str> = body
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    (0..words.len()).any(|index| modality_at(&words[index..]) == Some(modality))
}

/// The modality spelled by the words at the start of `words`, if any.
fn modality_at(words: &[&str]) -> Option<Modality> {
    let negated = words.get(1) == Some(&"NOT");
    match words.first() {
        Some(&"MUST") if negated => Some(Modality::MustNot),
        Some(&"MUST") => Some(Modality::Must),
        Some(&"SHOULD") if negated => Some(Modality::ShouldNot),
        Some(&"SHOULD") => Some(Modality::Should),
        Some(&"MAY") => Some(Modality::May),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const TODO_API: &str = include_str!("../../tests/fixtures/todo-api.spec.json");

    /// Read a graph without checking its invariants.
    fn unchecked(value: Value) -> SpecGraph {
        SpecGraph::deserialize(value).unwrap()
    }

    fn violations(value: Value) -> Vec<Violation> {
        match invariants(&unchecked(value)) {
            Ok(()) => Vec::new(),
            Err(violations) => violations.violations,
        }
    }

    fn req(id: &str, body: &str, modality: &str) -> Value {
        json!({
            "kind": "requirement",
            "id": id,
            "title": format!("Requirement {id}"),
            "body": body,
            "modality": modality
        })
    }

    /// A graph whose root holds `children` and whose glossary holds `terms`.
    fn graph(children: Vec<Value>, terms: Vec<Value>) -> Value {
        let mut root = json!({ "kind": "section", "id": "SEC-001", "title": "Root" });
        if !children.is_empty() {
            root["children"] = json!(children);
        }
        let mut glossary = json!({ "kind": "section", "id": "SEC-100", "title": "Glossary" });
        if !terms.is_empty() {
            glossary["terms"] = json!(terms);
        }
        json!({
            "formatVersion": "1",
            "spec": { "name": "test" },
            "root": root,
            "glossary": glossary
        })
    }

    fn term(id: &str) -> Value {
        json!({ "kind": "term", "id": id, "title": format!("Term {id}"), "definition": "Defined." })
    }

    fn rid(number: u32) -> RequirementId {
        RequirementId::new(number)
    }

    #[test]
    fn the_canon_example_satisfies_every_invariant() {
        let value: Value = serde_json::from_str(TODO_API).unwrap();
        assert_eq!(violations(value), []);
    }

    #[test]
    fn a_minimal_graph_satisfies_every_invariant() {
        assert_eq!(violations(graph(vec![], vec![])), []);
    }

    #[test]
    fn duplicate_requirement_ids_are_reported_once() {
        let value = graph(
            vec![
                req("REQ-001", "It MUST work.", "MUST"),
                req("REQ-001", "It MUST work.", "MUST"),
                req("REQ-001", "It MUST work.", "MUST"),
            ],
            vec![],
        );
        assert_eq!(
            violations(value),
            [Violation::DuplicateId("REQ-001".to_owned())]
        );
    }

    #[test]
    fn the_glossary_id_must_not_repeat_a_section_id() {
        let mut value = graph(vec![], vec![]);
        value["glossary"]["id"] = json!("SEC-001");
        assert_eq!(
            violations(value),
            [Violation::DuplicateId("SEC-001".to_owned())]
        );
    }

    #[test]
    fn duplicate_ids_are_found_at_any_depth() {
        let mut parent = req("REQ-001", "It MUST work.", "MUST");
        parent["acceptanceCriteria"] = json!([
            { "kind": "acceptanceCriterion", "id": "AC-001", "title": "One", "body": "One." },
            { "kind": "acceptanceCriterion", "id": "AC-001", "title": "Two", "body": "Two." }
        ]);
        let nested = json!({
            "kind": "section",
            "id": "SEC-002",
            "title": "Nested",
            "children": [parent, { "kind": "section", "id": "SEC-002", "title": "Again" }]
        });
        let value = graph(vec![nested], vec![term("TERM-001"), term("TERM-001")]);
        assert_eq!(
            violations(value),
            [
                Violation::DuplicateId("SEC-002".to_owned()),
                Violation::DuplicateId("AC-001".to_owned()),
                Violation::DuplicateId("TERM-001".to_owned()),
            ]
        );
    }

    #[test]
    fn ids_of_different_kinds_may_share_a_number() {
        let mut requirement = req("REQ-001", "It MUST work.", "MUST");
        requirement["usesTerm"] = json!(["TERM-001"]);
        let value = graph(vec![requirement], vec![term("TERM-001")]);
        assert_eq!(violations(value), []);
    }

    #[test]
    fn dangling_references_are_reported() {
        let mut requirement = req("REQ-001", "It MUST work.", "MUST");
        requirement["refines"] = json!(["REQ-009"]);
        requirement["dependsOn"] = json!(["SEC-009", "REQ-008"]);
        requirement["usesTerm"] = json!(["TERM-009"]);
        let mut value = graph(vec![requirement], vec![]);
        value["root"]["usesTerm"] = json!(["TERM-007"]);
        let dangling = |from: &str, relation, to: &str| Violation::DanglingReference {
            from: from.to_owned(),
            relation,
            to: to.to_owned(),
        };
        assert_eq!(
            violations(value),
            [
                dangling("SEC-001", Relation::UsesTerm, "TERM-007"),
                dangling("REQ-001", Relation::Refines, "REQ-009"),
                dangling("REQ-001", Relation::DependsOn, "SEC-009"),
                dangling("REQ-001", Relation::DependsOn, "REQ-008"),
                dangling("REQ-001", Relation::UsesTerm, "TERM-009"),
            ]
        );
    }

    #[test]
    fn depends_on_may_name_any_section_including_the_glossary() {
        let mut requirement = req("REQ-001", "It MUST work.", "MUST");
        requirement["dependsOn"] = json!(["SEC-001", "SEC-100"]);
        assert_eq!(violations(graph(vec![requirement], vec![])), []);
    }

    #[test]
    fn a_requirement_refining_itself_is_a_cycle() {
        let mut requirement = req("REQ-001", "It MUST work.", "MUST");
        requirement["refines"] = json!(["REQ-001"]);
        requirement["refinement"] = json!("AND");
        assert_eq!(
            violations(graph(vec![requirement], vec![])),
            [Violation::Cycle {
                relation: Relation::Refines,
                path: vec![rid(1), rid(1)],
            }]
        );
    }

    #[test]
    fn refinement_cycles_are_reported_with_their_path() {
        let mut one = req("REQ-001", "It MUST work.", "MUST");
        let mut two = req("REQ-002", "It MUST work.", "MUST");
        let mut three = req("REQ-003", "It MUST work.", "MUST");
        one["refines"] = json!(["REQ-002"]);
        two["refines"] = json!(["REQ-003"]);
        three["refines"] = json!(["REQ-001"]);
        for node in [&mut one, &mut two, &mut three] {
            node["refinement"] = json!("AND");
        }
        assert_eq!(
            violations(graph(vec![one, two, three], vec![])),
            [Violation::Cycle {
                relation: Relation::Refines,
                path: vec![rid(1), rid(2), rid(3), rid(1)],
            }]
        );
    }

    #[test]
    fn a_cycle_downstream_of_other_nodes_is_found() {
        // REQ-001 → REQ-002 ⇄ REQ-003, with REQ-004 a leaf hanging off REQ-002.
        let mut one = req("REQ-001", "It MUST work.", "MUST");
        let mut two = req("REQ-002", "It MUST work.", "MUST");
        let mut three = req("REQ-003", "It MUST work.", "MUST");
        let four = req("REQ-004", "It MUST work.", "MUST");
        one["dependsOn"] = json!(["REQ-002"]);
        two["dependsOn"] = json!(["REQ-004", "REQ-003"]);
        three["dependsOn"] = json!(["REQ-002", "SEC-001"]);
        assert_eq!(
            violations(graph(vec![one, two, three, four], vec![])),
            [Violation::Cycle {
                relation: Relation::DependsOn,
                path: vec![rid(2), rid(3), rid(2)],
            }]
        );
    }

    #[test]
    fn shared_refinement_parents_are_not_cycles() {
        // A diamond: REQ-002 and REQ-003 refine REQ-001; REQ-004 refines both.
        let mut one = req("REQ-001", "It MUST work.", "MUST");
        let mut two = req("REQ-002", "It MUST work.", "MUST");
        let mut three = req("REQ-003", "It MUST work.", "MUST");
        let mut four = req("REQ-004", "It MUST work.", "MUST");
        one["refinement"] = json!("AND");
        two["refinement"] = json!("OR");
        three["refinement"] = json!("XOR");
        two["refines"] = json!(["REQ-001"]);
        three["refines"] = json!(["REQ-001"]);
        four["refines"] = json!(["REQ-002", "REQ-003"]);
        assert_eq!(violations(graph(vec![one, two, three, four], vec![])), []);
    }

    #[test]
    fn long_chains_do_not_overflow_the_stack() {
        let chain: Vec<Value> = (1..=20_000)
            .map(|number| {
                let mut requirement = req(&format!("REQ-{number:03}"), "It MUST work.", "MUST");
                if number > 1 {
                    requirement["dependsOn"] = json!([format!("REQ-{:03}", number - 1)]);
                }
                requirement
            })
            .collect();
        assert_eq!(violations(graph(chain, vec![])), []);
    }

    #[test]
    fn edges_to_missing_requirements_do_not_count_toward_cycles() {
        let mut requirement = req("REQ-001", "It MUST work.", "MUST");
        requirement["dependsOn"] = json!(["REQ-002"]);
        let found = violations(graph(vec![requirement], vec![]));
        assert!(matches!(
            found.as_slice(),
            [Violation::DanglingReference { .. }]
        ));
    }

    #[test]
    fn both_relations_report_their_own_cycle() {
        let mut one = req("REQ-001", "It MUST work.", "MUST");
        one["refines"] = json!(["REQ-001"]);
        one["refinement"] = json!("AND");
        one["dependsOn"] = json!(["REQ-001"]);
        let found = violations(graph(vec![one], vec![]));
        assert_eq!(
            found,
            [
                Violation::Cycle {
                    relation: Relation::Refines,
                    path: vec![rid(1), rid(1)],
                },
                Violation::Cycle {
                    relation: Relation::DependsOn,
                    path: vec![rid(1), rid(1)],
                },
            ]
        );
    }

    #[test]
    fn a_refined_requirement_needs_a_refinement() {
        let parent = req("REQ-001", "It MUST work.", "MUST");
        let mut child = req("REQ-002", "It MUST work.", "MUST");
        child["refines"] = json!(["REQ-001"]);
        assert_eq!(
            violations(graph(vec![parent, child], vec![])),
            [Violation::MissingRefinement {
                requirement: rid(1),
                refiner: rid(2),
            }]
        );
    }

    #[test]
    fn a_leaf_must_not_have_a_refinement() {
        let mut leaf = req("REQ-001", "It MUST work.", "MUST");
        leaf["refinement"] = json!("OR");
        assert_eq!(
            violations(graph(vec![leaf], vec![])),
            [Violation::UnexpectedRefinement {
                requirement: rid(1),
            }]
        );
    }

    #[test]
    fn nesting_alone_does_not_require_a_refinement() {
        // A child nested purely to elaborate its parent refines nothing.
        let mut parent = req("REQ-001", "Input MUST be sanitized.", "MUST");
        parent["children"] = json!([req("REQ-002", "SQL MUST be neutralized.", "MUST")]);
        assert_eq!(violations(graph(vec![parent], vec![])), []);
    }

    #[test]
    fn terms_must_be_in_id_order() {
        let terms = vec![
            term("TERM-002"),
            term("TERM-001"),
            term("TERM-1000"),
            term("TERM-999"),
        ];
        assert_eq!(
            violations(graph(vec![], terms)),
            [
                Violation::TermOutOfOrder {
                    term: TermId::new(1),
                    previous: TermId::new(2),
                },
                Violation::TermOutOfOrder {
                    term: TermId::new(999),
                    previous: TermId::new(1000),
                },
            ]
        );
    }

    #[test]
    fn term_order_is_numeric() {
        let terms = vec![term("TERM-999"), term("TERM-1000")];
        assert_eq!(violations(graph(vec![], terms)), []);
    }

    #[test]
    fn every_violation_is_reported_together() {
        let mut leaf = req("REQ-001", "It MUST work.", "MUST");
        leaf["refinement"] = json!("OR");
        leaf["usesTerm"] = json!(["TERM-005"]);
        let found = violations(graph(vec![leaf], vec![term("TERM-002"), term("TERM-001")]));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn violations_render_as_a_list() {
        let mut leaf = req("REQ-001", "It MUST work.", "MUST");
        leaf["refinement"] = json!("OR");
        leaf["usesTerm"] = json!(["TERM-005"]);
        let err = invariants(&unchecked(graph(vec![leaf], vec![]))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the specification graph breaks its invariants:\n  \
             - REQ-001 names TERM-005 in `usesTerm`, but no such node exists\n  \
             - REQ-001 has a `refinement`, but no requirement refines it"
        );
    }

    #[test]
    fn cycles_render_with_arrows() {
        let violation = Violation::Cycle {
            relation: Relation::DependsOn,
            path: vec![rid(1), rid(2), rid(1)],
        };
        assert_eq!(
            violation.to_string(),
            "`dependsOn` forms a cycle: REQ-001 → REQ-002 → REQ-001"
        );
    }

    #[test]
    fn a_body_without_its_modality_is_a_warning() {
        let value = graph(
            vec![req("REQ-001", "There is an endpoint.", "MUST")],
            vec![],
        );
        assert_eq!(
            unchecked(value).warnings(),
            [Warning::ModalityNotStated {
                requirement: rid(1),
                modality: Modality::Must,
            }]
        );
    }

    #[test]
    fn modality_keywords_match_whole_phrases() {
        let cases = [
            ("It MUST work.", Modality::Must, true),
            ("It MUST NOT fail.", Modality::MustNot, true),
            ("It MUST NOT fail.", Modality::Must, false),
            ("It MUST work.", Modality::MustNot, false),
            ("It MUST work and MUST NOT fail.", Modality::Must, true),
            ("It SHOULD\nNOT fail.", Modality::ShouldNot, true),
            ("It SHOULD work.", Modality::Should, true),
            ("It **MAY** work.", Modality::May, true),
            ("It must work.", Modality::Must, false),
            ("MUSTARD is a condiment.", Modality::Must, false),
            ("It MAYBE works.", Modality::May, false),
        ];
        for (body, modality, expected) in cases {
            assert_eq!(
                states_modality(body, modality),
                expected,
                "{body:?} {modality}"
            );
        }
    }

    #[test]
    fn an_and_of_only_may_refinements_is_a_warning() {
        let mut parent = req("REQ-001", "It MUST work.", "MUST");
        parent["refinement"] = json!("AND");
        let mut one = req("REQ-002", "It MAY do one thing.", "MAY");
        let mut two = req("REQ-003", "It MAY do another.", "MAY");
        one["refines"] = json!(["REQ-001"]);
        two["refines"] = json!(["REQ-001"]);
        assert_eq!(
            unchecked(graph(vec![parent, one, two], vec![])).warnings(),
            [Warning::VacuousAnd {
                requirement: rid(1),
            }]
        );
    }

    #[test]
    fn an_and_with_any_stronger_refinement_is_not_vacuous() {
        let mut parent = req("REQ-001", "It MUST work.", "MUST");
        parent["refinement"] = json!("AND");
        let mut one = req("REQ-002", "It MAY do one thing.", "MAY");
        let mut two = req("REQ-003", "It SHOULD do another.", "SHOULD");
        one["refines"] = json!(["REQ-001"]);
        two["refines"] = json!(["REQ-001"]);
        assert_eq!(
            unchecked(graph(vec![parent, one, two], vec![])).warnings(),
            []
        );
    }

    #[test]
    fn an_or_of_may_refinements_is_not_vacuous() {
        // The canon example's authentication requirement has this shape.
        let mut parent = req("REQ-001", "It MUST accept a token.", "MUST");
        parent["refinement"] = json!("OR");
        let mut one = req("REQ-002", "It MAY accept OAuth.", "MAY");
        one["refines"] = json!(["REQ-001"]);
        assert_eq!(unchecked(graph(vec![parent, one], vec![])).warnings(), []);
    }
}
