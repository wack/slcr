use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use miette::{Diagnostic, Report, Result};
use thiserror::Error;

use super::{
    Environment, ToolKey,
    descriptor::{self, Descriptor, InvalidDescriptor},
    manifest::{ManifestFile, Project, ToolEntry},
    pin::Pin,
    pinned,
    source::SourceError,
    store::Unavailable,
};

/// Install every descriptor the manifest's tools pin that the descriptor
/// store doesn't hold, in the order of their keys: fetch each from its
/// recorded source, check that its bytes hash to the pin and that it is a
/// valid descriptor of the version the manifest declares, and only then
/// install it, replacing a corrupted or unreadable copy.
///
/// It prepares a fresh clone or an offline sandbox, as `cargo fetch` does,
/// and never writes the manifest: a source that serves other bytes now is
/// an error, never a reason to re-pin. A tool that fails doesn't stop the
/// rest; only a project or manifest that can't be read is an error here.
pub(crate) async fn install(env: &Environment<'_>) -> Result<Installed> {
    let project = Project::find_from(env.cwd)?;
    let manifest = ManifestFile::load(&project)?;
    let mut tools = Vec::new();
    for (key, entry) in manifest.manifest().tools() {
        let outcome = install_tool(env, project.root(), key, entry).await;
        tools.push(ToolInstall {
            key: key.clone(),
            outcome,
        });
    }
    Ok(Installed {
        manifest: project.shown_manifest().to_path_buf(),
        tools,
    })
}

/// Install the descriptor `entry`, declared under `key` in the project at
/// `root`, pins, unless the store already holds it.
async fn install_tool(
    env: &Environment<'_>,
    root: &Path,
    key: &ToolKey,
    entry: &ToolEntry,
) -> Result<Status> {
    let pin = *entry.pin();
    let replaced = match env.store.load(env.fs, &pin) {
        Ok(descriptor) => {
            return match pinned::version_problem(key, entry, &descriptor) {
                Some(problem) => Err(problem.into()),
                None => Ok(Status::AlreadyInstalled),
            };
        }
        // The bytes are the pinned ones, so fetching them again can't help.
        Err(Unavailable::Invalid(error)) => return Err(invalid(key, entry, error)),
        Err(Unavailable::Missing) => None,
        Err(Unavailable::Corrupted) => Some(Damage::Corrupted),
        Err(Unavailable::Unreadable(_)) => Some(Damage::Unreadable),
    };

    let source = entry.source();
    let origin = source.to_string();
    tracing::debug!(%origin, "fetching a descriptor");
    let bytes = source
        .fetch(env.fs, env.fetcher, root)
        .await
        .map_err(|error| InstallError::Unfetchable {
            tool: key.clone(),
            error,
        })?;
    if !pin.matches(&bytes) {
        return Err(InstallError::Changed {
            tool: key.clone(),
            origin,
            pin,
            served: Pin::of(&bytes),
        }
        .into());
    }
    let (text, descriptor) = descriptor::text(&origin, &bytes)
        .and_then(|text| Ok((text, Descriptor::parse(&origin, text)?)))
        .map_err(|error| invalid(key, entry, error))?;
    if let Some(problem) = pinned::version_problem(key, entry, &descriptor) {
        return Err(problem.into());
    }
    env.store.install(env.fs, text)?;
    Ok(Status::Installed { pin, replaced })
}

/// The error for the descriptor `entry`, declared under `key`, pins, which
/// isn't a valid descriptor.
fn invalid(key: &ToolKey, entry: &ToolEntry, error: InvalidDescriptor) -> Report {
    InstallError::Invalid {
        tool: key.clone(),
        origin: entry.source().to_string(),
        error,
    }
    .into()
}

/// Why [install] could not install one tool's descriptor. A descriptor
/// whose version isn't the one the manifest declares is reported as
/// `slcr vet` reports it, as a [pinned::DescriptorProblem].
#[derive(Debug, Error, Diagnostic)]
pub enum InstallError {
    #[error("could not fetch the descriptor tool {tool} pins")]
    Unfetchable {
        tool: ToolKey,
        #[source]
        #[diagnostic_source]
        error: SourceError,
    },

    #[error("{origin} no longer serves the descriptor tool {tool} pins")]
    #[diagnostic(help(
        "it serves {}, not {}: restore the pinned descriptor there, or re-pin the tool to what \
         it serves with `slcr tools update {tool}`, which changes SLCR.toml",
        .served.abbreviated(),
        .pin.abbreviated()
    ))]
    Changed {
        tool: ToolKey,
        /// The source, as the manifest records it.
        origin: String,
        pin: Pin,
        /// The pin of what the source serves.
        served: Pin,
    },

    #[error("the descriptor tool {tool} pins is not a valid descriptor")]
    #[diagnostic(help(
        "no other bytes match the pin; make {origin} serve a valid descriptor, then re-pin the \
         tool to it with `slcr tools update {tool}`"
    ))]
    Invalid {
        tool: ToolKey,
        /// The source, as the manifest records it.
        origin: String,
        #[source]
        #[diagnostic_source]
        error: InvalidDescriptor,
    },
}

/// What [install] did with each declared tool. It displays as the status
/// lines `slcr tools install` writes, one per tool, e.g.
/// `cargo-nextest: installed blake3:af1349b9f5f9…`.
#[derive(Debug)]
pub(crate) struct Installed {
    /// The manifest's path relative to the working directory.
    manifest: PathBuf,
    tools: Vec<ToolInstall>,
}

/// One declared tool, and what installing its descriptor did, or why it
/// failed.
#[derive(Debug)]
struct ToolInstall {
    key: ToolKey,
    outcome: Result<Status>,
}

/// What installing one tool's descriptor did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// The store already held a valid descriptor for the pin.
    AlreadyInstalled,
    /// It fetched the pinned descriptor and installed it, replacing a
    /// damaged copy if the store held one.
    Installed { pin: Pin, replaced: Option<Damage> },
}

/// What was wrong with the copy of a descriptor the store held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Damage {
    /// It didn't hash to its pin.
    Corrupted,
    /// It couldn't be read.
    Unreadable,
}

/// The error `slcr tools install` fails with when any tool fails.
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("installing failed for {failed} of {total} {}", if *.total == 1 { "tool" } else { "tools" })]
pub struct InstallFailed {
    failed: usize,
    total: usize,
}

impl Installed {
    /// Each declared tool, and what installing its descriptor did or why it
    /// failed, in the order of their keys.
    pub(crate) fn tools(&self) -> impl Iterator<Item = (&ToolKey, &Result<Status>)> {
        self.tools.iter().map(|tool| (&tool.key, &tool.outcome))
    }

    /// The status lines of the tools whose descriptor was fetched, or
    /// failed to be: every line [Installed] displays, but those of tools
    /// already installed.
    pub(crate) fn changes(&self) -> impl Iterator<Item = String> {
        self.tools
            .iter()
            .filter(|tool| !matches!(tool.outcome, Ok(Status::AlreadyInstalled)))
            .map(ToString::to_string)
    }

    /// Why each tool that failed did, in the order of their keys.
    pub(crate) fn errors(&self) -> impl Iterator<Item = &Report> {
        self.tools
            .iter()
            .filter_map(|tool| tool.outcome.as_ref().err())
    }

    /// Success, unless a tool failed.
    pub(crate) fn verdict(&self) -> Result<(), InstallFailed> {
        let failed = self.errors().count();
        if failed == 0 {
            Ok(())
        } else {
            Err(InstallFailed {
                failed,
                total: self.tools.len(),
            })
        }
    }
}

impl Display for Installed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.tools.is_empty() {
            return write!(
                f,
                "{} declares no tools, so there is nothing to install",
                self.manifest.display()
            );
        }
        let lines: Vec<String> = self.tools.iter().map(ToString::to_string).collect();
        f.write_str(&lines.join("\n"))
    }
}

impl Display for ToolInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = &self.key;
        match &self.outcome {
            Ok(Status::AlreadyInstalled) => write!(f, "{key}: already installed"),
            Ok(Status::Installed { pin, replaced }) => {
                write!(f, "{key}: installed {}", pin.abbreviated())?;
                match replaced {
                    Some(Damage::Corrupted) => f.write_str(", replacing a corrupted copy"),
                    Some(Damage::Unreadable) => f.write_str(", replacing an unreadable copy"),
                    None => Ok(()),
                }
            }
            Err(_) => write!(f, "{key}: failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::block_on;
    use crate::tools::fixture::{Fixture, NEXTEST};
    use crate::tools::https_server::{HttpsServer, Reply};
    use crate::tools::pinned::DescriptorProblem;
    use crate::tools::source::Fetcher;

    /// The pin of `text`.
    fn pin(text: &str) -> Pin {
        Pin::of(text.as_bytes())
    }

    /// A manifest entry for the tool `key` at `version`, from `source`
    /// (a TOML inline table), pinned to `text`.
    fn entry(key: &str, version: &str, source: &str, text: &str) -> String {
        format!(
            "\n[tools.{key}]\nversion = \"{version}\"\nsource = {source}\npin = \"{}\"\n",
            pin(text)
        )
    }

    /// A manifest with `entries`.
    fn manifest(entries: &[String]) -> String {
        format!(
            "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n{}",
            entries.concat()
        )
    }

    /// The entry for cargo-nextest 1.0.0 from its path in the project,
    /// pinned to NEXTEST.
    fn nextest() -> String {
        entry(
            "cargo-nextest",
            "1.0.0",
            "{ path = \"tools/cargo-nextest.toml\" }",
            NEXTEST,
        )
    }

    /// A valid descriptor other than NEXTEST, with the name `jev`.
    fn jev() -> String {
        NEXTEST
            .replace("name = \"cargo-nextest\"", "name = \"jev\"")
            .replace("version = \"1.0.0\"", "version = \"2.0.0\"")
    }

    impl Fixture {
        /// Run [install] from the project root with `fetcher`.
        async fn install_with(&self, fetcher: &Fetcher) -> Installed {
            let cwd = self.project();
            install(&self.env(fetcher, &cwd)).await.unwrap()
        }

        /// Run [install] on tools whose sources need no network.
        fn install(&self) -> Installed {
            block_on(self.install_with(&Fetcher::new())).unwrap()
        }

        /// Write `manifest` to SLCR.toml, and return it.
        fn declare(&self, manifest: String) -> String {
            self.write("SLCR.toml", &manifest);
            manifest
        }
    }

    /// Each tool's outcome, as `Ok(status)` or `Err(message)`.
    fn outcomes(installed: &Installed) -> Vec<Result<Status, String>> {
        installed
            .tools
            .iter()
            .map(|tool| tool.outcome.as_ref().copied().map_err(ToString::to_string))
            .collect()
    }

    /// The one error of `installed`, which must be an [InstallError].
    fn install_error(installed: &Installed) -> &InstallError {
        let errors: Vec<_> = installed.errors().collect();
        let [error] = errors[..] else {
            panic!("{errors:?}");
        };
        error.downcast_ref().expect("an InstallError")
    }

    #[test]
    fn a_missing_descriptor_kept_in_the_project_is_installed() {
        let fixture = Fixture::new();
        let manifest = fixture.declare(manifest(&[nextest()]));
        let installed = fixture.install();
        assert_eq!(
            outcomes(&installed),
            [Ok(Status::Installed {
                pin: pin(NEXTEST),
                replaced: None
            })]
        );
        assert_eq!(
            installed.to_string(),
            format!("cargo-nextest: installed {}", pin(NEXTEST).abbreviated())
        );
        assert_eq!(installed.verdict(), Ok(()));
        assert!(fixture.installed(NEXTEST));
        assert_eq!(fixture.manifest(), manifest);
    }

    #[test]
    fn a_missing_published_descriptor_is_fetched_and_installed() {
        let fixture = Fixture::new();
        let installed = block_on(async {
            let server = HttpsServer::start(vec![("/jev.toml", Reply::ok(jev()))]).await;
            let source = format!("{{ url = \"{}\" }}", server.https("/jev.toml"));
            fixture.declare(manifest(&[entry("jev", "2.0.0", &source, &jev())]));
            fixture.install_with(&server.fetcher()).await
        })
        .unwrap();
        assert_eq!(
            installed.to_string(),
            format!("jev: installed {}", pin(&jev()).abbreviated())
        );
        assert!(fixture.installed(&jev()));
    }

    #[test]
    fn an_installed_descriptor_is_not_fetched_again() {
        let fixture = Fixture::new();
        fixture.declare(manifest(&[nextest()]));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        // The source is gone, so a fetch would fail.
        std::fs::remove_file(fixture.project().join("tools/cargo-nextest.toml")).unwrap();
        let installed = fixture.install();
        assert_eq!(outcomes(&installed), [Ok(Status::AlreadyInstalled)]);
        assert_eq!(installed.to_string(), "cargo-nextest: already installed");
        assert_eq!(installed.verdict(), Ok(()));
    }

    #[test]
    fn a_corrupted_copy_is_replaced() {
        let fixture = Fixture::new();
        fixture.declare(manifest(&[nextest()]));
        let path = fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let path = fixture.store.path(&path);
        std::fs::write(&path, NEXTEST.replace("--no-fail-fast", "--fail-fast")).unwrap();

        let installed = fixture.install();
        assert_eq!(
            installed.to_string(),
            format!(
                "cargo-nextest: installed {}, replacing a corrupted copy",
                pin(NEXTEST).abbreviated()
            )
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), NEXTEST);
    }

    #[test]
    fn an_unreadable_copy_is_reported_as_replaced() {
        let installed = Installed {
            manifest: PathBuf::from("SLCR.toml"),
            tools: vec![ToolInstall {
                key: ToolKey::try_from("jev".to_owned()).unwrap(),
                outcome: Ok(Status::Installed {
                    pin: pin("jev"),
                    replaced: Some(Damage::Unreadable),
                }),
            }],
        };
        assert_eq!(
            installed.to_string(),
            format!(
                "jev: installed {}, replacing an unreadable copy",
                pin("jev").abbreviated()
            )
        );
    }

    #[test]
    fn a_copy_that_cannot_be_replaced_fails_only_its_tool() {
        let fixture = Fixture::new();
        fixture.write("tools/jev.toml", jev());
        fixture.declare(manifest(&[
            nextest(),
            entry("jev", "2.0.0", "{ path = \"tools/jev.toml\" }", &jev()),
        ]));
        // A directory where cargo-nextest's descriptor would be filed: it
        // can't be read, and can't be replaced either.
        std::fs::create_dir_all(fixture.store.path(&pin(NEXTEST))).unwrap();

        let installed = fixture.install();
        assert_eq!(
            installed.to_string(),
            format!(
                "cargo-nextest: failed\njev: installed {}",
                pin(&jev()).abbreviated()
            )
        );
        assert!(fixture.installed(&jev()));
    }

    #[test]
    fn a_source_that_serves_other_bytes_installs_and_pins_nothing() {
        let fixture = Fixture::new();
        let manifest = fixture.declare(manifest(&[nextest()]));
        let edited = format!("# Ours.\n{NEXTEST}");
        fixture.write("tools/cargo-nextest.toml", &edited);

        let installed = fixture.install();
        let error = install_error(&installed);
        assert!(
            matches!(error, InstallError::Changed { served, .. } if *served == pin(&edited)),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "tools/cargo-nextest.toml no longer serves the descriptor tool cargo-nextest pins"
        );
        assert_eq!(
            error.help().unwrap().to_string(),
            format!(
                "it serves {}, not {}: restore the pinned descriptor there, or re-pin the tool \
                 to what it serves with `slcr tools update cargo-nextest`, which changes \
                 SLCR.toml",
                pin(&edited).abbreviated(),
                pin(NEXTEST).abbreviated()
            )
        );
        assert_eq!(installed.to_string(), "cargo-nextest: failed");
        assert_eq!(
            installed.verdict(),
            Err(InstallFailed {
                failed: 1,
                total: 1
            })
        );
        assert_eq!(fixture.manifest(), manifest);
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn a_published_descriptor_that_changed_is_refused() {
        let fixture = Fixture::new();
        let installed = block_on(async {
            let server = HttpsServer::start(vec![("/jev.toml", Reply::ok(jev()))]).await;
            let source = format!("{{ url = \"{}\" }}", server.https("/jev.toml"));
            // Pinned to what the URL served before.
            let pinned = format!("# Older.\n{}", jev());
            fixture.declare(manifest(&[entry("jev", "2.0.0", &source, &pinned)]));
            fixture.install_with(&server.fetcher()).await
        })
        .unwrap();
        assert!(matches!(
            install_error(&installed),
            InstallError::Changed { .. }
        ));
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn a_pinned_descriptor_that_is_invalid_is_refused() {
        let fixture = Fixture::new();
        let invalid = NEXTEST.replace("{with.test}", "{with.name}");
        fixture.write("tools/cargo-nextest.toml", &invalid);
        fixture.declare(manifest(&[entry(
            "cargo-nextest",
            "1.0.0",
            "{ path = \"tools/cargo-nextest.toml\" }",
            &invalid,
        )]));
        let installed = fixture.install();
        let error = install_error(&installed);
        assert!(matches!(error, InstallError::Invalid { .. }), "{error:?}");
        assert_eq!(
            error.to_string(),
            "the descriptor tool cargo-nextest pins is not a valid descriptor"
        );
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn an_installed_descriptor_that_is_invalid_is_not_fetched_again() {
        let fixture = Fixture::new();
        let invalid = "name = \"cargo-nextest\"\n";
        fixture.declare(manifest(&[entry(
            "cargo-nextest",
            "1.0.0",
            "{ path = \"tools/missing.toml\" }",
            invalid,
        )]));
        fixture.store.install(&fixture.fs, invalid).unwrap();
        let installed = fixture.install();
        // Invalid, not unfetchable: nothing was fetched.
        assert!(matches!(
            install_error(&installed),
            InstallError::Invalid { .. }
        ));
    }

    #[test]
    fn a_pinned_descriptor_of_another_version_is_refused() {
        let fixture = Fixture::new();
        fixture.declare(manifest(&[entry(
            "cargo-nextest",
            "0.9.0",
            "{ path = \"tools/cargo-nextest.toml\" }",
            NEXTEST,
        )]));
        let installed = fixture.install();
        let errors: Vec<_> = installed.errors().collect();
        assert!(
            matches!(
                errors[..],
                [error] if matches!(
                    error.downcast_ref(),
                    Some(DescriptorProblem::DescriptorVersionMismatch { .. })
                )
            ),
            "{errors:?}"
        );
        assert!(fixture.store_is_empty());

        // An installed one is reported the same way.
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();
        let installed = fixture.install();
        assert_eq!(
            outcomes(&installed),
            [Err(
                "tool cargo-nextest is declared as version 0.9.0, but the descriptor it \
                  pins is version 1.0.0"
                    .to_owned()
            )]
        );
    }

    #[test]
    fn a_failed_fetch_installs_nothing() {
        let fixture = Fixture::new();
        let installed = block_on(async {
            let server = HttpsServer::start(Vec::new()).await;
            let source = format!("{{ url = \"{}\" }}", server.https("/jev.toml"));
            fixture.declare(manifest(&[entry("jev", "2.0.0", &source, &jev())]));
            fixture.install_with(&server.fetcher()).await
        })
        .unwrap();
        let error = install_error(&installed);
        assert!(
            matches!(
                error,
                InstallError::Unfetchable {
                    error: SourceError::Fetch { .. },
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn every_tool_is_installed_after_one_fails() {
        let fixture = Fixture::new();
        fixture.write("tools/jev.toml", jev());
        let manifest = fixture.declare(manifest(&[
            nextest(),
            entry(
                "ast-grep",
                "0.3.1",
                "{ path = \"tools/ast-grep.toml\" }",
                "ast-grep",
            ),
            entry("jev", "2.0.0", "{ path = \"tools/jev.toml\" }", &jev()),
        ]));
        fixture.store.install(&fixture.fs, NEXTEST).unwrap();

        let installed = fixture.install();
        assert_eq!(
            installed.to_string(),
            format!(
                "ast-grep: failed\ncargo-nextest: already installed\njev: installed {}",
                pin(&jev()).abbreviated()
            )
        );
        let errors: Vec<String> = installed.errors().map(ToString::to_string).collect();
        assert_eq!(
            errors,
            ["could not fetch the descriptor tool ast-grep pins"]
        );
        // `check` reports only what it fetched, or failed to.
        assert_eq!(
            installed.changes().collect::<Vec<_>>(),
            [
                "ast-grep: failed".to_owned(),
                format!("jev: installed {}", pin(&jev()).abbreviated()),
            ]
        );
        let keys: Vec<&str> = installed.tools().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["ast-grep", "cargo-nextest", "jev"]);
        assert_eq!(
            installed.verdict().unwrap_err().to_string(),
            "installing failed for 1 of 3 tools"
        );
        assert!(fixture.installed(&jev()));
        assert_eq!(fixture.manifest(), manifest);
    }

    #[test]
    fn a_manifest_without_tools_has_nothing_to_install() {
        let fixture = Fixture::new();
        let installed = fixture.install();
        assert_eq!(
            installed.to_string(),
            "SLCR.toml declares no tools, so there is nothing to install"
        );
        assert_eq!(installed.verdict(), Ok(()));
        assert!(fixture.store_is_empty());
    }

    #[test]
    fn the_manifest_is_named_relative_to_the_working_directory() {
        let fixture = Fixture::new();
        let cwd = fixture.project().join("crates/api");
        std::fs::create_dir_all(&cwd).unwrap();
        let fetcher = Fetcher::new();
        let installed = block_on(install(&fixture.env(&fetcher, &cwd)))
            .unwrap()
            .unwrap();
        assert_eq!(installed.manifest, Path::new("../../SLCR.toml"));
    }

    #[test]
    fn a_path_source_is_relative_to_the_project_root() {
        let fixture = Fixture::new();
        fixture.declare(manifest(&[nextest()]));
        let cwd = fixture.project().join("crates/api");
        std::fs::create_dir_all(&cwd).unwrap();
        let fetcher = Fetcher::new();
        let installed = block_on(install(&fixture.env(&fetcher, &cwd)))
            .unwrap()
            .unwrap();
        assert_eq!(installed.verdict(), Ok(()));
        assert!(fixture.installed(NEXTEST));
    }
}
