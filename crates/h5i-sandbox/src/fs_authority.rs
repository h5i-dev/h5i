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

/// Whether the filesystem-authority validator runs at all. **Fully opt-in**:
/// unset means the validator never executes — no computation, no host
/// measurement, no manifest field, no gate — so default behavior is exactly as
/// before this code existed. Set `H5I_FS_AUTHORITY_ENFORCE=1` to compute the
/// verdict at box create and run, record it, and fail closed on a violation
/// (ROADMAP.md §VF.4, the §V4 gating discipline: earn trust before it gates by
/// default).
pub fn enforce_enabled() -> bool {
    std::env::var_os("H5I_FS_AUTHORITY_ENFORCE").is_some_and(|v| v == "1")
}

/// The per-run verdict on a shipped effective config, one boolean per claim
/// (ROADMAP.md §VF.4). Recorded in the box manifest and rendered in
/// `box status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityVerdict {
    /// Every effective grant is one the declared policy authorized: the
    /// translation-validation of `compute_effective`'s output against intent.
    pub fs_subset: bool,
    /// Every read-write grant was declared writable ($WORK or `fs_write`).
    pub writes_confined: bool,
    /// No read-only overlay was left writable: the config-lock pin and the warm
    /// cache stay read-only. (Private, home-state, and the one cache-rw refresh
    /// bind are writable by design and not constrained here.)
    pub cache_readonly: bool,
    /// No effective grant, and no bind source or mountpoint beneath the
    /// worktree, resolves out through a planted symlink on the host. `None` when
    /// the host was not measured (non-Linux, or measurement skipped).
    pub symlink_clean: Option<bool>,
}

impl AuthorityVerdict {
    /// The gating verdict: the statically-decidable claims all hold. A false
    /// here is a real config/logic bug and is safe to fail a launch on;
    /// `symlink_clean` is evidence, surfaced but reported separately.
    pub fn confined(&self) -> bool {
        self.fs_subset && self.writes_confined && self.cache_readonly
    }
}

/// `sub` is a subset of `sup`, as sets of path strings.
fn subset(sub: &[String], sup: &[String]) -> bool {
    sub.iter().all(|s| sup.contains(s))
}

/// **The per-run translation validator** (ROADMAP.md §VF.4): re-check the
/// shipped effective grants against the declared policy, independently of the
/// resolver that produced them. This is the path-level companion to the proved
/// object-level [`validate`]: that one formalizes resolve-then-subset on an
/// abstract world; this one applies the subset claims to real output, catching
/// a `compute_effective` bug the way translation validation catches a compiler
/// bug.
///
/// - `declared_ro` / `declared_rw`: the source policy's read / read-write grant
///   paths, expanded exactly as `compute_effective` expands them ($WORK and
///   tilde), so a like-for-like subset check is meaningful.
/// - `eff_ro` / `eff_rw`: the effective config's `landlock.ro` / `landlock.rw`.
/// - `overlays_read_only`: the read-only overlay binds (config-lock, cache-ro)
///   are all non-writable (the caller computes this from the bind manifest;
///   private/home-state/cache-rw binds are writable by design).
pub fn validate_grants(
    declared_ro: &[String],
    declared_rw: &[String],
    eff_ro: &[String],
    eff_rw: &[String],
    overlays_read_only: bool,
) -> AuthorityVerdict {
    // Read authority the policy declares = read grants plus everything writable
    // (a write grant carries read), matching the abstract `readGrantPaths`.
    let declared_read: Vec<String> =
        declared_ro.iter().chain(declared_rw.iter()).cloned().collect();
    let eff_read: Vec<String> = eff_ro.iter().chain(eff_rw.iter()).cloned().collect();
    AuthorityVerdict {
        fs_subset: subset(&eff_read, &declared_read),
        writes_confined: subset(eff_rw, declared_rw),
        cache_readonly: overlays_read_only,
        symlink_clean: None,
    }
}

/// Which of `paths`, resolved on the host, escape the managed worktree through
/// a symlink? A path at or above `work` is the user's declared choice and is
/// not second-guessed; a path **beneath** `work` whose canonical form leaves
/// `work` is the planted-symlink escape (§VF.5) — the previous run's agent
/// redirected a worktree path out. Callers pass the landlock grants and the
/// bind sources/mountpoints; paths outside the worktree (h5i's managed cache
/// and home-state dirs) are ignored by construction. Returns the offenders.
///
/// Linux/Unix only (it canonicalizes on the host); the caller records `Some`
/// only where it ran.
#[cfg(unix)]
pub fn symlink_escapes(work: &std::path::Path, paths: &[String]) -> Vec<String> {
    let work_canon = std::fs::canonicalize(work).unwrap_or_else(|_| work.to_path_buf());
    let mut escapes: Vec<String> = paths
        .iter()
        .filter(|p| {
            let path = std::path::Path::new(p.as_str());
            // Only paths lexically beneath the worktree are constrained to it.
            if !path.starts_with(work) || path == work {
                return false;
            }
            match std::fs::canonicalize(path) {
                Ok(canon) => !canon.starts_with(&work_canon),
                // Unresolvable (missing/broken link) is fail-closed: flag it.
                Err(_) => true,
            }
        })
        .cloned()
        .collect();
    escapes.sort();
    escapes.dedup();
    escapes
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
    fn validate_grants_accepts_effective_subset() {
        // Effective grants derived from declared (exists-filter only removes),
        // so the translation-validation subset holds.
        let declared_ro = vec!["/etc/hosts".to_string(), "/opt/tools".to_string()];
        let declared_rw = vec!["/work".to_string(), "/work/out".to_string()];
        let eff_ro = vec!["/etc/hosts".to_string()]; // /opt/tools missing → skipped
        let eff_rw = vec!["/work".to_string(), "/work/out".to_string()];
        let v = validate_grants(&declared_ro, &declared_rw, &eff_ro, &eff_rw, true);
        assert!(v.confined(), "{v:?}");
    }

    #[test]
    fn validate_grants_rejects_undeclared_write() {
        // A write grant the policy never declared writable — a compute bug.
        let v = validate_grants(
            &[],
            &["/work".to_string()],
            &[],
            &["/work".to_string(), "/etc".to_string()],
            true,
        );
        assert!(!v.writes_confined);
        assert!(!v.fs_subset);
    }

    #[test]
    fn validate_grants_rejects_writable_overlay() {
        let v = validate_grants(&[], &["/work".to_string()], &[], &["/work".to_string()], false);
        assert!(!v.cache_readonly);
        assert!(!v.confined());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_beneath_worktree_is_flagged() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let outside = tmp.path().join("secret");
        std::fs::create_dir_all(&outside).unwrap();
        // A real subdir under the worktree, and a symlink under it escaping out
        // (the shape of a config-lock mountpoint the agent redirected).
        let good = work.join("src");
        std::fs::create_dir_all(&good).unwrap();
        let evil = work.join("evil");
        symlink(&outside, &evil).unwrap();

        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        // The worktree itself and a real subdir do not escape.
        assert!(symlink_escapes(&work, &[s(&work), s(&good)]).is_empty());
        // The escaping symlink is flagged; a path outside the worktree is ignored.
        assert_eq!(symlink_escapes(&work, &[s(&evil), s(&outside)]), vec![s(&evil)]);
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
