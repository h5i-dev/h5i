//! Making untrusted strings safe to show.
//!
//! [`sanitize_display`] and [`sanitize_block`] neutralise terminal control
//! sequences at the boundary where box-written or externally-fetched data reaches
//! a host-side surface. The other half of that defense, scrubbing secrets out of
//! anything about to be *stored*, is `h5i_core::secrets`, applied by
//! `h5i_core::receipt::append` before anything is written, and it needs more than
//! an error crate should carry.
//!
//! These are re-exported as `h5i_core::redact`, where they used to live. They sit
//! here so that `h5i-sandbox`, which is below `h5i-core`, can reach them: a
//! sanitiser only some of the workspace can call is one some of the workspace
//! will skip, and it did.

/// Make an untrusted string safe to print to a terminal.
///
/// Manifest ids, slugs and spool records can be written inside a box or pulled
/// from another clone, so they are untrusted input. A hostile value could embed
/// ANSI/OSC escape sequences (cursor moves, colour resets, clickable hyperlinks)
/// or newlines to spoof a status line. We drop control characters entirely
/// (neutralising the `ESC` that begins every escape sequence) and fold
/// tab/newline/CR into single spaces. Storage keeps the exact bytes; only
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

/// [`sanitize_display`] for text that is *meant* to have lines.
///
/// The single-line form folds `\n` into a space, which is right for a slug or a
/// status line and destroys a captured command's output, so a payload that needed
/// sanitising was left unsanitised instead, because the only tool available would
/// have run it all together. This sanitises each line and keeps the breaks, so a
/// recorded log stays a log while the escape sequences that would rewrite the
/// lines *around* it are gone.
///
/// A `\r` inside a line still becomes a space: carriage-return-to-column-zero is
/// how a single line overwrites what was printed before it, the same spoof as an
/// escape sequence with none of the escape.
pub fn sanitize_block(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&sanitize_display(line));
    }
    out
}

/// Bidirectional formatting characters, which reorder the text *around* them.
///
/// These are not control characters (`char::is_control` is false for every one of
/// them) so the pass above lets them through, and they are the sharpest remaining
/// tool for spoofing a rendered string. `http://evil.com/` followed by an
/// override and a reversed string displays as some other host entirely, with no
/// escape sequence involved anywhere. That matters most in the terminal viewer's
/// status line, whose entire claim is that the origin it shows is the origin you
/// are looking at, but a receipt or a report is just as misleading when it is
/// read.
///
/// Only the overrides, embeddings and isolates are dropped. The zero-width joiner
/// and non-joiner (`U+200C`, `U+200D`) are deliberately kept: they are in the
/// same Unicode category but carry no reordering power, and they are what holds a
/// multi-part emoji together in ordinary text.
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
    fn a_block_keeps_its_lines_and_loses_its_escapes() {
        // The single-line form folds `\n` into a space, which makes a captured
        // log unreadable, so a payload that needed sanitising was printed raw
        // instead. This keeps the shape and drops the sequences.
        let log = "building\n\u{1b}[2Jerror: \u{1b}[31mfailed\u{1b}[0m\ndone";
        let safe = sanitize_block(log);
        assert_eq!(safe, "building\n[2Jerror: [31mfailed[0m\ndone");
        assert!(!safe.contains('\u{1b}'));
        assert_eq!(safe.lines().count(), 3);

        // A bare CR is the same overwrite with no escape in it.
        assert_eq!(sanitize_block("a\rspoof\nb"), "a spoof\nb");
        // Framing is preserved exactly, trailing newline included.
        assert_eq!(sanitize_block("a\n"), "a\n");
        assert_eq!(sanitize_block(""), "");
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
