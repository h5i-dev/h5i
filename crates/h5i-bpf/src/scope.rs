//! Which of the host's events belong to this box.
//!
//! The hard half of a per-run detector. Too permissive and the user's own editor
//! is reported as box activity; too restrictive and the interesting child, the
//! `postinstall` that lived forty milliseconds, is the one missed.
//!
//! Cgroup-id and pid-namespace filters were considered and fail the same test:
//! **the scope has to be decided before the payload exists.** A scope programmed
//! after the child is spawned has already missed the exec that named it, the
//! most valuable event of the run. A cgroup id is knowable in advance only if
//! h5i creates the cgroup in advance, which it does not, and a pid-namespace
//! inode only from a process that does not exist yet.
//!
//! What *is* knowable in advance is h5i's own process tree, and the kernel can
//! maintain the descendant set from there. That is the Tetragon idea
//! (ROADMAP.md D3): lineage kept in the kernel rather than reconstructed by
//! racing `/proc`, where the short-lived child is already gone by the time
//! userspace reads it.
//!
//! The probe's state machine (`bpf/h5i_event.h`) closes the two holes seeding
//! from h5i's own tree would leave: h5i's *threads* are told from its *children*
//! by whether the new task leads its own thread group, and h5i's `pre_exec` work
//! is held back until the payload's `execve`, so the confinement machinery is
//! never reported as the box's behaviour.

use serde::{Deserialize, Serialize};

pub use crate::evidence::Coverage;

/// The isolation tier a run is using, as far as coverage is concerned.
///
/// Mirrors `h5i_sandbox::IsolationClaim` without depending on it: this crate
/// sits beside the sandbox rather than above it, and a string would let the
/// two drift silently. [`Tier::parse`] is the seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Workspace,
    Process,
    Supervised,
    Container,
    Microvm,
    /// A tier this build does not know. Treated as uncovered, because
    /// guessing in the permissive direction is the one mistake that produces
    /// a false clean bill of health.
    Unknown,
}

impl Tier {
    pub fn parse(s: &str) -> Self {
        match s {
            "workspace" => Self::Workspace,
            "process" => Self::Process,
            "supervised" => Self::Supervised,
            "container" => Self::Container,
            "microvm" => Self::Microvm,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Process => "process",
            Self::Supervised => "supervised",
            Self::Container => "container",
            Self::Microvm => "microvm",
            Self::Unknown => "unknown",
        }
    }

    /// How much of a run on this tier the pid-tree scope can see, and why not
    /// more. The reason travels into the receipt: `partial` with no
    /// explanation is barely better than a missing block.
    pub fn coverage(self) -> (Coverage, Option<&'static str>) {
        match self {
            // The payload is a direct descendant of h5i, so the tree holds it
            // and everything it spawns.
            Self::Workspace | Self::Process | Self::Supervised => (Coverage::Full, None),
            // Podman's `conmon` double-forks and reparents, so the workload
            // leaves h5i's process tree. What remains visible is the
            // runtime's own activity on the host, which is worth having and
            // is not the box.
            Self::Container => (
                Coverage::Partial,
                Some(
                    "the container runtime double-forks, so the workload leaves h5i's process \
                     tree; what this lane saw is the runtime's own activity on the host, not \
                     the box's",
                ),
            ),
            // A host probe cannot see guest syscalls at all. Reporting
            // anything else here would be the worst available failure.
            Self::Microvm => (
                Coverage::None,
                Some(
                    "the workload runs against a guest kernel, which a host probe cannot \
                     observe at all",
                ),
            ),
            Self::Unknown => (
                Coverage::None,
                Some("this build does not know how the box's processes relate to h5i's"),
            ),
        }
    }
}

/// The one scope mechanism this build implements. Named in the receipt so a
/// later privileged collector — which attaches out of band and can therefore
/// resolve a cgroup or a namespace — can add mechanisms without any reader
/// having to guess which one produced an old record.
pub const SCOPE_PIDTREE: &str = "pidtree";

/// Every task of the current process, which is what the pid tree is seeded
/// with.
///
/// All of them, not just the main thread: `std::process::Command` can be
/// called from any thread, and a tree seeded with only the main thread would
/// miss a payload spawned from a worker. Threads created *after* seeding are
/// picked up by the fork tracepoint and classified by the probe's state
/// machine, so this only has to cover the ones that already exist.
#[cfg(target_os = "linux")]
pub fn self_tids() -> Vec<u32> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for entry in dir.flatten() {
            if let Some(tid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
                out.push(tid);
            }
        }
    }
    if out.is_empty() {
        // A /proc that could not be read is not a reason to watch nothing:
        // the main thread is the one the payload is most likely spawned from.
        out.push(std::process::id());
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_round_trip() {
        for t in [
            Tier::Workspace,
            Tier::Process,
            Tier::Supervised,
            Tier::Container,
            Tier::Microvm,
        ] {
            assert_eq!(Tier::parse(t.as_str()), t);
        }
    }

    /// An unknown tier must be uncovered. Guessing "probably full" is the one
    /// error that turns an absence of evidence into a clean bill of health.
    #[test]
    fn an_unknown_tier_is_uncovered_not_assumed_full() {
        assert_eq!(Tier::parse("something-new"), Tier::Unknown);
        let (cov, why) = Tier::Unknown.coverage();
        assert_eq!(cov, Coverage::None);
        assert!(why.is_some());
    }

    #[test]
    fn the_kernel_tiers_are_fully_covered_and_the_guest_tier_is_not() {
        for t in [Tier::Workspace, Tier::Process, Tier::Supervised] {
            assert_eq!(t.coverage().0, Coverage::Full, "{}", t.as_str());
            assert!(t.coverage().1.is_none());
        }
        assert_eq!(Tier::Container.coverage().0, Coverage::Partial);
        assert_eq!(Tier::Microvm.coverage().0, Coverage::None);
    }

    /// Anything less than full coverage has to say why, or the receipt reads
    /// as "we looked and found nothing" when it means "we could not look".
    #[test]
    fn every_incomplete_coverage_carries_a_reason() {
        for t in [Tier::Container, Tier::Microvm, Tier::Unknown] {
            let (cov, why) = t.coverage();
            assert_ne!(cov, Coverage::Full);
            assert!(why.is_some(), "{} has no reason", t.as_str());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_seed_includes_this_thread() {
        let tids = self_tids();
        assert!(!tids.is_empty());
        // The process id is the main thread's tid, and it is always a task.
        assert!(tids.contains(&std::process::id()));
    }
}
