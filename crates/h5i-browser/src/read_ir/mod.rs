//! The Read IR: the page as an agent reads it, once.
//!
//! `docs/design/design-h5i-ir.md` is the specification. In one paragraph: the DOM
//! stays the source of truth, and this is a compact, read-only semantic cache
//! over it holding exactly what the snapshot, the delta and the markdown need,
//! so that the cost of an agent's step tracks what the step changed rather than
//! how big the page is.
//!
//! Phase 1 is what is here: a transient tree, built and dropped per reading,
//! rendering byte-identical output to the walker it replaces. Phases 2 and 3
//! (stable refs, a retained cache, incremental invalidation) are what the
//! shapes here are built to carry, and are deliberately not built yet.

pub mod build;
pub mod delta;
pub mod model;
pub mod render_snapshot;
pub mod text_arena;

#[cfg(test)]
mod equivalence;

pub use build::ReadTree;
pub use model::{ReadFlags, ReadId, ReadNode, ReadRole, RefRecord, TextId};
pub use text_arena::TextArena;

use crate::snapshot::{Line, RefEntry, Snapshot};

impl ReadTree {
    /// The same reading in the walker's own types.
    ///
    /// The compatibility seam, and the reason phase 1 can land without
    /// touching a single caller: everything that consumes a [`Snapshot`] keeps
    /// working, and the paths that only want text can take [`ReadTree::render`]
    /// instead and skip building any of this.
    ///
    /// It costs what the walker used to cost, which is the point of measuring
    /// the two separately: this is the price of the old shape, not of the IR.
    pub fn to_snapshot(&self) -> Snapshot {
        Snapshot {
            url: self.url().to_string(),
            title: self.title().to_string(),
            lines: self
                .nodes()
                .iter()
                .map(|node| Line {
                    depth: node.depth as usize,
                    role: node.role.as_str(node.level).to_string(),
                    text: self.text(node.name).to_string(),
                    reference: (node.ref_ordinal != 0)
                        .then(|| format!("e{}", node.ref_ordinal)),
                    href: (!node.href.is_empty()).then(|| self.text(node.href).to_string()),
                })
                .collect(),
            refs: self.ref_entries(),
            truncated: self.truncated(),
            notes: self.notes().to_vec(),
        }
    }

    /// The refs, spelled out the way the session hands them to an agent.
    pub fn ref_entries(&self) -> Vec<RefEntry> {
        self.refs()
            .iter()
            .enumerate()
            .map(|(at, record)| RefEntry {
                id: format!("e{}", at + 1),
                node_id: record.dom_id as usize,
                role: record.role.as_str(record.level).to_string(),
                name: self.text(record.name).to_string(),
                href: (!record.href.is_empty()).then(|| self.text(record.href).to_string()),
            })
            .collect()
    }
}
