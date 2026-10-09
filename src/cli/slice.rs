use miette::Result;

use super::NotImplemented;
use crate::Terminal;

/// Write the slice for one requirement: the smallest subgraph an agent
/// needs to implement or verify it.
pub struct Slice {
    terminal: Terminal,
}

impl Slice {
    pub fn new(terminal: Terminal) -> Self {
        Self { terminal }
    }

    /// A stub until the command is implemented.
    pub fn dispatch(self) -> Result<()> {
        Err(NotImplemented::new("slice").into())
    }
}
