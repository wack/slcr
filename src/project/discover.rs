use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{DirEntry, WalkBuilder};

use super::{Problem, ProjectFile, relative_to};
use crate::spec::file::RequirementsFile;
use crate::tools::manifest::ProjectGlob;
use crate::validators::is_validator_file;

/// What searching the project found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Discovery {
    /// The specification files, each once, ordered by their path relative
    /// to the project root.
    pub(crate) files: Vec<ProjectFile>,
    /// Every validator file in the project, ordered the same way.
    pub(crate) validators: Vec<ProjectFile>,
    /// Every pattern that couldn't be honored, every match that isn't a
    /// specification file, and every directory with more than one
    /// validator file.
    pub(crate) problems: Vec<Problem>,
}

/// Expand `specs`, the manifest's patterns, in the project rooted at
/// `root`, and find its validator files, naming files relative to `cwd`.
/// Both paths are absolute.
///
/// A pattern is a glob relative to the root, in which `*` and `?` never
/// match a `/` but do match a leading dot, and `**` matches any number of
/// directories. The walk sees hidden files and directories, other than
/// `.git`, and skips what git ignores: in a git repository, the
/// `.gitignore` files of the project and of its parent directories up to
/// the repository's root, `.git/info/exclude`, and git's global excludes.
/// Outside a git repository, as for git, nothing is ignored.
///
/// A validator file is any file named `.validators.slcr.<ext>` that the
/// same walk sees, whether or not a pattern matches it.
///
/// Every problem is collected: a pattern that isn't a valid glob or
/// matches nothing, a match that is a validator file or has no
/// specification format's extension, a directory with two validator
/// files, and a directory that can't be read.
pub(crate) fn discover(root: &Path, cwd: &Path, specs: &[ProjectGlob]) -> Discovery {
    let mut problems = Vec::new();
    let (set, patterns) = compile(specs, &mut problems);

    // Each matched file, with the indices of the patterns that match it.
    let mut matches: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    let mut validators = Vec::new();
    for relative in walk(root, cwd, &mut problems) {
        let matched = set.matches(&relative);
        if is_validator_file(&relative) {
            validators.push(ProjectFile::new(root, cwd, relative.clone()));
        }
        if !matched.is_empty() {
            matches.insert(relative, matched);
        }
    }

    let mut files = Vec::new();
    let mut matched_any = vec![false; patterns.len()];
    for (relative, matched) in matches {
        for &index in &matched {
            matched_any[index] = true;
        }
        let file = ProjectFile::new(root, cwd, relative);
        let matched_by = || matched.iter().map(|&i| patterns[i].to_string()).collect();
        if is_validator_file(file.relative()) {
            problems.push(Problem::MatchesValidatorFile {
                patterns: matched_by(),
                path: file.shown,
            });
        } else if RequirementsFile::new(file.relative()).is_err() {
            problems.push(Problem::UnsupportedFormat {
                patterns: matched_by(),
                path: file.shown,
            });
        } else {
            files.push(file);
        }
    }
    for (pattern, matched) in patterns.iter().zip(matched_any) {
        if !matched {
            problems.push(Problem::NoMatch {
                pattern: pattern.to_string(),
            });
        }
    }
    validators.sort();
    problems.extend(crowded(&validators));
    Discovery {
        files,
        validators,
        problems,
    }
}

/// A directory holds at most one validator file: report each that shares
/// its directory with an earlier one.
fn crowded(validators: &[ProjectFile]) -> Vec<Problem> {
    let mut first_in: BTreeMap<&Path, &ProjectFile> = BTreeMap::new();
    let mut problems = Vec::new();
    for file in validators {
        let directory = file.relative().parent().unwrap_or(Path::new(""));
        match first_in.get(directory) {
            Some(first) => problems.push(Problem::DuplicateValidatorFile {
                first: first.shown().to_path_buf(),
                second: file.shown().to_path_buf(),
            }),
            None => {
                first_in.insert(directory, file);
            }
        }
    }
    problems
}

/// Every file under `root` that the walk sees, as a path relative to it,
/// in no particular order. A directory that can't be read is a problem.
///
/// The walk sees hidden files and directories, other than `.git`, and
/// skips what git ignores, as [discover] describes.
fn walk(root: &Path, cwd: &Path, problems: &mut Vec<Problem>) -> Vec<PathBuf> {
    let walk = WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(true)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    let mut files = Vec::new();
    for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                unsearchable(&err, root, cwd, problems);
                continue;
            }
        };
        if !is_file(&entry) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .expect("the walk stays under its root");
        files.push(relative.to_path_buf());
    }
    files
}

/// Compile every valid pattern into one set, and report each invalid one.
/// The valid patterns are returned in the set's order.
fn compile<'a>(
    specs: &'a [ProjectGlob],
    problems: &mut Vec<Problem>,
) -> (GlobSet, Vec<&'a ProjectGlob>) {
    let mut builder = GlobSetBuilder::new();
    let mut patterns = Vec::with_capacity(specs.len());
    for pattern in specs {
        match GlobBuilder::new(pattern.as_str())
            .literal_separator(true)
            .build()
        {
            Ok(glob) => {
                builder.add(glob);
                patterns.push(pattern);
            }
            Err(err) => problems.push(Problem::InvalidPattern {
                pattern: pattern.to_string(),
                reason: err.kind().to_string(),
            }),
        }
    }
    let set = builder
        .build()
        .expect("a set of globs that each compiled compiles");
    (set, patterns)
}

/// Whether `entry` is a file, or a symbolic link to one.
fn is_file(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|file_type| {
        file_type.is_file() || (file_type.is_symlink() && entry.path().is_file())
    })
}

/// Report a directory the walk couldn't read, which might have held a
/// match. Other errors, such as a `.gitignore` line that isn't a valid
/// glob, cost at most a rule, so they are logged and the walk goes on.
fn unsearchable(err: &ignore::Error, root: &Path, cwd: &Path, problems: &mut Vec<Problem>) {
    fn visit(err: &ignore::Error, path: &Path, cwd: &Path, problems: &mut Vec<Problem>) {
        match err {
            ignore::Error::Partial(errs) => {
                for err in errs {
                    visit(err, path, cwd, problems);
                }
            }
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
                visit(err, path, cwd, problems);
            }
            ignore::Error::WithPath { path, err } => visit(err, path, cwd, problems),
            ignore::Error::Io(io) => problems.push(Problem::Unsearchable {
                path: relative_to(path, cwd),
                reason: io.to_string(),
            }),
            other => tracing::warn!(
                path = %relative_to(path, cwd).display(),
                "ignoring a problem while searching for specification files: {other}"
            ),
        }
    }

    visit(err, root, cwd, problems);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// A project directory holding `files`, each an empty file unless its
    /// contents follow a `=`.
    fn project(files: &[&str]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let (path, contents) = file.split_once('=').unwrap_or((file, ""));
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        dir
    }

    fn globs(patterns: &[&str]) -> Vec<ProjectGlob> {
        patterns
            .iter()
            .map(|pattern| ProjectGlob::try_from((*pattern).to_owned()).unwrap())
            .collect()
    }

    /// Discover `patterns` in `dir`, from its root.
    fn run(dir: &TempDir, patterns: &[&str]) -> Discovery {
        discover(dir.path(), dir.path(), &globs(patterns))
    }

    /// The paths of the files discovered, as written relative to the root.
    fn found(discovery: &Discovery) -> Vec<String> {
        discovery
            .files
            .iter()
            .map(|file| file.relative().display().to_string())
            .collect()
    }

    fn messages(discovery: &Discovery) -> Vec<String> {
        discovery.problems.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn matches_are_deduplicated_and_sorted_by_path() {
        let dir = project(&[
            "SPEC.slcr.yml",
            "docs/b.json",
            "docs/a/z.toml",
            "docs/a.yaml",
            "docs/a-b.yaml",
            "other/c.yaml",
        ]);
        let discovery = run(
            &dir,
            &["docs/**/*.yaml", "**/*.json", "docs/**", "SPEC.slcr.yml"],
        );
        assert_eq!(discovery.problems, []);
        assert_eq!(
            found(&discovery),
            [
                "SPEC.slcr.yml",
                "docs/a/z.toml",
                "docs/a-b.yaml",
                "docs/a.yaml",
                "docs/b.json",
            ]
        );
    }

    #[test]
    fn a_star_never_crosses_a_slash() {
        let dir = project(&["docs/a.yaml", "docs/api/b.yaml"]);
        assert_eq!(found(&run(&dir, &["docs/*.yaml"])), ["docs/a.yaml"]);
        assert_eq!(
            found(&run(&dir, &["docs/**/*.yaml"])),
            ["docs/a.yaml", "docs/api/b.yaml"]
        );
        // Nor does a question mark.
        let discovery = run(&dir, &["docs?a.yaml"]);
        assert_eq!(
            messages(&discovery),
            ["`specs` pattern `docs?a.yaml` matches no file"]
        );
    }

    #[test]
    fn a_pattern_that_matches_nothing_is_an_error() {
        let dir = project(&["SPEC.slcr.yml"]);
        let discovery = run(&dir, &["SPEC.slcr.yml", "SPEC.slcr.yaml", "docs/**/*.toml"]);
        assert_eq!(found(&discovery), ["SPEC.slcr.yml"]);
        assert_eq!(
            discovery.problems,
            [
                Problem::NoMatch {
                    pattern: "SPEC.slcr.yaml".to_owned()
                },
                Problem::NoMatch {
                    pattern: "docs/**/*.toml".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_pattern_never_matches_a_directory() {
        let dir = project(&["docs/a.yaml"]);
        let discovery = run(&dir, &["docs"]);
        assert_eq!(found(&discovery), [] as [&str; 0]);
        assert_eq!(
            messages(&discovery),
            ["`specs` pattern `docs` matches no file"]
        );
    }

    #[test]
    fn a_pattern_that_matches_a_validator_file_is_an_error() {
        let dir = project(&[
            "docs/api/.validators.slcr.toml",
            "docs/api/api.slcr.toml",
            "docs/.validators.slcr.yml",
        ]);
        let discovery = run(&dir, &["docs/**/*.slcr.toml", "docs/**"]);
        assert_eq!(found(&discovery), ["docs/api/api.slcr.toml"]);
        assert_eq!(
            messages(&discovery),
            [
                "`specs` pattern `docs/**` matches the validator file docs/.validators.slcr.yml",
                "`specs` patterns `docs/**/*.slcr.toml` and `docs/**` match the validator file \
                 docs/api/.validators.slcr.toml",
            ]
        );
        // A pattern can leave validator files out.
        let discovery = run(&dir, &["docs/**/[!.]*.slcr.toml"]);
        assert_eq!(discovery.problems, []);
        assert_eq!(found(&discovery), ["docs/api/api.slcr.toml"]);
    }

    /// The validator files discovered, as written relative to the root.
    fn validators(discovery: &Discovery) -> Vec<String> {
        discovery
            .validators
            .iter()
            .map(|file| file.relative().display().to_string())
            .collect()
    }

    #[test]
    fn validator_files_are_found_whatever_the_patterns_match() {
        let dir = project(&[
            "SPEC.slcr.yml",
            "src/views/items/.validators.slcr.toml",
            "src/.validators.slcr.yml",
            ".validators.slcr.json",
            "src/views/.validators.slcr.yaml",
            "src/views/validators.slcr.toml",
            "src/.hidden/.validators.slcr.toml",
        ]);
        let discovery = run(&dir, &["SPEC.slcr.yml"]);
        assert_eq!(discovery.problems, []);
        assert_eq!(found(&discovery), ["SPEC.slcr.yml"]);
        assert_eq!(
            validators(&discovery),
            [
                ".validators.slcr.json",
                "src/.hidden/.validators.slcr.toml",
                "src/.validators.slcr.yml",
                "src/views/.validators.slcr.yaml",
                "src/views/items/.validators.slcr.toml",
            ]
        );
    }

    #[test]
    fn validator_files_follow_the_same_walk_rules() {
        let dir = project(&[
            "SPEC.slcr.yml",
            ".git/HEAD",
            ".git/.validators.slcr.toml",
            ".gitignore=target/\n",
            "target/.validators.slcr.toml",
            "src/.validators.slcr.toml",
        ]);
        let discovery = run(&dir, &["SPEC.slcr.yml"]);
        assert_eq!(validators(&discovery), ["src/.validators.slcr.toml"]);
    }

    #[test]
    fn a_directory_holds_at_most_one_validator_file() {
        let dir = project(&[
            "SPEC.slcr.yml",
            "src/.validators.slcr.yml",
            "src/.validators.slcr.toml",
            "src/.validators.slcr.json",
            "src/api/.validators.slcr.json",
        ]);
        let cwd = dir.path().join("src");
        let discovery = discover(dir.path(), &cwd, &globs(&["SPEC.slcr.yml"]));
        // Every one is still vetted.
        assert_eq!(validators(&discovery).len(), 4);
        assert_eq!(
            messages(&discovery),
            [
                ".validators.slcr.json and .validators.slcr.toml are validator files in the \
                 same directory",
                ".validators.slcr.json and .validators.slcr.yml are validator files in the \
                 same directory",
            ]
        );
    }

    #[test]
    fn a_match_must_have_a_specification_format() {
        let dir = project(&["docs/a.yaml", "docs/notes.md", "docs/Makefile"]);
        let discovery = run(&dir, &["docs/*", "docs/*.md"]);
        assert_eq!(found(&discovery), ["docs/a.yaml"]);
        assert_eq!(
            messages(&discovery),
            [
                "`specs` pattern `docs/*` matches docs/Makefile, which is not a YAML, JSON, or \
                 TOML file",
                "`specs` patterns `docs/*` and `docs/*.md` match docs/notes.md, which is not a \
                 YAML, JSON, or TOML file",
            ]
        );
    }

    #[test]
    fn an_invalid_pattern_is_an_error_and_the_rest_still_match() {
        let dir = project(&["a.yaml"]);
        let discovery = run(&dir, &["docs/[a", "*.yaml"]);
        assert_eq!(found(&discovery), ["a.yaml"]);
        assert_eq!(
            discovery.problems,
            [Problem::InvalidPattern {
                pattern: "docs/[a".to_owned(),
                reason: "unclosed character class; missing ']'".to_owned(),
            }]
        );
    }

    #[test]
    fn gitignore_is_respected_in_a_git_repository() {
        let dir = project(&[
            ".git/HEAD",
            ".gitignore=generated/\n*.draft.yaml\n",
            "a.yaml",
            "b.draft.yaml",
            "generated/c.yaml",
            "docs/.gitignore=/local.yaml\n",
            "docs/local.yaml",
            "docs/kept.yaml",
        ]);
        let discovery = run(&dir, &["**/*.yaml", "generated/*.yaml"]);
        assert_eq!(found(&discovery), ["a.yaml", "docs/kept.yaml"]);
        assert_eq!(
            messages(&discovery),
            ["`specs` pattern `generated/*.yaml` matches no file"]
        );
    }

    #[test]
    fn nothing_is_ignored_outside_a_git_repository() {
        let dir = project(&[".gitignore=*.draft.yaml\n", "a.yaml", "b.draft.yaml"]);
        assert_eq!(found(&run(&dir, &["*.yaml"])), ["a.yaml", "b.draft.yaml"]);
    }

    #[test]
    fn gitignore_files_apply_from_the_repository_root_down() {
        let dir = project(&[
            // Outside the repository, so it doesn't apply.
            ".gitignore=*.yaml\n",
            "repo/.git/HEAD",
            "repo/.git/info/exclude=excluded.yaml\n",
            "repo/.gitignore=*.draft.yaml\n",
            "repo/project/a.yaml",
            "repo/project/b.draft.yaml",
            "repo/project/excluded.yaml",
        ]);
        let root = dir.path().join("repo/project");
        let discovery = discover(&root, &root, &globs(&["*.yaml"]));
        assert_eq!(found(&discovery), ["a.yaml"]);
    }

    #[test]
    fn hidden_files_and_directories_are_walked() {
        let dir = project(&[".specs/a.yaml", ".b.yaml", ".git/c.yaml"]);
        let discovery = run(&dir, &["**/*.yaml"]);
        assert_eq!(found(&discovery), [".b.yaml", ".specs/a.yaml"]);
        // But never `.git`.
        let discovery = run(&dir, &[".git/*.yaml"]);
        assert_eq!(
            messages(&discovery),
            ["`specs` pattern `.git/*.yaml` matches no file"]
        );
    }

    #[test]
    fn files_are_named_relative_to_the_working_directory() {
        let dir = project(&["SPEC.slcr.yml", "docs/a.yaml", "docs/.validators.slcr.toml"]);
        let cwd = dir.path().join("docs");
        let discovery = discover(dir.path(), &cwd, &globs(&["**"]));
        let shown: Vec<_> = discovery.files.iter().map(ProjectFile::shown).collect();
        assert_eq!(shown, [Path::new("../SPEC.slcr.yml"), Path::new("a.yaml")]);
        assert_eq!(
            messages(&discovery),
            ["`specs` pattern `**` matches the validator file .validators.slcr.toml"]
        );
    }

    #[test]
    fn the_order_is_the_same_on_every_run() {
        let names: Vec<String> = (0..50).map(|i| format!("d{}/s{i}.yaml", i % 7)).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let dir = project(&names);
        let first = found(&run(&dir, &["**/*.yaml"]));
        let mut sorted = first.clone();
        sorted.sort_by(|a, b| Path::new(a).cmp(Path::new(b)));
        assert_eq!(first, sorted);
        assert_eq!(first.len(), 50);
        for _ in 0..3 {
            assert_eq!(found(&run(&dir, &["**/*.yaml"])), first);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_file_is_matched() {
        let dir = project(&["real/a.yaml"]);
        std::os::unix::fs::symlink(dir.path().join("real/a.yaml"), dir.path().join("b.yaml"))
            .unwrap();
        assert_eq!(found(&run(&dir, &["*.yaml"])), ["b.yaml"]);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = project(&["a.yaml", "private/b.yaml"]);
        let private = dir.path().join("private");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&private).is_ok();
        let discovery = run(&dir, &["**/*.yaml"]);
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        // Permissions don't bind a privileged user, such as root in a
        // container.
        if readable {
            return;
        }
        assert_eq!(found(&discovery), ["a.yaml"]);
        assert_eq!(discovery.problems.len(), 1, "{:?}", discovery.problems);
        let Problem::Unsearchable { path, .. } = &discovery.problems[0] else {
            panic!("{:?}", discovery.problems);
        };
        assert_eq!(path, Path::new("private"));
    }
}
