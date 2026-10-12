use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Run every validator, and report whether the specification's
/// requirements are satisfied.
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
