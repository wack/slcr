//! Vetting specifications and projects held in memory, as a server that
//! stores them does, and reporting what vetting found in the JSON document
//! `slcr vet --format json` writes.
//!
//! [source] reads one specification as `slcr vet FILE` reads a file, and
//! [document] vets one already read. [project] vets a project as `slcr
//! vet` with no FILE vets one on disk, from its manifest and its
//! specifications' paths, without the validator files and descriptor store
//! that only a checkout of the project has. [json] writes the outcomes as
//! `slcr vet --format json` does.

use std::collections::BTreeMap;
use std::path::PathBuf;

use miette::Report as ErrorReport;

pub use crate::cli::vet::json::{Document as Report, ErrorJson};
pub use crate::cli::vet::{Outcome, ProjectOutcome};
use crate::fs::format::Format;
pub use crate::project::Problem;
use crate::project::discover::discover_among;
use crate::project::{IndexedSpec, ProjectFile, SpecIndex};
use crate::spec::graph::UnvettedDocument;
use crate::tools::manifest::{MANIFEST, Manifest};

/// Read `source` as a specification in `format` without vetting its
/// invariants, and vet it, as `slcr vet` vets a file at `path`: the
/// outcome names the file by `path`, and is the error that reading it
/// failed with, if it couldn't be read. `format` need not be the one
/// `path`'s extension names.
///
/// The document is returned if it could be read, whatever vetting found.
pub fn source(
    path: impl Into<PathBuf>,
    format: Format,
    source: String,
) -> (Outcome, Option<UnvettedDocument>) {
    let path = path.into();
    match format.parse::<UnvettedDocument>(&path, source) {
        Ok(document) => (Outcome::new(path, Ok(document.report())), Some(document)),
        Err(error) => (Outcome::new(path, Err(ErrorReport::new(error))), None),
    }
}

/// Vet `document`, a specification already read, as `slcr vet` vets a
/// file at `path`.
pub fn document(path: impl Into<PathBuf>, document: &UnvettedDocument) -> Outcome {
    Outcome::new(path.into(), Ok(document.report()))
}

/// Vet a project held in memory, as `slcr vet` with no FILE vets a project
/// on disk: `manifest`, its `SLCR.toml`, and `specs`, each specification
/// with its path relative to the project root.
///
/// The manifest's `specs` patterns are matched against those paths as
/// against the files of a project on disk, and each problem `slcr vet`
/// would report for the files of the project, written out, is reported:
/// a pattern that is invalid or matches nothing, a match that isn't a
/// specification file, and two specifications with the same name. A
/// specification that no pattern matches is reported as
/// [Problem::Unlisted], which `slcr vet` can't see on disk.
///
/// Like `slcr vet`, it returns an outcome for each specification a pattern
/// matches, in the order of their paths. Nothing is read from disk or
/// fetched: the manifest's tools are not checked.
pub fn project(
    manifest: &Manifest,
    specs: Vec<(PathBuf, UnvettedDocument)>,
) -> (ProjectOutcome, Vec<Outcome>) {
    let (discovery, unlisted) =
        discover_among(specs.iter().map(|(path, _)| path.clone()), manifest.specs());
    let mut problems = discovery.problems;
    problems.extend(unlisted.into_iter().map(|path| Problem::Unlisted { path }));
    let mut documents: BTreeMap<PathBuf, UnvettedDocument> = specs.into_iter().collect();
    let matched: Vec<(ProjectFile, UnvettedDocument)> = discovery
        .files
        .into_iter()
        .filter_map(|file| {
            let document = documents.remove(file.relative())?;
            Some((file, document))
        })
        .collect();
    let (_, duplicates) = SpecIndex::new(
        matched
            .iter()
            .map(|(file, document)| IndexedSpec { file, document }),
    );
    problems.extend(duplicates);
    let outcomes = matched
        .iter()
        .map(|(file, document)| self::document(file.shown(), document))
        .collect();
    (
        ProjectOutcome::new(PathBuf::from(MANIFEST), Ok(problems)),
        outcomes,
    )
}

/// The outcome of vetting a project whose manifest couldn't be loaded,
/// for `error`: for instance, a project held in memory that has none yet.
pub fn unloadable_project(error: ErrorReport) -> ProjectOutcome {
    ProjectOutcome::new(PathBuf::from(MANIFEST), Err(error))
}

/// The JSON document `slcr vet --format json` writes for `project`, if a
/// project was vetted, and `files`, which passes unless one of them fails,
/// as warnings do only if `deny_warnings`. With a project, it lists no
/// validator files: its `validators` array is empty.
///
/// It serializes as `slcr vet` writes it; [json] writes it as text.
pub fn report<'a>(
    project: Option<&'a ProjectOutcome>,
    files: &'a [Outcome],
    deny_warnings: bool,
) -> Report<'a> {
    Report::new(project, files, deny_warnings)
}

/// [report], as the text `slcr vet --format json` writes, without the
/// newline that ends its output.
pub fn json(project: Option<&ProjectOutcome>, files: &[Outcome], deny_warnings: bool) -> String {
    crate::cli::vet::json::document(project, files, deny_warnings)
}
