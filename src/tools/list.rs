use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use console::measure_text_width;
use miette::Result;
use serde::Serialize;

use super::{
    DescriptorVersion, ToolKey,
    manifest::{ManifestFile, Project},
    pin::Pin,
    source::Source,
    store::{DescriptorStore, Unavailable},
};
use crate::fs::FileSystem;
use crate::project::problem::display;

/// Every tool the manifest of the project that holds `cwd`, an absolute
/// path, declares, in the order of their keys, and whether `store` holds
/// the descriptor each pins. Nothing is fetched.
pub(crate) fn list(fs: &FileSystem, store: &DescriptorStore, cwd: &Path) -> Result<Listing> {
    let project = Project::find_from(cwd)?;
    let manifest = ManifestFile::load(&project)?;
    let tools = manifest
        .manifest()
        .tools()
        .iter()
        .map(|(key, entry)| Listed {
            key: key.clone(),
            version: entry.version().clone(),
            source: entry.source().clone(),
            pin: *entry.pin(),
            descriptor: match store.load(fs, entry.pin()) {
                Ok(_) => Installation::Installed,
                Err(Unavailable::Missing) => Installation::NotInstalled,
                Err(Unavailable::Corrupted) => Installation::Corrupted,
                Err(Unavailable::Unreadable(_)) => Installation::Unreadable,
                Err(Unavailable::Invalid(_)) => Installation::Invalid,
            },
        })
        .collect();
    Ok(Listing {
        manifest: project.shown_manifest().to_path_buf(),
        tools,
    })
}

/// What [list] found: the tools a manifest declares. It displays as the
/// table `slcr tools list` writes, and serializes as its JSON.
#[derive(Debug, Serialize)]
pub(crate) struct Listing {
    /// The manifest's path relative to the working directory.
    #[serde(serialize_with = "display")]
    manifest: PathBuf,
    tools: Vec<Listed>,
}

/// One declared tool.
#[derive(Debug, Serialize)]
pub(crate) struct Listed {
    key: ToolKey,
    version: DescriptorVersion,
    source: Source,
    pin: Pin,
    /// Whether the descriptor store holds the descriptor the pin names.
    descriptor: Installation,
}

/// The state of the descriptor a tool pins, in the descriptor store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Installation {
    /// It is installed, and it is a valid descriptor.
    Installed,
    NotInstalled,
    /// What is installed doesn't hash to the pin.
    Corrupted,
    Unreadable,
    /// What is installed hashes to the pin, but isn't a valid descriptor.
    Invalid,
}

impl Display for Installation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Installed => "installed",
            Self::NotInstalled => "not installed",
            Self::Corrupted => "corrupted",
            Self::Unreadable => "unreadable",
            Self::Invalid => "invalid",
        })
    }
}

/// The tools as aligned columns under a header and a rule, e.g.
///
/// ```text
/// name           version  descriptor  source                    pin
/// -------------  -------  ----------  ------------------------  ------
/// cargo-nextest  1.0.0    installed   tools/cargo-nextest.toml  blake3:…
/// ```
///
/// or a line saying there are none.
impl Display for Listing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.tools.is_empty() {
            return write!(
                f,
                "{} declares no tools; declare one with `slcr tools add <SOURCE>`",
                self.manifest.display()
            );
        }
        let header = ["name", "version", "descriptor", "source", "pin"].map(str::to_owned);
        let rows: Vec<[String; 5]> = self
            .tools
            .iter()
            .map(|tool| {
                [
                    tool.key.to_string(),
                    tool.version.to_string(),
                    tool.descriptor.to_string(),
                    tool.source.to_string(),
                    tool.pin.to_string(),
                ]
            })
            .collect();
        let widths: Vec<usize> = (0..header.len())
            .map(|i| {
                rows.iter()
                    .chain([&header])
                    .map(|row| measure_text_width(&row[i]))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |cells: &[String]| {
            let padded: Vec<String> = cells
                .iter()
                .zip(&widths)
                .map(|(cell, &width)| {
                    let padding = width - measure_text_width(cell);
                    format!("{cell}{}", " ".repeat(padding))
                })
                .collect();
            padded.join("  ").trim_end().to_owned()
        };
        let rule: Vec<String> = widths.iter().map(|&width| "-".repeat(width)).collect();
        let mut lines = vec![line(&header), line(&rule)];
        lines.extend(rows.iter().map(|row| line(row)));
        f.write_str(&lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fixture::{Fixture, NEXTEST};

    impl Fixture {
        /// Run [list] from the project root.
        fn list(&self) -> Listing {
            list(&self.fs, &self.store, &self.project()).unwrap()
        }
    }

    /// A manifest declaring cargo-nextest and jev, each pinned to the hash
    /// of its own key's descriptor.
    fn declaring(nextest: &str) -> String {
        format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n[tools.jev]\nversion = \
             \"2.0.0\"\nsource = {{ url = \"https://example.com/jev.toml\" }}\npin = \"{}\"\n\n\
             [tools.cargo-nextest]\nversion = \"1.0.0\"\nsource = {{ path = \
             \"tools/cargo-nextest.toml\" }}\npin = \"{}\"\n",
            Pin::of(b"jev"),
            Pin::of(nextest.as_bytes())
        )
    }

    #[test]
    fn every_declared_tool_is_listed_in_key_order() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", declaring(NEXTEST));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let listing = fixture.list();
        assert_eq!(
            listing.to_string(),
            format!(
                "name           version  descriptor     source                        pin\n\
                 -------------  -------  -------------  ----------------------------  \
                 -----------------------------------------------------------------------\n\
                 cargo-nextest  1.0.0    installed      tools/cargo-nextest.toml      {}\n\
                 jev            2.0.0    not installed  https://example.com/jev.toml  {}",
                Pin::of(NEXTEST.as_bytes()),
                Pin::of(b"jev")
            )
        );
        assert_eq!(
            serde_json::to_value(&listing).unwrap(),
            json!({
                "manifest": "SLCR.toml",
                "tools": [
                    {
                        "key": "cargo-nextest",
                        "version": "1.0.0",
                        "source": { "path": "tools/cargo-nextest.toml" },
                        "pin": Pin::of(NEXTEST.as_bytes()).to_string(),
                        "descriptor": "installed"
                    },
                    {
                        "key": "jev",
                        "version": "2.0.0",
                        "source": { "url": "https://example.com/jev.toml" },
                        "pin": Pin::of(b"jev").to_string(),
                        "descriptor": "not-installed"
                    }
                ]
            })
        );
    }

    #[test]
    fn the_state_of_each_descriptor_is_reported() {
        let fixture = Fixture::new();
        let states = |fixture: &Fixture| -> Vec<Installation> {
            fixture
                .list()
                .tools
                .iter()
                .map(|tool| tool.descriptor)
                .collect()
        };

        let corrupted = NEXTEST.replace("1.0.0", "1.0.1");
        fixture.write("SLCR.toml", declaring(&corrupted));
        std::fs::create_dir_all(fixture.store.path(&Pin::of(b"jev"))).unwrap();
        let pin = Pin::of(corrupted.as_bytes());
        std::fs::create_dir_all(fixture.store.path(&pin).parent().unwrap()).unwrap();
        std::fs::write(fixture.store.path(&pin), NEXTEST).unwrap();
        assert_eq!(
            states(&fixture),
            [Installation::Corrupted, Installation::Unreadable]
        );

        let invalid = "name = \"cargo-nextest\"\n";
        fixture.write("SLCR.toml", declaring(invalid));
        fixture.store.install(&fixture.fs, invalid).unwrap();
        assert_eq!(states(&fixture)[0], Installation::Invalid);
    }

    #[test]
    fn a_manifest_without_tools_says_so() {
        let fixture = Fixture::new();
        let listing = fixture.list();
        assert_eq!(
            listing.to_string(),
            "SLCR.toml declares no tools; declare one with `slcr tools add <SOURCE>`"
        );
        assert_eq!(
            serde_json::to_value(&listing).unwrap(),
            json!({ "manifest": "SLCR.toml", "tools": [] })
        );
    }

    #[test]
    fn the_manifest_is_named_relative_to_the_working_directory() {
        let fixture = Fixture::new();
        let cwd = fixture.project().join("crates/api");
        std::fs::create_dir_all(&cwd).unwrap();
        let listing = list(&fixture.fs, &fixture.store, &cwd).unwrap();
        assert_eq!(listing.manifest, Path::new("../../SLCR.toml"));
    }
}
