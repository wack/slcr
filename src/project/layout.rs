use std::path::{Path, PathBuf};

use super::ProjectFile;

/// Where a project is: its SLCR directory, which holds `SLCR.toml`, the
/// specifications, the validator files, and the descriptors kept as
/// `{ path }` sources, and its project root, the code its validators
/// check. Both are absolute.
///
/// The project root is the SLCR directory unless a caller names another,
/// as a program that keeps a repository's SLCR files in a directory of its
/// own does. Everything SLCR reads comes from the SLCR directory: `specs`
/// patterns and `{ path }` sources are relative to it, and discovery walks
/// it. The project root is only where validators run: it is
/// `{slcr.project_root}`, the `project` working directory, and the top of
/// a `nearest` search, and a validator file checks the directory at its
/// own path, relative to the SLCR directory, under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    slcr_dir: PathBuf,
    project_root: PathBuf,
}

impl Layout {
    /// The layout of a project whose SLCR directory is `slcr_dir` and
    /// whose project root is `project_root`, both absolute.
    pub(crate) fn new(slcr_dir: PathBuf, project_root: PathBuf) -> Self {
        Self {
            slcr_dir,
            project_root,
        }
    }

    /// The layout of a project whose project root is its SLCR directory,
    /// `dir`, an absolute path: where `SLCR.toml` sits beside the code.
    pub(crate) fn single(dir: PathBuf) -> Self {
        Self::new(dir.clone(), dir)
    }

    /// The directory that holds `SLCR.toml` and the project's SLCR files,
    /// as an absolute path.
    pub(crate) fn slcr_dir(&self) -> &Path {
        &self.slcr_dir
    }

    /// The directory the project's validators check, as an absolute path.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The directory that `file`, a validator file found in the SLCR
    /// directory, checks: the directory at its path relative to the SLCR
    /// directory, under the project root. It is `{slcr.validator_dir}`,
    /// the `validator-dir` working directory, and where a `nearest` search
    /// starts.
    pub(crate) fn validator_dir(&self, file: &ProjectFile) -> PathBuf {
        let relative = file.relative().parent().unwrap_or(Path::new(""));
        self.project_root.join(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator_file(slcr_dir: &str, relative: &str) -> ProjectFile {
        ProjectFile::new(Path::new(slcr_dir), Path::new(slcr_dir), relative.into())
    }

    #[test]
    fn a_single_directory_is_both_the_slcr_directory_and_the_project_root() {
        let layout = Layout::single(PathBuf::from("/repo"));
        assert_eq!(layout.slcr_dir(), Path::new("/repo"));
        assert_eq!(layout.project_root(), Path::new("/repo"));
        let file = validator_file("/repo", "src/api/.validators.slcr.toml");
        assert_eq!(layout.validator_dir(&file), Path::new("/repo/src/api"));
    }

    #[test]
    fn a_validator_file_checks_its_counterpart_under_the_project_root() {
        let layout = Layout::new(PathBuf::from("/data/app"), PathBuf::from("/repo"));
        assert_eq!(layout.slcr_dir(), Path::new("/data/app"));
        assert_eq!(layout.project_root(), Path::new("/repo"));
        let file = validator_file("/data/app", "src/api/.validators.slcr.toml");
        assert_eq!(
            file.absolute(),
            Path::new("/data/app/src/api/.validators.slcr.toml")
        );
        assert_eq!(layout.validator_dir(&file), Path::new("/repo/src/api"));
        // One at the top of the SLCR directory checks the project root.
        let top = validator_file("/data/app", ".validators.slcr.yaml");
        assert_eq!(layout.validator_dir(&top), Path::new("/repo"));
    }
}
