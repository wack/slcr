//! Deserializers for optional fields.
//!
//! Canonical form omits an optional field that is empty, rather than writing
//! it as `null` or `[]`. Fields pair these functions with
//! `#[serde(default)]`, so an absent field reads as `None` or as an empty
//! array, while a present field must hold a value.

use std::{collections::HashSet, fmt::Display, hash::Hash};

use serde::{Deserialize, Deserializer, de::Error as _};

/// Deserialize an optional value that, when present, is not `null`.
pub(super) fn some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Deserialize an array that must hold at least one element.
pub(super) fn non_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let items = Vec::deserialize(deserializer)?;
    if items.is_empty() {
        return Err(D::Error::custom(
            "an empty array must be omitted rather than written as `[]`",
        ));
    }
    Ok(items)
}

/// Deserialize an array that must hold at least one element, with no
/// element repeated.
pub(super) fn non_empty_set<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Display + Eq + Hash,
{
    let items = non_empty(deserializer)?;
    let mut seen = HashSet::with_capacity(items.len());
    if let Some(repeated) = items.iter().find(|item| !seen.insert(*item)) {
        return Err(D::Error::custom(format!(
            "`{repeated}` appears more than once in a set"
        )));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use crate::spec::id::RequirementId;

    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "super::some")]
        value: Option<String>,
        #[serde(default, deserialize_with = "super::non_empty")]
        list: Vec<u32>,
        #[serde(default, deserialize_with = "super::non_empty_set")]
        set: Vec<RequirementId>,
    }

    #[test]
    fn absent_fields_read_as_empty() {
        let holder: Holder = serde_json::from_str("{}").unwrap();
        assert!(holder.value.is_none());
        assert!(holder.list.is_empty());
        assert!(holder.set.is_empty());
    }

    #[test]
    fn present_values_are_kept() {
        let holder: Holder = serde_json::from_str(r#"{"value":"here"}"#).unwrap();
        assert_eq!(holder.value.as_deref(), Some("here"));
    }

    #[test]
    fn null_values_are_rejected() {
        for field in ["value", "list", "set"] {
            let json = format!(r#"{{"{field}":null}}"#);
            assert!(serde_json::from_str::<Holder>(&json).is_err(), "{field}");
        }
    }

    #[test]
    fn present_arrays_keep_their_order() {
        let holder: Holder =
            serde_json::from_str(r#"{"list":[3,1,3],"set":["REQ-002","REQ-001"]}"#).unwrap();
        assert_eq!(holder.list, [3, 1, 3]);
        assert_eq!(holder.set, [RequirementId::new(2), RequirementId::new(1)]);
    }

    #[test]
    fn empty_arrays_are_rejected() {
        let err = serde_json::from_str::<Holder>(r#"{"list":[]}"#).unwrap_err();
        assert!(err.to_string().contains("must be omitted"));
        assert!(serde_json::from_str::<Holder>(r#"{"set":[]}"#).is_err());
    }

    #[test]
    fn repeated_set_elements_are_rejected() {
        let err = serde_json::from_str::<Holder>(r#"{"set":["REQ-001","REQ-002","REQ-001"]}"#)
            .unwrap_err();
        assert!(err.to_string().contains("`REQ-001` appears more than once"));
    }

    #[test]
    fn set_elements_are_still_validated() {
        assert!(serde_json::from_str::<Holder>(r#"{"set":["REQ-1"]}"#).is_err());
    }
}
