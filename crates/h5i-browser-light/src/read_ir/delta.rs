//! What changed between two readings.
//!
//! Phase 1 carries the half of `docs/design-h5i-ir.md`'s delta design that can
//! be had from a transient tree: the unchanged answer, without materialising
//! either reading's lines. Two trees of the same page compare as fixed-width
//! nodes and interned spans, so "nothing moved" costs a scan of the node arena
//! and no allocation at all.
//!
//! The revision-stamped change log, and with it an unchanged delta that costs
//! nothing because no second tree was ever built, arrives with the retained
//! cache in phase 3. What is deliberately *not* here is a second diff
//! algorithm: when a page really did change, this defers to the walker's
//! longest common subsequence, so the engine holds exactly one opinion about
//! which lines were added and which were removed.

use crate::snapshot::{Delta, REPLACED_SURVIVAL};

use super::build::ReadTree;
use super::model::ReadNode;

impl ReadTree {
    /// Whether these two readings would print the same outline.
    ///
    /// The IR's answer to `line_identity`: the same four facts, compared as
    /// integers and string slices rather than assembled into a
    /// separator-joined `String` per line first.
    ///
    /// The ref is excluded, exactly as the walker excludes it. Refs are
    /// numbered by position, so an element that kept its text but shifted down
    /// the page would otherwise read as a removal and an addition, and every
    /// insertion near the top would report the rest of the page as new.
    pub fn same_reading_as(&self, previous: &ReadTree) -> bool {
        if self.line_count() != previous.line_count() {
            return false;
        }
        std::iter::zip(self.nodes(), previous.nodes())
            .all(|(a, b)| self.same_line(a, previous, b))
    }

    fn same_line(&self, a: &ReadNode, other: &ReadTree, b: &ReadNode) -> bool {
        a.depth == b.depth
            && a.role == b.role
            && a.level == b.level
            && self.text(a.name) == other.text(b.name)
            && self.text(a.href) == other.text(b.href)
    }

    /// This reading, expressed as its difference from an earlier one.
    ///
    /// Falls back to the walker's own difference when the page moved, which
    /// costs a materialisation of both readings. That is the honest shape of
    /// phase 1: the win is the case where nothing changed, which is most of
    /// them, and buying the other case needs the retained tree of phase 3.
    pub fn delta(&self, previous: &ReadTree) -> Delta {
        if self.same_reading_as(previous) {
            let url_changed = self.url != previous.url;
            let unchanged = self.line_count();
            // Computed the way `Snapshot::delta` computes it, empty-page branch
            // included: a reading with no lines at all is called replaced
            // rather than unchanged, because there is nothing there to have
            // recognised.
            let survival = if previous.line_count() == 0 { 0.0 } else { 1.0 };
            return Delta {
                url: self.url.clone(),
                replaced: url_changed || survival < REPLACED_SURVIVAL,
                url_changed,
                title_changed: self.title() != previous.title(),
                added: Vec::new(),
                removed: Vec::new(),
                unchanged,
                notes: self.notes.clone(),
            };
        }

        self.to_snapshot().delta(&previous.to_snapshot())
    }
}
