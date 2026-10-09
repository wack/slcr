use directories::ProjectDirs;
use miette::{Diagnostic, IntoDiagnostic, Result, miette};
use thiserror::Error;

use std::{io::Write, path::PathBuf};

pub(crate) use file::File;
// Re-exported so modules outside of `fs` can declare their own `StaticFile`
// marker types and reuse the loader.
pub(crate) use file::StaticFile;

use format::Format;

mod file;
pub(crate) mod format;

/// The name of the application as used on the filesystem for XDG conventions.
const APPLICATION_NAME: &str = "slcr";

/// An abstraction over the user's filesystem ensuring mediated
/// access to the most commonly used files.
#[derive(Clone)]
pub struct FileSystem {
    /// OS-specific file locations for standard operations,
    /// respecting $XDG_CONFIG and similar variables, and falling
    /// back to OS defaults.
    xdg_dirs: ProjectDirs,
}

#[derive(Debug, Error, Diagnostic)]
#[error("$HOME directory unavailable")]
pub struct MissingHomeDirectory;

/// The error returned when a file can't be read.
#[derive(Debug, Error, Diagnostic)]
#[error("could not read {}", .path.display())]
pub struct ReadError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

/// The format a file's extension names.
fn format_of<F: File>(file: &F) -> Result<Format> {
    Format::from_extension(file.extension()).ok_or_else(|| {
        miette!("Extension unknown! Internal error. Please file this error as a bug.")
    })
}

impl FileSystem {
    pub fn new() -> Result<Self, MissingHomeDirectory> {
        let dirs = ProjectDirs::from("", "", APPLICATION_NAME);
        let xdg_dirs = dirs.ok_or(MissingHomeDirectory)?;
        Ok(Self { xdg_dirs })
    }

    /// Returns `Ok(true)` if the file existed and was deleted.
    /// Returns `Ok(false)`` if the file did not exist.
    /// Returns `Err(_)`` if the file could not be deleted or there was another io error.
    pub(crate) fn delete_file<T: StaticFile>(&self) -> Result<bool> {
        // • Grab the path to the file.
        let path = T::static_path(self)?;
        // Remove the file but check the error.
        match std::fs::remove_file(path) {
            Ok(_) => Ok(true),
            Err(ref err) => match err.kind() {
                std::io::ErrorKind::NotFound => Ok(false),
                _ => Err(miette!("{}", err)),
            },
        }
    }

    /// Open the file and deserialize it with serde, in the format its
    /// extension names.
    pub(crate) fn load_file<F: File>(&self, file: F) -> Result<F::Data> {
        let format = format_of(&file)?;
        let path = file.path(self)?;
        let source = std::fs::read_to_string(&path).map_err(|source| ReadError {
            path: path.clone(),
            source,
        })?;
        Ok(format.parse(&path, source)?)
    }

    /// Store the file, using its canonical path.
    pub(crate) fn save_file<F: File>(&self, file: &F, blob: &F::Data) -> Result<()> {
        let format = format_of(file)?;
        // • Get the path to the file.
        let path = file.path(self)?;
        // • Serialize before touching the disk, so a failure can't
        //   truncate an existing file.
        let marshalled = format.serialize(blob)?;
        // • Create the file if it doesn't exist.
        let mut file = std::fs::File::create(path).into_diagnostic()?;
        file.write_all(marshalled.as_bytes()).into_diagnostic()?;
        file.sync_all().into_diagnostic()?;
        Ok(())
    }

    /// Returns the expected directory for this particular file type.
    fn dir(&self, typ: DirectoryType) -> Result<PathBuf> {
        match typ {
            DirectoryType::Cache => Ok(self.xdg_dirs.cache_dir().to_path_buf()),
            DirectoryType::Pwd => std::env::current_dir().into_diagnostic(),
            DirectoryType::Data => Ok(self.xdg_dirs.data_dir().to_path_buf()),
        }
    }

    // TODO: Should we mock this function out using a virtual filesystem
    //       for testing?
    /// Ensure the given directory exists by recursively creating
    /// the necessary config dirs.
    fn init_dir(&self, typ: DirectoryType) -> Result<PathBuf> {
        let path_buf = self.dir(typ)?;
        let path = path_buf.as_path();

        // Build an error that displays the path and the OS error message
        // if we can't create the directory.
        let on_err = |err| {
            let displayable_path = path.display();
            // TODO: Turn this into a error with a diagnostic code.
            miette!("Could not create cache directory at {displayable_path}: {err}")
        };

        // Create the directory. This is a no-op if they already exist.
        std::fs::create_dir_all(path).map_err(on_err)?;
        // Return an owned path to the directory.
        Ok(path.to_path_buf())
    }
}

/// A shorthand for referring to one of the $XDG directories.
/// As we need additional directories, we'll add them to the enum.
pub enum DirectoryType {
    /// The directory for non-essential project files
    Cache,
    /// Persistent data lives here between runs.
    // We will probably need this later, like when we need to check
    // version expiration dates without phoning home.
    #[allow(dead_code)]
    Data,
    /// Sometimes, we need to create new files from scratch in the
    /// working directory. This extension is for cases when we're
    /// not interested in the application root.
    Pwd,
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use super::format::ParseError;
    use super::*;
    use figment::Jail;
    use serde::{Deserialize, Serialize};

    /// A representative payload exercising nested and collection types.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Settings {
        name: String,
        retries: u32,
        verbose: bool,
        tags: Vec<String>,
        nested: Nested,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Nested {
        threshold: f64,
    }

    fn sample() -> Settings {
        Settings {
            name: "slcr".to_owned(),
            retries: 3,
            verbose: true,
            tags: vec!["alpha".to_owned(), "beta".to_owned()],
            nested: Nested { threshold: 0.75 },
        }
    }

    struct TomlSettings;

    impl StaticFile for TomlSettings {
        type Data = Settings;
        const DIR: DirectoryType = DirectoryType::Pwd;
        const NAME: &'static str = "settings";
        const EXTENSION: &'static str = "toml";
    }

    struct JsonSettings;

    impl StaticFile for JsonSettings {
        type Data = Settings;
        const DIR: DirectoryType = DirectoryType::Pwd;
        const NAME: &'static str = "settings";
        const EXTENSION: &'static str = "json";
    }

    struct YamlSettings;

    impl StaticFile for YamlSettings {
        type Data = Settings;
        const DIR: DirectoryType = DirectoryType::Pwd;
        const NAME: &'static str = "settings";
        const EXTENSION: &'static str = "yaml";
    }

    struct YmlSettings;

    impl StaticFile for YmlSettings {
        type Data = Settings;
        const DIR: DirectoryType = DirectoryType::Pwd;
        const NAME: &'static str = "settings";
        const EXTENSION: &'static str = "yml";
    }

    struct UnknownSettings;

    impl StaticFile for UnknownSettings {
        type Data = Settings;
        const DIR: DirectoryType = DirectoryType::Pwd;
        const NAME: &'static str = "settings";
        const EXTENSION: &'static str = "ini";
    }

    /// Save, load, and delete `F` from the jail's working directory,
    /// asserting the payload survives the round trip unchanged.
    fn round_trip<F: StaticFile<Data = Settings>>(file: F) {
        let fs = FileSystem::new().unwrap();
        let expected = sample();

        fs.save_file(&file, &expected).unwrap();
        let filename = format!("{}.{}", F::NAME, F::EXTENSION);
        assert!(std::path::Path::new(&filename).exists());

        let actual = fs.load_file(file).unwrap();
        assert_eq!(actual, expected);

        assert!(fs.delete_file::<F>().unwrap());
        assert!(!std::path::Path::new(&filename).exists());
    }

    #[test]
    fn toml_round_trip() {
        Jail::expect_with(|_| {
            round_trip(TomlSettings);
            Ok(())
        });
    }

    #[test]
    fn json_round_trip() {
        Jail::expect_with(|_| {
            round_trip(JsonSettings);
            Ok(())
        });
    }

    #[test]
    fn yaml_round_trip() {
        Jail::expect_with(|_| {
            round_trip(YamlSettings);
            Ok(())
        });
    }

    #[test]
    fn yml_round_trip() {
        Jail::expect_with(|_| {
            round_trip(YmlSettings);
            Ok(())
        });
    }

    #[test]
    fn toml_hand_written_file_loads() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "settings.toml",
                r#"
                name = "slcr"
                retries = 3
                verbose = true
                tags = ["alpha", "beta"]

                [nested]
                threshold = 0.75
                "#,
            )?;
            let fs = FileSystem::new().unwrap();
            assert_eq!(fs.load_file(TomlSettings).unwrap(), sample());
            Ok(())
        });
    }

    #[test]
    fn yaml_hand_written_file_loads() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "settings.yaml",
                "# The application name.\n\
                 name: slcr\n\
                 retries: 3\n\
                 verbose: true\n\
                 tags:\n  \
                   - alpha\n  \
                   - beta\n\
                 nested:\n  \
                   threshold: 0.75\n",
            )?;
            let fs = FileSystem::new().unwrap();
            assert_eq!(fs.load_file(YamlSettings).unwrap(), sample());
            Ok(())
        });
    }

    #[test]
    fn delete_missing_file_returns_false() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            assert!(!fs.delete_file::<JsonSettings>().unwrap());
            Ok(())
        });
    }

    #[test]
    fn load_missing_file_is_an_error() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            assert!(fs.load_file(TomlSettings).is_err());
            Ok(())
        });
    }

    #[test]
    fn load_malformed_file_is_an_error() {
        Jail::expect_with(|jail| {
            jail.create_file("settings.json", "{ not json")?;
            let fs = FileSystem::new().unwrap();
            assert!(fs.load_file(JsonSettings).is_err());
            Ok(())
        });
    }

    #[test]
    fn a_missing_file_names_its_path() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            let err = fs.load_file(JsonSettings).unwrap_err();
            let read = err.downcast_ref::<ReadError>().expect("a read error");
            assert!(read.path.ends_with("settings.json"), "{err}");
            assert_eq!(read.source.kind(), std::io::ErrorKind::NotFound);
            assert!(err.to_string().starts_with("could not read "), "{err}");
            Ok(())
        });
    }

    #[test]
    fn a_directory_cannot_be_read_as_a_file() {
        Jail::expect_with(|jail| {
            std::fs::create_dir(jail.directory().join("settings.yaml")).unwrap();
            let fs = FileSystem::new().unwrap();
            let err = fs.load_file(YamlSettings).unwrap_err();
            assert!(err.downcast_ref::<ReadError>().is_some(), "{err}");
            Ok(())
        });
    }

    #[test]
    fn a_file_that_is_not_utf8_cannot_be_read() {
        Jail::expect_with(|jail| {
            std::fs::write(jail.directory().join("settings.toml"), [0xff, 0xfe, b'x']).unwrap();
            let fs = FileSystem::new().unwrap();
            let err = fs.load_file(TomlSettings).unwrap_err();
            let read = err.downcast_ref::<ReadError>().expect("a read error");
            assert_eq!(read.source.kind(), std::io::ErrorKind::InvalidData);
            Ok(())
        });
    }

    #[test]
    fn a_malformed_file_reports_where_it_is_malformed() {
        Jail::expect_with(|jail| {
            jail.create_file("settings.yaml", "name: slcr\nretries: many\n")?;
            let fs = FileSystem::new().unwrap();
            let err = fs.load_file(YamlSettings).unwrap_err();
            let parse = err.downcast_ref::<ParseError>().expect("a parse error");
            let span = parse.span().expect("a location");
            assert_eq!(span.offset(), "name: slcr\nretries: ".len());
            assert!(err.to_string().ends_with("settings.yaml as YAML"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn unknown_extension_is_an_error() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            assert!(fs.save_file(&UnknownSettings, &sample()).is_err());
            assert!(fs.load_file(UnknownSettings).is_err());
            Ok(())
        });
    }
}
