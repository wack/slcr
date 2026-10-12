use std::{
    fmt::{self, Display},
    path::PathBuf,
};

use miette::Result;

use super::{
    DescriptorVersion, Environment, ToolKey,
    descriptor::{self, Descriptor},
    manifest::{ManifestFile, Project},
    pin::Pin,
};

/// Re-fetch the descriptor of the tool declared under `key` from its
/// source, validate it, install it in the descriptor store, and re-pin the
/// manifest entry to it.
///
/// The manifest is written last, and only if the descriptor is valid and
/// installed, so a failed update leaves the old pin in place. An entry
/// that already pins what its source serves is left alone, but its
/// descriptor is installed again if the store has lost it.
pub(crate) async fn update(env: &Environment<'_>, key: ToolKey) -> Result<Updated> {
    let project = Project::find_from(env.cwd)?;
    let mut manifest = ManifestFile::load(&project)?;
    let declared = manifest.tool(&key)?.clone();

    let source = declared.source();
    let origin = source.to_string();
    tracing::debug!(%origin, "fetching a descriptor");
    let bytes = source.fetch(env.fs, env.fetcher, project.root()).await?;
    let text = descriptor::text(&origin, &bytes)?;
    let descriptor = Descriptor::parse(&origin, text)?;
    let version = descriptor.version().clone();
    let pin = Pin::of(text.as_bytes());

    let change = if version != *declared.version() {
        Change::Updated
    } else if pin != *declared.pin() {
        Change::Repinned
    } else {
        Change::UpToDate
    };
    env.store.install(env.fs, text)?;
    if change != Change::UpToDate {
        manifest.repin_tool(&key, version.clone(), pin);
        manifest.save(env.fs)?;
    }
    Ok(Updated {
        manifest: project.shown_manifest().to_path_buf(),
        key,
        from: declared.version().clone(),
        to: version,
        change,
    })
}

/// What [update] did to the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// It pinned a descriptor with another version.
    Updated,
    /// It pinned a descriptor with the same version but other bytes, such
    /// as a descriptor kept in the project that was edited.
    Repinned,
    /// The entry already pinned what its source serves.
    UpToDate,
}

/// What [update] did, which displays as the status `slcr tools update`
/// reports, e.g. `SLCR.toml: updated tool cargo-nextest 1.0.0 -> 1.1.0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Updated {
    /// The manifest's path relative to the working directory.
    manifest: PathBuf,
    key: ToolKey,
    /// The version the manifest declared.
    from: DescriptorVersion,
    /// The version it declares now.
    to: DescriptorVersion,
    change: Change,
}

impl Display for Updated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            manifest,
            key,
            from,
            to,
            change,
        } = self;
        let manifest = manifest.display();
        match change {
            Change::Updated => write!(f, "{manifest}: updated tool {key} {from} -> {to}"),
            Change::Repinned => write!(f, "{manifest}: re-pinned tool {key} {to}"),
            Change::UpToDate => write!(f, "{manifest}: tool {key} {to} is up to date"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::block_on;
    use crate::tools::fixture::{Fixture, NEXTEST};
    use crate::tools::https_server::{HttpsServer, Reply};
    use crate::tools::manifest::NotDeclared;
    use crate::tools::source::{Fetcher, SourceError};

    /// NEXTEST with its version bumped.
    fn newer() -> String {
        NEXTEST.replace("version = \"1.0.0\"", "version = \"1.1.0\"")
    }

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    impl Fixture {
        /// Run [update] from the project root with `fetcher`.
        async fn update_with(&self, fetcher: &Fetcher, name: &str) -> Result<Updated> {
            let cwd = self.project();
            update(&self.env(fetcher, &cwd), key(name)).await
        }

        /// Run [update] on a tool whose source needs no network.
        fn update(&self, name: &str) -> Result<Updated> {
            block_on(self.update_with(&Fetcher::new(), name)).unwrap()
        }

        /// The fixture with cargo-nextest declared from its path source.
        fn declared() -> Self {
            let fixture = Self::new();
            fixture.add("tools/cargo-nextest.toml", None).unwrap();
            fixture
        }
    }

    #[test]
    fn a_new_version_is_installed_and_pinned() {
        let fixture = Fixture::declared();
        let before = fixture.manifest();
        fixture.write("tools/cargo-nextest.toml", newer());

        let updated = fixture.update("cargo-nextest").unwrap();
        assert_eq!(updated.change, Change::Updated);
        assert_eq!(
            updated.to_string(),
            "SLCR.toml: updated tool cargo-nextest 1.0.0 -> 1.1.0"
        );
        assert!(fixture.installed(&newer()));
        let expected = before
            .replace("version = \"1.0.0\"", "version = \"1.1.0\"")
            .replace(
                &Pin::of(NEXTEST.as_bytes()).to_string(),
                &Pin::of(newer().as_bytes()).to_string(),
            );
        assert_eq!(fixture.manifest(), expected);
    }

    #[test]
    fn an_edit_without_a_new_version_is_repinned() {
        let fixture = Fixture::declared();
        let edited = format!("# Ours.\n{NEXTEST}");
        fixture.write("tools/cargo-nextest.toml", &edited);

        let updated = fixture.update("cargo-nextest").unwrap();
        assert_eq!(updated.change, Change::Repinned);
        assert_eq!(
            updated.to_string(),
            "SLCR.toml: re-pinned tool cargo-nextest 1.0.0"
        );
        assert!(fixture.installed(&edited));
        assert!(
            fixture
                .manifest()
                .contains(&Pin::of(edited.as_bytes()).to_string())
        );
    }

    #[test]
    fn an_unchanged_descriptor_changes_nothing() {
        let fixture = Fixture::declared();
        let before = fixture.manifest();
        let updated = fixture.update("cargo-nextest").unwrap();
        assert_eq!(updated.change, Change::UpToDate);
        assert_eq!(
            updated.to_string(),
            "SLCR.toml: tool cargo-nextest 1.0.0 is up to date"
        );
        assert_eq!(fixture.manifest(), before);
    }

    #[test]
    fn an_unchanged_descriptor_is_reinstalled() {
        let fixture = Fixture::declared();
        std::fs::remove_file(fixture.store.path(&Pin::of(NEXTEST.as_bytes()))).unwrap();
        fixture.update("cargo-nextest").unwrap();
        assert!(fixture.installed(NEXTEST));
    }

    #[test]
    fn a_published_descriptor_is_fetched_again() {
        let fixture = Fixture::new();
        block_on(async {
            let server = HttpsServer::start(vec![("/nextest.toml", Reply::ok(newer()))]).await;
            fixture.write(
                "SLCR.toml",
                format!(
                    "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n\
                     [tools.cargo-nextest]\nversion = \"1.0.0\"\n\
                     source = {{ url = \"{}\" }}\npin = \"{}\"\n",
                    server.https("/nextest.toml"),
                    Pin::of(NEXTEST.as_bytes())
                ),
            );
            let updated = fixture
                .update_with(&server.fetcher(), "cargo-nextest")
                .await
                .unwrap();
            assert_eq!(updated.change, Change::Updated);
        })
        .unwrap();
        assert!(fixture.installed(&newer()));
        assert!(
            fixture
                .manifest()
                .contains(&Pin::of(newer().as_bytes()).to_string())
        );
    }

    #[test]
    fn the_entry_keeps_its_comments_and_layout() {
        let fixture = Fixture::new();
        let pin = Pin::of(NEXTEST.as_bytes());
        let manifest = format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n\
             # Runs the tests.\n[tools.nextest]\nversion   = \"1.0.0\" # bumped by hand?\n\
             source = {{ path = \"tools/cargo-nextest.toml\" }}\npin = \"{pin}\" # never by hand\n\n\
             [tools.zzz]\nversion = \"1.0.0\"\nsource = {{ path = \"tools/cargo-nextest.toml\" }}\n\
             pin = \"{pin}\"\n"
        );
        fixture.write("SLCR.toml", &manifest);
        fixture.write("tools/cargo-nextest.toml", newer());

        fixture.update("nextest").unwrap();
        let expected = manifest
            .replacen("version   = \"1.0.0\"", "version   = \"1.1.0\"", 1)
            .replacen(
                &pin.to_string(),
                &Pin::of(newer().as_bytes()).to_string(),
                1,
            );
        assert_eq!(fixture.manifest(), expected);
    }

    #[test]
    fn a_tool_in_an_inline_tools_table_is_repinned() {
        let fixture = Fixture::new();
        let manifest = format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\ntools = {{ cargo-nextest = \
             {{ version = \"1.0.0\", source = {{ path = \"tools/cargo-nextest.toml\" }}, \
             pin = \"{}\" }} }}\n",
            Pin::of(NEXTEST.as_bytes())
        );
        fixture.write("SLCR.toml", &manifest);
        fixture.write("tools/cargo-nextest.toml", newer());

        fixture.update("cargo-nextest").unwrap();
        let expected = manifest
            .replace("version = \"1.0.0\"", "version = \"1.1.0\"")
            .replace(
                &Pin::of(NEXTEST.as_bytes()).to_string(),
                &Pin::of(newer().as_bytes()).to_string(),
            );
        assert_eq!(fixture.manifest(), expected);
    }

    #[test]
    fn a_tool_that_is_not_declared_is_an_error() {
        let fixture = Fixture::declared();
        let err = fixture.update("jev").unwrap_err();
        let not_declared = err.downcast_ref::<NotDeclared>().unwrap();
        assert_eq!(
            not_declared.to_string(),
            "SLCR.toml declares no tool named jev"
        );
    }

    #[test]
    fn a_missing_descriptor_changes_nothing() {
        let fixture = Fixture::declared();
        let before = fixture.manifest();
        std::fs::remove_file(fixture.project().join("tools/cargo-nextest.toml")).unwrap();
        let err = fixture.update("cargo-nextest").unwrap_err();
        assert!(
            matches!(err.downcast_ref(), Some(SourceError::Missing { .. })),
            "{err:?}"
        );
        assert_eq!(fixture.manifest(), before);
    }

    #[test]
    fn an_invalid_descriptor_installs_and_pins_nothing() {
        let fixture = Fixture::declared();
        let before = fixture.manifest();
        let invalid = newer().replace("{with.test}", "{with.name}");
        fixture.write("tools/cargo-nextest.toml", &invalid);
        assert!(fixture.update("cargo-nextest").is_err());
        assert_eq!(fixture.manifest(), before);
        assert!(!fixture.installed(&invalid));
    }
}
