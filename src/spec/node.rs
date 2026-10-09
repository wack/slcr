use std::fmt::{self, Display};

use derive_getters::Getters;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::id::{CriterionId, DependencyId, RequirementId, SectionId, TermId};
use super::optional;
use super::text::{Markdown, Title};

/// Every serialized node carries a `kind` discriminator, but serde only
/// checks a tag when deserializing an enum: a struct-level
/// `#[serde(tag = "kind")]` writes the tag yet ignores it on the way in.
///
/// So each node type derives its serde code with `#[serde(remote = "Self")]`,
/// which emits inherent `serialize` and `deserialize` functions in place of
/// the trait impls, keeping the struct-level tag for output. This macro then
/// implements the traits: serialization forwards to the derived code, and
/// deserialization reads the node through a single-variant, internally
/// tagged enum that checks the `kind`. [SectionChild] reuses the inherent
/// functions directly, because its own tag has already selected the node.
macro_rules! tagged {
    ($node:ident, $kind:literal) => {
        impl Serialize for $node {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $node::serialize(self, serializer)
            }
        }

        impl<'de> Deserialize<'de> for $node {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                fn untagged<'de, D>(deserializer: D) -> Result<$node, D::Error>
                where
                    D: Deserializer<'de>,
                {
                    $node::deserialize(deserializer)
                }

                #[derive(Deserialize)]
                #[serde(tag = "kind")]
                enum Tagged {
                    #[serde(rename = $kind)]
                    Node(#[serde(deserialize_with = "untagged")] $node),
                }

                let Tagged::Node(node) = Tagged::deserialize(deserializer)?;
                Ok(node)
            }
        }
    };
}

tagged!(Section, "section");
tagged!(Requirement, "requirement");
tagged!(AcceptanceCriterion, "acceptanceCriterion");
tagged!(Glossary, "section");
tagged!(Term, "term");

/// Document structure and freeform prose: preambles, background, and
/// motivation. A Section groups other nodes and has no satisfaction value.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename = "section",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub struct Section {
    id: SectionId,
    title: Title,
    #[serde(
        default,
        deserialize_with = "optional::some",
        skip_serializing_if = "Option::is_none"
    )]
    body: Option<Markdown>,
    /// Terms whose definitions this node's slice pulls in.
    #[serde(
        default,
        deserialize_with = "optional::non_empty_set",
        skip_serializing_if = "Vec::is_empty"
    )]
    uses_term: Vec<TermId>,
    /// Contained Sections and Requirements, in document order.
    #[serde(
        default,
        deserialize_with = "optional::non_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    children: Vec<SectionChild>,
}

impl Section {
    /// A Section with only an ID and a title: no body, terms, or children.
    pub(crate) fn new(id: SectionId, title: Title) -> Self {
        Self {
            id,
            title,
            body: None,
            uses_term: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// A node contained by a [Section].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SectionChild {
    Section(#[serde(deserialize_with = "Section::deserialize")] Section),
    Requirement(#[serde(deserialize_with = "Requirement::deserialize")] Requirement),
}

impl Serialize for SectionChild {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Each node writes its own `kind`.
        match self {
            Self::Section(section) => section.serialize(serializer),
            Self::Requirement(requirement) => requirement.serialize(serializer),
        }
    }
}

/// A satisfiable statement.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename = "requirement",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub struct Requirement {
    id: RequirementId,
    title: Title,
    /// The primary statement, which should contain the modality keyword.
    body: Markdown,
    #[serde(
        default,
        deserialize_with = "optional::some",
        skip_serializing_if = "Option::is_none"
    )]
    rationale: Option<Markdown>,
    modality: Modality,
    /// How the requirements that refine this one combine. Present exactly
    /// when at least one requirement refines this one.
    #[serde(
        default,
        deserialize_with = "optional::some",
        skip_serializing_if = "Option::is_none"
    )]
    refinement: Option<Refinement>,
    /// Requirements this one helps satisfy. Always explicit: nesting never
    /// implies refinement.
    #[serde(
        default,
        deserialize_with = "optional::non_empty_set",
        skip_serializing_if = "Vec::is_empty"
    )]
    refines: Vec<RequirementId>,
    /// Requirements or Sections needed to understand or implement this one.
    /// Context only; never affects satisfaction.
    #[serde(
        default,
        deserialize_with = "optional::non_empty_set",
        skip_serializing_if = "Vec::is_empty"
    )]
    depends_on: Vec<DependencyId>,
    /// Terms whose definitions this node's slice pulls in.
    #[serde(
        default,
        deserialize_with = "optional::non_empty_set",
        skip_serializing_if = "Vec::is_empty"
    )]
    uses_term: Vec<TermId>,
    /// Advisory statements of what done looks like, in order.
    #[serde(
        default,
        deserialize_with = "optional::non_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    acceptance_criteria: Vec<AcceptanceCriterion>,
    /// Contained Requirements, in document order. Containment only: a child
    /// refines this requirement only if its `refines` says so.
    #[serde(
        default,
        deserialize_with = "optional::non_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    children: Vec<Requirement>,
}

/// An RFC 2119 requirement level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Modality {
    #[serde(rename = "MUST")]
    Must,
    #[serde(rename = "MUST NOT")]
    MustNot,
    #[serde(rename = "SHOULD")]
    Should,
    #[serde(rename = "SHOULD NOT")]
    ShouldNot,
    #[serde(rename = "MAY")]
    May,
}

impl Modality {
    /// The keyword as written in prose, e.g. "MUST NOT".
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Must => "MUST",
            Self::MustNot => "MUST NOT",
            Self::Should => "SHOULD",
            Self::ShouldNot => "SHOULD NOT",
            Self::May => "MAY",
        }
    }
}

impl Display for Modality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// How the requirements that refine a requirement combine to satisfy it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Refinement {
    And,
    Or,
    Xor,
}

/// A speculative, advisory statement of what done looks like for its
/// containing requirement. It asserts nothing.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename = "acceptanceCriterion",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub struct AcceptanceCriterion {
    id: CriterionId,
    title: Title,
    /// May contain a formal sketch in a code block; the engine never reads it.
    body: Markdown,
}

/// The auto-created Glossary Section. It contains every Term and nothing
/// else, and is logically the root's last child.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename = "section",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub struct Glossary {
    id: SectionId,
    title: Title,
    #[serde(
        default,
        deserialize_with = "optional::some",
        skip_serializing_if = "Option::is_none"
    )]
    body: Option<Markdown>,
    /// Serialized in ID order; rendered alphabetically by title.
    #[serde(
        default,
        deserialize_with = "optional::non_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    terms: Vec<Term>,
}

impl Glossary {
    /// The title every specification's Glossary is created with.
    pub const TITLE: &'static str = "Glossary";

    /// An empty Glossary, titled [Glossary::TITLE].
    pub(crate) fn new(id: SectionId) -> Self {
        Self {
            id,
            title: Title::try_from(Self::TITLE.to_owned()).expect("the Glossary's title is valid"),
            body: None,
            terms: Vec::new(),
        }
    }
}

/// A glossary entry.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename = "term",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub struct Term {
    id: TermId,
    title: Title,
    #[serde(
        default,
        deserialize_with = "optional::non_empty_set",
        skip_serializing_if = "Vec::is_empty"
    )]
    aliases: Vec<Title>,
    definition: Markdown,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn requirement(value: serde_json::Value) -> Result<Requirement, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn section(value: serde_json::Value) -> Result<Section, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn minimal_requirement() -> serde_json::Value {
        json!({
            "kind": "requirement",
            "id": "REQ-001",
            "title": "Endpoint exists",
            "body": "There MUST be an endpoint.",
            "modality": "MUST"
        })
    }

    #[test]
    fn a_minimal_requirement_deserializes() {
        let parsed = requirement(minimal_requirement()).unwrap();
        assert_eq!(*parsed.id(), RequirementId::new(1));
        assert_eq!(parsed.title().as_str(), "Endpoint exists");
        assert_eq!(*parsed.modality(), Modality::Must);
        assert!(parsed.refinement().is_none());
        assert!(parsed.refines().is_empty());
        assert!(parsed.children().is_empty());
    }

    #[test]
    fn a_full_requirement_deserializes() {
        let parsed = requirement(json!({
            "kind": "requirement",
            "id": "REQ-005",
            "title": "Controller",
            "body": "There MUST be a controller.",
            "rationale": "Requests need handling.",
            "modality": "MUST",
            "refinement": "XOR",
            "refines": ["REQ-001"],
            "dependsOn": ["REQ-002", "SEC-003"],
            "usesTerm": ["TERM-001"],
            "acceptanceCriteria": [{
                "kind": "acceptanceCriterion",
                "id": "AC-001",
                "title": "Controller is declared",
                "body": "A controller class exists."
            }],
            "children": [{
                "kind": "requirement",
                "id": "REQ-006",
                "title": "Validates input",
                "body": "It MAY validate input.",
                "modality": "MAY"
            }]
        }))
        .unwrap();
        assert_eq!(
            parsed.rationale().as_ref().unwrap().as_str(),
            "Requests need handling."
        );
        assert_eq!(*parsed.refinement(), Some(Refinement::Xor));
        assert_eq!(*parsed.refines(), [RequirementId::new(1)]);
        assert_eq!(
            *parsed.depends_on(),
            [
                DependencyId::Requirement(RequirementId::new(2)),
                DependencyId::Section(SectionId::new(3)),
            ]
        );
        assert_eq!(*parsed.uses_term(), [TermId::new(1)]);
        assert_eq!(*parsed.acceptance_criteria()[0].id(), CriterionId::new(1));
        assert_eq!(*parsed.children()[0].modality(), Modality::May);
    }

    #[test]
    fn every_modality_deserializes() {
        for (spelling, modality) in [
            ("MUST", Modality::Must),
            ("MUST NOT", Modality::MustNot),
            ("SHOULD", Modality::Should),
            ("SHOULD NOT", Modality::ShouldNot),
            ("MAY", Modality::May),
        ] {
            let parsed: Modality = serde_json::from_value(json!(spelling)).unwrap();
            assert_eq!(parsed, modality);
            assert_eq!(parsed.to_string(), spelling);
        }
    }

    #[test]
    fn unknown_modalities_are_rejected() {
        for spelling in ["must", "MUSTNOT", "MUST_NOT", "REQUIRED", "OPTIONAL"] {
            assert!(serde_json::from_value::<Modality>(json!(spelling)).is_err());
        }
    }

    #[test]
    fn every_refinement_deserializes() {
        for (spelling, refinement) in [
            ("AND", Refinement::And),
            ("OR", Refinement::Or),
            ("XOR", Refinement::Xor),
        ] {
            let parsed: Refinement = serde_json::from_value(json!(spelling)).unwrap();
            assert_eq!(parsed, refinement);
        }
        assert!(serde_json::from_value::<Refinement>(json!("and")).is_err());
        assert!(serde_json::from_value::<Refinement>(json!("NAND")).is_err());
    }

    #[test]
    fn a_missing_kind_is_rejected() {
        let mut value = minimal_requirement();
        value.as_object_mut().unwrap().remove("kind");
        let err = requirement(value).unwrap_err();
        assert!(err.to_string().contains("missing field `kind`"), "{err}");
    }

    #[test]
    fn a_mismatched_kind_is_rejected() {
        let mut value = minimal_requirement();
        value["kind"] = json!("section");
        let err = requirement(value).unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `section`"),
            "{err}"
        );
    }

    #[test]
    fn a_requirement_cannot_contain_a_section() {
        let mut value = minimal_requirement();
        value["children"] = json!([{ "kind": "section", "id": "SEC-002", "title": "Nested" }]);
        let err = requirement(value).unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `section`"),
            "{err}"
        );
    }

    #[test]
    fn a_section_cannot_contain_a_term() {
        let err = section(json!({
            "kind": "section",
            "id": "SEC-001",
            "title": "Root",
            "children": [{
                "kind": "term",
                "id": "TERM-001",
                "title": "Thing",
                "definition": "A thing."
            }]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown variant `term`"), "{err}");
    }

    #[test]
    fn acceptance_criteria_must_be_tagged() {
        let mut value = minimal_requirement();
        value["acceptanceCriteria"] =
            json!([{ "kind": "requirement", "id": "AC-001", "title": "Done", "body": "Done." }]);
        assert!(requirement(value).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = minimal_requirement();
        value["order"] = json!(1);
        let err = requirement(value).unwrap_err();
        assert!(err.to_string().contains("unknown field `order`"), "{err}");
    }

    #[test]
    fn unknown_fields_in_nested_nodes_are_rejected() {
        let err = section(json!({
            "kind": "section",
            "id": "SEC-001",
            "title": "Root",
            "children": [{ "kind": "section", "id": "SEC-002", "title": "Child", "status": "met" }]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown field `status`"), "{err}");
    }

    #[test]
    fn required_fields_are_enforced() {
        for field in ["id", "title", "body", "modality"] {
            let mut value = minimal_requirement();
            value.as_object_mut().unwrap().remove(field);
            let err = requirement(value).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("missing field `{field}`")),
                "{err}"
            );
        }
    }

    #[test]
    fn ids_must_match_the_node_kind() {
        let mut value = minimal_requirement();
        value["id"] = json!("SEC-001");
        assert!(requirement(value).is_err());
    }

    #[test]
    fn empty_optional_arrays_are_rejected() {
        for field in [
            "refines",
            "dependsOn",
            "usesTerm",
            "acceptanceCriteria",
            "children",
        ] {
            let mut value = minimal_requirement();
            value[field] = json!([]);
            assert!(requirement(value).is_err(), "{field}");
        }
    }

    #[test]
    fn null_optional_fields_are_rejected() {
        // Canonical form omits an empty optional field instead.
        for field in ["rationale", "refinement", "refines", "children"] {
            let mut value = minimal_requirement();
            value[field] = json!(null);
            assert!(requirement(value).is_err(), "{field}");
        }
    }

    #[test]
    fn children_keep_document_order() {
        let parsed = section(json!({
            "kind": "section",
            "id": "SEC-001",
            "title": "Root",
            "children": [
                { "kind": "section", "id": "SEC-003", "title": "Second" },
                minimal_requirement(),
                { "kind": "section", "id": "SEC-002", "title": "Third" }
            ]
        }))
        .unwrap();
        let kinds: Vec<&str> = parsed
            .children()
            .iter()
            .map(|child| match child {
                SectionChild::Section(section) => section.title().as_str(),
                SectionChild::Requirement(requirement) => requirement.title().as_str(),
            })
            .collect();
        assert_eq!(kinds, ["Second", "Endpoint exists", "Third"]);
    }

    #[test]
    fn serialization_writes_kinds_and_omits_empty_fields() {
        let value = json!({
            "kind": "section",
            "id": "SEC-001",
            "title": "Root",
            "children": [
                { "kind": "section", "id": "SEC-002", "title": "Child" },
                minimal_requirement()
            ]
        });
        let parsed = section(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    }

    #[test]
    fn terms_and_glossaries_deserialize() {
        let glossary: Glossary = serde_json::from_value(json!({
            "kind": "section",
            "id": "SEC-009",
            "title": "Glossary",
            "terms": [{
                "kind": "term",
                "id": "TERM-001",
                "title": "Slice",
                "aliases": ["Agent slice", "Context slice"],
                "definition": "A requirement plus its context."
            }]
        }))
        .unwrap();
        let term = &glossary.terms()[0];
        assert_eq!(term.aliases().len(), 2);
        assert_eq!(
            term.definition().as_str(),
            "A requirement plus its context."
        );
    }

    #[test]
    fn repeated_aliases_are_rejected() {
        let result = serde_json::from_value::<Term>(json!({
            "kind": "term",
            "id": "TERM-001",
            "title": "Slice",
            "aliases": ["Agent slice", "Agent slice"],
            "definition": "A requirement plus its context."
        }));
        assert!(result.is_err());
    }

    #[test]
    fn a_new_section_has_only_an_id_and_a_title() {
        let title = Title::try_from("Root".to_owned()).unwrap();
        let section = Section::new(SectionId::new(7), title.clone());
        assert_eq!(*section.id(), SectionId::new(7));
        assert_eq!(*section.title(), title);
        assert!(section.body().is_none());
        assert!(section.uses_term().is_empty());
        assert!(section.children().is_empty());
        assert_eq!(
            serde_json::to_value(&section).unwrap(),
            json!({ "kind": "section", "id": "SEC-007", "title": "Root" })
        );
    }

    #[test]
    fn a_new_glossary_is_empty_and_titled_glossary() {
        let glossary = Glossary::new(SectionId::new(2));
        assert_eq!(*glossary.id(), SectionId::new(2));
        assert_eq!(glossary.title().as_str(), Glossary::TITLE);
        assert_eq!(Glossary::TITLE, "Glossary");
        assert!(glossary.body().is_none());
        assert!(glossary.terms().is_empty());
        assert_eq!(
            serde_json::to_value(&glossary).unwrap(),
            json!({ "kind": "section", "id": "SEC-002", "title": "Glossary" })
        );
    }

    #[test]
    fn a_glossary_may_not_contain_children() {
        let result = serde_json::from_value::<Glossary>(json!({
            "kind": "section",
            "id": "SEC-009",
            "title": "Glossary",
            "children": [minimal_requirement()]
        }));
        assert!(result.is_err());
    }
}
