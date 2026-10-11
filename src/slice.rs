//! The agent slice of section 9 of the SLCR design document: the smallest
//! subgraph an agent needs to implement or verify one requirement R.
//!
//! [`members`](crate::slice::members) finds what `slice(R)` includes by
//! querying the specification's graph ([`SpecGraph`](crate::query::SpecGraph)),
//! and how much of each node it includes ([`Detail`](crate::slice::Detail)):
//!
//! 1. R itself, in full.
//! 2. R's containment ancestors, root to parent: bodies only.
//! 3. R's refinement descendants, transitively, in full.
//! 4. The requirements R refines, one level up: bodies only.
//! 5. The `dependsOn` closure of R and its refinement descendants: each
//!    target's body and acceptance criteria, plus its containment
//!    ancestors' bodies.
//! 6. The Terms that anything included uses.
//!
//! Nothing else: siblings of R and of its ancestors, the other children of
//! what R refines, and the descendants of what it depends on are left out.
//! A nested requirement that doesn't refine R is not a refinement
//! descendant, so it is left out too.
//!
//! Each closure is reached one edge at a time, until a step finds nothing
//! new. A quantified pattern such as `-[:REFINES]->{1,}` would match once
//! per path, and a DAG can have exponentially many paths between two nodes;
//! a step at a time visits each node once.

use std::collections::{BTreeMap, BTreeSet};

use slcr_gql::ast::Query;
use slcr_gql::{Error, Params, Value, parse_query};

use crate::query::{Cell, SpecGraph};
use crate::spec::id::{DependencyId, RequirementId, TermId};

/// How much of a node a slice includes. A node included for several
/// reasons is included in the greatest detail any of them asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Detail {
    /// The body only: a containment ancestor, or a requirement R refines.
    Body,
    /// The body and acceptance criteria, plus what the node depends on: a
    /// `dependsOn` target.
    Dependency,
    /// Everything: R and its refinement descendants.
    Full,
}

/// What the slice of one requirement includes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Members {
    requirement: RequirementId,
    /// Every included Section and Requirement, with its detail.
    nodes: BTreeMap<DependencyId, Detail>,
    terms: BTreeSet<TermId>,
}

impl Members {
    /// The requirement the slice is for.
    pub fn requirement(&self) -> RequirementId {
        self.requirement
    }

    /// How much of the Section or Requirement `id` the slice includes, if
    /// any of it.
    pub fn detail(&self, id: DependencyId) -> Option<Detail> {
        self.nodes.get(&id).copied()
    }

    /// Whether the slice includes the Term `id`.
    pub fn has_term(&self, id: TermId) -> bool {
        self.terms.contains(&id)
    }

    /// Every included Section and Requirement, with its detail, in ID order.
    pub fn nodes(&self) -> &BTreeMap<DependencyId, Detail> {
        &self.nodes
    }

    /// Every included Term, in ID order.
    pub fn terms(&self) -> &BTreeSet<TermId> {
        &self.terms
    }

    /// Include each of `ids` in at least `detail`.
    fn include(&mut self, ids: &BTreeSet<String>, detail: Detail) {
        for id in ids {
            let id = id.parse().expect("a Section or Requirement ID");
            let included = self.nodes.entry(id).or_insert(detail);
            *included = (*included).max(detail);
        }
    }
}

/// The queries that take one step along an edge: each returns the IDs one
/// edge away from the nodes whose IDs are in `$frontier`.
mod hop {
    /// From a requirement to the requirements that refine it.
    pub const REFINERS: &str =
        "MATCH (d:Requirement)-[:REFINES]->(r) WHERE r.id IN $frontier RETURN DISTINCT d.id";
    /// From a requirement to the requirements it refines.
    pub const REFINED: &str =
        "MATCH (r)-[:REFINES]->(p:Requirement) WHERE r.id IN $frontier RETURN DISTINCT p.id";
    /// From a requirement to the Sections and Requirements it depends on.
    pub const DEPENDENCIES: &str =
        "MATCH (s)-[:DEPENDS_ON]->(t) WHERE s.id IN $frontier RETURN DISTINCT t.id";
    /// From a node to its containment parent.
    pub const PARENTS: &str =
        "MATCH (p)-[:CONTAINS]->(c) WHERE c.id IN $frontier RETURN DISTINCT p.id";
    /// From a Section or Requirement to the Terms it uses.
    pub const TERMS: &str =
        "MATCH (n)-[:USES_TERM]->(t:Term) WHERE n.id IN $frontier RETURN DISTINCT t.id";
}

/// What the slice of `requirement` includes from `graph`, which must hold
/// it.
pub async fn members(graph: &SpecGraph, requirement: RequirementId) -> Result<Members, Error> {
    let own = BTreeSet::from([requirement.to_string()]);
    let descendants = reach(graph, &parse_query(hop::REFINERS)?, &own).await?;
    let refined = step(graph, &parse_query(hop::REFINED)?, &own).await?;
    let implemented: BTreeSet<String> = own.union(&descendants).cloned().collect();
    let dependencies = reach(graph, &parse_query(hop::DEPENDENCIES)?, &implemented).await?;
    let placed: BTreeSet<String> = own.union(&dependencies).cloned().collect();
    let ancestors = reach(graph, &parse_query(hop::PARENTS)?, &placed).await?;

    let mut members = Members {
        requirement,
        nodes: BTreeMap::new(),
        terms: BTreeSet::new(),
    };
    members.include(&ancestors, Detail::Body);
    members.include(&refined, Detail::Body);
    members.include(&dependencies, Detail::Dependency);
    members.include(&implemented, Detail::Full);

    let included = members.nodes.keys().map(ToString::to_string).collect();
    members.terms = step(graph, &parse_query(hop::TERMS)?, &included)
        .await?
        .iter()
        .map(|id| id.parse().expect("a Term ID"))
        .collect();
    Ok(members)
}

/// Every node reachable from `start` by one or more steps of `hop`, which
/// may include nodes in `start`.
async fn reach(
    graph: &SpecGraph,
    hop: &Query,
    start: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Error> {
    let mut reached = BTreeSet::new();
    let mut frontier = start.clone();
    while !frontier.is_empty() {
        let next = step(graph, hop, &frontier).await?;
        frontier = next.difference(&reached).cloned().collect();
        reached.extend(frontier.iter().cloned());
    }
    Ok(reached)
}

/// The IDs one step of `hop` away from `frontier`.
async fn step(
    graph: &SpecGraph,
    hop: &Query,
    frontier: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Error> {
    let ids = frontier.iter().cloned().map(Value::String).collect();
    let params = Params::from([("frontier".to_owned(), Value::List(ids))]);
    let rows = graph.query(hop, &params).await?;
    Ok(rows
        .rows
        .into_iter()
        .flatten()
        .map(|cell| match cell {
            Cell::String(id) => id,
            other => panic!("a hop returns IDs, not {other:?}"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value as Json, json};

    use super::Detail::{Body, Dependency, Full};
    use super::*;
    use crate::cli::block_on;
    use crate::spec::graph::SlcrRequirementsDocument;

    const TODO_API_JSON: &str = include_str!("../tests/fixtures/todo-api.spec.json");

    fn canon() -> SlcrRequirementsDocument {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    /// A checked document whose root, SEC-001, contains `children`, and
    /// whose Glossary, SEC-900, holds `terms`.
    fn spec(children: Json, terms: Json) -> SlcrRequirementsDocument {
        let mut root = json!({ "kind": "section", "id": "SEC-001", "title": "Root" });
        if children != json!([]) {
            root["children"] = children;
        }
        let mut glossary = json!({ "kind": "section", "id": "SEC-900", "title": "Glossary" });
        if terms != json!([]) {
            glossary["terms"] = terms;
        }
        serde_json::from_value(json!({
            "formatVersion": "1",
            "spec": { "name": "test" },
            "root": root,
            "glossary": glossary,
        }))
        .unwrap()
    }

    fn section(id: u32, fields: Json) -> Json {
        with(
            json!({ "kind": "section", "id": format!("SEC-{id:03}"), "title": "Section" }),
            fields,
        )
    }

    /// A MUST requirement with `fields`.
    fn requirement(id: u32, fields: Json) -> Json {
        with(
            json!({
                "kind": "requirement",
                "id": format!("REQ-{id:03}"),
                "title": "Requirement",
                "body": "It MUST hold.",
                "modality": "MUST",
            }),
            fields,
        )
    }

    fn term(id: u32) -> Json {
        json!({
            "kind": "term",
            "id": format!("TERM-{id:03}"),
            "title": format!("Term {id}"),
            "definition": "A term.",
        })
    }

    fn with(mut node: Json, fields: Json) -> Json {
        let node_fields = node.as_object_mut().unwrap();
        for (key, value) in fields.as_object().unwrap() {
            node_fields.insert(key.clone(), value.clone());
        }
        node
    }

    fn members_of(document: &SlcrRequirementsDocument, id: u32) -> Members {
        block_on(async {
            let graph = SpecGraph::new(document).await.unwrap();
            members(&graph, RequirementId::new(id)).await.unwrap()
        })
        .unwrap()
    }

    /// Each included node's ID and detail, then each included Term's ID.
    fn summary(members: &Members) -> (Vec<(String, Detail)>, Vec<String>) {
        let nodes = members
            .nodes()
            .iter()
            .map(|(id, detail)| (id.to_string(), *detail))
            .collect();
        let terms = members.terms().iter().map(ToString::to_string).collect();
        (nodes, terms)
    }

    fn nodes(expected: &[(&str, Detail)]) -> Vec<(String, Detail)> {
        expected
            .iter()
            .map(|(id, detail)| ((*id).to_owned(), *detail))
            .collect()
    }

    #[test]
    fn a_requirement_brings_its_ancestors_and_refinement_descendants() {
        // REQ-001 is refined by REQ-002 and REQ-003, and REQ-004 refines
        // both; REQ-002 also contains REQ-004.
        let members = members_of(&canon(), 1);
        assert_eq!(members.requirement(), RequirementId::new(1));
        assert_eq!(
            summary(&members),
            (
                nodes(&[
                    ("SEC-001", Body),
                    ("SEC-002", Body),
                    ("REQ-001", Full),
                    ("REQ-002", Full),
                    ("REQ-003", Full),
                    ("REQ-004", Full),
                ]),
                vec![],
            )
        );
    }

    #[test]
    fn a_requirement_brings_what_it_refines_but_not_their_other_children() {
        // REQ-003 refines REQ-001, its sibling REQ-002 does too, and
        // REQ-004, housed under REQ-002, refines REQ-003.
        let (included, _) = summary(&members_of(&canon(), 3));
        assert_eq!(
            included,
            nodes(&[
                ("SEC-001", Body),
                ("SEC-002", Body),
                ("REQ-001", Body),
                ("REQ-003", Full),
                ("REQ-004", Full),
            ])
        );
    }

    #[test]
    fn a_shared_requirement_brings_every_requirement_it_refines() {
        let (included, _) = summary(&members_of(&canon(), 4));
        assert_eq!(
            included,
            nodes(&[
                ("SEC-001", Body),
                ("SEC-002", Body),
                ("REQ-001", Body),
                ("REQ-002", Body),
                ("REQ-003", Body),
                ("REQ-004", Full),
            ])
        );
    }

    #[test]
    fn dependencies_bring_their_ancestors_and_terms() {
        // REQ-005 depends on REQ-001 and REQ-007, which uses TERM-001.
        let members = members_of(&canon(), 5);
        assert_eq!(
            summary(&members),
            (
                nodes(&[
                    ("SEC-001", Body),
                    ("SEC-002", Body),
                    ("SEC-003", Body),
                    ("SEC-004", Body),
                    ("REQ-001", Dependency),
                    ("REQ-005", Full),
                    ("REQ-006", Full),
                    ("REQ-007", Dependency),
                ]),
                vec!["TERM-001".to_owned()],
            )
        );
        assert!(members.has_term(TermId::new(1)));
        assert_eq!(members.detail("REQ-002".parse().unwrap()), None);
    }

    #[test]
    fn a_refinement_descendant_s_dependencies_are_included() {
        // REQ-006 refines REQ-005, so REQ-005's slice holds REQ-006's
        // dependencies; REQ-006's slice holds its own, and REQ-005's body.
        let mut value: Json = serde_json::from_str(TODO_API_JSON).unwrap();
        value["root"]["children"][1]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("dependsOn");
        value["root"]["children"][1]["children"][0]["children"][0]["dependsOn"] =
            json!(["REQ-007"]);
        let document: SlcrRequirementsDocument = serde_json::from_value(value).unwrap();
        let (five, _) = summary(&members_of(&document, 5));
        assert!(
            five.contains(&("REQ-007".to_owned(), Dependency)),
            "{five:?}"
        );
        let (six, _) = summary(&members_of(&document, 6));
        assert_eq!(
            six,
            nodes(&[
                ("SEC-001", Body),
                ("SEC-003", Body),
                ("SEC-004", Body),
                ("REQ-005", Body),
                ("REQ-006", Full),
                ("REQ-007", Dependency),
            ])
        );
    }

    #[test]
    fn a_leaf_with_nothing_else_brings_only_its_ancestors_and_terms() {
        let members = members_of(&canon(), 7);
        assert_eq!(
            summary(&members),
            (
                nodes(&[("SEC-001", Body), ("SEC-004", Body), ("REQ-007", Full)]),
                vec!["TERM-001".to_owned()],
            )
        );
    }

    #[test]
    fn nesting_without_refinement_is_left_out() {
        // REQ-002 is nested under REQ-001 to elaborate it, but refines
        // nothing; REQ-003 refines it.
        let document = spec(
            json!([requirement(
                1,
                json!({
                    "refinement": "AND",
                    "children": [
                        requirement(2, json!({})),
                        requirement(3, json!({ "refines": ["REQ-001"] })),
                    ],
                })
            )]),
            json!([]),
        );
        let (one, _) = summary(&members_of(&document, 1));
        assert_eq!(
            one,
            nodes(&[("SEC-001", Body), ("REQ-001", Full), ("REQ-003", Full)])
        );
        // The nested requirement's slice holds its container's body only.
        let (two, _) = summary(&members_of(&document, 2));
        assert_eq!(
            two,
            nodes(&[("SEC-001", Body), ("REQ-001", Body), ("REQ-002", Full)])
        );
    }

    #[test]
    fn dependencies_are_followed_transitively_but_not_into_their_descendants() {
        // REQ-001 depends on REQ-002, which depends on SEC-003 and is
        // refined by REQ-004. SEC-003 contains REQ-005, which is left out.
        let document = spec(
            json!([
                requirement(1, json!({ "dependsOn": ["REQ-002"] })),
                section(
                    2,
                    json!({
                        "body": "Background.",
                        "children": [
                            requirement(2, json!({
                                "refinement": "AND",
                                "dependsOn": ["SEC-003"],
                                "children": [requirement(4, json!({ "refines": ["REQ-002"] }))],
                            })),
                            requirement(3, json!({})),
                        ],
                    })
                ),
                section(3, json!({ "children": [requirement(5, json!({}))] })),
            ]),
            json!([]),
        );
        let (one, _) = summary(&members_of(&document, 1));
        assert_eq!(
            one,
            nodes(&[
                ("SEC-001", Body),
                ("SEC-002", Body),
                ("SEC-003", Dependency),
                ("REQ-001", Full),
                ("REQ-002", Dependency),
            ])
        );
    }

    #[test]
    fn a_node_included_for_several_reasons_gets_the_greatest_detail() {
        // REQ-002, nested under REQ-001 without refining it, depends on its
        // container to bring in its acceptance criteria.
        let document = spec(
            json!([requirement(
                1,
                json!({ "children": [requirement(2, json!({ "dependsOn": ["REQ-001"] }))] })
            )]),
            json!([]),
        );
        let (two, _) = summary(&members_of(&document, 2));
        assert_eq!(
            two,
            nodes(&[
                ("SEC-001", Body),
                ("REQ-001", Dependency),
                ("REQ-002", Full)
            ])
        );
    }

    #[test]
    fn terms_come_from_everything_included() {
        // The root uses TERM-001, an ancestor TERM-002, the requirement
        // TERM-003, its dependency TERM-004, and an excluded sibling
        // TERM-005.
        let document = spec(
            json!([
                section(
                    2,
                    json!({
                        "usesTerm": ["TERM-002"],
                        "children": [
                            requirement(1, json!({
                                "usesTerm": ["TERM-003"], "dependsOn": ["REQ-002"]
                            })),
                            requirement(3, json!({ "usesTerm": ["TERM-005"] })),
                        ],
                    })
                ),
                requirement(2, json!({ "usesTerm": ["TERM-004"] })),
            ]),
            json!([term(1), term(2), term(3), term(4), term(5)]),
        );
        let mut value = serde_json::to_value(&document).unwrap();
        value["root"]["usesTerm"] = json!(["TERM-001"]);
        let document: SlcrRequirementsDocument = serde_json::from_value(value).unwrap();
        let (_, terms) = summary(&members_of(&document, 1));
        assert_eq!(terms, ["TERM-001", "TERM-002", "TERM-003", "TERM-004"]);
    }

    #[test]
    fn closures_with_exponentially_many_paths_are_reached_quickly() {
        // Forty layers of two requirements, each depending on both in the
        // layer below: 2^40 paths from top to bottom.
        const LAYERS: u32 = 40;
        let mut children = Vec::new();
        for layer in 0..LAYERS {
            for side in 0..2 {
                let id = layer * 2 + side + 1;
                let mut fields = json!({});
                if layer + 1 < LAYERS {
                    let below = (layer + 1) * 2 + 1;
                    fields["dependsOn"] =
                        json!([format!("REQ-{below:03}"), format!("REQ-{:03}", below + 1)]);
                }
                children.push(requirement(id, fields));
            }
        }
        let document = spec(json!(children), json!([]));
        let (one, _) = summary(&members_of(&document, 1));
        // The root, REQ-001 itself, and every requirement below it.
        assert_eq!(one.len(), 1 + 1 + (LAYERS as usize - 1) * 2);
    }

    #[test]
    fn every_hop_parses() {
        for text in [
            hop::REFINERS,
            hop::REFINED,
            hop::DEPENDENCIES,
            hop::PARENTS,
            hop::TERMS,
        ] {
            parse_query(text).unwrap();
        }
    }
}
