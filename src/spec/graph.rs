use derive_getters::Getters;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use super::check::{self, Report, Violations, Warning};
use super::node::{Glossary, Requirement, Section, SectionChild};
use super::optional;
use super::text::SpecName;

/// One SLCR specification: the requirements document, stored as a graph.
///
/// The `contains` tree is the nesting of [Section]s and [Requirement]s, in
/// sibling order. The cross edges `refines`, `dependsOn`, and `usesTerm` are
/// ID references held on their source node.
///
/// Deserializing a graph enforces the grammar of the specification graph's
/// JSON Schema and every invariant listed in its `x-checkInvariants`, other
/// than those that are only warnings; see [SlcrRequirementsDocument::warnings].
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct SlcrRequirementsDocument {
    /// The JSON Schema this document claims to conform to.
    #[serde(
        rename = "$schema",
        default,
        deserialize_with = "optional::some",
        skip_serializing_if = "Option::is_none"
    )]
    schema: Option<String>,
    /// The version of the serialization format, not of the specification's
    /// content.
    format_version: FormatVersion,
    spec: SpecMetadata,
    /// The top of the containment tree.
    root: Section,
    /// Logically the root's last child, held apart because it is fixed and
    /// contains only Terms.
    glossary: Glossary,
}

impl SlcrRequirementsDocument {
    /// Every Section in the containment tree, in document order. The
    /// Glossary is not included.
    pub fn sections(&self) -> Vec<&Section> {
        fn visit<'a>(section: &'a Section, sections: &mut Vec<&'a Section>) {
            sections.push(section);
            for child in section.children() {
                if let SectionChild::Section(child) = child {
                    visit(child, sections);
                }
            }
        }

        let mut sections = Vec::new();
        visit(&self.root, &mut sections);
        sections
    }

    /// Every Requirement in the containment tree, in document order.
    pub fn requirements(&self) -> Vec<&Requirement> {
        fn visit_section<'a>(section: &'a Section, requirements: &mut Vec<&'a Requirement>) {
            for child in section.children() {
                match child {
                    SectionChild::Section(child) => visit_section(child, requirements),
                    SectionChild::Requirement(child) => visit_requirement(child, requirements),
                }
            }
        }

        fn visit_requirement<'a>(
            requirement: &'a Requirement,
            requirements: &mut Vec<&'a Requirement>,
        ) {
            requirements.push(requirement);
            for child in requirement.children() {
                visit_requirement(child, requirements);
            }
        }

        let mut requirements = Vec::new();
        visit_section(&self.root, &mut requirements);
        requirements
    }

    /// Advisory findings that don't make the graph invalid, in document
    /// order.
    pub fn warnings(&self) -> Vec<Warning> {
        check::warnings(self)
    }
}

impl Serialize for SlcrRequirementsDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SlcrRequirementsDocument::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for SlcrRequirementsDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        UncheckedDocument::deserialize(deserializer)?
            .into_checked()
            .map_err(D::Error::custom)
    }
}

/// A requirements document read for its grammar alone: the invariants its
/// JSON Schema can't express have not been checked.
///
/// [UncheckedDocument::report] lists everything a check finds, while
/// [UncheckedDocument::into_checked] yields the document only if it breaks
/// no invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncheckedDocument(SlcrRequirementsDocument);

impl UncheckedDocument {
    /// The document, which may break its invariants.
    pub fn document(&self) -> &SlcrRequirementsDocument {
        &self.0
    }

    /// Every invariant the document breaks, and every warning.
    pub fn report(&self) -> Report {
        check::report(&self.0)
    }

    /// The document, if it breaks none of its invariants.
    pub fn into_checked(self) -> Result<SlcrRequirementsDocument, Violations> {
        check::invariants(&self.0)?;
        Ok(self.0)
    }
}

impl Serialize for UncheckedDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SlcrRequirementsDocument::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for UncheckedDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The inherent function `remote = "Self"` derives, which reads the
        // grammar without checking invariants.
        SlcrRequirementsDocument::deserialize(deserializer).map(Self)
    }
}

/// The version of the serialization format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FormatVersion {
    #[serde(rename = "1")]
    V1,
}

/// Facts about the specification as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecMetadata {
    /// The qualifier in references such as `todo-api/REQ-002`.
    name: SpecName,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::spec::check::Violation;
    use crate::spec::id::{RequirementId, SectionId};
    use crate::spec::node::Modality;

    /// The canon `TodoListItem` example from the SLCR Schemas page.
    const TODO_API: &str = include_str!("../../tests/fixtures/todo-api.spec.json");

    fn todo_api() -> Value {
        serde_json::from_str(TODO_API).unwrap()
    }

    #[test]
    fn the_canon_example_deserializes() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        assert_eq!(
            graph.schema().as_deref(),
            Some("https://slcr.io/reference/schemas/spec/v1.schema.json")
        );
        assert_eq!(*graph.format_version(), FormatVersion::V1);
        assert_eq!(graph.spec().name().as_str(), "todo-api");
        assert_eq!(graph.root().title().as_str(), "TodoList API");
        assert_eq!(graph.glossary().terms().len(), 1);
    }

    #[test]
    fn the_canon_example_serializes_back_to_itself() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        assert_eq!(serde_json::to_value(&graph).unwrap(), todo_api());
    }

    #[test]
    fn sections_are_listed_in_document_order() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        let ids: Vec<SectionId> = graph.sections().iter().map(|s| *s.id()).collect();
        let expected: Vec<SectionId> = (1..=5).map(SectionId::new).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn requirements_are_listed_in_document_order() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        let ids: Vec<u32> = graph
            .requirements()
            .iter()
            .map(|r| r.id().number())
            .collect();
        // REQ-004 is nested under REQ-002, which precedes REQ-003.
        assert_eq!(ids, [1, 2, 4, 3, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn the_canon_example_has_no_warnings() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        assert!(graph.warnings().is_empty());
    }

    #[test]
    fn the_schema_reference_is_optional() {
        let mut value = todo_api();
        value.as_object_mut().unwrap().remove("$schema");
        let graph: SlcrRequirementsDocument = serde_json::from_value(value.clone()).unwrap();
        assert!(graph.schema().is_none());
        assert_eq!(serde_json::to_value(&graph).unwrap(), value);
    }

    #[test]
    fn required_top_level_fields_are_enforced() {
        for field in ["formatVersion", "spec", "root", "glossary"] {
            let mut value = todo_api();
            value.as_object_mut().unwrap().remove(field);
            let err = serde_json::from_value::<SlcrRequirementsDocument>(value).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("missing field `{field}`")),
                "{err}"
            );
        }
    }

    #[test]
    fn unknown_top_level_fields_are_rejected() {
        let mut value = todo_api();
        value["status"] = json!({});
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
    }

    #[test]
    fn only_format_version_one_is_supported() {
        for version in [json!("2"), json!(1), json!("1.0")] {
            let mut value = todo_api();
            value["formatVersion"] = version;
            assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
        }
    }

    #[test]
    fn spec_names_are_validated() {
        let mut value = todo_api();
        value["spec"]["name"] = json!("Todo API");
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
    }

    #[test]
    fn spec_metadata_rejects_unknown_fields() {
        let mut value = todo_api();
        value["spec"]["version"] = json!("1.0.0");
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
    }

    #[test]
    fn the_root_must_be_a_section() {
        let mut value = todo_api();
        value["root"]["kind"] = json!("requirement");
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
    }

    #[test]
    fn the_glossary_must_be_a_section() {
        let mut value = todo_api();
        value["glossary"]["kind"] = json!("glossary");
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(value).is_err());
    }

    #[test]
    fn invariants_are_checked_on_deserialize() {
        let mut value = todo_api();
        // REQ-002 and REQ-003 refine REQ-001, so it needs a `refinement`.
        value["root"]["children"][0]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("refinement");
        let err = serde_json::from_value::<SlcrRequirementsDocument>(value).unwrap_err();
        assert!(
            err.to_string()
                .contains("REQ-001 is refined by REQ-002 but has no `refinement`"),
            "{err}"
        );
    }

    /// The canon example with REQ-001's `refinement` removed, though REQ-002
    /// and REQ-003 refine it, and REQ-003's body stripped of its modality.
    fn broken() -> Value {
        let mut value = todo_api();
        let endpoint = &mut value["root"]["children"][0]["children"][0];
        endpoint.as_object_mut().unwrap().remove("refinement");
        endpoint["children"][1]["body"] = json!("A GET request reads an item.");
        value
    }

    #[test]
    fn the_canon_example_checks_clean() {
        let unchecked: UncheckedDocument = serde_json::from_str(TODO_API).unwrap();
        assert!(unchecked.report().is_clean());
        let checked: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        assert_eq!(unchecked.into_checked().unwrap(), checked);
    }

    #[test]
    fn an_unchecked_document_reads_despite_broken_invariants() {
        assert!(serde_json::from_value::<SlcrRequirementsDocument>(broken()).is_err());
        let unchecked: UncheckedDocument = serde_json::from_value(broken()).unwrap();
        assert_eq!(unchecked.document().spec().name().as_str(), "todo-api");
    }

    #[test]
    fn the_report_lists_violations_and_warnings_together() {
        let unchecked: UncheckedDocument = serde_json::from_value(broken()).unwrap();
        let report = unchecked.report();
        assert!(!report.is_clean());
        assert_eq!(
            report.violations(),
            [Violation::MissingRefinement {
                requirement: RequirementId::new(1),
                refiner: RequirementId::new(2),
            }]
        );
        assert_eq!(
            report.warnings(),
            [Warning::ModalityNotStated {
                requirement: RequirementId::new(3),
                modality: Modality::Must,
            }]
        );
    }

    #[test]
    fn into_checked_returns_every_violation() {
        let unchecked: UncheckedDocument = serde_json::from_value(broken()).unwrap();
        let violations = unchecked.clone().into_checked().unwrap_err();
        assert_eq!(violations.violations(), unchecked.report().violations());
    }

    #[test]
    fn an_unchecked_document_still_enforces_the_grammar() {
        let mut value = todo_api();
        value["root"]["id"] = json!("SEC-1");
        assert!(serde_json::from_value::<UncheckedDocument>(value).is_err());
    }

    #[test]
    fn an_unchecked_document_serializes_like_its_document() {
        let unchecked: UncheckedDocument = serde_json::from_value(broken()).unwrap();
        assert_eq!(serde_json::to_value(&unchecked).unwrap(), broken());
    }

    #[test]
    fn requirement_lookup_reflects_nesting() {
        let graph: SlcrRequirementsDocument = serde_json::from_str(TODO_API).unwrap();
        let authenticated = graph
            .requirements()
            .into_iter()
            .find(|r| *r.id() == RequirementId::new(4))
            .unwrap();
        assert_eq!(
            *authenticated.refines(),
            [RequirementId::new(2), RequirementId::new(3)]
        );
    }
}
