use std::num::NonZeroUsize;

use clap::{Args, ValueEnum};
use miette::{IntoDiagnostic, Result};

use super::vet::{self, Outcome, ProjectOutcome};
use super::{ProjectArgs, block_on};
use crate::Terminal;
use crate::check::flow::{self, Conclusion, Observer, Options};
use crate::check::json::Document;
use crate::check::status::ValidatorStatus;
use crate::check::verdict::severity;
use crate::fs::FileSystem;
use crate::run::process::Outcome as RunOutcome;
use crate::tools::install::Installed;
use crate::tools::{Environment, source::Fetcher, store::DescriptorStore};

mod report;

use report::{RedValidator, SpecFindings, Unrunnable};

/// The arguments to `slcr check`.
#[derive(Args, Clone, Debug)]
pub struct CheckArgs {
    /// Run at most N validators at once [default: the number of CPUs].
    /// With 1, they run one at a time, in the order they are reported.
    #[arg(short, long, value_name = "N", value_parser = parse_jobs)]
    jobs: Option<NonZeroUsize>,

    #[command(flatten)]
    project: ProjectArgs,

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
/// The project is the one in `--slcr-dir`, or else the one that holds the
/// working directory, and its validators run against `--project-root`,
/// which defaults to the directory of `SLCR.toml`.
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
        let project = self.args.project.locate(&cwd)?;
        let env = Environment {
            fs: &fs,
            store: &store,
            fetcher: &fetcher,
            cwd: &cwd,
            project,
        };
        block_on(self.check(&env))?
    }

    async fn check(&self, env: &Environment<'_>) -> Result<()> {
        let options = Options {
            install: !self.args.offline,
            jobs: self
                .args
                .jobs
                .unwrap_or_else(|| crate::check::Options::default().jobs),
            deny_warnings: self.args.deny_warnings,
            path: None,
        };
        let mut presenter = Presenter {
            terminal: &self.terminal,
            format: self.args.format,
            deny_warnings: self.args.deny_warnings,
        };
        flow::check(env, &options, interrupt(), &mut presenter).await
    }
}

/// Writes what a check does as it happens: in human form, the statuses
/// and summaries on stdout and the diagnostics on stderr, or else one JSON
/// document on stdout.
struct Presenter<'a> {
    terminal: &'a Terminal,
    format: OutputFormat,
    deny_warnings: bool,
}

impl Presenter<'_> {
    fn human(&self) -> bool {
        self.format == OutputFormat::Human
    }
}

impl Observer for Presenter<'_> {
    fn installed(&mut self, installed: &Installed) -> Result<()> {
        if self.human() {
            for error in installed.errors() {
                self.terminal.write_diagnostic(error.as_ref())?;
            }
            for line in installed.changes() {
                self.terminal.write_stdout_line(&line)?;
            }
        }
        Ok(())
    }

    fn vetted(
        &mut self,
        project: &ProjectOutcome,
        outcomes: &[Outcome],
        passed: bool,
    ) -> Result<()> {
        if self.human() {
            vet::write_findings(self.terminal, Some(project), outcomes, self.deny_warnings)?;
            if !passed {
                vet::write_statuses(self.terminal, Some(project), outcomes)?;
            }
        }
        Ok(())
    }

    fn status(&mut self, status: &ValidatorStatus) -> Result<()> {
        if self.human() {
            self.terminal.write_stdout_line(status.as_str())?;
        }
        Ok(())
    }

    fn concluded(&mut self, conclusion: &Conclusion<'_>) -> Result<()> {
        if !self.human() {
            return Ok(());
        }
        let deny_warnings = self.deny_warnings;
        for (job, finished) in conclusion.plan.jobs().zip(conclusion.results) {
            match finished {
                Some(Err(failure)) => {
                    self.terminal
                        .write_diagnostic(&Unrunnable::new(job, failure))?;
                }
                Some(Ok(run)) => {
                    let RunOutcome::Red(red) = run.outcome() else {
                        continue;
                    };
                    let requirement = job.validator.requirement();
                    let severity = conclusion
                        .evaluated
                        .and_then(|evaluated| severity(evaluated, requirement));
                    let diagnostic = RedValidator::new(job, red, severity, deny_warnings);
                    self.terminal
                        .write_diagnostic_with(&diagnostic, &report::output(run))?;
                }
                None => {}
            }
        }
        for spec in conclusion.evaluated.into_iter().flatten() {
            if !spec.evaluation.findings().is_empty() {
                let findings = SpecFindings::new(spec, deny_warnings);
                self.terminal.write_diagnostic(&findings)?;
            }
            self.terminal.write_stdout_line(&report::summary(spec))?;
        }
        Ok(())
    }

    fn reported(&mut self, document: &Document<'_>) -> Result<()> {
        match self.format {
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
