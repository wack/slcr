//! `slcr vet [FILE]... [--deny-warnings] [--format human|json]`: the files
//! given, or else the project.

use super::{
    NEXTEST_DESCRIPTOR, TODO_API_JSON, TODO_API_TOML, TODO_API_YAML, Workspace, pin, stored,
    with_violation, with_warning,
};

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

/// A manifest whose `specs` match the well-known file, and that declares
/// cargo-nextest at `version`, pinned to `descriptor`.
fn declaring(version: &str, descriptor: &str) -> String {
    format!(
        "{}\n[tools.cargo-nextest]\nversion = \"{version}\"\n\
         source = {{ path = \"tools/cargo-nextest.toml\" }}\npin = \"{}\"\n",
        manifest(&["SPEC.slcr.yml"]),
        pin(descriptor)
    )
}

/// A project whose specification is the canon example and which declares
/// cargo-nextest, with no descriptor installed and no validator file yet.
fn uninstalled() -> Workspace {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", declaring("1.0.0", NEXTEST_DESCRIPTOR))
        .file("SPEC.slcr.yml", TODO_API_YAML)
        .file("tools/cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    workspace
}

/// [uninstalled], with cargo-nextest's descriptor installed.
fn validated() -> Workspace {
    let workspace = uninstalled();
    workspace.install(NEXTEST_DESCRIPTOR);
    workspace
}

/// Validators that pass, in TOML.
const VALIDATORS_TOML: &str = r#"formatVersion = "1"

[[validator]]
requirement = "todo-api/REQ-002"
tool = "cargo-nextest"
description = "POST creates an item"
with = { test = "items::post::creates_item" }

[[validator]]
requirement = "todo-api/REQ-007"
criterion = "AC-001"
tool = "cargo-nextest"
with = { test = "db::schema::declares_items" }
"#;

/// Validators that pass, in JSON.
const VALIDATORS_JSON: &str = r#"{
  "formatVersion": "1",
  "validator": [
    {
      "requirement": "todo-api/REQ-003",
      "tool": "cargo-nextest",
      "with": { "test": "items::get::reads_an_item" }
    }
  ]
}
"#;

/// Validators that pass, in YAML.
const VALIDATORS_YAML: &str = r#"formatVersion: "1"
validator:
  - requirement: todo-api/REQ-005
    tool: cargo-nextest
    with:
      test: items::controller::routes
"#;

/// Validators with one of each problem a validator can have, in TOML.
const BROKEN_VALIDATORS: &str = r#"formatVersion = "1"

[[validator]]
requirement = "todo-api/REQ-002"
tool = "cargo-nextest"
with = { test = "items::post::creates_item" }

[[validator]]
requirement = "billing/REQ-001"
tool = "cargo-nextest"
with = { test = "billing::charges" }

[[validator]]
requirement = "todo-api/REQ-099"
tool = "cargo-nextest"
with = { test = "items::missing" }

[[validator]]
requirement = "todo-api/REQ-001"
criterion = "AC-001"
tool = "cargo-nextest"
with = { test = "items::endpoint" }

[[validator]]
requirement = "todo-api/REQ-002"
tool = "ast-grep"
with = { rule = "rules/post-handler.yml" }

[[validator]]
requirement = "todo-api/REQ-003"
tool = "cargo-nextest"
with = { test = "", filter = "all" }

[[validator]]
with = { test = "items::post::creates_item" }
tool = "cargo-nextest"
requirement = "todo-api/REQ-002"
"#;

#[test]
fn vet_vets_the_project_s_validator_files() {
    let workspace = validated();
    workspace
        .file("src/views/items/.validators.slcr.toml", VALIDATORS_TOML)
        .file("src/views/.validators.slcr.json", VALIDATORS_JSON)
        .file("src/.validators.slcr.yml", VALIDATORS_YAML)
        // Ignored, as specifications would be.
        .file(".git/HEAD", "ref: refs/heads/trunk\n")
        .file(".gitignore", "target/\n")
        .file("target/.validators.slcr.toml", "not even TOML");
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_names_validator_files_from_the_working_directory() {
    let workspace = validated();
    workspace
        .file("src/views/items/.validators.slcr.toml", VALIDATORS_TOML)
        .file("src/.validators.slcr.yml", VALIDATORS_YAML);
    assert_cmd_snapshot!(workspace.slcr_in("src/views", &["vet"]));
}

#[test]
fn vet_reports_every_problem_in_a_validator_file() {
    let workspace = validated();
    workspace.file("src/.validators.slcr.toml", BROKEN_VALIDATORS);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_validator_problems_in_json() {
    let workspace = validated();
    workspace
        .file("src/.validators.slcr.toml", BROKEN_VALIDATORS)
        .file("src/views/.validators.slcr.json", VALIDATORS_JSON)
        .file("src/views/items/.validators.slcr.yml", "validator: []\n");
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--format", "json"]));
}

#[test]
fn vet_labels_validator_problems_in_json_and_yaml() {
    let workspace = validated();
    workspace
        .file(
            "src/a/.validators.slcr.json",
            VALIDATORS_JSON.replace("REQ-003", "REQ-099"),
        )
        .file(
            "src/b/.validators.slcr.yaml",
            VALIDATORS_YAML.replace("cargo-nextest", "jev"),
        );
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_a_validator_file_that_breaks_its_schema() {
    let workspace = validated();
    workspace.file(
        "src/.validators.slcr.toml",
        VALIDATORS_TOML.replace("criterion = \"AC-001\"", "criterion = \"AC-1\""),
    );
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_two_validator_files_in_one_directory() {
    let workspace = validated();
    workspace
        .file("src/.validators.slcr.toml", VALIDATORS_TOML)
        .file("src/.validators.slcr.yml", VALIDATORS_YAML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_a_descriptor_that_is_not_installed() {
    let workspace = uninstalled();
    // Without its descriptor, no validator's `with` is checked.
    workspace.file("src/.validators.slcr.toml", BROKEN_VALIDATORS);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
    // Nothing was fetched or installed.
    assert!(!workspace.path(&stored(NEXTEST_DESCRIPTOR)).exists());
}

#[test]
fn vet_reports_a_corrupted_descriptor() {
    let workspace = validated();
    workspace.file(
        &stored(NEXTEST_DESCRIPTOR),
        NEXTEST_DESCRIPTOR.replace("--no-fail-fast", "--fail-fast"),
    );
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_an_invalid_descriptor() {
    let invalid = NEXTEST_DESCRIPTOR.replace("{with.test}", "{with.name}");
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", declaring("1.0.0", &invalid))
        .file("SPEC.slcr.yml", TODO_API_YAML)
        .install(&invalid);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_a_descriptor_whose_version_is_not_declared() {
    let workspace = validated();
    workspace
        .file("SLCR.toml", declaring("0.9.0", NEXTEST_DESCRIPTOR))
        .file("src/.validators.slcr.toml", VALIDATORS_TOML);
    assert_cmd_snapshot!(workspace.slcr(&["vet"]));
}

#[test]
fn vet_reports_descriptor_problems_in_json() {
    let workspace = uninstalled();
    workspace.file(
        "SLCR.toml",
        format!(
            "{}\n[tools.jev]\nversion = \"2.0.0\"\nsource = {{ path = \"tools/jev.toml\" }}\n\
             pin = \"{}\"\n",
            declaring("0.9.0", NEXTEST_DESCRIPTOR),
            pin("jev")
        ),
    );
    workspace.install(NEXTEST_DESCRIPTOR);
    assert_cmd_snapshot!(workspace.slcr(&["vet", "--format", "json"]));
}
