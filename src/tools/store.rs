use std::path::{Path, PathBuf};

use miette::{Diagnostic, Result};
use thiserror::Error;

use super::descriptor::{self, Descriptor, InvalidDescriptor};
use super::pin::Pin;
use crate::fs::FileSystem;

/// The descriptor store: where descriptors are installed on this machine,
/// filed by their pin at `<data dir>/descriptors/blake3/<hex>.toml`.
///
/// It lives under the platform's data directory (`$XDG_DATA_HOME/slcr` on
/// Linux), rather than the cache, because a cache wipe would destroy
/// descriptors
/// people wrote locally. Projects that pin the same bytes share one file,
/// and projects that pin different descriptors for the same tool can't
/// conflict. Nothing in the store relies on a descriptor's name or
/// version.
pub(crate) struct DescriptorStore {
    dir: PathBuf,
}

/// The error returned when the store's directory can't be created.
#[derive(Debug, Error, Diagnostic)]
#[error("could not create the descriptor store at {}", .dir.display())]
#[diagnostic(help(
    "slcr installs descriptors in its data directory; make sure it can create directories there"
))]
pub struct StoreUnavailable {
    dir: PathBuf,
    #[source]
    source: std::io::Error,
}

/// Why the store can't give back the descriptor a pin names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// Nothing is filed under the pin.
    Missing,
    /// What is filed under the pin doesn't hash to it.
    Corrupted,
    /// What is filed under the pin can't be read, for this reason.
    Unreadable(String),
    /// What is filed under the pin hashes to it, but isn't a valid
    /// descriptor. Errors name it by its abbreviated pin.
    Invalid(InvalidDescriptor),
}

impl DescriptorStore {
    /// The store in `fs`'s data directory.
    pub(crate) fn new(fs: &FileSystem) -> Self {
        Self::at(fs.data_dir().join("descriptors").join("blake3"))
    }

    /// The store filed under `dir`.
    pub(crate) fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Where the descriptor pinned by `pin` is filed.
    pub(crate) fn path(&self, pin: &Pin) -> PathBuf {
        self.dir.join(format!("{}.toml", pin.hex()))
    }

    /// File the descriptor `text` under its pin, and return the pin. A file
    /// already there that hashes to the pin is left alone; anything else
    /// there, such as a corrupted file, is replaced.
    pub(crate) fn install(&self, fs: &FileSystem, text: &str) -> Result<Pin> {
        let pin = Pin::of(text.as_bytes());
        let path = self.path(&pin);
        if self.holds(fs, &path, &pin) {
            return Ok(pin);
        }
        std::fs::create_dir_all(&self.dir).map_err(|source| StoreUnavailable {
            dir: self.dir.clone(),
            source,
        })?;
        fs.replace_file(&path, text)?;
        Ok(pin)
    }

    /// The descriptor filed under `pin`, once its bytes are checked against
    /// the pin and it is checked as a descriptor.
    pub(crate) fn load(&self, fs: &FileSystem, pin: &Pin) -> Result<Descriptor, Unavailable> {
        let bytes = match fs.read_bytes(&self.path(pin)) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err(Unavailable::Missing),
            // The reading error names the file's absolute path; its cause
            // doesn't.
            Err(err) => {
                let reason = std::error::Error::source(&err).map_or_else(
                    || "the file could not be read".to_owned(),
                    ToString::to_string,
                );
                return Err(Unavailable::Unreadable(reason));
            }
        };
        if !pin.matches(&bytes) {
            return Err(Unavailable::Corrupted);
        }
        let origin = pin.abbreviated();
        descriptor::text(&origin, &bytes)
            .and_then(|text| Descriptor::parse(&origin, text))
            .map_err(Unavailable::Invalid)
    }

    /// Whether the file at `path` holds exactly the bytes `pin` pins.
    fn holds(&self, fs: &FileSystem, path: &Path, pin: &Pin) -> bool {
        matches!(fs.read_bytes(path), Ok(Some(bytes)) if pin.matches(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cargo-nextest example on the Schemas page.
    const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

    /// A store in a fresh temporary directory, which the store's own
    /// directory is nested inside, as it is in a data directory.
    fn store() -> (tempfile::TempDir, DescriptorStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DescriptorStore::at(dir.path().join("descriptors/blake3"));
        (dir, store)
    }

    #[test]
    fn descriptors_are_filed_under_their_pin() {
        let (dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let text = "name = \"jev\"\n";

        let pin = store.install(&fs, text).unwrap();
        assert_eq!(pin, Pin::of(text.as_bytes()));
        let path = store.path(&pin);
        assert_eq!(
            path,
            dir.path()
                .join(format!("descriptors/blake3/{}.toml", pin.hex()))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    /// Installing writes through a temporary file renamed into place, so
    /// a file that keeps its inode was never rewritten.
    #[cfg(unix)]
    #[test]
    fn installing_again_changes_nothing() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let pin = store.install(&fs, "a").unwrap();
        let inode = || std::fs::metadata(store.path(&pin)).unwrap().ino();
        let before = inode();
        assert_eq!(store.install(&fs, "a").unwrap(), pin);
        assert_eq!(inode(), before);
    }

    #[test]
    fn a_corrupted_descriptor_is_replaced() {
        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let pin = store.install(&fs, "a").unwrap();
        std::fs::write(store.path(&pin), "tampered").unwrap();
        store.install(&fs, "a").unwrap();
        assert_eq!(std::fs::read_to_string(store.path(&pin)).unwrap(), "a");
    }

    #[test]
    fn descriptors_with_different_bytes_never_collide() {
        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let one = store.install(&fs, "name = \"curl\"\n").unwrap();
        let other = store.install(&fs, "name = \"curl\" # ours\n").unwrap();
        assert_ne!(store.path(&one), store.path(&other));
        assert!(store.path(&one).is_file() && store.path(&other).is_file());
    }

    #[test]
    fn an_installed_descriptor_loads_by_its_pin() {
        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let pin = store.install(&fs, NEXTEST).unwrap();
        let descriptor = store.load(&fs, &pin).unwrap();
        assert_eq!(descriptor, Descriptor::parse("any", NEXTEST).unwrap());
    }

    #[test]
    fn loading_checks_the_bytes_and_the_descriptor() {
        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let missing = Pin::of(b"missing");
        assert_eq!(store.load(&fs, &missing), Err(Unavailable::Missing));

        let pin = store.install(&fs, NEXTEST).unwrap();
        std::fs::write(store.path(&pin), format!("{NEXTEST}\n")).unwrap();
        assert_eq!(store.load(&fs, &pin), Err(Unavailable::Corrupted));

        let invalid = "name = \"cargo-nextest\"\n";
        let pin = store.install(&fs, invalid).unwrap();
        let Err(Unavailable::Invalid(err)) = store.load(&fs, &pin) else {
            panic!("expected an invalid descriptor");
        };
        // Named by its pin, never by its absolute path.
        assert_eq!(
            err.to_string(),
            format!("could not parse {} as TOML", pin.abbreviated())
        );

        let binary = b"name = \"\xff\"";
        let pin = Pin::of(binary);
        std::fs::write(store.path(&pin), binary).unwrap();
        let Err(Unavailable::Invalid(err)) = store.load(&fs, &pin) else {
            panic!("expected an invalid descriptor");
        };
        assert_eq!(
            err.to_string(),
            format!("{} is not UTF-8 text", pin.abbreviated())
        );
    }

    #[test]
    fn an_unreadable_descriptor_names_no_path() {
        let (_dir, store) = store();
        let fs = FileSystem::new().unwrap();
        let pin = Pin::of(b"a directory");
        std::fs::create_dir_all(store.path(&pin)).unwrap();
        let Err(Unavailable::Unreadable(reason)) = store.load(&fs, &pin) else {
            panic!("expected an unreadable descriptor");
        };
        assert!(reason.starts_with("Is a directory"), "{reason}");
    }

    #[test]
    fn a_store_that_cannot_be_created_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the store's parent directory would be.
        std::fs::write(dir.path().join("descriptors"), "").unwrap();
        let store = DescriptorStore::at(dir.path().join("descriptors/blake3"));
        let err = store.install(&FileSystem::new().unwrap(), "a").unwrap_err();
        assert!(err.downcast_ref::<StoreUnavailable>().is_some(), "{err:?}");
    }
}
