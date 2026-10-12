use std::collections::BTreeMap;

use miette::Diagnostic;
use serde::{Serialize, Serializer};
use thiserror::Error;

use super::{
    DescriptorVersion, ToolKey,
    descriptor::{Descriptor, InvalidDescriptor},
    manifest::ToolEntry,
    pin::Pin,
    store::{DescriptorStore, Unavailable},
};
use crate::fs::FileSystem;

/// What can be wrong with the descriptor a declared tool pins, as far as
/// can be told without fetching anything: the manifest schema's
/// `x-vetInvariants` about pins.
///
/// It serializes like a [crate::project::Problem]: an object whose `code`
/// names the problem in kebab-case, alongside the variant's fields.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq, Serialize)]
#[serde(
    tag = "code",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum DescriptorProblem {
    #[error("the descriptor tool {tool} pins is not installed")]
    #[diagnostic(help(
        "`slcr tools update {tool}` fetches it from its source and installs it, re-pinning the \
         tool if the source serves other bytes now"
    ))]
    DescriptorNotInstalled { tool: ToolKey, pin: Pin },

    #[error("the installed descriptor of tool {tool} does not hash to its pin")]
    #[diagnostic(help(
        "the descriptor store's copy changed after it was installed; `slcr tools update {tool}` \
         fetches it from its source and installs it again"
    ))]
    DescriptorCorrupted { tool: ToolKey, pin: Pin },

    #[error("could not read the installed descriptor of tool {tool}: {reason}")]
    DescriptorUnreadable {
        tool: ToolKey,
        pin: Pin,
        reason: String,
    },

    #[error("the installed descriptor of tool {tool} is not a valid descriptor")]
    #[diagnostic(help(
        "`slcr tools update {tool}` fetches its source again, which must serve a valid descriptor"
    ))]
    InvalidDescriptor {
        tool: ToolKey,
        pin: Pin,
        /// What is wrong with it: in JSON, each message, outermost first.
        #[source]
        #[diagnostic_source]
        #[serde(rename = "problems", serialize_with = "messages")]
        error: InvalidDescriptor,
    },

    #[error(
        "tool {tool} is declared as version {declared}, but the descriptor it pins is version \
         {actual}"
    )]
    #[diagnostic(help(
        "a tool's version is copied from its descriptor; `slcr tools update {tool}` corrects it"
    ))]
    DescriptorVersionMismatch {
        tool: ToolKey,
        declared: DescriptorVersion,
        actual: DescriptorVersion,
    },
}

/// Serialize an invalid descriptor's error as its messages, outermost
/// first: the error's own, then each problem's or the parse error's.
fn messages<S: Serializer>(error: &InvalidDescriptor, serializer: S) -> Result<S::Ok, S::Error> {
    let mut messages = vec![error.to_string()];
    match error {
        InvalidDescriptor::Broken { problems, .. } => {
            messages.extend(problems.iter().map(ToString::to_string));
        }
        InvalidDescriptor::Unparsable(parse) if parse.span().is_some() => {
            messages.push(parse.message().to_owned());
        }
        _ => {}
    }
    serializer.collect_seq(messages)
}

/// The descriptor that `entry`, declared under `key`, pins, from `store`,
/// whose copy must hash to the pin and be a valid descriptor. Nothing is
/// ever fetched.
pub(crate) fn load(
    fs: &FileSystem,
    store: &DescriptorStore,
    key: &ToolKey,
    entry: &ToolEntry,
) -> Result<Descriptor, DescriptorProblem> {
    let tool = key.clone();
    let pin = *entry.pin();
    store
        .load(fs, &pin)
        .map_err(|unavailable| match unavailable {
            Unavailable::Missing => DescriptorProblem::DescriptorNotInstalled { tool, pin },
            Unavailable::Corrupted => DescriptorProblem::DescriptorCorrupted { tool, pin },
            Unavailable::Unreadable(reason) => {
                DescriptorProblem::DescriptorUnreadable { tool, pin, reason }
            }
            Unavailable::Invalid(error) => {
                DescriptorProblem::InvalidDescriptor { tool, pin, error }
            }
        })
}

/// The problem with `entry`, declared under `key`, if the version it
/// declares isn't that of `descriptor`, the one it pins.
pub(crate) fn version_problem(
    key: &ToolKey,
    entry: &ToolEntry,
    descriptor: &Descriptor,
) -> Option<DescriptorProblem> {
    (entry.version() != descriptor.version()).then(|| {
        DescriptorProblem::DescriptorVersionMismatch {
            tool: key.clone(),
            declared: entry.version().clone(),
            actual: descriptor.version().clone(),
        }
    })
}

/// The descriptor each of `tools` pins, if it can be loaded, and every
/// problem with them, in the order of their keys.
pub(crate) fn load_all(
    fs: &FileSystem,
    store: &DescriptorStore,
    tools: &BTreeMap<ToolKey, ToolEntry>,
) -> (BTreeMap<ToolKey, Descriptor>, Vec<DescriptorProblem>) {
    let mut descriptors = BTreeMap::new();
    let mut problems = Vec::new();
    for (key, entry) in tools {
        match load(fs, store, key, entry) {
            Ok(descriptor) => {
                problems.extend(version_problem(key, entry, &descriptor));
                descriptors.insert(key.clone(), descriptor);
            }
            Err(problem) => problems.push(problem),
        }
    }
    (descriptors, problems)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::terminal::render_plain;
    use crate::tools::fixture::{Fixture, NEXTEST};
    use crate::tools::manifest::{ManifestFile, Project};

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    /// A manifest that declares cargo-nextest at `version`, pinned to
    /// `text`.
    fn manifest(version: &str, text: &str) -> String {
        format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n[tools.cargo-nextest]\n\
             version = \"{version}\"\nsource = {{ path = \"tools/cargo-nextest.toml\" }}\n\
             pin = \"{}\"\n",
            Pin::of(text.as_bytes())
        )
    }

    impl Fixture {
        /// Every descriptor problem in the fixture's project.
        fn descriptors(&self) -> (BTreeMap<ToolKey, Descriptor>, Vec<DescriptorProblem>) {
            let project = Project::find_from(&self.project()).unwrap();
            let file = ManifestFile::load(&project).unwrap();
            load_all(&self.fs, &self.store, file.manifest().tools())
        }
    }

    #[test]
    fn an_installed_descriptor_loads() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", manifest("1.0.0", NEXTEST));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let (descriptors, problems) = fixture.descriptors();
        assert_eq!(problems, []);
        let descriptor = &descriptors[&key("cargo-nextest")];
        assert_eq!(descriptor.name().as_str(), "cargo-nextest");
    }

    #[test]
    fn a_missing_descriptor_is_a_problem() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", manifest("1.0.0", NEXTEST));
        let (descriptors, problems) = fixture.descriptors();
        assert!(descriptors.is_empty());
        assert_eq!(
            problems,
            [DescriptorProblem::DescriptorNotInstalled {
                tool: key("cargo-nextest"),
                pin: Pin::of(NEXTEST.as_bytes()),
            }]
        );
        assert_eq!(
            render_plain(&problems[0]),
            "  × the descriptor tool cargo-nextest pins is not installed\n  help: `slcr tools \
             update cargo-nextest` fetches it from its source and\n        installs it, \
             re-pinning the tool if the source serves other bytes now\n"
        );
    }

    #[test]
    fn a_corrupted_descriptor_is_a_problem() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", manifest("1.0.0", NEXTEST));
        let pin = fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        std::fs::write(fixture.store.path(&pin), NEXTEST.replace("1.0.0", "6.6.6")).unwrap();
        let (descriptors, problems) = fixture.descriptors();
        assert!(descriptors.is_empty());
        assert_eq!(
            problems,
            [DescriptorProblem::DescriptorCorrupted {
                tool: key("cargo-nextest"),
                pin,
            }]
        );
    }

    #[test]
    fn an_unreadable_descriptor_is_a_problem() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", manifest("1.0.0", NEXTEST));
        let pin = Pin::of(NEXTEST.as_bytes());
        std::fs::create_dir_all(fixture.store.path(&pin)).unwrap();
        let (_, problems) = fixture.descriptors();
        let [DescriptorProblem::DescriptorUnreadable { reason, .. }] = &problems[..] else {
            panic!("{problems:?}");
        };
        assert!(
            !reason.contains(&*fixture.project().to_string_lossy()),
            "{reason}"
        );
    }

    #[test]
    fn an_invalid_descriptor_is_a_problem() {
        let fixture = Fixture::new();
        let invalid = NEXTEST.replace("{with.test}", "{with.name}");
        fixture.write("SLCR.toml", manifest("1.0.0", &invalid));
        fixture.store.install(&fixture.fs, &invalid).unwrap();
        let (descriptors, problems) = fixture.descriptors();
        assert!(descriptors.is_empty());
        let [problem @ DescriptorProblem::InvalidDescriptor { .. }] = &problems[..] else {
            panic!("{problems:?}");
        };
        let pin = Pin::of(invalid.as_bytes());
        assert_eq!(
            serde_json::to_value(problem).unwrap(),
            json!({
                "code": "invalid-descriptor",
                "tool": "cargo-nextest",
                "pin": pin.to_string(),
                "problems": [
                    format!("{} is not a valid descriptor", pin.abbreviated()),
                    "`{with.name}` in `invoke.argv` does not name a field of `validator.schema`"
                ]
            })
        );
    }

    #[test]
    fn an_unparsable_descriptor_is_labeled() {
        let fixture = Fixture::new();
        let invalid = NEXTEST.replace("[invoke]", "[invoke]\nshell = true");
        fixture.write("SLCR.toml", manifest("1.0.0", &invalid));
        fixture.store.install(&fixture.fs, &invalid).unwrap();
        let (_, problems) = fixture.descriptors();
        let pin = Pin::of(invalid.as_bytes());
        let rendered = render_plain(&problems[0]);
        assert!(
            rendered.starts_with(
                "  × the installed descriptor of tool cargo-nextest is not a valid descriptor\n  \
                 ╰─▶   × could not parse blake3:"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("╭─[{}:12:1]", pin.abbreviated())),
            "{rendered}"
        );
        assert!(rendered.contains("12 │ shell = true"), "{rendered}");
        assert!(rendered.contains("unknown field `shell`"), "{rendered}");
    }

    #[test]
    fn a_version_other_than_the_descriptor_s_is_a_problem() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", manifest("0.9.0", NEXTEST));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let (descriptors, problems) = fixture.descriptors();
        // The descriptor still loads: its validators can be checked.
        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "tool cargo-nextest is declared as version 0.9.0, but the descriptor it pins is \
                 version 1.0.0"
            ]
        );
        assert_eq!(
            serde_json::to_value(&problems[0]).unwrap(),
            json!({
                "code": "descriptor-version-mismatch",
                "tool": "cargo-nextest",
                "declared": "0.9.0",
                "actual": "1.0.0"
            })
        );
    }

    #[test]
    fn every_tool_is_checked() {
        let fixture = Fixture::new();
        let pin = Pin::of(b"missing");
        fixture.write(
            "SLCR.toml",
            format!(
                "{}\n[tools.a]\nversion = \"1.0.0\"\nsource = {{ path = \"a.toml\" }}\npin = \
                 \"{pin}\"\n\n[tools.z]\nversion = \"1.0.0\"\nsource = {{ path = \"z.toml\" }}\n\
                 pin = \"{pin}\"\n",
                manifest("1.0.0", NEXTEST)
            ),
        );
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let (descriptors, problems) = fixture.descriptors();
        assert_eq!(descriptors.len(), 1);
        let tools: Vec<String> = problems
            .iter()
            .map(|problem| match problem {
                DescriptorProblem::DescriptorNotInstalled { tool, .. } => tool.to_string(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(tools, ["a", "z"]);
    }
}
