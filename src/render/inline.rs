use std::fmt::Display;

use crate::spec::text::Title;

/// Characters that may start or end inline Markdown constructs wherever
/// they appear: code spans, emphasis, links, raw HTML and autolinks, entity
/// references, strikethrough, and backslash escapes themselves.
const ALWAYS_ESCAPED: [char; 9] = ['\\', '`', '*', '[', ']', '<', '>', '&', '~'];

/// `text`, a plain-text title, escaped so that inline Markdown renders it
/// literally: in a heading, in bold, or as a link's text.
///
/// Only what could be read as Markdown is escaped, so that the source stays
/// readable: an underscore between two letters or digits can't delimit
/// emphasis, so `todo_list_item` stays as it is, and a `#` matters only in
/// a trailing run that would close an ATX heading.
pub(super) fn escape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let closing_hashes = closing_hashes(&chars);
    let mut escaped = String::with_capacity(text.len());
    for (index, &c) in chars.iter().enumerate() {
        let needs_escape = match c {
            '_' => !intraword(&chars, index),
            '#' => index >= closing_hashes,
            _ => ALWAYS_ESCAPED.contains(&c),
        };
        if needs_escape {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Whether the character at `index` sits between two alphanumeric
/// characters.
fn intraword(chars: &[char], index: usize) -> bool {
    let alphanumeric = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    index > 0 && alphanumeric(chars.get(index - 1)) && alphanumeric(chars.get(index + 1))
}

/// Where the run of `#`s that ends `chars` starts, if it would close an
/// ATX heading: when it is the whole text or follows whitespace. Otherwise
/// the length of `chars`, so no `#` is escaped.
fn closing_hashes(chars: &[char]) -> usize {
    let run = chars.iter().rev().take_while(|&&c| c == '#').count();
    let start = chars.len() - run;
    let closes = run > 0 && (start == 0 || chars[start - 1].is_whitespace());
    if closes { start } else { chars.len() }
}

/// `text` escaped for HTML text content.
pub(super) fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// The hidden anchor that makes the node `id` a link target.
pub(super) fn anchor(id: impl Display) -> String {
    format!(r#"<a id="{id}"></a>"#)
}

/// A link to the node `id`, whose text is its `title`.
pub(super) fn link(title: &Title, id: impl Display) -> String {
    format!("[{}](#{id})", escape(title.as_str()))
}

#[cfg(test)]
mod tests {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    use super::*;

    /// Every ASCII punctuation character.
    const PUNCTUATION: &str = r##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##;

    /// Titles built to break naive Markdown: every punctuation character
    /// alone, doubled, and at either end of a word and between words, and
    /// the constructs they form.
    fn adversarial_titles() -> Vec<String> {
        let mut titles = Vec::new();
        for p in PUNCTUATION.chars() {
            titles.extend([
                format!("{p}"),
                format!("{p}{p}"),
                format!("{p}{p}{p}"),
                format!("a{p}"),
                format!("{p}a"),
                format!("a{p}b"),
                format!("a {p} b"),
                format!("a {p}"),
                format!("{p} a"),
                format!("{p}a{p}"),
                format!("a{p}{p}b"),
            ]);
        }
        titles.extend(
            [
                "*emphasis* and **strong**",
                "_emphasis_ and __strong__",
                "snake_case_name",
                "Table todo_list_item exists",
                "a_b_",
                "_a_b",
                "`code` span",
                "``double`` ticks",
                "[link](https://example.com)",
                "![image](image.png)",
                "[ref][1]",
                "<div>raw HTML</div>",
                "<https://example.com>",
                "&amp; &#35; &copy",
                "~~strike~~",
                "trailing backslash\\",
                "C# API",
                "Issue #",
                "Issue ##",
                "#hashtag",
                "1. Not a list",
                "- Not a list",
                "+ Not a list",
                "> Not a quote",
                "=== Not a setext underline",
                "Ünïcödé — ok … 日本語",
                "é_é",
                "two  spaces",
            ]
            .map(str::to_owned),
        );
        titles
    }

    fn options() -> Options {
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES
    }

    /// The literal text of the inline content of the first block of
    /// `markdown`, which must be a heading, paragraph, or strong span.
    fn first_text(markdown: &str) -> String {
        let mut text = String::new();
        for event in Parser::new_ext(markdown, options()) {
            match event {
                Event::Text(t) | Event::Code(t) => text.push_str(&t),
                Event::End(TagEnd::Heading(_) | TagEnd::Paragraph) => break,
                Event::Start(Tag::Heading { .. } | Tag::Paragraph | Tag::Strong) => {}
                Event::End(TagEnd::Strong) => {}
                other => panic!("unexpected {other:?} in {markdown:?}"),
            }
        }
        text
    }

    fn title(text: &str) -> Title {
        Title::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn plain_text_is_unchanged() {
        for text in ["TodoList API", "POST creates an item", "Ünïcödé", "v1.2"] {
            assert_eq!(escape(text), text);
        }
    }

    #[test]
    fn markdown_punctuation_is_escaped() {
        assert_eq!(
            escape("*a* [b] <c> `d` & ~e~ \\"),
            r"\*a\* \[b\] \<c\> \`d\` \& \~e\~ \\"
        );
    }

    #[test]
    fn underscores_are_escaped_only_outside_words() {
        assert_eq!(escape("todo_list_item"), "todo_list_item");
        assert_eq!(escape("_todo_"), r"\_todo\_");
        assert_eq!(escape("a__b"), r"a\_\_b");
        assert_eq!(escape("a _ b"), r"a \_ b");
        assert_eq!(escape("é_é"), "é_é");
    }

    #[test]
    fn only_a_closing_run_of_hashes_is_escaped() {
        assert_eq!(escape("C# API"), "C# API");
        assert_eq!(escape("C#"), "C#");
        assert_eq!(escape("#hashtag"), "#hashtag");
        assert_eq!(escape("Issue #"), r"Issue \#");
        assert_eq!(escape("Issue ##"), r"Issue \#\#");
        assert_eq!(escape("#"), r"\#");
        assert_eq!(escape("###"), r"\#\#\#");
    }

    #[test]
    fn titles_render_literally_in_headings() {
        for text in adversarial_titles() {
            let escaped = escape(&text);
            assert_eq!(first_text(&format!("# {escaped}")), text, "{escaped:?}");
            assert_eq!(
                first_text(&format!("### 1.2 {escaped}")),
                format!("1.2 {text}"),
                "{escaped:?}"
            );
        }
    }

    #[test]
    fn titles_render_literally_in_bold() {
        for text in adversarial_titles() {
            let escaped = escape(&text);
            let markdown = format!("**1.1.1.1.1.1 {escaped}**");
            assert_eq!(
                first_text(&markdown),
                format!("1.1.1.1.1.1 {text}"),
                "{escaped:?}"
            );
            let strong = Parser::new_ext(&markdown, options())
                .filter(|event| matches!(event, Event::Start(Tag::Strong)))
                .count();
            assert_eq!(strong, 1, "{escaped:?}");
        }
    }

    #[test]
    fn titles_render_literally_as_link_text() {
        for text in adversarial_titles() {
            let markdown = format!("- {}", link(&title(&text), "REQ-001"));
            let mut destinations = Vec::new();
            let mut link_text = String::new();
            let mut in_link = false;
            for event in Parser::new_ext(&markdown, options()) {
                match event {
                    Event::Start(Tag::Link { dest_url, .. }) => {
                        destinations.push(dest_url.to_string());
                        in_link = true;
                    }
                    Event::End(TagEnd::Link) => in_link = false,
                    Event::Text(t) | Event::Code(t) if in_link => link_text.push_str(&t),
                    _ => {}
                }
            }
            assert_eq!(destinations, ["#REQ-001"], "{text:?}");
            assert_eq!(link_text, text, "{text:?}");
        }
    }

    #[test]
    fn html_special_characters_are_escaped() {
        assert_eq!(
            escape_html(r#"<b>"Fish" & chips</b>"#),
            "&lt;b&gt;&quot;Fish&quot; &amp; chips&lt;/b&gt;"
        );
        assert_eq!(
            escape_html("todo_list_item *as is*"),
            "todo_list_item *as is*"
        );
    }

    #[test]
    fn anchors_carry_the_id() {
        assert_eq!(anchor("REQ-042"), r#"<a id="REQ-042"></a>"#);
    }

    #[test]
    fn links_point_at_the_anchor() {
        assert_eq!(
            link(&title("Table todo_list_item exists"), "REQ-007"),
            "[Table todo_list_item exists](#REQ-007)"
        );
        assert_eq!(link(&title("[x]"), "TERM-001"), r"[\[x\]](#TERM-001)");
    }
}
