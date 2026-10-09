use clap::Parser;
use derive_getters::Getters;
use tracing::level_filters::LevelFilter;

use super::colors::EnableColors;
use super::command::SlcrCommand;

/// slcr command-line interface.
#[derive(Getters, Parser)]
pub struct Cli {
    /// The subcommand to execute
    #[command(subcommand)]
    cmd: Option<SlcrCommand>,

    /// Whether to color the output
    #[arg(long, global = true, value_enum, default_value_t=EnableColors::default())]
    enable_colors: EnableColors,

    /// Sets the maximum log level. Defaults to INFO. Options are
    /// 'trace', 'debug', 'info', 'warn', 'error', and 'off'. 'off' implies no logger will occur.
    /// Options are case-insensitive.
    #[arg(long, env, global = true, default_value_t = LevelFilter::INFO)]
    log_level: LevelFilter,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_subcommand_parses_to_none() {
        let cli = Cli::parse_from(["slcr"]);
        assert!(cli.cmd().is_none());
    }

    #[test]
    fn colors_default_to_auto() {
        let cli = Cli::parse_from(["slcr"]);
        assert!(*cli.enable_colors() == EnableColors::Auto);
    }

    #[test]
    fn enable_colors_is_global() {
        // The flag is accepted after the subcommand, too.
        let cli = Cli::parse_from(["slcr", "version", "--enable-colors", "never"]);
        assert!(*cli.enable_colors() == EnableColors::Never);
    }

    #[test]
    fn log_level_defaults_to_info() {
        let cli = Cli::parse_from(["slcr"]);
        assert_eq!(*cli.log_level(), LevelFilter::INFO);
    }

    #[test]
    fn log_level_is_case_insensitive_and_global() {
        let cli = Cli::parse_from(["slcr", "version", "--log-level", "DEBUG"]);
        assert_eq!(*cli.log_level(), LevelFilter::DEBUG);
        let cli = Cli::parse_from(["slcr", "--log-level", "off"]);
        assert_eq!(*cli.log_level(), LevelFilter::OFF);
    }

    #[test]
    fn unknown_log_level_is_rejected() {
        assert!(Cli::try_parse_from(["slcr", "--log-level", "loud"]).is_err());
    }

    #[test]
    fn unknown_color_preference_is_rejected() {
        assert!(Cli::try_parse_from(["slcr", "--enable-colors", "sometimes"]).is_err());
    }
}
