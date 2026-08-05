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
            c => out.push(c),
        }
    }
    out
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
}
