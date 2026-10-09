use logging::setup_logger;
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, IntoDiagnostic, Result};

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
        miette::set_hook(Box::new(move |_| Box::new(report_handler(allow_color))))?;

        Ok(())
    }

    /// Write a diagnostic to stderr, rendered as the error hook renders
    /// errors, followed by a blank line.
    pub fn write_diagnostic(&self, diagnostic: &dyn Diagnostic) -> Result<()> {
        let rendered = render(diagnostic, report_handler(self.stderr.allow_color()))?;
        let term = self.stderr.term();
        term.write_line(rendered.trim_end()).into_diagnostic()?;
        term.write_line("").into_diagnostic()
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

/// The handler that renders diagnostics: miette's graphical handler, with a
/// colored theme when color is allowed and a plain Unicode one otherwise.
fn report_handler(allow_color: bool) -> GraphicalReportHandler {
    // TODO: Add brand colors to the colored theme.
    let theme = if allow_color {
        GraphicalTheme::unicode()
    } else {
        GraphicalTheme::unicode_nocolor()
    };
    GraphicalReportHandler::new_themed(theme)
}

/// Render `diagnostic` with `handler`.
fn render(diagnostic: &dyn Diagnostic, handler: GraphicalReportHandler) -> Result<String> {
    let mut rendered = String::new();
    handler
        .render_report(&mut rendered, diagnostic)
        .into_diagnostic()?;
    Ok(rendered)
}

/// Render `diagnostic` as plain text at a fixed width, as tests expect it.
#[cfg(test)]
pub(crate) fn render_plain(diagnostic: &dyn Diagnostic) -> String {
    render(diagnostic, report_handler(false).with_width(80)).unwrap()
}

#[cfg(test)]
mod tests {
    use miette::miette;

    use super::*;

    #[test]
    fn plain_rendering_has_no_color_codes() {
        let report = miette!(help = "try again", "it broke");
        let rendered = render_plain(report.as_ref());
        assert_eq!(rendered, "  × it broke\n  help: try again\n");
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn colored_rendering_uses_color_codes() {
        let report = miette!("it broke");
        let rendered = render(report.as_ref(), report_handler(true)).unwrap();
        assert!(rendered.contains('\u{1b}'), "{rendered:?}");
    }
}
