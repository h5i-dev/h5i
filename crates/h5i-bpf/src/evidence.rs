//! What the kernel lane puts in a receipt.
//!
//! These types are the *contract* between the detector and everything that
//! reads it. The receipt, the console, the export report, `h5i box detect
//! show`. They live here rather than in `h5i-core` so the rules engine can
//! produce them directly, and they are plain serde types with no eBPF in
//! them, so a build with `load` off still reads and renders a receipt written
//! by a build that had it on.
//!
//! The shape follows one rule, and it is the same rule the rest of h5i's
//! evidence follows: never let an absence render as a clean result. A run
//! the detector could not watch carries [`RuntimeEvidence::unavailable`] with
//! the reason; a run it watched incompletely carries
//! [`Coverage::Partial`] with the reason; a run that dropped events carries
//! [`RuntimeEvidence::events_lost`]. An empty `detections` list on its own
//! never means "clean". It means "clean *for what was collected*", and the
//! other fields are what say how much that was.

use serde::{Deserialize, Serialize};

/// The lane string this evidence is filed under. Deliberately distinct from
/// `host-env-run`, `tee-shim`, `shell-egress` and `runner-observed`: which
/// observer saw a thing is not something a reader should ever have to infer.
pub const LANE: &str = "kernel-bpf";

/// How much of the run the detector could actually see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Coverage {
    /// The box's processes were all inside the scope.
    Full,
    /// Some of the box's work ran where this scope cannot reach it. The
    /// container tier is the standing example: `conmon` double-forks and
    /// reparents, so the workload leaves h5i's process tree.
    Partial,
    /// Nothing about the box's own execution was observable. The microVM tier
    /// is the standing example: the workload's syscalls are made against a
    /// guest kernel that this probe is not in.
    #[default]
    None,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::None => "none",
        }
    }
}

/// How loud a detection is. Ordered, so the console can badge a record by its
/// worst finding without a second table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Worth recording, not worth reacting to. `exec.package-manager` is the
    /// archetype: nobody is alarmed that `npm` ran, and everybody wants to
    /// know it did once something else goes wrong.
    Info,
    /// Unusual for a development box, explicable.
    Notice,
    /// A boundary this product claims was pressed on.
    Alert,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Alert => "alert",
        }
    }
}

/// One rule that fired, folded across every event that matched it.
///
/// Folded rather than listed: a `postinstall` that opens `~/.ssh` four hundred
/// times is one finding with a count, not four hundred findings. The
/// exemplars are what makes the count actionable, and they are capped so a
/// flood becomes a number rather than a megabyte in the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    /// Stable rule id, e.g. `net.direct-egress`.
    pub rule: String,
    /// Family the id's prefix names: `net`, `secret`, `exec`, `priv`,
    /// `kernel`, `mount`.
    pub family: String,
    pub severity: Severity,
    /// The rule's own one-line description. Carried in the record rather than
    /// looked up at render time so an old receipt read by a new binary still
    /// says what it meant when it was written.
    pub title: String,
    pub count: u64,
    /// Nanoseconds on the kernel's monotonic clock, relative to boot. Kept raw
    /// rather than converted: the wall clock is on the record already, and
    /// converting would invent a precision the two clocks do not share.
    pub first_ns: u64,
    pub last_ns: u64,
    /// Up to [`MAX_EXAMPLES`] rendered matches, control-sanitised.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Matches beyond the exemplar cap, so a truncated list is never mistaken
    /// for a complete one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub examples_truncated: bool,
}

/// Exemplars kept per rule.
pub const MAX_EXAMPLES: usize = 5;
/// Longest exemplar kept, in bytes. A path can be a kilobyte, and a receipt is
/// something a person reads.
pub const MAX_EXAMPLE_LEN: usize = 240;

/// The block the receipt carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEvidence {
    /// Always [`LANE`]. Written out so a reader never has to know which field
    /// of which struct implied which observer.
    pub lane: String,
    /// Which scope mechanism selected the events, e.g. `pidtree`.
    pub scope: String,
    pub coverage: Coverage,
    /// Why coverage is not `full`. Present exactly when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_reason: Option<String>,
    /// Events the kernel emitted for this box and userspace decoded.
    #[serde(default)]
    pub events_seen: u64,
    /// Events the kernel dropped for want of ring-buffer space, plus any the
    /// userspace channel shed. Nonzero means the detections are a lower
    /// bound, and every renderer says so.
    #[serde(default)]
    pub events_lost: u64,
    /// Events the probe discarded in the kernel because no rule wanted them.
    /// Almost entirely read-only `openat`. Recorded because it is the number
    /// that says whether the in-kernel filter is doing its job.
    #[serde(default)]
    pub events_filtered: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detections: Vec<Detection>,
    /// Set when the detector did not run. The block is written anyway, with
    /// this filled in: a missing block would be indistinguishable from a
    /// quiet one, and that is the confusion this whole lane exists to remove.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl RuntimeEvidence {
    /// The block for a run the detector could not watch.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            lane: LANE.to_string(),
            scope: "none".to_string(),
            coverage: Coverage::None,
            coverage_reason: None,
            events_seen: 0,
            events_lost: 0,
            events_filtered: 0,
            detections: Vec::new(),
            unavailable: Some(reason.into()),
        }
    }

    /// Did the detector observe anything at all? The question a renderer has
    /// to answer before it is allowed to draw a green badge.
    pub fn observed(&self) -> bool {
        self.unavailable.is_none() && self.coverage != Coverage::None
    }

    /// The worst severity present, or `None` when nothing fired.
    pub fn worst(&self) -> Option<Severity> {
        self.detections.iter().map(|d| d.severity).max()
    }

    /// A one-line summary, for a list view.
    ///
    /// The wording is load-bearing. "no detections" is only ever said about a
    /// run that was actually watched; anything else says why it was not, so
    /// the line can never be read as a clean bill of health for a run nobody
    /// looked at.
    pub fn summary(&self) -> String {
        if let Some(why) = &self.unavailable {
            return format!("not observed ({why})");
        }
        if self.coverage == Coverage::None {
            let why = self.coverage_reason.as_deref().unwrap_or("out of scope");
            return format!("not observed ({why})");
        }
        let mut s = if self.detections.is_empty() {
            format!("no detections in {} events", self.events_seen)
        } else {
            let alerts = self
                .detections
                .iter()
                .filter(|d| d.severity == Severity::Alert)
                .count();
            let total: u64 = self.detections.iter().map(|d| d.count).sum();
            format!(
                "{} rule{} fired ({total} match{}), {alerts} alert{}",
                self.detections.len(),
                if self.detections.len() == 1 { "" } else { "s" },
                if total == 1 { "" } else { "es" },
                if alerts == 1 { "" } else { "s" },
            )
        };
        if self.coverage == Coverage::Partial {
            s.push_str(" — partial coverage");
        }
        if self.events_lost > 0 {
            s.push_str(&format!(" — {} events lost", self.events_lost));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(rule: &str, sev: Severity, count: u64) -> Detection {
        Detection {
            rule: rule.into(),
            family: rule.split('.').next().unwrap_or("").into(),
            severity: sev,
            title: "t".into(),
            count,
            first_ns: 1,
            last_ns: 2,
            examples: Vec::new(),
            examples_truncated: false,
        }
    }

    #[test]
    fn severity_orders_worst_last() {
        assert!(Severity::Alert > Severity::Notice);
        assert!(Severity::Notice > Severity::Info);
    }

    #[test]
    fn an_unavailable_block_never_reads_as_clean() {
        let e = RuntimeEvidence::unavailable("missing CAP_BPF");
        assert!(!e.observed());
        assert!(e.summary().starts_with("not observed"));
        assert!(e.summary().contains("CAP_BPF"));
        assert_eq!(e.worst(), None);
    }

    /// The failure this guards against is specific: a `coverage = none` block
    /// with an empty detection list rendering the same as a watched, quiet
    /// run.
    #[test]
    fn zero_coverage_never_reads_as_clean_either() {
        let e = RuntimeEvidence {
            lane: LANE.into(),
            scope: "pidtree".into(),
            coverage: Coverage::None,
            coverage_reason: Some("the microVM tier runs a guest kernel".into()),
            events_seen: 0,
            events_lost: 0,
            events_filtered: 0,
            detections: Vec::new(),
            unavailable: None,
        };
        assert!(!e.observed());
        assert!(e.summary().contains("guest kernel"));
    }

    #[test]
    fn a_watched_quiet_run_says_how_much_was_watched() {
        let e = RuntimeEvidence {
            lane: LANE.into(),
            scope: "pidtree".into(),
            coverage: Coverage::Full,
            coverage_reason: None,
            events_seen: 412,
            events_lost: 0,
            events_filtered: 90_000,
            detections: Vec::new(),
            unavailable: None,
        };
        assert!(e.observed());
        assert_eq!(e.summary(), "no detections in 412 events");
    }

    #[test]
    fn loss_and_partial_coverage_are_both_in_the_line() {
        let e = RuntimeEvidence {
            lane: LANE.into(),
            scope: "pidtree".into(),
            coverage: Coverage::Partial,
            coverage_reason: Some("container".into()),
            events_seen: 10,
            events_lost: 3,
            events_filtered: 0,
            detections: vec![det("net.direct-egress", Severity::Alert, 2)],
            unavailable: None,
        };
        let s = e.summary();
        assert!(s.contains("1 rule fired"), "{s}");
        assert!(s.contains("partial coverage"), "{s}");
        assert!(s.contains("3 events lost"), "{s}");
        assert_eq!(e.worst(), Some(Severity::Alert));
    }

    #[test]
    fn the_block_round_trips_through_json() {
        let e = RuntimeEvidence {
            lane: LANE.into(),
            scope: "pidtree".into(),
            coverage: Coverage::Full,
            coverage_reason: None,
            events_seen: 5,
            events_lost: 0,
            events_filtered: 1,
            detections: vec![det("kernel.bpf", Severity::Alert, 1)],
            unavailable: None,
        };
        let j = serde_json::to_string(&e).unwrap();
        let back: RuntimeEvidence = serde_json::from_str(&j).unwrap();
        assert_eq!(e, back);
    }

    /// Old records predate every count field. They must still read, or
    /// upgrading h5i would make yesterday's evidence unreadable.
    #[test]
    fn a_minimal_block_deserializes() {
        let j = r#"{"lane":"kernel-bpf","scope":"pidtree","coverage":"full"}"#;
        let e: RuntimeEvidence = serde_json::from_str(j).unwrap();
        assert_eq!(e.events_seen, 0);
        assert!(e.detections.is_empty());
        assert!(e.observed());
    }
}
