//! The slice of one requirement R, rendered under the rules of the whole
//! document, per section 9 of the SLCR design document.
//!
//! The slice reads like a shortened version of the document:
//!
//! 1. R's containment ancestors, root first, as headings with their bodies.
//! 2. R, and the requirements nested under it that the slice includes, as
//!    the document renders them.
//! 3. Everything else the slice includes, under "Related requirements", in
//!    document order: what R refines, the requirements refining R that are
//!    housed elsewhere, what R depends on, and the ancestors of those
//!    dependencies. Each is one heading level below that section, with its
//!    ancestor path as a breadcrumb rather than nested headings, since
//!    nesting would misplace it in the document. Its Refines line names
//!    every requirement it refines, its containment parent included, since
//!    no heading shows that parent.
//! 4. The Glossary, with only the terms the slice includes.
//!
//! Every node keeps the number it has in the document. A cross-reference
//! the renderer writes to a node the slice leaves out names it without
//! linking to it, so that each of those links resolves within the slice.
//! Prose is copied as written, as in the document.

use super::{GENERATED_PREFIX, Number, Renderer};
use crate::slice::{Detail, Members};
use crate::spec::graph::SlcrRequirementsDocument;
use crate::spec::id::{DependencyId, RequirementId};
use crate::spec::node::{Requirement, Section, SectionChild};
use crate::spec::text::Title;

/// The heading level of the nodes under "Related requirements".
const RELATED_LEVEL: usize = 3;

/// Render the slice of the requirement `members` is for, from `graph`, as
/// Markdown.
pub(crate) fn slice(graph: &SlcrRequirementsDocument, members: &Members) -> String {
    let requirement = members.requirement();
    let entries = Placement::new(graph, members).entries;
    let mut renderer = Renderer::new(graph, Some(members));
    renderer.blocks.push(&format!(
        "{GENERATED_PREFIX}from the {} specification: the slice for {requirement}. Do not \
         edit: change the specification and run `slcr slice {requirement}`. -->",
        graph.spec().name()
    ));

    let (placed, related): (Vec<_>, Vec<_>) = entries.iter().partition(|entry| entry.in_place);
    for entry in placed {
        renderer.heading(entry.node.id(), &entry.number, entry.node.title());
        renderer.block(entry);
    }
    if !related.is_empty() {
        renderer.blocks.push("## Related requirements");
    }
    for entry in related {
        renderer.heading_at(
            RELATED_LEVEL,
            entry.node.id(),
            &entry.number,
            entry.node.title(),
        );
        let path: Vec<String> = entry
            .ancestors
            .iter()
            .map(|ancestor| renderer.dependency_link(*ancestor))
            .collect();
        renderer.labeled("In", &path.join(" › "));
        renderer.block(entry);
    }

    let root = graph.root();
    renderer.glossary(&Number::default().child(root.children().len() + 1));
    renderer.blocks.finish()
}

impl Renderer<'_> {
    /// What `entry` shows under its heading.
    fn block(&mut self, entry: &Entry<'_>) {
        match entry.node {
            Node::Section(section) => self.section_block(section),
            Node::Requirement(requirement, parent) => {
                // The Refines line leaves out a containment parent only
                // where it is plain from the headings: under "Related
                // requirements", the parent is just a breadcrumb.
                let parent = parent.filter(|_| entry.in_place);
                self.requirement_block(requirement, parent, entry.detail);
            }
        }
    }
}

/// A Section or Requirement in the containment tree.
#[derive(Clone, Copy, Debug)]
enum Node<'a> {
    Section(&'a Section),
    /// A requirement, and its containment parent if that is a requirement.
    Requirement(&'a Requirement, Option<RequirementId>),
}

impl<'a> Node<'a> {
    fn id(self) -> DependencyId {
        match self {
            Self::Section(section) => DependencyId::Section(*section.id()),
            Self::Requirement(requirement, _) => DependencyId::Requirement(*requirement.id()),
        }
    }

    fn title(self) -> &'a Title {
        match self {
            Self::Section(section) => section.title(),
            Self::Requirement(requirement, _) => requirement.title(),
        }
    }
}

/// A node the slice includes, and where it goes.
#[derive(Debug)]
struct Entry<'a> {
    node: Node<'a>,
    number: Number,
    detail: Detail,
    /// The node's containment ancestors, root first.
    ancestors: Vec<DependencyId>,
    /// Whether it renders in its place in the tree, rather than under
    /// "Related requirements".
    in_place: bool,
}

/// Where a node's children go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Children {
    /// Into their place if they are on the path from the root to R.
    OnPath,
    /// Into their place if the slice includes them: R's, and those of
    /// included nodes nested under R.
    Nested,
    /// Under "Related requirements" if the slice includes them.
    Related,
}

/// Every node the slice includes, in document order, and where each goes.
struct Placement<'a> {
    members: &'a Members,
    /// The path from the root to R, both included.
    path: Vec<DependencyId>,
    entries: Vec<Entry<'a>>,
}

impl<'a> Placement<'a> {
    fn new(graph: &'a SlcrRequirementsDocument, members: &'a Members) -> Self {
        let requirement = DependencyId::Requirement(members.requirement());
        let mut placement = Self {
            members,
            path: path(graph.root(), requirement).expect("the sliced requirement is in the graph"),
            entries: Vec::new(),
        };
        placement.visit(
            Node::Section(graph.root()),
            Number::default(),
            &mut Vec::new(),
            Children::OnPath,
        );
        placement
    }

    /// Place `node`, numbered `number` under `ancestors`, where its parent
    /// puts its `children`; then its own children.
    fn visit(
        &mut self,
        node: Node<'a>,
        number: Number,
        ancestors: &mut Vec<DependencyId>,
        siblings: Children,
    ) {
        let id = node.id();
        let detail = self.members.detail(id);
        let on_path = self.path.contains(&id);
        let in_place = match siblings {
            Children::OnPath => on_path,
            Children::Nested => detail.is_some(),
            Children::Related => false,
        };
        let children = if id == *self.path.last().expect("a path") {
            Children::Nested
        } else if on_path {
            Children::OnPath
        } else if in_place {
            siblings
        } else {
            Children::Related
        };
        if let Some(detail) = detail {
            self.entries.push(Entry {
                node,
                number: number.clone(),
                detail,
                ancestors: ancestors.clone(),
                in_place,
            });
        }

        ancestors.push(id);
        match node {
            Node::Section(section) => {
                for (index, child) in section.children().iter().enumerate() {
                    let child = match child {
                        SectionChild::Section(child) => Node::Section(child),
                        SectionChild::Requirement(child) => Node::Requirement(child, None),
                    };
                    self.visit(child, number.child(index + 1), ancestors, children);
                }
            }
            Node::Requirement(requirement, _) => {
                for (index, child) in requirement.children().iter().enumerate() {
                    let child = Node::Requirement(child, Some(*requirement.id()));
                    self.visit(child, number.child(index + 1), ancestors, children);
                }
            }
        }
        ancestors.pop();
    }
}

/// The IDs on the path from `section` down to `target`, both included, if
/// `section` contains it.
fn path(section: &Section, target: DependencyId) -> Option<Vec<DependencyId>> {
    fn requirement_path(
        requirement: &Requirement,
        target: DependencyId,
    ) -> Option<Vec<DependencyId>> {
        let id = DependencyId::Requirement(*requirement.id());
        if id == target {
            return Some(vec![id]);
        }
        requirement.children().iter().find_map(|child| {
            let mut path = requirement_path(child, target)?;
            path.insert(0, id);
            Some(path)
        })
    }

    let id = DependencyId::Section(*section.id());
    if id == target {
        return Some(vec![id]);
    }
    section.children().iter().find_map(|child| {
        let mut path = match child {
            SectionChild::Section(child) => path(child, target)?,
            SectionChild::Requirement(child) => requirement_path(child, target)?,
        };
        path.insert(0, id);
        Some(path)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::super::tests::{
        anchors, canon, headings, link_targets, requirement, section, spec, spec_value, synthetic,
        term, visible_text, with,
    };
    use super::super::{DEEPEST_HEADING, is_rendered};
    use super::*;
    use crate::cli::block_on;
    use crate::query::SpecGraph;
    use crate::slice::members;

    /// What the canon example's controller, REQ-005, slices to.
    const TODO_API_REQ_005_MD: &str = include_str!("../../tests/fixtures/todo-api.REQ-005.md");

    fn members_of(graph: &SlcrRequirementsDocument, id: u32) -> Members {
        block_on(async {
            let store = SpecGraph::new(graph).await.unwrap();
            members(&store, RequirementId::new(id)).await.unwrap()
        })
        .unwrap()
    }

    fn slice_of(graph: &SlcrRequirementsDocument, id: u32) -> String {
        slice(graph, &members_of(graph, id))
    }

    /// The anchors a slice with `members` must have: one for each node it
    /// includes, each criterion of an included dependency or requirement in
    /// full, and the Glossary if the slice includes a term.
    fn expected_anchors(graph: &SlcrRequirementsDocument, members: &Members) -> Vec<String> {
        let mut ids: Vec<String> = members.nodes().keys().map(ToString::to_string).collect();
        for requirement in graph.requirements() {
            let detail = members.detail(DependencyId::Requirement(*requirement.id()));
            if detail >= Some(Detail::Dependency) {
                let criteria = requirement.acceptance_criteria().iter();
                ids.extend(criteria.map(|criterion| criterion.id().to_string()));
            }
        }
        ids.extend(members.terms().iter().map(ToString::to_string));
        let glossary = graph.glossary().id().to_string();
        if !members.terms().is_empty() && !ids.contains(&glossary) {
            ids.push(glossary);
        }
        ids.sort();
        ids
    }

    /// Assert what holds of every slice: one anchor for every node it
    /// includes and no other, every link resolving to one, headings no
    /// deeper than H6, no ID in the visible text, and clean text.
    fn assert_well_formed(graph: &SlcrRequirementsDocument, id: u32) {
        let members = members_of(graph, id);
        let rendered = slice(graph, &members);
        let context = format!("the slice for REQ-{id:03}:\n{rendered}");

        let anchors = anchors(&rendered);
        let unique: HashSet<&str> = anchors.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), anchors.len(), "anchors repeat in {context}");
        let mut sorted = anchors.clone();
        sorted.sort();
        assert_eq!(sorted, expected_anchors(graph, &members), "{context}");

        for target in link_targets(&rendered) {
            let id = target.strip_prefix('#').expect("an intra-document link");
            assert!(
                unique.contains(id),
                "{target} resolves to nothing in {context}"
            );
        }

        let headings = headings(&rendered);
        assert!(headings.iter().all(|(level, _)| *level <= DEEPEST_HEADING));
        assert_eq!(headings[0], (1, graph.root().title().to_string()));

        let text = visible_text(&rendered);
        for prefix in ["SEC-", "REQ-", "TERM-", "AC-"] {
            assert!(!text.contains(prefix), "an ID is visible in {context}");
        }

        assert!(!rendered.contains('\r'));
        assert!(rendered.ends_with('\n') && !rendered.ends_with("\n\n"));
        assert!(is_rendered(rendered.as_bytes()));
    }

    /// The part of `rendered` from the anchor of `from` up to the anchor of
    /// `to`, or to the end, without trailing whitespace.
    fn between<'a>(rendered: &'a str, from: &str, to: Option<&str>) -> &'a str {
        let start = rendered
            .find(&format!(r#"<a id="{from}"></a>"#))
            .unwrap_or_else(|| panic!("{from}'s anchor in {rendered}"));
        let end = to.map_or(rendered.len(), |to| {
            rendered
                .find(&format!(r#"<a id="{to}"></a>"#))
                .unwrap_or_else(|| panic!("{to}'s anchor in {rendered}"))
        });
        rendered[start..end].trim_end()
    }

    #[test]
    fn the_canon_controller_slices_exactly() {
        assert_eq!(slice_of(&canon(), 5), TODO_API_REQ_005_MD);
    }

    #[test]
    fn every_slice_of_the_canon_example_is_well_formed() {
        let graph = canon();
        for requirement in graph.requirements() {
            assert_well_formed(&graph, requirement.id().number());
        }
    }

    #[test]
    fn every_slice_of_a_large_document_is_well_formed() {
        let graph = synthetic();
        for requirement in graph.requirements() {
            assert_well_formed(&graph, requirement.id().number());
        }
    }

    #[test]
    fn slicing_is_deterministic() {
        let graph = synthetic();
        for requirement in graph.requirements() {
            let id = requirement.id().number();
            assert_eq!(slice_of(&graph, id), slice_of(&graph.clone(), id));
        }
    }

    #[test]
    fn the_slice_names_its_requirement_only_in_the_generated_comment() {
        let rendered = slice_of(&canon(), 7);
        assert!(rendered.starts_with(
            "<!-- Generated by slcr from the todo-api specification: the slice for REQ-007. \
             Do not edit: change the specification and run `slcr slice REQ-007`. -->\n\n"
        ));
    }

    #[test]
    fn a_leaf_slices_to_its_ancestors_itself_and_its_terms() {
        assert_eq!(
            slice_of(&canon(), 7),
            "<!-- Generated by slcr from the todo-api specification: the slice for REQ-007. \
             Do not edit: change the specification and run `slcr slice REQ-007`. -->\n\
             \n\
             <a id=\"SEC-001\"></a>\n\
             # TodoList API\n\
             \n\
             A REST web application in MVC layout with one resource.\n\
             \n\
             <a id=\"SEC-004\"></a>\n\
             ## 3. Models\n\
             \n\
             Persistence assumptions.\n\
             \n\
             <a id=\"REQ-007\"></a>\n\
             ### 3.1 Table todo_list_item exists\n\
             \n\
             There MUST be a table `todo_list_item` with columns `title` (string) and `done` \
             (bool).\n\
             \n\
             *Terms:* [TodoListItem](#TERM-001)\n\
             \n\
             *Acceptance criteria:*\n\
             \n\
             <a id=\"AC-001\"></a>**Table schema is declared**\n\
             \n\
             A table named `todo_list_item` is declared with a string column `title` and a \
             boolean column `done`. A structural match against the schema or migration source \
             is sufficient.\n\
             \n\
             <a id=\"SEC-006\"></a>\n\
             ## 5. Glossary\n\
             \n\
             <dl>\n\
             <dt><a id=\"TERM-001\"></a>TodoListItem</dt>\n\
             <dd>\n\
             \n\
             A single item on a to-do list, with a title and a done flag.\n\
             \n\
             </dd>\n\
             </dl>\n"
        );
    }

    #[test]
    fn a_slice_without_terms_has_no_glossary() {
        let rendered = slice_of(&canon(), 1);
        assert!(!rendered.contains("Glossary"), "{rendered}");
        assert!(
            rendered.ends_with("- [Request is authenticated](#REQ-004)\n"),
            "{rendered}"
        );
    }

    #[test]
    fn nested_refinements_render_in_place_and_elaborations_are_left_out() {
        // REQ-002 elaborates REQ-001 without refining it; REQ-003 refines
        // it, and REQ-004, nested under REQ-002, refines it too.
        let graph = spec(
            json!([with(
                requirement(1, "Parent"),
                json!({
                    "refinement": "AND",
                    "children": [
                        with(requirement(2, "Elaboration"), json!({
                            "children": [
                                with(requirement(4, "Deep refiner"), json!({ "refines": ["REQ-001"] })),
                            ],
                        })),
                        with(requirement(3, "Refiner"), json!({ "refines": ["REQ-001"] })),
                    ],
                })
            )]),
            json!([]),
        );
        let rendered = slice_of(&graph, 1);
        assert!(!rendered.contains("REQ-002"), "{rendered}");
        assert_eq!(
            between(&rendered, "REQ-001", None),
            "<a id=\"REQ-001\"></a>\n\
             ## 1. Parent\n\
             \n\
             Parent MUST hold.\n\
             \n\
             All of the following must hold:\n\
             \n\
             - [Deep refiner](#REQ-004)\n\
             - [Refiner](#REQ-003)\n\
             \n\
             <a id=\"REQ-003\"></a>\n\
             ### 1.2 Refiner\n\
             \n\
             Refiner MUST hold.\n\
             \n\
             ## Related requirements\n\
             \n\
             <a id=\"REQ-004\"></a>\n\
             ### 1.1.1 Deep refiner\n\
             \n\
             *In:* [Root](#SEC-001) › [Parent](#REQ-001) › Elaboration\n\
             \n\
             Deep refiner MUST hold.\n\
             \n\
             *Refines:* [Parent](#REQ-001)"
        );
    }

    #[test]
    fn a_related_requirement_names_every_requirement_it_refines() {
        // REQ-004 refines REQ-002, its container, and REQ-003, the slice's
        // subject. Under "Related requirements" no heading shows REQ-002,
        // which the slice leaves out.
        let rendered = slice_of(&canon(), 3);
        assert!(
            rendered.ends_with(
                "<a id=\"REQ-004\"></a>\n\
                 ### 1.1.1.1 Request is authenticated\n\
                 \n\
                 *In:* [TodoList API](#SEC-001) › [Views](#SEC-002) › \
                 [TodoListItem endpoint](#REQ-001) › POST creates an item\n\
                 \n\
                 The request MUST be authenticated.\n\
                 \n\
                 *Refines:* POST creates an item, [GET reads an item](#REQ-003)\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn ancestors_and_what_the_requirement_refines_show_their_bodies_only() {
        // REQ-002, the slice's subject, is nested under REQ-001 and refines
        // REQ-003, both with every part a requirement can have.
        let everything = json!({
            "rationale": "Because.",
            "refinement": "AND",
            "dependsOn": ["SEC-002"],
            "usesTerm": ["TERM-001"],
            "acceptanceCriteria": [{
                "kind": "acceptanceCriterion", "id": "AC-001", "title": "Done", "body": "Done."
            }],
        });
        let graph = spec(
            json!([
                with(
                    with(requirement(1, "Container"), everything.clone()),
                    json!({ "children": [
                        with(requirement(2, "Subject"), json!({ "refines": ["REQ-001", "REQ-003"] })),
                    ] })
                ),
                with(
                    with(requirement(3, "Refined"), everything),
                    json!({ "acceptanceCriteria": [{
                        "kind": "acceptanceCriterion", "id": "AC-002", "title": "Done", "body": "Done."
                    }] })
                ),
                with(section(2, "Elsewhere"), json!({ "body": "Not included." })),
            ]),
            json!([term(1, "Thing", "A thing.")]),
        );
        let rendered = slice_of(&graph, 2);
        assert_eq!(
            between(&rendered, "REQ-001", Some("SEC-900")),
            "<a id=\"REQ-001\"></a>\n\
             ## 1. Container\n\
             \n\
             Container MUST hold.\n\
             \n\
             *Terms:* [Thing](#TERM-001)\n\
             \n\
             <a id=\"REQ-002\"></a>\n\
             ### 1.1 Subject\n\
             \n\
             Subject MUST hold.\n\
             \n\
             *Refines:* [Refined](#REQ-003)\n\
             \n\
             ## Related requirements\n\
             \n\
             <a id=\"REQ-003\"></a>\n\
             ### 2. Refined\n\
             \n\
             *In:* [Root](#SEC-001)\n\
             \n\
             Refined MUST hold.\n\
             \n\
             *Terms:* [Thing](#TERM-001)"
        );
    }

    #[test]
    fn dependencies_show_their_criteria_and_dependencies_but_not_their_reasons() {
        let graph = spec(
            json!([
                with(
                    requirement(1, "Subject"),
                    json!({ "dependsOn": ["REQ-002"] })
                ),
                with(
                    requirement(2, "Dependency"),
                    json!({
                        "rationale": "Because.",
                        "refinement": "OR",
                        "dependsOn": ["SEC-002"],
                        "acceptanceCriteria": [{
                            "kind": "acceptanceCriterion", "id": "AC-001",
                            "title": "Checked", "body": "It is checked."
                        }],
                        "children": [
                            with(requirement(3, "Option"), json!({ "refines": ["REQ-002"] })),
                        ],
                    })
                ),
                with(
                    section(2, "Background"),
                    json!({
                        "body": "Some background.",
                        "children": [requirement(4, "Under the background")],
                    })
                ),
            ]),
            json!([]),
        );
        let rendered = slice_of(&graph, 1);
        assert_eq!(
            between(&rendered, "REQ-002", None),
            "<a id=\"REQ-002\"></a>\n\
             ### 2. Dependency\n\
             \n\
             *In:* [Root](#SEC-001)\n\
             \n\
             Dependency MUST hold.\n\
             \n\
             *Depends on:* [Background](#SEC-002)\n\
             \n\
             *Acceptance criteria:*\n\
             \n\
             <a id=\"AC-001\"></a>**Checked**\n\
             \n\
             It is checked.\n\
             \n\
             <a id=\"SEC-002\"></a>\n\
             ### 3. Background\n\
             \n\
             *In:* [Root](#SEC-001)\n\
             \n\
             Some background."
        );
    }

    #[test]
    fn the_glossary_holds_only_the_terms_the_slice_includes() {
        let graph = spec(
            json!([
                with(
                    requirement(1, "Subject"),
                    json!({ "usesTerm": ["TERM-002"] })
                ),
                with(
                    requirement(2, "Sibling"),
                    json!({ "usesTerm": ["TERM-001"] })
                ),
            ]),
            json!([
                term(1, "Excluded", "Left out."),
                term(2, "Included", "Kept.")
            ]),
        );
        let rendered = slice_of(&graph, 1);
        assert_eq!(
            between(&rendered, "SEC-900", None),
            "<a id=\"SEC-900\"></a>\n\
             ## 3. Glossary\n\
             \n\
             <dl>\n\
             <dt><a id=\"TERM-002\"></a>Included</dt>\n\
             <dd>\n\
             \n\
             Kept.\n\
             \n\
             </dd>\n\
             </dl>"
        );
    }

    #[test]
    fn a_dependency_on_the_glossary_brings_its_body() {
        let mut value = spec_value(
            json!([with(
                requirement(1, "Subject"),
                json!({ "dependsOn": ["SEC-900"] })
            )]),
            json!([term(1, "Unused", "Not used.")]),
        );
        value["glossary"]["body"] = json!("How terms are used.");
        let graph: SlcrRequirementsDocument = serde_json::from_value(value).unwrap();
        let rendered = slice_of(&graph, 1);
        assert!(
            rendered.ends_with(
                "*Depends on:* [Glossary](#SEC-900)\n\
                 \n\
                 <a id=\"SEC-900\"></a>\n\
                 ## 2. Glossary\n\
                 \n\
                 How terms are used.\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn the_path_to_a_node_runs_from_the_root() {
        let graph = canon();
        let ids = |target: &str| {
            path(graph.root(), target.parse().unwrap())
                .map(|path| path.iter().map(ToString::to_string).collect::<Vec<_>>())
        };
        assert_eq!(
            ids("REQ-004").unwrap(),
            ["SEC-001", "SEC-002", "REQ-001", "REQ-002", "REQ-004"]
        );
        assert_eq!(ids("SEC-001").unwrap(), ["SEC-001"]);
        assert_eq!(ids("REQ-099"), None);
    }
}
