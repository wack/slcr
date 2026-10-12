use std::path::{Path, PathBuf};

use miette::Diagnostic;
use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::spec::text::SpecName;
use crate::tools::pinned::DescriptorProblem;

/// Something wrong with the project as a whole, rather than inside one
/// of its files: a `specs` pattern that can't be honored, two
/// specifications with the same name, two validator files in one
/// directory, or a declared tool whose descriptor can't be used. Paths are
/// relative to the working directory.
///
/// It serializes like a [crate::spec::vet::Violation]: an object whose
/// `code` names the problem in kebab-case, alongside the variant's fields.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq, Serialize)]
#[serde(
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Problem {
    #[error("`specs` pattern `{pattern}` is not a valid glob: {reason}")]
    #[diagnostic(help(
        "`*` and `?` match within one path segment, `**` across segments, `[...]` one \
         character of a class, and `{{a,b}}` either alternative"
    ))]
    InvalidPattern { pattern: String, reason: String },

    /// A directory the walk couldn't read, which might hold a match.
    #[error("could not search {}: {reason}", .path.display())]
    Unsearchable {
        #[serde(serialize_with = "display")]
        path: PathBuf,
        reason: String,
    },

    #[error("{} the validator file {}", matched_by(.patterns), .path.display())]
    #[diagnostic(help(
        "`specs` names specification files only; narrow the pattern so that it skips \
         validator files, e.g. by starting the file name with `[!.]`"
    ))]
    MatchesValidatorFile {
        patterns: Vec<String>,
        #[serde(serialize_with = "display")]
        path: PathBuf,
    },

    #[error(
        "{} {}, which is not a YAML, JSON, or TOML file",
        matched_by(.patterns),
        .path.display()
    )]
    #[diagnostic(help(
        "a specification file's name must end in `.yaml`, `.yml`, `.json`, or `.toml`"
    ))]
    UnsupportedFormat {
        patterns: Vec<String>,
        #[serde(serialize_with = "display")]
        path: PathBuf,
    },

    #[error("`specs` pattern `{pattern}` matches no file")]
    #[diagnostic(help(
        "check it for typos; a pattern never matches a file that a `.gitignore` ignores"
    ))]
    NoMatch { pattern: String },

    #[error(
        "{} and {} both name their specification `{name}`",
        .first.display(),
        .second.display()
    )]
    #[diagnostic(help(
        "a specification's name qualifies references to its requirements, such as \
         `{name}/REQ-001`, so it must be unique in the project: change one `spec.name`"
    ))]
    DuplicateSpecName {
        name: SpecName,
        #[serde(serialize_with = "display")]
        first: PathBuf,
        #[serde(serialize_with = "display")]
        second: PathBuf,
    },

    #[error(
        "{} and {} are validator files in the same directory",
        .first.display(),
        .second.display()
    )]
    #[diagnostic(help(
        "a directory holds at most one validator file: move one's validators into the other, \
         and delete it"
    ))]
    DuplicateValidatorFile {
        #[serde(serialize_with = "display")]
        first: PathBuf,
        #[serde(serialize_with = "display")]
        second: PathBuf,
    },

    /// It serializes with a code of its own.
    #[serde(untagged)]
    #[error(transparent)]
    #[diagnostic(transparent)]
    Descriptor(DescriptorProblem),
}

/// The subject and verb of a message about what `patterns` match:
/// "`specs` pattern `a` matches", or "`specs` patterns `a` and `b` match".
fn matched_by(patterns: &[String]) -> String {
    let quoted: Vec<String> = patterns
        .iter()
        .map(|pattern| format!("`{pattern}`"))
        .collect();
    match quoted.as_slice() {
        [one] => format!("`specs` pattern {one} matches"),
        [first, second] => format!("`specs` patterns {first} and {second} match"),
        [rest @ .., last] => format!("`specs` patterns {}, and {last} match", rest.join(", ")),
        [] => "`specs` matches".to_owned(),
    }
}

/// Serialize a path as it displays, so that a path that isn't UTF-8 can't
/// fail serialization.
pub(crate) fn display<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&path.display())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn patterns(patterns: &[&str]) -> Vec<String> {
        patterns
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect()
    }

    #[test]
    fn messages_name_every_pattern() {
        let problem = |list: &[&str]| Problem::MatchesValidatorFile {
            patterns: patterns(list),
            path: PathBuf::from("docs/.validators.slcr.toml"),
        };
        assert_eq!(
            problem(&["docs/**"]).to_string(),
            "`specs` pattern `docs/**` matches the validator file docs/.validators.slcr.toml"
        );
        assert_eq!(
            problem(&["docs/**", "**/*.toml"]).to_string(),
            "`specs` patterns `docs/**` and `**/*.toml` match the validator file \
             docs/.validators.slcr.toml"
        );
        assert_eq!(
            problem(&["a/**", "b/**", "c/**"]).to_string(),
            "`specs` patterns `a/**`, `b/**`, and `c/**` match the validator file \
             docs/.validators.slcr.toml"
        );
        let unsupported = Problem::UnsupportedFormat {
            patterns: patterns(&["docs/*"]),
            path: PathBuf::from("docs/notes.md"),
        };
        assert_eq!(
            unsupported.to_string(),
            "`specs` pattern `docs/*` matches docs/notes.md, which is not a YAML, JSON, or \
             TOML file"
        );
    }

    #[test]
    fn duplicate_names_name_both_files() {
        let problem = Problem::DuplicateSpecName {
            name: SpecName::try_from("todo-api".to_owned()).unwrap(),
            first: PathBuf::from("a.yaml"),
            second: PathBuf::from("../b.json"),
        };
        assert_eq!(
            problem.to_string(),
            "a.yaml and ../b.json both name their specification `todo-api`"
        );
        assert_eq!(
            problem.help().unwrap().to_string(),
            "a specification's name qualifies references to its requirements, such as \
             `todo-api/REQ-001`, so it must be unique in the project: change one `spec.name`"
        );
    }

    #[test]
    fn problems_serialize_with_a_code_and_their_fields() {
        let problems = [
            Problem::InvalidPattern {
                pattern: "docs/[a".to_owned(),
                reason: "unclosed character class; missing ']'".to_owned(),
            },
            Problem::Unsearchable {
                path: PathBuf::from("private"),
                reason: "Permission denied (os error 13)".to_owned(),
            },
            Problem::MatchesValidatorFile {
                patterns: patterns(&["**"]),
                path: PathBuf::from(".validators.slcr.yml"),
            },
            Problem::UnsupportedFormat {
                patterns: patterns(&["*"]),
                path: PathBuf::from("notes.md"),
            },
            Problem::NoMatch {
                pattern: "spec.yml".to_owned(),
            },
            Problem::DuplicateSpecName {
                name: SpecName::try_from("todo-api".to_owned()).unwrap(),
                first: PathBuf::from("a.yaml"),
                second: PathBuf::from("b.yaml"),
            },
        ];
        assert_eq!(
            serde_json::to_value(problems).unwrap(),
            json!([
                {
                    "code": "invalid-pattern",
                    "pattern": "docs/[a",
                    "reason": "unclosed character class; missing ']'"
                },
                {
                    "code": "unsearchable",
                    "path": "private",
                    "reason": "Permission denied (os error 13)"
                },
                { "code": "matches-validator-file", "patterns": ["**"], "path": ".validators.slcr.yml" },
                { "code": "unsupported-format", "patterns": ["*"], "path": "notes.md" },
                { "code": "no-match", "pattern": "spec.yml" },
                { "code": "duplicate-spec-name", "name": "todo-api", "first": "a.yaml", "second": "b.yaml" }
            ])
        );
    }
}
