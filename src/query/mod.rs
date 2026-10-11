//! GQL queries over a specification, in the read-only SLCR query profile
//! that `slcr-gql` implements (see its `PROFILE.md`).
//!
//! A [SpecGraph] materializes a checked document into an in-memory property
//! graph ([materialize]), runs queries against it, and resolves what they
//! return into [Cell]s, which name nodes by their SLCR IDs.

use std::collections::HashMap;

use slcr_gql::ast::Query;
use slcr_gql::store::NodeId;
use slcr_gql::{Error, Params, PetgraphStore, StoreError};

use crate::spec::graph::SlcrRequirementsDocument;

pub use cell::Cell;
pub use error::InvalidQuery;
pub use materialize::materialize;

mod cell;
mod error;
mod materialize;

/// A specification, materialized into an in-memory graph to query.
pub struct SpecGraph {
    store: PetgraphStore,
    /// Each node's SLCR ID.
    ids: HashMap<NodeId, String>,
}

impl SpecGraph {
    pub async fn new(document: &SlcrRequirementsDocument) -> Result<Self, StoreError> {
        let mut store = PetgraphStore::new();
        let ids = materialize(document, &mut store).await?;
        let ids = ids.into_iter().map(|(id, node)| (node, id)).collect();
        Ok(Self { store, ids })
    }

    /// Check and run `query`, which [slcr_gql::parse_query] parsed, with
    /// `params` bound to its `$parameters`.
    pub async fn query(&self, query: &Query, params: &Params) -> Result<Rows, Error> {
        let result = slcr_gql::execute(&self.store, query, params).await?;
        let mut rows = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut cells = Vec::with_capacity(row.len());
            for value in row {
                cells.push(Cell::resolve(value, self).await?);
            }
            rows.push(cells);
        }
        Ok(Rows {
            columns: result.columns,
            rows,
        })
    }
}

/// What a query returned: its columns, and its rows in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

#[cfg(test)]
mod tests {
    use slcr_gql::Value;

    use super::*;
    use crate::cli::block_on;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");

    fn query(text: &str, params: &Params) -> Result<Rows, Error> {
        let document: SlcrRequirementsDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        block_on(async {
            let graph = SpecGraph::new(&document).await.unwrap();
            graph.query(&slcr_gql::parse_query(text)?, params).await
        })
        .unwrap()
    }

    fn node(id: &str) -> Cell {
        Cell::Node(id.to_owned())
    }

    #[test]
    fn queries_follow_edges_transitively() {
        let rows = query(
            "MATCH (d:Requirement)-[:REFINES]->{1,}(:Requirement {id: 'REQ-001'})
             RETURN DISTINCT d.id AS dependent ORDER BY dependent",
            &Params::new(),
        )
        .unwrap();
        assert_eq!(rows.columns, ["dependent"]);
        let ids: Vec<_> = rows.rows.into_iter().flatten().collect();
        let expected = ["REQ-002", "REQ-003", "REQ-004"];
        assert_eq!(ids, expected.map(|id| Cell::String(id.to_owned())));
    }

    #[test]
    fn nodes_are_named_by_their_ids() {
        let rows = query(
            "MATCH (r:Requirement)-[:HAS_CRITERION]->(c) RETURN r, c",
            &Params::new(),
        )
        .unwrap();
        assert_eq!(rows.rows, [[node("REQ-007"), node("AC-001")]]);
    }

    #[test]
    fn edges_are_named_by_their_label_and_endpoints() {
        let rows = query(
            "MATCH (:Requirement {id: 'REQ-004'})-[e:REFINES]->(r) RETURN e ORDER BY r.id",
            &Params::new(),
        )
        .unwrap();
        let edge = |target: &str| Cell::Edge {
            label: "REFINES".to_owned(),
            source: "REQ-004".to_owned(),
            target: target.to_owned(),
        };
        assert_eq!(rows.rows, [[edge("REQ-002")], [edge("REQ-003")]]);
    }

    #[test]
    fn nodes_inside_lists_are_resolved_too() {
        let rows = query(
            "MATCH (s:Section {id: 'SEC-002'})-[:CONTAINS]->{1,}(r)
             RETURN collect_list(r) AS contained",
            &Params::new(),
        )
        .unwrap();
        let contained = ["REQ-001", "REQ-002", "REQ-004", "REQ-003"].map(node);
        assert_eq!(rows.rows, [[Cell::List(contained.to_vec())]]);
    }

    #[test]
    fn parameters_are_bound() {
        let params = Params::from([("id".to_owned(), Value::String("REQ-009".to_owned()))]);
        let rows = query("MATCH (r {id: $id}) RETURN r.modality", &params).unwrap();
        assert_eq!(rows.rows, [[Cell::String("MAY".to_owned())]]);
    }

    #[test]
    fn errors_are_the_engine_s() {
        let err = query("MATCH (r) RETURN x", &Params::new()).unwrap_err();
        assert_eq!(err, Error::Semantic("undefined variable `x`".to_owned()));
    }
}
