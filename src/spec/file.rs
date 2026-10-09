use std::{ffi::OsStr, path::PathBuf};

use miette::{Diagnostic, Result};
use thiserror::Error;

use super::graph::SlcrRequirementsDocument;
use crate::fs::{File, FileSystem};

/// An SLCR requirements file: a specification graph stored at a
/// caller-chosen path. The path's extension selects the format: YAML
/// (`.yaml` or `.yml`), JSON (`.json`), or TOML (`.toml`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequirementsFile {
    path: PathBuf,
}

/// The error returned when a path's extension names no supported format.
#[derive(Debug, Error, Diagnostic)]
#[error("{} is not a YAML, JSON, or TOML file", .path.display())]
#[diagnostic(help("a requirements file's name must end in `.yaml`, `.yml`, `.json`, or `.toml`"))]
pub struct UnsupportedFormat {
    path: PathBuf,
}

impl RequirementsFile {
    /// The extensions [FileSystem] knows how to read and write.
    const EXTENSIONS: [&str; 4] = ["yaml", "yml", "json", "toml"];

    pub(crate) fn new(path: impl Into<PathBuf>) -> Result<Self, UnsupportedFormat> {
        let path = path.into();
        let supported = path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| Self::EXTENSIONS.contains(&extension));
        if supported {
            Ok(Self { path })
        } else {
            Err(UnsupportedFormat { path })
        }
    }
}

impl File for RequirementsFile {
    type Data = SlcrRequirementsDocument;

    fn extension(&self) -> &str {
        // `new` only accepts paths with a supported, UTF-8 extension.
        self.path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
    }

    /// The path as given, which may be relative to the working directory.
    /// Its parent directory is never created.
    fn path(&self, _fs: &FileSystem) -> Result<PathBuf> {
        Ok(self.path.clone())
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use figment::Jail;
    use serde_json::Value;

    use super::*;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");
    const TODO_API_TOML: &str = include_str!("../../tests/fixtures/todo-api.spec.toml");

    fn canon() -> SlcrRequirementsDocument {
        serde_json::from_str(TODO_API_JSON).unwrap()
    }

    /// Write `contents` to `name` in the jail and load it as a graph.
    fn load(jail: &mut Jail, name: &str, contents: &str) -> Result<SlcrRequirementsDocument> {
        jail.create_file(name, contents).unwrap();
        let fs = FileSystem::new().unwrap();
        fs.load_file(RequirementsFile::new(name).unwrap())
    }

    #[test]
    fn supported_extensions_are_accepted() {
        for (name, extension) in [
            ("spec.yaml", "yaml"),
            ("spec.yml", "yml"),
            ("spec.json", "json"),
            ("spec.toml", "toml"),
            ("todo-api.spec.json", "json"),
            ("nested/dir/spec.toml", "toml"),
        ] {
            let file = RequirementsFile::new(name).unwrap();
            assert_eq!(file.extension(), extension, "{name}");
        }
    }

    #[test]
    fn unsupported_extensions_are_rejected() {
        for name in [
            "spec",
            "spec.md",
            "spec.JSON",
            "spec.json.bak",
            ".json",
            "spec.",
        ] {
            let err = RequirementsFile::new(name).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("{name} is not a YAML, JSON, or TOML file")
            );
        }
    }

    #[test]
    fn the_path_is_returned_as_given() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            let file = RequirementsFile::new("specs/todo-api.yaml").unwrap();
            assert_eq!(
                file.path(&fs).unwrap(),
                PathBuf::from("specs/todo-api.yaml")
            );
            assert!(!std::path::Path::new("specs").exists());
            Ok(())
        });
    }

    #[test]
    fn json_files_load() {
        Jail::expect_with(|jail| {
            assert_eq!(load(jail, "todo-api.json", TODO_API_JSON).unwrap(), canon());
            Ok(())
        });
    }

    #[test]
    fn yaml_files_load() {
        Jail::expect_with(|jail| {
            assert_eq!(load(jail, "todo-api.yaml", TODO_API_YAML).unwrap(), canon());
            Ok(())
        });
    }

    #[test]
    fn yml_files_load() {
        Jail::expect_with(|jail| {
            assert_eq!(load(jail, "todo-api.yml", TODO_API_YAML).unwrap(), canon());
            Ok(())
        });
    }

    #[test]
    fn toml_files_load() {
        Jail::expect_with(|jail| {
            assert_eq!(load(jail, "todo-api.toml", TODO_API_TOML).unwrap(), canon());
            Ok(())
        });
    }

    #[test]
    fn every_format_round_trips() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            for name in ["out.json", "out.yaml", "out.yml", "out.toml"] {
                let file = RequirementsFile::new(name).unwrap();
                fs.save_file(&file, &canon()).unwrap();
                assert_eq!(fs.load_file(file).unwrap(), canon(), "{name}");
            }
            Ok(())
        });
    }

    #[test]
    fn saved_json_is_canonical() {
        Jail::expect_with(|jail| {
            let fs = FileSystem::new().unwrap();
            fs.save_file(&RequirementsFile::new("out.json").unwrap(), &canon())
                .unwrap();
            let saved = std::fs::read_to_string(jail.directory().join("out.json")).unwrap();
            let saved: Value = serde_json::from_str(&saved).unwrap();
            let expected: Value = serde_json::from_str(TODO_API_JSON).unwrap();
            assert_eq!(saved, expected);
            Ok(())
        });
    }

    #[test]
    fn invariant_violations_fail_to_load() {
        Jail::expect_with(|jail| {
            // REQ-009 refines REQ-008; break the reference.
            let broken = TODO_API_YAML.replace("refines: [REQ-008]", "refines: [REQ-099]");
            assert_ne!(broken, TODO_API_YAML);
            let err = load(jail, "broken.yaml", &broken).unwrap_err();
            assert!(
                err.to_string()
                    .contains("REQ-009 names REQ-099 in `refines`, but no such node exists"),
                "{err}"
            );
            Ok(())
        });
    }

    #[test]
    fn grammar_violations_fail_to_load() {
        Jail::expect_with(|jail| {
            let broken = TODO_API_TOML.replacen("modality = \"MUST\"", "modality = \"must\"", 1);
            assert_ne!(broken, TODO_API_TOML);
            assert!(load(jail, "broken.toml", &broken).is_err());
            Ok(())
        });
    }

    #[test]
    fn malformed_files_fail_to_load() {
        Jail::expect_with(|jail| {
            assert!(load(jail, "bad.json", "{ not json").is_err());
            assert!(load(jail, "bad.yaml", "root: [unclosed").is_err());
            assert!(load(jail, "bad.toml", "root = ").is_err());
            Ok(())
        });
    }

    #[test]
    fn missing_files_fail_to_load() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            assert!(
                fs.load_file(RequirementsFile::new("absent.yaml").unwrap())
                    .is_err()
            );
            Ok(())
        });
    }
}
