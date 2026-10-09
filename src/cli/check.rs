use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Report the invariants a specification breaks.
pub struct Check {
    terminal: Terminal,
}

impl Check {
    pub fn new(terminal: Terminal) -> Self {
        Self { terminal }
    }

    /// A stub until the command is implemented.
    pub fn dispatch(self) -> Result<()> {
        Err(NotImplemented::new("check").into())
    }
}
