//! `slcr vet [FILE]... [--deny-warnings] [--format human|json]`: the files
//! given, or else the project.

use super::{TODO_API_JSON, TODO_API_TOML, TODO_API_YAML, Workspace, with_violation, with_warning};

/// A workspace holding one file of each kind `vet` tells apart.
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
fn vet_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["vet", "--help"]));
}

#[test]
fn vet_passes_the_canon_example_in_every_format() {
    let workspace = Workspace::new();
    workspace
        .file("todo-api.yaml", TODO_API_YAML)
        .file("todo-api.json", TODO_API_JSON)
        .file("todo-api.toml", TODO_API_TOML);
    assert_cmd_snapshot!(workspace.slcr(&[
        "vet",
        "todo-api.yaml",
        "todo-api.json",
        "todo-api.toml"
    ]));
}

#[test]
fn vet_reports_every_finding_in_every_file() {
    let args: Vec<&str> = ["vet"].into_iter().chain(MIXED).collect();
    assert_cmd_snapshot!(mixed().slcr(&args));
}

#[test]
fn vet_passes_warnings_unless_they_are_denied() {
    let workspace = Workspace::new();
    workspace.file("warned.yaml", with_warning());
    assert_cmd_snapshot!("allowed", workspace.slcr(&["vet", "warned.yaml"]));
    assert_cmd_snapshot!(
        "denied",
        workspace.slcr(&["vet", "--deny-warnings", "warned.yaml"])
    );
}

#[test]
fn vet_reports_in_json() {
    let args: Vec<&str> = ["vet", "--format", "json"]
        .into_iter()
        .chain(MIXED)
        .collect();
    assert_cmd_snapshot!(mixed().slcr(&args));
}

#[test]
fn vet_reports_denied_warnings_in_json() {
    let workspace = Workspace::new();
    workspace.file("warned.yaml", with_warning());
    let args = ["vet", "--format", "json", "--deny-warnings", "warned.yaml"];
    assert_cmd_snapshot!(workspace.slcr(&args));
}

#[test]
fn vet_reports_in_human_form_explicitly() {
    let workspace = Workspace::new();
    workspace.file("broken.yaml", with_violation());
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--format", "human", "broken.yaml"]));
}

#[test]
fn vet_rejects_an_unknown_format() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["vet", "--format", "xml", "a.yaml"]));
}

/// A project manifest whose `specs` are `patterns`.
fn manifest(patterns: &[&str]) -> String {
    let patterns: Vec<String> = patterns
        .iter()
        .map(|pattern| format!("\"{pattern}\""))
        .collect();
    format!("formatVersion = \"1\"\nspecs = [{}]\n", patterns.join(", "))
}

/// `yaml`, a specification in YAML named `todo-api`, renamed `name`.
fn renamed(yaml: &str, name: &str) -> String {
    assert!(yaml.contains("  name: todo-api\n"));
    yaml.replacen("  name: todo-api\n", &format!("  name: {name}\n"), 1)
}

/// A project, in a git repository, with a specification in each format:
/// one in the well-known file, one in a hidden directory, which is
/// searched, and one beside a file that `.gitignore` leaves out.
fn project() -> Workspace {
    let billing = TODO_API_JSON.replacen("\"name\": \"todo-api\"", "\"name\": \"billing\"", 1);
    let drafts = TODO_API_TOML.replacen("name = \"todo-api\"", "name = \"drafts\"", 1);
    assert!(billing != TODO_API_JSON && drafts != TODO_API_TOML);
    let workspace = Workspace::new();
    workspace
        .file(
            "SLCR.toml",
            manifest(&["SPEC.slcr.yml", "docs/**/*.slcr.{json,toml}"]),
        )
        .file("SPEC.slcr.yml", TODO_API_YAML)
        .file("docs/api/billing.slcr.json", billing)
        .file("docs/.drafts/drafts.slcr.toml", drafts)
        .file(".git/HEAD", "ref: refs/heads/trunk\n")
        .file(".gitignore", "generated/\n")
        .file("docs/generated/stale.slcr.json", "not even JSON");
    workspace
}

#[test]
fn vet_without_files_vets_the_project() {
    assert_cmd_snapshot!(project().slcr(&["vet"]));
}

#[test]
fn vet_names_project_files_from_the_working_directory() {
    assert_cmd_snapshot!(project().slcr_in("docs/api", &["vet"]));
}

#[test]
fn vet_reports_every_finding_in_the_project() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["specs/*"]))
        .file("specs/ok.yaml", TODO_API_YAML)
        .file("specs/warned.yaml", renamed(&with_warning(), "warned"))
        .file("specs/broken.yaml", renamed(&with_violation(), "broken"))
        .file("specs/malformed.yaml", "[unclosed\n");
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--deny-warnings"]));
}

#[test]
fn vet_reports_a_pattern_that_matches_nothing() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["SPEC.slcr.yml", "SPEC.slcr.yaml"]))
        .file("SPEC.slcr.yml", TODO_API_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_a_pattern_that_matches_a_validator_file() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["docs/**/*.slcr.toml"]))
        .file("docs/api/api.slcr.toml", TODO_API_TOML)
        .file("docs/api/.validators.slcr.toml", "");
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_a_match_of_an_unsupported_format() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["specs/*"]))
        .file("specs/todo-api.yaml", TODO_API_YAML)
        .file("specs/notes.md", "# Notes\n");
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_an_invalid_pattern() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["specs/[a", "SPEC.slcr.yml"]))
        .file("SPEC.slcr.yml", TODO_API_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_duplicate_specification_names() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["specs/*"]))
        .file("specs/a.yaml", TODO_API_YAML)
        .file("specs/b.json", TODO_API_JSON);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_an_invalid_manifest() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["../SPEC.slcr.yml"]))
        .file("SPEC.slcr.yml", TODO_API_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_requires_a_project_without_files() {
    let workspace = Workspace::new();
    workspace.file("SPEC.slcr.yml", TODO_API_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_rejects_a_misnamed_manifest() {
    let workspace = Workspace::new();
    workspace
        .file("slcr.toml", manifest(&["SPEC.slcr.yml"]))
        .file("SPEC.slcr.yml", TODO_API_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_the_project_in_json() {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", manifest(&["specs/*", "docs/*.yaml"]))
        .file("specs/a.yaml", TODO_API_YAML)
        .file("specs/b.yaml", renamed(&with_warning(), "warned"))
        .file("specs/.validators.slcr.yml", "");
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--format", "json"]));
}

#[test]
fn vet_reports_an_invalid_manifest_in_json() {
    let workspace = Workspace::new();
    workspace.file("SLCR.toml", "formatVersion = \"1\"\n");
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--format", "json"]));
}
