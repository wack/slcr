use std::path::PathBuf;

use clap::Args;
use miette::{Diagnostic, IntoDiagnostic, Result};
use thiserror::Error;

use super::block_on;
use super::findings::FileFindings;
use crate::Terminal;
use crate::fs::FileSystem;
use crate::query::SpecGraph;
use crate::render;
use crate::slice::members;
use crate::spec::file::{DEFAULT_FILE, RequirementsFile};
use crate::spec::id::RequirementId;

/// The arguments to `slcr slice`.
#[derive(Args, Clone, Debug)]
pub struct SliceArgs {
    /// The requirement to slice, e.g. `REQ-042`.
    #[arg(value_name = "REQUIREMENT")]
    requirement: RequirementId,

    /// The requirements file to slice: `.yaml`, `.yml`, `.json`, or `.toml`.
    #[arg(value_name = "FILE", default_value = DEFAULT_FILE)]
    file: PathBuf,
}

/// Write the slice for one requirement: the smallest subgraph an agent
/// needs to implement or verify it, rendered as Markdown.
///
/// The specification must break none of its invariants; its findings,
/// warnings included, are reported on stderr as `slcr check` reports them.
/// The slice is written to stdout.
pub struct Slice {
    terminal: Terminal,
    args: SliceArgs,
}

impl Slice {
    pub fn new(terminal: Terminal, args: SliceArgs) -> Self {
        Self { terminal, args }
    }

    pub fn dispatch(self) -> Result<()> {
        let SliceArgs {
            requirement,
            file: path,
        } = self.args;
        let file = RequirementsFile::new(path.clone())?;

        tracing::debug!(path = %path.display(), %requirement, "slicing a requirements file");
        let fs = FileSystem::new()?;
        let unchecked = fs.load_file(file.unchecked())?;
        let report = unchecked.report();
        if !report.is_clean() {
            let findings = FileFindings::new(&path, &report, false);
            self.terminal.write_diagnostic(&findings)?;
        }
        let Ok(document) = unchecked.into_checked() else {
            return Err(SliceFailed { path }.into());
        };
        let known = document
            .requirements()
            .iter()
            .any(|candidate| *candidate.id() == requirement);
        if !known {
            return Err(UnknownRequirement {
                requirement,
                file: path.display().to_string(),
            }
            .into());
        }

        let members = block_on(async {
            let graph = SpecGraph::new(&document).await.into_diagnostic()?;
            members(&graph, requirement).await.into_diagnostic()
        })??;
        self.terminal
            .write_stdout(&render::slice(&document, &members))
    }
}

/// The error returned when the specification breaks its invariants, which
/// have been reported.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("could not slice {}, which breaks its invariants", .path.display())]
#[diagnostic(help("fix the violations reported above, then slice it again"))]
pub struct SliceFailed {
    path: PathBuf,
}

/// The error returned when the specification has no such requirement.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{file} has no requirement {requirement}")]
#[diagnostic(help("`slcr query 'MATCH (r:Requirement) RETURN r, r.title'` lists them"))]
pub struct UnknownRequirement {
    requirement: RequirementId,
    file: String,
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure returns a large `Result`; unavoidable here.
    #![allow(clippy::result_large_err)]

    use clap::Parser;
    use figment::Jail;

    use super::*;
    use crate::Cli;
    use crate::config::command::SlcrCommand;
    use crate::terminal::render_plain;

    const TODO_API_YAML: &str = include_str!("../../tests/fixtures/todo-api.spec.yaml");

    /// The arguments `slcr` parses `slice` and then `args` into.
    fn parse(args: &[&str]) -> Result<SliceArgs, clap::Error> {
        let argv = ["slcr", "slice"].iter().chain(args);
        match Cli::try_parse_from(argv)?.cmd() {
            Some(SlcrCommand::Slice(args)) => Ok(args.clone()),
            _ => panic!("expected `slice`"),
        }
    }

    /// Run `slcr slice` with `args`.
    fn run(args: &[&str]) -> Result<()> {
        let argv = ["slcr", "--enable-colors", "never", "slice"]
            .iter()
            .chain(args);
        let cli = Cli::parse_from(argv);
        let terminal = Terminal::new(&cli);
        cli.cmd().clone().unwrap().dispatch(terminal)
    }

    #[test]
    fn slice_defaults_to_the_well_known_file() {
        let args = parse(&["REQ-042"]).unwrap();
        assert_eq!(args.requirement, RequirementId::new(42));
        assert_eq!(args.file, PathBuf::from(DEFAULT_FILE));
    }

    #[test]
    fn slice_accepts_a_file() {
        let args = parse(&["REQ-1000", "specs/api.json"]).unwrap();
        assert_eq!(args.requirement, RequirementId::new(1000));
        assert_eq!(args.file, PathBuf::from("specs/api.json"));
    }

    #[test]
    fn slice_requires_a_requirement_id() {
        let err = parse(&[]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        for id in ["REQ-42", "req-042", "SEC-001", "42"] {
            let err = parse(&[id]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{id}");
        }
    }

    #[test]
    fn slice_reads_the_well_known_file() {
        Jail::expect_with(|jail| {
            jail.create_file("SPEC.slcr.yml", TODO_API_YAML)?;
            run(&["REQ-005"]).unwrap();
            Ok(())
        });
    }

    #[test]
    fn slice_rejects_a_requirement_the_specification_lacks() {
        Jail::expect_with(|jail| {
            jail.create_file("todo-api.yaml", TODO_API_YAML)?;
            let err = run(&["REQ-099", "todo-api.yaml"]).unwrap_err();
            assert_eq!(
                err.downcast_ref::<UnknownRequirement>(),
                Some(&UnknownRequirement {
                    requirement: RequirementId::new(99),
                    file: "todo-api.yaml".to_owned(),
                })
            );
            Ok(())
        });
    }

    #[test]
    fn errors_render_with_their_help() {
        let cases: [(&dyn Diagnostic, &str); 2] = [
            (
                &SliceFailed {
                    path: PathBuf::from("SPEC.slcr.yml"),
                },
                "  × could not slice SPEC.slcr.yml, which breaks its invariants\n  \
                 help: fix the violations reported above, then slice it again\n",
            ),
            (
                &UnknownRequirement {
                    requirement: RequirementId::new(99),
                    file: "SPEC.slcr.yml".to_owned(),
                },
                "  × SPEC.slcr.yml has no requirement REQ-099\n  \
                 help: `slcr query 'MATCH (r:Requirement) RETURN r, r.title'` lists them\n",
            ),
        ];
        for (diagnostic, expected) in cases {
            assert_eq!(render_plain(diagnostic), expected);
        }
    }
}
