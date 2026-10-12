use std::{collections::BTreeMap, path::PathBuf};

use miette::Diagnostic;
use thiserror::Error;

use super::{Problem, QualifiedRequirementId, SpecFile};
use crate::spec::graph::UnvettedDocument;
use crate::spec::node::Requirement;
use crate::spec::text::SpecName;

/// A loaded specification of the project, and the file it came from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexedSpec<'a> {
    pub(crate) file: &'a SpecFile,
    pub(crate) document: &'a UnvettedDocument,
}

impl<'a> IndexedSpec<'a> {
    /// The requirement `id`, wherever it is nested.
    fn requirement(self, id: &crate::spec::id::RequirementId) -> Option<&'a Requirement> {
        self.document
            .document()
            .requirements()
            .into_iter()
            .find(|requirement| requirement.id() == id)
    }
}

/// The project's specifications, by their `spec.name`, which is unique
/// across the files the manifest's `specs` match.
#[derive(Debug, Default)]
pub(crate) struct SpecIndex<'a> {
    specs: BTreeMap<&'a SpecName, IndexedSpec<'a>>,
}

/// A qualified ID, resolved.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolved<'a> {
    pub(crate) spec: IndexedSpec<'a>,
    pub(crate) requirement: &'a Requirement,
}

/// The ways a qualified ID can fail to resolve.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum UnresolvedRequirement {
    #[error("`{id}` names the specification `{}`, which the project doesn't have", .id.spec())]
    #[diagnostic(help("{known}"))]
    UnknownSpec {
        id: QualifiedRequirementId,
        /// The specifications the project does have.
        known: String,
    },

    #[error(
        "`{id}` names {}, which the specification `{}` ({}) doesn't have",
        .id.requirement(),
        .id.spec(),
        .path.display()
    )]
    UnknownRequirement {
        id: QualifiedRequirementId,
        /// The specification's file, relative to the working directory.
        path: PathBuf,
    },
}

impl<'a> SpecIndex<'a> {
    /// Index `specs`, given in the project's order. A specification whose
    /// name an earlier one has is left out, and reported as a problem that
    /// names both files.
    pub(crate) fn new(specs: impl IntoIterator<Item = IndexedSpec<'a>>) -> (Self, Vec<Problem>) {
        let mut index = Self::default();
        let mut problems = Vec::new();
        for spec in specs {
            let name = spec.document.document().spec().name();
            match index.specs.get(name) {
                Some(first) => problems.push(Problem::DuplicateSpecName {
                    name: name.clone(),
                    first: first.file.shown().to_path_buf(),
                    second: spec.file.shown().to_path_buf(),
                }),
                None => {
                    index.specs.insert(name, spec);
                }
            }
        }
        (index, problems)
    }

    /// The specification named `name`.
    pub(crate) fn get(&self, name: &SpecName) -> Option<IndexedSpec<'a>> {
        self.specs.get(name).copied()
    }

    /// The specifications' names, in order.
    pub(crate) fn names(&self) -> impl Iterator<Item = &'a SpecName> + '_ {
        self.specs.keys().copied()
    }

    /// The requirement `id` names, and its specification.
    pub(crate) fn resolve(
        &self,
        id: &QualifiedRequirementId,
    ) -> Result<Resolved<'a>, UnresolvedRequirement> {
        let Some(spec) = self.get(id.spec()) else {
            let names: Vec<&str> = self.names().map(SpecName::as_str).collect();
            let known = if names.is_empty() {
                "the project has no specifications".to_owned()
            } else {
                format!("the project's specifications are: {}", names.join(", "))
            };
            return Err(UnresolvedRequirement::UnknownSpec {
                id: id.clone(),
                known,
            });
        };
        let requirement = spec.requirement(id.requirement()).ok_or_else(|| {
            UnresolvedRequirement::UnknownRequirement {
                id: id.clone(),
                path: spec.file.shown().to_path_buf(),
            }
        })?;
        Ok(Resolved { spec, requirement })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::*;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");

    /// The canon example, named `name`.
    fn named(name: &str) -> UnvettedDocument {
        let mut value: Value = serde_json::from_str(TODO_API_JSON).unwrap();
        value["spec"]["name"] = Value::from(name);
        serde_json::from_value(value).unwrap()
    }

    fn file(relative: &str) -> SpecFile {
        SpecFile::new(Path::new("/p"), Path::new("/p"), PathBuf::from(relative))
    }

    fn id(value: &str) -> QualifiedRequirementId {
        value.parse().unwrap()
    }

    /// Files and the documents they hold, to index.
    fn specs(entries: &[(&str, &str)]) -> Vec<(SpecFile, UnvettedDocument)> {
        entries
            .iter()
            .map(|(path, name)| (file(path), named(name)))
            .collect()
    }

    fn index(specs: &[(SpecFile, UnvettedDocument)]) -> (SpecIndex<'_>, Vec<Problem>) {
        SpecIndex::new(
            specs
                .iter()
                .map(|(file, document)| IndexedSpec { file, document }),
        )
    }

    #[test]
    fn specifications_are_indexed_by_name() {
        let specs = specs(&[("todo.yaml", "todo-api"), ("billing.json", "billing")]);
        let (index, problems) = index(&specs);
        assert_eq!(problems, []);
        let names: Vec<_> = index.names().map(SpecName::as_str).collect();
        assert_eq!(names, ["billing", "todo-api"]);
        let billing = SpecName::try_from("billing".to_owned()).unwrap();
        assert_eq!(
            index.get(&billing).unwrap().file.relative(),
            Path::new("billing.json")
        );
    }

    #[test]
    fn a_duplicate_name_names_both_files() {
        let specs = specs(&[
            ("a.yaml", "todo-api"),
            ("b.json", "billing"),
            ("c.toml", "todo-api"),
            ("d.yml", "todo-api"),
        ]);
        let (index, problems) = index(&specs);
        let messages: Vec<String> = problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "a.yaml and c.toml both name their specification `todo-api`",
                "a.yaml and d.yml both name their specification `todo-api`",
            ]
        );
        // The first file keeps the name.
        let resolved = index.resolve(&id("todo-api/REQ-002")).unwrap();
        assert_eq!(resolved.spec.file.relative(), Path::new("a.yaml"));
    }

    #[test]
    fn a_qualified_id_resolves_to_its_requirement() {
        let specs = specs(&[("todo.yaml", "todo-api"), ("billing.json", "billing")]);
        let (index, _) = index(&specs);
        let resolved = index.resolve(&id("billing/REQ-002")).unwrap();
        assert_eq!(resolved.spec.file.relative(), Path::new("billing.json"));
        assert_eq!(resolved.requirement.id().to_string(), "REQ-002");
        assert_eq!(
            resolved.requirement.title().as_str(),
            "POST creates an item"
        );
        // Nested requirements resolve too.
        let nested = index.resolve(&id("todo-api/REQ-010")).unwrap();
        assert_eq!(nested.requirement.id().to_string(), "REQ-010");
    }

    #[test]
    fn an_unknown_spec_lists_the_known_ones() {
        let specs = specs(&[("todo.yaml", "todo-api"), ("billing.json", "billing")]);
        let (index, _) = index(&specs);
        let err = index.resolve(&id("todo/REQ-002")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`todo/REQ-002` names the specification `todo`, which the project doesn't have"
        );
        assert_eq!(
            err.help().unwrap().to_string(),
            "the project's specifications are: billing, todo-api"
        );

        let (empty, _) = SpecIndex::new([]);
        let err = empty.resolve(&id("todo/REQ-002")).unwrap_err();
        assert_eq!(
            err.help().unwrap().to_string(),
            "the project has no specifications"
        );
    }

    #[test]
    fn an_unknown_requirement_names_its_spec_and_file() {
        let specs = specs(&[("specs/todo.yaml", "todo-api")]);
        let (index, _) = index(&specs);
        let err = index.resolve(&id("todo-api/REQ-099")).unwrap_err();
        assert_eq!(
            err,
            UnresolvedRequirement::UnknownRequirement {
                id: id("todo-api/REQ-099"),
                path: PathBuf::from("specs/todo.yaml"),
            }
        );
        assert_eq!(
            err.to_string(),
            "`todo-api/REQ-099` names REQ-099, which the specification `todo-api` \
             (specs/todo.yaml) doesn't have"
        );
    }
}
