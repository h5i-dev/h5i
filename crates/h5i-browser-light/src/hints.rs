//! Hint labels: the short strings a human types to reach something on screen.
//!
//! A pointer is a poor instrument in a terminal. It reports cells rather than
//! pixels, it has no visible cursor once the viewer hides one, and every
//! movement's only feedback is a frame that arrives over a socket a few tens of
//! milliseconds later. Hints replace it with the thing terminals are actually
//! good at: naming a target and pressing a key.
//!
//! What makes this cheap here is that the hard half is already done elsewhere.
//! The hard half is deciding *what is actionable*, which every browser-side
//! implementation of this idea has to answer with a heuristic DOM walk of its
//! own. [`crate::snapshot`] already answers it, for the agent, with a rule the
//! verb layer then honours: a ref is minted only for something a caller could
//! act on. So a hint is a label stuck to a ref, and pressing it dispatches the
//! same verb an agent would have sent. The overlay cannot offer a target the
//! engine would refuse, because the overlay is not the one deciding.
//!
//! Labels are minted here, in the engine, rather than in each viewer. Two
//! viewers numbering the same page independently is two answers to a question
//! with one right answer, and the first time they disagreed it would be a human
//! pressing `sd` and activating something they were not looking at.

/// The alphabet labels are drawn from, most-reachable first.
///
/// Home row and its immediate neighbours, in the order Vimium settled on after
/// a long argument about finger travel. Deliberately not the whole alphabet:
/// every character added shortens labels by a fraction of a keystroke and costs
/// a hand movement on every label that uses it, and past about fourteen the
/// trade stops paying.
///
/// No character here may collide with a key that means something *during* hint
/// mode. `Escape` leaves, `Backspace` un-types, and neither is a letter, which
/// is what lets the whole alphabet stay available for labels.
pub const ALPHABET: &[u8] = b"sadfjklewcmpgh";

/// Prefix-free labels for `count` targets, shortest first.
///
/// Prefix-freeness is the property that makes typing a label unambiguous: if
/// `s` were both a label and the start of `sd`, pressing `s` could not be acted
/// on without waiting to see whether a `d` followed, and a viewer that waits is
/// a viewer that feels broken.
///
/// It is bought by splitting the labels into two lengths rather than padding
/// every label to the longest. With `k` characters and `n` targets, `d` digits
/// are needed; the first `(k^d - n) / k` values are spelled in `d - 1` and the
/// rest in `d`, which is exactly the count that leaves room for the long labels
/// to start past every short label's expansion. The short ones come first, so
/// the targets nearest the top of the document get the fewest keystrokes.
pub fn labels(count: usize) -> Vec<String> {
    let base = ALPHABET.len();
    if count == 0 {
        return Vec::new();
    }
    if count <= base {
        return (0..count).map(|i| spell(i, 1)).collect();
    }

    // Smallest `digits` with `base^digits >= count`. Computed by multiplying
    // rather than by logarithms: `count` is a count of DOM nodes, so it is
    // small, and a float log that lands a hair under an exact power would
    // produce labels one character too short and a collision to go with them.
    let mut digits = 1usize;
    let mut capacity = base;
    while capacity < count {
        // Saturating, so a page claiming an absurd number of targets runs out
        // of digits rather than wrapping to a tiny capacity.
        capacity = capacity.saturating_mul(base);
        digits += 1;
        if capacity == usize::MAX {
            break;
        }
    }

    let short = (capacity - count) / base;
    let long = count - short;
    let mut out = Vec::with_capacity(count);
    for i in 0..short {
        out.push(spell(i, digits - 1));
    }
    let start = short * base;
    for i in start..start + long {
        out.push(spell(i, digits));
    }
    out
}

/// `value` in base [`ALPHABET`], most significant first, left-padded to `width`.
fn spell(mut value: usize, width: usize) -> String {
    let base = ALPHABET.len();
    let mut chars = Vec::new();
    loop {
        chars.push(ALPHABET[value % base] as char);
        value /= base;
        if value == 0 {
            break;
        }
    }
    while chars.len() < width {
        chars.push(ALPHABET[0] as char);
    }
    chars.reverse();
    chars.into_iter().collect()
}

/// What typing `typed` has done to a set of labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Exactly one label is `typed`. Act on it.
    One(usize),
    /// Several labels still start with `typed`. Keep the overlay up, showing
    /// only these.
    Several(Vec<usize>),
    /// Nothing starts with `typed`. The human mistyped, and the honest answer
    /// is to say so rather than to act on the nearest thing.
    None,
}

/// Narrow `labels` by what has been typed so far.
///
/// Case-insensitive on the way in, because the labels are lower case and a
/// human with caps lock on has made a typing mistake rather than a different
/// request.
pub fn narrow(labels: &[String], typed: &str) -> Match {
    let typed = typed.to_ascii_lowercase();
    if typed.is_empty() {
        return Match::Several((0..labels.len()).collect());
    }
    let hits: Vec<usize> = labels
        .iter()
        .enumerate()
        .filter(|(_, label)| label.starts_with(&typed))
        .map(|(i, _)| i)
        .collect();
    match hits.len() {
        0 => Match::None,
        // An exact match is only decidable because the labels are prefix-free:
        // one hit that equals what was typed cannot be the start of another.
        1 if labels[hits[0]] == typed => Match::One(hits[0]),
        _ => Match::Several(hits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_page_with_no_targets_gets_no_labels() {
        assert!(labels(0).is_empty());
    }

    #[test]
    fn a_small_page_gets_one_character_labels() {
        let out = labels(5);
        assert_eq!(out, vec!["s", "a", "d", "f", "j"]);
    }

    /// The property the whole scheme rests on. Checked across the sizes where
    /// the two-length split actually happens rather than at one convenient
    /// count, because the split is where a prefix collision would be born.
    #[test]
    fn labels_are_never_prefixes_of_each_other() {
        for count in [1, 13, 14, 15, 27, 100, 196, 197, 500, 3000] {
            let out = labels(count);
            assert_eq!(out.len(), count, "count {count}");
            let unique: HashSet<&String> = out.iter().collect();
            assert_eq!(unique.len(), count, "count {count} has a duplicate label");
            for a in &out {
                for b in &out {
                    if a != b {
                        assert!(
                            !b.starts_with(a.as_str()),
                            "count {count}: `{a}` is a prefix of `{b}`"
                        );
                    }
                }
            }
        }
    }

    /// Short labels go to the top of the document, which is where the reader
    /// already is.
    #[test]
    fn the_earliest_targets_get_the_shortest_labels() {
        let out = labels(30);
        let first = out[0].len();
        let last = out[out.len() - 1].len();
        assert!(first < last, "{out:?}");
        // And the lengths never go back down, so "shortest first" is a rule
        // rather than a coincidence of this count.
        for pair in out.windows(2) {
            assert!(pair[0].len() <= pair[1].len(), "{out:?}");
        }
    }

    #[test]
    fn typing_a_whole_label_picks_exactly_one_target() {
        let out = labels(200);
        assert_eq!(narrow(&out, &out[7]), Match::One(7));
    }

    #[test]
    fn a_partial_label_keeps_the_overlay_up() {
        let out = labels(200);
        let prefix = &out[out.len() - 1][..1];
        match narrow(&out, prefix) {
            Match::Several(hits) => assert!(hits.len() > 1),
            other => panic!("expected several, got {other:?}"),
        }
    }

    #[test]
    fn a_key_no_label_starts_with_matches_nothing() {
        let out = labels(3);
        assert_eq!(narrow(&out, "z"), Match::None);
    }

    #[test]
    fn capitals_are_a_typing_mistake_rather_than_a_different_request() {
        let out = labels(200);
        let upper = out[7].to_ascii_uppercase();
        assert_eq!(narrow(&out, &upper), Match::One(7));
    }
}
