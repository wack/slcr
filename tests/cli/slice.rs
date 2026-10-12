//! `slcr slice REQUIREMENT [FILE]`.
//!
//! What a slice includes and how it renders are the library's to test;
//! these tests pin what the command reports and writes.

use super::{
    TODO_API_JSON, TODO_API_MD, TODO_API_TOML, TODO_API_YAML, Workspace, with_violation,
    with_warning,
};

/// What the canon example's controller, REQ-005, slices to.
const TODO_API_REQ_005_MD: &str = include_str!("../fixtures/todo-api.REQ-005.md");

/// A workspace holding the canon example at the well-known path.
fn canon() -> Workspace {
    let workspace = Workspace::new();
    workspace.file("SPEC.slcr.toml", TODO_API_TOML);
    workspace
}

#[test]
fn slice_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["slice", "--help"]));
}

#[test]
fn slice_reads_the_well_known_file() {
    let workspace = canon();
    assert_cmd_snapshot!(workspace.slcr(&["slice", "REQ-005"]));
    // A slice is written to stdout only.
    assert_eq!(workspace.files(), ["SPEC.slcr.toml"]);
}

#[test]
fn slice_reads_the_file_in_every_format() {
    let workspace = Workspace::new();
    workspace
        .file("todo-api.yaml", TODO_API_YAML)
        .file("todo-api.json", TODO_API_JSON)
        .file("todo-api.toml", TODO_API_TOML);
    for file in ["todo-api.yaml", "todo-api.json", "todo-api.toml"] {
        let output = workspace
            .slcr(&["slice", "REQ-005", file])
            .output()
            .unwrap();
        assert!(output.status.success(), "{file}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            TODO_API_REQ_005_MD,
            "{file}"
        );
        assert!(output.stderr.is_empty(), "{file}");
    }
}

#[test]
fn slice_brings_a_shared_requirement_s_parents() {
    // REQ-004 refines both REQ-002, its container, and REQ-003.
    assert_cmd_snapshot!(canon().slcr(&["slice", "REQ-004"]));
}

#[test]
fn slice_brings_refinements_housed_elsewhere() {
    // REQ-004, housed under REQ-002, refines REQ-003.
    assert_cmd_snapshot!(canon().slcr(&["slice", "REQ-003"]));
}

#[test]
fn slice_reports_warnings_and_slices_anyway() {
    let workspace = Workspace::new();
    workspace.file("todo-api.yaml", with_warning());
    assert_cmd_snapshot!(workspace.slcr(&["slice", "REQ-003", "todo-api.yaml"]));
}

#[test]
fn slice_refuses_a_specification_that_breaks_its_invariants() {
    let workspace = Workspace::new();
    workspace.file("todo-api.yaml", with_violation());
    assert_cmd_snapshot!(workspace.slcr(&["slice", "REQ-001", "todo-api.yaml"]));
}

#[test]
fn slice_reports_a_requirement_the_specification_lacks() {
    assert_cmd_snapshot!(canon().slcr(&["slice", "REQ-099"]));
}

#[test]
fn slice_rejects_an_invalid_requirement_id() {
    assert_cmd_snapshot!(canon().slcr(&["slice", "REQ-5"]));
}

#[test]
fn slice_rejects_an_id_of_another_kind() {
    assert_cmd_snapshot!(canon().slcr(&["slice", "SEC-001"]));
}

#[test]
fn slice_requires_a_requirement() {
    assert_cmd_snapshot!(canon().slcr(&["slice"]));
}

#[test]
fn slice_takes_only_one_file() {
    let args = ["slice", "REQ-001", "a.yaml", "b.yaml"];
    assert_cmd_snapshot!(Workspace::new().slcr(&args));
}

#[test]
fn slice_reports_a_missing_file() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["slice", "REQ-001"]));
}

#[test]
fn slice_rejects_an_unsupported_format() {
    let workspace = Workspace::new();
    workspace.file("SPEC.md", TODO_API_MD);
    assert_cmd_snapshot!(workspace.slcr(&["slice", "REQ-001", "SPEC.md"]));
}
