//! `slcr init [FILE] [--name NAME] [--title TITLE]`.

use insta::assert_snapshot;

use super::Workspace;

/// A manifest that `init` must leave as it is.
const MANIFEST: &str =
    "# The project manifest.\nformatVersion = \"1\"\nspecs = [\"**/*.slcr.toml\"]\n";

#[test]
fn init_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["init", "--help"]));
}

#[test]
fn init_creates_the_well_known_file_named_after_its_directory() {
    let workspace = Workspace::new();
    workspace.dir("todo-api");
    assert_cmd_snapshot!(workspace.slcr_in("todo-api", &["init"]));
    assert_eq!(
        workspace.files(),
        ["todo-api/SLCR.toml", "todo-api/SPEC.slcr.toml"]
    );
    assert_snapshot!("well_known_file", workspace.read("todo-api/SPEC.slcr.toml"));
    assert_snapshot!("new_manifest", workspace.read("todo-api/SLCR.toml"));
}

#[test]
fn init_writes_the_format_its_file_names() {
    for (format, file) in [
        ("json", "todo-api.spec.json"),
        ("yaml", "todo-api.slcr.yml"),
    ] {
        let workspace = Workspace::new();
        assert_cmd_snapshot!(format, workspace.slcr(&["init", file]));
        assert_snapshot!(format!("{format}_file"), workspace.read(file));
    }
}

#[test]
fn init_lists_a_file_in_a_subdirectory_in_the_new_manifest() {
    let workspace = Workspace::new();
    workspace.dir("specs");
    assert_cmd_snapshot!(workspace.slcr(&["init", "specs/api.slcr.toml"]));
    assert_eq!(workspace.files(), ["SLCR.toml", "specs/api.slcr.toml"]);
    assert_snapshot!("subdirectory_manifest", workspace.read("SLCR.toml"));
}

#[test]
fn init_takes_a_name_and_a_title() {
    let workspace = Workspace::new();
    let args = [
        "init",
        "specs.yaml",
        "--name",
        "billing",
        "--title",
        "Billing Service",
    ];
    assert_cmd_snapshot!(workspace.slcr(&args));
    assert_snapshot!("named_and_titled_file", workspace.read("specs.yaml"));
}

#[test]
fn init_leaves_an_existing_manifest_unchanged() {
    let workspace = Workspace::new();
    workspace.file("SLCR.toml", MANIFEST);
    assert_cmd_snapshot!(workspace.slcr(&["init", "--name", "todo-api"]));
    assert_eq!(workspace.files(), ["SLCR.toml", "SPEC.slcr.toml"]);
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn init_uses_the_manifest_of_a_parent_directory() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", MANIFEST)
        .dir("services/billing");
    assert_cmd_snapshot!(workspace.slcr_in("services/billing", &["init"]));
    assert_eq!(
        workspace.files(),
        ["SLCR.toml", "services/billing/SPEC.slcr.toml"]
    );
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn init_rejects_a_misnamed_manifest() {
    let workspace = Workspace::new();
    workspace.file("slcr.toml", MANIFEST).dir("billing");
    assert_cmd_snapshot!(workspace.slcr_in("billing", &["init"]));
    assert_eq!(workspace.files(), ["slcr.toml"]);
}

#[test]
fn init_rejects_a_file_outside_the_working_directory() {
    let workspace = Workspace::new();
    workspace.dir("project");
    let args = ["init", "../SPEC.slcr.toml", "--name", "todo-api"];
    assert_cmd_snapshot!(workspace.slcr_in("project", &args));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_rejects_a_file_a_glob_would_misread() {
    let workspace = Workspace::new();
    let args = ["init", "[draft].slcr.toml", "--name", "draft"];
    assert_cmd_snapshot!(workspace.slcr(&args));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_rejects_a_file_named_like_the_manifest() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["init", "SLCR.toml", "--name", "todo-api"]));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_never_overwrites_a_file() {
    let workspace = Workspace::new();
    workspace.file("todo-api.yaml", "keep: me\n");
    assert_cmd_snapshot!(workspace.slcr(&["init", "todo-api.yaml"]));
    assert_eq!(workspace.files(), ["todo-api.yaml"]);
    assert_eq!(workspace.read("todo-api.yaml"), "keep: me\n");
}

#[test]
fn init_does_not_create_a_missing_directory() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["init", "specs/todo-api.yaml"]));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_rejects_a_file_that_suggests_no_valid_name() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["init", "My Spec.yaml"]));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_rejects_an_unsupported_format() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["init", "SPEC.md", "--name", "todo-api"]));
    assert!(workspace.files().is_empty());
}

#[test]
fn init_rejects_an_invalid_name() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["init", "--name", "Todo API"]));
}

#[test]
fn init_rejects_an_invalid_title() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["init", "--title", " padded"]));
}

#[test]
fn init_takes_only_one_file() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["init", "a.yaml", "b.yaml"]));
}
