//! Making untrusted strings safe to show and safe to store.
//!
//! Two different defenses live here, both applied at the boundary where
//! box-written or externally-fetched data reaches a host-side surface:
//!
//! * [`sanitize_display`] neutralises terminal control sequences before a
//!   string is printed.
//! * Secret scrubbing is [`crate::secrets::redact_text`], applied by
//!   [`crate::receipt::append`] before anything is written.

/// Make an untrusted string safe to print to a terminal.
///
/// Manifest ids, slugs and spool records can be written inside a box or pulled
/// from another clone, so they are untrusted input. A hostile value could
/// embed ANSI/OSC escape sequences (cursor moves, colour resets, clickable
/// hyperlinks) or newlines to spoof a status line. We drop control characters
/// entirely (neutralising the `ESC` that begins every escape sequence) and
/// fold tab/newline/CR into single spaces. Storage keeps the exact bytes; only
/// rendering is sanitised.
pub fn sanitize_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {} // drop ESC and other C0/C1/DEL controls
            c if is_bidi_control(c) => {} // see below
            c => out.push(c),
        }
    }
    out
}

/// Bidirectional formatting characters, which reorder the text *around* them.
///
/// These are not control characters — `char::is_control` is false for every one
/// of them — so the pass above lets them through, and they are the sharpest
/// remaining tool for spoofing a rendered string. `http://evil.com/` followed
/// by an override and a reversed string displays as some other host entirely,
/// with no escape sequence involved anywhere. That matters most in the terminal
/// viewer's status line, whose entire claim is that the origin it shows is the
/// origin you are looking at, but a receipt or a report is just as misleading
/// when it is read.
///
/// Only the overrides, embeddings and isolates are dropped. The zero-width
/// joiner and non-joiner (`U+200C`, `U+200D`) are deliberately kept: they are
/// in the same Unicode category but they carry no reordering power, and they
/// are what holds a multi-part emoji together in ordinary text.
fn is_bidi_control(c: char) -> bool {
    matches!(c,
        '\u{200E}' | '\u{200F}'          // LRM, RLM
        | '\u{202A}'..='\u{202E}'        // LRE, RLE, PDF, LRO, RLO
        | '\u{2066}'..='\u{2069}'        // LRI, RLI, FSI, PDI
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_escape_sequences() {
        let hostile = "ok\u{1b}[2J\u{1b}]8;;http://evil\u{7}click";
        let safe = sanitize_display(hostile);
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{7}'));
        assert!(safe.starts_with("ok"));
    }

    #[test]
    fn folds_newlines_into_spaces() {
        assert_eq!(sanitize_display("a\nb\tc\rd"), "a b c d");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        assert_eq!(sanitize_display("env/claude/fix-auth"), "env/claude/fix-auth");
    }

    #[test]
    fn drops_the_bidi_controls_that_rewrite_what_a_reader_sees() {
        // No escape sequence involved: the override reorders the characters
        // after it, so a hostile URL renders as a host it is not. The viewer's
        // status line claims to show the origin you are looking at, and this is
        // what that claim rests on.
        let spoof = "http://evil.example/\u{202E}gro.knab//:sptth";
        let safe = sanitize_display(spoof);
        assert!(!safe.contains('\u{202E}'), "{safe:?}");
        assert!(safe.starts_with("http://evil.example/"), "{safe:?}");
        for c in ['\u{200E}', '\u{200F}', '\u{202A}', '\u{202D}', '\u{2066}', '\u{2069}'] {
            assert!(!sanitize_display(&format!("a{c}b")).contains(c), "{c:?} survived");
        }
    }

    #[test]
    fn keeps_the_zero_width_joiners_that_ordinary_text_needs() {
        // Same Unicode category as the overrides, no reordering power, and
        // what holds a multi-part emoji together. Dropping them would corrupt
        // legitimate text in every receipt and report.
        assert_eq!(sanitize_display("a\u{200D}b"), "a\u{200D}b");
        assert_eq!(sanitize_display("👩\u{200D}💻"), "👩\u{200D}💻");
    }
}
