use clap::Subcommand;
use miette::Result;

use crate::cli::{
    Check, CheckArgs, Eval, Init, InitArgs, Query, QueryArgs, Render, RenderArgs, Slice, SliceArgs,
    Tools, ToolsArgs, Version,
};
use crate::terminal::Terminal;

/// A `SlcrCommand` is one of the top-level commands accepted by
/// the slcr CLI.
#[derive(Subcommand, Clone)]
pub enum SlcrCommand {
    /// Create a specification with its root and Glossary sections
    Init(InitArgs),
    /// Render a specification as Markdown, to `SPEC.md` by default
    Render(RenderArgs),
    /// Query a specification with GQL
    Query(QueryArgs),
    /// Write the slice for one requirement: the context an agent needs to implement it
    Slice(SliceArgs),
    /// Report whether the specification's requirements are satisfied (not yet implemented)
    Eval,
    /// Report the invariants requirements files break
    Check(CheckArgs),
    /// Declare the external tools validators run, in the project manifest `SLCR.toml`
    Tools(ToolsArgs),
    /// Print the CLI version and exit
    Version,
}

impl SlcrCommand {
    /// dispatch the user-provided arguments to the command handler.
    pub fn dispatch(self, console: Terminal) -> Result<()> {
        match self {
            Self::Init(args) => Init::new(console, args).dispatch(),
            Self::Render(args) => Render::new(console, args).dispatch(),
            Self::Query(args) => Query::new(console, args).dispatch(),
            Self::Slice(args) => Slice::new(console, args).dispatch(),
            Self::Eval => Eval::new(console).dispatch(),
            Self::Check(args) => Check::new(console, args).dispatch(),
            Self::Tools(args) => Tools::new(console, args).dispatch(),
            Self::Version => Version::new(console).dispatch(),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::SlcrCommand;
    use crate::{Cli, Terminal};

    #[test]
    fn version_subcommand_parses() {
        let cli = Cli::parse_from(["slcr", "version"]);
        assert!(matches!(cli.cmd(), Some(SlcrCommand::Version)));
    }

    #[test]
    fn stubbed_subcommands_parse() {
        let cli = Cli::parse_from(["slcr", "eval"]);
        assert!(matches!(cli.cmd(), Some(SlcrCommand::Eval)));
    }

    #[test]
    fn render_parses_with_or_without_a_file() {
        for argv in [
            &["slcr", "render"][..],
            &["slcr", "render", "todo-api.spec.json"],
        ] {
            let cli = Cli::parse_from(argv);
            assert!(
                matches!(cli.cmd(), Some(SlcrCommand::Render(_))),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn init_parses_with_or_without_a_file() {
        for argv in [
            &["slcr", "init"][..],
            &["slcr", "init", "todo-api.spec.json"],
        ] {
            let cli = Cli::parse_from(argv);
            assert!(matches!(cli.cmd(), Some(SlcrCommand::Init(_))), "{argv:?}");
        }
    }

    #[test]
    fn query_parses_a_query_with_or_without_a_file() {
        for argv in [
            &["slcr", "query", "RETURN 1"][..],
            &["slcr", "query", "RETURN 1", "todo-api.spec.json"],
        ] {
            let cli = Cli::parse_from(argv);
            assert!(matches!(cli.cmd(), Some(SlcrCommand::Query(_))), "{argv:?}");
        }
    }

    #[test]
    fn slice_parses_a_requirement_with_or_without_a_file() {
        for argv in [
            &["slcr", "slice", "REQ-001"][..],
            &["slcr", "slice", "REQ-001", "todo-api.spec.json"],
        ] {
            let cli = Cli::parse_from(argv);
            assert!(matches!(cli.cmd(), Some(SlcrCommand::Slice(_))), "{argv:?}");
        }
    }

    #[test]
    fn check_parses_its_files() {
        let cli = Cli::parse_from(["slcr", "check", "a.yaml", "b.json"]);
        assert!(matches!(cli.cmd(), Some(SlcrCommand::Check(_))));
    }

    #[test]
    fn stubbed_subcommands_report_that_they_are_not_implemented() {
        let cli = Cli::parse_from(["slcr", "eval"]);
        let terminal = Terminal::new(&cli);
        let err = cli.cmd().clone().unwrap().dispatch(terminal).unwrap_err();
        assert_eq!(err.to_string(), "`slcr eval` is not implemented yet");
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        // `Cli` isn't `Debug`, so match manually instead of `unwrap_err`.
        let err = match Cli::try_parse_from(["slcr", "frobnicate"]) {
            Err(err) => err,
            Ok(_) => panic!("unknown subcommands must fail to parse"),
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }
}
