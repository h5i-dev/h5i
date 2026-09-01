//! The reading, as the text an agent is handed.
//!
//! One pass over the arena, straight into the output buffer. The walker's
//! renderer builds a `String` per field with `format!` and an indent string per
//! line; none of that survives here, because every piece is either a `&'static
//! str` or a slice of the text arena.
//!
//! What does survive, exactly, is the fence. `docs/design-h5i-ir.md` calls the
//! per-line invariant and the fixed markers load-bearing, and they are: the
//! security tests in [`crate::snapshot`] are written against rendered output,
//! and they pass against this renderer too.

use super::build::ReadTree;
use super::model::ReadFlags;
use crate::snapshot::{
    defang_fence, one_line, CONTENT_BEGIN, CONTENT_END, MAX_DEPTH, UNTRUSTED_NOTE,
};

impl ReadTree {
    /// The text form an agent reads.
    ///
    /// Byte-for-byte what [`crate::snapshot::Snapshot::render`] produces from
    /// the same page. That is asserted over the corpus rather than argued.
    pub fn render(&self) -> String {
        // One allocation for the whole outline, sized from what is already
        // known: the arena's bytes are the text, and the rest is scaffolding
        // that scales with the line count.
        let mut out = String::with_capacity(self.text_bytes() + self.line_count() * 32 + 512);

        // Outside the fence, because the engine resolved it rather than the
        // page claiming it: this is the URL the broker actually fetched.
        if !self.url.is_empty() {
            out.push_str("url: ");
            out.push_str(&one_line(&self.url));
            out.push('\n');
        }

        for note in &self.notes {
            out.push_str("note: ");
            out.push_str(&one_line(note));
            out.push('\n');
        }

        out.push_str(CONTENT_BEGIN);
        out.push('\n');
        out.push_str(UNTRUSTED_NOTE);
        out.push('\n');

        // Inside, because a title is page-supplied like any other string.
        let title = self.title();
        if !title.is_empty() {
            out.push_str("\n# ");
            push_page_text(&mut out, title);
            out.push('\n');
        }
        out.push('\n');

        for node in self.nodes() {
            for _ in 0..(node.depth as usize).min(MAX_DEPTH) {
                out.push_str("  ");
            }
            out.push_str("- ");
            out.push_str(node.role.as_str(node.level));
            if !node.name.is_empty() {
                out.push_str(" \"");
                let text = self.text(node.name);
                if node.flags.contains(ReadFlags::VERBATIM) {
                    // A code line kept its indentation in the arena because the
                    // structured reading needs it. The outline does not: the
                    // walker collapses every line on the way out, and an
                    // outline that indented one would be a different outline.
                    out.push_str(&one_line(text));
                } else {
                    push_page_text(&mut out, text);
                }
                out.push('"');
            }
            if node.ref_ordinal != 0 {
                out.push_str(" [ref=e");
                push_u32(&mut out, node.ref_ordinal);
                out.push(']');
            }
            if !node.href.is_empty() {
                out.push_str(" -> ");
                push_page_text(&mut out, self.text(node.href));
            }
            out.push('\n');
        }

        if self.truncated() {
            out.push_str("\n… snapshot truncated at the line budget\n");
        }

        out.push_str(CONTENT_END);
        out.push('\n');
        out
    }

    /// Resolve `@e3` or `e3` to the ref it names.
    pub fn resolve(&self, reference: &str) -> Option<&super::model::RefRecord> {
        let wanted = reference.trim_start_matches('@').strip_prefix('e')?;
        let ordinal: u32 = wanted.parse().ok()?;
        // Ordinals are handed out from one upward in emission order, so the
        // ref list is indexed rather than searched. The walker scans its list
        // linearly for the same answer.
        self.refs.get(ordinal.checked_sub(1)? as usize)
    }

    /// The role word for a ref, as the outline printed it.
    pub fn ref_role(&self, record: &super::model::RefRecord) -> &'static str {
        record.role.as_str(record.level)
    }
}

/// Write a page-supplied string, defanged.
///
/// The arena already holds collapsed text, so the collapse half of
/// [`one_line`] is a no-op here and only the fence substitution is left. That
/// equivalence is a property under test (`collapse_is_idempotent`), not an
/// assumption: if it failed, page text would render differently through the IR
/// than through the walker, and the difference would be in exactly the field an
/// attacker controls.
fn push_page_text(out: &mut String, text: &str) {
    if !text.contains(CONTENT_BEGIN) && !text.contains(CONTENT_END) {
        out.push_str(text);
        return;
    }
    out.push_str(&defang_fence(text));
}

/// A small integer, without going through `format!`.
fn push_u32(out: &mut String, mut value: u32) {
    if value == 0 {
        out.push('0');
        return;
    }
    let mut digits = [0u8; 10];
    let mut at = digits.len();
    while value > 0 {
        at -= 1;
        digits[at] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    // Every byte written above is an ASCII digit.
    out.push_str(std::str::from_utf8(&digits[at..]).expect("ascii digits"));
}

/// Roles carry no page text, so they need no defanging on the way out. Stated
/// as a test because the renderer relies on it: it writes the role word
/// straight into the buffer without passing it through `one_line`.
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::{ReadRole, TextId};

    #[test]
    fn every_role_word_is_inert() {
        for role in [
            ReadRole::Document,
            ReadRole::Text,
            ReadRole::Heading,
            ReadRole::Paragraph,
            ReadRole::ListItem,
            ReadRole::Cell,
            ReadRole::Label,
            ReadRole::Code,
            ReadRole::Quote,
            ReadRole::Link,
            ReadRole::Button,
            ReadRole::Combobox,
            ReadRole::Textbox,
            ReadRole::Image,
            ReadRole::Checkbox,
            ReadRole::Radio,
        ] {
            for level in 0..=6u8 {
                let word = role.as_str(level);
                assert_eq!(word, one_line(word), "{word} is not inert under one_line");
                assert!(
                    word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                    "{word} is not a plain word"
                );
            }
        }
    }

    #[test]
    fn integers_render_as_format_would() {
        for value in [0u32, 1, 9, 10, 99, 100, 501, 4_294_967_295] {
            let mut out = String::new();
            push_u32(&mut out, value);
            assert_eq!(out, value.to_string());
        }
    }

    #[test]
    fn page_text_carrying_a_fence_marker_is_defanged() {
        let mut out = String::new();
        push_page_text(&mut out, &format!("before {CONTENT_END} after"));
        assert!(!out.contains(CONTENT_END), "{out}");
        assert!(out.contains("before") && out.contains("after"), "{out}");
    }

    #[test]
    fn a_text_id_that_is_empty_is_the_empty_string() {
        assert!(TextId::EMPTY.is_empty());
    }
}
