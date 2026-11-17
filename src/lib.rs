//! SLCR: requirements written as a graph, and checked with validators.
//!
//! The crate is the `slcr` CLI and the library it is built on. A program
//! that stores or serves SLCR documents, such as a server, uses the library
//! to read, vet, write, render, slice, and query them exactly as the CLI
//! does, so that there is one definition of the format:
//!
//! - [spec] is the specification graph, [SlcrRequirementsDocument], read
//!   in any [Format] with [Format::parse] and written in its canonical form
//!   with [Format::serialize]. Reading it as an [UnvettedDocument] keeps a
//!   document that breaks its invariants, to report them.
//! - [vet] vets specifications and projects held in memory, and reports
//!   what it found as `slcr vet --format json` does.
//! - [render], [slice()], and [query()] produce what `slcr render`, `slcr
//!   slice`, and `slcr query --format json` write.
//! - [Manifest] is a project's `SLCR.toml`, read with [Manifest::parse]
//!   and written with [Manifest::to_toml]. Reading, writing, and vetting a
//!   manifest never fetch a tool's descriptor from its [Source]; only the
//!   calls below that install, add, and update tools do.
//! - [check::Report] reads back the document `slcr check --format json`
//!   writes.
//!
//! A program that keeps a project's SLCR files apart from its code, such
//! as a desktop application that keeps each repository's in a directory
//! of its own, works on the project on disk, as `slcr` does with
//! `--slcr-dir` and `--project-root`:
//!
//! - [Project::open] names a project by its SLCR directory, which holds
//!   `SLCR.toml`, its specifications, and its validator files, and its
//!   project root, the code its validators check. A validator file checks
//!   the directory at its own path, relative to the SLCR directory, under
//!   the project root. Files are named relative to the SLCR directory.
//! - [Project::create] creates a specification, and the manifest if there
//!   is none, as `slcr init --slcr-dir` does, and opens the project.
//! - [Project::add_tool], [Project::update_tool], and
//!   [Project::remove_tool] edit the manifest's tools as `slcr tools add`,
//!   `update`, and `remove` do; a path source is relative to the SLCR
//!   directory, and must be inside it.
//! - [DescriptorStore::at] names where the project's tools' descriptors are
//!   installed.
//! - [Project::vet] vets the project as `slcr vet` does, validator files
//!   and pinned descriptors included; [vet::json] writes what it found.
//! - [Project::install] installs every descriptor the store lacks, fetched
//!   with a [Fetcher], as `slcr tools install` does. It, adding a tool, and
//!   updating one are all that fetch.
//! - [Project::check] checks the project as `slcr check --offline` does,
//!   asynchronously on the caller's tokio runtime, which must have its I/O
//!   and time drivers enabled, reporting each [check::ValidatorStatus] as
//!   it would be written, stopping when the caller cancels it, and running
//!   tools on the `PATH` [check::Options] names. It returns a
//!   [check::Report]. On Unix, dropping the check kills its validators at
//!   once, with everything they started.
//!
//! [Project::install], [Project::add_tool], and [Project::update_tool]
//! need a tokio runtime too; the rest are synchronous.
//!
//! # Getting started
//!
//! A desktop application that tracks a user's repositories keeps each
//! one's SLCR files in a directory of its own, and checks the repository
//! on its own runtime:
//!
//! ```no_run
//! use slcr::check::Options;
//! use slcr::{DescriptorStore, Fetcher, Project, SpecName};
//!
//! # async fn example() -> miette::Result<()> {
//! // The application's directory for this repository's SLCR files, and
//! // the repository itself, which its validators check.
//! let slcr_dir = "/home/me/.local/share/my-app/projects/shop";
//! let repository = "/home/me/src/shop";
//! let project = match Project::open(slcr_dir, repository) {
//!     Ok(project) => project,
//!     // No `SLCR.toml` yet: start the project.
//!     Err(_) => {
//!         let name = SpecName::try_from("shop".to_owned())?;
//!         let (project, _) = Project::create(slcr_dir, repository, "SPEC.slcr.toml", name, None)?;
//!         project
//!     }
//! };
//! // Where the application installs descriptors, for every project.
//! let store = DescriptorStore::at("/home/me/.local/share/my-app/descriptors");
//! // Fetching happens only here, when the application allows it.
//! let installed = project.install(&store, &Fetcher::new()).await?;
//! println!("{installed}");
//! // Completes when the user presses Cancel.
//! let cancelled = std::future::pending::<()>();
//! let report = project
//!     .check(&store, &Options::default(), |status| println!("{status}"), cancelled)
//!     .await;
//! println!("passed: {}", report.passed);
//! # Ok(())
//! # }
//! ```
//!
//! None of these read or change the process's working directory or its
//! signal handlers, or change its environment, so that a program can work
//! on several projects at once. Of the environment they read only what
//! `slcr` itself reads: the `PATH` a check falls back to, the variables
//! that locate git's global excludes, and, while fetching, the proxy
//! variables.
//!
//! [Pin] spells a BLAKE3 hash as SLCR does everywhere: `blake3:` and 64
//! lowercase hexadecimal digits.

#![allow(dead_code)]

use std::time::Instant;

use miette::Diagnostic;
use thiserror::Error;

pub use cli::init::Initialized;
/// The binary's command line, which only `slcr` itself needs.
#[doc(hidden)]
pub use config::Cli;
pub use fs::format::{Format, ParseError};
pub use query::{Cell, InvalidQuery, Rows};
pub use spec::graph::{FormatVersion, SCHEMA_URL, SlcrRequirementsDocument, UnvettedDocument};
pub use spec::id::{CriterionId, RequirementId, SectionId, TermId};
pub use spec::text::{SpecName, Title};
pub use spec::vet::Report;
/// The binary's output, which only `slcr` itself needs.
#[doc(hidden)]
pub use terminal::Terminal;
pub use tools::add::Added as ToolAdded;
pub use tools::install::Installed;
pub use tools::manifest::{
    DirectoryProblem, Given, MANIFEST, Manifest, Project, ProjectError, ProjectGlob, ToolEntry,
    UnusableDirectory,
};
pub use tools::pin::Pin;
pub use tools::remove::Removed as ToolRemoved;
pub use tools::source::{Fetcher, HttpsUrl, InvalidAuthority, ProjectPath, Source};
pub use tools::store::DescriptorStore;
pub use tools::update::Updated as ToolUpdated;
pub use tools::{DescriptorVersion, ToolKey};

pub mod check;
pub mod vet;

/// Contains the dispatch logic for running individual CLI subcommands.
/// The CLI's main function calls into these entrypoints for each subcommand.
pub(crate) mod cli;
/// configuration of the CLI, either from the environment of flags.
mod config;
/// An abstraction over the user's filesystem, respecting $XDG_CONFIG.
mod fs;
/// What a program does with a project on disk: vet, install, check.
mod library;
/// A project's specifications: discovering, loading, and indexing them.
mod project;
pub mod query;
/// The canonical Markdown rendering of a specification.
mod render;
/// Running one validator and reading its outcome.
mod run;
/// Whether each requirement is satisfied, from its validators' verdicts.
mod satisfaction;
/// What the agent slice of one requirement includes.
mod slice;
pub mod spec;
/// Terminal output, color detection, and the miette error hook.
mod terminal;
/// External tools: the project manifest and tool descriptors.
mod tools;
/// Validator files, which bind requirements to the tools that check them.
mod validators;

/// `document` as Markdown, byte for byte as `slcr render` writes it.
pub fn render(document: &SlcrRequirementsDocument) -> String {
    render::document(document)
}

/// The slice of `requirement` in `document`, as Markdown, byte for byte as
/// `slcr slice` writes it.
pub async fn slice(
    document: &SlcrRequirementsDocument,
    requirement: RequirementId,
) -> Result<String, SliceError> {
    let known = document
        .requirements()
        .iter()
        .any(|candidate| *candidate.id() == requirement);
    if !known {
        return Err(SliceError::UnknownRequirement { requirement });
    }
    let graph = query::SpecGraph::new(document).await?;
    let members = slice::members(&graph, requirement).await?;
    Ok(render::slice(document, &members))
}

/// Why [slice()] returned no slice.
#[derive(Debug, Error, Diagnostic)]
pub enum SliceError {
    #[error("the specification has no requirement {requirement}")]
    #[diagnostic(help("`slcr query 'MATCH (r:Requirement) RETURN r, r.title'` lists them"))]
    UnknownRequirement { requirement: RequirementId },

    /// The queries that find the slice's members failed, which they don't
    /// on a vetted document.
    #[error("could not find what the slice includes")]
    Failed(#[from] slcr_gql::Error),
}

impl From<slcr_gql::StoreError> for SliceError {
    fn from(error: slcr_gql::StoreError) -> Self {
        Self::Failed(error.into())
    }
}

/// Run the read-only GQL query `text` against `document`, with `params`
/// bound to its `$parameters` as `slcr query --param NAME=JSON` binds them.
/// [Rows::to_json] writes the rows as `slcr query --format json` does.
///
/// With a `deadline`, a query still running when it passes is stopped with
/// [QueryError::DeadlineReached].
pub async fn query(
    document: &SlcrRequirementsDocument,
    text: &str,
    params: serde_json::Map<String, serde_json::Value>,
    deadline: Option<Instant>,
) -> Result<Rows, QueryError> {
    let invalid = |error| QueryError::Invalid(InvalidQuery::new(text, error));
    let parsed = slcr_gql::parse_query(text).map_err(invalid)?;
    let params: slcr_gql::Params = params
        .into_iter()
        .map(|(name, value)| (name, query::value(value)))
        .collect();
    let graph = query::SpecGraph::new(document)
        .await
        .map_err(|error| invalid(error.into()))?;
    match deadline {
        None => graph.query(&parsed, &params).await.map_err(invalid),
        Some(deadline) => match graph.query_until(&parsed, &params, deadline).await {
            Ok(rows) => Ok(rows),
            Err(query::Stopped::DeadlineReached) => Err(QueryError::DeadlineReached),
            Err(query::Stopped::Failed(error)) => Err(invalid(error)),
        },
    }
}

/// Why [query()] returned no rows.
#[derive(Debug, Error, Diagnostic)]
pub enum QueryError {
    /// The query can't run, or failed while running: the diagnostic `slcr
    /// query` reports, which labels a syntax error in the query's text.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Invalid(InvalidQuery),

    #[error("the query was stopped: it reached its time limit")]
    #[diagnostic(help("narrow the query, for instance by bounding its quantified paths"))]
    DeadlineReached,
}
