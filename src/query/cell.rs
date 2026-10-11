use std::collections::BTreeMap;
use std::fmt::{self, Display};

use serde::{Serialize, Serializer};
use slcr_gql::store::NodeId;
use slcr_gql::{Error, GraphStore, StoreError, StoreErrorKind, Value};

use super::SpecGraph;

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A value a query returned, with every node and edge in it resolved to
/// what names it in the specification.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    List(Vec<Cell>),
    Map(BTreeMap<String, Cell>),
    /// A node, by its SLCR ID, e.g. `REQ-004`.
    Node(String),
    /// An edge, by its label and its endpoints' SLCR IDs.
    Edge {
        label: String,
        source: String,
        target: String,
    },
}

impl Cell {
    /// Resolve `value`, which `graph` returned.
    pub(super) fn resolve(value: Value, graph: &SpecGraph) -> BoxFuture<'_, Result<Self, Error>> {
        Box::pin(async move {
            Ok(match value {
                Value::Null => Self::Null,
                Value::Bool(value) => Self::Bool(value),
                Value::Int(value) => Self::Int(value),
                Value::Float(value) => Self::Float(value),
                Value::String(value) => Self::String(value),
                Value::List(values) => {
                    let mut cells = Vec::with_capacity(values.len());
                    for value in values {
                        cells.push(Self::resolve(value, graph).await?);
                    }
                    Self::List(cells)
                }
                Value::Map(entries) => {
                    let mut cells = BTreeMap::new();
                    for (key, value) in entries {
                        cells.insert(key, Self::resolve(value, graph).await?);
                    }
                    Self::Map(cells)
                }
                Value::Node(node) => Self::Node(id(graph, node)?),
                Value::Edge(edge) => {
                    let endpoints = graph.store.edge(edge).await?;
                    // A materialized edge has exactly one label; `&` is how
                    // GQL would spell a conjunction of several.
                    let label = graph.store.edge_labels(edge).await?.join("&");
                    Self::Edge {
                        label,
                        source: id(graph, endpoints.source)?,
                        target: id(graph, endpoints.target)?,
                    }
                }
            })
        })
    }

    /// The cell as a table shows it: a string as it is, null as nothing,
    /// and anything else as [Display] writes it.
    pub fn text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Null => String::new(),
            other => other.to_string(),
        }
    }
}

/// The SLCR ID of `node`.
fn id(graph: &SpecGraph, node: NodeId) -> Result<String, StoreError> {
    graph.ids.get(&node).cloned().ok_or_else(|| {
        let message = format!("node {} has no SLCR ID", node.0);
        StoreError::new(StoreErrorKind::NotFound, message)
    })
}

/// Literals as GQL writes them, e.g. `'a string'`, `1.0`, and `[1, 2]`;
/// nodes as their IDs, e.g. `REQ-004`; and edges as a pattern, e.g.
/// `(REQ-004)-[:REFINES]->(REQ-002)`.
impl Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{}", Value::Float(*value)),
            Self::String(value) => write!(f, "{}", Value::String(value.clone())),
            Self::List(cells) => {
                f.write_str("[")?;
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{cell}")?;
                }
                f.write_str("]")
            }
            Self::Map(cells) => {
                f.write_str("{")?;
                for (i, (key, cell)) in cells.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {cell}")?;
                }
                f.write_str("}")
            }
            Self::Node(id) => f.write_str(id),
            Self::Edge {
                label,
                source,
                target,
            } => write!(f, "({source})-[:{label}]->({target})"),
        }
    }
}

/// JSON values, with nodes as their IDs and edges as
/// `{"label": .., "source": .., "target": ..}`. A float that isn't finite
/// is `null`, which is all JSON can say.
impl Serialize for Cell {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Edge<'a> {
            label: &'a str,
            source: &'a str,
            target: &'a str,
        }

        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Float(value) if value.is_finite() => serializer.serialize_f64(*value),
            Self::Float(_) => serializer.serialize_unit(),
            Self::String(value) | Self::Node(value) => serializer.serialize_str(value),
            Self::List(cells) => cells.serialize(serializer),
            Self::Map(cells) => cells.serialize(serializer),
            Self::Edge {
                label,
                source,
                target,
            } => Edge {
                label,
                source,
                target,
            }
            .serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn edge() -> Cell {
        Cell::Edge {
            label: "REFINES".to_owned(),
            source: "REQ-004".to_owned(),
            target: "REQ-002".to_owned(),
        }
    }

    #[test]
    fn cells_display_as_gql_literals_with_nodes_and_edges_by_id() {
        let map = Cell::Map(BTreeMap::from([
            ("a".to_owned(), Cell::Int(1)),
            ("b".to_owned(), Cell::Null),
        ]));
        for (cell, text) in [
            (Cell::Null, "null"),
            (Cell::Bool(true), "true"),
            (Cell::Int(-3), "-3"),
            (Cell::Float(2.0), "2.0"),
            (Cell::Float(0.5), "0.5"),
            (Cell::String("it's".to_owned()), r"'it\'s'"),
            (Cell::Node("REQ-004".to_owned()), "REQ-004"),
            (edge(), "(REQ-004)-[:REFINES]->(REQ-002)"),
            (map, "{a: 1, b: null}"),
        ] {
            assert_eq!(cell.to_string(), text);
        }
        let list = Cell::List(vec![
            Cell::Node("REQ-001".to_owned()),
            Cell::String("REQ-001".to_owned()),
        ]);
        assert_eq!(list.to_string(), "[REQ-001, 'REQ-001']");
    }

    #[test]
    fn table_text_shows_strings_bare_and_null_as_nothing() {
        assert_eq!(Cell::String("it's".to_owned()).text(), "it's");
        assert_eq!(Cell::Null.text(), "");
        assert_eq!(Cell::Node("REQ-004".to_owned()).text(), "REQ-004");
        assert_eq!(Cell::Int(7).text(), "7");
    }

    #[test]
    fn cells_serialize_as_json_with_nodes_and_edges_by_id() {
        let cells = Cell::List(vec![
            Cell::Null,
            Cell::Bool(false),
            Cell::Int(7),
            Cell::Float(1.5),
            Cell::Float(f64::NAN),
            Cell::String("text".to_owned()),
            Cell::Node("REQ-004".to_owned()),
            edge(),
            Cell::Map(BTreeMap::from([("k".to_owned(), Cell::Int(1))])),
        ]);
        assert_eq!(
            serde_json::to_value(&cells).unwrap(),
            json!([
                null,
                false,
                7,
                1.5,
                null,
                "text",
                "REQ-004",
                {"label": "REFINES", "source": "REQ-004", "target": "REQ-002"},
                {"k": 1}
            ])
        );
    }
}
