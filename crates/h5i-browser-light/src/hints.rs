//! Hint labels: the short strings a human types to reach something on screen.
//!
//! A hint is a label stuck to a [`crate::snapshot`] ref, so the overlay cannot
//! offer a target the verb layer would refuse — the snapshot already decided
//! what is actionable.
//!
//! Design: `docs/design-interminal-browser.md` V2.
//!
//! Labels are minted here so two viewers watching one page cannot disagree about
//! what `sd` means. *Matching* them against what has been typed is per-viewer
//! state and lives with each viewer (`h5i_core::termview::vim::narrow`, and the
//! same rule in JavaScript). Both halves rest on [`labels`] being prefix-free.

/// The alphabet labels are drawn from, most-reachable first.
///
/// Vimium's set: home row and its neighbours. Not the whole alphabet, because
/// past about fourteen characters the shorter labels stop paying for the extra
/// finger travel.
pub const ALPHABET: &[u8] = b"sadfjklewcmpgh";

/// Prefix-free labels for `count` targets, shortest first.
///
/// Prefix-freeness is what lets a viewer act the moment a label is complete,
/// with nothing to wait for. It comes from using two lengths rather than padding
/// to the longest: with `k` characters and `n` targets needing `d` digits, the
/// first `(k^d - n) / k` values are spelled in `d - 1` and the rest in `d`,
/// which leaves the long labels starting past every short one's expansion.
pub fn labels(count: usize) -> Vec<String> {
    let base = ALPHABET.len();
    if count == 0 {
        return Vec::new();
    }
    if count <= base {
        return (0..count).map(|i| spell(i, 1)).collect();
    }

    // Smallest `digits` with `base^digits >= count`, by multiplying rather than
    // by logarithms: a float log landing a hair under an exact power would give
    // labels one character too short, and a collision with them.
    let mut digits = 1usize;
    let mut capacity = base;
    while capacity < count {
        // Saturating: an absurd target count must run out of digits rather
        // than wrap to a tiny capacity.
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

    /// The property the scheme rests on, checked across the counts where the
    /// two-length split happens — which is where a collision would be born.
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

    /// Short labels go to the top of the document.
    #[test]
    fn the_earliest_targets_get_the_shortest_labels() {
        let out = labels(30);
        let first = out[0].len();
        let last = out[out.len() - 1].len();
        assert!(first < last, "{out:?}");
        // And never go back down, so "shortest first" is a rule.
        for pair in out.windows(2) {
            assert!(pair[0].len() <= pair[1].len(), "{out:?}");
        }
    }
}
