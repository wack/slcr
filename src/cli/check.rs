use std::{num::NonZeroUsize, sync::Arc};

use clap::{Args, ValueEnum};
use miette::{IntoDiagnostic, Result};

use super::block_on;
use super::vet::{self, VettedProject, json::Document as VetDocument, vet_project};
use crate::Terminal;
use crate::fs::FileSystem;
use crate::run::process::Outcome;
use crate::tools::{Environment, install::install, source::Fetcher, store::DescriptorStore};

mod json;
mod report;
mod schedule;
mod verdict;

use report::{RedValidator, SpecFindings, Unrunnable};
use schedule::{Plan, run_all};
use verdict::{Interrupted, NotVetted, Tally, evaluate_all, severity, verdicts};

/// The arguments to `slcr check`.
#[derive(Args, Clone, Debug)]
pub struct CheckArgs {
    /// Run at most N validators at once [default: the number of CPUs].
    /// With 1, they run one at a time, in the order they are reported.
    #[arg(short, long, value_name = "N", value_parser = parse_jobs)]
    jobs: Option<NonZeroUsize>,

    /// Never fetch a descriptor: one the descriptor store lacks fails
    /// vetting, as it does `slcr vet`.
    #[arg(long)]
    offline: bool,

    /// Fail on warnings too: vetting's, unsatisfied SHOULD and SHOULD NOT
    /// requirements, and absorbed failures.
    #[arg(long)]
    deny_warnings: bool,

    /// How to report what `check` found.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

/// How `slcr check` reports what it found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Each validator's status and each specification's summary on stdout,
    /// and diagnostics on stderr.
    #[default]
    Human,
    /// One JSON document on stdout, with every validator's whole output.
    Json,
}

fn parse_jobs(value: &str) -> Result<NonZeroUsize, String> {
    match value.parse::<usize>() {
        Ok(jobs) => NonZeroUsize::new(jobs).ok_or_else(|| "it must be at least 1".to_owned()),
        Err(_) => Err("expected a whole number of at least 1".to_owned()),
    }
}

/// Run every validator, and report whether each requirement is satisfied.
///
/// First, unless `--offline`, it installs every descriptor the manifest
/// pins that the store lacks, as `slcr tools install` does, and fails if
/// any can't be. Then it vets the project, as `slcr vet` does given no
/// files, and fails without running anything if vetting does, warnings
/// aside. Then it runs every validator, `--jobs` at a time, and evaluates
/// each specification from their results.
///
/// It fails if a validator can't be run, if Ctrl-C interrupts it, if an
/// unsatisfied MUST or MUST NOT requirement isn't absorbed, or, with
/// `--deny-warnings`, on any warning. Unknown requirements never fail it.
pub struct Check {
    terminal: Terminal,
    args: CheckArgs,
}

impl Check {
    pub fn new(terminal: Terminal, args: CheckArgs) -> Self {
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
        block_on(self.check(&env))?
    }

    fn human(&self) -> bool {
        self.args.format == OutputFormat::Human
    }

    async fn check(&self, env: &Environment<'_>) -> Result<()> {
        let deny_warnings = self.args.deny_warnings;

        // A project or manifest that can't be read fails before any tool
        // is installed; vetting reports it, in full.
        let installed = if self.args.offline {
            None
        } else {
            install(env).await.ok()
        };
        let descriptors = || installed.as_ref().map(json::descriptors);
        if let Some(installed) = &installed {
            if self.human() {
                for error in installed.errors() {
                    self.terminal.write_diagnostic(error.as_ref())?;
                }
                for line in installed.changes() {
                    self.terminal.write_stdout_line(&line)?;
                }
            }
            if let Err(failed) = installed.verdict() {
                self.write_json(&json::Document {
                    descriptors: descriptors(),
                    ..json::Document::default()
                })?;
                return Err(failed.into());
            }
        }

        let VettedProject {
            project,
            outcomes,
            loaded,
        } = vet_project(env.fs, env.store, env.cwd)?;
        let vetted = vet::verdict(Some(&project), &outcomes, false);
        if self.human() {
            vet::write_findings(&self.terminal, Some(&project), &outcomes, deny_warnings)?;
            if vetted.is_err() {
                vet::write_statuses(&self.terminal, Some(&project), &outcomes)?;
            }
        }
        let vet_document = || Some(VetDocument::new(Some(&project), &outcomes, deny_warnings));
        if let Err(failed) = vetted {
            self.write_json(&json::Document {
                descriptors: descriptors(),
                vet: vet_document(),
                ..json::Document::default()
            })?;
            return Err(NotVetted { failed }.into());
        }

        let plan = Arc::new(Plan::new(
            loaded.root,
            loaded.validators,
            loaded.descriptors,
        ));
        let jobs = self
            .args
            .jobs
            .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN));
        let ran = run_all(Arc::clone(&plan), jobs, interrupt(), |index, finished| {
            if !self.human() {
                return Ok(());
            }
            let status = report::status(plan.job(index), finished);
            self.terminal.write_stdout_line(&status)
        })
        .await?;
        let evaluated = (!ran.interrupted).then(|| {
            let verdicts = verdicts(&plan, &ran.results);
            evaluate_all(&loaded.specs, &verdicts)
        });

        if self.human() {
            for (job, finished) in plan.jobs().zip(&ran.results) {
                match finished {
                    Some(Err(failure)) => {
                        self.terminal
                            .write_diagnostic(&Unrunnable::new(job, failure))?;
                    }
                    Some(Ok(run)) => {
                        let Outcome::Red(red) = run.outcome() else {
                            continue;
                        };
                        let requirement = job.validator.requirement();
                        let severity = evaluated
                            .as_deref()
                            .and_then(|evaluated| severity(evaluated, requirement));
                        let diagnostic = RedValidator::new(job, red, severity, deny_warnings);
                        self.terminal
                            .write_diagnostic_with(&diagnostic, &report::output(run))?;
                    }
                    None => {}
                }
            }
            for spec in evaluated.iter().flatten() {
                if !spec.evaluation.findings().is_empty() {
                    let findings = SpecFindings::new(spec, deny_warnings);
                    self.terminal.write_diagnostic(&findings)?;
                }
                self.terminal.write_stdout_line(&report::summary(spec))?;
            }
        }

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
        self.write_json(&json::Document {
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

    /// Write `document` to stdout, if that's the format asked for.
    fn write_json(&self, document: &json::Document<'_>) -> Result<()> {
        match self.args.format {
            OutputFormat::Json => self.terminal.write_stdout_line(&document.to_json()),
            OutputFormat::Human => Ok(()),
        }
    }
}

/// Complete when Ctrl-C is pressed, or never, if slcr can't listen for it.
async fn interrupt() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;

    fn args(argv: &[&str]) -> CheckArgs {
        let cli = Cli::parse_from([&["slcr", "check"], argv].concat());
        let Some(SlcrCommand::Check(args)) = cli.cmd() else {
            panic!("expected `check`");
        };
        args.clone()
    }

    #[test]
    fn check_takes_its_flags() {
        let defaults = args(&[]);
        assert_eq!(defaults.jobs, None);
        assert!(!defaults.offline && !defaults.deny_warnings);
        assert_eq!(defaults.format, OutputFormat::Human);

        let given = args(&[
            "-j",
            "3",
            "--offline",
            "--deny-warnings",
            "--format",
            "json",
        ]);
        assert_eq!(given.jobs, NonZeroUsize::new(3));
        assert!(given.offline && given.deny_warnings);
        assert_eq!(given.format, OutputFormat::Json);
        assert_eq!(args(&["--jobs", "1"]).jobs, NonZeroUsize::new(1));
    }

    #[test]
    fn jobs_must_be_a_positive_whole_number() {
        assert_eq!(parse_jobs("8"), Ok(NonZeroUsize::new(8).unwrap()));
        assert_eq!(parse_jobs("0"), Err("it must be at least 1".to_owned()));
        for invalid in ["-1", "two", "1.5", ""] {
            assert_eq!(
                parse_jobs(invalid),
                Err("expected a whole number of at least 1".to_owned()),
                "{invalid}"
            );
        }
        assert!(Cli::try_parse_from(["slcr", "check", "-j", "0"]).is_err());
    }

    #[test]
    fn check_takes_no_files() {
        assert!(Cli::try_parse_from(["slcr", "check", "SPEC.slcr.toml"]).is_err());
    }
}
