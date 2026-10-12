use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{Environment, pin::Pin, source::Fetcher, store::DescriptorStore};
use crate::fs::FileSystem;

/// The cargo-nextest example on the Schemas page.
pub(crate) const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

/// A manifest that declares no tools.
pub(crate) const MANIFEST: &str = "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";

/// A project, with the cargo-nextest descriptor kept in it at
/// `tools/cargo-nextest.toml`, and an empty descriptor store beside it.
pub(crate) struct Fixture {
    dir: TempDir,
    pub(crate) fs: FileSystem,
    pub(crate) store: DescriptorStore,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("tools")).unwrap();
        std::fs::write(project.join("SLCR.toml"), MANIFEST).unwrap();
        std::fs::write(project.join("tools/cargo-nextest.toml"), NEXTEST).unwrap();
        let store = DescriptorStore::at(dir.path().join("store"));
        Self {
            dir,
            fs: FileSystem::new().unwrap(),
            store,
        }
    }

    pub(crate) fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    /// An environment that runs in `cwd` and fetches with `fetcher`.
    pub(crate) fn env<'a>(&'a self, fetcher: &'a Fetcher, cwd: &'a Path) -> Environment<'a> {
        Environment {
            fs: &self.fs,
            store: &self.store,
            fetcher,
            cwd,
        }
    }

    pub(crate) fn manifest(&self) -> String {
        std::fs::read_to_string(self.project().join("SLCR.toml")).unwrap()
    }

    /// Write `contents` to the project file at `path`.
    pub(crate) fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        std::fs::write(self.project().join(path), contents).unwrap();
    }

    /// Whether the store holds the descriptor `text`.
    pub(crate) fn installed(&self, text: &str) -> bool {
        self.store.path(&Pin::of(text.as_bytes())).is_file()
    }

    /// Whether the store holds anything at all.
    pub(crate) fn store_is_empty(&self) -> bool {
        !self.dir.path().join("store").exists()
    }
}
