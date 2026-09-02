//! What changed between two readings.
//!
//! Phase 1 handles unchanged transient trees without materialising their
//! lines. It compares fixed-width nodes and arena spans, requiring one scan
//! and only the allocation for the result URL.
//!
//! Phase 2 avoids building the second tree; phase 3 handles changed pages
//! through a revision-stamped change log.
//!
//! Changed trees use the walker's longest common subsequence, keeping one
//! definition of added and removed lines.

use crate::snapshot::{Delta, REPLACED_SURVIVAL};

use super::build::ReadTree;
use super::model::ReadNode;

impl ReadTree {
    /// Whether these two readings would print the same outline.
    ///
    /// Equivalent to `line_identity`, but compares fields directly.
    ///
    /// Refs are excluded because positional renumbering is not a content
    /// change.
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
    /// Changed pages fall back to the walker, materialising both readings.
    pub fn delta(&self, previous: &ReadTree) -> Delta {
        if self.same_reading_as(previous) {
            let url_changed = self.url != previous.url;
            let unchanged = self.line_count();
            // Match `Snapshot::delta`: an empty reading is replaced.
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
