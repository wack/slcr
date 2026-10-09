use logging::setup_logger;
use miette::{GraphicalReportHandler, GraphicalTheme, IntoDiagnostic, Result};

use crate::Cli;

use dest::TermDestination;

mod dest;
mod logging;

pub struct Terminal {
    stdout: TermDestination,
    stderr: TermDestination,
}

impl Terminal {
    pub fn new(cli: &Cli) -> Self {
        // Check to see whether we should color the
        // terminal output.
        let stdout = TermDestination::stdout(cli);
        let stderr = TermDestination::stderr(cli);
        // Logs go to stderr, so they follow stderr's color preference.
        setup_logger(*cli.log_level(), stderr.allow_color());

        Self { stdout, stderr }
    }

    /// Sets the global error handler for Miette. This should be called
    /// close to `main`.
    ///
    /// Errors always render through miette's graphical handler (message,
    /// labeled source snippet, help). Only its theme follows the user's
    /// color preference for stderr: colored when allowed, plain Unicode
    /// otherwise. (miette's `DebugReportHandler` is not a "plain" mode — it
    /// prints the raw `Diagnostic { .. }` struct plus a note to enable the
    /// `fancy` feature, which this crate already enables.)
    pub fn set_error_hook(&self) -> Result<()> {
        let allow_color = self.stderr.allow_color();
        // Set the hook and coerce the `InstallError` into an `ErrorReport`
        miette::set_hook(Box::new(move |_| {
            // TODO: Add brand colors to the colored theme.
            let theme = if allow_color {
                GraphicalTheme::unicode()
            } else {
                GraphicalTheme::unicode_nocolor()
            };
            Box::new(GraphicalReportHandler::new_themed(theme))
        }))?;

        Ok(())
    }

    /// Write a single line to stdout.
    pub fn write_stdout_line(&self, line: &str) -> Result<()> {
        self.stdout.term().write_line(line).into_diagnostic()
    }

    /// Whether stdout may emit color, honoring the global `--enable-colors` flag.
    pub fn stdout_allows_color(&self) -> bool {
        self.stdout.allow_color()
    }

    pub fn print_version(&self, version: &'static str) -> Result<()> {
        let msg = format!("v{version}");
        self.stdout
            .term()
            .write_line(msg.as_str())
            .into_diagnostic()
    }
}
