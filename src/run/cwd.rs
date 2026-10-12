use std::path::{Path, PathBuf};

use super::SpawnFailure;
use crate::project::relative_to;
use crate::tools::descriptor::Cwd;

/// The directory a validator whose file is in `validator_dir` runs in,
/// in the project rooted at `project_root`. Both are absolute.
pub(super) fn resolve(
    cwd: &Cwd,
    project_root: &Path,
    validator_dir: &Path,
) -> Result<PathBuf, SpawnFailure> {
    let dir = match cwd {
        Cwd::Project => project_root.to_path_buf(),
        Cwd::ValidatorDir => validator_dir.to_path_buf(),
        Cwd::Nearest(file) => nearest(file.as_str(), project_root, validator_dir)?,
    };
    if !dir.is_dir() {
        return Err(SpawnFailure::MissingDirectory {
            dir: relative_to(&dir, project_root),
        });
    }
    Ok(dir)
}

/// The closest of `validator_dir` and its ancestors, up to and including
/// `project_root`, with an entry named `file` in it.
fn nearest(file: &str, project_root: &Path, validator_dir: &Path) -> Result<PathBuf, SpawnFailure> {
    if !validator_dir.starts_with(project_root) {
        return Err(SpawnFailure::OutsideProject {
            dir: relative_to(validator_dir, project_root),
        });
    }
    validator_dir
        .ancestors()
        .take_while(|dir| dir.starts_with(project_root))
        .find(|dir| dir.join(file).exists())
        .map(Path::to_path_buf)
        .ok_or_else(|| SpawnFailure::NoNearest {
            file: file.to_owned(),
            start: relative_to(validator_dir, project_root),
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::tools::descriptor::FileName;

    /// A workspace whose project is `ws/project`, with `ws/Cargo.toml`
    /// above it and a validator directory at `ws/project/crates/api/specs`.
    struct Workspace {
        dir: TempDir,
    }

    impl Workspace {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            fs::create_dir_all(dir.path().join("project/crates/api/specs")).unwrap();
            fs::write(dir.path().join("Cargo.toml"), "").unwrap();
            Self { dir }
        }

        fn root(&self) -> PathBuf {
            self.dir.path().join("project")
        }

        fn validator_dir(&self) -> PathBuf {
            self.root().join("crates/api/specs")
        }

        fn touch(&self, path: &str) {
            fs::write(self.root().join(path), "").unwrap();
        }

        fn resolve(&self, cwd: &Cwd) -> Result<PathBuf, SpawnFailure> {
            resolve(cwd, &self.root(), &self.validator_dir())
        }
    }

    fn nearest(file: &str) -> Cwd {
        Cwd::Nearest(FileName::try_from(file.to_owned()).unwrap())
    }

    #[test]
    fn project_and_validator_dir_are_fixed_directories() {
        let ws = Workspace::new();
        assert_eq!(ws.resolve(&Cwd::Project).unwrap(), ws.root());
        assert_eq!(ws.resolve(&Cwd::ValidatorDir).unwrap(), ws.validator_dir());
    }

    #[test]
    fn nearest_starts_at_the_validator_dir() {
        let ws = Workspace::new();
        ws.touch("crates/api/specs/Cargo.toml");
        ws.touch("crates/api/Cargo.toml");
        assert_eq!(
            ws.resolve(&nearest("Cargo.toml")).unwrap(),
            ws.validator_dir()
        );
    }

    #[test]
    fn nearest_finds_the_closest_ancestor() {
        let ws = Workspace::new();
        ws.touch("crates/api/Cargo.toml");
        ws.touch("Cargo.toml");
        assert_eq!(
            ws.resolve(&nearest("Cargo.toml")).unwrap(),
            ws.root().join("crates/api")
        );
        // A directory of that name counts, as `.git` does.
        fs::create_dir(ws.root().join(".git")).unwrap();
        assert_eq!(ws.resolve(&nearest(".git")).unwrap(), ws.root());
    }

    #[test]
    fn nearest_stops_at_the_project_root() {
        let ws = Workspace::new();
        // `ws/Cargo.toml` is above the project root, so it isn't found.
        let err = ws.resolve(&nearest("Cargo.toml")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no directory from `crates/api/specs` up to the project root holds `Cargo.toml`"
        );
        ws.touch("Cargo.toml");
        assert_eq!(ws.resolve(&nearest("Cargo.toml")).unwrap(), ws.root());
    }

    #[test]
    fn nearest_needs_the_validator_file_in_the_project() {
        let ws = Workspace::new();
        let err = resolve(&nearest("Cargo.toml"), &ws.root(), ws.dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the validator file's directory, `..`, is outside the project root"
        );
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let ws = Workspace::new();
        let gone = ws.root().join("gone");
        let err = resolve(&Cwd::ValidatorDir, &ws.root(), &gone).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the working directory `gone` is not a directory"
        );
        let err = resolve(&nearest("Cargo.toml"), &ws.root(), &ws.root()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "not even the project root holds `Cargo.toml`"
        );
    }
}
