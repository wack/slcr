use std::{ffi::OsString, path::Path};

use serde_json::{Map, Value};

use super::SpawnFailure;
use crate::project::QualifiedRequirementId;
use crate::tools::descriptor::{Builtin, Segment, Template};

/// What a template's placeholders stand for.
pub(super) struct Values<'a> {
    pub(super) with: &'a Map<String, Value>,
    pub(super) project_root: &'a Path,
    pub(super) validator_dir: &'a Path,
    pub(super) requirement: &'a QualifiedRequirementId,
}

impl Values<'_> {
    /// The one argument `template` becomes. Each placeholder's value is
    /// inserted as is, so a value with spaces or braces in it is still part
    /// of this one argument, and nothing is ever split or joined.
    pub(super) fn substitute(&self, template: &Template) -> Result<OsString, SpawnFailure> {
        let mut arg = OsString::new();
        for segment in template.segments() {
            match segment {
                Segment::Literal(text) => arg.push(text),
                Segment::With(field) => arg.push(self.field(field)?),
                Segment::Builtin(Builtin::ProjectRoot) => arg.push(self.project_root),
                Segment::Builtin(Builtin::ValidatorDir) => arg.push(self.validator_dir),
                Segment::Builtin(Builtin::Requirement) => arg.push(self.requirement.to_string()),
            }
        }
        Ok(arg)
    }

    /// The text of `with.<field>`: a string verbatim, and a number or a
    /// boolean in its JSON spelling. Vetting the validator against its
    /// tool's schema should make anything else impossible, but the runner
    /// never guesses.
    fn field(&self, field: &str) -> Result<String, SpawnFailure> {
        let non_scalar = |found| SpawnFailure::NonScalarField {
            field: field.to_owned(),
            found,
        };
        match self.with.get(field) {
            Some(Value::String(text)) => Ok(text.clone()),
            Some(Value::Number(number)) => Ok(number.to_string()),
            Some(Value::Bool(boolean)) => Ok(boolean.to_string()),
            Some(Value::Null) => Err(non_scalar("null")),
            Some(Value::Array(_)) => Err(non_scalar("an array")),
            Some(Value::Object(_)) => Err(non_scalar("an object")),
            None => Err(SpawnFailure::MissingField {
                field: field.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::run::tests::descriptor;

    /// The arguments `argv`, a TOML array whose first element is the
    /// program, becomes with `with`.
    fn substitute(argv: &str, with: Value) -> Result<Vec<OsString>, SpawnFailure> {
        let descriptor = descriptor(argv, "", "");
        let requirement = "todo-api/REQ-002".parse().unwrap();
        let values = Values {
            with: with.as_object().unwrap(),
            project_root: Path::new("/work/my project"),
            validator_dir: Path::new("/work/my project/specs"),
            requirement: &requirement,
        };
        descriptor
            .invoke()
            .argv()
            .args()
            .iter()
            .map(|arg| values.substitute(arg))
            .collect()
    }

    fn args(argv: &str, with: Value) -> Vec<String> {
        substitute(argv, with)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn builtins_are_absolute_paths_and_the_qualified_id() {
        assert_eq!(
            args(
                r#"["t", "{slcr.project_root}", "--dir={slcr.validator_dir}", "{slcr.requirement}"]"#,
                json!({})
            ),
            [
                "/work/my project",
                "--dir=/work/my project/specs",
                "todo-api/REQ-002",
            ]
        );
    }

    #[test]
    fn scalars_are_substituted_in_their_json_spelling() {
        let with = json!({
            "name": "two words {and braces}",
            "count": -42,
            "ratio": 2.5,
            "flag": true,
        });
        assert_eq!(
            args(
                r#"["t", "{with.name}", "n={with.count}", "{with.ratio}{with.flag}", "{with.flag}"]"#,
                with
            ),
            ["two words {and braces}", "n=-42", "2.5true", "true",]
        );
    }

    #[test]
    fn each_template_is_exactly_one_argument() {
        // Spaces, quotes, and shell syntax stay in the one argument, and an
        // empty value is still an argument.
        let with = json!({ "name": "a b; echo 'c' $HOME", "none": "" });
        assert_eq!(
            args(r#"["t", "{with.name}", "{with.none}", ""]"#, with),
            ["a b; echo 'c' $HOME", "", ""]
        );
    }

    #[test]
    fn escaped_braces_stay_literal() {
        assert_eq!(
            args(
                r#"["t", "{{with.name}}", "{{{with.name}}}"]"#,
                json!({ "name": "x" })
            ),
            ["{with.name}", "{x}"]
        );
    }

    #[test]
    fn a_missing_field_is_an_error() {
        let err = substitute(r#"["t", "{with.name}"]"#, json!({ "count": 1 })).unwrap_err();
        assert!(matches!(err, SpawnFailure::MissingField { field } if field == "name"));
    }

    #[test]
    fn a_non_scalar_field_is_an_error() {
        for (value, found) in [
            (json!(null), "null"),
            (json!(["a"]), "an array"),
            (json!({ "a": 1 }), "an object"),
        ] {
            let err = substitute(r#"["t", "{with.list}"]"#, json!({ "list": value })).unwrap_err();
            assert!(
                matches!(err, SpawnFailure::NonScalarField { ref field, found: f } if field == "list" && f == found),
                "{err:?}"
            );
        }
    }
}
