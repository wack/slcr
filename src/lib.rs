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
//!   and written with [Manifest::to_toml]. Nothing in the library fetches
//!   a tool's descriptor from its [Source].
//! - [check::Report] reads back the document `slcr check --format json`
//!   writes.
//!
//! [Pin] spells a BLAKE3 hash as SLCR does everywhere: `blake3:` and 64
//! lowercase hexadecimal digits.

#![allow(dead_code)]

use std::time::Instant;

use miette::Diagnostic;
use thiserror::Error;

pub use config::Cli;
pub use fs::format::{Format, ParseError};
pub use query::{Cell, InvalidQuery, Rows};
pub use spec::graph::{FormatVersion, SCHEMA_URL, SlcrRequirementsDocument, UnvettedDocument};
pub use spec::id::{CriterionId, RequirementId, SectionId, TermId};
pub use spec::text::SpecName;
pub use spec::vet::Report;
pub use terminal::Terminal;
pub use tools::manifest::{MANIFEST, Manifest, ProjectGlob, ToolEntry};
pub use tools::pin::Pin;
pub use tools::source::{HttpsUrl, ProjectPath, Source};
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
