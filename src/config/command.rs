use clap::Subcommand;
use miette::Result;

use crate::cli::Version;
use crate::terminal::Terminal;

/// A `SlcrCommand` is one of the top-level commands accepted by
/// the slcr CLI.
#[derive(Subcommand, Clone)]
pub enum SlcrCommand {
    /// Print the CLI version and exit
    Version,
}

impl SlcrCommand {
    /// dispatch the user-provided arguments to the command handler.
    pub fn dispatch(self, console: Terminal) -> Result<()> {
        match self {
            Self::Version => Version::new(console).dispatch(),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::SlcrCommand;
    use crate::Cli;

    #[test]
    fn version_subcommand_parses() {
        let cli = Cli::parse_from(["slcr", "version"]);
        assert!(matches!(cli.cmd(), Some(SlcrCommand::Version)));
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
