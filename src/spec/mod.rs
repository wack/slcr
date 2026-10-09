//! The specification graph: SLCR's requirements document.
//!
//! The graph is defined by the specification graph JSON Schema (draft
//! 2020-12) on the SLCR Schemas page in Notion. It is serialized as YAML,
//! JSON, or TOML, all sharing the same field names and structure.

/// Invariants beyond the schema's grammar, checked on every load.
pub(crate) mod check;
/// Loading and saving a graph through [crate::fs::FileSystem].
pub(crate) mod file;
/// The top-level document.
pub(crate) mod graph;
/// Kind-prefixed node identifiers.
pub(crate) mod id;
/// The four node kinds and the Glossary.
pub(crate) mod node;
/// Deserializers for fields that canonical form omits when empty.
mod optional;
/// Deserializing tagged nodes without losing the location of errors.
mod tagged;
/// Validated strings: titles, Markdown prose, and specification names.
pub(crate) mod text;
