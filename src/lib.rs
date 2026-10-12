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
/// A project's specifications: discovering, loading, and indexing them.
mod project;
/// GQL queries over a specification's graph.
mod query;
/// The canonical Markdown rendering of a specification.
mod render;
/// Running one validator and reading its outcome.
mod run;
/// Whether each requirement is satisfied, from its validators' verdicts.
mod satisfaction;
/// What the agent slice of one requirement includes.
mod slice;
/// The specification graph: SLCR's requirements document.
mod spec;
/// Terminal output, color detection, and the miette error hook.
mod terminal;
/// External tools: the project manifest and tool descriptors.
mod tools;
/// Validator files, which bind requirements to the tools that check them.
mod validators;
