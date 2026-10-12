//! Where each validator, and each of its fields, is in a validator file's
//! source, so that a diagnostic about one can label it.
//!
//! The source is read a second time, after it has parsed, into a shape
//! that keeps locations instead of values: through the `toml` crate's
//! `Spanned` for TOML, and through serde-saphyr's for YAML and for JSON,
//! which YAML 1.2 reads too. Locations are only labels, so a source that
//! can't be mapped this way gets none.

use miette::SourceSpan;
use serde::{Deserialize, de::IgnoredAny};

use crate::fs::format::Format;

/// Where one validator is in its file's source: each label covers at most
/// the first line of what it locates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Spans {
    /// The validator as a whole.
    pub(crate) entry: Option<SourceSpan>,
    pub(crate) requirement: Option<SourceSpan>,
    pub(crate) criterion: Option<SourceSpan>,
    pub(crate) tool: Option<SourceSpan>,
    pub(crate) with: Option<SourceSpan>,
}

/// The spans of the `count` validators in `source`, written in `format`,
/// or none at all if they can't be told.
pub(crate) fn locate(format: Format, source: &str, count: usize) -> Vec<Spans> {
    let spans = match format {
        Format::Toml => toml_spans(source),
        Format::Json | Format::Yaml => yaml_spans(source),
    };
    match spans {
        Some(spans) if spans.len() == count => spans
            .into_iter()
            .map(|spans| spans.first_lines(source))
            .collect(),
        _ => Vec::new(),
    }
}

/// A validator file, keeping only where its validators are.
#[derive(Deserialize)]
struct Located<E> {
    validator: Vec<E>,
}

/// A validator's fields, keeping only where they are. Fields it doesn't
/// name are skipped.
#[derive(Deserialize)]
struct Fields<S> {
    requirement: Option<S>,
    criterion: Option<S>,
    tool: Option<S>,
    with: Option<S>,
}

impl<S> Fields<S> {
    fn spans(self, entry: Option<SourceSpan>, span: impl Fn(S) -> Option<SourceSpan>) -> Spans {
        Spans {
            entry,
            requirement: self.requirement.and_then(&span),
            criterion: self.criterion.and_then(&span),
            tool: self.tool.and_then(&span),
            with: self.with.and_then(&span),
        }
    }
}

fn toml_spans(source: &str) -> Option<Vec<Spans>> {
    type Spanned<T> = toml::Spanned<T>;
    let span = |spanned: Spanned<IgnoredAny>| Some(SourceSpan::from(spanned.span()));
    let located: Located<Spanned<Fields<Spanned<IgnoredAny>>>> = toml::from_str(source).ok()?;
    let spans = located.validator.into_iter().map(|entry| {
        let range = entry.span();
        entry
            .into_inner()
            .spans(Some(SourceSpan::from(range)), span)
    });
    Some(spans.collect())
}

fn yaml_spans(source: &str) -> Option<Vec<Spans>> {
    type Spanned<T> = serde_saphyr::Spanned<T>;
    fn span<T>(spanned: &Spanned<T>) -> Option<SourceSpan> {
        let span = spanned.referenced.span();
        let offset = usize::try_from(span.byte_offset()?).ok()?;
        let length = usize::try_from(span.byte_len()?).ok()?;
        Some(SourceSpan::from((offset, length)))
    }
    let located: Located<Spanned<Fields<Spanned<IgnoredAny>>>> =
        serde_saphyr::from_str(source).ok()?;
    let spans = located.validator.into_iter().map(|entry| {
        let whole = span(&entry);
        entry.value.spans(whole, |field| span(&field))
    });
    Some(spans.collect())
}

impl Spans {
    /// The same spans, each cut at the end of its first line and kept only
    /// if it lies within `source`.
    fn first_lines(self, source: &str) -> Self {
        let cut = |span: Option<SourceSpan>| {
            let span = span?;
            let text = source.get(span.offset()..span.offset() + span.len())?;
            let length = text.find(['\r', '\n']).unwrap_or(text.len());
            Some(SourceSpan::from((span.offset(), length)))
        };
        Self {
            entry: cut(self.entry),
            requirement: cut(self.requirement),
            criterion: cut(self.criterion),
            tool: cut(self.tool),
            with: cut(self.with),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text `span` covers in `source`.
    fn text(source: &str, span: Option<SourceSpan>) -> &str {
        let span = span.expect("a span");
        &source[span.offset()..span.offset() + span.len()]
    }

    #[test]
    fn toml_fields_and_entries_are_located() {
        let source = "formatVersion = \"1\"\n\n[[validator]]\nrequirement = \"a/REQ-001\"\n\
                      tool = \"jev\"\nwith = { x = 1 }\n\n[[validator]]\ncriterion = \"AC-001\"\n\
                      requirement = \"a/REQ-002\"\ntool = \"curl\"\n";
        let spans = locate(Format::Toml, source, 2);
        assert_eq!(spans.len(), 2);
        assert_eq!(text(source, spans[0].entry), "[[validator]]");
        assert_eq!(text(source, spans[0].requirement), "\"a/REQ-001\"");
        assert_eq!(text(source, spans[0].tool), "\"jev\"");
        assert_eq!(text(source, spans[0].with), "{ x = 1 }");
        assert_eq!(spans[0].criterion, None);
        assert_eq!(text(source, spans[1].criterion), "\"AC-001\"");
        assert_eq!(text(source, spans[1].requirement), "\"a/REQ-002\"");
        assert_eq!(spans[1].with, None);
    }

    #[test]
    fn inline_toml_validators_are_located() {
        let source = "formatVersion = \"1\"\nvalidator = [{ requirement = \"a/REQ-001\", tool = \
                      \"jev\" }, { requirement = \"a/REQ-002\", tool = \"jev\" }]\n";
        let spans = locate(Format::Toml, source, 2);
        assert_eq!(
            text(source, spans[1].entry),
            "{ requirement = \"a/REQ-002\", tool = \"jev\" }"
        );
        assert_eq!(text(source, spans[1].requirement), "\"a/REQ-002\"");
    }

    #[test]
    fn yaml_fields_are_located_and_entries_are_pointed_at() {
        let source = "formatVersion: \"1\"\nvalidator:\n  - requirement: a/REQ-001\n    tool: jev\n    \
                      with:\n      x: 1\n  - tool: \"curl\"\n    requirement: a/REQ-002\n";
        let spans = locate(Format::Yaml, source, 2);
        assert_eq!(text(source, spans[0].requirement), "a/REQ-001");
        assert_eq!(text(source, spans[0].tool), "jev");
        // A mapping is located by where it starts.
        let entry = spans[0].entry.unwrap();
        assert_eq!(entry.offset(), source.find("requirement").unwrap());
        assert!(spans[0].with.is_some());
        assert_eq!(text(source, spans[1].tool), "\"curl\"");
        assert_eq!(text(source, spans[1].requirement), "a/REQ-002");
    }

    #[test]
    fn json_is_located_as_yaml() {
        let source = "{\n  \"formatVersion\": \"1\",\n  \"validator\": [\n    {\"requirement\": \
                      \"a/REQ-001\", \"tool\": \"j\\u0065v\"},\n    {\n      \"tool\": \"curl\",\n      \
                      \"requirement\": \"a/REQ-002\"\n    }\n  ]\n}\n";
        let spans = locate(Format::Json, source, 2);
        assert_eq!(text(source, spans[0].requirement), "\"a/REQ-001\"");
        assert_eq!(text(source, spans[0].tool), "\"j\\u0065v\"");
        assert_eq!(text(source, spans[1].entry), "{");
        assert_eq!(text(source, spans[1].requirement), "\"a/REQ-002\"");
    }

    #[test]
    fn spans_end_with_their_first_line() {
        let source = "formatVersion = \"1\"\n[[validator]]\nrequirement = \"a/REQ-001\"\n\
                      tool = \"jev\"\nwith = { x = [\n  1,\n] }\n";
        let spans = locate(Format::Toml, source, 1);
        assert_eq!(text(source, spans[0].entry), "[[validator]]");
        assert_eq!(text(source, spans[0].with), "{ x = [");
    }

    #[test]
    fn a_source_that_cannot_be_mapped_has_no_spans() {
        assert_eq!(locate(Format::Toml, "not = [toml", 1), []);
        assert_eq!(locate(Format::Yaml, "validator: 1\n", 1), []);
        // Nor does one whose validators can't all be told apart.
        let source =
            "formatVersion = \"1\"\n[[validator]]\nrequirement = \"a/REQ-001\"\ntool = \"jev\"\n";
        assert_eq!(locate(Format::Toml, source, 2), []);
    }
}
