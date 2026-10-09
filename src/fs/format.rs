use std::{
    fmt::{self, Display},
    iter,
    path::{Path, PathBuf},
};

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode, SourceSpan};
use serde::{Serialize, de::DeserializeOwned};

/// A serialization format, chosen by a file's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    Toml,
    Yaml,
}

impl Format {
    /// The format a file extension names, without its dot: `json`, `toml`,
    /// `yaml`, or `yml`.
    pub(crate) fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    /// Deserialize `source`, the contents of the file at `path`.
    pub(crate) fn parse<T: DeserializeOwned>(
        self,
        path: &Path,
        source: String,
    ) -> Result<T, ParseError> {
        let (message, span) = match self {
            Self::Json => match serde_json::from_str(&source) {
                Ok(data) => return Ok(data),
                Err(err) => json_error(&source, &err),
            },
            Self::Toml => match toml::from_str(&source) {
                Ok(data) => return Ok(data),
                Err(err) => (err.message().to_owned(), err.span().map(SourceSpan::from)),
            },
            Self::Yaml => match serde_saphyr::from_str(&source) {
                Ok(data) => return Ok(data),
                Err(err) => yaml_error(&source, &err),
            },
        };
        Err(ParseError {
            path: path.to_path_buf(),
            format: self,
            message,
            source_code: Box::new(NamedSource::new(path.display().to_string(), source)),
            span,
        })
    }

    /// Serialize `data` in this format.
    pub(crate) fn serialize<T: Serialize>(self, data: &T) -> miette::Result<String> {
        use miette::IntoDiagnostic;
        match self {
            Self::Json => serde_json::to_string_pretty(data).into_diagnostic(),
            Self::Toml => toml::to_string_pretty(data).into_diagnostic(),
            Self::Yaml => serde_saphyr::to_string(data).into_diagnostic(),
        }
    }
}

impl Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
        })
    }
}

/// The message and location of a JSON error. serde_json reports the
/// 1-based line and byte column of the last character it read, and
/// appends them to its message.
fn json_error(source: &str, err: &serde_json::Error) -> (String, Option<SourceSpan>) {
    let (line, column) = (err.line(), err.column());
    let suffix = format!(" at line {line} column {column}");
    let message = without_suffix(err.to_string(), &suffix);
    if line == 0 {
        return (message, None);
    }
    let line_start: usize = source
        .split_inclusive('\n')
        .take(line - 1)
        .map(str::len)
        .sum();
    // Column 0 means no character on the line was read yet.
    let offset = floor_char_boundary(source, line_start + column.saturating_sub(1));
    let length = if column == 0 {
        0
    } else {
        source[offset..].chars().next().map_or(0, char::len_utf8)
    };
    (message, Some(SourceSpan::from((offset, length))))
}

/// The message and location of a YAML error. serde-saphyr reports byte
/// spans when reading from a string, and appends the line and column to
/// its message.
fn yaml_error(source: &str, err: &serde_saphyr::Error) -> (String, Option<SourceSpan>) {
    let plain = err.without_snippet().to_string();
    let Some(location) = err.location() else {
        return (plain, None);
    };
    let suffix = format!(" at line {}, column {}", location.line(), location.column());
    let message = without_suffix(plain, &suffix);
    let span = location.span();
    let span = span
        .byte_offset()
        .zip(span.byte_len())
        .and_then(|(offset, length)| {
            let offset = usize::try_from(offset).ok()?;
            let length = usize::try_from(length).ok()?;
            // Only label a span that lies within the source.
            source
                .get(offset..offset + length)
                .map(|_| SourceSpan::from((offset, length)))
        });
    (message, span)
}

fn without_suffix(message: String, suffix: &str) -> String {
    match message.strip_suffix(suffix) {
        Some(stripped) => stripped.to_owned(),
        None => message,
    }
}

/// The largest index no greater than `index` that starts a character in
/// `source`, or its length if `index` is past the end.
fn floor_char_boundary(source: &str, index: usize) -> usize {
    let mut index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The error returned when a file's contents can't be deserialized: either
/// they aren't well-formed in the file's format, or they don't have the
/// shape of the data the file holds.
///
/// When the deserializer reports where the problem is, the error labels
/// that place in the file's source.
#[derive(Debug)]
pub struct ParseError {
    path: PathBuf,
    format: Format,
    message: String,
    // Boxed to keep `Result<_, ParseError>` small.
    source_code: Box<NamedSource<String>>,
    span: Option<SourceSpan>,
}

impl ParseError {
    /// What the deserializer reported, without its location.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where in the file's source the problem is, if known.
    pub fn span(&self) -> Option<SourceSpan> {
        self.span
    }
}

impl Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not parse {} as {}",
            self.path.display(),
            self.format
        )?;
        // Without a location to label, the message belongs in the headline.
        if self.span.is_none() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

impl Diagnostic for ParseError {
    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(self.source_code.as_ref())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span?;
        let label = LabeledSpan::new_with_span(Some(self.message.clone()), span);
        Some(Box::new(iter::once(label)))
    }
}

#[cfg(test)]
mod tests {
    use miette::{GraphicalReportHandler, GraphicalTheme};
    use serde::Deserialize;

    use super::*;

    /// A small document with a validated field, an array, and no unknown
    /// fields allowed.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Doc {
        name: String,
        code: Code,
        #[serde(default)]
        tags: Vec<String>,
    }

    #[derive(Debug, PartialEq)]
    struct Code(u32);

    impl<'de> Deserialize<'de> for Code {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let value = String::deserialize(deserializer)?;
            value
                .strip_prefix("C-")
                .and_then(|number| number.parse().ok())
                .map(Code)
                .ok_or_else(|| serde::de::Error::custom(format!("`{value}` is not a code")))
        }
    }

    fn parse(format: Format, source: &str) -> Result<Doc, ParseError> {
        format.parse(Path::new("doc"), source.to_owned())
    }

    fn error(format: Format, source: &str) -> ParseError {
        parse(format, source).unwrap_err()
    }

    /// The text a span covers.
    fn spanned<'a>(source: &'a str, err: &ParseError) -> &'a str {
        let span = err.span().expect("the error is located");
        &source[span.offset()..span.offset() + span.len()]
    }

    fn render(err: &ParseError) -> String {
        let mut out = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .with_width(80)
            .render_report(&mut out, err)
            .unwrap();
        out
    }

    #[test]
    fn extensions_name_formats() {
        assert_eq!(Format::from_extension("json"), Some(Format::Json));
        assert_eq!(Format::from_extension("toml"), Some(Format::Toml));
        assert_eq!(Format::from_extension("yaml"), Some(Format::Yaml));
        assert_eq!(Format::from_extension("yml"), Some(Format::Yaml));
        for unknown in ["", "JSON", "ini", ".json"] {
            assert_eq!(Format::from_extension(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn formats_display_their_names() {
        assert_eq!(Format::Json.to_string(), "JSON");
        assert_eq!(Format::Toml.to_string(), "TOML");
        assert_eq!(Format::Yaml.to_string(), "YAML");
    }

    #[test]
    fn valid_documents_parse_in_every_format() {
        let expected = Doc {
            name: "x".to_owned(),
            code: Code(7),
            tags: vec!["a".to_owned()],
        };
        let sources = [
            (
                Format::Json,
                r#"{"name": "x", "code": "C-7", "tags": ["a"]}"#,
            ),
            (
                Format::Toml,
                "name = \"x\"\ncode = \"C-7\"\ntags = [\"a\"]\n",
            ),
            (Format::Yaml, "name: x\ncode: C-7\ntags: [a]\n"),
        ];
        for (format, source) in sources {
            assert_eq!(parse(format, source).unwrap(), expected, "{format}");
        }
    }

    #[test]
    fn json_syntax_errors_are_located() {
        let source = "{\n  \"name\": \"x\"\n  \"code\": \"C-1\"\n}";
        let err = error(Format::Json, source);
        assert_eq!(err.message(), "expected `,` or `}`");
        assert_eq!(spanned(source, &err), "\"");
        assert_eq!(err.span().unwrap().offset(), source.find("\"code").unwrap());
    }

    #[test]
    fn json_columns_count_bytes() {
        // serde_json reports the byte it had reached, here the closing brace
        // it peeked after the bad value. Counting `é` and `日` as one column
        // each would land on the quote before it instead.
        for name in ["e", "é", "日"] {
            let source = format!(r#"{{"name": "{name}", "code": "X-1"}}"#);
            let err = error(Format::Json, &source);
            assert_eq!(err.message(), "`X-1` is not a code");
            assert_eq!(spanned(&source, &err), "}", "{name}");
        }
    }

    #[test]
    fn json_errors_before_the_first_column_have_an_empty_span() {
        let err = error(Format::Json, "");
        assert_eq!(err.message(), "EOF while parsing a value");
        assert_eq!(err.span(), Some(SourceSpan::from((0, 0))));

        let err = error(Format::Json, r#"{"name": "x"}"#);
        assert_eq!(err.message(), "missing field `code`");
        assert_eq!(spanned(r#"{"name": "x"}"#, &err), "}");
    }

    #[test]
    fn json_unknown_fields_are_located() {
        let source = r#"{"name": "x", "oops": 1}"#;
        let err = error(Format::Json, source);
        assert!(err.message().starts_with("unknown field `oops`"), "{err:?}");
        assert_eq!(spanned(source, &err), "\"");
    }

    #[test]
    fn toml_errors_are_located() {
        let source = "name = \"é\"\ncode = \"X-1\"\n";
        let err = error(Format::Toml, source);
        assert_eq!(err.message(), "`X-1` is not a code");
        assert_eq!(spanned(source, &err), "\"X-1\"");

        let source = "name = \ncode = \"C-1\"\n";
        let err = error(Format::Toml, source);
        assert!(err.message().starts_with("invalid string"), "{err:?}");
        assert!(err.span().is_some());
    }

    #[test]
    fn yaml_errors_are_located() {
        let source = "name: é\ncode: X-1\n";
        let err = error(Format::Yaml, source);
        assert_eq!(err.message(), "`X-1` is not a code");
        assert_eq!(spanned(source, &err), "X-1");

        let source = "name: x\noops: 1\ncode: C-1\n";
        let err = error(Format::Yaml, source);
        assert!(err.message().starts_with("unknown field `oops`"), "{err:?}");
        assert_eq!(spanned(source, &err), "oops");
    }

    #[test]
    fn yaml_syntax_errors_are_located() {
        let source = "name: x\ncode: [unclosed\n";
        let err = error(Format::Yaml, source);
        assert!(err.span().is_some(), "{err:?}");
        assert!(!err.message().contains(" at line "), "{err:?}");
    }

    #[test]
    fn empty_documents_fail_in_every_format() {
        for format in [Format::Json, Format::Toml, Format::Yaml] {
            let err = error(format, "");
            assert!(!err.message().is_empty(), "{format}");
        }
    }

    #[test]
    fn floor_char_boundary_never_splits_a_character() {
        let source = "aé";
        assert_eq!(floor_char_boundary(source, 0), 0);
        assert_eq!(floor_char_boundary(source, 1), 1);
        assert_eq!(floor_char_boundary(source, 2), 1);
        assert_eq!(floor_char_boundary(source, 3), 3);
        assert_eq!(floor_char_boundary(source, 99), 3);
    }

    #[test]
    fn located_errors_render_with_a_labeled_snippet() {
        let source = "name: x\ncode: X-1\n";
        let err = Format::Yaml
            .parse::<Doc>(Path::new("specs/doc.yaml"), source.to_owned())
            .unwrap_err();
        assert_eq!(err.to_string(), "could not parse specs/doc.yaml as YAML");
        assert_eq!(
            render(&err),
            "  × could not parse specs/doc.yaml as YAML\n   \
             ╭─[specs/doc.yaml:2:7]\n \
             1 │ name: x\n \
             2 │ code: X-1\n   \
             ·       ─┬─\n   \
             ·        ╰── `X-1` is not a code\n   \
             ╰────\n"
        );
    }

    #[test]
    fn unlocated_errors_carry_their_message_in_the_headline() {
        let err = ParseError {
            path: PathBuf::from("doc.json"),
            format: Format::Json,
            message: "something went wrong".to_owned(),
            source_code: Box::new(NamedSource::new("doc.json", String::new())),
            span: None,
        };
        assert_eq!(
            err.to_string(),
            "could not parse doc.json as JSON: something went wrong"
        );
        assert!(err.labels().is_none());
    }
}
