use std::fmt::{self, Display};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A node's human-facing name, which becomes a heading when rendered. It is
/// a single line with no leading or trailing whitespace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Title(String);

/// The error returned when a string is not a valid [Title].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{0:?} is not a valid title")]
#[diagnostic(help("a title is a single, non-empty line with no leading or trailing whitespace"))]
pub struct InvalidTitle(String);

impl Title {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Title {
    type Error = InvalidTitle;

    /// Mirrors the schema's pattern, `^\S(?:[^\r\n]*\S)?$`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bounded = match (value.chars().next(), value.chars().next_back()) {
            (Some(first), Some(last)) => !is_ecma_whitespace(first) && !is_ecma_whitespace(last),
            _ => false,
        };
        if bounded && !value.contains(['\r', '\n']) {
            Ok(Self(value))
        } else {
            Err(InvalidTitle(value))
        }
    }
}

impl From<Title> for String {
    fn from(title: Title) -> Self {
        title.0
    }
}

impl Display for Title {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `c` matches `\s` in the ECMA-262 regular expressions that JSON
/// Schema patterns use. That set is Unicode's `White_Space` property, which
/// [char::is_whitespace] tests, minus NEL (U+0085) and plus the byte order
/// mark (U+FEFF).
fn is_ecma_whitespace(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// Non-empty, Markdown-formatted prose: a body, rationale, or definition.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Markdown(String);

/// The error returned when Markdown prose is empty.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("Markdown text must not be empty")]
#[diagnostic(help("omit an optional field instead of leaving it empty"))]
pub struct EmptyMarkdown;

impl Markdown {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Markdown {
    type Error = EmptyMarkdown;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            Err(EmptyMarkdown)
        } else {
            Ok(Self(value))
        }
    }
}

impl From<Markdown> for String {
    fn from(markdown: Markdown) -> Self {
        markdown.0
    }
}

impl Display for Markdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A specification's name, such as `todo-api`: lowercase ASCII letters and
/// digits in dash-separated words. It qualifies references to the
/// specification's nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpecName(String);

/// The error returned when a string is not a valid [SpecName].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{0:?} is not a valid specification name")]
#[diagnostic(help(
    "a specification name is lowercase letters and digits in dash-separated words, e.g. `todo-api`"
))]
pub struct InvalidSpecName(String);

impl SpecName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SpecName {
    type Error = InvalidSpecName;

    /// Mirrors the schema's pattern, `^[a-z0-9]+(?:-[a-z0-9]+)*$`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidSpecName(value))
        }
    }
}

impl From<SpecName> for String {
    fn from(name: SpecName) -> Self {
        name.0
    }
}

impl Display for SpecName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(value: &str) -> Result<Title, InvalidTitle> {
        Title::try_from(value.to_owned())
    }

    fn spec_name(value: &str) -> Result<SpecName, InvalidSpecName> {
        SpecName::try_from(value.to_owned())
    }

    #[test]
    fn single_line_titles_are_valid() {
        for value in ["X", "TodoList API", "Table todo_list_item exists", "a\tb"] {
            assert_eq!(title(value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn empty_titles_are_rejected() {
        assert_eq!(title(""), Err(InvalidTitle(String::new())));
    }

    #[test]
    fn titles_with_surrounding_whitespace_are_rejected() {
        for value in [
            " ",
            " leading",
            "trailing ",
            "\ttab",
            "nbsp\u{a0}",
            "\u{feff}bom",
        ] {
            assert!(title(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn multi_line_titles_are_rejected() {
        for value in ["two\nlines", "carriage\rreturn", "windows\r\nline"] {
            assert!(title(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn nel_is_not_whitespace_to_the_schema() {
        // ECMA-262's `\s` omits U+0085, so the schema accepts it at the edges.
        assert!(title("\u{85}edge\u{85}").is_ok());
    }

    #[test]
    fn titles_deserialize_with_validation() {
        let parsed: Title = serde_json::from_str(r#""Views""#).unwrap();
        assert_eq!(parsed.to_string(), "Views");
        let err = serde_json::from_str::<Title>(r#"" Views""#).unwrap_err();
        assert!(err.to_string().contains("is not a valid title"));
    }

    #[test]
    fn markdown_must_not_be_empty() {
        assert_eq!(Markdown::try_from(String::new()), Err(EmptyMarkdown));
        let body = Markdown::try_from("The request MUST be authenticated.".to_owned()).unwrap();
        assert_eq!(body.as_str(), "The request MUST be authenticated.");
    }

    #[test]
    fn markdown_may_span_lines_and_have_whitespace() {
        let body = " First paragraph.\n\nSecond paragraph.\n";
        assert_eq!(Markdown::try_from(body.to_owned()).unwrap().as_str(), body);
    }

    #[test]
    fn empty_markdown_fails_to_deserialize() {
        assert!(serde_json::from_str::<Markdown>(r#""""#).is_err());
    }

    #[test]
    fn kebab_case_spec_names_are_valid() {
        for value in ["todo-api", "api", "v2", "a-b-c", "2024-q3-plan"] {
            assert_eq!(spec_name(value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn malformed_spec_names_are_rejected() {
        for value in [
            "",
            "-",
            "Todo-api",
            "todo_api",
            "todo--api",
            "-todo",
            "todo-",
            "todo api",
            "tödo",
        ] {
            assert!(spec_name(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn values_serialize_as_plain_strings() {
        let name = spec_name("todo-api").unwrap();
        assert_eq!(serde_json::to_string(&name).unwrap(), r#""todo-api""#);
        let heading = title("Views").unwrap();
        assert_eq!(serde_json::to_string(&heading).unwrap(), r#""Views""#);
    }
}
