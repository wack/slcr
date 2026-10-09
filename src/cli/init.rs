use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Create a specification with its root and Glossary sections.
pub struct Init {
    terminal: Terminal,
}

impl Init {
    pub fn new(terminal: Terminal) -> Self {
        Self { terminal }
    }

    /// A stub until the command is implemented.
    pub fn dispatch(self) -> Result<()> {
        Err(NotImplemented::new("init").into())
    }
}
