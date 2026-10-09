//! `slcr check <FILE>... [--deny-warnings] [--format human|json]`.

use insta_cmd::assert_cmd_snapshot;

use super::{TODO_API_JSON, TODO_API_TOML, TODO_API_YAML, Workspace, with_violation, with_warning};

/// A workspace holding one file of each kind `check` tells apart.
fn mixed() -> Workspace {
    let workspace = Workspace::new();
    workspace
        .file("ok.yaml", TODO_API_YAML)
        .file("warned.yaml", with_warning())
        .file("broken.yaml", with_violation())
        // Syntax the parser can't place, and grammar it can.
        .file("malformed.yaml", "[unclosed\n")
        .file(
            "invalid.yaml",
            TODO_API_YAML.replacen("id: SEC-001", "id: SEC-1", 1),
        );
    workspace
}

/// Every kind of file, plus a missing file and an unsupported one.
const MIXED: [&str; 7] = [
    "ok.yaml",
    "warned.yaml",
    "broken.yaml",
    "malformed.yaml",
    "invalid.yaml",
    "missing.yaml",
    "notes.md",
];

#[test]
fn check_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["check", "--help"]));
}

#[test]
fn check_passes_the_canon_example_in_every_format() {
    let workspace = Workspace::new();
    workspace
        .file("todo-api.yaml", TODO_API_YAML)
        .file("todo-api.json", TODO_API_JSON)
        .file("todo-api.toml", TODO_API_TOML);
    assert_cmd_snapshot!(workspace.slcr(&[
        "check",
        "todo-api.yaml",
        "todo-api.json",
        "todo-api.toml"
    ]));
}

#[test]
fn check_reports_every_finding_in_every_file() {
    let args: Vec<&str> = ["check"].into_iter().chain(MIXED).collect();
    assert_cmd_snapshot!(mixed().slcr(&args));
}

#[test]
fn check_passes_warnings_unless_they_are_denied() {
    let workspace = Workspace::new();
    workspace.file("warned.yaml", with_warning());
    assert_cmd_snapshot!("allowed", workspace.slcr(&["check", "warned.yaml"]));
    assert_cmd_snapshot!(
        "denied",
        workspace.slcr(&["check", "--deny-warnings", "warned.yaml"])
    );
}

#[test]
fn check_reports_in_json() {
    let args: Vec<&str> = ["check", "--format", "json"]
        .into_iter()
        .chain(MIXED)
        .collect();
    assert_cmd_snapshot!(mixed().slcr(&args));
}

#[test]
fn check_reports_denied_warnings_in_json() {
    let workspace = Workspace::new();
    workspace.file("warned.yaml", with_warning());
    let args = [
        "check",
        "--format",
        "json",
        "--deny-warnings",
        "warned.yaml",
    ];
    assert_cmd_snapshot!(workspace.slcr(&args));
}

#[test]
fn check_reports_in_human_form_explicitly() {
    let workspace = Workspace::new();
    workspace.file("broken.yaml", with_violation());
    assert_cmd_snapshot!(workspace.slcr(&["check", "--format", "human", "broken.yaml"]));
}

#[test]
fn check_requires_a_file() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["check"]));
}

#[test]
fn check_rejects_an_unknown_format() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["check", "--format", "xml", "a.yaml"]));
}
