use std::{
    ffi::OsStr,
    fmt::{self, Display},
    path::{Component, Path, PathBuf},
};

use clap::Args;
use miette::{Diagnostic, IntoDiagnostic, Result};
use thiserror::Error;

use crate::Terminal;
use crate::fs::{File, FileSystem};
use crate::project::discover;
use crate::spec::file::{DEFAULT_FILE, RequirementsFile, base_name};
use crate::spec::graph::SlcrRequirementsDocument;
use crate::spec::text::{InvalidSpecName, InvalidTitle, SpecName, Title};
use crate::tools::manifest::{
    MANIFEST, ManifestFile, NewManifest, Project, ProjectError, ProjectGlob,
};

/// The stem of the well-known file name, whatever its extension. Its
/// specification is named after the directory that holds it.
const WELL_KNOWN_STEM: &str = "SPEC.slcr";

/// The characters a glob gives a meaning, which a path that a new
/// manifest lists in `specs` must not contain.
const GLOB_METACHARACTERS: [char; 7] = ['*', '?', '[', ']', '{', '}', '\\'];

/// The arguments to `slcr init`.
#[derive(Args, Clone, Debug)]
pub struct InitArgs {
    /// The requirements file to create: `.yaml`, `.yml`, `.json`, or `.toml`.
    /// It must not exist yet.
    #[arg(value_name = "FILE", default_value = DEFAULT_FILE)]
    file: PathBuf,

    /// The specification's name, e.g. `todo-api`. Defaults to the file's
    /// name without its extension and any `.slcr` or `.spec`, or, for
    /// `SPEC.slcr.*`, to the name of its directory.
    #[arg(long, value_parser = parse_spec_name)]
    name: Option<SpecName>,

    /// The root Section's title. Defaults to the specification's name.
    #[arg(long, value_parser = parse_title)]
    title: Option<Title>,
}

fn parse_spec_name(value: &str) -> Result<SpecName, InvalidSpecName> {
    SpecName::try_from(value.to_owned())
}

fn parse_title(value: &str) -> Result<Title, InvalidTitle> {
    Title::try_from(value.to_owned())
}

/// Create a specification with its root and Glossary sections, and, unless
/// the working directory is already in a project, the project manifest,
/// `SLCR.toml`, that lists it.
///
/// The file's extension selects its format, as it does for `slcr vet`.
/// The command never overwrites anything: it fails if either file exists,
/// and it doesn't create a missing directory. It creates both files or
/// neither.
pub struct Init {
    terminal: Terminal,
    args: InitArgs,
}

impl Init {
    pub fn new(terminal: Terminal, args: InitArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let InitArgs {
            file: path,
            name,
            title,
        } = self.args;
        let file = RequirementsFile::new(path.clone())?;
        let name = match name {
            Some(name) => name,
            None => derived_name(&path)?,
        };
        let title = title.unwrap_or_else(|| Title::from(&name));
        if is_manifest_name(&path) {
            return Err(InvalidSpecPath::ManifestName { path }.into());
        }
        let cwd = std::env::current_dir().into_diagnostic()?;
        // A project found in a parent directory is left alone too:
        // projects don't nest.
        let manifest = match Project::find_from(&cwd) {
            Ok(project) => ManifestStatus::Existing(project),
            Err(ProjectError::NotFound) => {
                ManifestStatus::New(NewManifest::new(listed(&path, &cwd)?))
            }
            Err(err) => return Err(err.into()),
        };
        tracing::debug!(path = %path.display(), %name, "creating a requirements file");
        let document = SlcrRequirementsDocument::new(name, title);
        let new_manifest = match &manifest {
            ManifestStatus::New(contents) => Some((NewManifestFile::new(MANIFEST), contents)),
            ManifestStatus::Existing(_) => None,
        };
        create(&FileSystem::new()?, &file, &document, new_manifest)?;
        let created = Created {
            path: &path,
            name: document.spec().name(),
        };
        self.terminal.write_stdout_line(&created.to_string())?;
        self.terminal.write_stdout_line(&manifest.to_string())?;
        if let ManifestStatus::Existing(project) = &manifest
            && let Some(unlisted) = unlisted(project, &cwd, &path)
        {
            self.terminal.write_diagnostic(&unlisted)?;
        }
        Ok(())
    }
}

/// Why an existing project's manifest doesn't list the specification
/// `slcr init` just created at `path`, if it doesn't: its `specs` don't
/// match the file, the file is outside the project, or the manifest can't
/// be read to tell.
fn unlisted(project: &Project, cwd: &Path, path: &Path) -> Option<Unlisted> {
    let manifest = project.shown_manifest().to_path_buf();
    let spec = path.to_path_buf();
    // The file exists now, so it resolves, as the project's root did.
    let absolute = std::fs::canonicalize(cwd.join(path)).ok()?;
    let Ok(relative) = absolute.strip_prefix(project.root()) else {
        return Some(Unlisted::Outside { manifest, spec });
    };
    let Ok(file) = ManifestFile::load(project) else {
        return Some(Unlisted::Unreadable { manifest, spec });
    };
    let discovery = discover(project.root(), cwd, file.manifest().specs());
    if discovery
        .files
        .iter()
        .any(|file| file.relative() == relative)
    {
        return None;
    }
    let entry = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(Unlisted::NotMatched {
        manifest,
        spec,
        entry,
    })
}

/// A warning that the project's manifest doesn't list a new
/// specification, so `slcr vet` and `slcr check` won't read it.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[diagnostic(severity(Warning))]
pub enum Unlisted {
    #[error("the `specs` of {} don't match {}", .manifest.display(), .spec.display())]
    #[diagnostic(help("add \"{entry}\" to `specs` so that `slcr vet` and `slcr check` read it"))]
    NotMatched {
        manifest: PathBuf,
        spec: PathBuf,
        /// The file's path relative to the project root: the entry to add.
        entry: String,
    },

    #[error(
        "{} is outside the project of {}, whose `specs` can't list it",
        .spec.display(),
        .manifest.display()
    )]
    #[diagnostic(help(
        "move the file into the project, or run `slcr init` where it starts a project of its own"
    ))]
    Outside { manifest: PathBuf, spec: PathBuf },

    #[error(
        "could not read {} to tell whether its `specs` match {}",
        .manifest.display(),
        .spec.display()
    )]
    #[diagnostic(help("`slcr vet` reports what is wrong with the manifest"))]
    Unreadable { manifest: PathBuf, spec: PathBuf },
}

/// What `slcr init` does with the project manifest, which it reports as
/// its status, e.g. `SLCR.toml: created project manifest`.
enum ManifestStatus {
    /// Create one in the working directory, with these contents.
    New(NewManifest),
    /// Leave alone the one of this project.
    Existing(Project),
}

impl Display for ManifestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New(_) => write!(f, "{MANIFEST}: created project manifest"),
            Self::Existing(project) => write!(
                f,
                "{}: left the existing project manifest unchanged",
                project.shown_manifest().display()
            ),
        }
    }
}

/// The path where `slcr init` creates a manifest.
struct NewManifestFile {
    path: PathBuf,
}

impl NewManifestFile {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl File for NewManifestFile {
    type Data = NewManifest;

    fn extension(&self) -> &str {
        "toml"
    }

    fn path(&self, _fs: &FileSystem) -> Result<PathBuf> {
        Ok(self.path.clone())
    }
}

/// Create the specification, and the manifest if one is given: both, or,
/// if either fails, neither.
fn create(
    fs: &FileSystem,
    spec: &RequirementsFile,
    document: &SlcrRequirementsDocument,
    manifest: Option<(NewManifestFile, &NewManifest)>,
) -> Result<()> {
    let Some((manifest, contents)) = manifest else {
        return fs.create_file(spec, document);
    };
    let spec_path = spec.path(fs)?;
    // The likeliest reasons to refuse, found before anything is written.
    fs.ensure_vacant(&spec_path)?;
    fs.ensure_vacant(&manifest.path)?;
    fs.create_file(spec, document)?;
    if let Err(err) = fs.create_file(&manifest, contents) {
        // Take the specification back, so no half-made project is left.
        if let Err(cleanup) = std::fs::remove_file(&spec_path) {
            tracing::warn!(
                path = %spec_path.display(),
                %cleanup,
                "could not remove the new specification"
            );
        }
        return Err(err);
    }
    Ok(())
}

/// Whether `path`'s file name is the manifest's, in any case: a
/// specification there would be taken for the manifest, or for a misnamed
/// one.
fn is_manifest_name(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(MANIFEST))
}

/// How a new manifest in `cwd`, the absolute working directory, lists the
/// specification at `path` in `specs`: relative to `cwd`, with forward
/// slashes.
fn listed(path: &Path, cwd: &Path) -> Result<ProjectGlob, InvalidSpecPath> {
    let outside = || InvalidSpecPath::Outside {
        path: path.to_path_buf(),
    };
    let relative = relative_to(path, cwd).ok_or_else(outside)?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(outside());
        };
        let part = part.to_str().ok_or_else(|| InvalidSpecPath::NotUtf8 {
            path: path.to_path_buf(),
        })?;
        parts.push(part);
    }
    let listed = parts.join("/");
    if listed.contains(GLOB_METACHARACTERS) {
        return Err(InvalidSpecPath::GlobCharacters {
            path: path.to_path_buf(),
        });
    }
    // Plain names joined by `/`, with no backslash, are project-relative
    // unless there are none, when `path` is the working directory itself.
    ProjectGlob::try_from(listed).map_err(|_| outside())
}

/// `path` relative to `cwd`, an absolute directory, if it is inside it.
///
/// `.` and `..` are resolved without the filesystem first, which keeps the
/// path's own spelling. Failing that, the directory that would hold the
/// file is resolved, for a path that reaches `cwd` through a symlink.
fn relative_to(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let joined = cwd.join(path);
    let mut lexical = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            component => lexical.push(component),
        }
    }
    if let Ok(relative) = lexical.strip_prefix(cwd) {
        return Some(relative.to_path_buf());
    }
    let resolved = std::fs::canonicalize(joined.parent()?).ok()?;
    let relative = resolved.strip_prefix(cwd).ok()?;
    Some(relative.join(joined.file_name()?))
}

/// The error returned when `slcr init` can't create a specification at a
/// path, or a new manifest can't list it there.
#[derive(Debug, Error, Diagnostic)]
pub enum InvalidSpecPath {
    #[error("{} is named like the project manifest, {MANIFEST}", .path.display())]
    #[diagnostic(help("choose another name for the specification file, e.g. `{DEFAULT_FILE}`"))]
    ManifestName { path: PathBuf },

    #[error("{} is outside the working directory, where {MANIFEST} would be created", .path.display())]
    #[diagnostic(help(
        "{MANIFEST} lists specification files relative to its own directory: create the file \
         inside the working directory, or run `slcr init` from a directory that contains it"
    ))]
    Outside { path: PathBuf },

    #[error("{} can't be listed in {MANIFEST}, which reads `specs` as globs", .path.display())]
    #[diagnostic(help("choose a path without `*`, `?`, `[`, `]`, `{{`, `}}`, or `\\`"))]
    GlobCharacters { path: PathBuf },

    #[error("{} can't be listed in {MANIFEST}, which holds only UTF-8", .path.display())]
    #[diagnostic(help("choose a path that is valid UTF-8"))]
    NotUtf8 { path: PathBuf },
}

/// The name a specification takes from its file's path when none is given.
fn derived_name(path: &Path) -> Result<SpecName, UnnamedSpec> {
    let candidate = name_candidate(path);
    candidate
        .clone()
        .and_then(|candidate| SpecName::try_from(candidate).ok())
        .ok_or_else(|| UnnamedSpec {
            path: path.to_path_buf(),
            candidate,
        })
}

/// What a specification would be named after its file's path, valid or
/// not: the file's [base_name], or, for the well-known `SPEC.slcr.*`, the
/// name of the directory that holds it.
fn name_candidate(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(OsStr::to_str)?;
    if stem == WELL_KNOWN_STEM {
        return directory_name(path);
    }
    base_name(path).map(str::to_owned)
}

/// The name of the directory that holds `path`, which need not exist.
fn directory_name(path: &Path) -> Option<String> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        // A bare file name is in the working directory.
        _ => Path::new("."),
    };
    // Resolving the directory turns `.` and `..` into real names. A
    // directory that doesn't exist can't be resolved, but its own name
    // will do: creating the file fails later anyway, and says why.
    let resolved = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    resolved
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
}

/// The error returned when no `--name` is given and the file's path doesn't
/// yield a valid specification name.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("could not name the specification in {}", .path.display())]
pub struct UnnamedSpec {
    path: PathBuf,
    /// The name the path suggested, if any.
    candidate: Option<String>,
}

impl Diagnostic for UnnamedSpec {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        let pass_one = "pass one with `--name`, e.g. `--name todo-api`";
        Some(Box::new(match &self.candidate {
            Some(candidate) => format!(
                "{candidate:?} is not a valid specification name, which is lowercase letters \
                 and digits in dash-separated words; {pass_one}"
            ),
            None => format!("the path does not suggest a name; {pass_one}"),
        }))
    }
}

/// The status `slcr init` reports, e.g.
/// `SPEC.slcr.yml: created specification todo-api`.
struct Created<'a> {
    path: &'a Path,
    name: &'a SpecName,
}

impl Display for Created<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: created specification {}",
            self.path.display(),
            self.name
        )
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use clap::Parser;
    use figment::Jail;

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;
    use crate::fs::AlreadyExists;
    use crate::spec::file::UnsupportedFormat;
    use crate::terminal::render_plain;

    /// What `slcr init` writes for `todo-api`, in each format.
    const TODO_API_YAML: &str = "\
$schema: https://slcr.io/reference/schemas/spec/v1.schema.json
formatVersion: \"1\"
spec:
  name: todo-api
root:
  kind: section
  id: SEC-001
  title: todo-api
glossary:
  kind: section
  id: SEC-002
  title: Glossary
";

    const TODO_API_JSON: &str = r#"{
  "$schema": "https://slcr.io/reference/schemas/spec/v1.schema.json",
  "formatVersion": "1",
  "spec": {
    "name": "todo-api"
  },
  "root": {
    "kind": "section",
    "id": "SEC-001",
    "title": "todo-api"
  },
  "glossary": {
    "kind": "section",
    "id": "SEC-002",
    "title": "Glossary"
  }
}
"#;

    const TODO_API_TOML: &str = r#""$schema" = "https://slcr.io/reference/schemas/spec/v1.schema.json"
formatVersion = "1"

[spec]
name = "todo-api"

[root]
kind = "section"
id = "SEC-001"
title = "todo-api"

[glossary]
kind = "section"
id = "SEC-002"
title = "Glossary"
"#;

    /// The arguments `slcr` parses `init` and then `args` into.
    fn parse(args: &[&str]) -> Result<InitArgs, clap::Error> {
        let argv = ["slcr", "init"].iter().chain(args);
        match Cli::try_parse_from(argv)?.cmd() {
            Some(SlcrCommand::Init(args)) => Ok(args.clone()),
            _ => panic!("expected `init`"),
        }
    }

    /// Run `slcr init` with `args`.
    fn run(args: &[&str]) -> Result<()> {
        let argv = ["slcr", "--enable-colors", "never", "init"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    fn read(jail: &Jail, path: &str) -> String {
        std::fs::read_to_string(jail.directory().join(path)).unwrap()
    }

    /// The names of the entries in `dir`, relative to the jail's directory,
    /// sorted.
    fn entries(jail: &Jail, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(jail.directory().join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn spec_name(value: &str) -> SpecName {
        SpecName::try_from(value.to_owned()).unwrap()
    }

    fn load(path: &str) -> SlcrRequirementsDocument {
        let fs = FileSystem::new().unwrap();
        fs.load_file(RequirementsFile::new(path).unwrap()).unwrap()
    }

    #[test]
    fn the_file_defaults_to_the_well_known_name() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.file, PathBuf::from("SPEC.slcr.toml"));
        assert_eq!(args.file, PathBuf::from(DEFAULT_FILE));
        assert!(args.name.is_none());
        assert!(args.title.is_none());
    }

    #[test]
    fn the_well_known_name_is_a_supported_requirements_file() {
        assert!(RequirementsFile::new(DEFAULT_FILE).is_ok());
        assert_eq!(
            Path::new(DEFAULT_FILE).file_stem().and_then(OsStr::to_str),
            Some(WELL_KNOWN_STEM)
        );
    }

    #[test]
    fn init_accepts_a_file_a_name_and_a_title() {
        let args = parse(&[
            "specs/api.json",
            "--name",
            "todo-api",
            "--title",
            "TodoList API",
            "--enable-colors",
            "never",
        ])
        .unwrap();
        assert_eq!(args.file, PathBuf::from("specs/api.json"));
        assert_eq!(args.name, Some(spec_name("todo-api")));
        assert_eq!(args.title.unwrap().as_str(), "TodoList API");
    }

    #[test]
    fn init_accepts_only_one_file() {
        assert!(parse(&["a.yaml", "b.yaml"]).is_err());
    }

    #[test]
    fn invalid_names_are_rejected_when_parsing() {
        for name in ["", "Todo API", "todo_api", "todo--api", "-todo", "tödo"] {
            // `--name=` keeps clap from reading `-todo` as a flag.
            let err = parse(&[&format!("--name={name}")]).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{name:?}"
            );
            assert!(
                err.to_string()
                    .contains("is not a valid specification name"),
                "{err}"
            );
        }
    }

    #[test]
    fn invalid_titles_are_rejected_when_parsing() {
        for title in ["", " leading", "trailing ", "two\nlines"] {
            let err = parse(&[&format!("--title={title}")]).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{title:?}"
            );
            assert!(err.to_string().contains("is not a valid title"), "{err}");
        }
    }

    #[test]
    fn names_derive_from_the_file_stem() {
        for (path, name) in [
            ("todo-api.yaml", "todo-api"),
            ("todo-api.spec.yaml", "todo-api"),
            ("todo-api.slcr.yml", "todo-api"),
            ("todo-api.spec.json", "todo-api"),
            ("specs/billing.slcr.toml", "billing"),
            ("spec.yaml", "spec"),
            ("slcr.yml", "slcr"),
            // Only the uppercase stem is the well-known one.
            ("spec.slcr.yml", "spec"),
            ("v2.json", "v2"),
        ] {
            assert_eq!(derived_name(Path::new(path)), Ok(spec_name(name)), "{path}");
        }
    }

    #[test]
    fn only_one_suffix_is_dropped_from_the_stem() {
        // `todo-api.slcr.spec` drops `.spec` and keeps `.slcr`, which is not
        // part of a valid name.
        let err = derived_name(Path::new("todo-api.slcr.spec.yml")).unwrap_err();
        assert_eq!(err.candidate.as_deref(), Some("todo-api.slcr"));
    }

    #[test]
    fn stems_that_are_not_valid_names_are_rejected() {
        for (path, candidate) in [
            ("My Spec.yaml", "My Spec"),
            ("todo_api.toml", "todo_api"),
            ("Todo.json", "Todo"),
            (".spec.yaml", ""),
            ("SPEC.yml", "SPEC"),
        ] {
            let err = derived_name(Path::new(path)).unwrap_err();
            assert_eq!(
                err,
                UnnamedSpec {
                    path: PathBuf::from(path),
                    candidate: Some(candidate.to_owned()),
                },
                "{path}"
            );
        }
    }

    #[test]
    fn the_well_known_file_is_named_after_its_directory() {
        Jail::expect_with(|jail| {
            jail.create_dir("todo-api")?;
            jail.create_dir("billing")?;
            let derived = |path: &str| derived_name(Path::new(path)).map(String::from);
            assert_eq!(derived("todo-api/SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("todo-api/SPEC.slcr.json").unwrap(), "todo-api");
            assert_eq!(derived("todo-api/./SPEC.slcr.toml").unwrap(), "todo-api");
            assert_eq!(
                derived("todo-api/../billing/SPEC.slcr.yaml").unwrap(),
                "billing"
            );

            jail.change_dir("todo-api")?;
            assert_eq!(derived("SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("./SPEC.slcr.yml").unwrap(), "todo-api");
            assert_eq!(derived("../billing/SPEC.slcr.yml").unwrap(), "billing");
            Ok(())
        });
    }

    #[test]
    fn a_missing_directory_still_names_the_well_known_file() {
        Jail::expect_with(|_| {
            let name = derived_name(Path::new("absent/SPEC.slcr.yml")).unwrap();
            assert_eq!(name, spec_name("absent"));
            Ok(())
        });
    }

    #[test]
    fn a_directory_that_is_not_a_valid_name_is_rejected() {
        Jail::expect_with(|jail| {
            jail.create_dir("My Project")?;
            let err = derived_name(Path::new("My Project/SPEC.slcr.yml")).unwrap_err();
            assert_eq!(err.candidate.as_deref(), Some("My Project"));
            Ok(())
        });
    }

    #[test]
    fn the_filesystem_root_does_not_name_a_specification() {
        let err = derived_name(Path::new("/SPEC.slcr.yml")).unwrap_err();
        assert_eq!(err.candidate, None);
    }

    #[test]
    fn init_creates_the_well_known_file_and_a_manifest_by_default() {
        Jail::expect_with(|jail| {
            jail.create_dir("todo-api")?;
            jail.change_dir("todo-api")?;
            run(&[]).unwrap();
            assert_eq!(entries(jail, "todo-api"), ["SLCR.toml", "SPEC.slcr.toml"]);
            assert_eq!(read(jail, "todo-api/SPEC.slcr.toml"), TODO_API_TOML);
            assert_eq!(
                read(jail, "todo-api/SLCR.toml"),
                "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.toml\"]\n"
            );
            Ok(())
        });
    }

    #[test]
    fn the_new_manifest_lists_the_file_relative_to_the_working_directory() {
        Jail::expect_with(|jail| {
            for (dir, path, listed) in [
                ("a", "specs/api.slcr.toml", "specs/api.slcr.toml"),
                ("b", "./specs/../specs/./api.json", "specs/api.json"),
                ("c", "../c/specs/api.yaml", "specs/api.yaml"),
            ] {
                jail.create_dir(format!("{dir}/specs"))?;
                jail.change_dir(dir)?;
                run(&[path, "--name", "todo-api"]).unwrap();
                jail.change_dir(jail.directory())?;
                let manifest: crate::tools::manifest::Manifest =
                    toml::from_str(&read(jail, &format!("{dir}/SLCR.toml"))).unwrap();
                let specs: Vec<_> = manifest.specs().iter().map(ProjectGlob::as_str).collect();
                assert_eq!(specs, [listed], "{path}");
            }
            Ok(())
        });
    }

    #[test]
    fn an_absolute_path_inside_the_working_directory_is_listed_relative_to_it() {
        Jail::expect_with(|jail| {
            let path = jail.directory().join("SPEC.slcr.toml");
            let path = path.to_str().unwrap();
            run(&[path, "--name", "todo-api"]).unwrap();
            assert_eq!(
                read(jail, "SLCR.toml"),
                "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.toml\"]\n"
            );
            Ok(())
        });
    }

    #[test]
    fn an_existing_manifest_is_left_unchanged() {
        Jail::expect_with(|jail| {
            let manifest = "# Mine.\nformatVersion = \"1\"\nspecs = [\"docs/*.toml\"]\n";
            jail.create_file("SLCR.toml", manifest)?;
            jail.create_dir("docs")?;
            run(&["docs/api.slcr.toml"]).unwrap();
            assert_eq!(read(jail, "SLCR.toml"), manifest);
            assert_eq!(entries(jail, "."), ["SLCR.toml", "docs"]);
            assert_eq!(entries(jail, "docs"), ["api.slcr.toml"]);
            Ok(())
        });
    }

    #[test]
    fn a_manifest_in_a_parent_directory_is_left_unchanged_and_not_nested() {
        Jail::expect_with(|jail| {
            // Not even a valid manifest: `init` only warns that it can't
            // tell whether it lists the new file.
            jail.create_file("SLCR.toml", "not = [toml")?;
            jail.create_dir("a/b")?;
            jail.change_dir("a/b")?;
            run(&[]).unwrap();
            assert_eq!(read(jail, "SLCR.toml"), "not = [toml");
            assert_eq!(entries(jail, "a/b"), ["SPEC.slcr.toml"]);
            // Without a new manifest to list it, a file outside the working
            // directory is fine.
            run(&["../todo-api.json"]).unwrap();
            assert_eq!(entries(jail, "a"), ["b", "todo-api.json"]);
            Ok(())
        });
    }

    #[test]
    fn a_misnamed_manifest_is_an_error_and_nothing_is_written() {
        Jail::expect_with(|jail| {
            jail.create_file("slcr.toml", "")?;
            jail.create_dir("nested")?;
            for dir in [
                jail.directory().to_path_buf(),
                jail.directory().join("nested"),
            ] {
                jail.change_dir(dir)?;
                let err = run(&["--name", "todo-api"]).unwrap_err();
                let misnamed = err.downcast_ref::<ProjectError>().expect("a project error");
                assert!(matches!(misnamed, ProjectError::Misnamed { .. }), "{err}");
            }
            assert_eq!(entries(jail, "."), ["nested", "slcr.toml"]);
            assert!(entries(jail, "nested").is_empty());
            Ok(())
        });
    }

    #[test]
    fn a_file_a_new_manifest_cannot_list_is_not_created() {
        Jail::expect_with(|jail| {
            jail.create_dir("project")?;
            jail.change_dir("project")?;
            let outside = jail.directory().join("SPEC.slcr.toml");
            for path in [
                "../SPEC.slcr.toml",
                "../project/../SPEC.slcr.toml",
                outside.to_str().unwrap(),
            ] {
                let err = run(&[path, "--name", "todo-api"]).unwrap_err();
                assert!(
                    matches!(
                        err.downcast_ref::<InvalidSpecPath>(),
                        Some(InvalidSpecPath::Outside { .. })
                    ),
                    "{path}: {err}"
                );
            }
            for path in [
                "*.toml",
                "draft?.toml",
                "[draft].toml",
                "{a,b}.toml",
                "back\\slash.toml",
            ] {
                let err = run(&[path, "--name", "todo-api"]).unwrap_err();
                assert!(
                    matches!(
                        err.downcast_ref::<InvalidSpecPath>(),
                        Some(InvalidSpecPath::GlobCharacters { .. })
                    ),
                    "{path}: {err}"
                );
            }
            assert_eq!(entries(jail, "."), ["project"]);
            assert!(entries(jail, "project").is_empty());
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_cannot_be_listed() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"spec\xff.toml"));
        let err = listed(path, Path::new("/project")).unwrap_err();
        assert!(matches!(err, InvalidSpecPath::NotUtf8 { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_symlink_to_the_working_directory_is_inside_it() {
        Jail::expect_with(|jail| {
            jail.create_dir("project/specs")?;
            let link = jail.directory().join("link");
            std::os::unix::fs::symlink(jail.directory().join("project"), &link).unwrap();
            jail.change_dir("project")?;
            let cwd = std::env::current_dir().unwrap();
            let glob = listed(&link.join("specs/api.toml"), &cwd).unwrap();
            assert_eq!(glob.as_str(), "specs/api.toml");
            Ok(())
        });
    }

    #[test]
    fn a_file_named_like_the_manifest_is_not_created() {
        Jail::expect_with(|jail| {
            for path in ["SLCR.toml", "slcr.toml"] {
                let err = run(&[path, "--name", "todo-api"]).unwrap_err();
                assert!(
                    matches!(
                        err.downcast_ref::<InvalidSpecPath>(),
                        Some(InvalidSpecPath::ManifestName { .. })
                    ),
                    "{path}: {err}"
                );
            }
            assert!(entries(jail, ".").is_empty());
            Ok(())
        });
    }

    #[test]
    fn an_existing_specification_without_a_manifest_writes_nothing() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.toml", "keep = \"me\"\n")?;
            let err = run(&["--name", "todo-api"]).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(entries(jail, "."), ["SPEC.slcr.toml"]);
            assert_eq!(read(jail, "SPEC.slcr.toml"), "keep = \"me\"\n");
            Ok(())
        });
    }

    #[test]
    fn a_manifest_that_cannot_be_written_takes_the_specification_back() {
        Jail::expect_with(|jail| {
            let fs = FileSystem::new().unwrap();
            let spec = RequirementsFile::new("SPEC.slcr.toml").unwrap();
            let document = SlcrRequirementsDocument::new(
                spec_name("todo-api"),
                Title::try_from("todo-api".to_owned()).unwrap(),
            );
            let contents =
                NewManifest::new(ProjectGlob::try_from(DEFAULT_FILE.to_owned()).unwrap());
            // The manifest's directory is missing, which only the write
            // finds out.
            let manifest = NewManifestFile::new("missing/SLCR.toml");
            let err = create(&fs, &spec, &document, Some((manifest, &contents))).unwrap_err();
            assert_eq!(err.to_string(), "could not write missing/SLCR.toml");
            assert!(entries(jail, ".").is_empty());

            // Anything already at the manifest's path is found first.
            jail.create_file("SLCR.toml", "")?;
            let manifest = NewManifestFile::new(MANIFEST);
            let err = create(&fs, &spec, &document, Some((manifest, &contents))).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(entries(jail, "."), ["SLCR.toml"]);
            Ok(())
        });
    }

    #[test]
    fn init_writes_each_format_exactly() {
        Jail::expect_with(|jail| {
            for (path, expected) in [
                ("todo-api.spec.yaml", TODO_API_YAML),
                ("todo-api.slcr.yml", TODO_API_YAML),
                ("todo-api.spec.json", TODO_API_JSON),
                ("todo-api.spec.toml", TODO_API_TOML),
            ] {
                run(&[path]).unwrap();
                assert_eq!(read(jail, path), expected, "{path}");
            }
            Ok(())
        });
    }

    #[test]
    fn the_created_file_loads_and_passes_the_strictest_vetting() {
        Jail::expect_with(|_| {
            let paths = ["new.yaml", "new.yml", "new.json", "new.toml"];
            for path in paths {
                run(&[path, "--name", "todo-api"]).unwrap();
                let document = load(path);
                let expected = SlcrRequirementsDocument::new(
                    spec_name("todo-api"),
                    Title::try_from("todo-api".to_owned()).unwrap(),
                );
                assert_eq!(document, expected, "{path}");
                assert!(document.warnings().is_empty(), "{path}");
            }
            let argv = ["slcr", "--enable-colors", "never", "vet", "--deny-warnings"]
                .into_iter()
                .chain(paths);
            let cli = Cli::parse_from(argv);
            let terminal = Terminal::new(&cli);
            cli.cmd().clone().unwrap().dispatch(terminal).unwrap();
            Ok(())
        });
    }

    #[test]
    fn the_name_and_title_can_be_given() {
        Jail::expect_with(|_| {
            run(&[
                "My Spec.json",
                "--name",
                "billing",
                "--title",
                "Billing Service",
            ])
            .unwrap();
            let document = load("My Spec.json");
            assert_eq!(document.spec().name().as_str(), "billing");
            assert_eq!(document.root().title().as_str(), "Billing Service");
            assert_eq!(document.glossary().title().as_str(), "Glossary");
            Ok(())
        });
    }

    #[test]
    fn the_title_defaults_to_the_name() {
        Jail::expect_with(|_| {
            run(&["spec.toml", "--name", "billing"]).unwrap();
            let document = load("spec.toml");
            assert_eq!(document.root().title().as_str(), "billing");
            Ok(())
        });
    }

    #[test]
    fn an_unnameable_specification_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["My Spec.yaml"]).unwrap_err();
            let unnamed = err.downcast_ref::<UnnamedSpec>().expect("unnamed");
            assert_eq!(unnamed.candidate.as_deref(), Some("My Spec"));
            assert!(!jail.directory().join("My Spec.yaml").exists());
            Ok(())
        });
    }

    #[test]
    fn an_unsupported_format_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["SPEC.md", "--name", "todo-api"]).unwrap_err();
            assert!(err.downcast_ref::<UnsupportedFormat>().is_some(), "{err}");
            assert!(!jail.directory().join("SPEC.md").exists());
            Ok(())
        });
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        Jail::expect_with(|jail| {
            jail.create_file("todo-api.yaml", "keep: me\n")?;
            let err = run(&["todo-api.yaml"]).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(read(jail, "todo-api.yaml"), "keep: me\n");
            Ok(())
        });
    }

    #[test]
    fn init_fails_the_second_time() {
        Jail::expect_with(|jail| {
            run(&["todo-api.json"]).unwrap();
            let err = run(&["todo-api.json", "--title", "Changed"]).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert_eq!(read(jail, "todo-api.json"), TODO_API_JSON);
            Ok(())
        });
    }

    #[test]
    fn a_missing_directory_is_not_created() {
        Jail::expect_with(|jail| {
            let err = run(&["specs/todo-api.yaml"]).unwrap_err();
            assert_eq!(err.to_string(), "could not write specs/todo-api.yaml");
            assert_eq!(
                err.help().map(|help| help.to_string()).as_deref(),
                Some("create the directory specs first")
            );
            assert!(entries(jail, ".").is_empty());
            Ok(())
        });
    }

    #[test]
    fn the_status_names_the_file_and_the_specification() {
        let name = spec_name("todo-api");
        let created = Created {
            path: Path::new("SPEC.slcr.toml"),
            name: &name,
        };
        assert_eq!(
            created.to_string(),
            "SPEC.slcr.toml: created specification todo-api"
        );
    }

    #[test]
    fn the_status_says_what_became_of_the_manifest() {
        let spec = ProjectGlob::try_from("SPEC.slcr.toml".to_owned()).unwrap();
        assert_eq!(
            ManifestStatus::New(NewManifest::new(spec)).to_string(),
            "SLCR.toml: created project manifest"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST), "").unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        let project = Project::find_from(&dir.path().join("docs")).unwrap();
        assert_eq!(
            ManifestStatus::Existing(project).to_string(),
            "../SLCR.toml: left the existing project manifest unchanged"
        );
    }

    /// What [unlisted] says of `path` in the project around the working
    /// directory.
    fn unlisted_in(path: &str) -> Option<Unlisted> {
        let cwd = std::env::current_dir().unwrap();
        let project = Project::find_from(&cwd).unwrap();
        unlisted(&project, &cwd, Path::new(path))
    }

    #[test]
    fn a_specification_the_manifest_lists_needs_no_warning() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "SLCR.toml",
                "formatVersion = \"1\"\nspecs = [\"docs/*.toml\"]\n",
            )?;
            jail.create_dir("docs")?;
            run(&["docs/api.slcr.toml"]).unwrap();
            assert_eq!(unlisted_in("docs/api.slcr.toml"), None);
            Ok(())
        });
    }

    #[test]
    fn a_specification_the_manifest_does_not_list_is_named_with_its_entry() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "SLCR.toml",
                "formatVersion = \"1\"\nspecs = [\"docs/*.toml\"]\n",
            )?;
            jail.create_dir("specs/api")?;
            jail.change_dir("specs")?;
            run(&["api/billing.slcr.json"]).unwrap();
            let warning = unlisted_in("api/billing.slcr.json").unwrap();
            assert_eq!(
                warning,
                Unlisted::NotMatched {
                    manifest: PathBuf::from("../SLCR.toml"),
                    spec: PathBuf::from("api/billing.slcr.json"),
                    entry: "specs/api/billing.slcr.json".to_owned(),
                }
            );
            assert_eq!(
                render_plain(&warning),
                "  ⚠ the `specs` of ../SLCR.toml don't match api/billing.slcr.json\n  \
                 help: add \"specs/api/billing.slcr.json\" to `specs` so that `slcr vet` and\n        \
                 `slcr check` read it\n"
            );
            Ok(())
        });
    }

    #[test]
    fn a_specification_outside_the_project_cannot_be_listed() {
        Jail::expect_with(|jail| {
            jail.create_dir("project")?;
            jail.create_file(
                "project/SLCR.toml",
                "formatVersion = \"1\"\nspecs = [\"*.toml\"]\n",
            )?;
            jail.change_dir("project")?;
            run(&["../todo-api.json"]).unwrap();
            assert_eq!(
                unlisted_in("../todo-api.json"),
                Some(Unlisted::Outside {
                    manifest: PathBuf::from("SLCR.toml"),
                    spec: PathBuf::from("../todo-api.json"),
                })
            );
            Ok(())
        });
    }

    #[test]
    fn an_unreadable_manifest_is_reported_but_not_fatal() {
        Jail::expect_with(|jail| {
            jail.create_file("SLCR.toml", "not = [toml")?;
            run(&["--name", "todo-api"]).unwrap();
            assert_eq!(
                unlisted_in("SPEC.slcr.toml"),
                Some(Unlisted::Unreadable {
                    manifest: PathBuf::from("SLCR.toml"),
                    spec: PathBuf::from("SPEC.slcr.toml"),
                })
            );
            Ok(())
        });
    }

    #[test]
    fn invalid_spec_paths_render_with_their_help() {
        let path = |path: &str| PathBuf::from(path);
        let cases = [
            (
                InvalidSpecPath::ManifestName {
                    path: path("slcr.toml"),
                },
                "  × slcr.toml is named like the project manifest, SLCR.toml\n  \
                 help: choose another name for the specification file, e.g. `SPEC.slcr.toml`\n",
            ),
            (
                InvalidSpecPath::Outside {
                    path: path("../SPEC.slcr.toml"),
                },
                "  × ../SPEC.slcr.toml is outside the working directory, where SLCR.toml would\n  \
                 │ be created\n  \
                 help: SLCR.toml lists specification files relative to its own directory:\n        \
                 create the file inside the working directory, or run `slcr init` from\n        \
                 a directory that contains it\n",
            ),
            (
                InvalidSpecPath::GlobCharacters {
                    path: path("specs/[draft].slcr.toml"),
                },
                "  × specs/[draft].slcr.toml can't be listed in SLCR.toml, which reads `specs`\n  \
                 │ as globs\n  \
                 help: choose a path without `*`, `?`, `[`, `]`, `{`, `}`, or `\\`\n",
            ),
            (
                InvalidSpecPath::NotUtf8 {
                    path: path("SPEC.slcr.toml"),
                },
                "  × SPEC.slcr.toml can't be listed in SLCR.toml, which holds only UTF-8\n  \
                 help: choose a path that is valid UTF-8\n",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(render_plain(&err), expected);
        }
    }

    #[test]
    fn an_unnamed_specification_explains_the_invalid_name() {
        let err = UnnamedSpec {
            path: PathBuf::from("My Project/SPEC.slcr.yml"),
            candidate: Some("My Project".to_owned()),
        };
        assert_eq!(
            render_plain(&err),
            "  × could not name the specification in My Project/SPEC.slcr.yml\n  \
             help: \"My Project\" is not a valid specification name, which is lowercase\n        \
             letters and digits in dash-separated words; pass one with `--name`,\n        \
             e.g. `--name todo-api`\n"
        );
    }

    #[test]
    fn an_unnamed_specification_without_a_candidate_asks_for_a_name() {
        let err = UnnamedSpec {
            path: PathBuf::from("/SPEC.slcr.yml"),
            candidate: None,
        };
        assert_eq!(
            err.help().unwrap().to_string(),
            "the path does not suggest a name; pass one with `--name`, e.g. `--name todo-api`"
        );
    }
}
