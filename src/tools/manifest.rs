use std::{
    collections::{BTreeMap, HashSet},
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use derive_getters::Getters;
use miette::{Diagnostic, Result};
use serde::{Deserialize, Deserializer, de::Error as _};
use thiserror::Error;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::source::{Source, is_project_relative};
use super::{DescriptorVersion, ToolKey, pin::Pin};
use crate::fs::{FileSystem, ReadError, format::Format};
use crate::spec::graph::FormatVersion;

/// The project manifest's name. Its directory is the project root.
pub(crate) const MANIFEST: &str = "SLCR.toml";

/// An SLCR project: the directory that holds `SLCR.toml`, found by walking
/// up from the working directory, the way Cargo finds `Cargo.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Project {
    /// The project root, as an absolute path.
    root: PathBuf,
    /// The manifest's path relative to the working directory, such as
    /// `SLCR.toml` or `../../SLCR.toml`: how messages name it.
    shown: PathBuf,
}

/// The ways finding the project can fail.
#[derive(Debug, Error, Diagnostic)]
pub enum ProjectError {
    #[error("could not find {MANIFEST} in the working directory or any parent directory")]
    #[diagnostic(help(
        "create {MANIFEST} at the project root, with `formatVersion = \"1\"` and the \
         specification files the project consumes, e.g. `specs = [\"SPEC.slcr.yml\"]`"
    ))]
    NotFound,

    /// A near miss, such as `slcr.toml`, which a case-insensitive
    /// filesystem would find and Linux CI wouldn't.
    #[error("{} is not named {MANIFEST}", .path.display())]
    #[diagnostic(help("the manifest's name is case-sensitive: rename it to {MANIFEST}"))]
    Misnamed { path: PathBuf },
}

impl Project {
    /// The project that holds `dir`, an absolute path.
    pub(crate) fn find_from(dir: &Path) -> Result<Self, ProjectError> {
        let mut up = PathBuf::new();
        for ancestor in dir.ancestors() {
            match manifest_in(ancestor) {
                Lookup::Found => {
                    return Ok(Self {
                        root: ancestor.to_path_buf(),
                        shown: up.join(MANIFEST),
                    });
                }
                Lookup::Misnamed(name) => {
                    return Err(ProjectError::Misnamed {
                        path: up.join(name),
                    });
                }
                Lookup::Absent => up.push(".."),
            }
        }
        Err(ProjectError::NotFound)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The manifest's absolute path.
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST)
    }

    /// The manifest's path relative to the working directory.
    pub(crate) fn shown_manifest(&self) -> &Path {
        &self.shown
    }
}

/// Whether a directory holds the manifest.
enum Lookup {
    Found,
    /// It holds a file whose name differs from the manifest's only in case.
    Misnamed(String),
    Absent,
}

/// Look for the manifest in `dir`. Its entries are listed, rather than the
/// manifest's path tested, because on a case-insensitive filesystem the
/// test would succeed for `slcr.toml` too.
fn manifest_in(dir: &Path) -> Lookup {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // A directory that can't be listed, such as an unreadable parent,
        // may still hold the manifest.
        return if dir.join(MANIFEST).is_file() {
            Lookup::Found
        } else {
            Lookup::Absent
        };
    };
    let mut near_miss = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == MANIFEST {
            return Lookup::Found;
        }
        if name.eq_ignore_ascii_case(MANIFEST) {
            near_miss = Some(name.to_owned());
        }
    }
    near_miss.map_or(Lookup::Absent, Lookup::Misnamed)
}

/// The contents of `SLCR.toml`, validated against the manifest schema.
#[derive(Clone, Debug, PartialEq, Getters, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Manifest {
    #[getter(skip)]
    #[allow(dead_code)]
    format_version: FormatVersion,
    /// The specification files the project consumes, as globs.
    #[serde(deserialize_with = "non_empty_set")]
    specs: Vec<ProjectGlob>,
    /// The declared descriptors, by manifest key.
    #[serde(default, deserialize_with = "non_empty_map")]
    tools: BTreeMap<ToolKey, ToolEntry>,
}

/// One `[tools]` entry: a descriptor, where to fetch it, and its pin.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolEntry {
    /// The descriptor's version, copied from it.
    version: DescriptorVersion,
    source: Source,
    pin: Pin,
}

impl ToolEntry {
    pub(crate) fn new(version: DescriptorVersion, source: Source, pin: Pin) -> Self {
        Self {
            version,
            source,
            pin,
        }
    }
}

/// Replace the value of `field`, a field of `table` that the manifest
/// schema requires, with `value`, keeping the key and the whitespace and
/// comments around the value.
fn replace_value(table: &mut dyn TableLike, field: &str, value: String) {
    let old = table
        .get_mut(field)
        .and_then(Item::as_value_mut)
        .expect("a required field of a manifest that deserialized has a value");
    let decor = old.decor().clone();
    *old = Value::from(value);
    *old.decor_mut() = decor;
}

/// An array with at least one element, and no element repeated.
fn non_empty_set<'de, D>(deserializer: D) -> Result<Vec<ProjectGlob>, D::Error>
where
    D: Deserializer<'de>,
{
    let items = Vec::<ProjectGlob>::deserialize(deserializer)?;
    if items.is_empty() {
        return Err(D::Error::custom(
            "`specs` must name at least one specification file",
        ));
    }
    let mut seen = HashSet::with_capacity(items.len());
    if let Some(repeated) = items.iter().find(|item| !seen.insert(*item)) {
        return Err(D::Error::custom(format!(
            "`{repeated}` appears more than once"
        )));
    }
    Ok(items)
}

/// A table with at least one entry.
fn non_empty_map<'de, D>(deserializer: D) -> Result<BTreeMap<ToolKey, ToolEntry>, D::Error>
where
    D: Deserializer<'de>,
{
    let map = BTreeMap::deserialize(deserializer)?;
    if map.is_empty() {
        return Err(D::Error::custom("an empty `tools` table must be omitted"));
    }
    Ok(map)
}

/// A glob relative to the project root, such as `docs/**/*.slcr.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct ProjectGlob(String);

impl TryFrom<String> for ProjectGlob {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_project_relative(&value) {
            Ok(Self(value))
        } else {
            Err(format!(
                "{value:?} is not a glob relative to the project root, which uses forward \
                 slashes, with no leading `/` and no `..` segment"
            ))
        }
    }
}

impl ProjectGlob {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ProjectGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A project's `SLCR.toml`, read both as a [Manifest] and as a document
/// that can be edited without disturbing the user's comments or
/// formatting, the way `cargo add` edits `Cargo.toml`.
pub(crate) struct ManifestFile {
    path: PathBuf,
    /// The manifest's path relative to the working directory.
    shown: PathBuf,
    manifest: Manifest,
    document: DocumentMut,
}

/// The error returned when the manifest declares no tool under a name.
#[derive(Debug, Error, Diagnostic)]
#[error("{} declares no tool named {key}", .manifest.display())]
#[diagnostic(help("{help}"))]
pub struct NotDeclared {
    manifest: PathBuf,
    key: ToolKey,
    help: String,
}

impl ManifestFile {
    /// Read the manifest of `project`.
    pub(crate) fn load(project: &Project) -> Result<Self> {
        let path = project.manifest_path();
        let shown = project.shown_manifest();
        let source = std::fs::read_to_string(&path)
            .map_err(|source| ReadError::new(shown.to_path_buf(), source))?;
        let manifest: Manifest = Format::Toml.parse(shown, source.clone())?;
        // The same grammar parsed it just now, so this can't fail.
        let document = source
            .parse::<DocumentMut>()
            .expect("a manifest that deserialized is a TOML document");
        Ok(Self {
            path,
            shown: shown.to_path_buf(),
            manifest,
            document,
        })
    }

    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The entry declared under `key`.
    pub(crate) fn tool(&self, key: &ToolKey) -> Result<&ToolEntry, NotDeclared> {
        self.manifest.tools.get(key).ok_or_else(|| {
            let declared: Vec<_> = self.manifest.tools.keys().map(ToolKey::as_str).collect();
            let help = if declared.is_empty() {
                "it declares no tools; declare one with `slcr tools add <SOURCE>`".to_owned()
            } else {
                format!("the tools it declares are: {}", declared.join(", "))
            };
            NotDeclared {
                manifest: self.shown.clone(),
                key: key.clone(),
                help,
            }
        })
    }

    /// Declare `entry` under `key`, replacing any entry already there.
    pub(crate) fn set_tool(&mut self, key: &ToolKey, entry: ToolEntry) {
        let tools = self.document.entry("tools").or_insert_with(|| {
            // `[tools.<key>]` headers, with no bare `[tools]` above them.
            let mut tools = Table::new();
            tools.set_implicit(true);
            Item::Table(tools)
        });
        let (field, location) = entry.source().field();
        let mut source = InlineTable::new();
        source.insert(field, location.into());
        let version = entry.version().to_string();
        let pin = entry.pin().to_string();
        match tools {
            Item::Table(tools) => {
                let mut table = Table::new();
                table.insert("version", toml_edit::value(version));
                table.insert("source", toml_edit::value(source));
                table.insert("pin", toml_edit::value(pin));
                tools.insert(key.as_str(), Item::Table(table));
            }
            Item::Value(Value::InlineTable(tools)) => {
                let mut table = InlineTable::new();
                table.insert("version", version.into());
                table.insert("source", Value::InlineTable(source));
                table.insert("pin", pin.into());
                tools.insert(key.as_str(), Value::InlineTable(table));
            }
            _ => unreachable!("a manifest that deserialized has a `tools` table"),
        }
        self.manifest.tools.insert(key.clone(), entry);
    }

    /// Re-pin the tool declared under `key` to the descriptor with
    /// `version` and `pin`, keeping its source. Its entry is edited in
    /// place, so its position, layout, and comments survive.
    ///
    /// # Panics
    ///
    /// If the manifest declares no tool under `key`.
    pub(crate) fn repin_tool(&mut self, key: &ToolKey, version: DescriptorVersion, pin: Pin) {
        let table = self
            .tools_table()
            .and_then(|tools| tools.get_mut(key.as_str()))
            .and_then(Item::as_table_like_mut)
            .expect("a declared tool has an entry in the document");
        replace_value(table, "version", version.to_string());
        replace_value(table, "pin", pin.to_string());
        let entry = self
            .manifest
            .tools
            .get_mut(key)
            .expect("a declared tool has an entry in the manifest");
        entry.version = version;
        entry.pin = pin;
    }

    /// Remove the tool declared under `key`, and return its entry. The
    /// `tools` table goes too when it is left empty, as the schema
    /// requires.
    pub(crate) fn remove_tool(&mut self, key: &ToolKey) -> Result<ToolEntry, NotDeclared> {
        self.tool(key)?;
        let tools = self
            .tools_table()
            .expect("a manifest that declares a tool has a `tools` table");
        tools.remove(key.as_str());
        if tools.is_empty() {
            self.document.remove("tools");
        }
        Ok(self
            .manifest
            .tools
            .remove(key)
            .expect("a declared tool has an entry in the manifest"))
    }

    /// The document's `tools` table, whether it is written as `[tools.*]`
    /// headers, a `[tools]` table, or an inline table.
    fn tools_table(&mut self) -> Option<&mut dyn TableLike> {
        self.document
            .get_mut("tools")
            .and_then(Item::as_table_like_mut)
    }

    /// Write the manifest back, atomically.
    pub(crate) fn save(&self, fs: &FileSystem) -> Result<()> {
        let contents = self.document.to_string();
        debug_assert!(
            toml::from_str::<Manifest>(&contents).is_ok(),
            "an edited manifest is still valid:\n{contents}"
        );
        fs.replace_file(&self.path, &contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::source::SourceArg;

    const EXAMPLE: &str = r#"# SLCR.toml
formatVersion = "1"
specs = ["docs/**/*.slcr.toml"]

[tools.cargo-nextest]
version = "1.0.0"
source = { url = "https://example.com/descriptors/cargo-nextest/1.0.0.toml" }
pin = "blake3:9f2c4e1a7b3d5f60812a4c6e8f0b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b"

[tools.ast-grep]
version = "0.3.1"
source = { path = "tools/descriptors/ast-grep.toml" }
pin = "blake3:41ab9c7e5d3f1a0b2c4d6e8f0a1b3c5d7e9f1a2b4c6d8e0f2a3b5c7d9e1f3a5b"
"#;

    fn parse(source: &str) -> Result<Manifest, toml::de::Error> {
        toml::from_str(source)
    }

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    #[test]
    fn the_schema_page_example_parses() {
        let manifest = parse(EXAMPLE).unwrap();
        assert_eq!(manifest.specs()[0].as_str(), "docs/**/*.slcr.toml");
        let keys: Vec<_> = manifest.tools().keys().map(ToolKey::as_str).collect();
        assert_eq!(keys, ["ast-grep", "cargo-nextest"]);
        let entry = &manifest.tools()[&key("ast-grep")];
        assert_eq!(entry.version().to_string(), "0.3.1");
        assert_eq!(
            entry.source().field(),
            ("path", "tools/descriptors/ast-grep.toml")
        );
    }

    #[test]
    fn tools_are_optional() {
        let manifest = parse("formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n").unwrap();
        assert!(manifest.tools().is_empty());
    }

    #[test]
    fn invalid_manifests_are_rejected() {
        let minimal = "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";
        for invalid in [
            "specs = [\"SPEC.slcr.yml\"]\n".to_owned(),
            "formatVersion = \"1\"\n".to_owned(),
            "formatVersion = \"2\"\nspecs = [\"SPEC.slcr.yml\"]\n".to_owned(),
            "formatVersion = \"1\"\nspecs = []\n".to_owned(),
            "formatVersion = \"1\"\nspecs = [\"a.toml\", \"a.toml\"]\n".to_owned(),
            "formatVersion = \"1\"\nspecs = [\"/abs.toml\"]\n".to_owned(),
            "formatVersion = \"1\"\nspecs = [\"../up.toml\"]\n".to_owned(),
            format!("{minimal}status = \"strict\"\n"),
            format!("{minimal}[tools]\n"),
            EXAMPLE.replace("[tools.ast-grep]", "[tools.Ast_Grep]"),
            EXAMPLE.replace("version = \"0.3.1\"", "version = \"0.3\""),
            EXAMPLE.replace("pin = \"blake3:41ab", "pin = \"xxh3:41ab"),
            EXAMPLE.replace("version = \"0.3.1\"\n", ""),
            EXAMPLE.replace("version = \"0.3.1\"", "version = \"0.3.1\"\nrev = \"main\""),
            EXAMPLE.replace("source = { path", "source = { git"),
        ] {
            assert!(parse(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn setting_a_tool_preserves_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST), EXAMPLE).unwrap();
        let project = Project::find_from(dir.path()).unwrap();
        let mut file = ManifestFile::load(&project).unwrap();
        let source = match "https://example.com/jev/2.0.0.toml".parse().unwrap() {
            SourceArg::Url(url) => Source::Url(url),
            SourceArg::Path(_) => unreachable!(),
        };
        let version = DescriptorVersion::try_from("2.0.0".to_owned()).unwrap();
        file.set_tool(
            &key("jev"),
            ToolEntry::new(version, source, Pin::of(b"jev")),
        );
        file.save(&FileSystem::new().unwrap()).unwrap();

        let written = std::fs::read_to_string(dir.path().join(MANIFEST)).unwrap();
        assert_eq!(
            written,
            format!(
                "{EXAMPLE}\n[tools.jev]\nversion = \"2.0.0\"\n\
                 source = {{ url = \"https://example.com/jev/2.0.0.toml\" }}\n\
                 pin = \"{}\"\n",
                Pin::of(b"jev")
            )
        );
        assert_eq!(parse(&written).unwrap().tools().len(), 3);
    }

    #[test]
    fn the_first_tool_gets_its_own_header() {
        let dir = tempfile::tempdir().unwrap();
        let minimal = "formatVersion = \"1\" # the only version\nspecs = [\"SPEC.slcr.yml\"]\n";
        std::fs::write(dir.path().join(MANIFEST), minimal).unwrap();
        let project = Project::find_from(dir.path()).unwrap();
        let mut file = ManifestFile::load(&project).unwrap();
        let source: Source = toml::from_str("path = \"jev.toml\"").unwrap();
        let version = DescriptorVersion::try_from("1.0.0".to_owned()).unwrap();
        file.set_tool(&key("jev"), ToolEntry::new(version, source, Pin::of(b"")));
        file.save(&FileSystem::new().unwrap()).unwrap();

        let written = std::fs::read_to_string(dir.path().join(MANIFEST)).unwrap();
        assert_eq!(
            written,
            format!(
                "{minimal}\n[tools.jev]\nversion = \"1.0.0\"\nsource = {{ path = \"jev.toml\" }}\n\
                 pin = \"{}\"\n",
                Pin::of(b"")
            )
        );
    }

    #[test]
    fn a_tool_joins_an_inline_tools_table() {
        let dir = tempfile::tempdir().unwrap();
        let inline = "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\ntools = { jev = \
                      { version = \"1.0.0\", source = { path = \"jev.toml\" }, pin = \"PIN\" } }\n"
            .replace("PIN", &Pin::of(b"jev").to_string());
        std::fs::write(dir.path().join(MANIFEST), &inline).unwrap();
        let project = Project::find_from(dir.path()).unwrap();
        let mut file = ManifestFile::load(&project).unwrap();
        let source: Source = toml::from_str("path = \"curl.toml\"").unwrap();
        let version = DescriptorVersion::try_from("2.0.0".to_owned()).unwrap();
        file.set_tool(
            &key("curl"),
            ToolEntry::new(version, source, Pin::of(b"curl")),
        );
        file.save(&FileSystem::new().unwrap()).unwrap();

        let written = std::fs::read_to_string(dir.path().join(MANIFEST)).unwrap();
        let manifest = parse(&written).unwrap();
        let keys: Vec<_> = manifest.tools().keys().map(ToolKey::as_str).collect();
        assert_eq!(keys, ["curl", "jev"]);
        // Still one inline table, with the new tool at its end.
        assert_eq!(written.lines().count(), 3, "{written}");
        assert!(written.contains("pin = \"blake3:"), "{written}");
        assert!(
            written.trim_end().ends_with(&format!(
                "curl = {{ version = \"2.0.0\", source = {{ path = \"curl.toml\" }}, pin = \"{}\" }} }}",
                Pin::of(b"curl")
            )),
            "{written}"
        );
    }

    #[test]
    fn the_project_is_found_in_the_nearest_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join(MANIFEST), "").unwrap();

        let project = Project::find_from(&nested).unwrap();
        assert_eq!(project.root(), dir.path());
        assert_eq!(project.shown_manifest(), Path::new("../../SLCR.toml"));

        std::fs::write(dir.path().join("a").join(MANIFEST), "").unwrap();
        let project = Project::find_from(&nested).unwrap();
        assert_eq!(project.root(), dir.path().join("a"));
        assert_eq!(project.shown_manifest(), Path::new("../SLCR.toml"));

        let project = Project::find_from(dir.path()).unwrap();
        assert_eq!(project.shown_manifest(), Path::new("SLCR.toml"));
    }

    #[test]
    fn a_near_miss_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("slcr.toml"), "").unwrap();
        let err = Project::find_from(&nested).unwrap_err();
        assert_eq!(err.to_string(), "../slcr.toml is not named SLCR.toml");
    }
}
