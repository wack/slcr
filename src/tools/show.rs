use std::{
    fmt::{self, Display},
    path::Path,
};

use miette::Result;
use serde::Serialize;
use serde_json::Value;

use super::{
    DescriptorVersion, ToolKey,
    descriptor::{Descriptor, Invoke, Results},
    manifest::{ManifestFile, Project},
    pin::Pin,
    pinned,
    source::Source,
    store::DescriptorStore,
};
use crate::fs::FileSystem;

/// The descriptor that the tool declared under `key`, in the manifest of
/// the project that holds `cwd`, an absolute path, pins, as `store` holds
/// it. Nothing is fetched: a descriptor that isn't installed, doesn't hash
/// to its pin, isn't valid, or doesn't have the version the manifest
/// declares is an error, as `slcr vet` reports it.
pub(crate) fn show(
    fs: &FileSystem,
    store: &DescriptorStore,
    cwd: &Path,
    key: ToolKey,
) -> Result<Shown> {
    let project = Project::find_from(cwd)?;
    let manifest = ManifestFile::load(&project)?;
    let entry = manifest.tool(&key)?;
    let descriptor = pinned::load(fs, store, &key, entry)?;
    if let Some(problem) = pinned::version_problem(&key, entry, &descriptor) {
        return Err(problem.into());
    }
    Ok(Shown {
        key,
        source: entry.source().clone(),
        pin: *entry.pin(),
        descriptor,
    })
}

/// What [show] found: a declared tool and its descriptor. It displays as
/// what `slcr tools show` writes, and serializes as its JSON.
#[derive(Debug)]
pub(crate) struct Shown {
    key: ToolKey,
    source: Source,
    pin: Pin,
    descriptor: Descriptor,
}

/// The JSON of a [Shown] tool: the manifest entry's fields, then the
/// descriptor's, with its validator schema last.
#[derive(Serialize)]
struct ShownJson<'a> {
    key: &'a ToolKey,
    name: &'a ToolKey,
    version: &'a DescriptorVersion,
    source: &'a Source,
    pin: &'a Pin,
    invoke: &'a Invoke,
    results: &'a Results,
    /// The JSON Schema that every validator's `with` must satisfy.
    schema: &'a Value,
}

impl Serialize for Shown {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let descriptor = &self.descriptor;
        ShownJson {
            key: &self.key,
            name: descriptor.name(),
            version: descriptor.version(),
            source: &self.source,
            pin: &self.pin,
            invoke: descriptor.invoke(),
            results: descriptor.results(),
            schema: descriptor.schema(),
        }
        .serialize(serializer)
    }
}

/// The tool as lines of fields, then its validator schema as JSON, e.g.
///
/// ```text
/// cargo-nextest 1.0.0
/// name:    cargo-nextest
/// source:  tools/cargo-nextest.toml
/// pin:     blake3:…
/// argv:    ["cargo", "nextest", "run", "-E", "test(={with.test})"]
/// cwd:     nearest Cargo.toml
/// env:     none
/// timeout: none
/// pass:    0
///
/// Each validator's `with` must satisfy this schema:
/// { … }
/// ```
impl Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let descriptor = &self.descriptor;
        let invoke = descriptor.invoke();
        // Each argument quoted as a JSON string, so that none is ambiguous.
        let argv = match serde_json::to_value(invoke.argv()) {
            Ok(Value::Array(args)) => args,
            _ => return Err(fmt::Error),
        };
        let argv: Vec<String> = argv.iter().map(ToString::to_string).collect();
        let argv = format!("[{}]", argv.join(", "));
        let env = if invoke.env().is_empty() {
            "none".to_owned()
        } else {
            let vars: Vec<String> = invoke
                .env()
                .iter()
                .map(|(name, value)| format!("{}={value:?}", name.as_str()))
                .collect();
            vars.join(" ")
        };
        let timeout = match invoke.timeout() {
            Some(seconds) if seconds.get() == 1 => "1 second".to_owned(),
            Some(seconds) => format!("{seconds} seconds"),
            None => "none".to_owned(),
        };
        let pass: Vec<String> = descriptor
            .results()
            .pass()
            .iter()
            .map(ToString::to_string)
            .collect();
        let schema = serde_json::to_string_pretty(descriptor.schema()).map_err(|_| fmt::Error)?;
        writeln!(f, "{} {}", self.key, descriptor.version())?;
        writeln!(f, "name:    {}", descriptor.name())?;
        writeln!(f, "source:  {}", self.source)?;
        writeln!(f, "pin:     {}", self.pin)?;
        writeln!(f, "argv:    {argv}")?;
        writeln!(f, "cwd:     {}", invoke.cwd())?;
        writeln!(f, "env:     {env}")?;
        writeln!(f, "timeout: {timeout}")?;
        writeln!(f, "pass:    {}", pass.join(", "))?;
        writeln!(f)?;
        writeln!(f, "Each validator's `with` must satisfy this schema:")?;
        f.write_str(&schema)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fixture::{Fixture, NEXTEST};
    use crate::tools::manifest::NotDeclared;
    use crate::tools::pinned::DescriptorProblem;

    fn key(key: &str) -> ToolKey {
        ToolKey::try_from(key.to_owned()).unwrap()
    }

    impl Fixture {
        /// Run [show] from the project root.
        fn show(&self, name: &str) -> Result<Shown> {
            show(&self.fs, &self.store, &self.project(), key(name))
        }
    }

    /// A manifest declaring cargo-nextest at `version`, pinned to
    /// `descriptor`.
    fn declaring(version: &str, descriptor: &str) -> String {
        format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n\n[tools.nextest]\nversion = \
             \"{version}\"\nsource = {{ path = \"tools/cargo-nextest.toml\" }}\npin = \"{}\"\n",
            Pin::of(descriptor.as_bytes())
        )
    }

    #[test]
    fn an_installed_descriptor_is_shown() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", declaring("1.0.0", NEXTEST));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let shown = fixture.show("nextest").unwrap();
        let pin = Pin::of(NEXTEST.as_bytes());
        assert_eq!(
            shown.to_string(),
            format!(
                "nextest 1.0.0\nname:    cargo-nextest\nsource:  tools/cargo-nextest.toml\n\
                 pin:     {pin}\nargv:    [\"cargo\", \"nextest\", \"run\", \"--no-fail-fast\", \
                 \"--no-tests=fail\", \"-E\", \"test(={{with.test}})\"]\n\
                 cwd:     nearest Cargo.toml\nenv:     none\ntimeout: none\npass:    0\n\n\
                 Each validator's `with` must satisfy this schema:\n{{\n  \
                 \"additionalProperties\": false,\n  \"properties\": {{\n    \"test\": {{\n      \
                 \"description\": \"Fully qualified test name.\",\n      \"minLength\": 1,\n      \
                 \"type\": \"string\"\n    }}\n  }},\n  \"required\": [\n    \"test\"\n  ],\n  \
                 \"type\": \"object\"\n}}"
            )
        );
        assert_eq!(
            serde_json::to_value(&shown).unwrap(),
            json!({
                "key": "nextest",
                "name": "cargo-nextest",
                "version": "1.0.0",
                "source": { "path": "tools/cargo-nextest.toml" },
                "pin": pin.to_string(),
                "invoke": {
                    "argv": ["cargo", "nextest", "run", "--no-fail-fast", "--no-tests=fail",
                             "-E", "test(={with.test})"],
                    "cwd": { "nearest": "Cargo.toml" },
                    "env": {},
                    "timeout": null
                },
                "results": { "pass": [0] },
                "schema": {
                    "type": "object",
                    "required": ["test"],
                    "additionalProperties": false,
                    "properties": {
                        "test": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Fully qualified test name."
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn env_and_timeout_are_shown() {
        let fixture = Fixture::new();
        let descriptor = NEXTEST.replace(
            "cwd = { nearest = \"Cargo.toml\" }",
            "cwd = \"project\"\nenv = { RUST_LOG = \"warn\", A = \"x y\" }\ntimeout = 1",
        );
        fixture.write("SLCR.toml", declaring("1.0.0", &descriptor));
        fixture.store.install(&fixture.fs, &descriptor).unwrap();
        let shown = fixture.show("nextest").unwrap().to_string();
        assert!(
            shown.contains(
                "cwd:     project\nenv:     A=\"x y\" RUST_LOG=\"warn\"\ntimeout: 1 second\n"
            ),
            "{shown}"
        );
        let descriptor = descriptor.replace("timeout = 1", "timeout = 600");
        fixture.write("SLCR.toml", declaring("1.0.0", &descriptor));
        fixture.store.install(&fixture.fs, &descriptor).unwrap();
        let shown = fixture.show("nextest").unwrap().to_string();
        assert!(shown.contains("timeout: 600 seconds\n"), "{shown}");
    }

    #[test]
    fn an_undeclared_tool_is_an_error() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", declaring("1.0.0", NEXTEST));
        let err = fixture.show("jev").unwrap_err();
        let not_declared = err.downcast_ref::<NotDeclared>().expect("not declared");
        assert_eq!(
            not_declared.to_string(),
            "SLCR.toml declares no tool named jev"
        );
    }

    #[test]
    fn a_descriptor_that_is_not_installed_is_an_error() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", declaring("1.0.0", NEXTEST));
        let err = fixture.show("nextest").unwrap_err();
        assert_eq!(
            err.downcast_ref::<DescriptorProblem>(),
            Some(&DescriptorProblem::DescriptorNotInstalled {
                tool: key("nextest"),
                pin: Pin::of(NEXTEST.as_bytes()),
            })
        );
        // Nothing was fetched.
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn a_version_other_than_the_descriptor_s_is_an_error() {
        let fixture = Fixture::new();
        fixture.write("SLCR.toml", declaring("0.9.0", NEXTEST));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let err = fixture.show("nextest").unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<DescriptorProblem>(),
                Some(DescriptorProblem::DescriptorVersionMismatch { .. })
            ),
            "{err:?}"
        );
    }
}
