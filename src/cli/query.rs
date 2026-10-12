use std::{collections::btree_map::Entry, path::PathBuf, str::FromStr};

use clap::{Args, ValueEnum};
use miette::{Diagnostic, IntoDiagnostic, Result};
use slcr_gql::{Params, Value, parse_query};
use thiserror::Error;

use super::block_on;
use super::findings::FileFindings;
use crate::Terminal;
use crate::fs::FileSystem;
use crate::query::{InvalidQuery, SpecGraph};
use crate::spec::file::{DEFAULT_FILE, RequirementsFile};

mod format;

/// The arguments to `slcr query`.
#[derive(Args, Clone, Debug)]
pub struct QueryArgs {
    /// The query: read-only GQL, in the SLCR query profile, e.g.
    /// "MATCH (r:Requirement) RETURN r.id".
    #[arg(value_name = "QUERY")]
    query: String,

    /// The requirements file to query: `.yaml`, `.yml`, `.json`, or `.toml`.
    #[arg(value_name = "FILE", default_value = DEFAULT_FILE)]
    file: PathBuf,

    /// Bind the query's `$NAME` to a JSON value, e.g. `--param 'id="REQ-001"'`
    /// or `--param limit=5`. Repeat it for each parameter.
    #[arg(short, long = "param", value_name = "NAME=JSON")]
    params: Vec<Param>,

    /// How to write the rows.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    format: OutputFormat,
}

/// How `slcr query` writes the rows it finds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Aligned columns under a header, then the number of rows.
    #[default]
    Table,
    /// A header line, then one line per row, with tab-separated columns.
    Tsv,
    /// One JSON document with the columns and the rows.
    Json,
}

/// A `--param`: a parameter's name and the value bound to it.
#[derive(Clone, Debug, PartialEq)]
struct Param {
    name: String,
    value: Value,
}

impl FromStr for Param {
    type Err = String;

    fn from_str(param: &str) -> Result<Self, Self::Err> {
        let Some((name, json)) = param.split_once('=') else {
            return Err("expected NAME=JSON".to_owned());
        };
        if !is_param_name(name) {
            return Err(format!(
                "`{name}` is not a parameter name: use letters, digits, and `_`, without the `$`"
            ));
        }
        let json: serde_json::Value = serde_json::from_str(json).map_err(|_| {
            format!("`{json}` is not JSON; quote a string, as in --param '{name}=\"{json}\"'")
        })?;
        Ok(Self {
            name: name.to_owned(),
            value: value(json),
        })
    }
}

/// Whether `name` can follow a `$` in a query: an ASCII identifier, or
/// digits.
fn is_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_digit() => name.chars().all(|c| c.is_ascii_digit()),
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// The query value of `json`. An integer that doesn't fit an INT64 is a
/// FLOAT64.
fn value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(value),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(value) => Value::Int(value),
            None => Value::Float(number.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(value) => Value::String(value),
        serde_json::Value::Array(values) => Value::List(values.into_iter().map(value).collect()),
        serde_json::Value::Object(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(key, json)| (key, value(json)))
                .collect(),
        ),
    }
}

/// Query a specification with GQL.
///
/// The query is parsed before the file is read. The specification must
/// break none of its invariants; its findings, warnings included, are
/// reported on stderr as `slcr vet` reports them. The rows are written to
/// stdout, with each node named by its ID.
pub struct Query {
    terminal: Terminal,
    args: QueryArgs,
}

impl Query {
    pub fn new(terminal: Terminal, args: QueryArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let QueryArgs {
            query: text,
            file: path,
            params,
            format,
        } = self.args;
        let params = bind(params)?;
        let query = parse_query(&text).map_err(|error| InvalidQuery::new(&text, error))?;
        let file = RequirementsFile::new(path.clone())?;

        tracing::debug!(path = %path.display(), "querying a requirements file");
        let fs = FileSystem::new()?;
        let unvetted = fs.load_file(file.unvetted())?;
        let report = unvetted.report();
        if !report.is_clean() {
            let findings = FileFindings::new(&path, &report, false);
            self.terminal.write_diagnostic(&findings)?;
        }
        let Ok(document) = unvetted.into_vetted() else {
            return Err(QueryFailed { path }.into());
        };

        let rows = block_on(async {
            let graph = SpecGraph::new(&document).await.into_diagnostic()?;
            let rows = graph.query(&query, &params).await;
            rows.map_err(|error| miette::Report::new(InvalidQuery::new(&text, error)))
        })??;
        let output = match format {
            OutputFormat::Table => format::table(&rows),
            OutputFormat::Tsv => format::tsv(&rows),
            OutputFormat::Json => format::json(&rows),
        };
        self.terminal.write_stdout(&output)
    }
}

/// The parameters `params` bind, each of which may be bound only once.
fn bind(params: Vec<Param>) -> Result<Params, BoundTwice> {
    let mut bound = Params::new();
    for Param { name, value } in params {
        match bound.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(entry) => {
                return Err(BoundTwice {
                    name: entry.key().clone(),
                });
            }
        }
    }
    Ok(bound)
}

/// The error returned when the specification breaks its invariants, which
/// have been reported.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("could not query {}, which breaks its invariants", .path.display())]
#[diagnostic(help("fix the violations reported above, then query it again"))]
pub struct QueryFailed {
    path: PathBuf,
}

/// The error returned when `--param` binds a name twice.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`${name}` is bound more than once")]
#[diagnostic(help("pass one `--param` for each name"))]
pub struct BoundTwice {
    name: String,
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use std::collections::BTreeMap;

    use clap::Parser;
    use figment::Jail;

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;

    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");

    /// The arguments `slcr` parses `query` and then `args` into.
    fn parse(args: &[&str]) -> Result<QueryArgs, clap::Error> {
        let argv = ["slcr", "query"].iter().chain(args);
        match Cli::try_parse_from(argv)?.cmd() {
            Some(SlcrCommand::Query(args)) => Ok(args.clone()),
            _ => panic!("expected `query`"),
        }
    }

    /// Run `slcr query` with `args`.
    fn run(args: &[&str]) -> Result<()> {
        let argv = ["slcr", "--enable-colors", "never", "query"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn param(text: &str) -> Result<Param, String> {
        text.parse()
    }

    #[test]
    fn query_defaults_to_the_well_known_file_and_a_table() {
        let args = parse(&["RETURN 1"]).unwrap();
        assert_eq!(args.query, "RETURN 1");
        assert_eq!(args.file, PathBuf::from(DEFAULT_FILE));
        assert!(args.params.is_empty());
        assert_eq!(args.format, OutputFormat::Table);
    }

    #[test]
    fn query_accepts_a_file_params_and_a_format() {
        let args = parse(&[
            "RETURN $a, $b",
            "api.json",
            "--param",
            "a=1",
            "-p",
            "b=\"x\"",
            "--format",
            "json",
        ])
        .unwrap();
        assert_eq!(args.file, PathBuf::from("api.json"));
        assert_eq!(
            args.params,
            [param("a=1").unwrap(), param("b=\"x\"").unwrap()]
        );
        assert_eq!(args.format, OutputFormat::Json);
    }

    #[test]
    fn query_requires_a_query() {
        let err = parse(&[]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn params_take_any_json_value() {
        for (text, value) in [
            ("n=null", Value::Null),
            ("n=true", Value::Bool(true)),
            ("n=-5", Value::Int(-5)),
            ("n=2.5", Value::Float(2.5)),
            (
                "n=18446744073709551615",
                Value::Float(18446744073709551615.0),
            ),
            ("n=\"REQ-001\"", Value::String("REQ-001".to_owned())),
            ("n=\"a=b\"", Value::String("a=b".to_owned())),
            (
                "n=[1,\"x\"]",
                Value::List(vec![Value::Int(1), Value::String("x".to_owned())]),
            ),
            (
                "n={\"k\":[]}",
                Value::Map(BTreeMap::from([("k".to_owned(), Value::List(vec![]))])),
            ),
        ] {
            assert_eq!(param(text).unwrap().value, value, "{text}");
        }
    }

    #[test]
    fn params_are_named_as_queries_name_them() {
        for name in ["id", "_x", "Req2", "1", "42"] {
            assert_eq!(param(&format!("{name}=1")).unwrap().name, name);
        }
        for name in ["", "$id", "2a", "a-b", "é"] {
            assert!(param(&format!("{name}=1")).is_err(), "{name}");
        }
    }

    #[test]
    fn a_param_needs_a_name_and_a_json_value() {
        assert_eq!(param("id").unwrap_err(), "expected NAME=JSON");
        assert_eq!(
            param("id=REQ-001").unwrap_err(),
            "`REQ-001` is not JSON; quote a string, as in --param 'id=\"REQ-001\"'"
        );
    }

    #[test]
    fn a_name_can_be_bound_only_once() {
        let params = vec![param("a=1").unwrap(), param("a=2").unwrap()];
        assert_eq!(
            bind(params).unwrap_err(),
            BoundTwice {
                name: "a".to_owned()
            }
        );
    }

    #[test]
    fn query_reads_the_well_known_file() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.toml", TODO_API_TOML)?;
            run(&["MATCH (r:Requirement) RETURN count(*)"]).unwrap();
            Ok(())
        });
    }

    #[test]
    fn a_syntax_error_is_reported_before_the_file_is_read() {
        Jail::expect_with(|_| {
            let err = run(&["MATCH (r RETURN r", "missing.yaml"]).unwrap_err();
            let err = err.downcast_ref::<InvalidQuery>().unwrap();
            assert!(matches!(err.error(), slcr_gql::Error::Syntax { .. }));
            Ok(())
        });
    }

    #[test]
    fn a_failing_query_reports_the_engine_s_error() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.toml", TODO_API_TOML)?;
            let err = run(&["RETURN $missing"]).unwrap_err();
            let err = err.downcast_ref::<InvalidQuery>().unwrap();
            assert!(matches!(err.error(), slcr_gql::Error::Semantic(_)));
            Ok(())
        });
    }
}
