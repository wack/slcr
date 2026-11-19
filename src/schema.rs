//! What the hand-written `schemars::JsonSchema` implementations share
//! (feature `schemars`): the patterns of the validated strings.
//!
//! A pattern is checked in an ECMA-262 engine by whoever validates a
//! document against the schema, and in the tests here by the `jsonschema`
//! crate, against the parser the type is read with. Both treat `\d` as
//! Unicode digits in some engines, so patterns spell digits `[0-9]`.

use schemars::{Schema, json_schema};
use serde_json::{Value, json};

/// A string schema restricted to `pattern`, which describes the whole
/// string.
pub(crate) fn string(description: &str, pattern: &str) -> Schema {
    json_schema!({
        "type": "string",
        "description": description,
        "pattern": pattern,
    })
}

/// A word of lowercase ASCII letters and digits, in dash-separated words:
/// the grammar of specification names and tool keys.
pub(crate) const SLUG: &str = "[a-z0-9]+(?:-[a-z0-9]+)*";

/// The numbers with as many digits as `max` (no leading zero) that are at
/// most `max`, one alternative for each digit where they first fall short.
fn same_length_up_to(max: &str) -> Vec<String> {
    let digits = max.as_bytes();
    let mut alternatives = Vec::new();
    for (index, &digit) in digits.iter().enumerate() {
        let lowest = if index == 0 { b'1' } else { b'0' };
        if digit > lowest {
            let rest = digits.len() - index - 1;
            alternatives.push(format!(
                "{}{}{}",
                &max[..index],
                class(lowest, digit - 1),
                rest_of(rest),
            ));
        }
    }
    alternatives.push(max.to_owned());
    alternatives
}

/// The digits from `low` to `high`, as a regular expression.
fn class(low: u8, high: u8) -> String {
    if low == high {
        char::from(low).to_string()
    } else {
        format!("[{}-{}]", char::from(low), char::from(high))
    }
}

/// Any `count` digits.
fn rest_of(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => "[0-9]".to_owned(),
        n => format!("[0-9]{{{n}}}"),
    }
}

/// The number of an ID: three digits, or four or more without a leading
/// zero, up to the largest `u32`, which is the most an ID can hold.
pub(crate) fn id_number() -> String {
    let max = u32::MAX.to_string();
    let longest = max.len();
    format!(
        "(?:[0-9]{{3}}|[1-9][0-9]{{3,{}}}|{})",
        longest - 2,
        same_length_up_to(&max).join("|")
    )
}

/// An ID with `prefix`, such as `REQ-042`, as a regular expression for
/// part of a string.
pub(crate) fn id(prefix: &str) -> String {
    format!("{prefix}-{}", id_number())
}

/// A SemVer 2.0.0 version, as a regular expression for a whole string:
/// the grammar of semver.org, but for the size of the three numbers: the
/// `semver` crate keeps each in a `u64`, and the pattern only limits each
/// to 20 digits, the length of the largest. It rejects nothing the crate
/// accepts, and accepts besides only a number of 20 digits above
/// `u64::MAX`.
pub(crate) fn semver() -> String {
    let number = "(?:0|[1-9][0-9]{0,19})";
    let identifier = "(?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)";
    let build = "[0-9a-zA-Z-]+";
    format!(
        "^{number}\\.{number}\\.{number}\
         (?:-{identifier}(?:\\.{identifier})*)?\
         (?:\\+{build}(?:\\.{build})*)?$"
    )
}

/// A transform for a type that serializes an optional field by leaving it
/// out when it has none, never as `null`: such a property is not required,
/// and doesn't allow `null`, which `Option<T>`'s own schema does.
///
/// It looks at the properties of the schema and of the alternatives
/// (`oneOf`, `anyOf`, and `allOf`) that its own object is merged with, such
/// as a flattened enum's variants, but not at nested objects: those are
/// schemas of types of their own.
pub(crate) fn omitted_not_null(schema: &mut Schema) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    let required: Vec<String> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        for (name, property) in properties {
            if !required.contains(name) {
                not_null(property);
            }
        }
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(Value::Array(alternatives)) = object.get_mut(key) {
            for alternative in alternatives {
                if let Ok(alternative) = <&mut Schema>::try_from(alternative) {
                    omitted_not_null(alternative);
                }
            }
        }
    }
}

/// `property`'s schema, without the `null` that `Option<T>` adds to the
/// schema of `T`.
fn not_null(property: &mut Value) {
    let Some(object) = property.as_object_mut() else {
        return;
    };
    let is_null = |value: &Value| value.get("type").and_then(Value::as_str) == Some("null");
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|name| name.as_str() != Some("null"));
        if types.len() == 1 {
            let only = types.remove(0);
            object.insert("type".to_owned(), only);
        }
    } else if let Some(Value::Array(alternatives)) = object.get_mut("anyOf") {
        alternatives.retain(|alternative| !is_null(alternative));
        if alternatives.len() == 1 {
            let only = alternatives.remove(0);
            object.remove("anyOf");
            if let Value::Object(only) = only {
                object.extend(only);
            }
        }
    }
}

/// A transform for a struct that serde writes with a `tag`, the `code` of
/// every problem, which `#[derive(JsonSchema)]` only knows for an enum's
/// variants: the property, first, and required.
pub(crate) fn tagged(tag: &'static str, code: &'static str) -> impl FnMut(&mut Schema) {
    move |schema| {
        let object = schema.ensure_object();
        let Value::Object(properties) = object
            .entry("properties")
            .or_insert_with(|| Value::Object(Default::default()))
        else {
            return;
        };
        let others = std::mem::take(properties);
        properties.insert(tag.to_owned(), json!({ "type": "string", "const": code }));
        properties.extend(others.into_iter().filter(|(name, _)| name != tag));
        if let Value::Array(required) = object
            .entry("required")
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            required.insert(0, Value::from(tag));
        }
    }
}
