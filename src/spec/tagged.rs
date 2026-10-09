//! Deserializing nodes, which carry a `kind` tag, without buffering them.
//!
//! Serde checks a tag only on an internally tagged enum, and it reads one by
//! buffering the whole map before deserializing it. By then the parser has
//! moved past the map, so every error inside it would be reported at the
//! map: since every node is nested in the root, at the root.
//!
//! Instead, [read_kind] reads a node's map up to its `kind`, which is checked
//! first, and [Fields] streams the rest to the node's derived deserializer as
//! the parser reads it, so an error is reported where it is.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{
    self, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, Visitor,
    value::MapAccessDeserializer,
};

/// The key of a node's tag.
const KIND: &str = "kind";

/// A node type, whose serialized form is tagged with its `kind`.
pub(super) trait Node: Sized {
    /// The node's `kind`.
    const KIND: &'static str;

    /// Deserialize the node from a map that yields every field but `kind`,
    /// with the node's derived deserializer.
    fn deserialize_fields<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
}

/// Deserialize a `T`, which must be tagged with its kind.
pub(super) fn deserialize<'de, T: Node, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    struct NodeVisitor<T>(PhantomData<T>);

    impl<'de, T: Node> Visitor<'de> for NodeVisitor<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a {} node", T::KIND)
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            let (_, fields) = read_kind(map, const { &[T::KIND] })?;
            T::deserialize_fields(MapAccessDeserializer::new(fields))
        }
    }

    deserializer.deserialize_map(NodeVisitor(PhantomData))
}

/// Read `map` up to its `kind`, which must be one of `kinds`, and return
/// the kind and the node's other fields.
///
/// The kind is checked before any other field, so a node of the wrong kind
/// is reported as one whatever order its fields are in. Canonical form
/// writes `kind` first, so nothing is buffered and every field keeps its
/// location; entries before `kind` are buffered and replayed, so an error
/// within one of them can't be located.
pub(super) fn read_kind<'de, A: MapAccess<'de>>(
    mut map: A,
    kinds: &'static [&'static str],
) -> Result<(&'static str, Fields<A>), A::Error> {
    let mut buffered = Vec::new();
    while let Some(key) = map.next_key::<String>()? {
        if key == KIND {
            let kind = map.next_value_seed(Kind(kinds))?;
            let fields = Fields {
                map,
                buffered: buffered.into_iter(),
                value: None,
            };
            return Ok((kind, fields));
        }
        let value: serde_json::Value = map.next_value()?;
        buffered.push((key, value));
    }
    Err(de::Error::missing_field(KIND))
}

/// A node's map without its `kind`, which has been read: any entries
/// buffered before the kind, then the rest of the map as the parser reads
/// it. A second `kind` is an error.
pub(super) struct Fields<A> {
    map: A,
    buffered: std::vec::IntoIter<(String, serde_json::Value)>,
    /// The value of the buffered entry whose key was just read.
    value: Option<serde_json::Value>,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for Fields<A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        if let Some((key, value)) = self.buffered.next() {
            self.value = Some(value);
            return seed.deserialize(key.into_deserializer()).map(Some);
        }
        match self.map.next_key::<String>()? {
            Some(key) if key == KIND => Err(de::Error::duplicate_field(KIND)),
            Some(key) => seed.deserialize(key.into_deserializer()).map(Some),
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, A::Error> {
        match self.value.take() {
            Some(value) => seed.deserialize(value).map_err(de::Error::custom),
            None => self.map.next_value_seed(seed),
        }
    }
}

/// Reads a `kind`, which must be one of the given kinds. Checking it while
/// it is read locates an unexpected kind where it is written.
struct Kind(&'static [&'static str]);

impl<'de> DeserializeSeed<'de> for Kind {
    type Value = &'static str;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Kind {
    type Value = &'static str;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a node kind")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.0
            .iter()
            .find(|kind| **kind == value)
            .copied()
            .ok_or_else(|| E::unknown_variant(value, self.0))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use crate::fs::format::{Format, ParseError};
    use crate::spec::graph::SlcrRequirementsDocument;
    use crate::spec::node::Requirement;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");
    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");

    fn canon() -> SlcrRequirementsDocument {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    fn parse(format: Format, source: &str) -> Result<SlcrRequirementsDocument, ParseError> {
        format.parse(Path::new("spec"), source.to_owned())
    }

    /// The 1-based line of `source` that holds byte `offset`.
    fn line_of(source: &str, offset: usize) -> usize {
        source[..offset].matches('\n').count() + 1
    }

    /// `source` with the first `from` replaced by `to`, and the line `to`
    /// is on.
    fn broken(source: &str, from: &str, to: &str) -> (String, usize) {
        let offset = source.find(from).expect("the text to replace");
        (source.replacen(from, to, 1), line_of(source, offset))
    }

    #[test]
    fn errors_deep_in_the_tree_are_located_where_they_are_in_every_format() {
        // REQ-007 is nested three deep: root, then Models, then itself.
        for (format, source, from, to) in [
            (Format::Yaml, TODO_API_YAML, "id: REQ-007", "id: REQ-7"),
            (
                Format::Json,
                TODO_API_JSON,
                r#""id": "REQ-007""#,
                r#""id": "REQ-7""#,
            ),
            (
                Format::Toml,
                TODO_API_TOML,
                r#"id = "REQ-007""#,
                r#"id = "REQ-7""#,
            ),
        ] {
            let (source, line) = broken(source, from, to);
            let err = parse(format, &source).unwrap_err();
            assert_eq!(
                err.message(),
                "`REQ-7` is not a valid requirement ID",
                "{format}"
            );
            let span = err.span().expect("a location");
            assert_eq!(line_of(&source, span.offset()), line, "{format}");
        }
    }

    #[test]
    fn a_wrong_value_deep_in_the_tree_is_located_in_yaml() {
        let (source, line) = broken(TODO_API_YAML, "modality: MAY", "modality: may");
        let err = parse(Format::Yaml, &source).unwrap_err();
        let span = err.span().expect("a location");
        assert_eq!(line_of(&source, span.offset()), line);
        assert_eq!(&source[span.offset()..span.offset() + span.len()], "may");
    }

    #[test]
    fn an_unknown_field_in_a_node_is_located() {
        let (source, line) = broken(
            TODO_API_YAML,
            "      title: Controllers\n",
            "      title: Controllers\n      order: 2\n",
        );
        let err = parse(Format::Yaml, &source).unwrap_err();
        assert!(
            err.message().starts_with("unknown field `order`"),
            "{err:?}"
        );
        // The field follows the line that was replaced.
        assert_eq!(line_of(&source, err.span().unwrap().offset()), line + 1);
    }

    #[test]
    fn a_wrong_kind_is_located_at_the_kind() {
        let (source, line) = broken(TODO_API_YAML, "- kind: requirement", "- kind: term");
        let err = parse(Format::Yaml, &source).unwrap_err();
        assert!(
            err.message().starts_with("unknown variant `term`"),
            "{err:?}"
        );
        // serde-saphyr labels the entry's key, `kind`, on the same line.
        let span = err.span().expect("a location");
        assert_eq!(line_of(&source, span.offset()), line);
    }

    #[test]
    fn the_kind_is_checked_before_any_other_field() {
        // `id` comes first and would fail as a requirement ID, but the
        // wrong kind is what's reported.
        let source = r#"{ "id": "SEC-001", "title": "Nested", "kind": "section" }"#;
        let err = serde_json::from_str::<Requirement>(source).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("unknown variant `section`, expected `requirement`"),
            "{err}"
        );
    }

    #[test]
    fn a_kind_after_other_fields_is_still_read() {
        // `serde_json::Value` sorts keys, so every node's `kind` comes after
        // `body`, `children`, and `id`, and every child's fields before its
        // kind are buffered and replayed.
        let value: Value = serde_json::from_str(TODO_API_JSON).unwrap();
        let sorted = serde_json::to_string(&value).unwrap();
        assert!(sorted.contains(r#""id":"SEC-001","kind":"section""#));
        let graph: SlcrRequirementsDocument = serde_json::from_str(&sorted).unwrap();
        assert_eq!(graph, canon());
    }

    #[test]
    fn errors_in_fields_before_a_late_kind_are_still_reported() {
        let source = r#"{ "id": "REQ-1", "kind": "requirement" }"#;
        let err = serde_json::from_str::<Requirement>(source).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("`REQ-1` is not a valid requirement ID"),
            "{err}"
        );
    }

    #[test]
    fn a_node_needs_exactly_one_kind() {
        let missing = serde_json::from_value::<Requirement>(json!({ "id": "REQ-001" }));
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("missing field `kind`")
        );

        let source = r#"{ "kind": "requirement", "id": "REQ-001", "kind": "requirement" }"#;
        let err = serde_json::from_str::<Requirement>(source).unwrap_err();
        assert!(
            err.to_string().starts_with("duplicate field `kind`"),
            "{err}"
        );
    }

    #[test]
    fn a_node_must_be_a_map() {
        let err = serde_json::from_value::<Requirement>(json!("REQ-001")).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"invalid type: string "REQ-001", expected a requirement node"#
        );
        let err = serde_json::from_str::<SlcrRequirementsDocument>(&TODO_API_JSON.replacen(
            r#""children": ["#,
            r#""children": ["SEC-009", "#,
            1,
        ))
        .unwrap_err();
        assert!(
            err.to_string().starts_with(
                "invalid type: string \"SEC-009\", expected a section or requirement node"
            ),
            "{err}"
        );
    }
}
