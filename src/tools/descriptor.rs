use std::{
    collections::{BTreeMap, HashSet},
    fmt::{self, Display},
    num::NonZeroU64,
    path::Path,
};

use derive_getters::Getters;
use miette::Diagnostic;
use serde::{
    Deserialize, Deserializer,
    de::{self, Error as _, MapAccess, Visitor},
};
use serde_json::Value;
use thiserror::Error;

use super::{DescriptorVersion, ToolKey};
use crate::fs::format::{Format, ParseError};
use crate::spec::graph::FormatVersion;

/// A tool descriptor: how SLCR runs one external tool, and how it reads
/// the outcome. Descriptors are TOML, purely declarative, and never
/// executable.
///
/// Every descriptor satisfies the descriptor schema and its
/// `x-vetInvariants` that concern the descriptor alone: its validator
/// schema is a valid, self-contained JSON Schema, and every `{with.<field>}`
/// placeholder names a required scalar field of it. Read one with
/// [Descriptor::parse].
#[derive(Clone, Debug, PartialEq, Getters)]
pub(crate) struct Descriptor {
    /// The default manifest key.
    name: ToolKey,
    version: DescriptorVersion,
    /// The JSON Schema every validator entry's `with` table must satisfy.
    schema: Value,
    invoke: Invoke,
    results: Results,
}

/// A descriptor as written, before the checks its schema can't express.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawDescriptor {
    #[allow(dead_code)]
    format_version: FormatVersion,
    name: ToolKey,
    version: DescriptorVersion,
    validator: ValidatorSection,
    invoke: Invoke,
    #[serde(default)]
    results: Results,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatorSection {
    schema: toml::Table,
}

/// How SLCR runs one validator: directly, never through a shell, with
/// stdin closed.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Invoke {
    argv: Argv,
    cwd: Cwd,
    /// Literal values that set or override variables in the environment
    /// the child inherits.
    #[serde(default, deserialize_with = "non_empty_map")]
    env: BTreeMap<EnvName, String>,
    /// Seconds before the run is killed, which makes the validator red.
    /// `None` means no timeout.
    #[serde(default)]
    timeout: Option<NonZeroU64>,
}

/// How a run's outcome becomes green or red.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Results {
    /// The exit codes that mean green. Any other is red.
    #[serde(default = "default_pass", deserialize_with = "non_empty_set")]
    pass: Vec<u8>,
}

fn default_pass() -> Vec<u8> {
    vec![0]
}

impl Default for Results {
    fn default() -> Self {
        Self {
            pass: default_pass(),
        }
    }
}

/// A map with at least one entry.
fn non_empty_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    let map = BTreeMap::deserialize(deserializer)?;
    if map.is_empty() {
        return Err(D::Error::custom(
            "an empty table must be omitted rather than written as `{}`",
        ));
    }
    Ok(map)
}

/// An array with at least one element, and no element repeated.
fn non_empty_set<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let items = Vec::<u8>::deserialize(deserializer)?;
    if items.is_empty() {
        return Err(D::Error::custom("at least one exit code must pass"));
    }
    let mut seen = HashSet::with_capacity(items.len());
    if let Some(repeated) = items.iter().find(|item| !seen.insert(**item)) {
        return Err(D::Error::custom(format!(
            "exit code {repeated} appears more than once"
        )));
    }
    Ok(items)
}

/// The argument vector: a bare program name, then argument templates.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Deserialize)]
#[serde(try_from = "Vec<String>")]
pub(crate) struct Argv {
    program: ProgramName,
    args: Vec<Template>,
}

impl TryFrom<Vec<String>> for Argv {
    type Error = String;

    fn try_from(argv: Vec<String>) -> Result<Self, Self::Error> {
        let mut argv = argv.into_iter();
        let program = argv
            .next()
            .ok_or("`argv` must name a program")?
            .try_into()?;
        let args = argv
            .enumerate()
            .map(|(index, arg)| {
                Template::parse(&arg).map_err(|problem| {
                    // `argv[0]` is the program.
                    format!("argv[{}], {arg:?}: {problem}", index + 1)
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { program, args })
    }
}

/// A bare program name, such as `cargo`, resolved through `PATH` when a
/// validator runs: never a path, and never a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramName(String);

impl TryFrom<String> for ProgramName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && !value.contains(['/', '\\', '{', '}', '\0'])
            && value != "."
            && value != "..";
        if valid {
            Ok(Self(value))
        } else {
            Err(format!(
                "the program, {value:?}, must be a bare name such as `cargo`: \
                 no path separators and no placeholders"
            ))
        }
    }
}

impl ProgramName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One argument, with placeholders that are substituted inside it. A
/// substitution never creates, splits, or joins arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template(Vec<Segment>);

/// A piece of a [Template].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// Text, with `{{` and `}}` already unescaped.
    Literal(String),
    /// `{with.<field>}`: a top-level scalar field of the validator entry's
    /// `with` table.
    With(String),
    /// `{slcr.<name>}`: a value SLCR provides.
    Builtin(Builtin),
}

/// The values SLCR provides to templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// `{slcr.project_root}`: the directory that holds `SLCR.toml`.
    ProjectRoot,
    /// `{slcr.validator_dir}`: the directory that holds the validator file.
    ValidatorDir,
    /// `{slcr.requirement}`: the qualified requirement ID, such as
    /// `todo-api/REQ-002`.
    Requirement,
}

impl Template {
    pub(crate) fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// Mirrors the schema's `template` pattern: the only braces are escapes
    /// and well-formed placeholders.
    fn parse(value: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut rest = value;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            match c {
                '{' if rest.starts_with('{') => {
                    literal.push('{');
                    rest = &rest[1..];
                }
                '}' if rest.starts_with('}') => {
                    literal.push('}');
                    rest = &rest[1..];
                }
                '{' => {
                    let end = rest
                        .find('}')
                        .ok_or("a `{` opens a placeholder that is never closed; write `{{` for a literal brace")?;
                    let placeholder = Segment::placeholder(&rest[..end])?;
                    if !literal.is_empty() {
                        segments.push(Segment::Literal(std::mem::take(&mut literal)));
                    }
                    segments.push(placeholder);
                    rest = &rest[end + 1..];
                }
                '}' => return Err("a lone `}`; write `}}` for a literal brace".to_owned()),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }
        Ok(Self(segments))
    }

    /// The `with` fields this template substitutes.
    fn with_fields(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|segment| match segment {
            Segment::With(field) => Some(field.as_str()),
            _ => None,
        })
    }
}

impl Segment {
    /// The placeholder whose name is `name`, the text between its braces.
    fn placeholder(name: &str) -> Result<Self, String> {
        if let Some(field) = name.strip_prefix("with.") {
            let mut chars = field.chars();
            let valid = chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if valid {
                return Ok(Self::With(field.to_owned()));
            }
            return Err(format!("`{{{name}}}` does not name a `with` field"));
        }
        match name {
            "slcr.project_root" => Ok(Self::Builtin(Builtin::ProjectRoot)),
            "slcr.validator_dir" => Ok(Self::Builtin(Builtin::ValidatorDir)),
            "slcr.requirement" => Ok(Self::Builtin(Builtin::Requirement)),
            _ => Err(format!(
                "`{{{name}}}` is not a placeholder; placeholders are `{{with.<field>}}`, \
                 `{{slcr.project_root}}`, `{{slcr.validator_dir}}`, and `{{slcr.requirement}}`"
            )),
        }
    }
}

/// The working directory a validator runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cwd {
    /// `"project"`: the project root.
    Project,
    /// `"validator-dir"`: the directory that holds the validator file.
    ValidatorDir,
    /// `{ nearest = "<file>" }`: the closest ancestor of the validator
    /// file's directory, no higher than the project root, that holds the
    /// named file.
    Nearest(FileName),
}

impl<'de> Deserialize<'de> for Cwd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CwdVisitor;

        impl<'de> Visitor<'de> for CwdVisitor {
            type Value = Cwd;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(r#""project", "validator-dir", or { nearest = "<file>" }"#)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Cwd, E> {
                match value {
                    "project" => Ok(Cwd::Project),
                    "validator-dir" => Ok(Cwd::ValidatorDir),
                    _ => Err(E::invalid_value(de::Unexpected::Str(value), &self)),
                }
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Cwd, A::Error> {
                let mut nearest = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "nearest" {
                        return Err(A::Error::unknown_field(&key, &["nearest"]));
                    }
                    if nearest.is_some() {
                        return Err(A::Error::duplicate_field("nearest"));
                    }
                    nearest = Some(map.next_value::<FileName>()?);
                }
                nearest
                    .map(Cwd::Nearest)
                    .ok_or_else(|| A::Error::missing_field("nearest"))
            }
        }

        deserializer.deserialize_any(CwdVisitor)
    }
}

/// A single file name, such as `Cargo.toml`: not a path.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct FileName(String);

impl TryFrom<String> for FileName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && !value.contains(['/', '\\', '\0'])
            && value != "."
            && value != "..";
        if valid {
            Ok(Self(value))
        } else {
            Err(format!("{value:?} is not a file name"))
        }
    }
}

impl FileName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name of an environment variable: `^[A-Za-z_][A-Za-z0-9_]*$`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct EnvName(String);

impl TryFrom<String> for EnvName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut chars = value.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            Ok(Self(value))
        } else {
            Err(format!("{value:?} is not an environment variable name"))
        }
    }
}

impl EnvName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The error returned when bytes are not a valid descriptor.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum InvalidDescriptor {
    #[error("{origin} is not UTF-8 text")]
    #[diagnostic(help("a descriptor is a TOML file"))]
    NotUtf8 { origin: String },

    #[error(transparent)]
    #[diagnostic(transparent)]
    Unparsable(Box<ParseError>),

    #[error("{origin} is not a valid descriptor")]
    Broken {
        origin: String,
        #[related]
        problems: Vec<Problem>,
    },
}

/// One way a descriptor breaks an invariant its schema can't express.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum Problem {
    #[error("`validator.schema` is not a valid JSON Schema: {0}")]
    #[diagnostic(help(
        "the validator schema is JSON Schema (draft 2020-12), written as TOML, and \
         every `$ref` in it must resolve within the schema itself"
    ))]
    InvalidSchema(String),

    #[error("`validator.schema` must have `type = \"object\"`")]
    #[diagnostic(help("the validator schema validates a validator entry's `with` table"))]
    NotAnObjectSchema,

    #[error("`validator.schema` holds a TOML date or time, which JSON Schema can't")]
    Datetime,

    #[error("`{{with.{field}}}` in `invoke.argv` does not name a field of `validator.schema`")]
    #[diagnostic(help(
        "declare `{field}` in the validator schema's top-level `properties`, and list it in `required`"
    ))]
    UndeclaredField { field: String },

    #[error("`{{with.{field}}}` in `invoke.argv` names a field `validator.schema` doesn't require")]
    #[diagnostic(help(
        "list `{field}` in the validator schema's `required`: every placeholder needs a value"
    ))]
    OptionalField { field: String },

    #[error("`{{with.{field}}}` in `invoke.argv` names a field that is not a scalar")]
    #[diagnostic(help(
        "give `{field}` a `type` of \"string\", \"number\", \"integer\", or \"boolean\": \
         only scalars can be substituted into an argument"
    ))]
    NonScalarField { field: String },
}

/// The text of the descriptor in `bytes`, which came from `origin`: the
/// path or URL to name in errors. A descriptor is TOML, so it is UTF-8.
pub(crate) fn text<'a>(origin: &str, bytes: &'a [u8]) -> Result<&'a str, InvalidDescriptor> {
    std::str::from_utf8(bytes).map_err(|_| InvalidDescriptor::NotUtf8 {
        origin: origin.to_owned(),
    })
}

impl Descriptor {
    /// Read the descriptor `source`, which came from `origin`: the path or
    /// URL to name in errors.
    pub(crate) fn parse(origin: &str, source: &str) -> Result<Self, InvalidDescriptor> {
        let raw: RawDescriptor = Format::Toml
            .parse(Path::new(origin), source.to_owned())
            .map_err(|err| InvalidDescriptor::Unparsable(Box::new(err)))?;
        let (schema, problems) = match to_json(toml::Value::Table(raw.validator.schema)) {
            Ok(schema) => {
                let problems = schema_problems(&schema, &raw.invoke.argv);
                (schema, problems)
            }
            Err(problem) => (Value::Null, vec![problem]),
        };
        if !problems.is_empty() {
            return Err(InvalidDescriptor::Broken {
                origin: origin.to_owned(),
                problems,
            });
        }
        Ok(Self {
            name: raw.name,
            version: raw.version,
            schema,
            invoke: raw.invoke,
            results: raw.results,
        })
    }
}

/// What is wrong with the validator `schema`, alone and as the source of
/// the fields `argv` substitutes.
fn schema_problems(schema: &Value, argv: &Argv) -> Vec<Problem> {
    let mut problems = Vec::new();
    if schema.get("type") != Some(&Value::from("object")) {
        problems.push(Problem::NotAnObjectSchema);
    }
    // Compiling the schema validates it against the 2020-12 meta-schema,
    // and fails on a `$ref` it can't resolve: with no retriever enabled,
    // that is any `$ref` outside the schema.
    if let Err(err) = jsonschema::draft202012::new(schema) {
        problems.push(Problem::InvalidSchema(err.to_string()));
    }
    let mut seen = HashSet::new();
    let fields = argv.args.iter().flat_map(Template::with_fields);
    for field in fields.filter(|field| seen.insert(*field)) {
        if let Some(problem) = field_problem(schema, field) {
            problems.push(problem);
        }
    }
    problems
}

/// Why `{with.<field>}` can't be substituted, if it can't: it must name a
/// required, top-level property of `schema` whose type is a scalar.
fn field_problem(schema: &Value, field: &str) -> Option<Problem> {
    let field_owned = || field.to_owned();
    let Some(property) = schema.get("properties").and_then(|p| p.get(field)) else {
        return Some(Problem::UndeclaredField {
            field: field_owned(),
        });
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|name| name == field));
    if !required {
        return Some(Problem::OptionalField {
            field: field_owned(),
        });
    }
    let scalar = property
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|ty| matches!(ty, "string" | "number" | "integer" | "boolean"));
    if !scalar {
        return Some(Problem::NonScalarField {
            field: field_owned(),
        });
    }
    None
}

/// `value` in JSON's data model, which TOML's maps onto except for dates
/// and times.
fn to_json(value: toml::Value) -> Result<Value, Problem> {
    Ok(match value {
        toml::Value::String(string) => Value::String(string),
        toml::Value::Integer(integer) => Value::from(integer),
        // TOML allows `nan` and `inf`, which JSON doesn't; they become
        // `null`, which no schema keyword accepts as a number.
        toml::Value::Float(float) => Value::from(float),
        toml::Value::Boolean(boolean) => Value::Bool(boolean),
        toml::Value::Datetime(_) => return Err(Problem::Datetime),
        toml::Value::Array(array) => {
            Value::Array(array.into_iter().map(to_json).collect::<Result<_, _>>()?)
        }
        toml::Value::Table(table) => Value::Object(
            table
                .into_iter()
                .map(|(key, value)| Ok((key, to_json(value)?)))
                .collect::<Result<_, _>>()?,
        ),
    })
}

impl Display for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ProjectRoot => "slcr.project_root",
            Self::ValidatorDir => "slcr.validator_dir",
            Self::Requirement => "slcr.requirement",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::render_plain;

    /// The cargo-nextest example on the Schemas page.
    const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

    fn parse(source: &str) -> Result<Descriptor, InvalidDescriptor> {
        Descriptor::parse("descriptor.toml", source)
    }

    /// The invariants `source` breaks.
    fn problems(source: &str) -> Vec<Problem> {
        match parse(source) {
            Err(InvalidDescriptor::Broken { problems, .. }) => problems,
            other => panic!("expected broken invariants, got {other:?}"),
        }
    }

    /// The plain rendering of the error `source` fails with.
    fn failure(source: &str) -> String {
        render_plain(&parse(source).unwrap_err())
    }

    #[test]
    fn the_schema_page_example_parses() {
        let descriptor = parse(NEXTEST).unwrap();
        assert_eq!(descriptor.name().as_str(), "cargo-nextest");
        assert_eq!(descriptor.version().to_string(), "1.0.0");
        assert_eq!(descriptor.invoke().argv().program().as_str(), "cargo");
        assert_eq!(
            descriptor.invoke().argv().args()[5].segments(),
            [
                Segment::Literal("test(=".to_owned()),
                Segment::With("test".to_owned()),
                Segment::Literal(")".to_owned()),
            ]
        );
        assert_eq!(
            *descriptor.invoke().cwd(),
            Cwd::Nearest(FileName("Cargo.toml".to_owned()))
        );
        assert!(descriptor.invoke().env().is_empty());
        assert_eq!(*descriptor.invoke().timeout(), None);
        assert_eq!(descriptor.results().pass(), &[0]);
        assert_eq!(descriptor.schema()["required"], serde_json::json!(["test"]));
    }

    #[test]
    fn results_default_to_passing_on_zero() {
        let source = NEXTEST.replace("\n[results]\npass = [0]\n", "");
        assert_eq!(parse(&source).unwrap().results().pass(), &[0]);
    }

    #[test]
    fn invoke_takes_env_and_a_timeout() {
        let source = NEXTEST.replace(
            "cwd = { nearest = \"Cargo.toml\" }",
            "cwd = \"validator-dir\"\nenv = { RUST_LOG = \"warn\" }\ntimeout = 600",
        );
        let descriptor = parse(&source).unwrap();
        let invoke = descriptor.invoke();
        assert_eq!(*invoke.cwd(), Cwd::ValidatorDir);
        assert_eq!(invoke.env().len(), 1);
        assert_eq!(invoke.timeout().map(NonZeroU64::get), Some(600));
    }

    #[test]
    fn templates_unescape_braces_and_read_builtins() {
        assert_eq!(
            Template::parse("{{{slcr.requirement}}}-{slcr.project_root}{slcr.validator_dir}")
                .unwrap()
                .segments(),
            [
                Segment::Literal("{".to_owned()),
                Segment::Builtin(Builtin::Requirement),
                Segment::Literal("}-".to_owned()),
                Segment::Builtin(Builtin::ProjectRoot),
                Segment::Builtin(Builtin::ValidatorDir),
            ]
        );
        assert_eq!(Template::parse("").unwrap().segments(), []);
        for invalid in [
            "{",
            "}",
            "{with}",
            "{with.}",
            "{with.1st}",
            "{slcr.unknown}",
            "{env.HOME}",
            "a{with.x",
        ] {
            assert!(Template::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn the_program_is_a_bare_name() {
        for invalid in [
            "/usr/bin/cargo",
            "./cargo",
            "bin\\cargo",
            "{with.x}",
            ".",
            "..",
        ] {
            let source = NEXTEST.replace("[\"cargo\",", &format!("[{invalid:?},"));
            assert!(parse(&source).is_err(), "{invalid}");
        }
        let source = NEXTEST.replace(
            "argv = [\"cargo\", \"nextest\", \"run\", \"--no-fail-fast\", \"--no-tests=fail\",\n        \"-E\", \"test(={with.test})\"]",
            "argv = []",
        );
        assert!(failure(&source).contains("`argv` must name a program"));
    }

    #[test]
    fn unknown_fields_are_errors_at_every_level() {
        for (from, to) in [
            ("name = ", "fingerprint = []\nname = "),
            (
                "[validator.schema]",
                "[validator]\nextra = 1\n[validator.schema]",
            ),
            ("[invoke]", "[invoke]\nshell = true"),
            ("pass = [0]", "pass = [0]\nformat = \"json\""),
            (
                "cwd = { nearest = \"Cargo.toml\" }",
                "cwd = { nearest = \"Cargo.toml\", above = 1 }",
            ),
        ] {
            assert!(NEXTEST.contains(from));
            let source = NEXTEST.replacen(from, to, 1);
            assert!(
                matches!(parse(&source), Err(InvalidDescriptor::Unparsable(_))),
                "{to}"
            );
        }
    }

    #[test]
    fn invalid_fields_are_rejected() {
        for (from, to) in [
            ("formatVersion = \"1\"", "formatVersion = \"2\""),
            ("name = \"cargo-nextest\"", "name = \"Cargo Nextest\""),
            ("version = \"1.0.0\"", "version = \"1.0\""),
            (
                "cwd = { nearest = \"Cargo.toml\" }",
                "cwd = { nearest = \"crates/Cargo.toml\" }",
            ),
            ("cwd = { nearest = \"Cargo.toml\" }", "cwd = \"home\""),
            ("pass = [0]", "pass = []"),
            ("pass = [0]", "pass = [0, 0]"),
            ("pass = [0]", "pass = [256]"),
            ("[invoke]", "[invoke]\nenv = {}"),
            ("[invoke]", "[invoke]\nenv = { \"1X\" = \"y\" }"),
            ("[invoke]", "[invoke]\ntimeout = 0"),
        ] {
            assert!(NEXTEST.contains(from), "{from}");
            let source = NEXTEST.replacen(from, to, 1);
            assert!(
                matches!(parse(&source), Err(InvalidDescriptor::Unparsable(_))),
                "{to}"
            );
        }
        // `cwd` is required.
        let source = NEXTEST.replace("cwd = { nearest = \"Cargo.toml\" }\n", "");
        assert!(failure(&source).contains("missing field `cwd`"));
    }

    #[test]
    fn placeholders_must_name_required_scalar_fields() {
        let field = || "name".to_owned();
        let undeclared = NEXTEST.replace("{with.test}", "{with.name}");
        assert_eq!(
            problems(&undeclared),
            [Problem::UndeclaredField { field: field() }]
        );
        let message = failure(&undeclared);
        assert!(message.contains("descriptor.toml is not a valid descriptor"));

        let field = || "test".to_owned();
        let optional = NEXTEST.replace("required = [\"test\"]\n", "");
        assert_eq!(
            problems(&optional),
            [Problem::OptionalField { field: field() }]
        );
        let array = NEXTEST.replace("type = \"string\", minLength = 1", "type = \"array\"");
        assert_eq!(
            problems(&array),
            [Problem::NonScalarField { field: field() }]
        );
        // Each field is reported once, however often it is substituted.
        let twice = NEXTEST.replace("\"-E\",", "\"{with.name}\", \"{with.name}\", \"-E\",");
        assert_eq!(
            problems(&twice),
            [Problem::UndeclaredField {
                field: "name".to_owned()
            }]
        );
    }

    #[test]
    fn the_validator_schema_must_be_a_valid_object_schema() {
        let not_object = NEXTEST.replace("type = \"object\"", "type = \"string\"");
        assert!(problems(&not_object).contains(&Problem::NotAnObjectSchema));
        let invalid = NEXTEST.replace("additionalProperties = false", "minProperties = -1");
        assert!(matches!(
            problems(&invalid)[..],
            [Problem::InvalidSchema(_)]
        ));
        let external = NEXTEST.replace(
            "additionalProperties = false",
            "\"$ref\" = \"https://example.com/schema.json\"",
        );
        assert!(matches!(
            problems(&external)[..],
            [Problem::InvalidSchema(_)]
        ));
        let internal = NEXTEST.replace(
            "additionalProperties = false",
            "\"$defs\".name = { type = \"string\" }\n\"$ref\" = \"#/$defs/name\"",
        );
        assert!(parse(&internal).is_ok());
        let datetime = NEXTEST.replace("additionalProperties = false", "default = 1979-05-27");
        assert_eq!(problems(&datetime), [Problem::Datetime]);
    }

    #[test]
    fn a_descriptor_must_be_utf8() {
        assert_eq!(text("d.toml", b"name = \"x\"").unwrap(), "name = \"x\"");
        let err = text("d.toml", b"name = \"\xff\"").unwrap_err();
        assert_eq!(err.to_string(), "d.toml is not UTF-8 text");
    }
}
