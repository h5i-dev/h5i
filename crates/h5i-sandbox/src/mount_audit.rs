//! The mount-realization audit (design-policy.md §P3). `validate_effective`
//! checks the *plan*; this checks what the kernel actually realized. After
//! setup, the
//! supervisor reads the child's `/proc/<pid>/mountinfo` and diffs the realized
//! mounts against the plan (the `EffectiveConfig` binds): a bind that did not
//! land where planned, or a read-only overlay realized read-write, is the shape
//! of the runc 2025 mount-swap / masked-path CVEs. Detected here and failed
//! closed, rather than trusted.
//!
//! Honest bounds (§P3): `mountinfo` exposes mount topology and flags, not the
//! installed Landlock ruleset or seccomp filter, and a symlink race that leaves
//! topology unchanged is not visible here. Those are prevented by construction
//! (§P4), and this audit is the net under that discipline, not a substitute.
//!
//! The parse and diff are pure (they take the `mountinfo` text), so they are
//! unit-tested against synthetic input; only reading the file is Linux-only.

/// A mount the plan intends: a mount point and whether it must be read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedMount {
    /// The mount point (target) as it appears in `mountinfo` field 5.
    pub target: String,
    /// The plan requires this mount to be read-only (a config-lock or cache-ro
    /// overlay).
    pub read_only: bool,
}

/// A discrepancy between the plan and what the kernel realized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountMismatch {
    /// The plan expected a mount at `target`, but `mountinfo` has none there.
    Missing { target: String },
    /// The plan required `target` read-only, but it was realized read-write.
    /// The dangerous case (a failed remount or a swapped source).
    WritableButExpectedRo { target: String },
}

impl MountMismatch {
    pub fn target(&self) -> &str {
        match self {
            MountMismatch::Missing { target } | MountMismatch::WritableButExpectedRo { target } => {
                target
            }
        }
    }
}

/// One realized mount, parsed from a `mountinfo` line: its mount point and
/// whether its per-mount options carry `ro`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Realized {
    mount_point: String,
    read_only: bool,
}

/// Parse a `/proc/<pid>/mountinfo` line into (mount point, read-only). Format
/// (man 5 proc): `id parent major:minor root mount-point options …`. The mount
/// point is field 5 (index 4); the per-mount options are field 6 (index 5), a
/// comma list whose first element is `ro` or `rw`. Octal escapes in the mount
/// point (`\040` etc.) are left as-is: the plan's targets are h5i-created and
/// escape-free, and a mount point that needed escaping would simply not match a
/// plan target. The fail-closed direction.
fn parse_line(line: &str) -> Option<Realized> {
    let mut it = line.split_whitespace();
    let mount_point = it.nth(4)?.to_string();
    let options = it.next()?;
    let read_only = options.split(',').any(|o| o == "ro");
    Some(Realized { mount_point, read_only })
}

/// Parse the whole `mountinfo` text into realized mounts, skipping malformed
/// lines (a line the kernel would not have written; a real one always parses).
fn parse_mountinfo(text: &str) -> Vec<Realized> {
    text.lines().filter_map(parse_line).collect()
}

/// *The audit*: diff the plan against realized `mountinfo`. Returns every
/// mismatch. A planned mount missing, or a planned read-only mount realized
/// writable. An empty result means the realized mount topology is consistent
/// with the plan for the audited targets.
///
/// The last realized mount at a target wins (later mounts stack on top), so a
/// target's effective read-only-ness is that of its topmost mount.
pub fn audit_mounts(expected: &[ExpectedMount], mountinfo: &str) -> Vec<MountMismatch> {
    let realized = parse_mountinfo(mountinfo);
    let mut mismatches = Vec::new();
    for e in expected {
        // The topmost mount at this target, if any.
        match realized.iter().rev().find(|r| r.mount_point == e.target) {
            None => mismatches.push(MountMismatch::Missing { target: e.target.clone() }),
            Some(top) => {
                if e.read_only && !top.read_only {
                    mismatches
                        .push(MountMismatch::WritableButExpectedRo { target: e.target.clone() });
                }
            }
        }
    }
    mismatches
}

/// The mounts an effective config plans, as audit expectations: each bind's
/// target, read-only iff the bind is not writable. This is the plan side of the
/// audit; the caller pairs it with a child's realized `mountinfo`.
#[cfg(target_os = "linux")]
pub fn expected_mounts(cfg: &crate::effective::EffectiveConfig) -> Vec<ExpectedMount> {
    cfg.binds
        .iter()
        .map(|b| ExpectedMount { target: b.target.clone(), read_only: !b.writable })
        .collect()
}

/// Read `/proc/<pid>/mountinfo` for a stopped child and audit it against the
/// plan. Linux only; the caller runs this at the exec barrier (§P3), after
/// the child has finished setup and before it execs.
#[cfg(target_os = "linux")]
pub fn audit_pid(pid: u32, expected: &[ExpectedMount]) -> std::io::Result<Vec<MountMismatch>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/mountinfo"))?;
    Ok(audit_mounts(expected, &text))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic mountinfo excerpt: a rw worktree bind and a ro config-lock.
    const SAMPLE: &str = "\
22 21 0:20 / /work rw,relatime shared:1 - ext4 /dev/sda1 rw
23 22 0:20 /cfg /work/.claude/settings.json ro,relatime - ext4 /dev/sda1 ro
24 21 0:21 / /root/.npm ro,relatime - ext4 /dev/sda1 ro";

    fn exp(target: &str, ro: bool) -> ExpectedMount {
        ExpectedMount { target: target.into(), read_only: ro }
    }

    #[test]
    fn consistent_plan_has_no_mismatch() {
        let plan = vec![
            exp("/work", false),
            exp("/work/.claude/settings.json", true),
            exp("/root/.npm", true),
        ];
        assert!(audit_mounts(&plan, SAMPLE).is_empty());
    }

    #[test]
    fn ro_overlay_realized_writable_is_caught() {
        // The config-lock is expected ro but realized rw (a failed remount).
        let swapped = SAMPLE.replace(
            "/work/.claude/settings.json ro,relatime",
            "/work/.claude/settings.json rw,relatime",
        );
        let plan = vec![exp("/work/.claude/settings.json", true)];
        assert_eq!(
            audit_mounts(&plan, &swapped),
            vec![MountMismatch::WritableButExpectedRo {
                target: "/work/.claude/settings.json".into()
            }]
        );
    }

    #[test]
    fn missing_planned_mount_is_caught() {
        let plan = vec![exp("/cache/pip", true)];
        assert_eq!(
            audit_mounts(&plan, SAMPLE),
            vec![MountMismatch::Missing { target: "/cache/pip".into() }]
        );
    }

    #[test]
    fn topmost_mount_wins() {
        // A rw mount stacked on top of an earlier ro one at the same target:
        // the top is writable, so a ro expectation is violated.
        let stacked = "\
10 1 0:1 / /work/x ro,relatime - ext4 /dev/sda1 ro
11 1 0:1 / /work/x rw,relatime - ext4 /dev/sda1 rw";
        let plan = vec![exp("/work/x", true)];
        assert_eq!(
            audit_mounts(&plan, stacked),
            vec![MountMismatch::WritableButExpectedRo { target: "/work/x".into() }]
        );
    }

    #[test]
    fn rw_plan_ignores_realized_ro() {
        // A writable plan is not violated by a stricter realized ro (narrower
        // is the fail-closed direction; only widening is a fault).
        let plan = vec![exp("/root/.npm", false)];
        assert!(audit_mounts(&plan, SAMPLE).is_empty());
    }
}
