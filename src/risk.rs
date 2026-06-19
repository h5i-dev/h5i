//! Deterministic sandbox **boundary-pressure** classifier.
//!
//! This module answers one question for the `h5i serve` Sandbox dashboard: *did
//! an environment's activity press against — or trip — the sandbox boundary?*
//! It is **deterministic and explainable** (no LLM, no heuristics that can't be
//! pointed at a matched string) and it is **honest about evidence**:
//!
//!   - When enforcement actually *fired* — a mediated-commit `violation` event,
//!     or an allowlist-proxy `403` (egress denied) — we say **"Boundary
//!     blocked"** (red). These are facts: the kernel/proxy refused something.
//!   - When a command merely *looks* like probing — `unshare`, a read of
//!     `/etc/shadow`, a `curl` to a raw IP — but nothing was observed to be
//!     denied, we say **"Boundary pressure"** (amber). A shape, not a verdict.
//!   - When an env runs risky-looking work under a tier that *cannot* contain it
//!     (e.g. `workspace` isolation touching `~/.ssh`), we say **"Weak
//!     isolation"** (grey) — a capability gap, never an accusation.
//!
//! We never label an agent "malicious" or claim "attempted escape" unless a
//! concrete enforcement event is attached. The score is a transparent sum of
//! per-finding weights (see [`Finding::weight`]) capped at 100.
//!
//! Inputs are exactly what h5i already records: env events (`refs/h5i/env`),
//! capture manifests (`refs/h5i/objects`, incl. the [`objects::EgressSummary`]
//! the container proxy now populates), and the resolved policy. The scanning
//! core is string-pure so it unit-tests without a repository.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::env::EnvEvent;
use crate::objects::{self, EgressSummary};
use crate::sandbox::Profile;

/// The five inspectable dimensions of the sandbox boundary — the dashboard's
/// per-run lanes. Every [`Finding`] belongs to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    /// Filesystem reach: sensitive paths, escapes, denied reads/writes.
    Fs,
    /// Network egress: proxy denials, raw-IP shapes, exfil tooling.
    Net,
    /// Process/privilege: namespace, ptrace, mount, capability tooling.
    Proc,
    /// Resource limits: wall-clock kills, cpu/mem/fsize caps.
    Resource,
    /// Provenance/integrity: mediated-commit boundary trips, policy mismatch.
    Provenance,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Fs => "fs",
            Lane::Net => "net",
            Lane::Proc => "proc",
            Lane::Resource => "resource",
            Lane::Provenance => "provenance",
        }
    }
}

/// Finding severity, also used as the overall env risk level. `Info` covers both
/// a clean env (score 0) and low-signal observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// One classified signal, always anchored to a matched string (`evidence`) and,
/// where possible, to the capture/event it came from so the UI can deep-link.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub lane: Lane,
    /// Stable machine id (e.g. `"privilege-tool"`) — the UI groups/filters on it.
    pub kind: String,
    /// Short human label following the copy rules ("Boundary blocked", …).
    pub title: String,
    /// What was matched and why it matters.
    pub detail: String,
    /// The exact matched token/snippet (already secret-redacted upstream).
    pub evidence: String,
    /// Capture this came from, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    /// Event timestamp this came from, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_ts: Option<String>,
}

impl Finding {
    /// Score weight for this finding's `kind`. The rubric is fixed and additive
    /// (capped at 100 by [`EnvRisk`]) so a score is always explainable by the
    /// findings beneath it.
    pub fn weight(&self) -> u32 {
        match self.kind.as_str() {
            // Enforcement actually fired — highest confidence.
            "commit-violation" => 40,
            "egress-denied" => 40,
            // A denied-syscall *shape* in the output (EPERM near a sensitive op).
            "denied-syscall-evidence" => 18,
            // Probing shapes (no observed denial).
            "sensitive-target" => 25,
            "privilege-tool" => 20,
            "raw-ip-egress" => 15,
            // Resource pressure.
            "wall-timeout" => 10,
            // Low-signal context.
            "exfil-tool" => 8,
            "weak-isolation" => 5,
            // A cross-run escalation bonus, added synthetically.
            "repeated-probe" => 10,
            _ => 0,
        }
    }
}

/// Per-env risk roll-up: an explainable score, the contributing findings, and
/// lane tallies for the dashboard's escalation view.
#[derive(Debug, Clone, Serialize)]
pub struct EnvRisk {
    /// 0..=100. A transparent, capped sum of [`Finding::weight`].
    pub score: u32,
    /// Overall level (drives the badge colour).
    pub level: Severity,
    /// Findings, most severe first.
    pub findings: Vec<Finding>,
    /// Count of findings per lane (a 3+-lane spread is real escalation).
    pub lane_counts: BTreeMap<String, u32>,
    /// Timestamp of the most recent *enforcement* denial, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_denial_ts: Option<String>,
}

// ─── pattern tables (deterministic; each entry is a substring match) ──────────

/// Privilege / isolation tooling — programs whose presence in a command is a
/// boundary-probe shape even when the run ultimately fails normally.
const PRIVILEGE_TOOLS: &[&str] = &[
    "unshare", "nsenter", "setns", "chroot", "pivot_root", "mount ", "umount",
    "ptrace", "strace", "gdb ", "bpftrace", "bpftool", "perf_event_open",
    "iptables", "nft ", "nftables", "capsh", "setcap", "modprobe", "insmod",
    "sudo ", " su ", "su -", "keyctl", "io_uring",
];

/// Sensitive filesystem / credential targets.
const SENSITIVE_TARGETS: &[&str] = &[
    "docker.sock", "/var/run/docker.sock", "/proc/self/root", "/proc/1/",
    "/proc/sys/", "/sys/kernel", "/dev/mem", "/dev/kmem", "/etc/shadow",
    "/etc/sudoers", "/.ssh", "/.ssh/", ".ssh/id_", "id_rsa", "id_ed25519",
    "/.aws", ".aws/credentials", ".git/hooks", "/root/", "authorized_keys",
];

/// Common egress tools — interesting only with a target (raw IP, etc.).
const EXFIL_TOOLS: &[&str] = &["curl ", "wget ", " nc ", "ncat ", "socat ", "openssl s_client"];

/// Output substrings that indicate a kernel/landlock/seccomp denial.
const DENIAL_MARKERS: &[&str] = &[
    "operation not permitted", "permission denied", "eperm", "eacces",
    "landlock", "seccomp", "bad system call", "sigsys", "cap_sys_admin",
];

// ─── string-pure scanners (unit-testable without a repo) ──────────────────────

fn mk(
    severity: Severity,
    lane: Lane,
    kind: &str,
    title: &str,
    detail: String,
    evidence: String,
) -> Finding {
    Finding {
        severity,
        lane,
        kind: kind.into(),
        title: title.into(),
        detail,
        evidence,
        capture_id: None,
        event_ts: None,
    }
}

/// Scan a command line for boundary-pressure shapes. Pure; no denial verdict is
/// implied (these are amber "pressure" unless a denial is observed elsewhere).
pub fn scan_command(cmd: &str) -> Vec<Finding> {
    let lc = cmd.to_ascii_lowercase();
    let mut out = Vec::new();

    for tool in PRIVILEGE_TOOLS {
        if lc.contains(tool) {
            out.push(mk(
                Severity::Warning,
                Lane::Proc,
                "privilege-tool",
                "Boundary pressure",
                format!("command invokes privilege/isolation tooling (`{}`)", tool.trim()),
                tool.trim().to_string(),
            ));
        }
    }
    for target in SENSITIVE_TARGETS {
        if lc.contains(target) {
            out.push(mk(
                Severity::Warning,
                Lane::Fs,
                "sensitive-target",
                "Boundary pressure",
                format!("command references a sensitive target (`{target}`)"),
                target.to_string(),
            ));
        }
    }
    // Network exfil tools, escalated to "raw-ip" when pointed at an IP literal.
    for tool in EXFIL_TOOLS {
        if lc.contains(tool) {
            if let Some(ip) = first_raw_ip(cmd) {
                out.push(mk(
                    Severity::Warning,
                    Lane::Net,
                    "raw-ip-egress",
                    "Boundary pressure",
                    format!("network tool `{}` targets a raw IP ({ip}) — proxy-bypass shape", tool.trim()),
                    ip,
                ));
            } else {
                out.push(mk(
                    Severity::Info,
                    Lane::Net,
                    "exfil-tool",
                    "Boundary pressure",
                    format!("command uses a network egress tool (`{}`)", tool.trim()),
                    tool.trim().to_string(),
                ));
            }
        }
    }
    out
}

/// Scan captured output text for denial markers (a denied-syscall *shape*).
pub fn scan_output(summary: &str) -> Vec<Finding> {
    let lc = summary.to_ascii_lowercase();
    let mut out = Vec::new();
    for marker in DENIAL_MARKERS {
        if lc.contains(marker) {
            out.push(mk(
                Severity::Warning,
                Lane::Proc,
                "denied-syscall-evidence",
                "Boundary pressure",
                format!("output contains a denial marker (`{marker}`) — possible kernel/landlock/seccomp refusal"),
                marker.to_string(),
            ));
            break; // one marker per capture is enough signal; avoid spam
        }
    }
    out
}

/// Turn an [`EgressSummary`] into findings: each denied host is a **blocked**
/// boundary (the proxy returned 403 — enforcement fired), so these are red.
pub fn scan_egress(eg: &EgressSummary) -> Vec<Finding> {
    let mut out = Vec::new();
    for h in &eg.hosts {
        if h.denied > 0 {
            out.push(mk(
                Severity::Critical,
                Lane::Net,
                "egress-denied",
                "Boundary blocked",
                format!(
                    "egress proxy refused {} request(s) to {}:{} (off-allowlist)",
                    h.denied, h.host, h.port
                ),
                format!("{}:{}", h.host, h.port),
            ));
        }
    }
    out
}

/// Extract the first raw IPv4 literal from `s` (host of a `curl http://1.2.3.4`
/// shape). Conservative: four dotted 0-255 octets, not part of a longer number.
fn first_raw_ip(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !is_ip_char(bytes[i - 1])) {
            if let Some((ip, next)) = parse_ipv4_at(s, i) {
                // Reject the loopback the proxy itself uses.
                if ip != "127.0.0.1" {
                    return Some(ip);
                }
                i = next;
                continue;
            }
        }
        i += 1;
    }
    None
}

fn is_ip_char(b: u8) -> bool {
    b.is_ascii_digit() || b == b'.'
}

fn parse_ipv4_at(s: &str, start: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let mut i = start;
    let mut octets = 0;
    while octets < 4 {
        let oct_start = i;
        let mut val: u32 = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            val = val * 10 + (bytes[i] - b'0') as u32;
            i += 1;
        }
        if i == oct_start || val > 255 {
            return None;
        }
        octets += 1;
        if octets < 4 {
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
            } else {
                return None;
            }
        }
    }
    // Must not be immediately followed by another digit/dot (would be a longer token).
    if i < bytes.len() && is_ip_char(bytes[i]) {
        return None;
    }
    Some((s[start..i].to_string(), i))
}

// ─── orchestration ────────────────────────────────────────────────────────────

/// Classify one environment from its events and resolved capture manifests.
///
/// `policy` is the *enforced* policy (resolved profile), used to flag
/// weak-isolation mismatches; pass `None` if unavailable. `captures` should be
/// the resolved manifests for `manifest.captures` (the server resolves these);
/// any subset is fine — missing captures simply contribute no findings.
pub fn classify_env(
    manifest: &crate::env::EnvManifest,
    policy: Option<&Profile>,
    events: &[EnvEvent],
    captures: &[objects::Manifest],
) -> EnvRisk {
    let mut findings: Vec<Finding> = Vec::new();
    let mut last_denial_ts: Option<String> = None;

    // 1. Violation events — mediated-commit boundary trips (enforcement fired).
    for ev in events {
        if ev.event == "violation" {
            let f = Finding {
                severity: Severity::Critical,
                lane: Lane::Provenance,
                kind: "commit-violation".into(),
                title: "Boundary blocked".into(),
                detail: ev
                    .detail
                    .clone()
                    .unwrap_or_else(|| "mediated commit refused (fail-closed)".into()),
                evidence: "mediated-commit refusal".into(),
                capture_id: None,
                event_ts: Some(ev.ts.clone()),
            };
            bump_denial(&mut last_denial_ts, &ev.ts);
            findings.push(f);
        }
    }

    // 2. Per-capture scanning: command shape, output denial markers, egress.
    // Weakness is computed *per lane*: at the process/container tier Landlock
    // and seccomp still enforce FS and PROC even when networking is wide open,
    // so only the genuinely-uncontained lanes get the grey "weak" relabel.
    let weak = weak_lanes(manifest, policy);
    for cap in captures {
        let ts = Some(cap.timestamp.clone());
        if let Some(cmd) = &cap.cmd {
            for mut f in scan_command(cmd) {
                f.capture_id = Some(cap.id.clone());
                f.event_ts = ts.clone();
                // In a lane this tier can't actually confine, relabel as a
                // capability gap (grey) rather than implying confinement pressure.
                if weak.contains(&f.lane) {
                    annotate_weak(&mut f);
                }
                findings.push(f);
            }
        }
        for mut f in scan_output(&cap.summary) {
            f.capture_id = Some(cap.id.clone());
            f.event_ts = ts.clone();
            findings.push(f);
        }
        if let Some(eg) = &cap.egress {
            for mut f in scan_egress(eg) {
                f.capture_id = Some(cap.id.clone());
                f.event_ts = ts.clone();
                bump_denial(&mut last_denial_ts, &cap.timestamp);
                findings.push(f);
            }
        }
    }

    // 3. Resource pressure from exec events (wall-clock kills).
    for ev in events {
        if ev.event == "exec" {
            if let Some(d) = &ev.detail {
                if d.contains("timed-out") {
                    findings.push(Finding {
                        severity: Severity::Warning,
                        lane: Lane::Resource,
                        kind: "wall-timeout".into(),
                        title: "Resource limit hit".into(),
                        detail: "run was killed by the wall-clock limit (exit 124)".into(),
                        evidence: "timed-out".into(),
                        capture_id: ev.capture.clone(),
                        event_ts: Some(ev.ts.clone()),
                    });
                }
            }
        }
    }

    // 4. Cross-run escalation: a suspicious kind recurring across >=2 captures.
    add_repeated_probe_bonus(&mut findings);

    finalize(findings, last_denial_ts)
}

/// The lanes a policy fails to actually confine — findings in these get the
/// grey "weak isolation" relabel instead of amber "boundary pressure", because
/// the tier provides no enforcement there (a capability gap, not pressure).
///
/// RESOURCE and PROVENANCE are never weak: a wall-clock kill or a
/// mediated-commit refusal is an enforcement *fact*, independent of tier.
///
/// - `workspace` (no kernel confinement at all) → FS, NET, PROC all weak.
/// - `process`/`container` with `host` networking and no egress allowlist →
///   only NET is uncontained; Landlock/seccomp still enforce FS and PROC.
/// - otherwise (process/container with net deny or an egress allowlist) →
///   nothing weak; findings stand as honest pressure.
///
/// When `policy` is absent (a pulled env) we fall back to the manifest's
/// isolation claim and, lacking the resolved net settings, do not assume a
/// network gap — conservative: show pressure rather than silently greying it.
fn weak_lanes(
    manifest: &crate::env::EnvManifest,
    policy: Option<&Profile>,
) -> std::collections::HashSet<Lane> {
    use crate::sandbox::{IsolationClaim, NetMode};
    let mut weak = std::collections::HashSet::new();

    let is_workspace = manifest.isolation_claim == "workspace"
        || policy.map(|p| matches!(p.isolation, IsolationClaim::Workspace)).unwrap_or(false);
    if is_workspace {
        weak.insert(Lane::Fs);
        weak.insert(Lane::Net);
        weak.insert(Lane::Proc);
        return weak;
    }
    // Stronger tier: only an unfiltered network is uncontained.
    if let Some(p) = policy {
        if p.net_mode == NetMode::Host && p.net_egress.is_empty() {
            weak.insert(Lane::Net);
        }
    }
    weak
}

/// Relabel a pressure finding as a weak-isolation capability gap (grey copy).
fn annotate_weak(f: &mut Finding) {
    f.kind = "weak-isolation".into();
    f.title = "Weak isolation".into();
    f.severity = Severity::Info;
    f.detail = format!("{} — under an isolation tier that cannot fully contain it", f.detail);
}

fn bump_denial(slot: &mut Option<String>, ts: &str) {
    if slot.as_deref().map(|cur| ts > cur).unwrap_or(true) {
        *slot = Some(ts.to_string());
    }
}

/// Add a one-off `repeated-probe` bonus finding when the same suspicious kind
/// shows up in two or more distinct captures — escalation the single-event
/// weights miss.
fn add_repeated_probe_bonus(findings: &mut Vec<Finding>) {
    use std::collections::HashMap;
    let mut caps_by_kind: HashMap<&str, std::collections::HashSet<String>> = HashMap::new();
    for f in findings.iter() {
        if matches!(f.kind.as_str(), "privilege-tool" | "sensitive-target" | "raw-ip-egress") {
            if let Some(cap) = &f.capture_id {
                caps_by_kind.entry(kind_static(&f.kind)).or_default().insert(cap.clone());
            }
        }
    }
    let mut bonuses = Vec::new();
    for (kind, caps) in caps_by_kind {
        if caps.len() >= 2 {
            bonuses.push(mk(
                Severity::Warning,
                Lane::Proc,
                "repeated-probe",
                "Boundary pressure",
                format!("`{kind}` shape recurred across {} runs — sustained probing", caps.len()),
                kind.to_string(),
            ));
        }
    }
    findings.append(&mut bonuses);
}

/// Map a kind string to a `'static` slice for use as a map key / evidence.
fn kind_static(kind: &str) -> &'static str {
    match kind {
        "privilege-tool" => "privilege-tool",
        "sensitive-target" => "sensitive-target",
        "raw-ip-egress" => "raw-ip-egress",
        _ => "probe",
    }
}

/// Sort findings, compute the capped score and lane tallies, and derive the
/// overall level.
fn finalize(mut findings: Vec<Finding>, last_denial_ts: Option<String>) -> EnvRisk {
    // Most severe first, then by weight.
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.weight().cmp(&a.weight()))
            .then(a.lane.cmp(&b.lane))
    });

    let raw_score: u32 = findings.iter().map(|f| f.weight()).sum();
    let score = raw_score.min(100);

    let mut lane_counts: BTreeMap<String, u32> = BTreeMap::new();
    for f in &findings {
        *lane_counts.entry(f.lane.as_str().to_string()).or_insert(0) += 1;
    }
    let distinct_lanes = lane_counts.len();
    let has_critical = findings.iter().any(|f| f.severity == Severity::Critical);

    // Level: any enforcement denial OR score>=50 OR a 3+-lane spread → Critical;
    // some pressure → Warning; otherwise Info (incl. a clean env, score 0).
    let level = if has_critical || score >= 50 || distinct_lanes >= 3 {
        Severity::Critical
    } else if score >= 20 || distinct_lanes >= 2 {
        Severity::Warning
    } else {
        Severity::Info
    };

    EnvRisk { score, level, findings, lane_counts, last_denial_ts }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_privilege_tooling() {
        let f = scan_command("unshare --mount --net /bin/sh");
        assert!(f.iter().any(|x| x.kind == "privilege-tool" && x.lane == Lane::Proc));
    }

    #[test]
    fn detects_sensitive_targets() {
        let f = scan_command("cat /etc/shadow && ls ~/.ssh");
        let kinds: Vec<_> = f.iter().map(|x| x.kind.as_str()).collect();
        assert!(kinds.contains(&"sensitive-target"));
        // both /etc/shadow and a .ssh reference matched
        assert!(f.iter().filter(|x| x.kind == "sensitive-target").count() >= 2);
    }

    #[test]
    fn detects_docker_sock() {
        let f = scan_command("curl --unix-socket /var/run/docker.sock http://x/containers/json");
        assert!(f.iter().any(|x| x.kind == "sensitive-target" && x.evidence.contains("docker.sock")));
    }

    #[test]
    fn raw_ip_egress_is_flagged_over_plain_exfil() {
        let f = scan_command("curl http://203.0.113.5/payload");
        assert!(f.iter().any(|x| x.kind == "raw-ip-egress" && x.evidence == "203.0.113.5"));
        // A domain target is only the low-signal exfil-tool finding.
        let g = scan_command("curl https://pypi.org/simple/");
        assert!(g.iter().any(|x| x.kind == "exfil-tool"));
        assert!(!g.iter().any(|x| x.kind == "raw-ip-egress"));
    }

    #[test]
    fn first_raw_ip_parsing() {
        assert_eq!(first_raw_ip("curl http://1.2.3.4/x"), Some("1.2.3.4".into()));
        assert_eq!(first_raw_ip("ping 255.255.255.255"), Some("255.255.255.255".into()));
        assert_eq!(first_raw_ip("v1.2.3.4.5 is a version"), None); // 5 octets → not an IPv4
        assert_eq!(first_raw_ip("256.1.1.1"), None); // out of range
        assert_eq!(first_raw_ip("no ip here"), None);
        assert_eq!(first_raw_ip("loopback 127.0.0.1 only"), None); // the proxy's own
    }

    #[test]
    fn output_denial_markers() {
        let f = scan_output("open(\"/etc/shadow\"): Operation not permitted");
        assert!(f.iter().any(|x| x.kind == "denied-syscall-evidence"));
        assert!(scan_output("all tests passed").is_empty());
    }

    #[test]
    fn egress_denied_is_critical_blocked() {
        let eg = EgressSummary {
            allowed: 1,
            denied: 2,
            hosts: vec![
                objects::EgressHost { host: "pypi.org".into(), port: 443, allowed: 1, denied: 0 },
                objects::EgressHost { host: "evil.example".into(), port: 443, allowed: 0, denied: 2 },
            ],
            hosts_truncated: false,
            log: None,
        };
        let f = scan_egress(&eg);
        assert_eq!(f.len(), 1, "only the denied host produces a finding");
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].title, "Boundary blocked");
        assert!(f[0].evidence.contains("evil.example"));
    }

    fn manifest(isolation: &str) -> crate::env::EnvManifest {
        crate::env::EnvManifest {
            id: "env/claude/x".into(),
            agent: "claude".into(),
            slug: "x".into(),
            base_commit: "0".into(),
            base_tree: "0".into(),
            parent_branch: "main".into(),
            branch: "refs/heads/h5i/env/claude/x".into(),
            parent_context_branch: "main".into(),
            context_branch: "env/claude/x".into(),
            profile: "default".into(),
            policy_digest: "d".into(),
            isolation_claim: isolation.into(),
            backend: "worktree".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            status: "idle".into(),
            captures: vec!["cap0".into()],
            service_digest: None,
        }
    }

    fn capture(id: &str, ts: &str, cmd: &str, summary: &str) -> objects::Manifest {
        objects::Manifest {
            id: id.into(),
            kind: "tool-output".into(),
            cmd: Some(cmd.into()),
            cwd: None,
            exit_code: Some(0),
            git_tree: None,
            branch: None,
            files: vec![],
            diff_files: vec![],
            timestamp: ts.into(),
            raw_oid: "sha256:0".into(),
            raw_size: 0,
            raw_lines: 0,
            filter_version: 1,
            summary: summary.into(),
            highlights: vec![],
            store: "local".into(),
            codec: "none".into(),
            raw_tokens: None,
            summary_tokens: None,
            structured: None,
            env_id: Some("env/claude/x".into()),
            policy_digest: Some("d".into()),
            evidence_source: None,
            egress: None,
            redactions: vec![],
        }
    }

    #[test]
    fn violation_event_drives_critical_with_denial_ts() {
        let m = manifest("process");
        let events = vec![EnvEvent {
            ts: "2026-06-10T00:00:01Z".into(),
            env_id: m.id.clone(),
            agent: "claude".into(),
            event: "violation".into(),
            detail: Some("mediated commit refused — nested .git".into()),
            capture: None,
        }];
        let risk = classify_env(&m, None, &events, &[]);
        assert_eq!(risk.level, Severity::Critical);
        assert!(risk.score >= 40);
        assert_eq!(risk.last_denial_ts.as_deref(), Some("2026-06-10T00:00:01Z"));
        assert_eq!(risk.lane_counts.get("provenance"), Some(&1));
    }

    #[test]
    fn clean_env_scores_zero_info() {
        let m = manifest("process");
        let caps = vec![capture("cap0", "t1", "cargo test --quiet", "all tests passed")];
        let risk = classify_env(&m, None, &[], &caps);
        assert_eq!(risk.score, 0);
        assert_eq!(risk.level, Severity::Info);
        assert!(risk.findings.is_empty());
    }

    #[test]
    fn workspace_tier_downgrades_pressure_to_weak_isolation() {
        let m = manifest("workspace");
        let caps = vec![capture("cap0", "t1", "cat /etc/shadow", "root:x:0:0")];
        let risk = classify_env(&m, None, &[], &caps);
        // Relabeled grey, not amber/red.
        assert!(risk.findings.iter().any(|f| f.kind == "weak-isolation"));
        assert!(!risk.findings.iter().any(|f| f.kind == "sensitive-target"));
    }

    #[test]
    fn process_tier_host_net_only_downgrades_net_lane() {
        use crate::sandbox::{IsolationClaim, NetMode, Profile};
        // A process-tier env with wide-open host networking: NET is uncontained,
        // but Landlock/seccomp still enforce FS and PROC.
        let mut p = Profile::builtin("p", IsolationClaim::Process);
        p.net_mode = NetMode::Host;
        let m = manifest("process");
        let caps = vec![capture(
            "cap0",
            "t1",
            "unshare --mount sh -c 'curl http://203.0.113.5/x; cat /etc/shadow'",
            "",
        )];
        let risk = classify_env(&m, Some(&p), &[], &caps);

        // FS + PROC stand as honest amber pressure (the kernel enforces them).
        assert!(
            risk.findings.iter().any(|f| f.kind == "sensitive-target" && f.lane == Lane::Fs),
            "FS pressure must NOT be greyed at process tier: {:?}",
            risk.findings.iter().map(|f| (&f.kind, f.lane)).collect::<Vec<_>>()
        );
        assert!(risk.findings.iter().any(|f| f.kind == "privilege-tool" && f.lane == Lane::Proc));
        // Only the NET finding (raw-ip) is downgraded to grey — host net is real.
        assert!(
            risk.findings.iter().any(|f| f.kind == "weak-isolation" && f.lane == Lane::Net),
            "NET should be greyed under unfiltered host networking"
        );
        assert!(!risk.findings.iter().any(|f| f.kind == "raw-ip-egress"));
    }

    #[test]
    fn process_tier_net_deny_greys_nothing() {
        use crate::sandbox::{IsolationClaim, NetMode, Profile};
        let mut p = Profile::builtin("p", IsolationClaim::Process);
        p.net_mode = NetMode::Deny;
        let m = manifest("process");
        let caps = vec![capture("cap0", "t1", "cat /etc/shadow", "")];
        let risk = classify_env(&m, Some(&p), &[], &caps);
        // Net-deny process tier confines every lane → no weak relabel anywhere.
        assert!(risk.findings.iter().any(|f| f.kind == "sensitive-target"));
        assert!(!risk.findings.iter().any(|f| f.kind == "weak-isolation"));
    }

    #[test]
    fn repeated_probe_bonus_across_runs() {
        let m = manifest("process");
        let caps = vec![
            capture("cap0", "t1", "unshare --net sh", "x"),
            capture("cap1", "t2", "unshare --mount sh", "y"),
        ];
        let risk = classify_env(&m, None, &[], &caps);
        assert!(
            risk.findings.iter().any(|f| f.kind == "repeated-probe"),
            "two runs with the same privilege-tool shape should escalate"
        );
    }

    /// Build a capture carrying an egress summary (the container tier's proxy
    /// verdicts) so classify_env's NET-lane path can be exercised end to end.
    fn capture_with_egress(id: &str, ts: &str, eg: objects::EgressSummary) -> objects::Manifest {
        let mut c = capture(id, ts, "pip install requests", "ok");
        c.egress = Some(eg);
        c
    }

    #[test]
    fn egress_denied_through_classify_env_is_critical() {
        let m = manifest("container");
        let eg = objects::EgressSummary {
            allowed: 3,
            denied: 2,
            hosts: vec![
                objects::EgressHost { host: "pypi.org".into(), port: 443, allowed: 3, denied: 0 },
                objects::EgressHost { host: "evil.example".into(), port: 443, allowed: 0, denied: 2 },
            ],
            hosts_truncated: false,
            log: None,
        };
        let caps = vec![capture_with_egress("cap0", "2026-06-10T00:00:05Z", eg)];
        let risk = classify_env(&m, None, &[], &caps);
        // The denied host is a real boundary trip: critical, NET lane, blocked.
        let f = risk.findings.iter().find(|f| f.kind == "egress-denied").expect("egress-denied finding");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.lane, Lane::Net);
        assert_eq!(f.title, "Boundary blocked");
        assert!(f.evidence.contains("evil.example"));
        assert_eq!(risk.level, Severity::Critical);
        // The denial timestamp is recorded for the dashboard's "last denial".
        assert_eq!(risk.last_denial_ts.as_deref(), Some("2026-06-10T00:00:05Z"));
        // The allowed host produced no finding.
        assert!(!risk.findings.iter().any(|f| f.evidence.contains("pypi.org")));
    }

    #[test]
    fn score_caps_at_100() {
        let m = manifest("process");
        // A pile of high-weight findings whose raw sum exceeds 100.
        let caps = vec![capture(
            "cap0",
            "t1",
            "mount /dev/x /mnt; unshare --net sh; ptrace; cat /etc/shadow ~/.ssh/id_rsa; \
             curl --unix-socket /var/run/docker.sock http://x; curl http://203.0.113.5/p",
            "",
        )];
        let risk = classify_env(&m, None, &[], &caps);
        let raw: u32 = risk.findings.iter().map(|f| f.weight()).sum();
        assert!(raw > 100, "test should generate >100 raw weight, got {raw}");
        assert_eq!(risk.score, 100, "score must be capped at 100");
        assert_eq!(risk.level, Severity::Critical);
    }

    #[test]
    fn three_lane_spread_escalates_to_critical() {
        // No single critical finding, but pressure across FS + NET + PROC.
        let m = manifest("process"); // not weak → findings stay as real pressure
        use crate::sandbox::{IsolationClaim, NetMode, Profile};
        let mut p = Profile::builtin("p", IsolationClaim::Process);
        p.net_mode = NetMode::Deny; // nothing greyed
        let caps = vec![capture(
            "cap0",
            "t1",
            "unshare --mount sh -c 'cat /etc/shadow; curl http://203.0.113.5/x'",
            "",
        )];
        let risk = classify_env(&m, Some(&p), &[], &caps);
        let lanes: std::collections::BTreeSet<_> = risk.findings.iter().map(|f| f.lane).collect();
        assert!(lanes.contains(&Lane::Fs) && lanes.contains(&Lane::Net) && lanes.contains(&Lane::Proc));
        assert!(risk.lane_counts.len() >= 3);
        assert_eq!(risk.level, Severity::Critical, "a 3+-lane spread is critical escalation");
        // ...but none of the individual findings is itself critical (no denial).
        assert!(!risk.findings.iter().any(|f| f.severity == Severity::Critical));
    }

    #[test]
    fn lane_counts_are_reported_per_lane() {
        let m = manifest("process");
        use crate::sandbox::{IsolationClaim, NetMode, Profile};
        let mut p = Profile::builtin("p", IsolationClaim::Process);
        p.net_mode = NetMode::Deny;
        let caps = vec![capture("cap0", "t1", "cat /etc/shadow /root/.ssh/id_rsa", "")];
        let risk = classify_env(&m, Some(&p), &[], &caps);
        // Two FS sensitive-targets → fs lane count >= 2.
        assert!(*risk.lane_counts.get("fs").unwrap_or(&0) >= 2);
    }
}
