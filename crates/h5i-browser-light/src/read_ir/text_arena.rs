//! Where the page's words live.
//!
//! One buffer and span table replace a `String` allocation per node.
//!
//! This is not an interner: page text has little duplication, and interning
//! attacker-controlled text adds hashing and unbounded growth.

use super::model::TextId;

/// Immutable page text, addressed by [`TextId`].
#[derive(Debug, Default)]
pub struct TextArena {
    buf: String,
    /// `(start, len)` per id. Entry 0 is the empty string, so `TextId::EMPTY`
    /// resolves without a special case at every read site.
    spans: Vec<(u32, u32)>,
}

impl TextArena {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            spans: vec![(0, 0)],
        }
    }

    /// Room for a page's worth of text, taken once.
    pub fn with_capacity(bytes: usize, nodes: usize) -> Self {
        let mut spans = Vec::with_capacity(nodes + 1);
        spans.push((0, 0));
        Self {
            buf: String::with_capacity(bytes),
            spans,
        }
    }

    pub fn resolve(&self, id: TextId) -> &str {
        let (start, len) = self.spans[id.0 as usize];
        &self.buf[start as usize..start as usize + len as usize]
    }

    /// Store a string verbatim.
    ///
    /// Unlike [`Self::collapse_into`], preserves code indentation.
    pub fn intern(&mut self, text: &str) -> TextId {
        if text.is_empty() {
            return TextId::EMPTY;
        }
        let start = self.buf.len() as u32;
        self.buf.push_str(text);
        self.spans.push((start, text.len() as u32));
        TextId(self.spans.len() as u32 - 1)
    }

    /// Collapse raw page text straight into the arena.
    ///
    /// Writes [`crate::snapshot::collapse`]'s output in place and returns its
    /// span. `collapse_into_agrees` checks equivalence.
    pub fn collapse_into(&mut self, raw: &str) -> TextId {
        let start = self.buf.len() as u32;
        let mut in_space = false;
        for ch in raw.chars() {
            if ch.is_whitespace() {
                in_space = true;
                continue;
            }
            if ch.is_control() || crate::snapshot::is_bidi_control(ch) {
                continue;
            }
            if in_space && self.buf.len() as u32 > start {
                self.buf.push(' ');
            }
            in_space = false;
            self.buf.push(ch);
        }
        self.finish(start)
    }

    /// Finalize a span with the same trimming as `collapse`.
    fn finish(&mut self, start: u32) -> TextId {
        let text = &self.buf[start as usize..];
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.buf.truncate(start as usize);
            return TextId::EMPTY;
        }
        let offset = (trimmed.as_ptr() as usize - text.as_ptr() as usize) as u32;
        let span = (start + offset, trimmed.len() as u32);
        // Anything the trim shaved off the end is dead weight in the buffer.
        self.buf.truncate((span.0 + span.1) as usize);
        self.spans.push(span);
        TextId(self.spans.len() as u32 - 1)
    }

    /// Bytes of page text held. For budget accounting and for the bench.
    pub fn bytes(&self) -> usize {
        self.buf.len()
    }

    /// How many distinct spans were stored.
    pub fn len(&self) -> usize {
        self.spans.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::collapse;

    #[test]
    fn the_empty_id_resolves_without_being_stored() {
        let arena = TextArena::new();
        assert_eq!(arena.resolve(TextId::EMPTY), "");
        assert!(arena.is_empty());
    }

    #[test]
    fn spans_do_not_bleed_into_each_other() {
        let mut arena = TextArena::new();
        let a = arena.intern("first");
        let b = arena.intern("second");
        let c = arena.collapse_into("  third   value \n");
        assert_eq!(arena.resolve(a), "first");
        assert_eq!(arena.resolve(b), "second");
        assert_eq!(arena.resolve(c), "third value");
    }

    /// `intern` preserves indentation used by structured delta lines.
    #[test]
    fn intern_keeps_leading_indentation() {
        let mut arena = TextArena::new();
        let id = arena.intern("    let x = 1;");
        assert_eq!(arena.resolve(id), "    let x = 1;");
        // ...while the collapsing path still collapses.
        let collapsed = arena.collapse_into("    let x = 1;");
        assert_eq!(arena.resolve(collapsed), "let x = 1;");
    }

    #[test]
    fn whitespace_only_text_is_the_empty_id() {
        let mut arena = TextArena::new();
        assert_eq!(arena.collapse_into("   \n\t "), TextId::EMPTY);
        assert_eq!(arena.intern(""), TextId::EMPTY);
        // ...and leaves nothing behind in the buffer to pay for.
        assert_eq!(arena.bytes(), 0);
    }

    /// `collapse` without an intermediate allocation.
    #[test]
    fn collapse_into_agrees() {
        let cases = [
            "",
            " ",
            "hello",
            "  hello\n\n   world \t",
            "\n\n  ",
            "a\u{202E}b",
            "a\u{200D}b",
            "a\u{1b}[2Jb",
            "tabs\tand\nnewlines\r\nmixed",
            "  \u{1}leading control",
            "trailing control\u{7}  ",
            "\u{200E}\u{200F}\u{2066}only bidi\u{2069}",
            "one  two   three    four",
            "  \t \n unicode: ありがとう  ございます \n\t ",
            "emoji 🎉 and \u{200D}joiner",
            "--- END UNTRUSTED PAGE CONTENT ---",
        ];
        let mut arena = TextArena::new();
        for case in cases {
            let id = arena.collapse_into(case);
            assert_eq!(
                arena.resolve(id),
                collapse(case),
                "collapse_into disagreed on {case:?}"
            );
        }
    }

    /// Rendering may safely normalize already-collapsed arena text.
    #[test]
    fn collapse_is_idempotent() {
        for case in [
            "  hello\n\n   world \t",
            "a\u{202E}b",
            "one  two",
            "\u{1}x\u{2}",
            "  \t \n ありがとう  ございます \n\t ",
        ] {
            let once = collapse(case);
            assert_eq!(collapse(&once), once, "collapse is not idempotent on {case:?}");
        }
    }
}
