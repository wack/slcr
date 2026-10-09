#![allow(dead_code)]

pub use config::Cli;
pub use terminal::Terminal;

/// Contains the dispatch logic for running individual CLI subcommands.
/// The CLI's main function calls into these entrypoints for each subcommand.
mod cli;
/// configuration of the CLI, either from the environment of flags.
mod config;
/// An abstraction over the user's filesystem, respecting $XDG_CONFIG.
mod fs;
/// The specification graph: SLCR's requirements document.
mod spec;
/// Terminal output, color detection, and the miette error hook.
mod terminal;
