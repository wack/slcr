use std::{
    collections::{BTreeMap, HashSet},
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use derive_getters::Getters;
use miette::{Diagnostic, Result};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::source::{Source, is_project_relative};
use super::{DescriptorVersion, ToolKey, pin::Pin};
use crate::fs::{FileSystem, ReadError, format::Format, format::ParseError};
use crate::project::problem::display;
use crate::project::{Layout, relative_to};
use crate::spec::graph::FormatVersion;

/// The project manifest's name. Its directory is the SLCR directory.
pub const MANIFEST: &str = "SLCR.toml";

/// An SLCR project on disk: its SLCR directory, which holds `SLCR.toml`
/// and the project's specifications and validator files, and its project
/// root, the code its validators check, which is the SLCR directory
/// unless they are kept apart; and how messages name the manifest.
///
/// A program names one with [Project::open], and vets, installs, and
/// checks it with [Project::vet], [Project::install], and
/// [Project::check].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    layout: Layout,
    /// The manifest's path relative to the working directory, such as
    /// `SLCR.toml` or `../../SLCR.toml`: how messages name it.
    shown: PathBuf,
}

/// The ways finding the project can fail.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum ProjectError {
    #[error("could not find {MANIFEST} in the working directory or any parent directory")]
    #[diagnostic(help(
        "create {MANIFEST} beside the project's specifications, with `formatVersion = \"1\"` \
         and the specification files the project consumes, e.g. \
         `specs = [\"SPEC.slcr.toml\"]`"
    ))]
    NotFound,

    /// The directory named as the one that holds the manifest doesn't.
    #[error("{} holds no {MANIFEST}", .dir.display())]
    #[diagnostic(help(
        "name the directory of {MANIFEST} itself: its parent directories are not searched"
    ))]
    NoManifest {
        /// As it was given.
        dir: PathBuf,
    },

    /// A near miss, such as `slcr.toml`, which a case-insensitive
    /// filesystem would find and Linux CI wouldn't.
    #[error("{} is not named {MANIFEST}", .path.display())]
    #[diagnostic(help("the manifest's name is case-sensitive: rename it to {MANIFEST}"))]
    Misnamed { path: PathBuf },

    /// A directory given for the project that isn't one.
    #[error(transparent)]
    #[diagnostic(transparent)]
    UnusableDirectory(UnusableDirectory),
}

/// A directory named for a project that can't be used as one, such as one
/// that doesn't exist.
#[derive(Debug, Error, Diagnostic)]
#[error("{given}, {}, {problem}", .dir.display())]
pub struct UnusableDirectory {
    given: Given,
    /// As it was given.
    dir: PathBuf,
    problem: DirectoryProblem,
    /// What to name instead.
    #[help]
    help: String,
}

impl UnusableDirectory {
    /// Which of the project's directories it was named as.
    pub fn given(&self) -> Given {
        self.given
    }

    /// The directory, as it was named.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Why it can't be used.
    pub fn problem(&self) -> &DirectoryProblem {
        &self.problem
    }
}

/// Which of a project's directories a caller named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Given {
    /// The directory that holds `SLCR.toml`.
    SlcrDir,
    /// The directory the validators check.
    ProjectRoot,
}

impl Display for Given {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SlcrDir => "the SLCR directory",
            Self::ProjectRoot => "the project root",
        })
    }
}

/// Why a directory given for a project can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirectoryProblem {
    Missing,
    NotADirectory,
    /// A program named it by a relative path, which would be relative to
    /// a working directory the library never reads.
    Relative,
    /// It couldn't be resolved, for this reason.
    Unreadable(String),
}

impl Display for DirectoryProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("does not exist"),
            Self::NotADirectory => f.write_str("is not a directory"),
            Self::Relative => f.write_str("is not an absolute path"),
            Self::Unreadable(reason) => write!(f, "can't be read: {reason}"),
        }
    }
}

impl Project {
    /// The project laid out as `layout`, whose manifest messages name
    /// `shown`, its path relative to the working directory.
    fn new(layout: Layout, shown: PathBuf) -> Self {
        Self { layout, shown }
    }

    /// The project a command works on, from the directories it was given,
    /// each relative to `cwd`, the working directory, an absolute path:
    /// the one whose manifest is in `slcr_dir`, if given, or else the one
    /// that holds `cwd`; and whose project root is `project_root`, if
    /// given, or else its SLCR directory.
    pub(crate) fn locate(
        slcr_dir: Option<&Path>,
        project_root: Option<&Path>,
        cwd: &Path,
    ) -> Result<Self, ProjectError> {
        match slcr_dir {
            Some(slcr_dir) => Self::at(slcr_dir, project_root, cwd),
            None => {
                let project_root = project_root
                    .map(|root| directory(Given::ProjectRoot, root, cwd))
                    .transpose()?;
                let project = Self::find_from(cwd)?;
                Ok(match project_root {
                    Some(root) => project.with_project_root(root),
                    None => project,
                })
            }
        }
    }

    /// The project whose SLCR directory, which holds `SLCR.toml`, is
    /// `slcr_dir`, and whose project root, the directory its validators
    /// check, is `project_root`: the same directory, unless the project's
    /// SLCR files are kept apart from its code. Both must be absolute, and
    /// are resolved to canonical paths. Only `slcr_dir` is searched for
    /// the manifest, never its parents, and the project names its files
    /// relative to it.
    pub fn open(
        slcr_dir: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ProjectError> {
        let (slcr_dir, project_root) = (slcr_dir.as_ref(), project_root.as_ref());
        // Files are named relative to the SLCR directory, as `slcr` names
        // them run there.
        let (names_from, _) = Self::resolve_absolute(slcr_dir, project_root)?;
        Self::at(slcr_dir, Some(project_root), &names_from)
    }

    /// The project whose manifest is in `slcr_dir`, and whose project root
    /// is `project_root`, if given, or else `slcr_dir`. Both are relative
    /// to `cwd`, the working directory, an absolute path, and are resolved
    /// to canonical absolute paths. Only `slcr_dir` itself is searched for
    /// the manifest, never its parents.
    pub(crate) fn at(
        slcr_dir: &Path,
        project_root: Option<&Path>,
        cwd: &Path,
    ) -> Result<Self, ProjectError> {
        let resolved = directory(Given::SlcrDir, slcr_dir, cwd)?;
        let project_root = project_root
            .map(|root| directory(Given::ProjectRoot, root, cwd))
            .transpose()?;
        match manifest_in(&resolved) {
            Lookup::Found => {}
            Lookup::Misnamed(name) => {
                return Err(ProjectError::Misnamed {
                    path: slcr_dir.join(name),
                });
            }
            Lookup::Absent => {
                return Err(ProjectError::NoManifest {
                    dir: slcr_dir.to_path_buf(),
                });
            }
        }
        let shown = relative_to(&resolved.join(MANIFEST), cwd);
        let project = Self::new(Layout::single(resolved), shown);
        Ok(match project_root {
            Some(root) => project.with_project_root(root),
            None => project,
        })
    }

    /// The project whose manifest is in `dir`, relative to `cwd` or
    /// absolute, if that directory holds one; if it holds none, the
    /// directory, as a canonical absolute path, where one could be
    /// created. Its parents are never searched.
    pub(crate) fn in_dir(dir: &Path, cwd: &Path) -> Result<Result<Self, PathBuf>, ProjectError> {
        let resolved = directory(Given::SlcrDir, dir, cwd)?;
        match manifest_in(&resolved) {
            Lookup::Found => Self::at(dir, None, cwd).map(Ok),
            Lookup::Misnamed(name) => Err(ProjectError::Misnamed {
                path: dir.join(name),
            }),
            Lookup::Absent => Ok(Err(resolved)),
        }
    }

    /// `slcr_dir` and `project_root`, which must be absolute, as the
    /// canonical paths of directories.
    pub(crate) fn resolve_absolute(
        slcr_dir: &Path,
        project_root: &Path,
    ) -> Result<(PathBuf, PathBuf), ProjectError> {
        for (role, dir) in [
            (Given::SlcrDir, slcr_dir),
            (Given::ProjectRoot, project_root),
        ] {
            if !dir.is_absolute() {
                return Err(unusable(role, dir, DirectoryProblem::Relative));
            }
        }
        Ok((
            directory(Given::SlcrDir, slcr_dir, slcr_dir)?,
            directory(Given::ProjectRoot, project_root, project_root)?,
        ))
    }

    /// This project, with its project root at `project_root`, an absolute
    /// path.
    fn with_project_root(self, project_root: PathBuf) -> Self {
        let slcr_dir = self.layout.slcr_dir().to_path_buf();
        Self {
            layout: Layout::new(slcr_dir, project_root),
            ..self
        }
    }

    /// The project that holds `dir`, an absolute path, found by walking up
    /// from it to the nearest `SLCR.toml`, the way Cargo finds
    /// `Cargo.toml`. Its project root is its SLCR directory.
    pub(crate) fn find_from(dir: &Path) -> Result<Self, ProjectError> {
        let mut up = PathBuf::new();
        for ancestor in dir.ancestors() {
            match manifest_in(ancestor) {
                Lookup::Found => {
                    return Ok(Self::new(
                        Layout::single(ancestor.to_path_buf()),
                        up.join(MANIFEST),
                    ));
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

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The directory that holds `SLCR.toml` and the project's SLCR files,
    /// as an absolute path: canonical, unless the project was found by
    /// walking up from a working directory that isn't.
    pub fn slcr_dir(&self) -> &Path {
        self.layout.slcr_dir()
    }

    /// The directory the project's validators check, as a canonical
    /// absolute path.
    pub fn project_root(&self) -> &Path {
        self.layout.project_root()
    }

    /// The manifest's absolute path.
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.slcr_dir().join(MANIFEST)
    }

    /// The manifest's path relative to the working directory.
    pub(crate) fn shown_manifest(&self) -> &Path {
        &self.shown
    }
}

/// `given`, a directory relative to `cwd` that a caller named as `role`,
/// as a canonical absolute path: symlinks followed, so that it compares
/// with the paths under it as they are found.
fn directory(role: Given, given: &Path, cwd: &Path) -> Result<PathBuf, ProjectError> {
    let invalid = |problem| unusable(role, given, problem);
    match std::fs::canonicalize(cwd.join(given)) {
        Ok(resolved) if resolved.is_dir() => Ok(resolved),
        Ok(_) => Err(invalid(DirectoryProblem::NotADirectory)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(invalid(DirectoryProblem::Missing))
        }
        Err(err) => Err(invalid(DirectoryProblem::Unreadable(err.to_string()))),
    }
}

/// The error for `given`, a directory a caller named as `role`, which has
/// `problem`.
fn unusable(role: Given, given: &Path, problem: DirectoryProblem) -> ProjectError {
    let help = match role {
        Given::SlcrDir => match manifest_file_parent(given) {
            Some(parent) if problem == DirectoryProblem::NotADirectory => format!(
                "name the directory that holds {MANIFEST}: did you mean `{}`?",
                parent.display()
            ),
            _ => format!("name the directory that holds {MANIFEST}"),
        },
        Given::ProjectRoot => "name the directory the validators check".to_owned(),
    };
    ProjectError::UnusableDirectory(UnusableDirectory {
        given: role,
        dir: given.to_path_buf(),
        problem,
        help,
    })
}

/// If `given` names the manifest itself, the directory that holds it, as
/// it would be given: `.` for a bare `SLCR.toml`.
fn manifest_file_parent(given: &Path) -> Option<&Path> {
    if given.file_name()? != MANIFEST {
        return None;
    }
    match given.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Some(parent),
        _ => Some(Path::new(".")),
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
///
/// It serializes in its canonical form: `formatVersion`, `specs`, then
/// `tools`, omitted when empty, in key order, each with its `version`,
/// `source`, and `pin`. [Manifest::to_toml] writes that form as the TOML
/// `slcr tools add` writes.
#[derive(Clone, Debug, PartialEq, Getters, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    format_version: FormatVersion,
    /// The specification files the project consumes, as globs.
    #[serde(deserialize_with = "non_empty_set")]
    specs: Vec<ProjectGlob>,
    /// The declared descriptors, by manifest key.
    #[serde(
        default,
        deserialize_with = "non_empty_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    tools: BTreeMap<ToolKey, ToolEntry>,
}

impl Manifest {
    /// Read `source` as the contents of `SLCR.toml`. An error names the
    /// file `SLCR.toml` and labels where in `source` the problem is, as
    /// `slcr vet` reports a manifest it can't load.
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        Format::Toml.parse(Path::new(MANIFEST), source.to_owned())
    }

    /// The manifest as TOML: `formatVersion` and `specs`, then a
    /// `[tools.<key>]` table for each tool in key order, with its `source`
    /// inline, as `slcr tools add` writes one. Comments and formatting of
    /// the TOML the manifest was read from are not kept, but it reads back
    /// as an equal manifest.
    pub fn to_toml(&self) -> String {
        let mut document = DocumentMut::new();
        document.insert(
            "formatVersion",
            toml_edit::value(self.format_version.as_str()),
        );
        let specs: toml_edit::Array = self.specs.iter().map(ProjectGlob::as_str).collect();
        document.insert("specs", toml_edit::value(specs));
        for (key, entry) in &self.tools {
            insert_tool(&mut document, key, entry);
        }
        document.to_string()
    }
}

/// Declare `entry` under `key` in `document`, replacing any entry already
/// there: as a `[tools.<key>]` table, or as an entry of an inline `tools`
/// table if the document has one, with its `source` inline.
fn insert_tool(document: &mut DocumentMut, key: &ToolKey, entry: &ToolEntry) {
    let tools = document.entry("tools").or_insert_with(|| {
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
}

/// One `[tools]` entry: a descriptor, where to fetch it, and its pin.
#[derive(Clone, Debug, PartialEq, Eq, Getters, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEntry {
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

/// A glob relative to the SLCR directory, such as `docs/**/*.slcr.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ProjectGlob(String);

impl TryFrom<String> for ProjectGlob {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_project_relative(&value) {
            Ok(Self(value))
        } else {
            Err(format!(
                "{value:?} is not a glob relative to the directory of SLCR.toml, which uses \
                 forward slashes, with no leading `/` and no `..` segment"
            ))
        }
    }
}

impl ProjectGlob {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ProjectGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The manifest `slcr init` creates for a new project: the format version
/// and the one specification file it creates, with no tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NewManifest {
    format_version: FormatVersion,
    specs: [ProjectGlob; 1],
}

impl NewManifest {
    pub(crate) fn new(spec: ProjectGlob) -> Self {
        Self {
            format_version: FormatVersion::V1,
            specs: [spec],
        }
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
///
/// It serializes like a [crate::project::Problem], with the `code`
/// `tool-not-declared`.
#[derive(Debug, Error, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename = "tool-not-declared")]
#[error("{} declares no tool named {key}", .manifest.display())]
pub struct NotDeclared {
    #[serde(serialize_with = "display")]
    manifest: PathBuf,
    key: ToolKey,
    /// The keys the manifest does declare.
    declared: Vec<ToolKey>,
}

impl Diagnostic for NotDeclared {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        let declared: Vec<_> = self.declared.iter().map(ToolKey::as_str).collect();
        Some(Box::new(if declared.is_empty() {
            "it declares no tools; declare one with `slcr tools add <SOURCE>`".to_owned()
        } else {
            format!("the tools it declares are: {}", declared.join(", "))
        }))
    }
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
        self.manifest.tools.get(key).ok_or_else(|| NotDeclared {
            manifest: self.shown.clone(),
            key: key.clone(),
            declared: self.manifest.tools.keys().cloned().collect(),
        })
    }

    /// Declare `entry` under `key`, replacing any entry already there.
    pub(crate) fn set_tool(&mut self, key: &ToolKey, entry: ToolEntry) {
        insert_tool(&mut self.document, key, &entry);
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

    fn version(version: &str) -> DescriptorVersion {
        DescriptorVersion::try_from(version.to_owned()).unwrap()
    }

    /// A manifest that declares no tools.
    const MINIMAL: &str = "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";

    /// `source`, loaded as the manifest of a project in a temporary
    /// directory.
    fn load(source: &str) -> (tempfile::TempDir, ManifestFile) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST), source).unwrap();
        let file = ManifestFile::load(&Project::find_from(dir.path()).unwrap()).unwrap();
        (dir, file)
    }

    /// Save `file`, and return what it wrote, after checking that it reads
    /// back as the manifest `file` holds in memory.
    fn saved(dir: &tempfile::TempDir, file: &ManifestFile) -> String {
        file.save(&FileSystem::new().unwrap()).unwrap();
        let written = std::fs::read_to_string(dir.path().join(MANIFEST)).unwrap();
        assert_eq!(parse(&written).unwrap(), *file.manifest(), "{written}");
        written
    }

    /// The tool `name`, pinned to the hash of its name, written with
    /// dotted keys.
    fn dotted(name: &str) -> String {
        let pin = Pin::of(name.as_bytes());
        format!(
            "tools.{name}.version = \"1.0.0\"\ntools.{name}.source.path = \"{name}.toml\"\n\
             tools.{name}.pin = \"{pin}\"\n"
        )
    }

    /// The tool `name`, pinned to the hash of its name, with its source
    /// in a table of its own, under its own header.
    fn sourced(name: &str) -> String {
        let pin = Pin::of(name.as_bytes());
        format!(
            "\n[tools.{name}]\nversion = \"1.0.0\"\npin = \"{pin}\"\n\n\
             [tools.{name}.source]\npath = \"{name}.toml\"\n"
        )
    }

    #[test]
    fn an_undeclared_tool_is_an_error_that_names_the_declared_ones() {
        let (_dir, file) = load(EXAMPLE);
        let err = file.tool(&key("jev")).unwrap_err();
        assert_eq!(err.to_string(), "SLCR.toml declares no tool named jev");
        assert_eq!(
            err.help().unwrap().to_string(),
            "the tools it declares are: ast-grep, cargo-nextest"
        );

        let (_dir, file) = load(MINIMAL);
        let err = file.tool(&key("jev")).unwrap_err();
        assert_eq!(
            err.help().unwrap().to_string(),
            "it declares no tools; declare one with `slcr tools add <SOURCE>`"
        );
    }

    #[test]
    fn repinning_a_tool_changes_only_its_version_and_pin() {
        let (dir, mut file) = load(EXAMPLE);
        let pin = Pin::of(b"ast-grep");
        file.repin_tool(&key("ast-grep"), version("0.4.0"), pin);

        let entry = file.tool(&key("ast-grep")).unwrap();
        assert_eq!(entry.version(), &version("0.4.0"));
        assert_eq!(entry.pin(), &pin);
        assert_eq!(
            saved(&dir, &file),
            EXAMPLE.replace("\"0.3.1\"", "\"0.4.0\"").replace(
                "blake3:41ab9c7e5d3f1a0b2c4d6e8f0a1b3c5d7e9f1a2b4c6d8e0f2a3b5c7d9e1f3a5b",
                &pin.to_string()
            )
        );
    }

    #[test]
    fn removing_a_tool_removes_only_its_entry() {
        let (dir, mut file) = load(EXAMPLE);
        let removed = file.remove_tool(&key("cargo-nextest")).unwrap();
        assert_eq!(removed.version(), &version("1.0.0"));
        let keys: Vec<_> = file
            .manifest()
            .tools()
            .keys()
            .map(ToolKey::as_str)
            .collect();
        assert_eq!(keys, ["ast-grep"]);

        let nextest = EXAMPLE.find("[tools.cargo-nextest]").unwrap()
            ..EXAMPLE.find("[tools.ast-grep]").unwrap();
        let mut expected = EXAMPLE.to_owned();
        expected.replace_range(nextest, "");
        assert_eq!(saved(&dir, &file), expected);
    }

    #[test]
    fn removing_an_undeclared_tool_changes_nothing() {
        let (dir, mut file) = load(EXAMPLE);
        let err = file.remove_tool(&key("jev")).unwrap_err();
        assert_eq!(err.to_string(), "SLCR.toml declares no tool named jev");
        assert_eq!(file.manifest().tools().len(), 2);
        assert_eq!(saved(&dir, &file), EXAMPLE);
    }

    #[test]
    fn tools_written_with_dotted_keys_are_edited_in_place() {
        let (jev, curl) = (dotted("jev"), dotted("curl"));
        let (dir, mut file) = load(&format!("{MINIMAL}{jev}{curl}"));
        file.repin_tool(&key("curl"), version("2.0.0"), Pin::of(b"new"));
        file.remove_tool(&key("jev")).unwrap();
        let repinned = curl
            .replace("\"1.0.0\"", "\"2.0.0\"")
            .replace(&Pin::of(b"curl").to_string(), &Pin::of(b"new").to_string());
        assert_eq!(saved(&dir, &file), format!("{MINIMAL}{repinned}"));

        file.remove_tool(&key("curl")).unwrap();
        assert_eq!(saved(&dir, &file), MINIMAL);
    }

    #[test]
    fn a_tool_with_a_source_table_is_edited_in_place() {
        let (jev, curl) = (sourced("jev"), sourced("curl"));
        let (dir, mut file) = load(&format!("{MINIMAL}{jev}{curl}"));
        file.repin_tool(&key("curl"), version("2.0.0"), Pin::of(b"new"));
        // The source's own header goes with its tool.
        file.remove_tool(&key("jev")).unwrap();
        let repinned = curl
            .replace("\"1.0.0\"", "\"2.0.0\"")
            .replace(&Pin::of(b"curl").to_string(), &Pin::of(b"new").to_string());
        assert_eq!(saved(&dir, &file), format!("{MINIMAL}{repinned}"));

        file.remove_tool(&key("curl")).unwrap();
        assert_eq!(saved(&dir, &file), MINIMAL);
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
    fn a_new_manifest_reads_back_as_a_manifest() {
        let spec = ProjectGlob::try_from("specs/api.slcr.toml".to_owned()).unwrap();
        let written = Format::Toml
            .serialize(&NewManifest::new(spec.clone()))
            .unwrap();
        assert_eq!(
            written,
            "formatVersion = \"1\"\nspecs = [\"specs/api.slcr.toml\"]\n"
        );
        let manifest = parse(&written).unwrap();
        assert_eq!(manifest.specs(), &[spec]);
        assert!(manifest.tools().is_empty());
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
        // Its project root is its SLCR directory.
        assert_eq!(*project.layout(), Layout::single(dir.path().to_path_buf()));
        assert_eq!(project.shown_manifest(), Path::new("../../SLCR.toml"));

        std::fs::write(dir.path().join("a").join(MANIFEST), "").unwrap();
        let project = Project::find_from(&nested).unwrap();
        assert_eq!(project.slcr_dir(), dir.path().join("a"));
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

    /// A workspace, canonical as a working directory is, holding
    /// `app/SLCR.toml`, `app/src`, and `repo/src`.
    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(cwd.join("app/src")).unwrap();
        std::fs::create_dir_all(cwd.join("repo/src")).unwrap();
        std::fs::write(cwd.join("app").join(MANIFEST), "").unwrap();
        (dir, cwd)
    }

    #[test]
    fn a_named_slcr_directory_is_the_project_s() {
        let (_dir, cwd) = workspace();
        let project = Project::at(Path::new("app"), None, &cwd).unwrap();
        assert_eq!(*project.layout(), Layout::single(cwd.join("app")));
        assert_eq!(project.shown_manifest(), Path::new("app/SLCR.toml"));

        let project = Project::at(Path::new("../app"), Some(Path::new(".")), &cwd.join("repo"));
        let project = project.unwrap();
        assert_eq!(
            *project.layout(),
            Layout::new(cwd.join("app"), cwd.join("repo"))
        );
        assert_eq!(project.shown_manifest(), Path::new("../app/SLCR.toml"));
    }

    #[test]
    fn a_named_slcr_directory_is_never_searched_above() {
        let (_dir, cwd) = workspace();
        let err = Project::at(Path::new("app/src"), None, &cwd).unwrap_err();
        assert_eq!(err.to_string(), "app/src holds no SLCR.toml");
        std::fs::write(cwd.join("app/src/slcr.toml"), "").unwrap();
        let err = Project::at(Path::new("app/src"), None, &cwd).unwrap_err();
        assert_eq!(err.to_string(), "app/src/slcr.toml is not named SLCR.toml");
    }

    #[test]
    fn a_named_directory_must_be_one() {
        let (_dir, cwd) = workspace();
        let message = |slcr_dir: Option<&str>, root: Option<&str>| {
            Project::locate(
                slcr_dir.map(Path::new),
                root.map(Path::new),
                &cwd.join("app"),
            )
            .unwrap_err()
            .to_string()
        };
        assert_eq!(
            message(Some("missing"), None),
            "the SLCR directory, missing, does not exist"
        );
        assert_eq!(
            message(Some("SLCR.toml"), None),
            "the SLCR directory, SLCR.toml, is not a directory"
        );
        assert_eq!(
            message(None, Some("../missing")),
            "the project root, ../missing, does not exist"
        );
        assert_eq!(
            message(Some("."), Some("SLCR.toml")),
            "the project root, SLCR.toml, is not a directory"
        );
    }

    #[test]
    fn naming_the_manifest_suggests_its_directory() {
        let (_dir, cwd) = workspace();
        let help = |slcr_dir: &str, root: Option<&str>| {
            let err =
                Project::locate(Some(Path::new(slcr_dir)), root.map(Path::new), &cwd).unwrap_err();
            err.help().map(|help| help.to_string())
        };
        let did_you_mean = "name the directory that holds SLCR.toml: did you mean `app`?";
        assert_eq!(help("app/SLCR.toml", None).as_deref(), Some(did_you_mean));
        assert_eq!(
            help("app/SLCR.toml", Some("repo")).as_deref(),
            Some(did_you_mean)
        );
        let in_app = Project::locate(Some(Path::new("SLCR.toml")), None, &cwd.join("app"));
        assert_eq!(
            in_app.unwrap_err().help().unwrap().to_string(),
            "name the directory that holds SLCR.toml: did you mean `.`?"
        );
        // A missing one isn't the manifest.
        assert_eq!(
            help("repo/SLCR.toml", None).as_deref(),
            Some("name the directory that holds SLCR.toml")
        );
        assert_eq!(
            help("app", Some("app/SLCR.toml")).as_deref(),
            Some("name the directory the validators check")
        );
    }

    #[test]
    fn a_project_root_can_be_named_for_the_project_found() {
        let (_dir, cwd) = workspace();
        let project = Project::locate(None, Some(Path::new("../../repo")), &cwd.join("app/src"));
        let project = project.unwrap();
        assert_eq!(
            *project.layout(),
            Layout::new(cwd.join("app"), cwd.join("repo"))
        );
        assert_eq!(project.shown_manifest(), Path::new("../SLCR.toml"));
        // Without either, the project is found as before.
        let found = Project::locate(None, None, &cwd.join("app/src")).unwrap();
        assert_eq!(found, Project::find_from(&cwd.join("app/src")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn named_directories_are_resolved_through_symlinks() {
        let (_dir, cwd) = workspace();
        std::os::unix::fs::symlink(cwd.join("repo"), cwd.join("link")).unwrap();
        let project = Project::at(Path::new("app"), Some(Path::new("link")), &cwd).unwrap();
        assert_eq!(project.layout().project_root(), cwd.join("repo"));
    }
}
