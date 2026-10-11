use std::fmt::{self, Display};
use std::str::FromStr;

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The algorithm prefix every pin carries.
const PREFIX: &str = "blake3:";

/// A descriptor's pin: the BLAKE3 hash of its bytes, written as `blake3:`
/// followed by 64 lowercase hex characters.
///
/// The pin is authoritative. Bytes that don't hash to it are never run, and
/// the descriptor store files each descriptor under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct Pin(blake3::Hash);

/// The error returned when a string is not a valid [Pin].
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("{0:?} is not a valid pin")]
#[diagnostic(help("a pin is `blake3:` followed by 64 lowercase hex characters"))]
pub struct InvalidPin(String);

impl Pin {
    /// The pin of `bytes`.
    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes))
    }

    /// Whether `bytes` hash to this pin.
    pub(crate) fn matches(&self, bytes: &[u8]) -> bool {
        // `blake3::Hash`'s equality is constant-time.
        Self::of(bytes) == *self
    }

    /// The hash alone, as 64 lowercase hex characters.
    pub(crate) fn hex(&self) -> impl Display {
        self.0.to_hex()
    }
}

impl FromStr for Pin {
    type Err = InvalidPin;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidPin(value.to_owned());
        let hex = value.strip_prefix(PREFIX).ok_or_else(invalid)?;
        // `from_hex` also accepts uppercase, which the schema doesn't.
        let lowercase = hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !lowercase {
            return Err(invalid());
        }
        blake3::Hash::from_hex(hex).map(Self).map_err(|_| invalid())
    }
}

impl TryFrom<String> for Pin {
    type Error = InvalidPin;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Pin> for String {
    fn from(pin: Pin) -> Self {
        pin.to_string()
    }
}

impl Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.0.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BLAKE3 of the empty input.
    const EMPTY: &str = "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn a_pin_is_the_blake3_hash_of_the_bytes() {
        assert_eq!(Pin::of(b"").to_string(), EMPTY);
        assert!(Pin::of(b"").matches(b""));
        assert!(!Pin::of(b"").matches(b"x"));
    }

    #[test]
    fn a_pin_round_trips_through_its_string() {
        let pin: Pin = EMPTY.parse().unwrap();
        assert_eq!(pin, Pin::of(b""));
        assert_eq!(pin.to_string(), EMPTY);
        assert_eq!(
            pin.hex().to_string(),
            EMPTY.strip_prefix("blake3:").unwrap()
        );
    }

    #[test]
    fn only_lowercase_blake3_pins_parse() {
        let hex = EMPTY.strip_prefix("blake3:").unwrap();
        for invalid in [
            hex.to_owned(),
            format!("BLAKE3:{hex}"),
            format!("xxh3:{hex}"),
            format!("blake3:{}", hex.to_uppercase()),
            format!("blake3:{}", &hex[1..]),
            format!("blake3:{hex}0"),
            format!("blake3:{}g", &hex[1..]),
            String::new(),
        ] {
            assert_eq!(
                invalid.parse::<Pin>(),
                Err(InvalidPin(invalid.clone())),
                "{invalid}"
            );
        }
    }
}
