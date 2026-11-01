//! GQL queries over a specification, in the read-only SLCR query profile
//! that `slcr-gql` implements (see its `PROFILE.md`).
//!
//! A [SpecGraph] materializes a vetted document into an in-memory property
//! graph ([materialize]), runs queries against it, and resolves what they
//! return into [Cell]s, which name nodes by their SLCR IDs.

use std::collections::HashMap;
use std::time::Instant;

use serde::Serialize;
use slcr_gql::ast::Query;
use slcr_gql::store::NodeId;
use slcr_gql::{Error, Params, PetgraphStore, StoreError, Value};

use crate::spec::graph::SlcrRequirementsDocument;

pub use cell::Cell;
pub use error::InvalidQuery;
pub use materialize::materialize;

mod cell;
mod deadline;
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
        self.resolve(result).await
    }

    /// [SpecGraph::query], stopped with [Stopped::DeadlineReached] if it is
    /// still running at `deadline`. The engine is stopped where it is, not
    /// abandoned: it reads the graph at every step of a match, and every
    /// read after the deadline fails.
    pub async fn query_until(
        &self,
        query: &Query,
        params: &Params,
        deadline: Instant,
    ) -> Result<Rows, Stopped> {
        let bounded = deadline::Bounded::new(&self.store, deadline);
        let result = match slcr_gql::execute(&bounded, query, params).await {
            Ok(result) => result,
            Err(error) if deadline::is_deadline(&error) => return Err(Stopped::DeadlineReached),
            Err(error) => return Err(Stopped::Failed(error)),
        };
        self.resolve(result).await.map_err(Stopped::Failed)
    }

    /// Name each node and edge in `result` by its SLCR ID.
    async fn resolve(&self, result: slcr_gql::QueryResult) -> Result<Rows, Error> {
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

/// Why [SpecGraph::query_until] returned no rows.
#[derive(Debug, PartialEq)]
pub enum Stopped {
    /// The query was still running at its deadline.
    DeadlineReached,
    /// The query can't run, or failed while running.
    Failed(Error),
}

/// What a query returned: its columns, and its rows in order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

impl Rows {
    /// The rows as the one JSON document `slcr query --format json` writes:
    ///
    /// ```json
    /// {
    ///   "columns": ["r", "e"],
    ///   "rows": [
    ///     ["REQ-004", {"label": "REFINES", "source": "REQ-004", "target": "REQ-002"}]
    ///   ]
    /// }
    /// ```
    ///
    /// Nodes are their IDs, edges are objects naming their label and
    /// endpoints, and a float that isn't finite is `null`. It ends in a
    /// newline.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("query results serialize to JSON");
        json.push('\n');
        json
    }
}

/// The query value of `json`, as `--param NAME=JSON` binds it: an integer
/// that fits in an `i64` is an integer, any other number a float, an array
/// a list, and an object a map.
pub fn value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(value),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(value) => Value::Int(value),
            None => Value::Float(number.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(value) => Value::String(value),
        serde_json::Value::Array(values) => Value::List(values.into_iter().map(value).collect()),
        serde_json::Value::Object(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(key, json)| (key, value(json)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

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

    /// A specification whose requirements form a chain of `depth`
    /// diamonds: REQ-001 is refined by two requirements, which the top of
    /// the next diamond both refine, and so on, so the top of the last
    /// diamond has `2^(depth - 1)` paths of `refines` edges to REQ-001.
    fn diamonds(depth: u32) -> SlcrRequirementsDocument {
        let requirement = |id: u32, refines: Vec<u32>, refined: bool| {
            let mut node = json!({
                "kind": "requirement",
                "id": format!("REQ-{id:03}"),
                "title": format!("Requirement {id}"),
                "body": "It MUST hold.",
                "modality": "MUST",
            });
            if refined {
                node["refinement"] = json!("AND");
            }
            if !refines.is_empty() {
                let refines: Vec<_> = refines.iter().map(|id| format!("REQ-{id:03}")).collect();
                node["refines"] = json!(refines);
            }
            node
        };
        // The top of diamond `i` is REQ-(3i+1); its sides are the next two.
        let mut children = Vec::new();
        for i in 0..depth {
            let top = 3 * i + 1;
            let refines = if i == 0 {
                vec![]
            } else {
                vec![top - 2, top - 1]
            };
            children.push(requirement(top, refines, true));
        }
        for i in 0..depth {
            let top = 3 * i + 1;
            let refined = i + 1 < depth;
            children.push(requirement(top + 1, vec![top], refined));
            children.push(requirement(top + 2, vec![top], refined));
        }
        let document = json!({
            "formatVersion": "1",
            "spec": { "name": "diamonds" },
            "root": {
                "kind": "section",
                "id": "SEC-001",
                "title": "Diamonds",
                "children": children,
            },
            "glossary": { "kind": "section", "id": "SEC-002", "title": "Glossary" },
        });
        serde_json::from_value(document).unwrap()
    }

    #[test]
    fn a_query_that_ends_before_its_deadline_returns_its_rows() {
        let document: SlcrRequirementsDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        let text = "MATCH (r:Requirement) RETURN r.id ORDER BY r.id";
        let deadline = Instant::now() + Duration::from_secs(60);
        let (bounded, unbounded) = block_on(async {
            let graph = SpecGraph::new(&document).await.unwrap();
            let query = slcr_gql::parse_query(text).unwrap();
            let params = Params::new();
            (
                graph.query_until(&query, &params, deadline).await,
                graph.query(&query, &params).await,
            )
        })
        .unwrap();
        assert_eq!(bounded.unwrap(), unbounded.unwrap());
    }

    #[test]
    fn a_query_still_running_at_its_deadline_is_stopped() {
        let document = diamonds(40);
        let text = "MATCH (:Requirement {id: 'REQ-118'})-[:REFINES]->{1,}(r) RETURN count(r)";
        let started = Instant::now();
        let result = block_on(async {
            let graph = SpecGraph::new(&document).await.unwrap();
            let query = slcr_gql::parse_query(text).unwrap();
            let deadline = Instant::now() + Duration::from_millis(100);
            graph.query_until(&query, &Params::new(), deadline).await
        })
        .unwrap();
        assert_eq!(result.unwrap_err(), Stopped::DeadlineReached);
        // 2^39 paths would take far longer than this to enumerate.
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn errors_are_not_mistaken_for_the_deadline() {
        let document: SlcrRequirementsDocument = serde_json::from_str(TODO_API_JSON).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let result = block_on(async {
            let graph = SpecGraph::new(&document).await.unwrap();
            let query = slcr_gql::parse_query("MATCH (r) RETURN x").unwrap();
            graph.query_until(&query, &Params::new(), deadline).await
        })
        .unwrap();
        assert_eq!(
            result.unwrap_err(),
            Stopped::Failed(Error::Semantic("undefined variable `x`".to_owned()))
        );
    }

    #[test]
    fn rows_serialize_as_the_cli_writes_json() {
        let rows = query(
            "MATCH (:Requirement {id: 'REQ-004'})-[e:REFINES]->(r) RETURN r, e ORDER BY r.id LIMIT 1",
            &Params::new(),
        )
        .unwrap();
        assert_eq!(
            rows.to_json(),
            r#"{
  "columns": [
    "r",
    "e"
  ],
  "rows": [
    [
      "REQ-002",
      {
        "label": "REFINES",
        "source": "REQ-004",
        "target": "REQ-002"
      }
    ]
  ]
}
"#
        );
    }

    #[test]
    fn json_values_bind_as_query_values() {
        let json = json!({"n": 1, "big": 1e300, "list": [true, null, "s"]});
        let Value::Map(map) = value(json) else {
            panic!("an object is a map");
        };
        assert_eq!(map["n"], Value::Int(1));
        assert_eq!(map["big"], Value::Float(1e300));
        assert_eq!(
            map["list"],
            Value::List(vec![
                Value::Bool(true),
                Value::Null,
                Value::String("s".to_owned())
            ])
        );
    }

    #[test]
    fn errors_are_the_engine_s() {
        let err = query("MATCH (r) RETURN x", &Params::new()).unwrap_err();
        assert_eq!(err, Error::Semantic("undefined variable `x`".to_owned()));
    }
}
