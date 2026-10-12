//! `slcr tools add SOURCE [--as NAME]`, `slcr tools update NAME`, and
//! `slcr tools remove NAME`.

use insta::assert_snapshot;

use super::{DATA_DIR, NEXTEST_DESCRIPTOR, Workspace, stored};

/// A manifest that declares no tools yet.
const MANIFEST: &str =
    "# The project manifest.\nformatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";

/// A workspace that is a project with no tools, and the cargo-nextest
/// descriptor kept in it.
fn project() -> Workspace {
    let workspace = Workspace::new();
    workspace
        .file("SLCR.toml", MANIFEST)
        .file("tools/cargo-nextest.toml", NEXTEST_DESCRIPTOR);
    workspace
}

/// A project that declares cargo-nextest, from the descriptor kept in it.
fn declared() -> Workspace {
    let workspace = project();
    let args = ["tools", "add", "tools/cargo-nextest.toml"];
    assert!(workspace.slcr(&args).status().unwrap().success());
    workspace
}

/// The cargo-nextest descriptor with its version bumped.
fn newer() -> String {
    NEXTEST_DESCRIPTOR.replace("version = \"1.0.0\"", "version = \"1.1.0\"")
}

/// [MANIFEST], declaring cargo-nextest 1.0.0 from `source`, pinned to the
/// cargo-nextest descriptor: an entry `slcr tools add` could not have
/// written, such as one whose source is unreachable.
fn declaring(source: &str) -> String {
    let hex = blake3::hash(NEXTEST_DESCRIPTOR.as_bytes()).to_hex();
    format!(
        "{MANIFEST}\n[tools.cargo-nextest]\nversion = \"1.0.0\"\nsource = {source}\n\
         pin = \"blake3:{hex}\"\n"
    )
}

/// A project whose manifest declares two tools, with comments and
/// spacing of its own, and the cargo-nextest descriptor kept in it.
fn commented() -> Workspace {
    let workspace = project();
    let pin = |text: &str| format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex());
    workspace.file(
        "SLCR.toml",
        format!(
            "{MANIFEST}\n# Runs the test suite.\n[tools.cargo-nextest]\n\
             version = \"1.0.0\"  # copied from the descriptor\n\
             source  = {{ path = \"tools/cargo-nextest.toml\" }}\n\
             pin     = \"{}\"  # never edited by hand\n\n\
             # Lints the code.\n[tools.ast-grep]\nversion = \"0.3.1\"\n\
             source  = {{ path = \"tools/ast-grep.toml\" }}\npin     = \"{}\"\n\n\
             # The end.\n",
            pin(NEXTEST_DESCRIPTOR),
            pin("ast-grep"),
        ),
    );
    workspace
}

/// Run `slcr` with `args` in `workspace`, whose data directory is a file,
/// and check that it fails to create the descriptor store.
///
/// Not a snapshot: the error rightly names the store, which is under the
/// absolute data directory the workspace sets, so it differs on every run.
/// It is checked exactly instead.
fn assert_store_unavailable(workspace: &Workspace, args: &[&str]) {
    let output = workspace.slcr(args).output().unwrap();
    let store = workspace.path(DATA_DIR).join("descriptors/blake3");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "Error:   × could not create the descriptor store at {}\n  ╰─▶ Not a directory (os \
             error 20)\n  help: slcr installs descriptors in its data directory; make sure it can \
             create directories there\n\n",
            store.display()
        )
    );
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
            .any(|file| file.starts_with(DATA_DIR))
    );
}

#[test]
fn tools_add_rejects_a_descriptor_that_is_not_utf8() {
    let workspace = project();
    workspace.file("tools/cargo-nextest.toml", b"name = \"\xff\"\n");
    assert_cmd_snapshot!(workspace.slcr(&["tools", "add", "tools/cargo-nextest.toml"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_add_reports_a_descriptor_store_it_cannot_create() {
    let workspace = project();
    // A file where the data directory would be.
    workspace.file(DATA_DIR, "");
    assert_store_unavailable(&workspace, &["tools", "add", "tools/cargo-nextest.toml"]);
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
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

#[test]
fn tools_update_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "update", "--help"]));
}

#[test]
fn tools_update_installs_and_pins_a_new_version() {
    let workspace = declared();
    workspace.file("tools/cargo-nextest.toml", newer());
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_snapshot!("updated_manifest", workspace.read("SLCR.toml"));
    // The old descriptor stays installed: other projects may pin it.
    let (old, new) = (stored(NEXTEST_DESCRIPTOR), stored(&newer()));
    let mut expected = [
        old.as_str(),
        new.as_str(),
        "SLCR.toml",
        "tools/cargo-nextest.toml",
    ];
    expected.sort_unstable();
    assert_eq!(workspace.files(), expected);
    assert_eq!(workspace.read(&new), newer());
}

#[test]
fn tools_update_repins_an_edited_descriptor() {
    let workspace = declared();
    let edited = format!("# Ours.\n{NEXTEST_DESCRIPTOR}");
    workspace.file("tools/cargo-nextest.toml", &edited);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    let pin = blake3::hash(edited.as_bytes()).to_hex();
    assert!(
        workspace
            .read("SLCR.toml")
            .contains(&format!("pin = \"blake3:{pin}\""))
    );
    assert!(workspace.files().contains(&stored(&edited)));
}

#[test]
fn tools_update_leaves_an_up_to_date_tool_alone() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_update_finds_the_project_above_the_working_directory() {
    let workspace = declared();
    workspace
        .file("tools/cargo-nextest.toml", newer())
        .dir("crates/api");
    let args = ["tools", "update", "cargo-nextest"];
    assert_cmd_snapshot!(workspace.slcr_in("crates/api", &args));
    assert!(workspace.read("SLCR.toml").contains("version = \"1.1.0\""));
}

#[test]
fn tools_update_requires_a_declared_tool() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "jev"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_update_requires_a_manifest() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.files(), Vec::<String>::new());
}

#[test]
fn tools_update_reports_a_missing_descriptor() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    std::fs::remove_file(workspace.path("tools/cargo-nextest.toml")).unwrap();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_update_rejects_a_descriptor_that_breaks_an_invariant() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    let undeclared = newer().replace("{with.test}", "{with.name}");
    workspace.file("tools/cargo-nextest.toml", &undeclared);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
    assert!(!workspace.files().contains(&stored(&undeclared)));
}

#[test]
fn tools_update_rejects_an_invalid_name() {
    assert_cmd_snapshot!(project().slcr(&["tools", "update", "Nextest"]));
}

#[test]
fn tools_update_keeps_the_rest_of_the_manifest() {
    let workspace = commented();
    workspace.file("tools/cargo-nextest.toml", newer());
    let args = ["tools", "update", "cargo-nextest"];
    assert!(workspace.slcr(&args).status().unwrap().success());
    assert_snapshot!(workspace.read("SLCR.toml"));
}

#[test]
fn tools_update_rejects_a_misnamed_manifest() {
    let workspace = Workspace::new();
    workspace.file("slcr.toml", declaring("{ path = \"cargo-nextest.toml\" }"));
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
}

#[test]
fn tools_update_rejects_an_invalid_manifest() {
    let workspace = project();
    let invalid = declaring("{ path = \"../cargo-nextest.toml\" }");
    workspace.file("SLCR.toml", &invalid);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), invalid);
}

#[cfg(unix)]
#[test]
fn tools_update_rejects_a_descriptor_outside_the_project() {
    let workspace = Workspace::new();
    let manifest = declaring("{ path = \"tools/cargo-nextest.toml\" }");
    workspace
        .file("project/SLCR.toml", &manifest)
        .file("cargo-nextest.toml", newer())
        .dir("project/tools");
    // A path in the project that leads out of it.
    std::os::unix::fs::symlink(
        workspace.path("cargo-nextest.toml"),
        workspace.path("project/tools/cargo-nextest.toml"),
    )
    .unwrap();
    let args = ["tools", "update", "cargo-nextest"];
    assert_cmd_snapshot!(workspace.slcr_in("project", &args));
    assert_eq!(workspace.read("project/SLCR.toml"), manifest);
}

#[test]
fn tools_update_rejects_an_unparsable_descriptor() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    let unknown_field = newer().replace("[invoke]\n", "[invoke]\nshell = true\n");
    workspace.file("tools/cargo-nextest.toml", &unknown_field);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
    assert!(!workspace.files().contains(&stored(&unknown_field)));
}

#[test]
fn tools_update_rejects_a_descriptor_that_is_not_utf8() {
    let workspace = declared();
    let manifest = workspace.read("SLCR.toml");
    workspace.file("tools/cargo-nextest.toml", b"name = \"\xff\"\n");
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_update_reports_a_failed_fetch() {
    let workspace = project();
    // Nothing listens on port 1.
    let manifest = declaring("{ url = \"https://127.0.0.1:1/cargo-nextest.toml\" }");
    workspace.file("SLCR.toml", &manifest);
    let mut command = workspace.slcr(&["tools", "update", "cargo-nextest"]);
    for proxy in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        command.env_remove(proxy);
    }
    assert_cmd_snapshot!(command);
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_update_reports_a_descriptor_store_it_cannot_create() {
    let workspace = project();
    let manifest = declaring("{ path = \"tools/cargo-nextest.toml\" }");
    workspace
        .file("SLCR.toml", &manifest)
        .file("tools/cargo-nextest.toml", newer())
        // A file where the data directory would be.
        .file(DATA_DIR, "");
    assert_store_unavailable(&workspace, &["tools", "update", "cargo-nextest"]);
    assert_eq!(workspace.read("SLCR.toml"), manifest);
}

#[test]
fn tools_remove_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "remove", "--help"]));
}

#[test]
fn tools_remove_removes_the_entry() {
    let workspace = declared();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "remove", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
    // The descriptor stays installed: other projects may pin it.
    assert!(workspace.files().contains(&stored(NEXTEST_DESCRIPTOR)));
}

#[test]
fn tools_remove_finds_the_project_above_the_working_directory() {
    let workspace = declared();
    workspace.dir("crates/api");
    let args = ["tools", "remove", "cargo-nextest"];
    assert_cmd_snapshot!(workspace.slcr_in("crates/api", &args));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_remove_requires_a_declared_tool() {
    let workspace = project();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "remove", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), MANIFEST);
}

#[test]
fn tools_remove_requires_a_manifest() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "remove", "cargo-nextest"]));
    assert_eq!(workspace.files(), Vec::<String>::new());
}

#[test]
fn tools_remove_rejects_an_invalid_name() {
    assert_cmd_snapshot!(project().slcr(&["tools", "remove", "Nextest"]));
}

#[test]
fn tools_remove_keeps_the_rest_of_the_manifest() {
    let workspace = commented();
    let args = ["tools", "remove", "cargo-nextest"];
    assert!(workspace.slcr(&args).status().unwrap().success());
    assert_snapshot!(workspace.read("SLCR.toml"));
}

#[test]
fn tools_remove_rejects_a_misnamed_manifest() {
    let workspace = Workspace::new();
    workspace.file("slcr.toml", declaring("{ path = \"cargo-nextest.toml\" }"));
    assert_cmd_snapshot!(workspace.slcr(&["tools", "remove", "cargo-nextest"]));
}

#[test]
fn tools_remove_rejects_an_invalid_manifest() {
    let workspace = project();
    let invalid = declaring("{ path = \"../cargo-nextest.toml\" }");
    workspace.file("SLCR.toml", &invalid);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "remove", "cargo-nextest"]));
    assert_eq!(workspace.read("SLCR.toml"), invalid);
}

#[test]
fn tools_update_installs_a_descriptor_that_vet_reports_missing() {
    let workspace = declared();
    let installed = stored(NEXTEST_DESCRIPTOR);
    std::fs::remove_file(workspace.path(&installed)).unwrap();
    workspace.file("SPEC.slcr.yml", super::TODO_API_YAML);
    assert!(!workspace.slcr(&["vet"]).status().unwrap().success());
    assert_cmd_snapshot!(workspace.slcr(&["tools", "update", "cargo-nextest"]));
    assert_eq!(workspace.read(&installed), NEXTEST_DESCRIPTOR);
    assert!(workspace.slcr(&["vet"]).status().unwrap().success());
}

#[test]
fn tools_list_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "list", "--help"]));
}

/// [commented], with cargo-nextest's descriptor installed and ast-grep's
/// not.
fn half_installed() -> Workspace {
    let workspace = commented();
    workspace.install(NEXTEST_DESCRIPTOR);
    workspace
}

#[test]
fn tools_list_lists_every_declared_tool() {
    let workspace = half_installed();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "list"]));
    // Listing fetches and installs nothing.
    assert_eq!(
        workspace.files(),
        [
            stored(NEXTEST_DESCRIPTOR).as_str(),
            "SLCR.toml",
            "tools/cargo-nextest.toml"
        ]
    );
}

#[test]
fn tools_list_reports_in_json() {
    let workspace = half_installed();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "list", "--format", "json"]));
}

#[test]
fn tools_list_reports_a_manifest_without_tools() {
    assert_cmd_snapshot!(project().slcr(&["tools", "list"]));
}

#[test]
fn tools_list_requires_a_manifest() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "list"]));
}

#[test]
fn tools_list_rejects_an_unknown_format() {
    assert_cmd_snapshot!(project().slcr(&["tools", "list", "--format", "yaml"]));
}

#[test]
fn tools_show_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "show", "--help"]));
}

#[test]
fn tools_show_prints_a_declared_tool_s_descriptor() {
    let workspace = declared();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "show", "cargo-nextest"]));
}

#[test]
fn tools_show_reports_in_json() {
    let workspace = declared();
    let args = ["tools", "show", "cargo-nextest", "--format", "json"];
    assert_cmd_snapshot!(workspace.slcr(&args));
}

#[test]
fn tools_show_requires_a_declared_tool() {
    assert_cmd_snapshot!(half_installed().slcr(&["tools", "show", "jev"]));
}

#[test]
fn tools_show_reports_a_descriptor_that_is_not_installed() {
    let workspace = half_installed();
    assert_cmd_snapshot!(workspace.slcr(&["tools", "show", "ast-grep"]));
    // Nothing was fetched or installed.
    assert_eq!(workspace.files().len(), 3);
}

#[test]
fn tools_show_reports_a_corrupted_descriptor() {
    let workspace = declared();
    workspace.file(&stored(NEXTEST_DESCRIPTOR), newer());
    assert_cmd_snapshot!(workspace.slcr(&["tools", "show", "cargo-nextest"]));
}

#[test]
fn tools_show_reports_a_version_the_manifest_does_not_declare() {
    let workspace = declared();
    let manifest = workspace
        .read("SLCR.toml")
        .replace("version = \"1.0.0\"", "version = \"0.9.0\"");
    workspace.file("SLCR.toml", manifest);
    assert_cmd_snapshot!(workspace.slcr(&["tools", "show", "cargo-nextest"]));
}

#[test]
fn tools_show_requires_a_manifest() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["tools", "show", "cargo-nextest"]));
}

#[test]
fn tools_show_rejects_an_invalid_name() {
    assert_cmd_snapshot!(project().slcr(&["tools", "show", "Nextest"]));
}
