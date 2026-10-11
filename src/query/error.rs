use std::fmt::{self, Display};
use std::iter;

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode};
use slcr_gql::Error;

/// Where the query profile is specified.
const PROFILE: &str = "https://github.com/wack/gql-rust/blob/trunk/PROFILE.md";

/// The error returned when a query can't be run, or fails while running.
///
/// A syntax error, or a feature outside the query profile, is labeled where
/// it is in the query's text.
#[derive(Debug)]
pub struct InvalidQuery {
    // Boxed to keep `Result<_, InvalidQuery>` small.
    query: Box<NamedSource<String>>,
    error: Error,
}

impl InvalidQuery {
    /// `error`, which running the query `text` returned.
    pub fn new(text: &str, error: Error) -> Self {
        Self {
            query: Box::new(NamedSource::new("query", text.to_owned())),
            error,
        }
    }

    pub fn error(&self) -> &Error {
        &self.error
    }
}

impl Display for InvalidQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.error {
            Error::Syntax { .. } => f.write_str("the query is not valid GQL"),
            Error::Unsupported { feature, .. } => {
                write!(f, "`{feature}` is outside the SLCR query profile")
            }
            Error::Semantic(message) => write!(f, "the query is invalid: {message}"),
            Error::Data(message) => write!(f, "the query failed: {message}"),
            Error::Store(error) => write!(f, "the query failed: {error}"),
        }
    }
}

impl std::error::Error for InvalidQuery {}

impl Diagnostic for InvalidQuery {
    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        match self.error {
            // On a line of its own, so that the URL isn't wrapped.
            Error::Unsupported { .. } => Some(Box::new(format!(
                "queries use a read-only subset of GQL, specified at\n{PROFILE}"
            ))),
            _ => None,
        }
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(self.query.as_ref())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let (label, span) = match &self.error {
            Error::Syntax { message, span } => (message.clone(), span),
            Error::Unsupported { span, .. } => ("not supported".to_owned(), span),
            _ => return None,
        };
        let label = LabeledSpan::new_with_span(Some(label), span.clone());
        Some(Box::new(iter::once(label)))
    }
}

#[cfg(test)]
mod tests {
    use slcr_gql::parse_query;

    use super::*;
    use crate::terminal::render_plain;

    fn rendered(text: &str) -> String {
        let error = parse_query(text).unwrap_err();
        render_plain(&InvalidQuery::new(text, error))
    }

    #[test]
    fn a_syntax_error_is_labeled_in_the_query() {
        assert_eq!(
            rendered("MATCH (r:Requirement RETURN r"),
            "  × the query is not valid GQL
   ╭─[query:1:22]
 1 │ MATCH (r:Requirement RETURN r
   ·                      ───┬──
   ·                         ╰── found 'RETURN' expected '&', '|', ':', '{', WHERE, or ')'
   ╰────
"
        );
    }

    #[test]
    fn an_unsupported_feature_is_labeled_and_points_at_the_profile() {
        assert_eq!(
            rendered("OPTIONAL MATCH (r) RETURN r"),
            format!(
                "  × `OPTIONAL` is outside the SLCR query profile
   ╭─[query:1:1]
 1 │ OPTIONAL MATCH (r) RETURN r
   · ────┬───
   ·     ╰── not supported
   ╰────
  help: queries use a read-only subset of GQL, specified at
        {PROFILE}
"
            )
        );
    }

    #[test]
    fn errors_without_a_location_are_headlines() {
        let semantic = InvalidQuery::new("RETURN x", Error::Semantic("undefined".to_owned()));
        assert_eq!(
            render_plain(&semantic),
            "  × the query is invalid: undefined\n"
        );
        let data = InvalidQuery::new("RETURN 1/0", Error::Data("division by zero".to_owned()));
        assert_eq!(
            render_plain(&data),
            "  × the query failed: division by zero\n"
        );
    }
}
