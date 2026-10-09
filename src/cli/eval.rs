use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Report whether the specification's requirements are satisfied, from the
/// verdicts its validators reported.
pub struct Eval {
    terminal: Terminal,
}

impl Eval {
    pub fn new(terminal: Terminal) -> Self {
        Self { terminal }
    }

    /// A stub until the command is implemented.
    pub fn dispatch(self) -> Result<()> {
        Err(NotImplemented::new("eval").into())
    }
}
