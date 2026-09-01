//! Terminal columns, which are not characters.
//!
//! The viewer's chrome (the status row, the log pane) claims to occupy exactly
//! the cells it was given, and that claim is what keeps page text out of it.
//! Counting `char`s breaks the claim in both directions: one CJK codepoint
//! paints two cells, so a row measured at the terminal's width wraps onto the
//! next one and repaints the page, and a combining mark paints none, so a row
//! measured as full is short and leaves the previous frame showing. Both are
//! reachable from a hostile page, which chooses the URL and the console text.

use unicode_width::UnicodeWidthChar;

/// Cells `c` paints. Zero for anything with no width of its own; control
/// characters are already gone by here (see [`crate::redact::sanitize_display`]).
fn cell(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Cells `s` paints.
pub fn width(s: &str) -> usize {
    s.chars().map(cell).sum()
}

/// The longest prefix of `s` that fits in `cols` cells.
///
/// A double-width glyph straddling the edge is dropped rather than half-drawn,
/// so the result is never wider than asked for.
pub fn head(s: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = cell(c);
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The longest *suffix* of `s` that fits in `cols` cells. What keeps the
/// identifying end of a host when the front has to go.
pub fn tail(s: &str, cols: usize) -> String {
    let mut kept = std::collections::VecDeque::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let w = cell(c);
        if used + w > cols {
            break;
        }
        kept.push_front(c);
        used += w;
    }
    kept.into_iter().collect()
}

/// `s` truncated to `cols` cells and space-padded to exactly that many.
pub fn fit(s: &str, cols: usize) -> String {
    let mut out = head(s, cols);
    for _ in width(&out)..cols {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_glyph_costs_two_cells_and_a_combining_mark_none() {
        assert_eq!(width("ab"), 2);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{301}"), 1);
    }

    #[test]
    fn a_row_of_wide_glyphs_still_fits_the_row_it_was_given() {
        // The bug this module exists for: eighty `char`s of CJK is a hundred
        // and sixty cells, so a status row measured in characters wrapped and
        // painted over the page below it.
        let hostile: String = "日".repeat(80);
        assert_eq!(fit(&hostile, 80).chars().count(), 40);
        assert_eq!(width(&fit(&hostile, 80)), 80);
    }

    #[test]
    fn a_glyph_that_straddles_the_edge_is_dropped_rather_than_halved() {
        assert_eq!(head("a日b", 2), "a");
        assert_eq!(width(&fit("a日b", 2)), 2);
    }

    #[test]
    fn padding_fills_what_the_text_does_not() {
        assert_eq!(fit("ab", 5), "ab   ");
        assert_eq!(fit("", 3), "   ");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn the_tail_keeps_the_end_a_host_is_identified_by() {
        assert_eq!(tail("bank.example.evil.test", 9), "evil.test");
        assert_eq!(tail("日本語", 4), "本語");
        // Same straddle rule, at the other edge.
        assert_eq!(tail("日本語", 3), "語");
    }
}
