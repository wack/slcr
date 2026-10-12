use std::{collections::BTreeMap, path::PathBuf};

use miette::Diagnostic;
use thiserror::Error;

use super::{Problem, QualifiedRequirementId, SpecFile};
use crate::spec::graph::UnvettedDocument;
use crate::spec::id::{CriterionId, RequirementId};
use crate::spec::node::{AcceptanceCriterion, Requirement};
use crate::spec::text::SpecName;

/// A loaded specification of the project, and the file it came from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexedSpec<'a> {
    pub(crate) file: &'a SpecFile,
    pub(crate) document: &'a UnvettedDocument,
}

impl<'a> IndexedSpec<'a> {
    /// The requirement `id`, wherever it is nested.
    fn requirement(self, id: &RequirementId) -> Option<&'a Requirement> {
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

/// The error returned when a requirement doesn't contain an acceptance
/// criterion, as a validator's `criterion` must name one it does.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "`{criterion}` is not an acceptance criterion of `{requirement}` ({})",
    .path.display()
)]
pub struct UnknownCriterion {
    criterion: CriterionId,
    requirement: QualifiedRequirementId,
    /// The specification's file, relative to the working directory.
    path: PathBuf,
    /// The requirement that does contain the criterion, if one does.
    owner: Option<RequirementId>,
    /// The requirement's own criteria.
    own: Vec<CriterionId>,
}

impl Diagnostic for UnknownCriterion {
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        let own = if self.own.is_empty() {
            format!(
                "{} has no acceptance criteria",
                self.requirement.requirement()
            )
        } else {
            let own: Vec<String> = self.own.iter().map(ToString::to_string).collect();
            format!(
                "the acceptance criteria of {} are: {}",
                self.requirement.requirement(),
                own.join(", ")
            )
        };
        Some(Box::new(match self.owner {
            Some(owner) => format!("{} belongs to {owner}; {own}", self.criterion),
            None => own,
        }))
    }
}

impl<'a> Resolved<'a> {
    /// The acceptance criterion `id` among the requirement's own: those it
    /// contains, not those of the requirements nested in it.
    pub(crate) fn criterion(
        &self,
        id: &CriterionId,
    ) -> Result<&'a AcceptanceCriterion, UnknownCriterion> {
        let own = self.requirement.acceptance_criteria();
        if let Some(criterion) = own.iter().find(|criterion| criterion.id() == id) {
            return Ok(criterion);
        }
        let document = self.spec.document.document();
        let owner = document
            .requirements()
            .into_iter()
            .find(|requirement| {
                requirement
                    .acceptance_criteria()
                    .iter()
                    .any(|criterion| criterion.id() == id)
            })
            .map(|requirement| *requirement.id());
        Err(UnknownCriterion {
            criterion: *id,
            requirement: QualifiedRequirementId::new(
                document.spec().name().clone(),
                *self.requirement.id(),
            ),
            path: self.spec.file.shown().to_path_buf(),
            owner,
            own: own.iter().map(|criterion| *criterion.id()).collect(),
        })
    }
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

    fn criterion(value: &str) -> CriterionId {
        value.parse().unwrap()
    }

    #[test]
    fn a_criterion_resolves_within_its_requirement() {
        let specs = specs(&[("todo.yaml", "todo-api")]);
        let (index, _) = index(&specs);
        let table = index.resolve(&id("todo-api/REQ-007")).unwrap();
        let resolved = table.criterion(&criterion("AC-001")).unwrap();
        assert_eq!(resolved.title().as_str(), "Table schema is declared");
    }

    #[test]
    fn a_criterion_of_another_requirement_names_its_owner() {
        let specs = specs(&[("specs/todo.yaml", "todo-api")]);
        let (index, _) = index(&specs);
        let endpoint = index.resolve(&id("todo-api/REQ-001")).unwrap();
        let err = endpoint.criterion(&criterion("AC-001")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`AC-001` is not an acceptance criterion of `todo-api/REQ-001` (specs/todo.yaml)"
        );
        assert_eq!(
            err.help().unwrap().to_string(),
            "AC-001 belongs to REQ-007; REQ-001 has no acceptance criteria"
        );
    }

    #[test]
    fn an_unknown_criterion_lists_the_requirements_own() {
        let specs = specs(&[("todo.yaml", "todo-api")]);
        let (index, _) = index(&specs);
        let table = index.resolve(&id("todo-api/REQ-007")).unwrap();
        let err = table.criterion(&criterion("AC-099")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`AC-099` is not an acceptance criterion of `todo-api/REQ-007` (todo.yaml)"
        );
        assert_eq!(
            err.help().unwrap().to_string(),
            "the acceptance criteria of REQ-007 are: AC-001"
        );
    }
}
