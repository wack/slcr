//! A validator's status line, as `slcr check` writes it and the library
//! reports it while a check runs.

use std::fmt::{self, Display};

use super::schedule::{Finished, Job};
use crate::run::process::{Outcome, Red};

/// A validator as `check` names it: its file, its position in the file,
/// what it checks, and its tool, e.g.
/// `src/.validators.slcr.toml validator 1 (todo-api/REQ-002, cargo-nextest)`.
pub(crate) struct Name<'a>(pub(crate) Job<'a>);

impl Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Job {
            file,
            position,
            validator,
        } = self.0;
        let path = file.file().shown().display();
        write!(
            f,
            "{path} validator {position} ({}",
            validator.requirement()
        )?;
        if let Some(criterion) = validator.criterion() {
            write!(f, " {criterion}")?;
        }
        write!(f, ", {})", validator.tool())
    }
}

/// One validator's status, which displays as the line `slcr check`
/// writes for it, e.g.
/// `src/.validators.slcr.toml validator 1 (todo-api/REQ-002, cargo-nextest): green`:
/// `green`, `red` and why, `could not be run`, or, if it hadn't finished
/// when the check was interrupted, `interrupted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorStatus {
    line: String,
}

impl ValidatorStatus {
    /// The status of `job`, which ended as `finished`, if it did.
    pub(crate) fn new(job: Job<'_>, finished: Option<&Finished>) -> Self {
        let outcome = match finished {
            None => "interrupted".to_owned(),
            Some(Err(_)) => "could not be run".to_owned(),
            Some(Ok(run)) => match run.outcome() {
                Outcome::Green => "green".to_owned(),
                Outcome::Red(red) => format!("red ({})", why(red)),
            },
        };
        Self {
            line: format!("{}: {outcome}", Name(job)),
        }
    }

    /// The line `slcr check` writes.
    pub fn as_str(&self) -> &str {
        &self.line
    }
}

impl Display for ValidatorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line)
    }
}

/// Why a validator is red, in a few words.
pub(crate) fn why(red: &Red) -> String {
    match red {
        Red::ExitCode(code) => format!("exit {code}"),
        Red::Terminated {
            signal: Some(signal),
        } => format!("killed by signal {signal}"),
        Red::Terminated { signal: None } => "terminated".to_owned(),
        Red::TimedOut => "timed out".to_owned(),
        Red::Lost { reason } => format!("lost track of it: {reason}"),
    }
}
