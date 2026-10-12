use std::{
    ffi::OsStr,
    fmt::{self, Display},
    path::Path,
    sync::Arc,
};

use derive_getters::Getters;
use miette::{Diagnostic, NamedSource, Result};
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::{Map, Value};
use thiserror::Error;

use super::locate::{Spans, locate};
use crate::fs::{ReadError, format::Format};
use crate::project::{ProjectFile, QualifiedRequirementId};
use crate::spec::{graph::FormatVersion, id::CriterionId, optional::some};
use crate::tools::ToolKey;

/// The names a validator file may have: one per format, which its
/// extension selects.
pub(crate) const FILE_NAMES: [&str; 4] = [
    ".validators.slcr.toml",
    ".validators.slcr.json",
    ".validators.slcr.yaml",
    ".validators.slcr.yml",
];

/// Whether the file at `path` is a validator file, by its name alone.
pub(crate) fn is_validator_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| FILE_NAMES.contains(&name))
}

/// A validator file's contents, as the validator schema describes them.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Contents {
    #[allow(dead_code)]
    format_version: FormatVersion,
    #[serde(deserialize_with = "at_least_one")]
    validator: Vec<Validator>,
}

/// One validator: a requirement, or one of its acceptance criteria, and
/// the tool that checks it. Validators have no names; a file's position in
/// its `validator` array identifies one.
#[derive(Clone, Debug, PartialEq, Getters, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Validator {
    /// The requirement it checks, qualified by its specification's name.
    requirement: QualifiedRequirementId,
    /// The acceptance criterion of the requirement it checks, if it checks
    /// one rather than the requirement as a whole.
    #[serde(default, deserialize_with = "some")]
    criterion: Option<CriterionId>,
    /// The key of the tool, in the manifest's `[tools]`, that runs it.
    tool: ToolKey,
    #[serde(default, deserialize_with = "some")]
    description: Option<Description>,
    /// The tool-specific fields, which the tool's validator schema checks.
    #[serde(default, deserialize_with = "with_table")]
    with: Option<Map<String, Value>>,
}

impl Validator {
    /// `with`, or an empty object if it is omitted: what the tool's
    /// validator schema checks.
    pub(crate) fn with_or_empty(&self) -> Value {
        Value::Object(self.with.clone().unwrap_or_default())
    }

    /// Whether `other` is the same validator once both are canonical: the
    /// same fields with the same values, whatever order their keys are
    /// written in, an omitted `with` being the same as an empty one.
    pub(crate) fn is_identical(&self, other: &Self) -> bool {
        let empty = Map::new();
        let with = |validator: &Self| validator.with.as_ref().unwrap_or(&empty).clone();
        self.requirement == other.requirement
            && self.criterion == other.criterion
            && self.tool == other.tool
            && self.description == other.description
            // JSON objects compare as maps, whatever their order.
            && with(self) == with(other)
    }
}

/// A validator's note for the people who read it, which is never empty.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Description(String);

/// The error returned when a string is not a valid [Description].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("a description must not be empty")]
#[diagnostic(help("omit `description` rather than leaving it empty"))]
pub struct EmptyDescription;

impl TryFrom<String> for Description {
    type Error = EmptyDescription;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            Err(EmptyDescription)
        } else {
            Ok(Self(value))
        }
    }
}

impl Description {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Description {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `validator` array, which must hold at least one validator.
fn at_least_one<'de, D>(deserializer: D) -> Result<Vec<Validator>, D::Error>
where
    D: Deserializer<'de>,
{
    let validators = Vec::deserialize(deserializer)?;
    if validators.is_empty() {
        return Err(D::Error::custom(
            "`validator` must hold at least one validator",
        ));
    }
    Ok(validators)
}

/// The key the `toml` crate gives the table it reads a date or time as.
const TOML_DATETIME: &str = "$__toml_private_datetime";

/// A present `with`, which is a table that holds no TOML date or time: a
/// validator schema is JSON Schema, which has none.
fn with_table<'de, D>(deserializer: D) -> Result<Option<Map<String, Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    fn holds_datetime(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key(TOML_DATETIME) || map.values().any(holds_datetime)
            }
            Value::Array(items) => items.iter().any(holds_datetime),
            _ => false,
        }
    }

    // Read as any value, not as a map, which a YAML null would pass as.
    let with = match Value::deserialize(deserializer)? {
        Value::Object(with) => with,
        other => {
            let kind = match other {
                Value::Null => "null",
                Value::Bool(_) => "a boolean",
                Value::Number(_) => "a number",
                Value::String(_) => "a string",
                Value::Array(_) => "an array",
                Value::Object(_) => unreachable!(),
            };
            return Err(D::Error::custom(format!(
                "`with` must be a table of the tool's fields, not {kind}"
            )));
        }
    };
    if with.values().any(holds_datetime) {
        return Err(D::Error::custom(
            "`with` holds a TOML date or time, which a validator schema can't check; write it \
             as a string",
        ));
    }
    Ok(Some(with))
}

/// A validator file, read and checked against the validator schema.
#[derive(Debug)]
pub(crate) struct ValidatorFile {
    file: ProjectFile,
    validators: Vec<Validator>,
    /// Where each validator is in the source, by position; empty if the
    /// source couldn't be mapped.
    spans: Vec<Spans>,
    /// The file's text, named by its [ProjectFile::shown] path, for
    /// diagnostics to label.
    source: Arc<NamedSource<String>>,
}

impl ValidatorFile {
    /// Read `file`, a validator file, in the format its extension names.
    /// Errors name it by its [ProjectFile::shown] path.
    pub(crate) fn load(file: ProjectFile) -> Result<Self> {
        let shown = file.shown();
        let format = file
            .relative()
            .extension()
            .and_then(OsStr::to_str)
            .and_then(Format::from_extension)
            .expect("a validator file's name has a format's extension");
        let text = std::fs::read_to_string(file.absolute())
            .map_err(|source| ReadError::new(shown.to_path_buf(), source))?;
        let contents: Contents = format.parse(shown, text.clone())?;
        let spans = locate(format, &text, contents.validator.len());
        let source = Arc::new(NamedSource::new(shown.display().to_string(), text));
        Ok(Self {
            file,
            validators: contents.validator,
            spans,
            source,
        })
    }

    pub(crate) fn file(&self) -> &ProjectFile {
        &self.file
    }

    /// The validators, in the order the file lists them.
    pub(crate) fn validators(&self) -> &[Validator] {
        &self.validators
    }

    /// Where the validator at `index`, counted from 0, is in the source,
    /// as far as is known.
    pub(crate) fn spans(&self, index: usize) -> Spans {
        self.spans.get(index).copied().unwrap_or_default()
    }

    pub(crate) fn source(&self) -> &Arc<NamedSource<String>> {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::fs::format::ParseError;

    /// The example on the Schemas page.
    pub(crate) const EXAMPLE_TOML: &str = r#"# src/views/items/.validators.slcr.toml
formatVersion = "1"

[[validator]]
requirement = "todo-api/REQ-002"
criterion = "AC-004"
tool = "ast-grep"
description = "POST handler is defined and registered with the router"
with = { rule = "rules/post-handler.yml" }

[[validator]]
requirement = "todo-api/REQ-002"
tool = "cargo-nextest"
with = { test = "items::post::creates_item" }
"#;

    /// The example, in JSON.
    const EXAMPLE_JSON: &str = r#"{
  "formatVersion": "1",
  "validator": [
    {
      "requirement": "todo-api/REQ-002",
      "criterion": "AC-004",
      "tool": "ast-grep",
      "description": "POST handler is defined and registered with the router",
      "with": { "rule": "rules/post-handler.yml" }
    },
    {
      "requirement": "todo-api/REQ-002",
      "tool": "cargo-nextest",
      "with": { "test": "items::post::creates_item" }
    }
  ]
}
"#;

    /// The example, in YAML.
    const EXAMPLE_YAML: &str = r#"formatVersion: "1"
validator:
  - requirement: todo-api/REQ-002
    criterion: AC-004
    tool: ast-grep
    description: POST handler is defined and registered with the router
    with:
      rule: rules/post-handler.yml
  - requirement: todo-api/REQ-002
    tool: cargo-nextest
    with: { test: "items::post::creates_item" }
"#;

    fn parse(format: Format, source: &str) -> Result<Contents, ParseError> {
        format.parse(Path::new(".validators.slcr"), source.to_owned())
    }

    /// The message of the error `source` fails to parse with.
    fn failure(format: Format, source: &str) -> String {
        parse(format, source).unwrap_err().message().to_owned()
    }

    #[test]
    fn the_schema_page_example_parses_in_every_format() {
        let sources = [
            (Format::Toml, EXAMPLE_TOML),
            (Format::Json, EXAMPLE_JSON),
            (Format::Yaml, EXAMPLE_YAML),
        ];
        for (format, source) in sources {
            let contents = parse(format, source).unwrap();
            let [first, second] = &contents.validator[..] else {
                panic!("{format}: {contents:?}");
            };
            assert_eq!(first.requirement().to_string(), "todo-api/REQ-002");
            assert_eq!(first.criterion().unwrap().to_string(), "AC-004");
            assert_eq!(first.tool().as_str(), "ast-grep");
            assert_eq!(
                first.description().as_ref().map(Description::as_str),
                Some("POST handler is defined and registered with the router")
            );
            assert_eq!(
                first.with_or_empty(),
                json!({ "rule": "rules/post-handler.yml" })
            );
            assert_eq!(*second.criterion(), None);
            assert_eq!(*second.description(), None);
            assert_eq!(second.tool().as_str(), "cargo-nextest");
            assert_eq!(
                second.with_or_empty(),
                json!({ "test": "items::post::creates_item" })
            );
        }
    }

    #[test]
    fn with_is_optional_and_checked_as_an_empty_object() {
        let source =
            "formatVersion = \"1\"\n[[validator]]\nrequirement = \"a/REQ-001\"\ntool = \"jev\"\n";
        let contents = parse(Format::Toml, source).unwrap();
        assert_eq!(*contents.validator[0].with(), None);
        assert_eq!(contents.validator[0].with_or_empty(), json!({}));
    }

    #[test]
    fn invalid_files_are_rejected() {
        let minimal =
            "formatVersion = \"1\"\n[[validator]]\nrequirement = \"a/REQ-001\"\ntool = \"jev\"\n";
        assert!(parse(Format::Toml, minimal).is_ok());
        let cases = [
            ("formatVersion = \"1\"\n", "missing field `validator`"),
            (
                "formatVersion = \"1\"\nvalidator = []\n",
                "`validator` must hold at least one validator",
            ),
            (
                "[[validator]]\nrequirement = \"a/REQ-001\"\ntool = \"jev\"\n",
                "missing field `formatVersion`",
            ),
            (
                &minimal.replace("\"1\"", "\"2\""),
                "unknown variant `2`, expected `1`",
            ),
            (
                &format!("{minimal}checks = 1\n"),
                "unknown field `checks`, expected one of",
            ),
            (
                &minimal.replace(
                    "formatVersion = \"1\"\n",
                    "formatVersion = \"1\"\nname = \"x\"\n",
                ),
                "unknown field `name`, expected `formatVersion` or `validator`",
            ),
            (
                &minimal.replace("tool = \"jev\"\n", ""),
                "missing field `tool`",
            ),
            (
                &minimal.replace("requirement = \"a/REQ-001\"\n", ""),
                "missing field `requirement`",
            ),
            (
                &minimal.replace("a/REQ-001", "REQ-001"),
                "`REQ-001` is not a valid qualified requirement ID",
            ),
            (
                &minimal.replace("a/REQ-001", "a/REQ-1"),
                "`a/REQ-1` is not a valid qualified requirement ID",
            ),
            (
                &format!("{minimal}criterion = \"AC-4\"\n"),
                "`AC-4` is not a valid acceptance criterion ID",
            ),
            (
                &format!("{minimal}criterion = \"REQ-004\"\n"),
                "`REQ-004` is not a valid acceptance criterion ID",
            ),
            (
                &minimal.replace("\"jev\"", "\"Jev\""),
                "\"Jev\" is not a valid tool name",
            ),
            (
                &format!("{minimal}description = \"\"\n"),
                "a description must not be empty",
            ),
            (
                &format!("{minimal}with = 1\n"),
                "`with` must be a table of the tool's fields, not a number",
            ),
            (
                &format!("{minimal}with = [\"a\"]\n"),
                "`with` must be a table of the tool's fields, not an array",
            ),
            (
                &format!("{minimal}with = {{ since = 1979-05-27 }}\n"),
                "`with` holds a TOML date or time, which a validator schema can't check; write it \
                 as a string",
            ),
            (
                &format!("{minimal}with = {{ at = [{{ when = 07:32:00 }}] }}\n"),
                "`with` holds a TOML date or time, which a validator schema can't check; write it \
                 as a string",
            ),
        ];
        for (source, message) in cases {
            let failure = failure(Format::Toml, source);
            assert!(failure.starts_with(message), "{source}\n{failure}");
        }
    }

    #[test]
    fn present_optional_fields_must_have_values() {
        let minimal =
            r#"{"formatVersion": "1", "validator": [{"requirement": "a/REQ-001", "tool": "jev"}]}"#;
        assert!(parse(Format::Json, minimal).is_ok());
        for field in ["criterion", "description", "with"] {
            let source = minimal.replace("\"tool\"", &format!("\"{field}\": null, \"tool\""));
            assert!(parse(Format::Json, &source).is_err(), "{field}");
        }
        let yaml = "formatVersion: \"1\"\nvalidator:\n  - requirement: a/REQ-001\n    tool: jev\n    with:\n";
        assert!(parse(Format::Yaml, yaml).is_err());
    }

    #[test]
    fn errors_are_located_in_every_format() {
        let sources = [
            (Format::Toml, EXAMPLE_TOML.replace("AC-004", "AC-4")),
            (Format::Json, EXAMPLE_JSON.replace("AC-004", "AC-4")),
            (Format::Yaml, EXAMPLE_YAML.replace("AC-004", "AC-4")),
        ];
        for (format, source) in sources {
            let err = parse(format, &source).unwrap_err();
            let span = err.span().expect("a location");
            let line = source[..span.offset()].lines().count();
            let expected = source
                .lines()
                .position(|line| line.contains("AC-4"))
                .unwrap();
            // JSON reports where it stopped reading, at the end of the value.
            assert_eq!(line.max(1) - 1, expected, "{format}: {span:?}");
        }
    }

    #[test]
    fn identical_validators_are_the_same_whatever_their_order() {
        let source = r#"{"formatVersion": "1", "validator": [
            {"requirement": "a/REQ-001", "tool": "jev", "with": {"x": 1, "y": [2, {"z": 3, "w": 4}]}},
            {"with": {"y": [2, {"w": 4, "z": 3}], "x": 1}, "tool": "jev", "requirement": "a/REQ-001"},
            {"requirement": "a/REQ-001", "tool": "jev", "with": {"x": 1, "y": [{"z": 3, "w": 4}, 2]}},
            {"requirement": "a/REQ-001", "tool": "jev"},
            {"requirement": "a/REQ-001", "tool": "jev", "with": {}},
            {"requirement": "a/REQ-001", "tool": "jev", "description": "x"},
            {"requirement": "a/REQ-001", "tool": "jev", "criterion": "AC-001"},
            {"requirement": "a/REQ-001", "tool": "curl"},
            {"requirement": "a/REQ-002", "tool": "jev"}
        ]}"#;
        let validators = parse(Format::Json, source).unwrap().validator;
        let identical: Vec<(usize, usize)> = (0..validators.len())
            .flat_map(|i| (0..i).map(move |j| (j, i)))
            .filter(|&(j, i)| validators[i].is_identical(&validators[j]))
            .collect();
        // Arrays keep their order; an empty `with` is an omitted one.
        assert_eq!(identical, [(0, 1), (3, 4)]);
    }

    #[test]
    fn validator_file_names_are_recognized() {
        for name in FILE_NAMES {
            assert!(is_validator_file(&Path::new("docs").join(name)), "{name}");
        }
        for name in [
            "validators.slcr.toml",
            ".validators.slcr.md",
            ".validators.toml",
            "a.validators.slcr.toml",
            ".validators.slcr.TOML",
        ] {
            assert!(!is_validator_file(Path::new(name)), "{name}");
        }
    }

    /// `files`, written in a temporary directory, as project files of it.
    fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<ProjectFile>) {
        let dir = tempfile::tempdir().unwrap();
        let files = files
            .iter()
            .map(|(path, contents)| {
                let absolute = dir.path().join(path);
                std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
                std::fs::write(&absolute, contents).unwrap();
                ProjectFile::new(dir.path(), dir.path(), PathBuf::from(path))
            })
            .collect();
        (dir, files)
    }

    #[test]
    fn a_file_loads_in_the_format_its_extension_names() {
        let (_dir, files) = project(&[
            ("a/.validators.slcr.toml", EXAMPLE_TOML),
            ("b/.validators.slcr.json", EXAMPLE_JSON),
            ("c/.validators.slcr.yaml", EXAMPLE_YAML),
            ("d/.validators.slcr.yml", EXAMPLE_YAML),
        ]);
        for file in files {
            let shown = file.shown().display().to_string();
            let loaded = ValidatorFile::load(file).unwrap();
            assert_eq!(loaded.validators().len(), 2, "{shown}");
            assert_eq!(loaded.source().name(), shown);
            assert!(loaded.spans(1).requirement.is_some(), "{shown}");
            assert_eq!(loaded.spans(2), Spans::default());
        }
    }

    #[test]
    fn errors_name_the_file_as_it_is_shown() {
        let (_dir, files) = project(&[("a/.validators.slcr.yml", "validator: [\n")]);
        let err = ValidatorFile::load(files[0].clone()).unwrap_err();
        let parse = err.downcast_ref::<ParseError>().expect("a parse error");
        assert_eq!(
            parse.to_string(),
            "could not parse a/.validators.slcr.yml as YAML: unclosed bracket '['"
        );
        let missing = ProjectFile::new(
            Path::new("/p"),
            Path::new("/p/a"),
            PathBuf::from("b/.validators.slcr.json"),
        );
        let err = ValidatorFile::load(missing).unwrap_err();
        assert_eq!(err.to_string(), "could not read ../b/.validators.slcr.json");
    }
}
