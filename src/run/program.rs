//! slcr finds programs itself rather than leaving it to
//! `std::process::Command`, whose search differs by platform (on Windows it
//! looks in the current directory and slcr's own directory first), so that
//! what runs is exactly what the rules allow: an executable file in an
//! absolute directory on the `PATH` the child will see.

use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use super::SpawnFailure;
use crate::tools::descriptor::ProgramName;

/// Whether `name` is the variable the search path is read from.
pub(super) fn is_path_variable(name: &str) -> bool {
    if cfg!(windows) {
        // Windows environment variable names ignore case.
        name.eq_ignore_ascii_case("PATH")
    } else {
        name == "PATH"
    }
}

/// The absolute path of the first executable file named `program` in the
/// directories of `path`, a `PATH` value. An empty or relative entry is
/// skipped, never read as the current directory.
pub(super) fn find(program: &ProgramName, path: Option<&OsStr>) -> Result<PathBuf, SpawnFailure> {
    let not_found = || SpawnFailure::ProgramNotFound {
        program: program.as_str().to_owned(),
    };
    let path = path.ok_or_else(not_found)?;
    env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| candidates(&dir, program.as_str()))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(not_found)
}

/// The files that could be `program` in `dir`.
#[cfg(not(windows))]
fn candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    vec![dir.join(program)]
}

/// The files that could be `program` in `dir`: the name itself, then the
/// name with each of `PATHEXT`'s extensions. Batch files are left out:
/// Windows runs them through `cmd.exe`, and validators never run through a
/// shell.
#[cfg(windows)]
fn candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    let extensions = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE".to_owned());
    let mut candidates = vec![dir.join(program)];
    candidates.extend(
        extensions
            .split(';')
            .filter(|ext| !ext.is_empty())
            .filter(|ext| !ext.eq_ignore_ascii_case(".bat") && !ext.eq_ignore_ascii_case(".cmd"))
            .map(|ext| dir.join(format!("{program}{ext}"))),
    );
    candidates
}

/// Whether `path`, following symlinks, is a regular file with an execute
/// permission bit set.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether `path`, following symlinks, is an executable file: on Windows,
/// a file named with an extension that can run directly.
#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    let runnable = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"));
    runnable && path.is_file()
}

#[cfg(all(test, unix))]
pub(super) mod tests {
    use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;

    use super::*;
    use crate::run::SpawnFailure;

    /// Write an executable script named `name` in `dir`.
    pub(crate) fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn program(name: &str) -> ProgramName {
        ProgramName::try_from(name.to_owned()).unwrap()
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        env::join_paths(dirs).unwrap()
    }

    #[test]
    fn the_first_executable_on_path_is_found() {
        let (first, second) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let expected = executable(second.path(), "tool");
        let path = path_of(&[first.path(), second.path()]);
        assert_eq!(find(&program("tool"), Some(&path)).unwrap(), expected);

        let earlier = executable(first.path(), "tool");
        assert_eq!(find(&program("tool"), Some(&path)).unwrap(), earlier);
    }

    #[test]
    fn non_executables_are_skipped() {
        let (first, second) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        // Not executable, a directory, and a dangling symlink.
        fs::write(first.path().join("tool"), "#!/bin/sh\n").unwrap();
        let path = path_of(&[first.path(), second.path()]);
        assert!(matches!(
            find(&program("tool"), Some(&path)),
            Err(SpawnFailure::ProgramNotFound { .. })
        ));
        fs::remove_file(first.path().join("tool")).unwrap();
        fs::create_dir(first.path().join("tool")).unwrap();
        std::os::unix::fs::symlink(second.path().join("missing"), second.path().join("tool"))
            .unwrap();
        assert!(find(&program("tool"), Some(&path)).is_err());

        // A symlink to an executable is followed.
        let third = TempDir::new().unwrap();
        let target = executable(third.path(), "real");
        std::os::unix::fs::symlink(&target, third.path().join("tool")).unwrap();
        let path = path_of(&[first.path(), third.path()]);
        assert_eq!(
            find(&program("tool"), Some(&path)).unwrap(),
            third.path().join("tool")
        );
    }

    // `Jail`'s error type is figment's, which is large.
    #[allow(clippy::result_large_err)]
    #[test]
    fn empty_and_relative_entries_and_the_current_directory_are_never_searched() {
        figment::Jail::expect_with(|jail| {
            let cwd = jail.directory().to_path_buf();
            executable(&cwd, "tool");
            fs::create_dir(cwd.join("bin")).unwrap();
            executable(&cwd.join("bin"), "tool");
            for path in ["", ".", "bin", "./bin", ":bin:", "::"] {
                assert!(
                    find(&program("tool"), Some(OsStr::new(path))).is_err(),
                    "{path:?}"
                );
            }
            assert!(find(&program("tool"), None).is_err());
            // The same directories, named absolutely, are searched.
            let path = path_of(&[&cwd.join("bin")]);
            assert_eq!(
                find(&program("tool"), Some(&path)).unwrap(),
                cwd.join("bin/tool")
            );
            Ok(())
        });
    }

    #[test]
    fn only_path_names_the_variable() {
        assert!(is_path_variable("PATH"));
        assert!(!is_path_variable("Path"));
        assert!(!is_path_variable("MANPATH"));
    }
}
