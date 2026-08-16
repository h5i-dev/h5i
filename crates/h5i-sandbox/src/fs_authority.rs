//! The filesystem-authority validator, ported from the Lean `H5iFs`
//! (ROADMAP.md §VF.4). Given the shipped plan's grant lists and the measured
//! world, it resolves each grant to the object it actually reaches — following
//! symlinks and honoring object identity — and checks the induced authority is
//! a subset of the source policy. A grant path that reaches a secret through a
//! planted symlink or hard link is rejected on the plan, before the box runs.
//!
//! This is a **port**, not the proof: the Lean `validate`/`validate_sound` is
//! the specification, and `tests/validate_drt.rs` differentially tests this
//! code against `h5i-spec --validate` over generated worlds. Kept a small pure
//! function so the sampling is strong evidence the two agree (the same
//! discipline as `effective::interferes` versus the Lean `interferesCheck`).
//!
//! Mirrors `lean/H5iFs/Core.lean` (`resolveFrom`) and `lean/H5iFs/Validate.lean`
//! (`EffectivePlan.authority`, `validate`) line for line, including the
//! fuel-bounded loop cutoff.

/// An object identity — opaque, not an inode number (see the Lean header).
pub type NodeId = u64;

/// A path as components, matching `H5iSpec.FsPath`.
pub type FsPath = Vec<String>;

/// What an object is; a symlink's target is an absolute component path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink(FsPath),
}

/// A directory entry: `name` under `parent` names `child`. A hard link is two
/// entries with one `child`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub parent: NodeId,
    pub name: String,
    pub child: NodeId,
}

/// The measured world: an object table and a name graph. (Content and metadata
/// are not needed by `validate` and are omitted from the port.)
#[derive(Clone, Debug, Default)]
pub struct FsState {
    pub nodes: Vec<(NodeId, NodeKind)>,
    pub entries: Vec<Entry>,
    pub root: NodeId,
}

/// The declared policy, as the object sets the user's grants denote.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub may_read: Vec<NodeId>,
    pub may_write: Vec<NodeId>,
}

/// The shipped plan's grants: read-only and read-write component paths.
#[derive(Clone, Debug, Default)]
pub struct EffectivePlan {
    pub ro: Vec<FsPath>,
    pub rw: Vec<FsPath>,
}

/// Default resolution fuel — the loop/depth cutoff. Matches `H5iFs.resolveFuel`.
pub const RESOLVE_FUEL: u32 = 64;

impl FsState {
    fn kind_of(&self, n: NodeId) -> Option<&NodeKind> {
        self.nodes.iter().find(|(id, _)| *id == n).map(|(_, k)| k)
    }

    fn child_of(&self, dir: NodeId, name: &str) -> Option<NodeId> {
        self.entries
            .iter()
            .find(|e| e.parent == dir && e.name == name)
            .map(|e| e.child)
    }

    /// Resolve `comps` from `cur`, following absolute symlink targets from the
    /// root, `fuel` bounding total steps. `None` on a missing component, a
    /// non-directory in the middle, or fuel exhausted by a loop. Mirrors
    /// `H5iFs.FsState.resolveFrom`.
    fn resolve_from(&self, fuel: u32, cur: NodeId, comps: &[String]) -> Option<NodeId> {
        if fuel == 0 {
            return None;
        }
        match comps.split_first() {
            None => Some(cur),
            Some((c, rest)) => {
                let child = self.child_of(cur, c)?;
                match self.kind_of(child) {
                    Some(NodeKind::Symlink(target)) => {
                        let mut next = target.clone();
                        next.extend_from_slice(rest);
                        self.resolve_from(fuel - 1, self.root, &next)
                    }
                    _ => self.resolve_from(fuel - 1, child, rest),
                }
            }
        }
    }

    /// Resolve an absolute path to the object it names.
    pub fn resolve(&self, path: &[String]) -> Option<NodeId> {
        self.resolve_from(RESOLVE_FUEL, self.root, path)
    }
}

impl EffectivePlan {
    /// The object-level authority the plan installs: read from ro+rw grants,
    /// write from rw grants only; grants that do not resolve are dropped.
    /// Mirrors `EffectivePlan.authority`.
    fn authority(&self, fs: &FsState) -> (Vec<NodeId>, Vec<NodeId>) {
        let readable = self
            .ro
            .iter()
            .chain(self.rw.iter())
            .filter_map(|p| fs.resolve(p))
            .collect();
        let writable = self.rw.iter().filter_map(|p| fs.resolve(p)).collect();
        (readable, writable)
    }
}

/// The validator: the plan's induced object authority is a subset of the
/// source policy. Mirrors `H5iFs.validate`; `validate_sound` is the Lean proof
/// that an accepted plan admits no effect the policy forbids.
pub fn validate(pol: &Policy, fs: &FsState, plan: &EffectivePlan) -> bool {
    let (readable, writable) = plan.authority(fs);
    readable.iter().all(|o| pol.may_read.contains(o))
        && writable.iter().all(|o| pol.may_write.contains(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Attacks` world in miniature: granted /work beside a secret /home
    /// chain, a planted symlink /work/evil and a hard link /work/alias.
    fn attacks_world() -> FsState {
        FsState {
            nodes: vec![
                (0, NodeKind::Dir),
                (1, NodeKind::Dir),
                (2, NodeKind::Dir),
                (3, NodeKind::Dir),
                (4, NodeKind::File),
                (5, NodeKind::Dir),
                (6, NodeKind::File),
                (7, NodeKind::Symlink(vec!["home".into(), "user".into(), ".ssh".into()])),
            ],
            entries: vec![
                Entry { parent: 0, name: "home".into(), child: 1 },
                Entry { parent: 1, name: "user".into(), child: 2 },
                Entry { parent: 2, name: ".ssh".into(), child: 3 },
                Entry { parent: 3, name: "id_rsa".into(), child: 4 },
                Entry { parent: 0, name: "work".into(), child: 5 },
                Entry { parent: 5, name: "main.rs".into(), child: 6 },
                Entry { parent: 5, name: "evil".into(), child: 7 },
                Entry { parent: 5, name: "alias".into(), child: 4 },
            ],
            root: 0,
        }
    }

    fn pol() -> Policy {
        Policy { may_read: vec![5, 6, 7], may_write: vec![5, 6, 7] }
    }

    #[test]
    fn accepts_benign_work_grant() {
        let plan = EffectivePlan { ro: vec![], rw: vec![vec!["work".into()]] };
        assert!(validate(&pol(), &attacks_world(), &plan));
    }

    #[test]
    fn rejects_symlink_grant() {
        let plan =
            EffectivePlan { ro: vec![vec!["work".into(), "evil".into()]], rw: vec![] };
        assert!(!validate(&pol(), &attacks_world(), &plan));
    }

    #[test]
    fn rejects_hardlink_grant() {
        let plan =
            EffectivePlan { ro: vec![], rw: vec![vec!["work".into(), "alias".into()]] };
        assert!(!validate(&pol(), &attacks_world(), &plan));
    }

    #[test]
    fn symlink_loop_fails_closed() {
        // a -> b, b -> a : resolving /a exhausts fuel, returns None (denied)
        let fs = FsState {
            nodes: vec![
                (0, NodeKind::Dir),
                (1, NodeKind::Symlink(vec!["b".into()])),
                (2, NodeKind::Symlink(vec!["a".into()])),
            ],
            entries: vec![
                Entry { parent: 0, name: "a".into(), child: 1 },
                Entry { parent: 0, name: "b".into(), child: 2 },
            ],
            root: 0,
        };
        assert_eq!(fs.resolve(&["a".into()]), None);
    }
}
