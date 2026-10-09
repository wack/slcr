use std::{
    fmt::{self, Debug, Display},
    hash::Hash,
    marker::PhantomData,
    str::FromStr,
};

use miette::Diagnostic;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

/// The identifier of a Section node, e.g. `SEC-001`.
pub type SectionId = Id<kind::Section>;
/// The identifier of a Requirement node, e.g. `REQ-042`.
pub type RequirementId = Id<kind::Requirement>;
/// The identifier of a Term node, e.g. `TERM-013`.
pub type TermId = Id<kind::Term>;
/// The identifier of an Acceptance Criterion node, e.g. `AC-009`.
pub type CriterionId = Id<kind::Criterion>;

/// Marker types naming the kind of node an [Id] identifies. They are
/// uninhabited; they exist only at the type level.
pub mod kind {
    use super::Kind;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Section {}

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Requirement {}

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Term {}

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Criterion {}

    impl Kind for Section {
        const PREFIX: &'static str = "SEC";
        const NAME: &'static str = "section";
    }

    impl Kind for Requirement {
        const PREFIX: &'static str = "REQ";
        const NAME: &'static str = "requirement";
    }

    impl Kind for Term {
        const PREFIX: &'static str = "TERM";
        const NAME: &'static str = "term";
    }

    impl Kind for Criterion {
        const PREFIX: &'static str = "AC";
        const NAME: &'static str = "acceptance criterion";
    }
}

/// A kind of node, which determines the prefix of its IDs.
pub trait Kind: Copy + Ord + Hash {
    /// The prefix before the dash, e.g. "REQ".
    const PREFIX: &'static str;
    /// A human-readable name for error messages, e.g. "requirement".
    const NAME: &'static str;
}

/// A kind-prefixed, sequential node identifier such as `REQ-042`.
///
/// The number is spelled with exactly three zero-padded digits up to 999
/// and without padding from 1000 on, so every ID has exactly one spelling.
/// IDs order numerically rather than as strings: `REQ-999` sorts before
/// `REQ-1000`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id<K: Kind> {
    number: u32,
    kind: PhantomData<K>,
}

impl<K: Kind> Id<K> {
    pub fn new(number: u32) -> Self {
        Self {
            number,
            kind: PhantomData,
        }
    }

    /// The sequence number, without the prefix.
    pub fn number(self) -> u32 {
        self.number
    }
}

/// The error returned when a string is not a valid ID of the expected kind.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`{value}` is not a valid {kind} ID")]
#[diagnostic(help(
    "expected `{prefix}-` and then three digits, or four or more without a leading zero"
))]
pub struct InvalidId {
    value: String,
    kind: &'static str,
    prefix: &'static str,
}

impl<K: Kind> FromStr for Id<K> {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidId {
            value: value.to_owned(),
            kind: K::NAME,
            prefix: K::PREFIX,
        };
        let digits = value
            .strip_prefix(K::PREFIX)
            .and_then(|rest| rest.strip_prefix('-'))
            .ok_or_else(invalid)?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        // Enforce the single spelling: three digits, or more without padding.
        let canonical = digits.len() == 3 || (digits.len() > 3 && !digits.starts_with('0'));
        if !canonical {
            return Err(invalid());
        }
        // Only an absurdly long number can overflow.
        digits.parse().map(Self::new).map_err(|_| invalid())
    }
}

impl<K: Kind> Display for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{:03}", K::PREFIX, self.number)
    }
}

impl<K: Kind> Debug for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<K: Kind> Serialize for Id<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de, K: Kind> Deserialize<'de> for Id<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

/// The target of a `dependsOn` edge: a Section or a Requirement.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyId {
    Section(SectionId),
    Requirement(RequirementId),
}

/// The error returned when a string is neither a Section ID nor a
/// Requirement ID.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
#[error("`{0}` is not a valid section or requirement ID")]
#[diagnostic(help("`dependsOn` may only name sections (`SEC-001`) and requirements (`REQ-001`)"))]
pub struct InvalidDependencyId(String);

impl FromStr for DependencyId {
    type Err = InvalidDependencyId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self::Section)
            .or_else(|_| value.parse().map(Self::Requirement))
            .map_err(|_| InvalidDependencyId(value.to_owned()))
    }
}

impl Display for DependencyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Section(id) => Display::fmt(id, f),
            Self::Requirement(id) => Display::fmt(id, f),
        }
    }
}

impl Debug for DependencyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl Serialize for DependencyId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DependencyId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_digit_ids_parse() {
        assert_eq!("REQ-001".parse(), Ok(RequirementId::new(1)));
        assert_eq!("REQ-042".parse(), Ok(RequirementId::new(42)));
        assert_eq!("REQ-999".parse(), Ok(RequirementId::new(999)));
    }

    #[test]
    fn zero_is_a_valid_number() {
        // The schema's pattern admits `000`.
        assert_eq!("SEC-000".parse(), Ok(SectionId::new(0)));
    }

    #[test]
    fn long_ids_parse_without_padding() {
        assert_eq!("REQ-1000".parse(), Ok(RequirementId::new(1000)));
        assert_eq!("TERM-123456".parse(), Ok(TermId::new(123_456)));
    }

    #[test]
    fn every_prefix_parses() {
        assert!("SEC-001".parse::<SectionId>().is_ok());
        assert!("REQ-001".parse::<RequirementId>().is_ok());
        assert!("TERM-001".parse::<TermId>().is_ok());
        assert!("AC-001".parse::<CriterionId>().is_ok());
    }

    #[test]
    fn non_canonical_spellings_are_rejected() {
        for value in ["REQ-1", "REQ-01", "REQ-0001", "REQ-0999", "REQ-01000"] {
            assert!(value.parse::<RequirementId>().is_err(), "{value}");
        }
    }

    #[test]
    fn malformed_ids_are_rejected() {
        for value in [
            "",
            "REQ",
            "REQ-",
            "REQ001",
            "REQ_001",
            "req-001",
            "REQ-abc",
            "REQ-00a",
            "REQ-+01",
            "REQ--01",
            " REQ-001",
            "REQ-001 ",
            "REQ-١٢٣",
        ] {
            assert!(value.parse::<RequirementId>().is_err(), "{value:?}");
        }
    }

    #[test]
    fn the_prefix_must_match_the_kind() {
        assert!("SEC-001".parse::<RequirementId>().is_err());
        assert!("REQ-001".parse::<TermId>().is_err());
        assert!("TERM-001".parse::<CriterionId>().is_err());
        assert!("AC-001".parse::<SectionId>().is_err());
    }

    #[test]
    fn overflowing_numbers_are_rejected() {
        assert!("REQ-99999999999".parse::<RequirementId>().is_err());
    }

    #[test]
    fn invalid_id_names_the_value_and_kind() {
        let err = "REQ-1".parse::<RequirementId>().unwrap_err();
        assert_eq!(err.to_string(), "`REQ-1` is not a valid requirement ID");
    }

    #[test]
    fn display_uses_the_canonical_spelling() {
        assert_eq!(RequirementId::new(7).to_string(), "REQ-007");
        assert_eq!(CriterionId::new(999).to_string(), "AC-999");
        assert_eq!(SectionId::new(1000).to_string(), "SEC-1000");
        assert_eq!(format!("{:?}", TermId::new(13)), "TERM-013");
    }

    #[test]
    fn ids_order_numerically() {
        let mut ids: Vec<RequirementId> = ["REQ-1000", "REQ-999", "REQ-002"]
            .into_iter()
            .map(|value| value.parse().unwrap())
            .collect();
        ids.sort();
        let spelled: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(spelled, ["REQ-002", "REQ-999", "REQ-1000"]);
    }

    #[test]
    fn ids_round_trip_through_serde_as_strings() {
        let id = RequirementId::new(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""REQ-042""#);
        assert_eq!(serde_json::from_str::<RequirementId>(&json).unwrap(), id);
    }

    #[test]
    fn invalid_ids_fail_to_deserialize() {
        let err = serde_json::from_str::<RequirementId>(r#""SEC-001""#).unwrap_err();
        assert!(err.to_string().contains("not a valid requirement ID"));
        assert!(serde_json::from_str::<RequirementId>("42").is_err());
    }

    #[test]
    fn dependency_ids_accept_sections_and_requirements() {
        assert_eq!(
            "SEC-002".parse(),
            Ok(DependencyId::Section(SectionId::new(2)))
        );
        assert_eq!(
            "REQ-1000".parse(),
            Ok(DependencyId::Requirement(RequirementId::new(1000)))
        );
    }

    #[test]
    fn dependency_ids_reject_other_kinds() {
        for value in ["TERM-001", "AC-001", "REQ-1", "SEC"] {
            assert_eq!(
                value.parse::<DependencyId>(),
                Err(InvalidDependencyId(value.to_owned()))
            );
        }
    }

    #[test]
    fn dependency_ids_round_trip_through_serde() {
        let ids = vec![
            DependencyId::Requirement(RequirementId::new(1)),
            DependencyId::Section(SectionId::new(4)),
        ];
        let json = serde_json::to_string(&ids).unwrap();
        assert_eq!(json, r#"["REQ-001","SEC-004"]"#);
        assert_eq!(
            serde_json::from_str::<Vec<DependencyId>>(&json).unwrap(),
            ids
        );
    }
}
