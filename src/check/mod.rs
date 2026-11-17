//! Checking a project: running its validators and evaluating each
//! requirement from their results, as `slcr check` does.
//!
//! [crate::Project::check] checks a project on disk on its caller's
//! runtime, with the [Options] given, telling its caller each
//! [ValidatorStatus] as `slcr check` writes it, and returns the [Report]
//! `slcr check --format json` writes. [Report] also reads that document
//! back, for a program that stores what `slcr check` reported elsewhere.

use std::{ffi::OsString, num::NonZeroUsize};

pub use report::*;
pub use status::ValidatorStatus;

/// Running the whole of a check, observed by the CLI or the library.
pub(crate) mod flow;
/// The document `slcr check --format json` writes.
pub(crate) mod json;
/// The document `slcr check --format json` writes, read back.
mod report;
/// Running every validator, `-j` at a time.
pub(crate) mod schedule;
/// A validator's status line.
pub(crate) mod status;
/// From the validators' results to each requirement's satisfaction.
pub(crate) mod verdict;

/// How [crate::Project::check] checks a project, as `slcr check
/// --offline` does with the corresponding flags.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// How many validators may run at once, as `--jobs` says. Defaults to
    /// the number of CPUs.
    pub jobs: NonZeroUsize,
    /// Whether a warning fails the check, as `--deny-warnings` says.
    /// Defaults to `false`.
    pub deny_warnings: bool,
    /// The `PATH` each tool's program is found on, and which each
    /// validator inherits, in place of the process's own, unless the
    /// tool's descriptor sets its own `env.PATH`. Defaults to none: the
    /// process's own `PATH`, as `slcr check` uses.
    pub path: Option<OsString>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jobs: std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
            deny_warnings: false,
            path: None,
        }
    }
}
