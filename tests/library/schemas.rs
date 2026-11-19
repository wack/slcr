//! The JSON Schemas of the documents the library reads and writes, behind
//! the `schemars` feature: what a program that generates types from them
//! relies on. They are held to what `slcr` really writes (the documents of
//! the CLI's own snapshot tests, and of runs here), to what the parsers
//! accept, and to the shape of each result.

use std::path::{Path, PathBuf};
use std::process::Command;

use jsonschema::Validator;
use miette::Diagnostic;
use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use slcr::check::Report as CheckReport;
use slcr::vet::{self, ErrorJson};
use slcr::{
    CriterionId, DescriptorStore, DescriptorVersion, Fetcher, Format, Initialized, Installed,
    Manifest, Pin, Project, RequirementId, SectionId, SpecName, TermId, Title, ToolAdded, ToolKey,
    ToolRemoved, ToolUpdated, UnvettedDocument,
};
use tempfile::TempDir;

use super::{TODO_API_YAML, block_on};

/// The schema of `T`, as a program that generates types gets it with
/// `schema_for!`, which is the schema of how `T` is read.
fn schema_of<T: JsonSchema + ?Sized>() -> Value {
    let read = SchemaGenerator::default().into_root_schema_for::<T>();
    // A schema must not depend on whether it describes reading or writing:
    // what the writer leaves out is optional, and never null, to either.
    let written = SchemaSettings::default()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    assert_eq!(read, written, "{} differs by contract", T::schema_name());
    canonical(read.to_value())
}

/// `value` with every object's keys in order, so that a schema reads the
/// same whatever order `serde_json` keeps them in.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

fn pretty(schema: &Value) -> String {
    serde_json::to_string_pretty(schema).unwrap()
}

fn validator(schema: &Value) -> Validator {
    jsonschema::draft202012::new(schema).expect("the schema is a valid JSON Schema")
}

/// Why `document` doesn't satisfy `validator`, if it doesn't.
fn errors(validator: &Validator, document: &Value) -> Vec<String> {
    validator
        .iter_errors(document)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect()
}

#[track_caller]
fn assert_valid(validator: &Validator, document: &Value, what: &str) {
    let errors = errors(validator, document);
    assert!(
        errors.is_empty(),
        "{what} is not valid: {errors:#?}\n{}",
        pretty(document)
    );
}

// What the CLI writes

/// The JSON a snapshot of the CLI's tests holds in its stdout, and the
/// arguments `slcr` ran with.
fn snapshots() -> Vec<(String, Vec<String>, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cli/snapshots");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "snap")
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let header = text.split("\n---\n").next()?;
            let args: Vec<String> = header
                .lines()
                .skip_while(|line| *line != "  args:")
                .skip(1)
                .take_while(|line| line.starts_with("    - "))
                .map(|line| line[6..].trim_matches('"').to_owned())
                .collect();
            let start = text.find("----- stdout -----\n")? + "----- stdout -----\n".len();
            let end = text.find("----- stderr -----")?;
            // Durations are redacted in the snapshots; put numbers back.
            let stdout = text[start..end].replace("\"[DURATION]\"", "1234");
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some((name, args, stdout))
        })
        .collect()
}

/// The documents the CLI's snapshot tests hold in a stdout that `matches`,
/// by the snapshot's name.
fn written_where(matches: fn(&Value) -> bool) -> Vec<(String, Value)> {
    snapshots()
        .into_iter()
        .filter_map(|(name, _, stdout)| Some((name, serde_json::from_str::<Value>(&stdout).ok()?)))
        .filter(|(_, document)| matches(document))
        .collect()
}

/// What `slcr vet --format json` writes: a verdict and every file.
fn is_vet(document: &Value) -> bool {
    document.get("passed").is_some() && document.get("files").is_some_and(Value::is_array)
}

/// What `slcr check --format json` writes: a verdict, and whether it was
/// interrupted.
fn is_check(document: &Value) -> bool {
    document.get("passed").is_some() && document.get("interrupted").is_some()
}

/// A directory `slcr` runs in, with its data directory in it.
struct Workspace {
    dir: TempDir,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Self {
        Self {
            dir: super::directory(files),
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    /// What `slcr` with `args` writes to stdout.
    fn slcr(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_slcr"))
            .args(args)
            .current_dir(self.dir.path())
            .env_remove("LOG_LEVEL")
            .env("NO_COLOR", "1")
            .env("XDG_DATA_HOME", self.path(".data"))
            .env("HOME", self.path(".home"))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    /// The document `slcr` with `args` writes.
    fn document(&self, args: &[&str]) -> Value {
        let written = self.slcr(args);
        serde_json::from_str(&written).unwrap_or_else(|error| panic!("{error}: {written}"))
    }
}

/// A tool that runs `with.script` with `sh -c`.
const SH: &str = r#"formatVersion = "1"
name = "sh"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["script"]
properties.script = { type = "string" }

[invoke]
argv = ["sh", "-c", "{with.script}"]
cwd = "validator-dir"
timeout = 60

[results]
pass = [0]
"#;

/// A tool whose program is on no PATH.
const MISSING: &str = r#"formatVersion = "1"
name = "missing"
version = "1.0.0"

[validator.schema]
type = "object"

[invoke]
argv = ["slcr-no-such-program"]
cwd = "project"

[results]
pass = [0]
"#;

fn pin(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

/// A project of the canon specification, declaring `sh` and `missing`
/// from descriptors in it, with a validator for each, to be installed by
/// the first command that fetches. `scripts` is run by `sh`.
fn project_checking(script: &str) -> Workspace {
    let manifest = format!(
        "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n\
         [tools.sh]\nversion = \"1.0.0\"\nsource = {{ path = \"tools/sh.toml\" }}\n\
         pin = \"{}\"\n\n\
         [tools.missing]\nversion = \"1.0.0\"\nsource = {{ path = \"tools/missing.toml\" }}\n\
         pin = \"{}\"\n",
        pin(SH),
        pin(MISSING)
    );
    let validators = format!(
        "formatVersion = \"1\"\n\n\
         [[validator]]\nrequirement = \"todo-api/REQ-002\"\ntool = \"sh\"\n\
         with = {{ script = {script:?} }}\n\n\
         [[validator]]\nrequirement = \"todo-api/REQ-003\"\ntool = \"missing\"\n"
    );
    Workspace::new(&[
        ("SLCR.toml", &manifest),
        ("SPEC.slcr.yml", TODO_API_YAML),
        ("tools/sh.toml", SH),
        ("tools/missing.toml", MISSING),
        (".validators.slcr.toml", &validators),
    ])
}

// Documents

/// The check documents the CLI's tests snapshot, and what the CLI writes
/// for a validator that can't run, a red one, and a green one.
fn check_documents() -> Vec<(String, Value)> {
    let mut documents = written_where(is_check);
    let workspace = project_checking("exit 3");
    let mut run = workspace.document(&["check", "--format", "json"]);
    for validator in run["validators"].as_array_mut().unwrap() {
        validator.as_object_mut().unwrap().remove("durationMs");
    }
    documents.push(("a run of red and unrunnable validators".to_owned(), run));
    documents
}

/// The vet documents the CLI's tests snapshot, each check's embedded
/// one, and those of every finding and problem the library can report.
fn vet_documents() -> Vec<(String, Value)> {
    let mut documents = written_where(is_vet);
    let library = findings().into_iter().chain(problems());
    documents.extend(
        library
            .enumerate()
            .map(|(index, document)| (format!("the library's document {index}"), document)),
    );
    for (name, check) in check_documents() {
        if let Some(vet) = check.get("vet") {
            documents.push((format!("{name}, vetted"), vet.clone()));
        }
    }
    documents
}

#[test]
fn the_vet_report_schema_accepts_every_document_slcr_vet_writes() {
    let schema = schema_of::<vet::Report<'static>>();
    let validator = validator(&schema);
    let documents = vet_documents();
    // The CLI's tests cover passing and failing files, files that can't be
    // loaded, projects with every kind of problem, and validator files.
    assert!(documents.len() >= 8, "{} vet documents", documents.len());
    let has = |key: &str| {
        documents
            .iter()
            .any(|(_, document)| document.get(key).is_some())
    };
    assert!(has("project") && has("validators"));
    assert!(
        documents
            .iter()
            .any(|(_, document)| document["passed"] == true)
    );
    assert!(
        documents
            .iter()
            .any(|(_, document)| document["passed"] == false)
    );
    for (name, document) in &documents {
        assert_valid(&validator, document, name);
    }
}

#[test]
fn the_vet_report_schema_accepts_what_the_library_writes() {
    let schema = schema_of::<vet::Report<'static>>();
    let validator = validator(&schema);
    let broken = TODO_API_YAML.replace("refines: [REQ-008]", "refines: [REQ-099]");
    assert_ne!(broken, TODO_API_YAML);
    let (clean, _) = vet::source("todo-api.yaml", Format::Yaml, TODO_API_YAML.to_owned());
    let (findings, _) = vet::source("broken.yaml", Format::Yaml, broken);
    let (unreadable, _) = vet::source("bad.yaml", Format::Yaml, "root: [".to_owned());
    let outcomes = [clean, findings, unreadable];
    for deny_warnings in [false, true] {
        let written = vet::json(None, &outcomes, deny_warnings);
        let document: Value = serde_json::from_str(&written).unwrap();
        assert_valid(&validator, &document, "vet::json");
        // The document the library hands out serializes the same.
        let report = vet::report(None, &outcomes, deny_warnings);
        assert_eq!(serde_json::to_value(&report).unwrap(), document);
    }
}

/// A requirement of a [demo]: its ID, modality, refinement, the IDs it
/// refines, and its body.
type Requirement<'a> = (&'a str, &'a str, Option<&'a str>, &'a [&'a str], &'a str);

/// A specification named `demo`, with `requirements`, and the glossary's
/// `terms`.
fn demo(requirements: &[Requirement], terms: &[&str]) -> String {
    let mut text = String::from(
        "formatVersion: \"1\"\nspec:\n  name: demo\nroot:\n  kind: section\n  id: SEC-001\n  \
         title: Demo\n  children:\n",
    );
    for (id, modality, refinement, refines, body) in requirements {
        text.push_str(&format!(
            "    - kind: requirement\n      id: {id}\n      title: Title {id}\n      \
             body: {body}\n      modality: {modality}\n"
        ));
        if let Some(refinement) = refinement {
            text.push_str(&format!("      refinement: {refinement}\n"));
        }
        if !refines.is_empty() {
            text.push_str(&format!("      refines: [{}]\n", refines.join(", ")));
        }
    }
    text.push_str("glossary:\n  kind: section\n  id: SEC-002\n  title: Glossary\n");
    if !terms.is_empty() {
        text.push_str("  terms:\n");
        for term in terms {
            text.push_str(&format!(
                "    - kind: term\n      id: {term}\n      title: Term {term}\n      \
                 definition: A term.\n"
            ));
        }
    }
    text
}

/// Every `code` that `schema` knows.
fn codes_in(schema: &Value) -> std::collections::BTreeSet<String> {
    fn walk(value: &Value, found: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                let code = map
                    .get("properties")
                    .and_then(|properties| properties.get("code"))
                    .and_then(|code| code.get("const"))
                    .and_then(Value::as_str);
                found.extend(code.map(str::to_owned));
                map.values().for_each(|value| walk(value, found));
            }
            Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut found = Default::default();
    walk(schema, &mut found);
    found
}

/// Every `code` that `documents` have.
fn codes(documents: &[Value]) -> std::collections::BTreeSet<String> {
    fn walk(value: &Value, found: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                found.extend(map.get("code").and_then(Value::as_str).map(str::to_owned));
                map.values().for_each(|value| walk(value, found));
            }
            Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut found = Default::default();
    documents
        .iter()
        .for_each(|document| walk(document, &mut found));
    found
}

/// `slcr vet`'s documents for specifications that each break, or warn of,
/// one invariant, vetted by the library.
fn findings() -> Vec<Value> {
    let leaf = |id, body| (id, "MUST", None, &[][..], body);
    let documents = [
        demo(
            &[
                leaf("REQ-001", "It MUST work."),
                leaf("REQ-001", "It MUST work."),
            ],
            &[],
        ),
        demo(
            &[("REQ-001", "MUST", None, &["REQ-099"], "It MUST work.")],
            &["TERM-001"],
        ),
        demo(
            &[
                (
                    "REQ-001",
                    "MUST",
                    Some("AND"),
                    &["REQ-002"],
                    "It MUST work.",
                ),
                (
                    "REQ-002",
                    "MUST",
                    Some("AND"),
                    &["REQ-001"],
                    "It MUST work.",
                ),
            ],
            &[],
        ),
        demo(
            &[
                leaf("REQ-001", "It MUST work."),
                ("REQ-002", "MUST", None, &["REQ-001"], "It MUST work."),
            ],
            &[],
        ),
        demo(
            &[("REQ-001", "MUST", Some("OR"), &[], "It MUST work.")],
            &[],
        ),
        demo(
            &[leaf("REQ-001", "It MUST work.")],
            &["TERM-002", "TERM-001"],
        ),
        demo(&[leaf("REQ-001", "It works.")], &[]),
        demo(
            &[
                ("REQ-001", "MUST", Some("AND"), &[], "It MUST work."),
                ("REQ-002", "MAY", None, &["REQ-001"], "It MAY work."),
            ],
            &[],
        ),
    ];
    documents
        .into_iter()
        .map(|text| {
            let (outcome, _) = vet::source("demo.yaml", Format::Yaml, text);
            serde_json::from_str(&vet::json(None, &[outcome], false)).unwrap()
        })
        .collect()
}

/// `slcr vet`'s documents for projects with every problem the library can
/// find in one held in memory, and on disk.
fn problems() -> Vec<Value> {
    let demo = demo(&[("REQ-001", "MUST", None, &[], "It MUST work.")], &[]);
    let read = |path: &str| {
        Format::Yaml
            .parse::<UnvettedDocument>(Path::new(path), demo.clone())
            .unwrap()
    };
    let manifest =
        Manifest::parse("formatVersion = \"1\"\nspecs = [\"[\", \"nowhere/*.yaml\", \"docs/*\"]\n")
            .unwrap();
    let specs = [
        "docs/a.yaml",
        "docs/b.yaml",
        "docs/notes.md",
        "docs/.validators.slcr.toml",
        "elsewhere/c.yaml",
    ]
    .map(|path| (PathBuf::from(path), read(path)))
    .to_vec();
    let (project, outcomes) = vet::project(&manifest, specs);
    let in_memory: Value =
        serde_json::from_str(&vet::json(Some(&project), &outcomes, false)).unwrap();

    // Two validator files in one directory, and descriptors the store
    // holds damaged: changed, unreadable, and not a descriptor.
    let invalid = "not = \"a descriptor\"\n";
    let entry = |key: &str, text: &str| {
        format!(
            "\n[tools.{key}]\nversion = \"1.0.0\"\nsource = {{ path = \"tools/{key}.toml\" }}\n\
             pin = \"{}\"\n",
            pin(text)
        )
    };
    let manifest = format!(
        "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n{}{}{}",
        entry("changed", SH),
        entry("unreadable", MISSING),
        entry("invalid", invalid)
    );
    let validators = "formatVersion = \"1\"\n";
    let workspace = Workspace::new(&[
        ("SLCR.toml", &manifest),
        ("SPEC.slcr.yml", &demo),
        ("a/.validators.slcr.toml", validators),
        ("a/.validators.slcr.yaml", "formatVersion: \"1\"\n"),
        (".store/placeholder", ""),
    ]);
    let root = workspace.path(".").canonicalize().unwrap();
    let file = |text: &str| {
        root.join(".store")
            .join(format!("{}.toml", &pin(text)[7..]))
    };
    std::fs::write(file(SH), "tampered").unwrap();
    std::fs::create_dir(file(MISSING)).unwrap();
    std::fs::write(file(invalid), invalid).unwrap();
    let project = Project::open(&root, &root).unwrap();
    let (outcome, outcomes) = project.vet(&DescriptorStore::at(root.join(".store")));
    let on_disk: Value =
        serde_json::from_str(&vet::json(Some(&outcome), &outcomes, false)).unwrap();

    // What an unreadable directory gives, which a test can't make as the
    // user who may read anything: the problem as the library serializes it.
    let unsearchable = vet::Problem::Unsearchable {
        path: PathBuf::from("docs"),
        reason: "permission denied".to_owned(),
    };
    let mut problem = serde_json::to_value(&unsearchable).unwrap();
    problem["message"] = json!(unsearchable.to_string());
    problem["help"] = unsearchable
        .help()
        .map_or(Value::Null, |help| json!(help.to_string()));
    if problem["help"].is_null() {
        problem.as_object_mut().unwrap().remove("help");
    }
    let unsearchable = json!({
        "passed": false,
        "project": { "manifest": "SLCR.toml", "passed": false, "problems": [problem] },
        "files": [],
    });
    vec![in_memory, on_disk, unsearchable]
}

#[test]
fn the_vet_report_schema_accepts_every_finding_and_problem_the_library_reports() {
    let schema = schema_of::<vet::Report<'static>>();
    let validator = validator(&schema);
    let documents: Vec<Value> = vet_documents()
        .into_iter()
        .map(|(name, document)| {
            assert_valid(&validator, &document, &name);
            document
        })
        .collect();
    // Each code the schema knows is written by a real run, or the library.
    let known = codes_in(&schema);
    let written = codes(&documents);
    assert!(known.len() >= 28, "{known:?}");
    let missing: Vec<_> = known.difference(&written).collect();
    assert!(missing.is_empty(), "no document has a {missing:?}");
    let unknown: Vec<_> = written.difference(&known).collect();
    assert!(unknown.is_empty(), "the schema doesn't know {unknown:?}");
}

#[test]
fn the_check_report_schema_accepts_every_document_slcr_check_writes() {
    let schema = schema_of::<CheckReport>();
    let validator = validator(&schema);
    let documents = check_documents();
    assert!(documents.len() >= 3, "{} check documents", documents.len());
    for (name, document) in &documents {
        assert_valid(&validator, document, name);
        // The reader and the schema agree.
        let report = CheckReport::from_json(&document.to_string())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(&serde_json::to_value(&report).unwrap(), document, "{name}");
    }
    // A validator that couldn't run has the error `slcr` writes for it.
    let (_, run) = documents.last().unwrap();
    let outcomes: Vec<&Value> = run["validators"]
        .as_array()
        .unwrap()
        .iter()
        .map(|validator| &validator["outcome"])
        .collect();
    assert_eq!(outcomes, ["red", "could-not-run"]);
    assert!(run["validators"][1]["error"]["message"].is_string());
}

#[test]
fn the_check_report_schema_accepts_what_the_library_checks() {
    let schema = schema_of::<CheckReport>();
    let validator = validator(&schema);
    let workspace = project_checking("exit 0");
    let (project, store) = open(&workspace);
    block_on(async {
        let fetcher = Fetcher::new();
        let installed = project.install(&store, &fetcher).await.unwrap();
        assert!(installed.passed());
        let options = slcr::check::Options::default();
        // To the end, and cancelled before it starts.
        let finished = project
            .check(&store, &options, |_| {}, std::future::pending())
            .await;
        let cancelled = project
            .check(&store, &options, |_| {}, std::future::ready(()))
            .await;
        assert!(!finished.interrupted && cancelled.interrupted);
        for (what, report) in [("finished", finished), ("cancelled", cancelled)] {
            let document = serde_json::to_value(&report).unwrap();
            assert_valid(&validator, &document, what);
        }
    });
}

/// `workspace`'s project, and a store beside it.
fn open(workspace: &Workspace) -> (Project, DescriptorStore) {
    let root = workspace.path(".").canonicalize().unwrap();
    let store = DescriptorStore::at(root.join(".store"));
    (Project::open(&root, &root).unwrap(), store)
}

/// Every object of `document`, as the JSON pointer to it.
fn objects(document: &Value, at: &str, found: &mut Vec<String>) {
    match document {
        Value::Object(map) => {
            found.push(at.to_owned());
            for (key, value) in map {
                let key = key.replace('~', "~0").replace('/', "~1");
                objects(value, &format!("{at}/{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                objects(value, &format!("{at}/{index}"), found);
            }
        }
        _ => {}
    }
}

/// Every scalar of `document`, as the JSON pointer to it.
fn scalars(document: &Value, at: &str, found: &mut Vec<String>) {
    match document {
        Value::Object(map) => {
            for (key, value) in map {
                let key = key.replace('~', "~0").replace('/', "~1");
                scalars(value, &format!("{at}/{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                scalars(value, &format!("{at}/{index}"), found);
            }
        }
        _ => found.push(at.to_owned()),
    }
}

/// Wrong documents, made from right ones: `schema` rejects each, and, if
/// `read` is given, the type that reads the document does too.
fn assert_rejects_every_corruption(
    schema: &Value,
    documents: &[(String, Value)],
    read: Option<fn(&Value) -> bool>,
) {
    let validator = validator(schema);
    let mut corruptions = 0;
    for (name, document) in documents {
        assert_valid(&validator, document, name);
        let mut paths = Vec::new();
        objects(document, "", &mut paths);
        for path in paths {
            // Every object is closed: an unknown field is rejected.
            let mut corrupt = document.clone();
            corrupt.pointer_mut(&path).unwrap()["unknownField"] = json!(true);
            assert!(
                !validator.is_valid(&corrupt),
                "{name}: an unknown field at `{path}` is accepted"
            );
            corruptions += 1;
        }
        let mut paths = Vec::new();
        scalars(document, "", &mut paths);
        for path in paths {
            // No value is an array, or an object.
            for wrong in [json!([]), json!({})] {
                let mut corrupt = document.clone();
                *corrupt.pointer_mut(&path).unwrap() = wrong;
                assert!(
                    !validator.is_valid(&corrupt),
                    "{name}: a wrong type at `{path}` is accepted"
                );
                // The reader keeps the vet document and each error unread.
                let unread = path.split('/').any(|part| part == "vet" || part == "error");
                if let (Some(read), false) = (read, unread) {
                    assert!(!read(&corrupt), "{name}: the reader accepts `{path}`");
                }
                corruptions += 1;
            }
        }
    }
    assert!(corruptions > 500, "{corruptions} corruptions");
}

#[test]
fn every_type_of_the_documents_has_a_schema() {
    use slcr::check;
    use slcr::spec::node::Modality;
    use slcr::spec::vet::{Finding, Relation, Violation, Warning};

    // Each is a root that a program can generate a type from, whatever
    // contract it generates for, and has a name of its own.
    let schemas = [
        schema_of::<check::Report>(),
        schema_of::<check::Descriptor>(),
        schema_of::<check::InstallStatus>(),
        schema_of::<check::Damage>(),
        schema_of::<check::Validator>(),
        schema_of::<check::RunOutcome>(),
        schema_of::<check::Reason>(),
        schema_of::<check::Stream>(),
        schema_of::<check::Spec>(),
        schema_of::<check::Summary>(),
        schema_of::<check::RequirementStatus>(),
        schema_of::<check::Finding>(),
        schema_of::<check::FindingCode>(),
        schema_of::<check::Status>(),
        schema_of::<check::Severity>(),
        schema_of::<Modality>(),
        schema_of::<vet::Report<'static>>(),
        schema_of::<vet::ErrorJson>(),
        schema_of::<vet::Problem>(),
        schema_of::<Violation>(),
        schema_of::<Warning>(),
        schema_of::<Relation>(),
        schema_of::<Finding<'static>>(),
    ];
    let titles: std::collections::BTreeSet<&str> = schemas
        .iter()
        .map(|schema| schema["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), schemas.len(), "{titles:?}");

    // What a finding, an invariant, and a problem write is closed on its
    // own as well, though a document writes more beside it.
    let violation = validator(&schema_of::<Violation>());
    let broken = json!({ "code": "dangling-reference", "from": "REQ-001",
                         "relation": "dependsOn", "to": "REQ-002" });
    assert!(violation.is_valid(&broken));
    for wrong in [
        json!({ "code": "dangling-reference", "from": "REQ-001", "relation": "dependsOn",
                "to": "REQ-002", "message": "m" }),
        json!({ "code": "dangling-reference", "from": "REQ-001", "relation": "other",
                "to": "REQ-002" }),
        json!({ "code": "cycle", "relation": "refines", "path": ["REQ-1"] }),
        json!({ "code": "nonsense" }),
        json!({ "from": "REQ-001", "relation": "refines", "to": "REQ-002" }),
    ] {
        assert!(!violation.is_valid(&wrong), "{wrong}");
    }
    let problem = validator(&schema_of::<vet::Problem>());
    assert!(problem.is_valid(&json!({ "code": "no-match", "pattern": "*.yaml" })));
    assert!(
        problem.is_valid(&json!({ "code": "descriptor-corrupted", "tool": "sh",
                                      "pin": pin("x") }))
    );
    assert!(!problem.is_valid(&json!({ "code": "no-match", "pattern": "*.yaml", "x": 1 })));
    assert!(!problem.is_valid(&json!({ "code": "descriptor-corrupted", "tool": "sh" })));
}

#[test]
fn the_vet_report_schema_rejects_wrong_documents() {
    let schema = schema_of::<vet::Report<'static>>();
    assert_rejects_every_corruption(&schema, &vet_documents(), None);

    let validator = validator(&schema);
    let (_, clean) = written_where(is_vet)
        .into_iter()
        .find(|(name, _)| name == "cli__vet__vet_reports_in_json")
        .unwrap();
    let (_, project) = written_where(is_vet)
        .into_iter()
        .find(|(name, _)| name == "cli__vet__vet_reports_validator_problems_in_json")
        .unwrap();
    let wrong = |document: &Value, path: &str, value: Value| {
        let mut wrong = document.clone();
        match wrong.pointer_mut(path) {
            Some(place) => *place = value,
            None => panic!("{path}"),
        }
        wrong
    };
    let removed = |document: &Value, parent: &str, key: &str| {
        let mut wrong = document.clone();
        wrong
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap();
        wrong
    };
    // What is required, is.
    for key in ["passed", "files"] {
        assert!(!validator.is_valid(&removed(&clean, "", key)), "{key}");
    }
    assert!(!validator.is_valid(&removed(&clean, "/files/0", "path")));
    assert!(!validator.is_valid(&removed(&clean, "/files/0", "passed")));
    assert!(!validator.is_valid(&removed(&clean, "/files/0", "findings")));
    for (path, value) in [
        ("/passed", json!("yes")),
        ("/files/0/path", json!(7)),
        ("/files/0/violations", json!(-1)),
        ("/files/0/passed", json!(null)),
    ] {
        assert!(
            !validator.is_valid(&wrong(&clean, path, value.clone())),
            "{path}: {value}"
        );
    }
    // A validator problem has its validator's position and a known code.
    let problems = "/validators/0/problems/0";
    assert!(project.pointer(problems).is_some());
    for (key, value) in [
        ("code", json!("not-a-code")),
        ("validator", json!(0.5)),
        ("validator", json!("1")),
        ("message", json!(null)),
    ] {
        let path = format!("{problems}/{key}");
        assert!(
            !validator.is_valid(&wrong(&project, &path, value.clone())),
            "{path}: {value}"
        );
    }
    for document in [json!([]), json!(null), json!({}), json!({ "passed": true })] {
        assert!(!validator.is_valid(&document), "{document}");
    }
}

#[test]
fn the_check_report_schema_rejects_wrong_documents() {
    let schema = schema_of::<CheckReport>();
    fn reads(document: &Value) -> bool {
        CheckReport::from_json(&document.to_string()).is_ok()
    }
    assert_rejects_every_corruption(&schema, &check_documents(), Some(reads));

    let validator = validator(&schema);
    let (_, document) = check_documents()
        .into_iter()
        .find(|(name, _)| name == "cli__check__check_reports_in_json")
        .unwrap();
    let corrupt: Vec<(&str, Value)> = vec![
        ("/specs/0/requirements/0/status", json!("green")),
        ("/specs/0/requirements/0/id", json!("REQ-2")),
        ("/specs/0/name", json!("Shop")),
        ("/specs/0/findings/0/severity", json!("fatal")),
        ("/specs/0/findings/0/modality", json!("must")),
        ("/specs/0/findings/0/code", json!("other")),
        ("/validators/0/requirement", json!("REQ-002")),
        ("/validators/0/requirement", json!("shop/REQ-2")),
        ("/validators/0/outcome", json!("blue")),
        ("/validators/0/reason/code", json!("exploded")),
        ("/validators/0/exitCode", json!("1")),
        ("/descriptors/0/status", json!("absent")),
        ("/passed", json!(null)),
    ];
    for (path, value) in corrupt {
        let mut wrong = document.clone();
        match wrong.pointer_mut(path) {
            Some(place) => *place = value.clone(),
            None => panic!("{path}"),
        }
        assert!(!validator.is_valid(&wrong), "{path}: {value} is accepted");
        assert!(!reads(&wrong), "{path}: {value} is read");
    }
    // A required field can't be left out, and the verdict is required.
    for key in ["passed", "interrupted"] {
        let mut wrong = document.clone();
        wrong.as_object_mut().unwrap().remove(key);
        assert!(!validator.is_valid(&wrong), "{key}");
        assert!(!reads(&wrong), "{key}");
    }
    for document in [json!([]), json!(null), json!({}), json!({ "passed": true })] {
        assert!(!validator.is_valid(&document), "{document}");
    }
}

/// Check documents that are valid in every way a reason or an outcome can
/// differ, which the reader reads and the schema accepts alike.
#[test]
fn the_check_report_schema_and_its_reader_agree() {
    let schema = schema_of::<CheckReport>();
    let validator = validator(&schema);
    let validator_with = |changes: Value| {
        let mut validator = json!({
            "file": ".validators.slcr.toml",
            "position": 1,
            "requirement": "shop/REQ-002",
            "tool": "sh",
            "outcome": "red",
        });
        for (key, value) in changes.as_object().unwrap() {
            validator[key] = value.clone();
        }
        json!({ "passed": false, "interrupted": false, "validators": [validator] })
    };
    let stream = json!({ "text": "", "bytes": 0, "truncated": false, "cutOff": false });
    let documents = [
        json!({ "passed": true, "interrupted": false }),
        json!({ "passed": false, "interrupted": true }),
        json!({ "passed": true, "interrupted": false, "descriptors": [] }),
        json!({ "passed": true, "interrupted": false, "descriptors": [
            { "tool": "sh", "status": "already-installed" },
            { "tool": "jev", "status": "installed", "pin": pin("x"), "replaced": "corrupted" },
            { "tool": "jev", "status": "installed", "pin": pin("x"), "replaced": "unreadable" },
            { "tool": "ast-grep", "status": "failed", "error": {
                "message": "no", "causes": ["a", "b"], "help": "yes",
                "labels": [{ "message": "here", "line": 1, "column": 2, "offset": 3, "length": 4 }]
            } },
        ] }),
        validator_with(
            json!({ "reason": { "code": "exit-code" }, "exitCode": 1, "durationMs": 5,
            "stdout": stream, "stderr": stream }),
        ),
        validator_with(json!({ "reason": { "code": "terminated", "signal": 9 } })),
        validator_with(json!({ "reason": { "code": "terminated" } })),
        validator_with(json!({ "reason": { "code": "timed-out" } })),
        validator_with(json!({ "reason": { "code": "lost", "detail": "gone" } })),
        validator_with(json!({ "outcome": "green", "criterion": "AC-009" })),
        validator_with(json!({ "outcome": "could-not-run", "error": { "message": "x" } })),
        validator_with(json!({ "outcome": "interrupted" })),
        validator_with(json!({ "requirement": "a-1/REQ-4294967295" })),
        // What neither reads.
        validator_with(json!({ "reason": { "code": "lost" } })),
        validator_with(json!({ "reason": { "code": "terminated", "signal": "9" } })),
        validator_with(json!({ "exitCode": 1.5 })),
        validator_with(json!({ "durationMs": -1 })),
        validator_with(json!({ "position": "1" })),
        validator_with(json!({ "criterion": "AC-1" })),
        validator_with(json!({ "requirement": "a--b/REQ-001" })),
        validator_with(json!({ "requirement": "shop/REQ-4294967296" })),
        validator_with(json!({ "tool": "Sh" })),
        validator_with(json!({ "stdout": { "text": "", "bytes": 0, "truncated": false } })),
        json!({ "passed": true, "interrupted": false, "descriptors": [
            { "tool": "sh", "status": "installed", "pin": "blake3:ABC" } ] }),
        json!({ "passed": true, "interrupted": false, "descriptors": [
            { "tool": "sh", "status": "installed", "replaced": "ruined" } ] }),
        json!({ "passed": true, "interrupted": false, "specs": [ {
            "name": "shop", "file": "SPEC.slcr.toml", "passed": true,
            "summary": { "satisfied": 1, "unsatisfied": 0, "unknown": 0, "violations": 0,
                         "warnings": 0, "informational": 0, "coverageGaps": 0 },
            "requirements": [{ "id": "REQ-001", "status": "satisfied" }],
            "findings": [{ "code": "absorbed", "requirement": "REQ-002", "modality": "SHOULD NOT",
                           "absorbedBy": ["REQ-001"], "severity": "warning", "message": "m",
                           "help": "h" }],
            "coverageGaps": ["REQ-003"] } ] }),
        json!({ "passed": true, "interrupted": false, "specs": [ {
            "name": "shop", "file": "SPEC.slcr.toml", "passed": true,
            "summary": { "satisfied": 1, "unsatisfied": 0, "unknown": 0, "violations": 0,
                         "warnings": 0, "informational": 0 },
            "requirements": [], "findings": [], "coverageGaps": [] } ] }),
    ];
    let mut read = 0;
    for document in &documents {
        let reads = CheckReport::from_json(&document.to_string()).is_ok();
        let accepts = validator.is_valid(document);
        assert_eq!(accepts, reads, "{}", pretty(document));
        read += usize::from(reads);
    }
    // Both ways, a good many.
    assert!(read >= 12 && documents.len() - read >= 10, "{read}");

    // The reader keeps the embedded vet document and each error as JSON
    // objects, unread, so it reads some the schema, which says what they
    // are, rejects; and some it reads more loosely than the writer writes.
    for wrong in [
        // Serde's `deny_unknown_fields` doesn't reach a unit variant of an
        // internally tagged enum, which the writer never gives a field.
        validator_with(json!({ "reason": { "code": "exit-code", "signal": 9 } })),
        json!({ "passed": true, "interrupted": false, "vet": { "passed": 1 } }),
        json!({ "passed": true, "interrupted": false, "vet": {} }),
        validator_with(json!({ "error": { "message": "x", "other": 1 } })),
        validator_with(json!({ "error": { "causes": [] } })),
        validator_with(json!({ "error": { "message": "x", "labels": [{ "line": 1 }] } })),
    ] {
        assert!(
            CheckReport::from_json(&wrong.to_string()).is_ok(),
            "{wrong}"
        );
        assert!(!validator.is_valid(&wrong), "{wrong}");
    }
}

// Strings

/// Whether the schema of `T` accepts each of `samples`, as `T` reads it.
#[track_caller]
fn assert_pattern_agrees<T: JsonSchema + DeserializeOwned>(samples: &[&str], accepted: &[&str]) {
    let schema = schema_of::<T>();
    let validator = validator(&schema);
    for sample in samples {
        let document = json!(sample);
        let reads = serde_json::from_value::<T>(document.clone()).is_ok();
        let matches = validator.is_valid(&document);
        assert_eq!(
            matches,
            reads,
            "{}: {sample:?}: the pattern {} it, the parser {} it",
            T::schema_name(),
            if matches { "accepts" } else { "rejects" },
            if reads { "accepts" } else { "rejects" },
        );
        assert_eq!(reads, accepted.contains(sample), "{sample:?}");
    }
    // Only strings.
    for other in [json!(1), json!(null), json!(true), json!([]), json!({})] {
        assert!(!validator.is_valid(&other), "{other}");
    }
}

const SLUGS: [&str; 18] = [
    "todo-api",
    "a",
    "0",
    "a-1-b",
    "ast-grep2",
    "",
    "-",
    "-a",
    "a-",
    "a--b",
    "A",
    "a_b",
    "a b",
    "a\n",
    "\na",
    "é",
    "a.b",
    "ａ",
];
const VALID_SLUGS: [&str; 5] = ["todo-api", "a", "0", "a-1-b", "ast-grep2"];

#[test]
fn spec_names_and_tool_keys_have_the_pattern_their_parsers_enforce() {
    assert_pattern_agrees::<SpecName>(&SLUGS, &VALID_SLUGS);
    assert_pattern_agrees::<ToolKey>(&SLUGS, &VALID_SLUGS);
}

#[test]
fn titles_have_the_pattern_their_parser_enforces() {
    let samples = [
        "X",
        "TodoList API",
        "Table todo_list_item exists",
        "a\tb",
        "a  b",
        "é",
        "a\u{2028}b",
        "\u{2028}",
        "a\u{85}",
        "\u{85}a",
        "",
        " ",
        " leading",
        "trailing ",
        "\ttab",
        "nbsp\u{a0}",
        "\u{a0}nbsp",
        "\u{feff}bom",
        "bom\u{feff}",
        "a\u{3000}",
        "a\u{200b}",
        "two\nlines",
        "two\r\nlines",
        "cr\r",
        "\nlf",
        "a\u{b}",
        "a\u{c}",
    ];
    let accepted = [
        "X",
        "TodoList API",
        "Table todo_list_item exists",
        "a\tb",
        "a  b",
        "é",
        "a\u{2028}b",
        "a\u{85}",
        "\u{85}a",
        "a\u{200b}",
    ];
    assert_pattern_agrees::<Title>(&samples, &accepted);
}

#[test]
fn pins_have_the_pattern_their_parser_enforces() {
    let hex = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let valid = format!("blake3:{hex}");
    let zeros = format!("blake3:{}", "0".repeat(64));
    let samples = [
        valid.clone(),
        zeros.clone(),
        format!("blake3:{}", hex.to_uppercase()),
        format!("blake3:{}", &hex[1..]),
        format!("blake3:{hex}0"),
        format!("sha256:{hex}"),
        format!("BLAKE3:{hex}"),
        format!("blake3: {hex}"),
        format!("blake3:{hex}\n"),
        format!("blake3:{}g", &hex[1..]),
        format!("blake3:{}\u{661}", &hex[2..]),
        hex.to_owned(),
        "blake3:".to_owned(),
        String::new(),
    ];
    let samples: Vec<&str> = samples.iter().map(String::as_str).collect();
    assert_pattern_agrees::<Pin>(&samples, &[valid.as_str(), zeros.as_str()]);
}

#[test]
fn versions_have_the_pattern_their_parser_enforces() {
    let valid = [
        "0.0.0",
        "1.0.0",
        "0.3.1",
        "10.20.30",
        "1.2.3-rc.1+build.5",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-0.3.7",
        "1.0.0-x.7.z.92",
        "1.0.0-x-y-z.-",
        "1.0.0+20130313144700",
        "1.0.0-beta+exp.sha.5114f85",
        "1.0.0+21AF26D3----117B344092BD",
        "1.0.0-0A",
        "1.0.0-a0",
        "1.0.0+001",
        "1.0.0+0",
        "1.0.0-0",
        "18446744073709551615.18446744073709551615.18446744073709551615",
        "1.0.0-99999999999999999999999999",
        "1.0.0-a.b.c.d.e.f.g.h.i.j.k.l.m.n",
    ];
    let invalid = [
        "",
        "1",
        "1.0",
        "01.0.0",
        "1.00.0",
        "1.0.00",
        "1.0.0-",
        "v1.0.0",
        "1.0.0-01",
        "1.0.0-alpha.01",
        "1.0.0-alpha..1",
        "1.0.0-alpha.",
        "1.0.0+",
        "1.0.0+a..b",
        "1.0.0+a.",
        "1.0.0++a",
        "1.0.0-a+",
        "1.0.0.0",
        "1.0.0 ",
        " 1.0.0",
        "1.0.0\n",
        "1.0.0-é",
        "1.0.0+é",
        "1.0.0-_",
        "-1.0.0",
        "+1.0.0",
        "1.+0.0",
        "1.0.0-\u{661}",
        "\u{661}.0.0",
        "1,0,0",
        "a.b.c",
        "1.0.0-a b",
        "1.0.0+a_b",
        "18446744073709551616.0.0",
    ];
    // Only the pattern is looser than the parser, and says so: it limits
    // each of the three numbers to 20 digits, not to what a `u64` holds.
    let documented_gap = ["18446744073709551616.0.0", "0.99999999999999999999.0"];
    let samples: Vec<&str> = valid.iter().chain(&invalid).copied().collect();
    let schema = schema_of::<DescriptorVersion>();
    let validator = validator(&schema);
    for sample in samples {
        let reads = serde_json::from_value::<DescriptorVersion>(json!(sample)).is_ok();
        let matches = validator.is_valid(&json!(sample));
        assert_eq!(reads, valid.contains(&sample), "{sample:?}");
        if documented_gap.contains(&sample) {
            assert!(matches && !reads, "{sample:?}");
        } else {
            assert_eq!(matches, reads, "{sample:?}");
        }
    }
    let too_long = "123456789012345678901.0.0";
    assert!(!validator.is_valid(&json!(too_long)));
    assert!(serde_json::from_value::<DescriptorVersion>(json!(too_long)).is_err());
    assert!(validator.is_valid(&json!("0.99999999999999999999.0")));
    assert!(
        serde_json::from_value::<DescriptorVersion>(json!("0.99999999999999999999.0")).is_err()
    );
}

/// Samples for the ID of `prefix`.
fn id_samples(prefix: &str) -> Vec<String> {
    let numbers = [
        "000",
        "001",
        "042",
        "999",
        "1000",
        "1001",
        "12345",
        "999999999",
        "1000000000",
        "4294967295",
        "4294967296",
        "4294967299",
        "4999999999",
        "5000000000",
        "9999999999",
        "10000000000",
        "99999999999",
        "0001",
        "0999",
        "01",
        "1",
        "12",
        "",
        "-1",
        "+01",
        "-01",
        "00a",
        "0 1",
        " 001",
        "001 ",
        "001\n",
        "\u{661}\u{662}\u{663}",
        "\u{ff11}\u{ff10}\u{ff10}\u{ff10}",
    ];
    let mut samples: Vec<String> = numbers.iter().map(|n| format!("{prefix}-{n}")).collect();
    samples.extend([
        prefix.to_owned(),
        format!("{prefix}-"),
        format!("{prefix}001"),
        format!("{prefix}_001"),
        format!("{prefix}--001"),
        format!("{}-001", prefix.to_lowercase()),
        format!(" {prefix}-001"),
        format!("{prefix}-001 "),
        format!("X{prefix}-001"),
        format!("{prefix}-001X"),
        "REQ-001".to_owned(),
        "SEC-001".to_owned(),
        "TERM-001".to_owned(),
        "AC-001".to_owned(),
        "TERM-001AC-001".to_owned(),
    ]);
    samples
}

#[track_caller]
fn assert_ids_agree<T: JsonSchema + DeserializeOwned>(prefix: &str) {
    let samples = id_samples(prefix);
    let borrowed: Vec<&str> = samples.iter().map(String::as_str).collect();
    let accepted: Vec<String> = [
        "000",
        "001",
        "042",
        "999",
        "1000",
        "1001",
        "12345",
        "999999999",
        "1000000000",
        "4294967295",
    ]
    .iter()
    .map(|n| format!("{prefix}-{n}"))
    .collect();
    let accepted: Vec<&str> = accepted.iter().map(String::as_str).collect();
    assert_pattern_agrees::<T>(&borrowed, &accepted);
}

#[test]
fn ids_have_the_pattern_their_parser_enforces() {
    assert_ids_agree::<RequirementId>("REQ");
    assert_ids_agree::<CriterionId>("AC");
    assert_ids_agree::<SectionId>("SEC");
    assert_ids_agree::<TermId>("TERM");
}

#[test]
fn qualified_requirement_ids_have_the_pattern_their_parser_enforces() {
    // The type isn't named by the library's API, but the check report holds
    // it, as every validator's `requirement`.
    let schema = schema_of::<CheckReport>();
    let validator = validator(&schema);
    let document = |requirement: &str| {
        json!({
            "passed": true,
            "interrupted": false,
            "validators": [{
                "file": "f",
                "position": 1,
                "requirement": requirement,
                "tool": "sh",
                "outcome": "green",
            }],
        })
    };
    let samples = [
        "todo-api/REQ-002",
        "a/REQ-000",
        "a-1/REQ-1000",
        "shop/REQ-4294967295",
        "shop/REQ-4294967296",
        "REQ-002",
        "/REQ-002",
        "shop/",
        "shop",
        "shop/REQ-2",
        "shop/REQ-0002",
        "shop/SEC-001",
        "shop/req-001",
        "Shop/REQ-001",
        "sh op/REQ-001",
        "shop//REQ-001",
        "shop/a/REQ-001",
        "shop/REQ-001/",
        "shop/REQ-001\n",
        "shop\n/REQ-001",
        " shop/REQ-001",
        "-shop/REQ-001",
        "shop-/REQ-001",
        "sh--op/REQ-001",
        "shop\\REQ-001",
        "",
    ];
    let mut accepted = 0;
    for sample in samples {
        let document = document(sample);
        let reads = CheckReport::from_json(&document.to_string()).is_ok();
        assert_eq!(validator.is_valid(&document), reads, "{sample:?}");
        accepted += usize::from(reads);
    }
    assert_eq!(accepted, 4);
}

// Results

/// A project directory, an application's: `app` holds the SLCR files.
fn application() -> (TempDir, PathBuf) {
    let dir = super::directory(&[]);
    let app = dir.path().canonicalize().unwrap().join("app");
    std::fs::create_dir(&app).unwrap();
    (dir, app)
}

fn spec_name(name: &str) -> SpecName {
    SpecName::try_from(name.to_owned()).unwrap()
}

fn key(key: &str) -> ToolKey {
    ToolKey::try_from(key.to_owned()).unwrap()
}

#[test]
fn initializing_serializes_as_camel_case_and_matches_its_schema() {
    let schema = schema_of::<Initialized>();
    let validator = validator(&schema);
    let (_dir, app) = application();
    let (project, created) =
        Project::create(&app, &app, "SPEC.slcr.toml", spec_name("shop"), None).unwrap();
    let created = serde_json::to_value(&created).unwrap();
    assert_eq!(
        created,
        json!({ "spec": "SPEC.slcr.toml", "name": "shop", "createdManifest": true })
    );
    assert_valid(&validator, &created, "a created project");
    drop(project);

    // Joining a project whose manifest doesn't list the file warns, with
    // the JSON of an error.
    let (_, joined) =
        Project::create(&app, &app, "OTHER.slcr.toml", spec_name("other"), None).unwrap();
    let warning = joined
        .warning()
        .map(|warning| ErrorJson::of(warning))
        .unwrap();
    let joined = serde_json::to_value(&joined).unwrap();
    assert_eq!(
        joined,
        json!({
            "spec": "OTHER.slcr.toml",
            "name": "other",
            "createdManifest": false,
            "warning": {
                "message": "the `specs` of SLCR.toml don't match OTHER.slcr.toml",
                "help": "add \"OTHER.slcr.toml\" to `specs` so that `slcr vet` and `slcr check` \
                         read it",
            },
        })
    );
    assert_eq!(joined["warning"], serde_json::to_value(&warning).unwrap());
    assert_valid(&validator, &joined, "a warned join");

    // Joining one that lists it doesn't.
    let (_, listed) =
        Project::create(&app, &app, "SPEC.slcr.json", spec_name("listed"), None).unwrap();
    assert!(listed.warning().is_some());
    std::fs::write(
        app.join("SLCR.toml"),
        "formatVersion = \"1\"\nspecs = [\"*.slcr.*\"]\n",
    )
    .unwrap();
    let (_, listed) =
        Project::create(&app, &app, "THIRD.slcr.yaml", spec_name("third"), None).unwrap();
    let listed = serde_json::to_value(&listed).unwrap();
    assert_eq!(
        listed,
        json!({ "spec": "THIRD.slcr.yaml", "name": "third", "createdManifest": false })
    );
    assert_valid(&validator, &listed, "a listed join");

    // Anything else is wrong.
    for wrong in [
        json!({ "spec": "a", "name": "shop" }),
        json!({ "spec": "a", "name": "Shop", "createdManifest": true }),
        json!({ "spec": "a", "name": "shop", "createdManifest": 1 }),
        json!({ "spec": "a", "name": "shop", "createdManifest": true, "warning": null }),
        json!({ "spec": "a", "name": "shop", "createdManifest": true, "warning": {} }),
        json!({ "spec": "a", "name": "shop", "createdManifest": true, "created_manifest": true }),
        json!({ "name": "shop", "createdManifest": true }),
    ] {
        assert!(!validator.is_valid(&wrong), "{wrong}");
    }
}

/// `SH` at `version`, with `extra` appended.
fn descriptor(version: &str, extra: &str) -> String {
    SH.replace("version = \"1.0.0\"", &format!("version = \"{version}\"")) + extra
}

#[test]
fn managing_tools_serializes_each_result_and_matches_its_schema() {
    let added_schema = validator(&schema_of::<ToolAdded>());
    let updated_schema = validator(&schema_of::<ToolUpdated>());
    let removed_schema = validator(&schema_of::<ToolRemoved>());
    let installed_schema = validator(&schema_of::<Installed>());
    let (_dir, app) = application();
    Project::create(&app, &app, "SPEC.slcr.toml", spec_name("shop"), None).unwrap();
    let project = Project::open(&app, &app).unwrap();
    std::fs::create_dir(app.join("tools")).unwrap();
    std::fs::write(app.join("tools/sh.toml"), SH).unwrap();
    let store = DescriptorStore::at(app.join("store"));
    let fetcher = Fetcher::new();

    block_on(async {
        let added = project
            .add_tool(&store, &fetcher, "tools/sh.toml", None)
            .await
            .unwrap();
        let again = project
            .add_tool(&store, &fetcher, "tools/sh.toml", None)
            .await
            .unwrap();
        for (what, result, change) in [
            ("added", &added, "declared"),
            ("again", &again, "already-declared"),
        ] {
            let json = serde_json::to_value(result).unwrap();
            assert_eq!(
                json,
                json!({ "manifest": "SLCR.toml", "tool": "sh", "version": "1.0.0", "change": change }),
                "{what}"
            );
            assert_valid(&added_schema, &json, what);
        }
        assert_eq!(added.to_string(), "SLCR.toml: added tool sh 1.0.0");

        // The same version, other bytes; a newer version; and nothing.
        let mut updates = Vec::new();
        for (text, change, from, to) in [
            (
                descriptor("1.0.0", "\n# edited\n"),
                "repinned",
                "1.0.0",
                "1.0.0",
            ),
            (descriptor("1.1.0", ""), "updated", "1.0.0", "1.1.0"),
            (descriptor("1.1.0", ""), "up-to-date", "1.1.0", "1.1.0"),
        ] {
            std::fs::write(app.join("tools/sh.toml"), text).unwrap();
            let updated = project
                .update_tool(&store, &fetcher, key("sh"))
                .await
                .unwrap();
            let json = serde_json::to_value(&updated).unwrap();
            assert_eq!(
                json,
                json!({ "manifest": "SLCR.toml", "tool": "sh", "from": from, "to": to,
                        "change": change })
            );
            assert_valid(&updated_schema, &json, change);
            updates.push(updated.to_string());
        }
        assert_eq!(
            updates,
            [
                "SLCR.toml: re-pinned tool sh 1.0.0",
                "SLCR.toml: updated tool sh 1.0.0 -> 1.1.0",
                "SLCR.toml: tool sh 1.1.0 is up to date",
            ]
        );

        // Everything pinned is installed in this store, and not in a new one.
        let installed = project.install(&store, &fetcher).await.unwrap();
        let json = serde_json::to_value(&installed).unwrap();
        assert_eq!(
            json,
            json!({ "passed": true,
                    "descriptors": [{ "tool": "sh", "status": "already-installed" }] })
        );
        assert_valid(&installed_schema, &json, "already installed");
        assert_eq!(
            json["descriptors"],
            serde_json::to_value(installed.descriptors()).unwrap()
        );
        let empty = DescriptorStore::at(app.join("other-store"));
        let installed = project.install(&empty, &fetcher).await.unwrap();
        let json = serde_json::to_value(&installed).unwrap();
        let pinned = pin(&descriptor("1.1.0", ""));
        assert_eq!(
            json,
            json!({ "passed": true,
                    "descriptors": [{ "tool": "sh", "status": "installed", "pin": pinned }] })
        );
        assert_valid(&installed_schema, &json, "installed");

        // A source that serves other bytes than the pin fails, with an error.
        std::fs::write(app.join("tools/sh.toml"), descriptor("1.1.0", "# other\n")).unwrap();
        let failing = DescriptorStore::at(app.join("third-store"));
        let installed = project.install(&failing, &fetcher).await.unwrap();
        assert!(!installed.passed());
        let json = serde_json::to_value(&installed).unwrap();
        assert_eq!(json["passed"], false);
        assert_eq!(json["descriptors"][0]["status"], "failed");
        assert!(json["descriptors"][0]["error"]["message"].is_string());
        assert_valid(&installed_schema, &json, "failed");
        // Nothing to install is a pass with nothing in it.
        let removed = project.remove_tool(key("sh")).unwrap();
        let json = serde_json::to_value(&removed).unwrap();
        assert_eq!(
            json,
            json!({ "manifest": "SLCR.toml", "tool": "sh", "version": "1.1.0" })
        );
        assert_valid(&removed_schema, &json, "removed");
        assert_eq!(removed.to_string(), "SLCR.toml: removed tool sh 1.1.0");
        let nothing = project.install(&store, &fetcher).await.unwrap();
        let json = serde_json::to_value(&nothing).unwrap();
        assert_eq!(json, json!({ "passed": true, "descriptors": [] }));
        assert_valid(&installed_schema, &json, "nothing to install");
    });

    // Anything else is wrong.
    let removed = json!({ "manifest": "SLCR.toml", "tool": "sh", "version": "1.0.0" });
    let with = |value: &Value, key: &str, set: Value| {
        let mut value = value.clone();
        value[key] = set;
        value
    };
    assert!(removed_schema.is_valid(&removed));
    let added = with(&removed, "change", json!("declared"));
    assert!(added_schema.is_valid(&added));
    for wrong in [
        with(&added, "change", json!("changed")),
        with(&added, "change", json!(null)),
        with(&added, "change", json!("repinned")),
        with(&added, "version", json!("1.0")),
        with(&added, "tool", json!("Sh")),
        with(&added, "extra", json!(1)),
        removed.clone(),
        json!({ "manifest": "SLCR.toml", "key": "sh", "version": "1.0.0", "change": "declared" }),
    ] {
        assert!(!added_schema.is_valid(&wrong), "{wrong}");
    }
    let updated = json!({ "manifest": "SLCR.toml", "tool": "sh", "from": "1.0.0", "to": "1.1.0",
                          "change": "up-to-date" });
    assert!(updated_schema.is_valid(&updated));
    for wrong in [
        with(&updated, "change", json!("declared")),
        with(&updated, "to", json!("1.0.0.0")),
        with(&updated, "from", json!(null)),
        with(&updated, "extra", json!(1)),
        with(&removed, "change", json!("updated")),
    ] {
        assert!(!updated_schema.is_valid(&wrong), "{wrong}");
    }
    for wrong in [
        with(&removed, "version", json!("x")),
        with(&removed, "change", json!("removed")),
        json!({ "manifest": "SLCR.toml", "tool": "sh" }),
    ] {
        assert!(!removed_schema.is_valid(&wrong), "{wrong}");
    }
    for wrong in [
        json!({ "passed": true }),
        json!({ "descriptors": [] }),
        json!({ "passed": 1, "descriptors": [] }),
        json!({ "passed": true, "descriptors": [], "manifest": "SLCR.toml" }),
        json!({ "passed": true, "descriptors": [{ "tool": "sh", "status": "ok" }] }),
        json!({ "passed": true, "descriptors": [{ "tool": "sh", "status": "installed", "pin": "x" }] }),
    ] {
        assert!(!installed_schema.is_valid(&wrong), "{wrong}");
    }
}

// Errors

#[test]
fn any_error_the_library_returns_is_written_as_slcr_writes_an_error() {
    let schema = schema_of::<ErrorJson>();
    let validator = validator(&schema);

    // Read as the CLI reads a file, and written as it writes the report.
    let source = "root: [\n".to_owned();
    let parse = Format::Yaml
        .parse::<UnvettedDocument>(Path::new("bad.yaml"), source.clone())
        .unwrap_err();
    let from_parse = serde_json::to_value(ErrorJson::of(&parse)).unwrap();
    let (outcome, document) = vet::source("bad.yaml", Format::Yaml, source);
    assert!(document.is_none());
    let written: Value = serde_json::from_str(&vet::json(None, &[outcome], false)).unwrap();
    assert_eq!(from_parse, written["files"][0]["error"]);
    assert!(from_parse["labels"][0]["line"].is_u64());

    let (_dir, app) = application();
    // A typed error, and a report, and the errors of other calls.
    let no_manifest = Project::open(&app, &app).unwrap_err();
    let report = block_on(async {
        let project = {
            Project::create(&app, &app, "SPEC.slcr.toml", spec_name("shop"), None).unwrap();
            Project::open(&app, &app).unwrap()
        };
        let store = DescriptorStore::at(app.join("store"));
        project
            .add_tool(&store, &Fetcher::new(), "tools/missing.toml", None)
            .await
            .unwrap_err()
    });
    let removed = Project::open(&app, &app)
        .unwrap()
        .remove_tool(key("sh"))
        .unwrap_err();
    let slice = block_on(slcr::slice(&super::canon(), RequirementId::new(999))).unwrap_err();
    let query = block_on(slcr::query(
        &super::canon(),
        "MATCH (n RETURN n",
        Map::new(),
        None,
    ))
    .unwrap_err();
    let errors: Vec<(&str, ErrorJson, String, Option<String>)> = vec![
        (
            "a project error",
            ErrorJson::of(&no_manifest),
            no_manifest.to_string(),
            slcr_help(&no_manifest),
        ),
        (
            "a report",
            ErrorJson::new(&report),
            report.to_string(),
            report.help().map(|help| help.to_string()),
        ),
        (
            "a report from a typed error",
            ErrorJson::new(&removed),
            removed.to_string(),
            removed.help().map(|help| help.to_string()),
        ),
        (
            "a slice error",
            ErrorJson::of(&slice),
            slice.to_string(),
            slcr_help(&slice),
        ),
        (
            "a query error",
            ErrorJson::of(&query),
            query.to_string(),
            slcr_help(&query),
        ),
    ];
    for (what, json, message, help) in errors {
        let json = serde_json::to_value(&json).unwrap();
        assert_eq!(json["message"], message.as_str(), "{what}");
        assert_eq!(
            json.get("help").and_then(Value::as_str),
            help.as_deref(),
            "{what}"
        );
        assert_valid(&validator, &json, what);
    }
    // The query's syntax error is located in the query.
    let located = serde_json::to_value(ErrorJson::of(&query)).unwrap();
    assert!(located["labels"][0]["offset"].is_u64(), "{located}");

    // Anything else is wrong.
    for wrong in [
        json!({}),
        json!({ "message": null }),
        json!({ "message": "m", "causes": "c" }),
        json!({ "message": "m", "help": null }),
        json!({ "message": "m", "labels": [{ "line": 1, "column": 1, "offset": 0 }] }),
        json!({ "message": "m", "labels": [{ "line": 0.5, "column": 1, "offset": 0, "length": 1 }] }),
        json!({ "message": "m", "code": "x" }),
    ] {
        assert!(!validator.is_valid(&wrong), "{wrong}");
    }
}

/// The help of a typed error, as a diagnostic gives it.
fn slcr_help(error: &dyn miette::Diagnostic) -> Option<String> {
    error.help().map(|help| help.to_string())
}

// The schemas themselves, for review when one changes.

#[test]
fn the_vet_report_schema_is_snapshotted() {
    insta::assert_snapshot!(pretty(&schema_of::<vet::Report<'static>>()));
}

#[test]
fn the_check_report_schema_is_snapshotted() {
    insta::assert_snapshot!(pretty(&schema_of::<CheckReport>()));
}

#[test]
fn the_error_schema_is_snapshotted() {
    insta::assert_snapshot!(pretty(&schema_of::<ErrorJson>()));
}

#[test]
fn the_tool_result_schemas_are_snapshotted() {
    insta::assert_snapshot!("added", pretty(&schema_of::<ToolAdded>()));
    insta::assert_snapshot!("updated", pretty(&schema_of::<ToolUpdated>()));
    insta::assert_snapshot!("removed", pretty(&schema_of::<ToolRemoved>()));
    insta::assert_snapshot!("installed", pretty(&schema_of::<Installed>()));
}

#[test]
fn the_init_result_schema_is_snapshotted() {
    insta::assert_snapshot!(pretty(&schema_of::<Initialized>()));
}

#[test]
fn the_string_schemas_are_snapshotted() {
    insta::assert_snapshot!("spec_name", pretty(&schema_of::<SpecName>()));
    insta::assert_snapshot!("tool_key", pretty(&schema_of::<ToolKey>()));
    insta::assert_snapshot!("title", pretty(&schema_of::<Title>()));
    insta::assert_snapshot!("pin", pretty(&schema_of::<Pin>()));
    insta::assert_snapshot!("version", pretty(&schema_of::<DescriptorVersion>()));
    insta::assert_snapshot!("requirement_id", pretty(&schema_of::<RequirementId>()));
    insta::assert_snapshot!("criterion_id", pretty(&schema_of::<CriterionId>()));
    insta::assert_snapshot!("section_id", pretty(&schema_of::<SectionId>()));
    insta::assert_snapshot!("term_id", pretty(&schema_of::<TermId>()));
}
