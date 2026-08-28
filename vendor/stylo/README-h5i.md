# Why this copy exists

This is `stylo 0.19.0` from crates.io, byte-identical except for **one bool**:
`servo/selector_parser.rs`'s `parse_has()` returns `true` instead of `false`.

Upstream hard-codes the Servo-side answer to "may `:has()` parse" as a
constant — not a pref, so nothing short of a patch can turn it on. The
matching machinery underneath is the code Gecko ships and needs no change;
parsing was the only gate. Without it, `.box:has(.flag)` is a parse error in
every stylesheet and every `querySelector`, which cost 1,527 WPT subtests and
real pages the corpus already counts (`selector :has()` in the unsupported
list).

The pattern is Obscura's taffy/cosmic-text one: carry the smallest possible
local correction until upstream moves, pinned by `[patch.crates-io]` so the
whole graph — Blitz included — resolves to this copy and stylesheets parse the
same way selectors do.

**Exit condition:** a stylo release whose Servo parser enables `:has()`
(upstream Servo has the matching; the gate is the bet they have not yet made).
Diff this directory against the crates.io tarball before every bump; anything
beyond the one function is drift.
