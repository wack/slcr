use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

use super::block_on;
use crate::Terminal;
use crate::fs::FileSystem;
use crate::tools::{
    Environment, InvalidToolKey, ToolKey,
    add::add,
    list::list,
    remove::remove,
    show::show,
    source::{Fetcher, SourceArg},
    store::DescriptorStore,
    update::update,
};

/// The arguments to `slcr tools`.
#[derive(Args, Clone, Debug)]
pub struct ToolsArgs {
    #[command(subcommand)]
    command: ToolsCommand,
}

/// The subcommands of `slcr tools`, which edit and read the `[tools]`
/// table of the project manifest, `SLCR.toml`.
#[derive(Subcommand, Clone, Debug)]
pub enum ToolsCommand {
    /// Declare a tool in SLCR.toml: fetch its descriptor, check it, pin it,
    /// and install it
    Add(AddArgs),
    /// Re-fetch a declared tool's descriptor from its source, check it,
    /// re-pin it, and install it
    Update(KeyArgs),
    /// Remove a tool from SLCR.toml
    Remove(KeyArgs),
    /// List the tools SLCR.toml declares, and whether each one's descriptor
    /// is installed
    List(ListArgs),
    /// Print a declared tool's installed descriptor: how it runs, and the
    /// schema each validator's `with` must satisfy
    Show(ShowArgs),
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

/// The arguments to `slcr tools update` and `slcr tools remove`.
#[derive(Args, Clone, Debug)]
pub struct KeyArgs {
    /// The name the tool is declared under in SLCR.toml.
    #[arg(value_name = "NAME", value_parser = parse_key)]
    key: ToolKey,
}

/// The arguments to `slcr tools list`.
#[derive(Args, Clone, Debug)]
pub struct ListArgs {
    /// How to write the tools.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

/// The arguments to `slcr tools show`.
#[derive(Args, Clone, Debug)]
pub struct ShowArgs {
    /// The name the tool is declared under in SLCR.toml.
    #[arg(value_name = "NAME", value_parser = parse_key)]
    key: ToolKey,

    /// How to write the tool.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

/// How `slcr tools list` and `slcr tools show` write what they find.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Text for people.
    #[default]
    Human,
    /// One JSON document.
    Json,
}

impl OutputFormat {
    /// `found` in this format.
    fn write<T: std::fmt::Display + Serialize>(self, found: &T) -> String {
        match self {
            Self::Human => found.to_string(),
            Self::Json => serde_json::to_string_pretty(found)
                .expect("what the tools commands find serializes"),
        }
    }
}

fn parse_key(value: &str) -> Result<ToolKey, InvalidToolKey> {
    ToolKey::try_from(value.to_owned())
}

/// Edit and read the tools a project declares. `list` and `show` read
/// descriptors from the descriptor store only, and never fetch one.
pub struct Tools {
    terminal: Terminal,
    args: ToolsArgs,
}

impl Tools {
    pub fn new(terminal: Terminal, args: ToolsArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let fs = FileSystem::new()?;
        let store = DescriptorStore::new(&fs);
        let fetcher = Fetcher::new();
        let cwd = std::env::current_dir().into_diagnostic()?;
        let env = Environment {
            fs: &fs,
            store: &store,
            fetcher: &fetcher,
            cwd: &cwd,
        };
        let status = match self.args.command {
            ToolsCommand::Add(AddArgs { source, key }) => {
                block_on(add(&env, source, key))??.to_string()
            }
            ToolsCommand::Update(KeyArgs { key }) => block_on(update(&env, key))??.to_string(),
            ToolsCommand::Remove(KeyArgs { key }) => remove(&fs, &cwd, key)?.to_string(),
            ToolsCommand::List(ListArgs { format }) => format.write(&list(&fs, &store, &cwd)?),
            ToolsCommand::Show(ShowArgs { key, format }) => {
                format.write(&show(&fs, &store, &cwd, key)?)
            }
        };
        self.terminal.write_stdout_line(&status)
    }
}
