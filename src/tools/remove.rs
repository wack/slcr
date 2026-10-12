use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use miette::Result;

use super::{
    DescriptorVersion, ToolKey,
    manifest::{ManifestFile, Project},
};
use crate::fs::FileSystem;

/// Remove the tool declared under `key` from the manifest of the project
/// that holds `cwd`, an absolute path.
///
/// Its descriptor stays in the descriptor store, which other projects on
/// the machine may pin it from.
pub(crate) fn remove(fs: &FileSystem, cwd: &Path, key: ToolKey) -> Result<Removed> {
    let project = Project::find_from(cwd)?;
    let mut manifest = ManifestFile::load(&project)?;
    let entry = manifest.remove_tool(&key)?;
    manifest.save(fs)?;
    Ok(Removed {
        manifest: project.shown_manifest().to_path_buf(),
        key,
        version: entry.version().clone(),
    })
}

/// What [remove] did, which displays as the status `slcr tools remove`
/// reports, e.g. `SLCR.toml: removed tool cargo-nextest 1.0.0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removed {
    /// The manifest's path relative to the working directory.
    manifest: PathBuf,
    key: ToolKey,
    version: DescriptorVersion,
}

impl Display for Removed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            manifest,
            key,
            version,
        } = self;
        write!(f, "{}: removed tool {key} {version}", manifest.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::fixture::{Fixture, MANIFEST, NEXTEST};
    use crate::tools::manifest::NotDeclared;
    use crate::tools::pin::Pin;

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    impl Fixture {
        /// Run [remove] from the project root.
        fn remove(&self, name: &str) -> Result<Removed> {
            remove(&self.fs, &self.project(), key(name))
        }
    }

    #[test]
    fn the_last_tool_takes_the_tools_table_with_it() {
        let fixture = Fixture::new();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        let removed = fixture.remove("cargo-nextest").unwrap();
        assert_eq!(
            removed.to_string(),
            "SLCR.toml: removed tool cargo-nextest 1.0.0"
        );
        // `add` added a blank line before the entry, and it goes with it.
        assert_eq!(fixture.manifest(), MANIFEST);
    }

    #[test]
    fn the_rest_of_the_file_is_untouched() {
        let fixture = Fixture::new();
        let pin = Pin::of(NEXTEST.as_bytes());
        let entry = |name: &str| {
            format!(
                "[tools.{name}]\nversion = \"1.0.0\"\n\
                 source = {{ path = \"tools/cargo-nextest.toml\" }}\npin = \"{pin}\" # pinned\n"
            )
        };
        let head = "# The project.\nformatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n";
        fixture.write(
            "SLCR.toml",
            format!(
                "{head}\n# First.\n{}\n# Second.\n{}\n# Third.\n{}",
                entry("a"),
                entry("b"),
                entry("c")
            ),
        );
        fixture.remove("b").unwrap();
        assert_eq!(
            fixture.manifest(),
            format!("{head}\n# First.\n{}\n# Third.\n{}", entry("a"), entry("c"))
        );
    }

    #[test]
    fn an_explicit_tools_table_is_removed_when_it_is_left_empty() {
        let fixture = Fixture::new();
        fixture.write(
            "SLCR.toml",
            format!(
                "{MANIFEST}\n[tools]\njev = {{ version = \"1.0.0\", source = {{ path = \"jev.toml\" }}, \
                 pin = \"{}\" }}\n",
                Pin::of(b"jev")
            ),
        );
        fixture.remove("jev").unwrap();
        // The blank line before `[tools]` goes with it.
        assert_eq!(fixture.manifest(), MANIFEST);
    }

    #[test]
    fn a_tool_leaves_an_inline_tools_table() {
        let fixture = Fixture::new();
        let tool = |name: &str| {
            format!(
                "{name} = {{ version = \"1.0.0\", source = {{ path = \"{name}.toml\" }}, pin = \"{}\" }}",
                Pin::of(name.as_bytes())
            )
        };
        fixture.write(
            "SLCR.toml",
            format!(
                "{MANIFEST}tools = {{ {}, {} }}\n",
                tool("curl"),
                tool("jev")
            ),
        );
        fixture.remove("curl").unwrap();
        assert_eq!(
            fixture.manifest(),
            format!("{MANIFEST}tools = {{ {} }}\n", tool("jev"))
        );
        fixture.remove("jev").unwrap();
        assert_eq!(fixture.manifest(), MANIFEST);
    }

    #[test]
    fn the_descriptor_stays_installed() {
        let fixture = Fixture::new();
        fixture.add("tools/cargo-nextest.toml", None).unwrap();
        fixture.remove("cargo-nextest").unwrap();
        assert!(fixture.installed(NEXTEST));
    }

    #[test]
    fn a_tool_that_is_not_declared_is_an_error() {
        let fixture = Fixture::new();
        let err = fixture.remove("cargo-nextest").unwrap_err();
        let not_declared = err.downcast_ref::<NotDeclared>().unwrap();
        assert_eq!(
            not_declared.to_string(),
            "SLCR.toml declares no tool named cargo-nextest"
        );
        assert_eq!(fixture.manifest(), MANIFEST);
    }
}
