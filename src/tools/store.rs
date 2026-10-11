use std::path::{Path, PathBuf};

use miette::{Diagnostic, Result};
use thiserror::Error;

use super::pin::Pin;
use crate::fs::FileSystem;

/// The descriptor store: where descriptors are installed on this machine,
/// filed by their pin at `<data dir>/descriptors/blake3/<hex>.toml`.
///
/// It lives under the data directory (`$XDG_DATA_HOME/slcr` on Linux,
/// unless `SLCR_DATA_DIR` overrides it)
/// rather than the cache, because a cache wipe would destroy descriptors
/// people wrote locally. Projects that pin the same bytes share one file,
/// and projects that pin different descriptors for the same tool can't
/// conflict. Nothing in the store relies on a descriptor's name or
/// version.
pub(crate) struct DescriptorStore {
    dir: PathBuf,
}

/// The error returned when the store's directory can't be created.
#[derive(Debug, Error, Diagnostic)]
#[error("could not create the descriptor store")]
pub struct StoreUnavailable {
    #[source]
    source: std::io::Error,
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

    /// File `bytes`, which must hash to `pin`, under `pin`. A file already
    /// there that hashes to `pin` is left alone; anything else there, such
    /// as a corrupted file, is replaced.
    pub(crate) fn install(&self, fs: &FileSystem, pin: &Pin, bytes: &[u8]) -> Result<()> {
        assert!(
            pin.matches(bytes),
            "a descriptor is filed under its own pin"
        );
        let path = self.path(pin);
        if self.holds(fs, &path, pin) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|source| StoreUnavailable { source })?;
        // Descriptors are TOML, so they are text.
        let text = std::str::from_utf8(bytes).expect("a descriptor is UTF-8");
        fs.replace_file(&path, text)?;
        Ok(())
    }

    /// Whether the file at `path` holds exactly the bytes `pin` pins.
    fn holds(&self, fs: &FileSystem, path: &Path, pin: &Pin) -> bool {
        matches!(fs.read_bytes(path), Ok(Some(bytes)) if pin.matches(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_filed_under_their_pin() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSystem::new().unwrap();
        let store = DescriptorStore::at(dir.path().join("descriptors/blake3"));
        let bytes = b"name = \"jev\"\n";
        let pin = Pin::of(bytes);

        store.install(&fs, &pin, bytes).unwrap();
        let path = store.path(&pin);
        assert_eq!(
            path,
            dir.path()
                .join(format!("descriptors/blake3/{}.toml", pin.hex()))
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // Installing again changes nothing; a corrupted file is replaced.
        store.install(&fs, &pin, bytes).unwrap();
        std::fs::write(&path, "tampered").unwrap();
        store.install(&fs, &pin, bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
