use std::{
    fmt::{self, Display},
    sync::Arc,
};

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode, SourceSpan};
use serde::Serialize;
use thiserror::Error;

use crate::project::index::{UnknownCriterion, UnresolvedRequirement};
use crate::tools::{ToolKey, manifest::NotDeclared};

/// One way a validator breaks an invariant of the validator schema's
/// `x-vetInvariants`.
///
/// It serializes like a [crate::project::Problem]: an object whose `code`
/// names the problem in kebab-case, alongside the variant's fields.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq, Serialize)]
#[serde(
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Problem {
    /// `with`, or an empty object if it is omitted, fails the validator
    /// schema of the descriptor its tool pins.
    #[error("{} does not satisfy the validator schema of {tool}: {reason}", at(.pointer))]
    #[diagnostic(help("`slcr tools show {tool}` prints the schema"))]
    InvalidWith {
        tool: ToolKey,
        /// Where in `with` the schema fails, as a JSON Pointer; empty for
        /// `with` itself.
        pointer: String,
        reason: String,
    },

    #[error("it is identical to validator {first}")]
    #[diagnostic(help("remove one of them: identical validators check the same thing twice"))]
    DuplicateValidator {
        /// The position of the first validator it repeats, from 1.
        first: usize,
    },

    // These serialize with codes of their own.
    #[serde(untagged)]
    #[error(transparent)]
    #[diagnostic(transparent)]
    Requirement(UnresolvedRequirement),

    #[serde(untagged)]
    #[error(transparent)]
    #[diagnostic(transparent)]
    Criterion(UnknownCriterion),

    #[serde(untagged)]
    #[error(transparent)]
    #[diagnostic(transparent)]
    Tool(NotDeclared),
}

/// Where a schema error is: `with`, or a place inside it.
fn at(pointer: &str) -> String {
    if pointer.is_empty() {
        "`with`".to_owned()
    } else {
        format!("`with` at `{pointer}`")
    }
}

/// A [Problem] with one validator of a validator file, which it locates.
#[derive(Debug)]
pub(crate) struct Finding {
    /// The validator's position in the file's `validator` array, from 1.
    validator: usize,
    problem: Problem,
    /// What in the file's source to label, if it is known.
    span: Option<SourceSpan>,
    source: Arc<NamedSource<String>>,
}

impl Finding {
    pub(crate) fn new(
        validator: usize,
        problem: Problem,
        span: Option<SourceSpan>,
        source: Arc<NamedSource<String>>,
    ) -> Self {
        Self {
            validator,
            problem,
            span,
            source,
        }
    }

    /// The validator's position in the file, from 1.
    pub(crate) fn validator(&self) -> usize {
        self.validator
    }

    pub(crate) fn problem(&self) -> &Problem {
        &self.problem
    }
}

impl Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "validator {}: {}", self.validator, self.problem)
    }
}

impl std::error::Error for Finding {}

impl Diagnostic for Finding {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        self.problem.help()
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.span.map(|_| self.source.as_ref() as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span?;
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            None, span,
        ))))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::terminal::render_plain;

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    #[test]
    fn schema_errors_say_where_they_are() {
        let problem = |pointer: &str| Problem::InvalidWith {
            tool: key("cargo-nextest"),
            pointer: pointer.to_owned(),
            reason: "\"\" is shorter than 1 character".to_owned(),
        };
        assert_eq!(
            problem("/test").to_string(),
            "`with` at `/test` does not satisfy the validator schema of cargo-nextest: \"\" is \
             shorter than 1 character"
        );
        assert_eq!(
            problem("").to_string(),
            "`with` does not satisfy the validator schema of cargo-nextest: \"\" is shorter \
             than 1 character"
        );
        assert_eq!(
            problem("").help().unwrap().to_string(),
            "`slcr tools show cargo-nextest` prints the schema"
        );
    }

    #[test]
    fn problems_serialize_with_a_code_and_their_fields() {
        let problems = [
            Problem::InvalidWith {
                tool: key("jev"),
                pointer: "/x".to_owned(),
                reason: "1 is not of type \"string\"".to_owned(),
            },
            Problem::DuplicateValidator { first: 1 },
        ];
        assert_eq!(
            serde_json::to_value(problems).unwrap(),
            json!([
                {
                    "code": "invalid-with",
                    "tool": "jev",
                    "pointer": "/x",
                    "reason": "1 is not of type \"string\""
                },
                { "code": "duplicate-validator", "first": 1 }
            ])
        );
    }

    #[test]
    fn a_finding_names_its_validator_and_labels_it() {
        let source = Arc::new(NamedSource::new(
            "a/.validators.slcr.toml",
            "formatVersion = \"1\"\n[[validator]]\n".to_owned(),
        ));
        let finding = Finding::new(
            2,
            Problem::DuplicateValidator { first: 1 },
            Some(SourceSpan::from((20, 13))),
            source.clone(),
        );
        assert_eq!(
            finding.to_string(),
            "validator 2: it is identical to validator 1"
        );
        assert_eq!(
            render_plain(&finding),
            "  × validator 2: it is identical to validator 1\n   \
             ╭─[a/.validators.slcr.toml:2:1]\n \
             1 │ formatVersion = \"1\"\n \
             2 │ [[validator]]\n   \
             · ─────────────\n   \
             ╰────\n  \
             help: remove one of them: identical validators check the same thing twice\n"
        );
        let unlocated = Finding::new(1, Problem::DuplicateValidator { first: 1 }, None, source);
        assert!(unlocated.source_code().is_none());
        assert!(unlocated.labels().is_none());
    }
}
