use std::{
    fmt::{self, Display},
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand};
use miette::{Diagnostic, IntoDiagnostic, Result};
use thiserror::Error;

use crate::Terminal;
use crate::fs::FileSystem;
use crate::tools::{
    DescriptorVersion, InvalidToolKey, ToolKey,
    descriptor::Descriptor,
    manifest::{ManifestFile, Project, ToolEntry},
    pin::Pin,
    source::SourceArg,
    store::DescriptorStore,
};

/// The arguments to `slcr tools`.
#[derive(Args, Clone, Debug)]
pub struct ToolsArgs {
    #[command(subcommand)]
    command: ToolsCommand,
}

/// The subcommands of `slcr tools`, which edit the `[tools]` table of the
/// project manifest, `SLCR.toml`.
#[derive(Subcommand, Clone, Debug)]
pub enum ToolsCommand {
    /// Declare a tool in SLCR.toml: fetch its descriptor, check it, pin it,
    /// and install it
    Add(AddArgs),
}

/// The arguments to `slcr tools add`.
#[derive(Args, Clone, Debug)]
pub struct AddArgs {
    /// Where to fetch the descriptor from: an `https://` URL, or the path to
    /// a file inside the project.
    #[arg(value_name = "SOURCE")]
    source: SourceArg,

    /// The name to declare the tool under, which validators refer to it by.
    /// Defaults to the descriptor's own name.
    #[arg(long = "as", value_name = "NAME", value_parser = parse_key)]
    key: Option<ToolKey>,
}

fn parse_key(value: &str) -> Result<ToolKey, InvalidToolKey> {
    ToolKey::try_from(value.to_owned())
}

/// Edit the tools a project declares.
pub struct Tools {
    terminal: Terminal,
    args: ToolsArgs,
}

impl Tools {
    pub fn new(terminal: Terminal, args: ToolsArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        match self.args.command {
            ToolsCommand::Add(args) => add(&self.terminal, args),
        }
    }
}

/// `slcr tools add`: fetch a descriptor, validate it, pin it, install it in
/// the descriptor store, and declare it in the manifest.
///
/// The manifest is written last, so it never declares a descriptor that
/// failed to install. Adding a descriptor that is already declared under
/// the same name, from the same source, with the same pin, changes nothing.
fn add(terminal: &Terminal, args: AddArgs) -> Result<()> {
    let AddArgs { source, key } = args;
    let fs = FileSystem::new()?;
    let cwd = std::env::current_dir().into_diagnostic()?;
    let project = Project::find_from(&cwd)?;
    let mut manifest = ManifestFile::load(&project)?;

    let origin = origin(&source);
    tracing::debug!(%origin, "fetching a descriptor");
    let (source, bytes) = source.fetch(&fs, &cwd, project.root())?;
    let descriptor = Descriptor::parse(&origin, &bytes)?;
    let pin = Pin::of(&bytes);
    let key = key.unwrap_or_else(|| descriptor.name().clone());
    let entry = ToolEntry::new(descriptor.version().clone(), source, pin);

    let unchanged = match manifest.manifest().tools().get(&key) {
        Some(existing) if *existing == entry => true,
        Some(_) => {
            return Err(AlreadyDeclared {
                manifest: project.shown_manifest().to_path_buf(),
                key,
            }
            .into());
        }
        None => false,
    };
    DescriptorStore::new(&fs).install(&fs, &pin, &bytes)?;
    if !unchanged {
        manifest.set_tool(&key, entry);
        manifest.save(&fs)?;
    }
    let added = Added {
        manifest: project.shown_manifest(),
        key: &key,
        version: descriptor.version(),
        unchanged,
    };
    terminal.write_stdout_line(&added.to_string())
}

/// How errors name a source: the URL, or the path as it was given.
fn origin(source: &SourceArg) -> String {
    match source {
        SourceArg::Url(url) => url.to_string(),
        SourceArg::Path(path) => path.display().to_string(),
    }
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

/// The status `slcr tools add` reports, e.g.
/// `SLCR.toml: added tool cargo-nextest 1.0.0`.
struct Added<'a> {
    manifest: &'a Path,
    key: &'a ToolKey,
    version: &'a DescriptorVersion,
    unchanged: bool,
}

impl Display for Added<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            manifest,
            key,
            version,
            unchanged,
        } = self;
        let manifest = manifest.display();
        if *unchanged {
            write!(f, "{manifest}: tool {key} {version} is already declared")
        } else {
            write!(f, "{manifest}: added tool {key} {version}")
        }
    }
}
