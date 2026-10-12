//! Loads a [SlcrRequirementsDocument] into a property graph that `slcr-gql`
//! can query.
//!
//! The mapping is the one `slcr-gql` documents in its `PROFILE.md` ("The
//! SLCR graph") and implements in `slcr_gql::load_spec`, which reads only
//! JSON. Materializing slcr's own vetted document instead reads every
//! format slcr does, through one schema. The tests hold the two to the same
//! graph, element for element, in the same insertion order: a query without
//! `ORDER BY` returns rows in insertion order, so order is part of the
//! contract.
//!
//! | Document                        | Graph                                                        |
//! |---------------------------------|--------------------------------------------------------------|
//! | Section (and the Glossary)      | `:Section {id, title, body?}`                                |
//! | Requirement                     | `:Requirement {id, title, body, modality, refinement?, rationale?}` |
//! | Acceptance Criterion            | `:AcceptanceCriterion {id, title, body}`                     |
//! | Term                            | `:Term {id, title, definition, aliases?}`                     |
//! | `children` / `terms`            | `(parent)-[:CONTAINS {position}]->(child)`                   |
//! | the Glossary                    | the root's last `CONTAINS` child                             |
//! | `acceptanceCriteria`            | `(requirement)-[:HAS_CRITERION {position}]->(criterion)`      |
//! | `refines`                       | `(requirement)-[:REFINES]->(refined)`                        |
//! | `dependsOn`                     | `(requirement)-[:DEPENDS_ON]->(dependency)`                  |
//! | `usesTerm`                      | `(node)-[:USES_TERM]->(term)`                                |
//!
//! Every node also has `spec`, the specification's name.

use std::collections::BTreeMap;
use std::fmt::Display;

use slcr_gql::store::NodeId;
use slcr_gql::{GraphStoreMut, SpecIndex, StoreError, Value};

use crate::spec::graph::SlcrRequirementsDocument;
use crate::spec::node::{Requirement, Section, SectionChild};

/// Node labels.
pub mod label {
    pub const SECTION: &str = "Section";
    pub const REQUIREMENT: &str = "Requirement";
    pub const CRITERION: &str = "AcceptanceCriterion";
    pub const TERM: &str = "Term";
}

/// Edge labels.
pub mod edge {
    pub const CONTAINS: &str = "CONTAINS";
    pub const HAS_CRITERION: &str = "HAS_CRITERION";
    pub const REFINES: &str = "REFINES";
    pub const DEPENDS_ON: &str = "DEPENDS_ON";
    pub const USES_TERM: &str = "USES_TERM";
}

/// The property every node's ID is stored in.
pub const ID: &str = "id";

type Properties = BTreeMap<String, Value>;
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Load `document` into `store`, returning each node's ID in the store by
/// its SLCR ID.
///
/// A vetted document has unique IDs and no dangling references, so only
/// the store can fail.
pub async fn materialize(
    document: &SlcrRequirementsDocument,
    store: &mut dyn GraphStoreMut,
) -> Result<SpecIndex, StoreError> {
    let mut graph = Materializer {
        store,
        spec: document.spec().name().to_string(),
        ids: SpecIndex::new(),
        references: Vec::new(),
    };
    let root = graph.section(document.root()).await?;

    let glossary = document.glossary();
    let properties = titled(
        glossary.title(),
        [("body", glossary.body().as_ref().map(text))],
    );
    let node = graph
        .node(label::SECTION, glossary.id(), properties)
        .await?;
    let position = document.root().children().len();
    graph.contain(root, node, edge::CONTAINS, position).await?;
    for (position, term) in glossary.terms().iter().enumerate() {
        let aliases = (!term.aliases().is_empty())
            .then(|| Value::List(term.aliases().iter().map(text).collect()));
        let properties = titled(
            term.title(),
            [
                ("definition", Some(text(term.definition()))),
                ("aliases", aliases),
            ],
        );
        let child = graph.node(label::TERM, term.id(), properties).await?;
        graph.contain(node, child, edge::CONTAINS, position).await?;
    }

    // Every node exists now, so every reference resolves.
    for (from, label, to) in std::mem::take(&mut graph.references) {
        let (source, target) = (graph.ids[&from], graph.ids[&to]);
        graph
            .store
            .add_edge(source, target, vec![label.to_owned()], Properties::new())
            .await?;
    }
    Ok(graph.ids)
}

struct Materializer<'a> {
    store: &'a mut dyn GraphStoreMut,
    spec: String,
    ids: SpecIndex,
    /// Cross edges, as (source ID, label, target ID), added once every node
    /// exists.
    references: Vec<(String, &'static str, String)>,
}

impl Materializer<'_> {
    /// Add a node labeled `label` with `properties`, plus its ID and the
    /// specification's name.
    async fn node(
        &mut self,
        label: &str,
        id: &impl Display,
        mut properties: Properties,
    ) -> Result<NodeId, StoreError> {
        let id = id.to_string();
        properties.insert(ID.to_owned(), Value::String(id.clone()));
        properties.insert("spec".to_owned(), Value::String(self.spec.clone()));
        let node = self
            .store
            .add_node(vec![label.to_owned()], properties)
            .await?;
        self.ids.insert(id, node);
        Ok(node)
    }

    /// Add the edge from `parent` to the child at `position` among its
    /// siblings.
    async fn contain(
        &mut self,
        parent: NodeId,
        child: NodeId,
        label: &str,
        position: usize,
    ) -> Result<(), StoreError> {
        let position = i64::try_from(position).expect("a sibling position fits in an i64");
        let properties = Properties::from([("position".to_owned(), Value::Int(position))]);
        self.store
            .add_edge(parent, child, vec![label.to_owned()], properties)
            .await?;
        Ok(())
    }

    /// Defer an edge labeled `label` from `from` to each of `to`.
    fn refer(&mut self, from: &impl Display, label: &'static str, to: &[impl Display]) {
        for to in to {
            self.references
                .push((from.to_string(), label, to.to_string()));
        }
    }

    // Recursive async functions return boxed futures.
    fn section<'s>(
        &'s mut self,
        section: &'s Section,
    ) -> BoxFuture<'s, Result<NodeId, StoreError>> {
        Box::pin(async move {
            let properties = titled(
                section.title(),
                [("body", section.body().as_ref().map(text))],
            );
            let node = self.node(label::SECTION, section.id(), properties).await?;
            self.refer(section.id(), edge::USES_TERM, section.uses_term());
            for (position, child) in section.children().iter().enumerate() {
                let child = match child {
                    SectionChild::Section(section) => self.section(section).await?,
                    SectionChild::Requirement(requirement) => self.requirement(requirement).await?,
                };
                self.contain(node, child, edge::CONTAINS, position).await?;
            }
            Ok(node)
        })
    }

    fn requirement<'s>(
        &'s mut self,
        requirement: &'s Requirement,
    ) -> BoxFuture<'s, Result<NodeId, StoreError>> {
        Box::pin(async move {
            let properties = titled(
                requirement.title(),
                [
                    ("body", Some(text(requirement.body()))),
                    ("modality", Some(text(requirement.modality()))),
                    ("rationale", requirement.rationale().as_ref().map(text)),
                    ("refinement", requirement.refinement().as_ref().map(text)),
                ],
            );
            let node = self
                .node(label::REQUIREMENT, requirement.id(), properties)
                .await?;
            self.refer(requirement.id(), edge::REFINES, requirement.refines());
            self.refer(requirement.id(), edge::DEPENDS_ON, requirement.depends_on());
            self.refer(requirement.id(), edge::USES_TERM, requirement.uses_term());
            for (position, criterion) in requirement.acceptance_criteria().iter().enumerate() {
                let properties =
                    titled(criterion.title(), [("body", Some(text(criterion.body())))]);
                let child = self
                    .node(label::CRITERION, criterion.id(), properties)
                    .await?;
                self.contain(node, child, edge::HAS_CRITERION, position)
                    .await?;
            }
            for (position, child) in requirement.children().iter().enumerate() {
                let child = self.requirement(child).await?;
                self.contain(node, child, edge::CONTAINS, position).await?;
            }
            Ok(node)
        })
    }
}

fn text(value: impl Display) -> Value {
    Value::String(value.to_string())
}

/// A node's properties: its `title`, plus each of `optional` that is
/// present.
fn titled<const N: usize>(title: impl Display, optional: [(&str, Option<Value>); N]) -> Properties {
    let optional = optional
        .into_iter()
        .filter_map(|(key, value)| Some((key.to_owned(), value?)));
    std::iter::once(("title".to_owned(), text(title)))
        .chain(optional)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use slcr_gql::store::Incidence;
    use slcr_gql::{GraphStore, PetgraphStore, load_spec};

    use super::*;
    use crate::cli::block_on;
    use crate::spec::text::{SpecName, Title};

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");
    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");

    /// Every node, then every edge, of `store`, in insertion order, with
    /// their labels and properties: equal for two stores exactly when they
    /// hold the same graph, built in the same order.
    async fn dump(store: &dyn GraphStore) -> Vec<String> {
        let mut lines = Vec::new();
        let mut edges = Vec::new();
        for node in store.nodes().await.unwrap() {
            let labels = store.node_labels(node).await.unwrap();
            let properties = store.node_properties(node).await.unwrap();
            lines.push(format!("{node:?} {labels:?} {properties:?}"));
            edges.extend(store.edges(node, Incidence::Outgoing).await.unwrap());
        }
        edges.sort_by_key(|edge| edge.id);
        for edge in edges {
            let labels = store.edge_labels(edge.id).await.unwrap();
            let properties = store.edge_properties(edge.id).await.unwrap();
            lines.push(format!(
                "{:?} {:?}->{:?} {labels:?} {properties:?}",
                edge.id, edge.source, edge.target
            ));
        }
        lines
    }

    /// The graph `slcr_gql::load_spec` loads from `json`.
    async fn reference(json: &str) -> (Vec<String>, SpecIndex) {
        let mut store = PetgraphStore::new();
        let ids = load_spec(json, &mut store).await.unwrap();
        (dump(&store).await, ids)
    }

    /// The graph `document` materializes to.
    async fn materialized(document: &SlcrRequirementsDocument) -> (Vec<String>, SpecIndex) {
        let mut store = PetgraphStore::new();
        let ids = materialize(document, &mut store).await.unwrap();
        (dump(&store).await, ids)
    }

    /// Assert that `document`, serialized as JSON, is what `load_spec`
    /// reads into the same graph `document` materializes to.
    fn assert_matches_load_spec(document: &SlcrRequirementsDocument) {
        let json = serde_json::to_string(document).unwrap();
        block_on(async {
            assert_eq!(materialized(document).await, reference(&json).await);
        })
        .unwrap();
    }

    #[test]
    fn every_format_materializes_to_the_graph_load_spec_reads() {
        let documents: [SlcrRequirementsDocument; 3] = [
            serde_json::from_str(TODO_API_JSON).unwrap(),
            serde_saphyr::from_str(TODO_API_YAML).unwrap(),
            toml::from_str(TODO_API_TOML).unwrap(),
        ];
        block_on(async {
            let expected = reference(TODO_API_JSON).await;
            for document in &documents {
                assert_eq!(materialized(document).await, expected);
            }
        })
        .unwrap();
    }

    #[test]
    fn every_optional_field_materializes_as_load_spec_reads_it() {
        // The canon example lacks a rationale, aliases, a Section that uses a
        // Term, and a dependency on a Section.
        let mut value: serde_json::Value = serde_json::from_str(TODO_API_JSON).unwrap();
        let controllers = &mut value["root"]["children"][1];
        controllers["usesTerm"] = json!(["TERM-001"]);
        let validates = &mut controllers["children"][0]["children"][0];
        validates["rationale"] = json!("Bad payloads must not reach the model.");
        validates["dependsOn"] = json!(["SEC-004"]);
        value["glossary"]["terms"][0]["aliases"] = json!(["Todo item", "Item"]);
        let document: SlcrRequirementsDocument = serde_json::from_value(value).unwrap();
        assert!(document.warnings().is_empty());

        assert_matches_load_spec(&document);
        let (lines, _) = block_on(materialized(&document)).unwrap();
        assert!(lines.iter().any(|line| line.contains("rationale")));
        assert!(lines.iter().any(|line| line.contains("aliases")));
    }

    #[test]
    fn a_new_document_materializes_as_load_spec_reads_it() {
        let name = SpecName::try_from("empty".to_owned()).unwrap();
        let title = Title::try_from("Empty".to_owned()).unwrap();
        assert_matches_load_spec(&SlcrRequirementsDocument::new(name, title));
    }

    #[test]
    fn every_node_is_indexed_by_its_id() {
        let document: SlcrRequirementsDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        block_on(async {
            let mut store = PetgraphStore::new();
            let ids = materialize(&document, &mut store).await.unwrap();
            assert_eq!(ids.len(), store.node_count());
            for (id, node) in &ids {
                let stored = store.node_property(*node, ID).await.unwrap();
                assert_eq!(stored, Some(Value::String(id.clone())));
            }
        })
        .unwrap();
    }
}
