//! What `slcr check` does, from installing descriptors to its report,
//! with nothing written: the CLI and the library run this one sequence,
//! and each observes it in its own way.

use std::{ffi::OsString, future::Future, num::NonZeroUsize, sync::Arc};

use miette::Result;

use super::json::{self, Document};
use super::schedule::{Finished, Plan, run_all};
use super::status::ValidatorStatus;
use super::verdict::{Evaluated, Interrupted, NotVetted, Tally, evaluate_all, verdicts};
use crate::cli::vet::{self, Outcome, ProjectOutcome, json::Document as VetDocument};
use crate::project::vet::{VettedProject, vet_project};
use crate::tools::{Environment, install::Installed, install::install};

/// How to check a project.
#[derive(Clone, Debug)]
pub(crate) struct Options {
    /// Install every descriptor the store lacks first, as `slcr check`
    /// does unless `--offline`. The library never does.
    pub(crate) install: bool,
    /// How many validators may run at once.
    pub(crate) jobs: NonZeroUsize,
    /// Whether warnings fail the check.
    pub(crate) deny_warnings: bool,
    /// The `PATH` tools are found on and validators inherit, in place of
    /// the process's own.
    pub(crate) path: Option<OsString>,
}

/// Every validator, and how each ended, once the run is over.
pub(crate) struct Conclusion<'a> {
    pub(crate) plan: &'a Plan,
    /// Each validator's result, in the plan's order: `None` for one that
    /// hadn't finished when the check was interrupted.
    pub(crate) results: &'a [Option<Finished>],
    /// Each specification, evaluated, unless the check was interrupted.
    pub(crate) evaluated: Option<&'a [Evaluated<'a>]>,
}

/// What a caller of [check] sees of it, as it happens. Each method is
/// called at most once, in the order they are declared, except
/// [Observer::status], called once for each validator, in the plan's
/// order; [Observer::reported] is always called last, unless an earlier
/// call failed.
pub(crate) trait Observer {
    /// Installing ended with `installed`, if installing was asked for and
    /// the manifest could be read.
    fn installed(&mut self, installed: &Installed) -> Result<()> {
        let _ = installed;
        Ok(())
    }

    /// Vetting found `project` and `outcomes`, and passed, warnings aside,
    /// or not.
    fn vetted(
        &mut self,
        project: &ProjectOutcome,
        outcomes: &[Outcome],
        passed: bool,
    ) -> Result<()> {
        let _ = (project, outcomes, passed);
        Ok(())
    }

    /// A validator's status, as soon as it and every validator before it
    /// have finished, or once the check was interrupted.
    fn status(&mut self, status: &ValidatorStatus) -> Result<()> {
        let _ = status;
        Ok(())
    }

    /// Every validator has finished, or the check was interrupted.
    fn concluded(&mut self, conclusion: &Conclusion<'_>) -> Result<()> {
        let _ = conclusion;
        Ok(())
    }

    /// The document `slcr check --format json` writes, as far as the check
    /// got.
    fn reported(&mut self, document: &Document<'_>) -> Result<()>;
}

/// Check the project of `env`, as `slcr check` does: install what the
/// store lacks, if `options` say to; vet the project, and stop there if it
/// fails, warnings aside; run every validator, at most `options.jobs` at
/// once, until they finish or `interrupt` completes; evaluate each
/// specification; and report it all to `observer`.
///
/// It returns the verdict: why the check failed, if it did, or an error
/// of `observer`'s.
pub(crate) async fn check(
    env: &Environment<'_>,
    options: &Options,
    interrupt: impl Future<Output = ()>,
    observer: &mut impl Observer,
) -> Result<()> {
    let deny_warnings = options.deny_warnings;

    // A manifest that can't be read fails before any tool is installed;
    // vetting reports it, in full.
    let installed = if options.install {
        install(env).await.ok()
    } else {
        None
    };
    let descriptors = || installed.as_ref().map(json::descriptors);
    if let Some(installed) = &installed {
        observer.installed(installed)?;
        if let Err(failed) = installed.verdict() {
            observer.reported(&Document {
                descriptors: descriptors(),
                ..Document::default()
            })?;
            return Err(failed.into());
        }
    }

    let VettedProject {
        project,
        outcomes,
        loaded,
    } = vet_project(env.fs, env.store, &env.project, env.cwd);
    let vetted = vet::verdict(Some(&project), &outcomes, false);
    observer.vetted(&project, &outcomes, vetted.is_ok())?;
    let vet_document = || Some(VetDocument::new(Some(&project), &outcomes, deny_warnings));
    if let Err(failed) = vetted {
        observer.reported(&Document {
            descriptors: descriptors(),
            vet: vet_document(),
            ..Document::default()
        })?;
        return Err(NotVetted { failed }.into());
    }

    let plan = Arc::new(Plan::new(
        loaded.layout,
        loaded.validators,
        loaded.descriptors,
        options.path.clone(),
    ));
    let ran = run_all(
        Arc::clone(&plan),
        options.jobs,
        interrupt,
        |index, finished| observer.status(&ValidatorStatus::new(plan.job(index), finished)),
    )
    .await?;
    let evaluated = (!ran.interrupted).then(|| {
        let verdicts = verdicts(&plan, &ran.results);
        evaluate_all(&loaded.specs, &verdicts)
    });
    observer.concluded(&Conclusion {
        plan: &plan,
        results: &ran.results,
        evaluated: evaluated.as_deref(),
    })?;

    let verdict: Result<()> = match &evaluated {
        None => Err(Interrupted {
            finished: ran.results.iter().flatten().count(),
            total: ran.results.len(),
        }
        .into()),
        Some(evaluated) => {
            let vet_warnings = outcomes.iter().map(vet::Outcome::warnings).sum();
            let tally = Tally::new(vet_warnings, &ran.results, evaluated);
            tally.verdict(deny_warnings).map_err(Into::into)
        }
    };
    observer.reported(&Document {
        passed: verdict.is_ok(),
        interrupted: ran.interrupted,
        descriptors: descriptors(),
        vet: vet_document(),
        validators: Some(json::validators(&plan, &ran.results)),
        specs: evaluated
            .as_deref()
            .map(|evaluated| json::specs(evaluated, deny_warnings)),
    })?;
    verdict
}
