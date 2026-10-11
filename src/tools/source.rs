use std::{
    fmt::{self, Display},
    path::{Component, Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use miette::Diagnostic;
use serde::Deserialize;
use thiserror::Error;

use crate::fs::{FileSystem, ReadError};

/// How long fetching a descriptor over HTTPS may take, all told.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Where SLCR fetches a manifest entry's descriptor from: `{ url = "..." }`
/// for a published descriptor, or `{ path = "..." }` for one kept in the
/// project.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawSource")]
pub(crate) enum Source {
    Url(HttpsUrl),
    Path(ProjectPath),
}

/// A source as written: a table with exactly one of its two fields.
/// Deserializing through this rather than an untagged enum keeps the
/// location of an error inside the table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    url: Option<HttpsUrl>,
    path: Option<ProjectPath>,
}

impl TryFrom<RawSource> for Source {
    type Error = &'static str;

    fn try_from(raw: RawSource) -> Result<Self, Self::Error> {
        match (raw.url, raw.path) {
            (Some(url), None) => Ok(Self::Url(url)),
            (None, Some(path)) => Ok(Self::Path(path)),
            _ => Err("a source has exactly one of `url` and `path`"),
        }
    }
}

impl Source {
    /// The source's single field, as `cargo add` writes it: the name of the
    /// field and its value.
    pub(crate) fn field(&self) -> (&'static str, &str) {
        match self {
            Self::Url(url) => ("url", url.as_str()),
            Self::Path(path) => ("path", path.as_str()),
        }
    }

    /// Fetch the descriptor's bytes: over HTTPS, or from the file at the
    /// path, which must be inside the project at `root`.
    pub(crate) fn fetch(&self, fs: &FileSystem, root: &Path) -> miette::Result<Vec<u8>> {
        match self {
            Self::Url(url) => Ok(url.fetch()?),
            Self::Path(path) => {
                let file = path.resolve(root)?;
                Ok(read_descriptor(fs, &file, Path::new(path.as_str()))?)
            }
        }
    }
}

impl Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => url.fmt(f),
            Self::Path(path) => path.fmt(f),
        }
    }
}

/// The bytes of the descriptor at `file`, reported as `shown` when it
/// can't be read.
fn read_descriptor(fs: &FileSystem, file: &Path, shown: &Path) -> Result<Vec<u8>, SourceError> {
    match fs.read_bytes(file) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(SourceError::Missing {
            path: shown.to_path_buf(),
        }),
        Err(err) => Err(SourceError::Unreadable(err.relabeled(shown))),
    }
}

/// An HTTPS URL, from which a published descriptor is fetched.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct HttpsUrl(String);

impl HttpsUrl {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The bytes the URL serves. Only HTTPS is used, even across
    /// redirects, and any status but success is an error.
    fn fetch(&self) -> Result<Vec<u8>, SourceError> {
        let fail = |source| SourceError::Fetch {
            url: self.0.clone(),
            source: Box::new(source),
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(FETCH_TIMEOUT))
            .build()
            .into();
        let mut response = agent.get(&self.0).call().map_err(fail)?;
        response.body_mut().read_to_vec().map_err(fail)
    }
}

impl TryFrom<String> for HttpsUrl {
    type Error = SourceError;

    /// Mirrors the schema's pattern, `^https://[^\s]+$`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(char::is_whitespace));
        if valid {
            Ok(Self(value))
        } else {
            Err(SourceError::NotHttps { url: value })
        }
    }
}

impl Display for HttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A path relative to the project root, with forward slashes, to a
/// descriptor kept in the project.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct ProjectPath(String);

impl ProjectPath {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The file this path names in the project at `root`, which it must
    /// still be inside after following symlinks.
    fn resolve(&self, root: &Path) -> Result<PathBuf, SourceError> {
        let shown = Path::new(&self.0);
        let path = root.join(shown);
        let resolved = canonical(&path, shown)?;
        let root = canonical(root, Path::new("."))?;
        if resolved.starts_with(&root) {
            Ok(resolved)
        } else {
            Err(SourceError::OutsideProject {
                path: shown.to_path_buf(),
            })
        }
    }

    /// The project path of the local file at `path`, which is relative to
    /// the working directory `cwd` and must be inside the project at
    /// `root`, and the file it resolves to.
    fn locate(path: &Path, cwd: &Path, root: &Path) -> Result<(Self, PathBuf), SourceError> {
        let resolved = canonical(&cwd.join(path), path)?;
        let root = canonical(root, Path::new("."))?;
        let outside = || SourceError::OutsideProject {
            path: path.to_path_buf(),
        };
        let relative = resolved.strip_prefix(&root).map_err(|_| outside())?;
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => {
                    parts.push(part.to_str().ok_or_else(|| SourceError::Unrepresentable {
                        path: path.to_path_buf(),
                    })?)
                }
                // A canonical path relative to its ancestor has only
                // normal components.
                _ => return Err(outside()),
            }
        }
        let project_path =
            Self::try_from(parts.join("/")).map_err(|_| SourceError::Unrepresentable {
                path: path.to_path_buf(),
            })?;
        Ok((project_path, resolved))
    }
}

/// `path` with every symlink followed, reported as `shown` if it can't be.
fn canonical(path: &Path, shown: &Path) -> Result<PathBuf, SourceError> {
    std::fs::canonicalize(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            SourceError::Missing {
                path: shown.to_path_buf(),
            }
        } else {
            SourceError::Unreadable(ReadError::new(shown.to_path_buf(), source))
        }
    })
}

impl TryFrom<String> for ProjectPath {
    type Error = SourceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_project_relative(&value) {
            Ok(Self(value))
        } else {
            Err(SourceError::NotProjectRelative { path: value })
        }
    }
}

impl Display for ProjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `value` is relative to the project root, as the schemas'
/// paths and globs are: forward slashes, no leading `/`, no `..` segment,
/// and no backslash or NUL.
pub(crate) fn is_project_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', '\0'])
        && !value.split('/').any(|segment| segment == "..")
}

/// A source as given on the command line: an HTTPS URL, or a path to a
/// local file relative to the working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceArg {
    Url(HttpsUrl),
    Path(PathBuf),
}

impl FromStr for SourceArg {
    type Err = SourceError;

    /// Anything with a scheme is a URL, which must be HTTPS; anything else
    /// is a path.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.contains("://") {
            HttpsUrl::try_from(value.to_owned()).map(Self::Url)
        } else {
            Ok(Self::Path(PathBuf::from(value)))
        }
    }
}

impl SourceArg {
    /// The manifest source this names in the project at `root`, and the
    /// descriptor's bytes.
    pub(crate) fn fetch(
        self,
        fs: &FileSystem,
        cwd: &Path,
        root: &Path,
    ) -> Result<(Source, Vec<u8>), SourceError> {
        match self {
            Self::Url(url) => {
                let bytes = url.fetch()?;
                Ok((Source::Url(url), bytes))
            }
            Self::Path(path) => {
                let (project_path, file) = ProjectPath::locate(&path, cwd, root)?;
                let bytes = read_descriptor(fs, &file, &path)?;
                Ok((Source::Path(project_path), bytes))
            }
        }
    }
}

/// The ways a source can fail to yield a descriptor.
#[derive(Debug, Error, Diagnostic)]
pub enum SourceError {
    #[error("{url} is not an HTTPS URL")]
    #[diagnostic(help(
        "descriptors are fetched only over HTTPS; pass an `https://` URL, or a path to a file in the project"
    ))]
    NotHttps { url: String },

    #[error("{path:?} is not a path relative to the project root")]
    #[diagnostic(help(
        "a path source uses forward slashes, with no leading `/` and no `..` segment"
    ))]
    NotProjectRelative { path: String },

    #[error("{} does not exist", .path.display())]
    Missing { path: PathBuf },

    #[error(transparent)]
    #[diagnostic(transparent)]
    Unreadable(ReadError),

    #[error("{} is outside the project", .path.display())]
    #[diagnostic(help(
        "a descriptor kept in the project must be inside the directory that holds SLCR.toml; \
         copy it there, or publish it at an HTTPS URL"
    ))]
    OutsideProject { path: PathBuf },

    #[error("{} can't be written as a project path", .path.display())]
    #[diagnostic(help("a path source must be UTF-8, without backslashes"))]
    Unrepresentable { path: PathBuf },

    #[error("could not fetch {url}")]
    Fetch {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_arg_with_a_scheme_is_a_url() {
        assert_eq!(
            "https://example.com/d.toml".parse::<SourceArg>().unwrap(),
            SourceArg::Url(HttpsUrl("https://example.com/d.toml".to_owned()))
        );
        assert_eq!(
            "tools/d.toml".parse::<SourceArg>().unwrap(),
            SourceArg::Path(PathBuf::from("tools/d.toml"))
        );
        for insecure in ["http://example.com/d.toml", "file:///d.toml", "https://"] {
            assert!(
                matches!(
                    insecure.parse::<SourceArg>(),
                    Err(SourceError::NotHttps { .. })
                ),
                "{insecure}"
            );
        }
    }

    #[test]
    fn project_paths_stay_inside_the_project() {
        for valid in [
            "d.toml",
            "tools/descriptors/d.toml",
            "./d.toml",
            "a..b/d.toml",
        ] {
            assert!(ProjectPath::try_from(valid.to_owned()).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "/d.toml",
            "../d.toml",
            "a/../d.toml",
            "a/..",
            "a\\d.toml",
        ] {
            assert!(
                ProjectPath::try_from(invalid.to_owned()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn a_source_deserializes_from_a_table_with_one_field() {
        let url: Source = toml::from_str("url = \"https://example.com/d.toml\"").unwrap();
        assert_eq!(url.field(), ("url", "https://example.com/d.toml"));
        let path: Source = toml::from_str("path = \"tools/d.toml\"").unwrap();
        assert_eq!(path.field(), ("path", "tools/d.toml"));
        for invalid in [
            "",
            "url = \"https://example.com/d.toml\"\npath = \"tools/d.toml\"",
            "git = \"https://example.com/d.git\"",
            "url = \"http://example.com/d.toml\"",
            "path = \"../d.toml\"",
        ] {
            assert!(toml::from_str::<Source>(invalid).is_err(), "{invalid}");
        }
    }
}
