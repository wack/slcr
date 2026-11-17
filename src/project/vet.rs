//! Vetting a project on disk, as `slcr vet` does given no FILE, and
//! keeping what loaded for `slcr check` to run.

use std::collections::BTreeMap;
use std::path::Path;

use super::{IndexedSpec, Layout, Problem, ProjectFile, SpecIndex, discover, relative_to};
use crate::cli::vet::{Outcome, ProjectOutcome, ValidatorOutcome};
use crate::fs::FileSystem;
use crate::spec::graph::UnvettedDocument;
use crate::tools::descriptor::Descriptor;
use crate::tools::manifest::{ManifestFile, Project};
use crate::tools::{ToolKey, pinned, store::DescriptorStore};
use crate::validators::ValidatorFile;
use crate::validators::vet::{Context, vet as vet_validators};

/// What vetting a project found, and the parts of it that loaded.
#[derive(Debug)]
pub(crate) struct VettedProject {
    pub(crate) project: ProjectOutcome,
    /// What vetting each specification file found, in the order of their
    /// paths.
    pub(crate) outcomes: Vec<Outcome>,
    pub(crate) loaded: Loaded,
}

/// The parts of a project that loaded, which `slcr check` runs and
/// evaluates once the project vets cleanly; then nothing is missing.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// Where the project is: what its validators run against.
    pub(crate) layout: Layout,
    /// Each specification that loaded, in the order of their paths.
    pub(crate) specs: Vec<(ProjectFile, UnvettedDocument)>,
    /// Each validator file that loaded, in the order of their paths.
    pub(crate) validators: Vec<ValidatorFile>,
    /// The descriptor of each declared tool that the store holds.
    pub(crate) descriptors: BTreeMap<ToolKey, Descriptor>,
}

/// Vet `project`, as `slcr vet` with no FILE does, naming its files
/// relative to `cwd`, an absolute path, which the library makes its SLCR
/// directory:
/// read its manifest, and check the descriptor each declared tool pins as
/// `store` holds it; discover its specification files and validator files
/// in its SLCR directory; vet each specification, check that their names
/// are unique, then vet each validator file against them.
pub(crate) fn vet_project(
    fs: &FileSystem,
    store: &DescriptorStore,
    project: &Project,
    cwd: &Path,
) -> VettedProject {
    let layout = project.layout().clone();
    let manifest = project.shown_manifest().to_path_buf();
    tracing::debug!(manifest = %manifest.display(), "vetting a project");
    let file = match ManifestFile::load(project) {
        Ok(file) => file,
        Err(error) => {
            let project = ProjectOutcome::new(manifest, Err(error));
            return VettedProject {
                project,
                outcomes: Vec::new(),
                loaded: Loaded {
                    layout,
                    specs: Vec::new(),
                    validators: Vec::new(),
                    descriptors: BTreeMap::new(),
                },
            };
        }
    };
    let discovery = discover(layout.slcr_dir(), cwd, file.manifest().specs());
    let mut problems = discovery.problems;
    problems.extend(discovery.validators.iter().filter_map(|file| {
        let counterpart = layout.validator_dir(file);
        (!counterpart.is_dir()).then(|| Problem::MissingCounterpart {
            path: file.shown().to_path_buf(),
            counterpart: relative_to(&counterpart, cwd),
        })
    }));
    let loaded: Vec<_> = discovery
        .files
        .into_iter()
        .map(|file| {
            tracing::debug!(path = %file.shown().display(), "vetting a requirements file");
            let document = file.load_spec(fs);
            (file, document)
        })
        .collect();
    let (index, duplicates) = SpecIndex::new(loaded.iter().filter_map(|(file, document)| {
        let document = document.as_ref().ok()?;
        Some(IndexedSpec { file, document })
    }));
    problems.extend(duplicates);

    let (descriptors, unusable) = pinned::load_all(fs, store, file.manifest().tools());
    problems.extend(unusable.into_iter().map(Problem::Descriptor));
    // Each loaded descriptor's schema compiled when it was checked.
    let schemas = descriptors
        .iter()
        .filter_map(|(key, descriptor)| {
            let schema = jsonschema::draft202012::new(descriptor.schema()).ok()?;
            Some((key.clone(), schema))
        })
        .collect();
    let cx = Context {
        specs: &index,
        complete: loaded.iter().all(|(_, document)| document.is_ok()),
        manifest: &file,
        schemas: &schemas,
    };
    let mut validator_files = Vec::new();
    let validators = discovery
        .validators
        .into_iter()
        .map(|file| {
            tracing::debug!(path = %file.shown().display(), "vetting a validator file");
            let path = file.shown().to_path_buf();
            let result = ValidatorFile::load(file).map(|file| {
                let findings = vet_validators(&file, &cx);
                validator_files.push(file);
                findings
            });
            ValidatorOutcome::new(path, result)
        })
        .collect();
    // It borrows `loaded`, which is taken apart next.
    drop(index);

    let mut specs = Vec::new();
    let mut outcomes = Vec::new();
    for (file, document) in loaded {
        let path = file.shown().to_path_buf();
        let result = document.map(|document| {
            let report = document.report();
            specs.push((file, document));
            report
        });
        outcomes.push(Outcome::new(path, result));
    }
    let project_outcome = ProjectOutcome::new(manifest, Ok(problems)).with_validators(validators);
    VettedProject {
        project: project_outcome,
        outcomes,
        loaded: Loaded {
            layout,
            specs,
            validators: validator_files,
            descriptors,
        },
    }
}
