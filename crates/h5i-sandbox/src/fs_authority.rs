//! The filesystem-authority validator (design-policy.md §P2). Given the shipped
//! plan's grant lists and the measured world, it re-derives the authority the
//! plan installs and checks it is a subset of what the declared policy
//! authorized. Translation validation on the resolver's output, catching a
//! `compute_effective` bug the way translation validation catches a compiler
//! bug. A worktree path that resolves out through a planted symlink is reported
//! as a boundary signal beside the verdict.
//! Pure and cross-platform apart from [`symlink_escapes`], which measures the
//! host. Fully opt-in: see [`enforce_enabled`].

/// Whether the filesystem-authority validator runs at all. *Fully opt-in*:
/// unset means the validator never executes (no computation, no host
/// measurement, no manifest field, no gate) so default behavior is exactly as
/// before this code existed. Set `H5I_FS_AUTHORITY_ENFORCE=1` to compute the
/// verdict at box create and run, record it, and fail closed on a violation
/// (design-policy.md §P2: earn trust before it gates by default).
pub fn enforce_enabled() -> bool {
    std::env::var_os("H5I_FS_AUTHORITY_ENFORCE").is_some_and(|v| v == "1")
}

/// The per-run verdict on a shipped effective config, one boolean per claim
/// (design-policy.md §P2). Recorded in the box manifest and rendered in
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
    /// worktree, resolves out through a planted symlink on the host. `None`
    /// when the host was not measured (non-Linux, or measurement skipped).
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

/// The per-run translation validator (design-policy.md §P2): re-check the shipped effective
/// grants against the declared policy, independently of the resolver that produced them.
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
/// not second-guessed; a path *beneath* `work` whose canonical form leaves
/// `work` is the planted-symlink escape (§P3), the previous run's agent
/// redirecting a worktree path out. Callers pass the landlock grants and the
/// bind sources/mountpoints; paths outside the worktree (h5i's managed cache
/// and home-state dirs) are ignored by construction. Returns the offenders.
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
        // A write grant the policy never declared writable. A compute bug.
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
        // The escaping symlink is flagged; a path outside the worktree is
        // ignored.
        assert_eq!(symlink_escapes(&work, &[s(&evil), s(&outside)]), vec![s(&evil)]);
    }
}
