//! `slcr query QUERY [FILE] [--param NAME=JSON]... [--format FORMAT]`.
//!
//! The query engine's semantics are `slcr-gql`'s to test; these tests pin
//! what the command reports, how it writes rows, and how it names nodes.

use insta_cmd::assert_cmd_snapshot;

use super::{
    TODO_API_JSON, TODO_API_MD, TODO_API_TOML, TODO_API_YAML, Workspace, with_violation,
    with_warning,
};

/// What transitively refines REQ-001: three requirements.
const DEPENDENTS: &str = "MATCH (d:Requirement)-[:REFINES]->{1,}(:Requirement {id: 'REQ-001'}) \
     RETURN DISTINCT d.id AS dependent, d.modality ORDER BY dependent";

/// A workspace holding the canon example at the well-known path.
fn canon() -> Workspace {
    let workspace = Workspace::new();
    workspace.file("SPEC.slcr.yml", TODO_API_YAML);
    workspace
}

#[test]
fn query_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["query", "--help"]));
}

#[test]
fn query_reads_the_well_known_file() {
    assert_cmd_snapshot!(canon().slcr(&["query", DEPENDENTS]));
}

#[test]
fn query_reads_the_file_in_every_format() {
    let workspace = Workspace::new();
    workspace
        .file("todo-api.yaml", TODO_API_YAML)
        .file("todo-api.json", TODO_API_JSON)
        .file("todo-api.toml", TODO_API_TOML);
    let outputs: Vec<_> = ["todo-api.yaml", "todo-api.json", "todo-api.toml"]
        .into_iter()
        .map(|file| {
            workspace
                .slcr(&["query", DEPENDENTS, file])
                .output()
                .unwrap()
        })
        .collect();
    assert!(outputs.iter().all(|output| output.status.success()));
    assert!(outputs.iter().all(|output| output == &outputs[0]));
    assert_cmd_snapshot!(workspace.slcr(&["query", DEPENDENTS, "todo-api.json"]));
}

#[test]
fn query_names_nodes_and_edges_by_id() {
    let query = "MATCH (r:Requirement {id: 'REQ-004'})-[e:REFINES]->(t) \
                 RETURN r, e, t ORDER BY t.id";
    assert_cmd_snapshot!(canon().slcr(&["query", query]));
}

#[test]
fn query_names_nodes_in_lists_by_id() {
    let query = "MATCH (s:Section)-[:CONTAINS]->(c) \
                 RETURN s.id AS section, collect_list(c) AS children ORDER BY section";
    assert_cmd_snapshot!(canon().slcr(&["query", query]));
}

#[test]
fn query_reports_no_rows() {
    let query = "MATCH (r:Requirement {modality: 'MUST NOT'}) RETURN r";
    assert_cmd_snapshot!(canon().slcr(&["query", query]));
}

#[test]
fn query_binds_params() {
    let query = "MATCH (r:Requirement) WHERE r.modality = $modality AND r.id IN $ids \
                 RETURN r, r.title ORDER BY r.id";
    let args = [
        "query",
        query,
        "--param",
        "modality=\"MAY\"",
        "-p",
        r#"ids=["REQ-009", "REQ-010", "REQ-099"]"#,
    ];
    assert_cmd_snapshot!(canon().slcr(&args));
}

#[test]
fn query_writes_a_table_explicitly() {
    let args = [
        "query",
        "MATCH (t:Term) RETURN t, t.title",
        "--format",
        "table",
    ];
    assert_cmd_snapshot!(canon().slcr(&args));
}

#[test]
fn query_writes_tsv() {
    let query = "MATCH (r)-[e:HAS_CRITERION]->(c) RETURN r, e, c, c.body, e.position, r.rationale";
    assert_cmd_snapshot!(canon().slcr(&["query", query, "--format", "tsv"]));
}

#[test]
fn query_writes_json() {
    let query = "MATCH (r)-[e:HAS_CRITERION]->(c) \
                 RETURN r, e, c, c.body, e.position, r.rationale, [1.5, true] AS list";
    assert_cmd_snapshot!(canon().slcr(&["query", query, "--format", "json"]));
}

#[test]
fn query_reports_warnings_and_queries_anyway() {
    let workspace = Workspace::new();
    workspace.file("todo-api.yaml", with_warning());
    let query = "MATCH (r {id: 'REQ-003'}) RETURN r.body";
    assert_cmd_snapshot!(workspace.slcr(&["query", query, "todo-api.yaml"]));
}

#[test]
fn query_refuses_a_specification_that_breaks_its_invariants() {
    let workspace = Workspace::new();
    workspace.file("todo-api.yaml", with_violation());
    let args = ["query", "MATCH (r) RETURN r", "todo-api.yaml"];
    assert_cmd_snapshot!(workspace.slcr(&args));
}

#[test]
fn query_reports_a_syntax_error() {
    assert_cmd_snapshot!(canon().slcr(&["query", "MATCH (r:Requirement RETURN r"]));
}

#[test]
fn query_reports_a_syntax_error_before_reading_the_file() {
    let args = ["query", "MATCH (r RETURN r", "missing.yaml"];
    assert_cmd_snapshot!(Workspace::new().slcr(&args));
}

#[test]
fn query_reports_a_feature_outside_the_profile() {
    assert_cmd_snapshot!(canon().slcr(&["query", "OPTIONAL MATCH (r) RETURN r"]));
}

#[test]
fn query_reports_an_invalid_query() {
    assert_cmd_snapshot!(canon().slcr(&["query", "MATCH (r) RETURN s"]));
}

#[test]
fn query_reports_a_missing_param() {
    assert_cmd_snapshot!(canon().slcr(&["query", "MATCH (r {id: $id}) RETURN r"]));
}

#[test]
fn query_reports_a_query_that_fails() {
    assert_cmd_snapshot!(canon().slcr(&["query", "MATCH (r) RETURN r.id + 1"]));
}

#[test]
fn query_rejects_a_param_bound_twice() {
    let args = ["query", "RETURN $n", "-p", "n=1", "-p", "n=2"];
    assert_cmd_snapshot!(canon().slcr(&args));
}

#[test]
fn query_rejects_a_param_that_is_not_json() {
    let args = [
        "query",
        "MATCH (r {id: $id}) RETURN r",
        "--param",
        "id=REQ-001",
    ];
    assert_cmd_snapshot!(canon().slcr(&args));
}

#[test]
fn query_rejects_a_param_without_a_value() {
    assert_cmd_snapshot!(canon().slcr(&["query", "RETURN $id", "--param", "id"]));
}

#[test]
fn query_rejects_a_param_named_with_a_dollar() {
    assert_cmd_snapshot!(canon().slcr(&["query", "RETURN $id", "--param", "$id=1"]));
}

#[test]
fn query_rejects_an_unknown_format() {
    assert_cmd_snapshot!(canon().slcr(&["query", "RETURN 1", "--format", "csv"]));
}

#[test]
fn query_requires_a_query() {
    assert_cmd_snapshot!(canon().slcr(&["query"]));
}

#[test]
fn query_takes_only_one_file() {
    let args = ["query", "RETURN 1", "a.yaml", "b.yaml"];
    assert_cmd_snapshot!(Workspace::new().slcr(&args));
}

#[test]
fn query_reports_a_missing_file() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["query", "RETURN 1"]));
}

#[test]
fn query_rejects_an_unsupported_format() {
    let workspace = Workspace::new();
    workspace.file("SPEC.md", TODO_API_MD);
    assert_cmd_snapshot!(workspace.slcr(&["query", "RETURN 1", "SPEC.md"]));
}
