use miette::Diagnostic;
use thiserror::Error;

/// The error a stubbed command returns until it is implemented.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`slcr {command}` is not implemented yet")]
pub struct NotImplemented {
    command: &'static str,
}

impl NotImplemented {
    pub fn new(command: &'static str) -> Self {
        Self { command }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_command() {
        let err = NotImplemented::new("render");
        assert_eq!(err.to_string(), "`slcr render` is not implemented yet");
    }
}
