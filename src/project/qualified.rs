use std::{
    fmt::{self, Display},
    str::FromStr,
};

use derive_getters::Getters;
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::spec::id::RequirementId;
use crate::spec::text::SpecName;

/// A requirement named from outside its specification, such as
/// `todo-api/REQ-002`: the specification's `name`, a slash, and the
/// requirement's ID. The qualifier is always the name, never the file's
/// path, so moving a file breaks no reference.
///
/// It mirrors the validator file schema's pattern,
/// `^[a-z0-9]+(?:-[a-z0-9]+)*/REQ-(?:[0-9]{3}|[1-9][0-9]{3,})$`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Getters, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QualifiedRequirementId {
    spec: SpecName,
    requirement: RequirementId,
}

/// The error returned when a string is not a valid
/// [QualifiedRequirementId].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`{0}` is not a valid qualified requirement ID")]
#[diagnostic(help(
    "expected a specification's name, a slash, and a requirement ID, e.g. `todo-api/REQ-002`"
))]
pub struct InvalidQualifiedId(String);

impl QualifiedRequirementId {
    pub fn new(spec: SpecName, requirement: RequirementId) -> Self {
        Self { spec, requirement }
    }
}

impl FromStr for QualifiedRequirementId {
    type Err = InvalidQualifiedId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidQualifiedId(value.to_owned());
        let (spec, requirement) = value.split_once('/').ok_or_else(invalid)?;
        let spec = SpecName::try_from(spec.to_owned()).map_err(|_| invalid())?;
        let requirement = requirement.parse().map_err(|_| invalid())?;
        Ok(Self::new(spec, requirement))
    }
}

impl TryFrom<String> for QualifiedRequirementId {
    type Error = InvalidQualifiedId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<QualifiedRequirementId> for String {
    fn from(id: QualifiedRequirementId) -> Self {
        id.to_string()
    }
}

impl Display for QualifiedRequirementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.spec, self.requirement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<QualifiedRequirementId, InvalidQualifiedId> {
        value.parse()
    }

    fn id(spec: &str, number: u32) -> QualifiedRequirementId {
        let spec = SpecName::try_from(spec.to_owned()).unwrap();
        QualifiedRequirementId::new(spec, RequirementId::new(number))
    }

    #[test]
    fn qualified_ids_parse() {
        assert_eq!(parse("todo-api/REQ-002"), Ok(id("todo-api", 2)));
        assert_eq!(parse("a/REQ-000"), Ok(id("a", 0)));
        assert_eq!(parse("v2-billing/REQ-1000"), Ok(id("v2-billing", 1000)));
        let parsed = parse("todo-api/REQ-042").unwrap();
        assert_eq!(parsed.spec().as_str(), "todo-api");
        assert_eq!(*parsed.requirement(), RequirementId::new(42));
    }

    #[test]
    fn invalid_spellings_are_rejected() {
        for value in [
            "",
            "/",
            "REQ-002",
            "todo-api",
            "todo-api/",
            "/REQ-002",
            "todo-api/REQ-2",
            "todo-api/REQ-0002",
            "todo-api/SEC-001",
            "todo-api/AC-001",
            "todo-api/req-002",
            "Todo-Api/REQ-002",
            "todo_api/REQ-002",
            "todo--api/REQ-002",
            "-todo/REQ-002",
            "todo-api/REQ-002/x",
            "todo-api//REQ-002",
            "todo-api\\REQ-002",
            "todo-api:REQ-002",
            " todo-api/REQ-002",
            "todo-api/REQ-002 ",
            "specs/todo-api.yaml/REQ-002",
        ] {
            assert_eq!(
                parse(value),
                Err(InvalidQualifiedId(value.to_owned())),
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_error_names_the_value_and_the_form() {
        let err = parse("todo-api/REQ-2").unwrap_err();
        assert_eq!(
            err.to_string(),
            "`todo-api/REQ-2` is not a valid qualified requirement ID"
        );
        assert_eq!(
            err.help().unwrap().to_string(),
            "expected a specification's name, a slash, and a requirement ID, e.g. \
             `todo-api/REQ-002`"
        );
    }

    #[test]
    fn display_round_trips() {
        for value in ["todo-api/REQ-002", "a/REQ-999", "b-2/REQ-1000"] {
            assert_eq!(parse(value).unwrap().to_string(), value);
        }
    }

    #[test]
    fn qualified_ids_round_trip_through_serde_as_strings() {
        let ids = vec![id("todo-api", 2), id("billing", 1000)];
        let json = serde_json::to_string(&ids).unwrap();
        assert_eq!(json, r#"["todo-api/REQ-002","billing/REQ-1000"]"#);
        let back: Vec<QualifiedRequirementId> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ids);
        assert_eq!(String::from(id("todo-api", 2)), "todo-api/REQ-002");
        assert_eq!(
            QualifiedRequirementId::try_from("todo-api/REQ-002".to_owned()),
            Ok(id("todo-api", 2))
        );
    }

    #[test]
    fn invalid_ids_fail_to_deserialize() {
        let err =
            serde_json::from_str::<QualifiedRequirementId>(r#""todo-api/REQ-2""#).unwrap_err();
        assert!(
            err.to_string()
                .contains("`todo-api/REQ-2` is not a valid qualified requirement ID"),
            "{err}"
        );
        assert!(serde_json::from_str::<QualifiedRequirementId>("2").is_err());
    }
}
