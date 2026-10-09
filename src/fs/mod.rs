use directories::ProjectDirs;
use miette::{Diagnostic, IntoDiagnostic, Result, miette};
use thiserror::Error;

use std::{
    fmt::Display,
    fs::OpenOptions,
    io::{ErrorKind, Write},
    path::PathBuf,
};

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

/// The error returned when a file to be created already exists.
#[derive(Debug, Error, Diagnostic)]
#[error("{} already exists", .path.display())]
#[diagnostic(help("choose another path, or delete the existing file first"))]
pub struct AlreadyExists {
    path: PathBuf,
}

/// The error returned when a file can't be written.
#[derive(Debug, Error)]
#[error("could not write {}", .path.display())]
pub struct WriteError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

impl Diagnostic for WriteError {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        // A missing directory is the likeliest cause, and the one a user
        // fixes by hand.
        if self.source.kind() != ErrorKind::NotFound {
            return None;
        }
        let directory = self.path.parent()?;
        if directory.as_os_str().is_empty() {
            return None;
        }
        Some(Box::new(format!(
            "create the directory {} first",
            directory.display()
        )))
    }
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

    /// Store the file at its canonical path, which must not exist yet.
    ///
    /// Unlike [FileSystem::save_file], this never overwrites anything:
    /// whatever is at the path, even a dangling symlink, makes it fail with
    /// [AlreadyExists]. The check and the creation are one atomic step, so
    /// nothing can appear at the path in between.
    pub(crate) fn create_file<F: File>(&self, file: &F, blob: &F::Data) -> Result<()> {
        let format = format_of(file)?;
        let path = file.path(self)?;
        // Serialize first, so a failure can't leave an empty file behind.
        let marshalled = format.serialize(blob)?;
        let mut handle = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(handle) => handle,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                return Err(AlreadyExists { path }.into());
            }
            Err(source) => return Err(WriteError { path, source }.into()),
        };
        let written = handle
            .write_all(marshalled.as_bytes())
            .and_then(|()| handle.sync_all());
        if let Err(source) = written {
            drop(handle);
            // Don't leave a partial file behind. The write error is the one
            // worth reporting, so a failure to clean up is ignored.
            let _ = std::fs::remove_file(&path);
            return Err(WriteError { path, source }.into());
        }
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
    use crate::terminal::render_plain;
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
            assert!(fs.create_file(&UnknownSettings, &sample()).is_err());
            assert!(!std::path::Path::new("settings.ini").exists());
            assert!(fs.load_file(UnknownSettings).is_err());
            Ok(())
        });
    }

    /// A file of `T` at a caller-chosen path, whose extension names its
    /// format.
    struct At<T>(PathBuf, std::marker::PhantomData<T>);

    fn at<T>(path: &str) -> At<T> {
        At(PathBuf::from(path), std::marker::PhantomData)
    }

    impl<T: serde::de::DeserializeOwned + Serialize> File for At<T> {
        type Data = T;

        fn extension(&self) -> &str {
            self.0.extension().unwrap().to_str().unwrap()
        }

        fn path(&self, _fs: &FileSystem) -> Result<PathBuf> {
            Ok(self.0.clone())
        }
    }

    /// Data that always fails to serialize.
    #[derive(Debug, Deserialize)]
    struct Unserializable;

    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("refused"))
        }
    }

    #[test]
    fn create_file_writes_a_new_file_in_its_format() {
        Jail::expect_with(|jail| {
            let fs = FileSystem::new().unwrap();
            for (file, format) in [
                (at::<Settings>("new.json"), Format::Json),
                (at("new.yaml"), Format::Yaml),
                (at("new.toml"), Format::Toml),
            ] {
                fs.create_file(&file, &sample()).unwrap();
                let written = std::fs::read_to_string(jail.directory().join(&file.0)).unwrap();
                assert_eq!(written, format.serialize(&sample()).unwrap(), "{format}");
                assert_eq!(fs.load_file(file).unwrap(), sample(), "{format}");
            }
            Ok(())
        });
    }

    #[test]
    fn create_file_never_overwrites_a_file() {
        Jail::expect_with(|jail| {
            jail.create_file("settings.json", "keep me")?;
            let fs = FileSystem::new().unwrap();
            let err = fs.create_file(&JsonSettings, &sample()).unwrap_err();
            let exists = err.downcast_ref::<AlreadyExists>().expect("already exists");
            assert!(exists.path.ends_with("settings.json"), "{err}");
            let contents = std::fs::read_to_string(jail.directory().join("settings.json")).unwrap();
            assert_eq!(contents, "keep me");
            Ok(())
        });
    }

    #[test]
    fn create_file_fails_the_second_time() {
        Jail::expect_with(|_| {
            let fs = FileSystem::new().unwrap();
            fs.create_file(&YamlSettings, &sample()).unwrap();
            let err = fs.create_file(&YamlSettings, &sample()).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            Ok(())
        });
    }

    #[test]
    fn create_file_never_replaces_a_directory() {
        Jail::expect_with(|jail| {
            jail.create_dir("settings.toml")?;
            let fs = FileSystem::new().unwrap();
            let err = fs.create_file(&TomlSettings, &sample()).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            assert!(jail.directory().join("settings.toml").is_dir());
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn create_file_never_follows_a_symlink() {
        Jail::expect_with(|jail| {
            let link = jail.directory().join("settings.json");
            std::os::unix::fs::symlink(jail.directory().join("target.json"), &link).unwrap();
            let fs = FileSystem::new().unwrap();
            let err = fs.create_file(&JsonSettings, &sample()).unwrap_err();
            assert!(err.downcast_ref::<AlreadyExists>().is_some(), "{err}");
            // The link still dangles: nothing was written through it.
            assert!(!jail.directory().join("target.json").exists());
            Ok(())
        });
    }

    #[test]
    fn create_file_does_not_create_a_missing_directory() {
        Jail::expect_with(|jail| {
            let fs = FileSystem::new().unwrap();
            let err = fs
                .create_file(&at::<Settings>("missing/settings.json"), &sample())
                .unwrap_err();
            let write = err.downcast_ref::<WriteError>().expect("a write error");
            assert_eq!(write.path, PathBuf::from("missing/settings.json"));
            assert_eq!(write.source.kind(), std::io::ErrorKind::NotFound);
            assert!(!jail.directory().join("missing").exists());
            Ok(())
        });
    }

    #[test]
    fn create_file_fails_when_the_parent_is_not_a_directory() {
        Jail::expect_with(|jail| {
            jail.create_file("plain", "")?;
            let fs = FileSystem::new().unwrap();
            let err = fs
                .create_file(&at::<Settings>("plain/settings.json"), &sample())
                .unwrap_err();
            let write = err.downcast_ref::<WriteError>().expect("a write error");
            assert_eq!(write.source.kind(), std::io::ErrorKind::NotADirectory);
            assert!(write.help().is_none());
            Ok(())
        });
    }

    #[test]
    fn create_file_leaves_nothing_behind_when_serialization_fails() {
        Jail::expect_with(|jail| {
            let fs = FileSystem::new().unwrap();
            for name in ["bad.json", "bad.yaml", "bad.toml"] {
                assert!(
                    fs.create_file(&at::<Unserializable>(name), &Unserializable)
                        .is_err(),
                    "{name}"
                );
                assert!(!jail.directory().join(name).exists(), "{name}");
            }
            Ok(())
        });
    }

    #[test]
    fn already_exists_renders_its_help() {
        let err = AlreadyExists {
            path: PathBuf::from("SPEC.slcr.yml"),
        };
        assert_eq!(
            render_plain(&err),
            "  × SPEC.slcr.yml already exists\n  \
             help: choose another path, or delete the existing file first\n"
        );
    }

    #[test]
    fn a_missing_directory_renders_with_help_to_create_it() {
        let err = WriteError {
            path: PathBuf::from("specs/api/SPEC.slcr.yml"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert_eq!(
            render_plain(&err),
            "  × could not write specs/api/SPEC.slcr.yml\n  \
             ╰─▶ entity not found\n  \
             help: create the directory specs/api first\n"
        );
    }

    #[test]
    fn write_errors_offer_help_only_for_a_missing_directory() {
        let error = |path: &str, kind| WriteError {
            path: PathBuf::from(path),
            source: std::io::Error::from(kind),
        };
        let help = |err: WriteError| err.help().map(|help| help.to_string());
        assert_eq!(
            help(error("a/b.yml", ErrorKind::NotFound)).as_deref(),
            Some("create the directory a first")
        );
        // A file in the working directory has no directory to create.
        assert_eq!(help(error("b.yml", ErrorKind::NotFound)), None);
        assert_eq!(help(error("a/b.yml", ErrorKind::PermissionDenied)), None);
    }
}
