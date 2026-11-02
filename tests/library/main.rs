//! The library, through its public API alone: what a program embedding
//! SLCR can do, held to what the CLI does with the same documents. Where
//! the CLI's output is the definition, a test runs the real binary on the
//! same files and compares.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use slcr::vet::{self, Problem};
use slcr::{
    Format, Manifest, QueryError, RequirementId, SlcrRequirementsDocument, SliceError,
    UnvettedDocument,
};
use tempfile::TempDir;

const TODO_API_YAML: &str = include_str!("../fixtures/todo-api.spec.yaml");
const TODO_API_JSON: &str = include_str!("../fixtures/todo-api.spec.json");
const TODO_API_TOML: &str = include_str!("../fixtures/todo-api.spec.toml");
const TODO_API_MD: &str = include_str!("../fixtures/todo-api.md");
const TODO_API_REQ_005_MD: &str = include_str!("../fixtures/todo-api.REQ-005.md");

/// A directory holding `files`, each a path and its contents.
fn directory(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

/// Run `slcr` with `args` in `dir`, and return its stdout.
fn slcr(dir: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_slcr"))
        .args(args)
        .current_dir(dir)
        .env_remove("LOG_LEVEL")
        .env("NO_COLOR", "1")
        // The descriptor store, which these tests never fill.
        .env("XDG_DATA_HOME", dir.join(".data"))
        .env("HOME", dir.join(".home"))
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

fn canon() -> SlcrRequirementsDocument {
    Format::Json
        .parse(Path::new("todo-api.json"), TODO_API_JSON.to_owned())
        .unwrap()
}

fn unvetted(source: &str) -> UnvettedDocument {
    Format::Yaml
        .parse(Path::new("spec.yaml"), source.to_owned())
        .unwrap()
}

/// The canon example, renamed.
fn named(name: &str) -> String {
    TODO_API_YAML.replacen("name: todo-api", &format!("name: {name}"), 1)
}

// Documents

#[test]
fn documents_round_trip_in_their_canonical_form() {
    for (format, source) in [
        (Format::Json, TODO_API_JSON),
        (Format::Yaml, TODO_API_YAML),
        (Format::Toml, TODO_API_TOML),
    ] {
        let document: SlcrRequirementsDocument = format
            .parse(Path::new("todo-api"), source.to_owned())
            .unwrap();
        assert_eq!(document, canon(), "{format}");
        // Written in its canonical form, it reads back as the same
        // document, and is written the same way again.
        let written = format.serialize(&document).unwrap();
        let read: SlcrRequirementsDocument = format
            .parse(Path::new("todo-api"), written.clone())
            .unwrap();
        assert_eq!(read, document, "{format}");
        assert_eq!(format.serialize(&read).unwrap(), written, "{format}");
    }
    // Each format's canonical form holds the same JSON data.
    let json: Value = serde_json::from_str(&Format::Json.serialize(&canon()).unwrap()).unwrap();
    assert_eq!(json, serde_json::from_str::<Value>(TODO_API_JSON).unwrap());
}

#[test]
fn yaml_scalars_that_are_not_strings_are_rejected() {
    let quoted = [
        ("formatVersion: \"1\"", "formatVersion: 1"),
        ("title: TodoList API", "title: 2024"),
        ("title: TodoList API", "title: true"),
        ("title: TodoList API", "title: 1.5"),
    ];
    for (canonical, untyped) in quoted {
        assert!(TODO_API_YAML.contains(canonical), "{canonical}");
        let source = TODO_API_YAML.replacen(canonical, untyped, 1);
        let (outcome, document) = vet::source("todo-api.yaml", Format::Yaml, source);
        assert!(document.is_none(), "{untyped}");
        assert!(!outcome.passes(false), "{untyped}");
    }
    // Quoted, the same values are strings; YAML 1.2 reads `yes` as one.
    for title in ["\"2024\"", "'true'", "yes"] {
        let source = TODO_API_YAML.replacen("title: TodoList API", &format!("title: {title}"), 1);
        let (outcome, document) = vet::source("todo-api.yaml", Format::Yaml, source);
        assert!(outcome.passes(true), "{title}");
        assert!(document.is_some(), "{title}");
    }
}

// Vetting

#[test]
fn files_vet_as_slcr_vet_reports_them() {
    let with_violation = TODO_API_YAML.replace("refines: [REQ-008]", "refines: [REQ-099]");
    let with_warning =
        TODO_API_YAML.replace("The endpoint MUST accept GET", "The endpoint accepts GET");
    let unknown_field = TODO_API_YAML.replacen("  title: TodoList API", "  titel: TodoList API", 1);
    let untyped = TODO_API_YAML.replacen("formatVersion: \"1\"", "formatVersion: 1", 1);
    let not_yaml = "root: [unclosed".to_owned();
    for (name, source) in [
        ("canon", TODO_API_YAML.to_owned()),
        ("violation", with_violation),
        ("warning", with_warning),
        ("unknown-field", unknown_field),
        ("untyped", untyped),
        ("not-yaml", not_yaml),
    ] {
        let path = format!("{name}.yaml");
        let dir = directory(&[(&path, &source)]);
        let cli = slcr(dir.path(), &["vet", "--format", "json", &path]);
        let (outcome, _) = vet::source(path.as_str(), Format::Yaml, source);
        let library = vet::json(None, &[outcome], false) + "\n";
        assert_eq!(library, cli, "{name}");
    }
}

#[test]
fn a_document_already_read_vets_as_its_source_does() {
    let document = unvetted(&TODO_API_YAML.replace("The endpoint MUST", "The endpoint"));
    let (from_source, _) = vet::source(
        "todo-api.yaml",
        Format::Yaml,
        TODO_API_YAML.replace("The endpoint MUST", "The endpoint"),
    );
    let read = vet::document("todo-api.yaml", &document);
    assert_eq!(
        vet::json(None, &[read], false),
        vet::json(None, &[from_source], false)
    );
    let outcomes = [vet::document("todo-api.yaml", &document)];
    assert!(vet::report(None, &outcomes, false).passed());
    assert!(!vet::report(None, &outcomes, true).passed());
}

/// The files of a project, written out, and the same project in memory:
/// its manifest, and each specification at its path.
fn project(
    manifest: &str,
    specs: &[(&str, &str)],
) -> (TempDir, Manifest, Vec<(PathBuf, UnvettedDocument)>) {
    let mut files = vec![("SLCR.toml", manifest)];
    files.extend_from_slice(specs);
    let dir = directory(&files);
    let manifest = Manifest::parse(manifest).unwrap();
    let specs = specs
        .iter()
        .map(|(path, source)| {
            let format = Format::from_extension(path.rsplit('.').next().unwrap()).unwrap();
            let document = format.parse(Path::new(path), (*source).to_owned()).unwrap();
            (PathBuf::from(path), document)
        })
        .collect();
    (dir, manifest, specs)
}

/// A project to vet: its name, its manifest, and each specification's path
/// and contents.
type ProjectCase = (&'static str, &'static str, Vec<(&'static str, String)>);

#[test]
fn projects_vet_as_slcr_vet_reports_their_files() {
    // Each case: its name, its manifest, and its specifications.
    let cases: [ProjectCase; 4] = [
        (
            "clean",
            "formatVersion = \"1\"\nspecs = [\"*.slcr.yaml\"]\n",
            vec![("SPEC.slcr.yaml", TODO_API_YAML.to_owned())],
        ),
        (
            "no match",
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yaml\", \"docs/*.slcr.toml\"]\n",
            vec![("SPEC.slcr.yaml", TODO_API_YAML.to_owned())],
        ),
        (
            "duplicate names and a warning",
            "formatVersion = \"1\"\nspecs = [\"**/*.yaml\"]\n",
            vec![
                ("a/one.yaml", TODO_API_YAML.to_owned()),
                (
                    "b/two.yaml",
                    TODO_API_YAML.replace("The endpoint MUST", "The endpoint"),
                ),
                ("c/other.yaml", named("other")),
            ],
        ),
        (
            "a match in another format",
            "formatVersion = \"1\"\nspecs = [\"docs/*\"]\n",
            vec![
                ("docs/api.json", TODO_API_JSON.to_owned()),
                ("docs/web.toml", TODO_API_TOML.replace("todo-api", "web")),
            ],
        ),
    ];
    for (name, manifest, specs) in cases {
        let specs: Vec<(&str, &str)> = specs.iter().map(|(p, s)| (*p, s.as_str())).collect();
        let (dir, manifest, specs) = project(manifest, &specs);
        let cli = slcr(dir.path(), &["vet", "--format", "json"]);
        let (project, outcomes) = vet::project(&manifest, specs);
        let library = vet::json(Some(&project), &outcomes, false) + "\n";
        assert_eq!(library, cli, "{name}");
    }
}

#[test]
fn a_specification_no_pattern_matches_is_reported() {
    let (_dir, manifest, specs) = project(
        "formatVersion = \"1\"\nspecs = [\"docs/*.slcr.toml\"]\n",
        &[("SPEC.slcr.yaml", TODO_API_YAML)],
    );
    let (project, outcomes) = vet::project(&manifest, specs);
    assert!(!project.passes());
    assert!(outcomes.is_empty());
    assert_eq!(
        project.result().unwrap(),
        [
            Problem::NoMatch {
                pattern: "docs/*.slcr.toml".to_owned()
            },
            Problem::Unlisted {
                path: PathBuf::from("SPEC.slcr.yaml")
            },
        ]
    );
    let json: Value = serde_json::from_str(&vet::json(Some(&project), &outcomes, false)).unwrap();
    assert_eq!(
        json["project"]["problems"][1],
        json!({
            "code": "unlisted",
            "path": "SPEC.slcr.yaml",
            "message": "no `specs` pattern matches the specification SPEC.slcr.yaml",
            "help": "add a pattern that matches it to `specs`, or delete the specification",
        })
    );
    assert_eq!(json["validators"], json!([]));
}

#[test]
fn a_project_without_a_manifest_is_reported_as_unloadable() {
    let project = vet::unloadable_project(miette::miette!("the project has no SLCR.toml"));
    assert!(!project.passes());
    let json: Value = serde_json::from_str(&vet::json(Some(&project), &[], false)).unwrap();
    assert_eq!(
        json,
        json!({
            "passed": false,
            "project": {
                "manifest": "SLCR.toml",
                "passed": false,
                "error": { "message": "the project has no SLCR.toml" },
            },
            "files": [],
            "validators": [],
        })
    );
}

// Rendering, slicing, and querying

#[test]
fn documents_render_as_slcr_render_writes_them() {
    assert_eq!(slcr::render(&canon()), TODO_API_MD);
}

#[test]
fn requirements_slice_as_slcr_slice_writes_them() {
    let requirement: RequirementId = "REQ-005".parse().unwrap();
    let slice = block_on(slcr::slice(&canon(), requirement)).unwrap();
    assert_eq!(slice, TODO_API_REQ_005_MD);
}

#[test]
fn slicing_a_requirement_the_document_lacks_fails() {
    let requirement: RequirementId = "REQ-999".parse().unwrap();
    let error = block_on(slcr::slice(&canon(), requirement)).unwrap_err();
    assert!(matches!(error, SliceError::UnknownRequirement { .. }));
    assert_eq!(
        error.to_string(),
        "the specification has no requirement REQ-999"
    );
}

#[test]
fn queries_return_rows_as_slcr_query_writes_them() {
    let dir = directory(&[("todo-api.json", TODO_API_JSON)]);
    for (text, params) in [
        (
            "MATCH (r:Requirement) RETURN r.id, r.title ORDER BY r.id",
            json!({}),
        ),
        (
            "MATCH (r:Requirement)-[e:REFINES]->(p) WHERE p.id = $id RETURN r, e ORDER BY r.id",
            json!({ "id": "REQ-001" }),
        ),
    ] {
        let mut args = vec!["query", "--format", "json"];
        let bound: Vec<String> = params
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        for param in &bound {
            args.extend(["--param", param]);
        }
        args.extend([text, "todo-api.json"]);
        let cli = slcr(dir.path(), &args);
        let params: Map<String, Value> = params.as_object().unwrap().clone();
        let rows = block_on(slcr::query(&canon(), text, params, None)).unwrap();
        assert_eq!(rows.to_json(), cli, "{text}");
    }
}

#[test]
fn a_query_that_cant_run_reports_where_its_error_is() {
    let error = block_on(slcr::query(&canon(), "MATCH (r RETURN r", Map::new(), None)).unwrap_err();
    let QueryError::Invalid(invalid) = &error else {
        panic!("{error:?}");
    };
    let json = serde_json::to_value(vet::ErrorJson::of(invalid)).unwrap();
    assert_eq!(json["message"], "the query is not valid GQL");
    let label = &json["labels"][0];
    assert_eq!(label["line"], 1);
    assert!(label["column"].as_u64().unwrap() > 1, "{json}");
}

#[test]
fn a_query_still_running_at_its_deadline_is_stopped() {
    let deadline = Instant::now() - Duration::from_secs(1);
    let error = block_on(slcr::query(
        &canon(),
        "MATCH (r:Requirement) RETURN r.id",
        Map::new(),
        Some(deadline),
    ))
    .unwrap_err();
    assert!(matches!(error, QueryError::DeadlineReached), "{error:?}");
    // With time to spare, the deadline changes nothing.
    let deadline = Instant::now() + Duration::from_secs(60);
    let text = "MATCH (r:Requirement) RETURN r.id ORDER BY r.id";
    let bounded = block_on(slcr::query(&canon(), text, Map::new(), Some(deadline))).unwrap();
    let unbounded = block_on(slcr::query(&canon(), text, Map::new(), None)).unwrap();
    assert_eq!(bounded, unbounded);
}

// Manifests

const MANIFEST: &str = r#"# Comments and layout are not kept.
formatVersion = "1"
specs = ["docs/*.slcr.toml", "SPEC.slcr.toml"]

[tools.cargo-nextest]
version = "1.0.0"
source = { url = "https://example.com/descriptors/cargo-nextest/1.0.0.toml" }
pin = "blake3:9f2c4e1a7b3d5f60812a4c6e8f0b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b"

[tools.ast-grep]
version = "0.3.1"
source = { path = "tools/descriptors/ast-grep.toml" }
pin = "blake3:41ab9c7e5d3f1a0b2c4d6e8f0a1b3c5d7e9f1a2b4c6d8e0f2a3b5c7d9e1f3a5b"
"#;

#[test]
fn manifests_are_written_as_slcr_tools_add_writes_them() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    let toml = manifest.to_toml();
    assert_eq!(
        toml,
        r#"formatVersion = "1"
specs = ["docs/*.slcr.toml", "SPEC.slcr.toml"]

[tools.ast-grep]
version = "0.3.1"
source = { path = "tools/descriptors/ast-grep.toml" }
pin = "blake3:41ab9c7e5d3f1a0b2c4d6e8f0a1b3c5d7e9f1a2b4c6d8e0f2a3b5c7d9e1f3a5b"

[tools.cargo-nextest]
version = "1.0.0"
source = { url = "https://example.com/descriptors/cargo-nextest/1.0.0.toml" }
pin = "blake3:9f2c4e1a7b3d5f60812a4c6e8f0b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b"
"#
    );
    assert_eq!(Manifest::parse(&toml).unwrap(), manifest);
    let specs: Vec<&str> = manifest.specs().iter().map(|glob| glob.as_str()).collect();
    assert_eq!(specs, ["docs/*.slcr.toml", "SPEC.slcr.toml"]);
}

#[test]
fn manifests_serialize_in_their_canonical_form() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    assert_eq!(
        serde_json::to_value(&manifest).unwrap(),
        json!({
            "formatVersion": "1",
            "specs": ["docs/*.slcr.toml", "SPEC.slcr.toml"],
            "tools": {
                "ast-grep": {
                    "version": "0.3.1",
                    "source": { "path": "tools/descriptors/ast-grep.toml" },
                    "pin": "blake3:41ab9c7e5d3f1a0b2c4d6e8f0a1b3c5d7e9f1a2b4c6d8e0f2a3b5c7d9e1f3a5b",
                },
                "cargo-nextest": {
                    "version": "1.0.0",
                    "source": { "url": "https://example.com/descriptors/cargo-nextest/1.0.0.toml" },
                    "pin": "blake3:9f2c4e1a7b3d5f60812a4c6e8f0b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b",
                },
            },
        })
    );
    let bare = Manifest::parse("formatVersion = \"1\"\nspecs = [\"SPEC.slcr.toml\"]\n").unwrap();
    let json = serde_json::to_string(&bare).unwrap();
    assert_eq!(json, r#"{"formatVersion":"1","specs":["SPEC.slcr.toml"]}"#);
    let read: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(read, bare);
}

#[test]
fn invalid_manifests_are_rejected_with_where_the_problem_is() {
    for (name, source) in [
        (
            "unknown key",
            "formatVersion = \"1\"\nspecs = [\"a.toml\"]\nextra = 1\n",
        ),
        ("missing specs", "formatVersion = \"1\"\n"),
        (
            "bad pin",
            "formatVersion = \"1\"\nspecs = [\"a.toml\"]\n\n[tools.t]\nversion = \"1.0.0\"\n\
             source = { path = \"t.toml\" }\npin = \"sha256:00\"\n",
        ),
    ] {
        let error = Manifest::parse(source).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("could not parse SLCR.toml as TOML"),
            "{name}: {error}"
        );
        let json = serde_json::to_value(vet::ErrorJson::of(&error)).unwrap();
        assert!(json["message"].is_string(), "{name}: {json}");
    }
}

#[test]
fn manifests_and_projects_never_fetch_descriptors() {
    // A server that would see any connection made to it.
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let port = server.local_addr().unwrap().port();
    let manifest = format!(
        "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yaml\"]\n\n\
         [tools.remote]\nversion = \"1.0.0\"\n\
         source = {{ url = \"https://127.0.0.1:{port}/remote.toml\" }}\n\
         pin = \"blake3:{zeros}\"\n\n\
         [tools.local]\nversion = \"1.0.0\"\n\
         source = {{ path = \"nowhere/at/all.toml\" }}\n\
         pin = \"blake3:{zeros}\"\n",
        zeros = "0".repeat(64),
    );
    let manifest = Manifest::parse(&manifest).unwrap();
    let written = Manifest::parse(&manifest.to_toml()).unwrap();
    assert_eq!(written, manifest);
    let specs = vec![(PathBuf::from("SPEC.slcr.yaml"), unvetted(TODO_API_YAML))];
    let (project, outcomes) = vet::project(&manifest, specs);
    assert!(project.passes(), "{:?}", project.result());
    assert!(outcomes.iter().all(|outcome| outcome.passes(true)));
    assert!(!Path::new("nowhere").exists());
    let accepted = server.accept();
    assert!(
        matches!(&accepted, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "{accepted:?}"
    );
}

#[test]
fn check_reports_read_back_as_check_wrote_them() {
    for snapshot in [
        include_str!("../cli/snapshots/cli__check__check_reports_in_json.snap"),
        include_str!("../cli/snapshots/cli__check__check_reports_a_vetting_failure_in_json.snap"),
    ] {
        // The stdout `check` wrote, with its redacted durations put back.
        let start = snapshot.find("----- stdout -----\n").unwrap() + 19;
        let end = snapshot.find("----- stderr -----").unwrap();
        let written = snapshot[start..end].replace("\"[DURATION]\"", "1234");
        let report = slcr::check::Report::from_json(&written).unwrap();
        let original: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(serde_json::to_value(&report).unwrap(), original);
        if let Some(specs) = &report.specs {
            assert!(specs.iter().all(|spec| !spec.requirements.is_empty()));
        }
    }
    for document in [
        json!({}),
        json!({ "passed": true, "interrupted": false, "extra": 1 }),
        json!({
            "passed": true,
            "interrupted": false,
            "specs": [{
                "name": "shop", "file": "SPEC.slcr.yml", "passed": true,
                "summary": {
                    "satisfied": 1, "unsatisfied": 0, "unknown": 0, "violations": 0,
                    "warnings": 0, "informational": 0, "coverageGaps": 0
                },
                "requirements": [{ "id": "REQ-001", "status": "green" }],
                "findings": [], "coverageGaps": []
            }]
        }),
        json!({
            "passed": true,
            "interrupted": false,
            "validators": [{
                "file": ".validators.slcr.toml", "position": 1, "requirement": "REQ-001",
                "tool": "sh", "outcome": "green"
            }]
        }),
    ] {
        assert!(
            slcr::check::Report::from_json(&document.to_string()).is_err(),
            "{document}"
        );
    }
}

/// Run `future` to completion on a runtime of its own.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}
