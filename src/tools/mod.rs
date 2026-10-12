//! External tools: the project manifest that declares them, and the
//! descriptors that say how to run them.
//!
//! The formats are defined by the manifest and descriptor JSON Schemas on
//! the SLCR Schemas page in Notion, and the rules around them on the "SLCR
//! — Tools: Manifest, Descriptors, and Invocation" page.

use std::{
    fmt::{self, Display},
    path::Path,
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fs::FileSystem;
use crate::spec::text::is_slug;

/// `slcr tools add`: declaring a descriptor in the manifest.
pub(crate) mod add;
/// What a descriptor declares, validated against its schema.
pub(crate) mod descriptor;
/// `slcr tools install`: installing every pinned descriptor the store lacks.
pub(crate) mod install;
/// `slcr tools list`: the declared tools, and whether each is installed.
pub(crate) mod list;
/// `SLCR.toml`: the project manifest, and finding the project root.
pub(crate) mod manifest;
/// The BLAKE3 pin of a descriptor's bytes.
pub(crate) mod pin;
/// Checking a declared tool's descriptor, as installed, against its pin.
pub(crate) mod pinned;
/// `slcr tools remove`: removing a tool from the manifest.
pub(crate) mod remove;
/// `slcr tools show`: a declared tool's pinned descriptor.
pub(crate) mod show;
/// Where a manifest entry's descriptor comes from, and fetching it.
pub(crate) mod source;
/// The content-addressed descriptor store under `$XDG_DATA_HOME`.
pub(crate) mod store;
/// `slcr tools update`: re-fetching and re-pinning a declared descriptor.
pub(crate) mod update;

/// A project with a descriptor in it, for tests of the commands.
#[cfg(test)]
mod fixture;
/// A local HTTPS server for tests of fetching descriptors.
#[cfg(test)]
mod https_server;

/// What the `slcr tools` commands read and write outside the project,
/// passed in so that a caller, such as a test, chooses it.
pub(crate) struct Environment<'a> {
    pub(crate) fs: &'a FileSystem,
    pub(crate) store: &'a store::DescriptorStore,
    pub(crate) fetcher: &'a source::Fetcher,
    /// The working directory, as an absolute path: where the project is
    /// looked for, and what a path source is relative to.
    pub(crate) cwd: &'a Path,
}

/// The key a descriptor is declared under in the manifest's `[tools]`
/// table, such as `cargo-nextest`, which validators use to refer to it.
/// Descriptor names take the same form, and a key defaults to its
/// descriptor's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ToolKey(String);

/// The error returned when a string is not a valid [ToolKey].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{0:?} is not a valid tool name")]
#[diagnostic(help(
    "a tool name is lowercase letters and digits in dash-separated words, e.g. `cargo-nextest`"
))]
pub struct InvalidToolKey(String);

impl ToolKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ToolKey {
    type Error = InvalidToolKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_slug(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidToolKey(value))
        }
    }
}

impl From<ToolKey> for String {
    fn from(key: ToolKey) -> Self {
        key.0
    }
}

impl Display for ToolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A descriptor's SemVer 2.0.0 version, such as `1.2.0`: the version of
/// the descriptor, not of the tool it runs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DescriptorVersion(semver::Version);

/// The error returned when a string is not a valid [DescriptorVersion].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{0:?} is not a valid SemVer version")]
#[diagnostic(help("a descriptor's version is a SemVer 2.0.0 version, e.g. `1.2.0`"))]
pub struct InvalidVersion(String);

impl TryFrom<String> for DescriptorVersion {
    type Error = InvalidVersion;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // `semver` rejects leading zeros, empty identifiers, and anything
        // else SemVer 2.0.0 does, so a parsed version displays exactly as
        // it was written.
        semver::Version::parse(&value)
            .map(Self)
            .map_err(|_| InvalidVersion(value))
    }
}

impl From<DescriptorVersion> for String {
    fn from(version: DescriptorVersion) -> Self {
        version.to_string()
    }
}

impl Display for DescriptorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_keys_are_slugs() {
        for valid in ["cargo-nextest", "jev", "ast-grep2", "a-1-b"] {
            assert_eq!(ToolKey::try_from(valid.to_owned()).unwrap().as_str(), valid);
        }
        for invalid in ["", "Jev", "cargo_nextest", "-jev", "jev-", "a--b", "a b"] {
            assert_eq!(
                ToolKey::try_from(invalid.to_owned()),
                Err(InvalidToolKey(invalid.to_owned()))
            );
        }
    }

    #[test]
    fn descriptor_versions_are_semver() {
        for valid in ["1.0.0", "0.3.1", "1.2.3-rc.1+build.5", "10.20.30"] {
            let version = DescriptorVersion::try_from(valid.to_owned()).unwrap();
            assert_eq!(version.to_string(), valid);
        }
        for invalid in ["1", "1.0", "01.0.0", "1.0.0-", "v1.0.0", "1.0.0-01", ""] {
            assert_eq!(
                DescriptorVersion::try_from(invalid.to_owned()),
                Err(InvalidVersion(invalid.to_owned())),
                "{invalid}"
            );
        }
    }
}
