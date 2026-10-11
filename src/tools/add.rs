use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use miette::{Diagnostic, Result};
use thiserror::Error;

use super::{
    DescriptorVersion, ToolKey,
    descriptor::{self, Descriptor},
    manifest::{ManifestFile, Project, ToolEntry},
    pin::Pin,
    source::{Fetcher, SourceArg},
    store::DescriptorStore,
};
use crate::fs::FileSystem;

/// What [add] reads and writes outside the project, passed in so that a
/// caller, such as a test, chooses it.
pub(crate) struct Environment<'a> {
    pub(crate) fs: &'a FileSystem,
    pub(crate) store: &'a DescriptorStore,
    pub(crate) fetcher: &'a Fetcher,
    /// The working directory, as an absolute path: where the project is
    /// looked for, and what a path source is relative to.
    pub(crate) cwd: &'a Path,
}

/// Fetch the descriptor at `source`, validate it, pin it, install it in the
/// descriptor store, and declare it in the manifest under `key`, or else
/// under the descriptor's own name.
///
/// Nothing is installed or declared unless the descriptor is valid and the
/// name is free, and the manifest is written last, so it never declares a
/// descriptor that failed to install. Adding a descriptor that is already
/// declared under the same name, from the same source, with the same pin,
/// changes nothing in the manifest, but installs the descriptor again if
/// the store has lost it.
pub(crate) async fn add(
    env: &Environment<'_>,
    source: SourceArg,
    key: Option<ToolKey>,
) -> Result<Added> {
    let project = Project::find_from(env.cwd)?;
    let mut manifest = ManifestFile::load(&project)?;

    let origin = source.to_string();
    tracing::debug!(%origin, "fetching a descriptor");
    let (source, bytes) = source
        .fetch(env.fs, env.fetcher, env.cwd, project.root())
        .await?;
    let text = descriptor::text(&origin, &bytes)?;
    let descriptor = Descriptor::parse(&origin, text)?;
    let key = key.unwrap_or_else(|| descriptor.name().clone());
    let entry = ToolEntry::new(
        descriptor.version().clone(),
        source,
        Pin::of(text.as_bytes()),
    );

    let change = match manifest.manifest().tools().get(&key) {
        Some(existing) if *existing == entry => Change::AlreadyDeclared,
        Some(_) => {
            return Err(AlreadyDeclared {
                manifest: project.shown_manifest().to_path_buf(),
                key,
            }
            .into());
        }
        None => Change::Declared,
    };
    env.store.install(env.fs, text)?;
    if change == Change::Declared {
        manifest.set_tool(&key, entry);
        manifest.save(env.fs)?;
    }
    Ok(Added {
        manifest: project.shown_manifest().to_path_buf(),
        key,
        version: descriptor.version().clone(),
        change,
    })
}

/// The error returned when the manifest already declares a different tool
/// under the name being added.
#[derive(Debug, Error, Diagnostic)]
#[error("{} already declares a tool named {key}", .manifest.display())]
#[diagnostic(help(
    "declare this descriptor under another name with `--as <NAME>`, or remove the existing \
     entry from the manifest first"
))]
pub struct AlreadyDeclared {
    manifest: PathBuf,
    key: ToolKey,
}

/// What [add] did to the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// It declared the tool.
    Declared,
    /// The manifest already declared exactly this, so it changed nothing.
    AlreadyDeclared,
}

/// What [add] did, which displays as the status `slcr tools add` reports,
/// e.g. `SLCR.toml: added tool cargo-nextest 1.0.0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Added {
    /// The manifest's path relative to the working directory.
    manifest: PathBuf,
    key: ToolKey,
    version: DescriptorVersion,
    change: Change,
}

impl Display for Added {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            manifest,
            key,
            version,
            change,
        } = self;
        let manifest = manifest.display();
        match change {
            Change::Declared => write!(f, "{manifest}: added tool {key} {version}"),
            Change::AlreadyDeclared => {
                write!(f, "{manifest}: tool {key} {version} is already declared")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::cli::block_on;
    use crate::tools::https_server::{HttpsServer, Reply};
    use crate::tools::source::SourceError;

    /// The cargo-nextest example on the Schemas page.
    const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

    /// A manifest that declares no tools.
    const MANIFEST: &str = "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";

    /// A project, with the cargo-nextest descriptor kept in it at
    /// `tools/cargo-nextest.toml`, and an empty descriptor store beside it.
    struct Fixture {
        dir: TempDir,
        fs: FileSystem,
        store: DescriptorStore,
    }

    impl Fixture {
        fn new() -> Self {
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

        fn project(&self) -> PathBuf {
            self.dir.path().join("project")
        }

        fn manifest(&self) -> String {
            std::fs::read_to_string(self.project().join("SLCR.toml")).unwrap()
        }

        /// Whether the store holds the descriptor `text`.
        fn installed(&self, text: &str) -> bool {
            self.store.path(&Pin::of(text.as_bytes())).is_file()
        }

        /// Whether the store holds anything at all.
        fn store_is_empty(&self) -> bool {
            !self.dir.path().join("store").exists()
        }

        /// Run [add] from the project root with `fetcher`.
        async fn add_with(
            &self,
            fetcher: &Fetcher,
            source: &str,
            key: Option<&str>,
        ) -> Result<Added> {
            let cwd = self.project();
            let env = Environment {
                fs: &self.fs,
                store: &self.store,
                fetcher,
                cwd: &cwd,
            };
            let key = key.map(|key| ToolKey::try_from(key.to_owned()).unwrap());
            add(&env, source.parse().unwrap(), key).await
        }

        /// Run [add] on a source that needs no network.
        fn add(&self, source: &str, key: Option<&str>) -> Result<Added> {
            let fetcher = Fetcher::new();
            block_on(self.add_with(&fetcher, source, key)).unwrap()
        }
    }

    #[test]
    fn a_descriptor_kept_in_the_project_is_installed_and_declared() {
        let fixture = Fixture::new();
        let added = fixture.add("tools/cargo-nextest.toml", None).unwrap();
        assert_eq!(added.change, Change::Declared);
        assert_eq!(
            added.to_string(),
            "SLCR.toml: added tool cargo-nextest 1.0.0"
        );
        assert!(fixture.installed(NEXTEST));
        assert!(
            fixture
                .manifest()
                .contains("[tools.cargo-nextest]\nversion = \"1.0.0\"\n")
        );
    }

    #[test]
    fn a_published_descriptor_is_fetched_installed_and_declared() {
        let fixture = Fixture::new();
        block_on(async {
            let server = HttpsServer::start(vec![("/nextest.toml", Reply::ok(NEXTEST))]).await;
            let url = server.https("/nextest.toml");
            let added = fixture
                .add_with(&server.fetcher(), &url, None)
                .await
                .unwrap();
            assert_eq!(added.change, Change::Declared);
            assert!(
                fixture
                    .manifest()
                    .contains(&format!("source = {{ url = \"{url}\" }}"))
            );
        })
        .unwrap();
        assert!(fixture.installed(NEXTEST));
    }

    #[test]
    fn a_failed_fetch_installs_and_declares_nothing() {
        let fixture = Fixture::new();
        block_on(async {
            let server = HttpsServer::start(Vec::new()).await;
            let url = server.https("/missing.toml");
            let err = fixture
                .add_with(&server.fetcher(), &url, None)
                .await
                .unwrap_err();
            assert!(
                matches!(err.downcast_ref(), Some(SourceError::Fetch { .. })),
                "{err:?}"
            );
        })
        .unwrap();
        assert_eq!(fixture.manifest(), MANIFEST);
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn a_name_overrides_the_descriptors_own() {
        let fixture = Fixture::new();
        let added = fixture
            .add("tools/cargo-nextest.toml", Some("nextest"))
            .unwrap();
        assert_eq!(added.key.as_str(), "nextest");
        assert!(fixture.manifest().contains("[tools.nextest]"));
        assert!(!fixture.manifest().contains("[tools.cargo-nextest]"));
    }

    #[test]
    fn adding_the_same_descriptor_again_changes_nothing() {
        let fixture = Fixture::new();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        let manifest = fixture.manifest();
        let added = fixture.add("tools/cargo-nextest.toml", None).unwrap();
        assert_eq!(added.change, Change::AlreadyDeclared);
        assert_eq!(
            added.to_string(),
            "SLCR.toml: tool cargo-nextest 1.0.0 is already declared"
        );
        assert_eq!(fixture.manifest(), manifest);
    }

    #[test]
    fn adding_the_same_descriptor_again_reinstalls_it() {
        let fixture = Fixture::new();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        std::fs::remove_file(fixture.store.path(&Pin::of(NEXTEST.as_bytes()))).unwrap();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        assert!(fixture.installed(NEXTEST));
    }

    #[test]
    fn another_tool_of_the_same_name_is_refused_before_anything_is_installed() {
        let fixture = Fixture::new();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        let manifest = fixture.manifest();
        let newer = NEXTEST.replace("version = \"1.0.0\"", "version = \"1.1.0\"");
        std::fs::write(fixture.project().join("tools/cargo-nextest.toml"), &newer).unwrap();

        let err = fixture.add("tools/cargo-nextest.toml", None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "SLCR.toml already declares a tool named cargo-nextest"
        );
        assert_eq!(fixture.manifest(), manifest);
        assert!(!fixture.installed(&newer));
    }

    #[test]
    fn an_invalid_descriptor_installs_and_declares_nothing() {
        let fixture = Fixture::new();
        for invalid in [
            NEXTEST.replace("{with.test}", "{with.name}").into_bytes(),
            NEXTEST
                .replace("[invoke]\n", "[invoke]\nshell = true\n")
                .into_bytes(),
            b"name = \"\xff\"".to_vec(),
        ] {
            std::fs::write(fixture.project().join("tools/cargo-nextest.toml"), invalid).unwrap();
            assert!(fixture.add("tools/cargo-nextest.toml", None).is_err());
            assert_eq!(fixture.manifest(), MANIFEST);
            assert!(fixture.store_is_empty());
        }
    }

    #[test]
    fn the_manifest_is_named_relative_to_the_working_directory() {
        let fixture = Fixture::new();
        let cwd = fixture.project().join("crates/api");
        std::fs::create_dir_all(&cwd).unwrap();
        let fetcher = Fetcher::new();
        let env = Environment {
            fs: &fixture.fs,
            store: &fixture.store,
            fetcher: &fetcher,
            cwd: &cwd,
        };
        let source = "../../tools/cargo-nextest.toml".parse().unwrap();
        let added = block_on(add(&env, source, None)).unwrap().unwrap();
        assert_eq!(
            added.to_string(),
            "../../SLCR.toml: added tool cargo-nextest 1.0.0"
        );
        assert!(
            fixture
                .manifest()
                .contains("source = { path = \"tools/cargo-nextest.toml\" }")
        );
    }
}
