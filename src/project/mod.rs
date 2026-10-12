//! An SLCR project's files: the specifications its manifest's `specs`
//! patterns match, loaded and indexed by name, so that a qualified
//! reference such as `todo-api/REQ-002` resolves to its requirement, and
//! the validator files beside the code.
//!
//! The rules are those of the manifest schema on the Notion "Schemas"
//! page and of "SLCR — Tools: Manifest, Descriptors, and Invocation".

use std::path::{Component, Path, PathBuf};

use miette::Result;

use crate::fs::FileSystem;
use crate::spec::file::RequirementsFile;
use crate::spec::graph::UnvettedDocument;

pub(crate) use discover::discover;
pub(crate) use index::{IndexedSpec, SpecIndex};
pub(crate) use problem::Problem;
pub(crate) use qualified::QualifiedRequirementId;

/// Expanding the `specs` patterns into the project's specification files.
pub(crate) mod discover;
/// The project's specifications by name, and resolving qualified IDs.
pub(crate) mod index;
/// What can be wrong with the project as a whole.
pub(crate) mod problem;
/// `<spec-name>/REQ-<n>`: a requirement named from outside its spec.
pub(crate) mod qualified;

/// A file of a project, found by [discover]: a specification or a
/// validator file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProjectFile {
    /// Relative to the project root, which orders the project's files.
    relative: PathBuf,
    /// Relative to the working directory: how messages name the file.
    shown: PathBuf,
    absolute: PathBuf,
}

impl ProjectFile {
    /// The file at `relative` in the project rooted at `root`, named
    /// relative to `cwd`. Both are absolute.
    pub(crate) fn new(root: &Path, cwd: &Path, relative: PathBuf) -> Self {
        let absolute = root.join(&relative);
        Self {
            shown: relative_to(&absolute, cwd),
            relative,
            absolute,
        }
    }

    /// The path relative to the project root.
    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }

    /// The path relative to the working directory, as the user would type
    /// it.
    pub(crate) fn shown(&self) -> &Path {
        &self.shown
    }

    /// The absolute path.
    pub(crate) fn absolute(&self) -> &Path {
        &self.absolute
    }

    /// Read the specification in the file without vetting it. Errors name
    /// the file by its [ProjectFile::shown] path.
    pub(crate) fn load_spec(&self, fs: &FileSystem) -> Result<UnvettedDocument> {
        let file = RequirementsFile::new(&self.shown)?.unvetted();
        fs.load_file_at(file, &self.absolute)
    }
}

/// `path` relative to `base`, both absolute, such as `../docs/a.yaml`. If
/// they share no root, as on different Windows drives, `path` itself.
pub(crate) fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let mut path_parts = path.components().peekable();
    let mut base_parts = base.components().peekable();
    if path_parts.peek() != base_parts.peek() {
        return path.to_path_buf();
    }
    while let (Some(left), Some(right)) = (path_parts.peek(), base_parts.peek()) {
        if left != right {
            break;
        }
        path_parts.next();
        base_parts.next();
    }
    let mut relative = PathBuf::new();
    for _ in base_parts {
        relative.push(Component::ParentDir);
    }
    relative.extend(path_parts);
    relative
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_made_relative_to_a_base() {
        let cases = [
            ("/p/docs/a.yaml", "/p", "docs/a.yaml"),
            ("/p/docs/a.yaml", "/p/docs", "a.yaml"),
            ("/p/a.yaml", "/p/docs", "../a.yaml"),
            ("/p/x/y/a.yaml", "/p/docs/api", "../../x/y/a.yaml"),
            ("/p/SLCR.toml", "/p/a/b", "../../SLCR.toml"),
            // Components compare whole, never as prefixes of each other.
            ("/p/docs2/a.yaml", "/p/docs", "../docs2/a.yaml"),
        ];
        for (path, base, relative) in cases {
            assert_eq!(
                relative_to(Path::new(path), Path::new(base)),
                PathBuf::from(relative),
                "{path} from {base}"
            );
        }
    }

    #[test]
    fn a_spec_file_is_named_relative_to_the_working_directory() {
        let file = ProjectFile::new(
            Path::new("/p"),
            Path::new("/p/docs"),
            PathBuf::from("specs/a.yaml"),
        );
        assert_eq!(file.relative(), Path::new("specs/a.yaml"));
        assert_eq!(file.shown(), Path::new("../specs/a.yaml"));
        assert_eq!(file.absolute, Path::new("/p/specs/a.yaml"));
    }
}
