//! `slcr tools add SOURCE [--as NAME]`.

use insta::assert_snapshot;
use insta_cmd::assert_cmd_snapshot;

use super::{DATA_HOME, NEXTEST_DESCRIPTOR, Workspace};

/// A manifest that declares no tools yet.
const MANIFEST: &str =
    "# The project manifest.\nformatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";

/// Where the descriptor store files a descriptor, relative to the
/// workspace: under the BLAKE3 hash of its bytes.
fn stored(descriptor: &str) -> String {
    let hex = blake3::hash(descriptor.as_bytes()).to_hex();
    format!("{DATA_HOME}/descriptors/blake3/{hex}.toml")
}

/// A workspace that is a project with no tools, and the cargo-nextest
/// descriptor kept in it.
fn project() -> Workspace {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", MANIFEST)
        .file("tools/cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    workspace
}

#[test]
fn tools_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "--help"]));
}

#[test]
fn tools_add_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "add", "--help"]));
}

#[test]
fn tools_add_declares_and_installs_a_descriptor_kept_in_the_project() {
    let workspace = project();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/cargo-nextest.toml"]));
    assert_snapshot!("declared_manifest", workspace.read("SLCR.toml"));
    let installed = stored(NEXTEST_DESCRIPTOR);
    assert_eq!(
        workspace.files(),
        [installed.as_str(), "SLCR.toml", "tools/cargo-nextest.toml"]
    );
    assert_eq!(workspace.read(&installed), NEXTEST_DESCRIPTOR);
}

#[test]
fn tools_add_takes_a_name() {
    let workspace = project();
    let args = [
        "tools",
        "add",
        "tools/cargo-nextest.toml",
        "--as",
        "nextest",
    ];
    assert_cmd_snapshot!(workspace.slcr(&args));
    assert_snapshot!("named_manifest", workspace.read("SLCR.toml"));
}

#[test]
fn tools_add_finds_the_project_above_the_working_directory() {
    let workspace = project();
    workspace.dir("crates/api");
    let args = ["tools", "add", "../../tools/cargo-nextest.toml"];
    assert_cmd_snapshot!(workspace.slcr_in("crates/api", &args));
    // The source is recorded relative to the project root.
    assert!(
        workspace
            .read("SLCR.toml")
            .contains("source = { path = \"tools/cargo-nextest.toml\" }")
    );
}

#[test]
fn tools_add_is_idempotent() {
    let workspace = project();
    let args = ["tools", "add", "tools/cargo-nextest.toml"];
    assert!(workspace.slcr(&args).status().unwrap().success());
    let manifest = workspace.read("SLCR.toml");
    assert_cmd_snapshot!(workspace.slcr(&args));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_add_never_replaces_another_tool_of_the_same_name() {
    let workspace = project();
    let args = ["tools", "add", "tools/cargo-nextest.toml"];
    assert!(workspace.slcr(&args).status().unwrap().success());
    let manifest = workspace.read("SLCR.toml");
    let newer = NEXTEST_DESCRIPTOR.replace("version = \"1.0.0\"", "version = \"1.1.0\"");
    workspace.file("tools/cargo-nextest.toml", &newer);
    assert_cmd_snapshot!(workspace.slcr(&args));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
    assert!(!workspace.files().contains(&stored(&newer)));
}

#[test]
fn tools_add_requires_a_manifest() {
    let workspace = Workspace::new();
    workspace.file("cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "cargo-nextest.toml"]));
    assert_eq!(workspace.files(), ["cargo-nextest.toml"]);
}

#[test]
fn tools_add_rejects_a_misnamed_manifest() {
    let workspace = Workspace::new();
    workspace
        .file("slcr.toml", MANIFEST)
        .file("cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "cargo-nextest.toml"]));
}

#[test]
fn tools_add_rejects_an_invalid_manifest() {
    let workspace = project();
    let invalid = MANIFEST.replace(
        "specs = [\"SPEC.slcr.yml\"]",
        "specs = [\"../SPEC.slcr.yml\"]",
    );
    workspace.file("SLCR.toml", &invalid);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/cargo-nextest.toml"]));
    assert_eq!(workspace.read("SLCR.toml"), invalid);
}

#[test]
fn tools_add_rejects_a_descriptor_outside_the_project() {
    let workspace = Workspace::new();
    workspace
        .file("project/SLCR.toml", MANIFEST)
        .file("cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    let args = ["tools", "add", "../cargo-nextest.toml"];
    assert_cmd_snapshot!(workspace.slcr_in("project", &args));
    assert_eq!(workspace.read("project/SLCR.toml"), MANIFEST);
}

#[test]
fn tools_add_reports_a_missing_descriptor() {
    let workspace = project();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/jev.toml"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_add_rejects_an_unparsable_descriptor() {
    let workspace = project();
    let unknown_field = NEXTEST_DESCRIPTOR.replace("[invoke]\n", "[invoke]\nshell = true\n");
    workspace.file("tools/cargo-nextest.toml", unknown_field);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/cargo-nextest.toml"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
    assert!(
        !workspace
            .files()
            .iter()
            .any(|file| file.starts_with(DATA_HOME))
    );
}

#[test]
fn tools_add_rejects_a_descriptor_that_breaks_an_invariant() {
    let workspace = project();
    let undeclared = NEXTEST_DESCRIPTOR.replace("{with.test}", "{with.name}");
    workspace.file("tools/cargo-nextest.toml", undeclared);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/cargo-nextest.toml"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_add_fetches_only_over_https() {
    let workspace = project();
    let args = ["tools", "add", "http://example.com/cargo-nextest.toml"];
    assert_cmd_snapshot!(workspace.slcr(&args));
}

#[test]
fn tools_add_reports_a_failed_fetch() {
    let workspace = project();
    // Nothing listens on port 1.
    let mut command = workspace.slcr(&["tools", "add", "https://127.0.0.1:1/cargo-nextest.toml"]);
    for proxy in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        command.env_remove(proxy);
    }
    assert_cmd_snapshot!(command);
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_add_rejects_an_invalid_name() {
    let workspace = project();
    let args = [
        "tools",
        "add",
        "tools/cargo-nextest.toml",
        "--as",
        "Nextest",
    ];
    assert_cmd_snapshot!(workspace.slcr(&args));
}
