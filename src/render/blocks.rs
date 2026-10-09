use crate::spec::text::Markdown;

/// A Markdown document under construction: a sequence of blocks separated
/// by exactly one blank line, with LF line endings and a single trailing
/// newline once [Blocks::finish]ed.
///
/// Every block passes through here, so these rules hold for the whole
/// document no matter what the renderer or the specification's prose does.
#[derive(Debug, Default)]
pub(super) struct Blocks {
    text: String,
}

impl Blocks {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Append `block`, normalized. A block that is empty once normalized is
    /// left out, so it can't leave a stray blank line behind.
    pub(super) fn push(&mut self, block: &str) {
        let block = normalize(block);
        if block.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(&block);
    }

    /// Append the specification's own prose, as written apart from its
    /// normalization.
    pub(super) fn prose(&mut self, markdown: &Markdown) {
        self.push(markdown.as_str());
    }

    /// The finished document, ending in a single newline.
    pub(super) fn finish(mut self) -> String {
        self.text.push('\n');
        self.text
    }
}

/// `text` with LF line endings, without leading blank lines, and without
/// trailing whitespace, so that blocks join with exactly one blank line.
///
/// Everything else is kept exactly, including the indentation of the first
/// line, blank lines within the text, and the contents of code blocks.
pub(super) fn normalize(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }
    text[start..].trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(text: &str) -> Markdown {
        Markdown::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn an_empty_document_is_a_single_newline() {
        assert_eq!(Blocks::new().finish(), "\n");
    }

    #[test]
    fn blocks_are_separated_by_one_blank_line() {
        let mut blocks = Blocks::new();
        blocks.push("# Title");
        blocks.push("First paragraph.");
        blocks.push("- one\n- two");
        assert_eq!(
            blocks.finish(),
            "# Title\n\nFirst paragraph.\n\n- one\n- two\n"
        );
    }

    #[test]
    fn empty_blocks_are_left_out() {
        let mut blocks = Blocks::new();
        blocks.push("");
        blocks.push("First.");
        blocks.push("  \n\t\n");
        blocks.prose(&markdown(" "));
        blocks.push("Second.");
        assert_eq!(blocks.finish(), "First.\n\nSecond.\n");
    }

    #[test]
    fn prose_is_appended_as_written() {
        let mut blocks = Blocks::new();
        blocks.prose(&markdown("It *MUST* hold.\n\nSee `code`."));
        assert_eq!(blocks.finish(), "It *MUST* hold.\n\nSee `code`.\n");
    }

    #[test]
    fn line_endings_become_lf() {
        assert_eq!(normalize("a\r\nb\rc\n\rd"), "a\nb\nc\n\nd");
    }

    #[test]
    fn leading_blank_lines_and_trailing_whitespace_are_trimmed() {
        assert_eq!(normalize("\n \n\t\nText.  \n\n \n"), "Text.");
        assert_eq!(normalize("\r\n\r\nText.\r\n"), "Text.");
    }

    #[test]
    fn whitespace_only_text_normalizes_to_nothing() {
        for text in ["", " ", "\n", "\r\n", " \t \n \r\n "] {
            assert_eq!(normalize(text), "", "{text:?}");
        }
    }

    #[test]
    fn the_first_line_keeps_its_indentation() {
        // Four spaces make an indented code block, which trimming would
        // turn into a paragraph.
        assert_eq!(normalize("\n    let x = 1;\n"), "    let x = 1;");
    }

    #[test]
    fn interior_blank_lines_and_code_are_kept_exactly() {
        let text = "Intro.\n\n\n```datalog\nverified(R).\n\n\tindented(\"tab\").  \n```\n\nOutro.";
        assert_eq!(normalize(text), text);
    }

    #[test]
    fn interior_hard_line_breaks_are_kept() {
        assert_eq!(normalize("one  \ntwo\\\nthree"), "one  \ntwo\\\nthree");
    }
}
