use console::{Term, colors_enabled, colors_enabled_stderr};

use crate::Cli;

/// A TermDestination references an output file, usually stdout.
pub(super) struct TermDestination {
    term: Term,
    allow_color: bool,
}

impl TermDestination {
    /// Getter for the terminal held by this object.
    pub(super) fn term(&self) -> &Term {
        &self.term
    }

    /// Getter for whether color is allowed.
    pub(super) fn allow_color(&self) -> bool {
        self.allow_color
    }

    pub(super) fn stdout(cli: &Cli) -> Self {
        let term = Term::stdout();
        // Respect the user's preference for color,
        // but fall back to inspecting the terminal for a tty
        // if no preference has been provided.
        let allow_color = cli
            .enable_colors()
            .color_preference()
            .unwrap_or_else(colors_enabled);
        TermDestination { term, allow_color }
    }

    /// This function is very similar to stdout, but using stderr
    /// instead.
    pub(super) fn stderr(cli: &Cli) -> Self {
        let term = Term::stderr();
        // Respect the user's preference for color,
        // but fall back to inspecting the terminal for a tty
        // if no preference has been provided.
        let allow_color = cli
            .enable_colors()
            .color_preference()
            .unwrap_or_else(colors_enabled_stderr);
        TermDestination { term, allow_color }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn explicit_preference_overrides_tty_detection() {
        let always = Cli::parse_from(["slcr", "--enable-colors", "always"]);
        assert!(TermDestination::stdout(&always).allow_color());
        assert!(TermDestination::stderr(&always).allow_color());

        let never = Cli::parse_from(["slcr", "--enable-colors", "never"]);
        assert!(!TermDestination::stdout(&never).allow_color());
        assert!(!TermDestination::stderr(&never).allow_color());
    }
}
