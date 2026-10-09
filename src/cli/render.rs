use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Render a specification as Markdown to `SPEC.md`.
pub struct Render {
    terminal: Terminal,
}

impl Render {
    pub fn new(terminal: Terminal) -> Self {
        Self { terminal }
    }

    /// A stub until the command is implemented.
    pub fn dispatch(self) -> Result<()> {
        Err(NotImplemented::new("render").into())
    }
}
