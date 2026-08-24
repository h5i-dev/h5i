//! From verdicts to a question a human can answer.
//!
//! The receipt already knows a box was refused `registry.npmjs.org:443` eleven
//! times. What it does not do is tell you what to *do* about it, and the last
//! step — working out which host it was and writing the `h5i box allow` line —
//! is the mechanical one people skip. This module does that step, and refuses
//! to do it where allowing would be the wrong answer.
//!
//! Two rules it keeps:
//!
//! * **Suggest, never run.** Every output of this tool is text for a person to
//!   read and paste. Widening a boundary is a decision, and a decision needs a
//!   decider.
//! * **Say which suggestion you cannot make.** A destination that looks like a
//!   beacon gets no command and a reason, rather than a command with a warning
//!   next to it that nobody reads.

use std::collections::BTreeMap;

use crate::receipt::{ExecRecord, Receipts};

/// Hosts that are how software is built: registries, source forges, artifact
/// and image stores. Matched as exact names or as suffixes (`.` prefixed).
/// Being on this list changes the *note*, never the command — an unrecognised
/// host is still suggested, because "I don't know this one" is not evidence of
/// anything and the reviewer is the one who knows what they ran.
const DEPENDENCY_HOSTS: &[&str] = &[
    // JavaScript
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "nodejs.org",
    "jsr.io",
    "deno.land",
    // Python
    "pypi.org",
    "files.pythonhosted.org",
    // Rust
    "crates.io",
    ".crates.io",
    "static.rust-lang.org",
    "sh.rustup.rs",
    // Go
    "proxy.golang.org",
    "sum.golang.org",
    "storage.googleapis.com",
    // JVM / Ruby / PHP / .NET
    "repo.maven.apache.org",
    "repo1.maven.org",
    ".rubygems.org",
    "rubygems.org",
    ".nuget.org",
    "packagist.org",
    "repo.packagist.org",
    // OS packages
    "deb.debian.org",
    "security.debian.org",
    "archive.ubuntu.com",
    "security.ubuntu.com",
    "dl-cdn.alpinelinux.org",
    // Source forges and images
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "gitlab.com",
    "bitbucket.org",
    "ghcr.io",
    "registry-1.docker.io",
    "auth.docker.io",
    "production.cloudflare.docker.com",
    "quay.io",
];

/// Destinations whose whole job is to be told about you. Nothing here is
/// needed to build, test or run anything, so the tool declines to write the
/// line rather than making it one keystroke away.
const BEACON_HOSTS: &[&str] = &[
    ".google-analytics.com",
    "google-analytics.com",
    "googletagmanager.com",
    ".doubleclick.net",
    "doubleclick.net",
    ".segment.io",
    "segment.io",
    "api.segment.io",
    ".mixpanel.com",
    "mixpanel.com",
    ".amplitude.com",
    "amplitude.com",
    ".sentry.io",
    "sentry.io",
    ".datadoghq.com",
    "datadoghq.com",
    ".bugsnag.com",
    "bugsnag.com",
    ".newrelic.com",
    "newrelic.com",
    ".hotjar.com",
    "hotjar.com",
    "scorecardresearch.com",
    ".scorecardresearch.com",
    "matomo.cloud",
    ".matomo.cloud",
    "app.posthog.com",
    ".posthog.com",
];

/// Name parts that mean "this endpoint exists to receive reports". Matched as
/// whole dot-separated labels, so `telemetry.example.net` is a beacon and
/// `metrics-api.example.com` — which may well be the thing under test — is not.
const BEACON_LABELS: &[&str] = &[
    "telemetry",
    "analytics",
    "beacon",
    "beacons",
    "tracking",
    "tracker",
    "pixel",
    "metrics",
    "collector",
    "crashreports",
    "crash-reports",
];

/// Can `h5i box allow` reach this box at all?
///
/// The host-side allowlist is merged by the proxy tiers only, and only into a
/// profile that already scopes `net.egress` — a deny-all profile is never
/// widened from outside the digested policy. So for a supervised- or
/// process-tier box the honest answer is not a shorter command, it is a
/// different one: edit `.h5i/env.toml` and re-create the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowReach {
    /// A proxy tier: `h5i box allow` applies at the next run.
    Proxy,
    /// A kernel tier (or no network at all): the allowlist lives in the policy.
    Policy,
    /// No isolation claim in this receipt — say both, claim neither.
    Unknown,
}

impl AllowReach {
    pub fn from_claim(claim: Option<&str>) -> Self {
        match claim.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("container") | Some("microvm") => Self::Proxy,
            Some("supervised") | Some("process") | Some("workspace") => Self::Policy,
            _ => Self::Unknown,
        }
    }
}

/// What the tool advises for one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suggestion {
    /// Allow it — `rule` is the allowlist entry (`host`, or `host:port` when
    /// the port is not the web's).
    Allow { rule: String, why: String },
    /// Deliberately no command, with the reason in the reader's words.
    None { why: String },
}

impl Suggestion {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::None { .. } => "no-suggestion",
        }
    }

    pub fn rule(&self) -> Option<&str> {
        match self {
            Self::Allow { rule, .. } => Some(rule),
            Self::None { .. } => None,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Self::Allow { why, .. } | Self::None { why } => why,
        }
    }
}

/// One `host:port` the allowlist refused, with everything a reviewer needs to
/// decide: how often, in which runs, and what was running at the time.
#[derive(Debug, Clone)]
pub struct Destination {
    pub host: String,
    pub port: u16,
    pub denied: u64,
    /// Non-zero when the same destination was also *reached* — a host that is
    /// half allowed is usually a missing `:port` or a second name, not a new
    /// decision.
    pub allowed: u64,
    /// Short run handles, in the order they first appear in the receipt.
    pub runs: Vec<String>,
    /// The command from the first run that hit this, secret-redacted by h5i
    /// before it was ever written.
    pub example_cmd: Option<String>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub suggestion: Suggestion,
}

impl Destination {
    /// `host:port`, the form h5i's own receipt and dashboard use.
    pub fn label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// The whole advice for one receipt.
#[derive(Debug, Clone)]
pub struct Advice {
    pub destinations: Vec<Destination>,
    /// Runs that carried any egress verdict at all — the denominator.
    pub runs_seen: usize,
    /// Runs with at least one refusal.
    pub runs_refused: usize,
    pub total_denied: u64,
    pub reach: AllowReach,
    pub isolation_claim: Option<String>,
    pub profile: Option<String>,
    pub env_id: Option<String>,
    pub origin: String,
    pub warnings: Vec<String>,
}

impl Advice {
    pub fn is_empty(&self) -> bool {
        self.destinations.is_empty()
    }
}

/// Aggregate the refusals in a receipt and decide what to say about each.
pub fn advise(receipts: &Receipts, min_count: u64) -> Advice {
    let mut by_dest: BTreeMap<(String, u16), Destination> = BTreeMap::new();
    // Counted over *every* record, not only the ones that were refused: a
    // destination that was reached in one run and refused in another is the
    // most useful thing this report can point at — usually a second name for
    // the same service, or an entry that is missing its port.
    let mut reached: BTreeMap<(String, u16), u64> = BTreeMap::new();
    let mut runs_seen = 0usize;
    let mut runs_refused = 0usize;
    let mut warnings = receipts.warnings.clone();
    let mut truncated_runs: Vec<String> = Vec::new();

    for rec in &receipts.records {
        let Some(eg) = &rec.egress else { continue };
        runs_seen += 1;
        if eg.hosts_truncated {
            truncated_runs.push(rec.short_id().to_string());
        }
        let mut refused_here = false;
        for h in &eg.hosts {
            if h.allowed > 0 {
                *reached.entry((sanitize(&h.host), h.port)).or_default() += h.allowed;
            }
            if h.denied == 0 {
                continue;
            }
            refused_here = true;
            let host = sanitize(&h.host);
            let entry = by_dest
                .entry((host.clone(), h.port))
                .or_insert_with(|| new_destination(host, h.port, rec));
            entry.denied += h.denied;
            let id = rec.short_id().to_string();
            if !entry.runs.contains(&id) {
                entry.runs.push(id);
            }
            if !rec.timestamp.is_empty() {
                entry.last_seen = Some(sanitize(&rec.timestamp));
            }
        }
        // A run whose summary counts refusals but names no host is the clamped
        // case, or an older receipt: say so rather than reporting nothing.
        if !refused_here && eg.denied > 0 {
            warnings.push(format!(
                "run {} recorded {} refusal(s) with no per-destination detail",
                rec.short_id(),
                eg.denied
            ));
        }
        if refused_here {
            runs_refused += 1;
        }
    }

    if !truncated_runs.is_empty() {
        warnings.push(format!(
            "h5i clamped the per-destination list in {} run(s) ({}) — there were more destinations than these",
            truncated_runs.len(),
            truncated_runs.join(", ")
        ));
    }

    let reach = AllowReach::from_claim(receipts.identity.isolation_claim.as_deref());
    let mut destinations: Vec<Destination> = by_dest
        .into_values()
        .filter(|d| d.denied >= min_count)
        .collect();
    for d in &mut destinations {
        d.allowed = reached
            .get(&(d.host.clone(), d.port))
            .copied()
            .unwrap_or_default();
        d.suggestion = classify(&d.host, d.port);
    }
    // Loudest first; ties by name so two runs of the tool agree.
    destinations.sort_by(|a, b| {
        b.denied
            .cmp(&a.denied)
            .then_with(|| a.label().cmp(&b.label()))
    });

    let total_denied = destinations.iter().map(|d| d.denied).sum();
    Advice {
        destinations,
        runs_seen,
        runs_refused,
        total_denied,
        reach,
        isolation_claim: receipts.identity.isolation_claim.clone(),
        profile: receipts.identity.profile.clone(),
        env_id: receipts.identity.env_id.clone(),
        origin: receipts.origin.display().to_string(),
        warnings,
    }
}

fn new_destination(host: String, port: u16, rec: &ExecRecord) -> Destination {
    Destination {
        host,
        port,
        denied: 0,
        allowed: 0,
        runs: Vec::new(),
        example_cmd: rec.cmd.as_deref().map(sanitize).filter(|c| !c.is_empty()),
        first_seen: Some(sanitize(&rec.timestamp)).filter(|t| !t.is_empty()),
        last_seen: None,
        suggestion: Suggestion::None { why: String::new() },
    }
}

/// Decide what to advise for one destination.
pub fn classify(host: &str, port: u16) -> Suggestion {
    let lower = host.trim_end_matches('.').to_ascii_lowercase();
    if lower.is_empty() {
        return Suggestion::None {
            why: "the receipt recorded no host name for this destination".into(),
        };
    }
    if is_ip_literal(&lower) {
        return Suggestion::None {
            why: "a bare address names nothing you can review — find out what it is first".into(),
        };
    }
    if let Some(reason) = beacon_reason(&lower) {
        return Suggestion::None { why: reason };
    }
    let rule = if matches!(port, 80 | 443) {
        lower.clone()
    } else {
        // The allowlist takes a `:port` form, and a non-web port is exactly
        // where you want the entry to be narrow.
        format!("{lower}:{port}")
    };
    let why = if matches_list(&lower, DEPENDENCY_HOSTS) {
        "a package registry or source host — the ordinary reason a build reaches out".into()
    } else if matches!(port, 80 | 443) {
        "not a host this tool recognises — check the command that wanted it".into()
    } else {
        format!(
            "not a host this tool recognises, and :{port} is not HTTP — check the command that wanted it"
        )
    };
    Suggestion::Allow { rule, why }
}

fn beacon_reason(host: &str) -> Option<String> {
    if matches_list(host, BEACON_HOSTS) {
        return Some("this is a known telemetry endpoint, not a dependency".into());
    }
    let label = host
        .split('.')
        .find(|l| BEACON_LABELS.contains(&l.to_ascii_lowercase().as_str()))?;
    Some(format!(
        "this looks like a beacon, not a dependency (the '{label}' label)"
    ))
}

/// Exact name, or suffix for entries written with a leading dot.
fn matches_list(host: &str, list: &[&str]) -> bool {
    list.iter().any(|e| {
        if let Some(suffix) = e.strip_prefix('.') {
            host == suffix || host.ends_with(e)
        } else {
            host == *e
        }
    })
}

/// Is this an address rather than a name? Covers the bracketed and bare IPv6
/// forms a proxy log can carry as well as dotted quads.
fn is_ip_literal(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<std::net::IpAddr>().is_ok()
}

/// Strip anything that would let recorded text steer the terminal it is
/// printed to. h5i redacts secrets before a command is ever written; escape
/// sequences are the other half, and this report is read in a terminal.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' {
                ' '
            } else if c.is_control() || matches!(c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::{BoxIdentity, EgressHost, EgressSummary};
    use std::path::PathBuf;

    fn rec(id: &str, ts: &str, cmd: &str, hosts: &[(&str, u16, u64, u64)]) -> ExecRecord {
        ExecRecord {
            id: id.into(),
            timestamp: ts.into(),
            env_id: "env/claude/fix".into(),
            cmd: Some(cmd.into()),
            egress: Some(EgressSummary {
                denied: hosts.iter().map(|h| h.3).sum(),
                hosts: hosts
                    .iter()
                    .map(|(h, p, a, d)| EgressHost {
                        host: (*h).into(),
                        port: *p,
                        allowed: *a,
                        denied: *d,
                    })
                    .collect(),
                hosts_truncated: false,
            }),
        }
    }

    fn receipts(records: Vec<ExecRecord>, claim: Option<&str>) -> Receipts {
        Receipts {
            records,
            identity: BoxIdentity {
                env_id: Some("env/claude/fix".into()),
                profile: Some("review".into()),
                isolation_claim: claim.map(str::to_string),
            },
            origin: PathBuf::from("receipt.json"),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn refusals_are_grouped_across_runs_and_counted() {
        let r = receipts(
            vec![
                rec(
                    "a3f1c2aa",
                    "t1",
                    "npm install",
                    &[("registry.npmjs.org", 443, 0, 7)],
                ),
                rec(
                    "9b04de11",
                    "t2",
                    "npm test",
                    &[("registry.npmjs.org", 443, 0, 4)],
                ),
            ],
            Some("container"),
        );
        let a = advise(&r, 1);
        assert_eq!(a.destinations.len(), 1);
        let d = &a.destinations[0];
        assert_eq!(d.denied, 11);
        assert_eq!(d.runs, vec!["a3f1c2", "9b04de"]);
        assert_eq!(d.suggestion.rule(), Some("registry.npmjs.org"));
        assert_eq!(a.runs_refused, 2);
        assert_eq!(a.total_denied, 11);
    }

    #[test]
    fn allowed_traffic_alone_produces_no_candidates() {
        let r = receipts(
            vec![rec("a", "t", "curl", &[("api.github.com", 443, 9, 0)])],
            Some("container"),
        );
        let a = advise(&r, 1);
        assert!(a.is_empty());
        assert_eq!(a.runs_seen, 1);
        assert_eq!(a.runs_refused, 0);
    }

    /// The signal a per-record tally hid: refused in one run, reached in
    /// another, which is nearly always a policy that is *almost* right.
    #[test]
    fn a_destination_reached_in_another_run_carries_that_count() {
        let r = receipts(
            vec![
                rec("a", "t1", "pip install", &[("pypi.org", 443, 0, 2)]),
                rec("b", "t2", "pytest", &[("pypi.org", 443, 5, 0)]),
            ],
            Some("container"),
        );
        let a = advise(&r, 1);
        assert_eq!(a.destinations[0].denied, 2);
        assert_eq!(a.destinations[0].allowed, 5);
        assert_eq!(a.destinations[0].runs, vec!["a"]);
    }

    #[test]
    fn the_loudest_destination_is_reported_first() {
        let r = receipts(
            vec![rec(
                "a",
                "t",
                "make",
                &[
                    ("quiet.example.com", 443, 0, 1),
                    ("loud.example.com", 443, 0, 30),
                ],
            )],
            Some("container"),
        );
        let a = advise(&r, 1);
        assert_eq!(a.destinations[0].host, "loud.example.com");
    }

    #[test]
    fn a_beacon_gets_a_reason_instead_of_a_command() {
        let s = classify("telemetry.example.net", 443);
        assert_eq!(s.rule(), None);
        assert!(s.why().contains("beacon"), "{}", s.why());
        assert!(classify("api.segment.io", 443).rule().is_none());
        // A metrics API under test is not a beacon by its hyphenated name.
        assert!(classify("metrics-api.example.com", 443).rule().is_some());
    }

    #[test]
    fn a_bare_address_gets_no_command() {
        assert!(classify("203.0.113.7", 443).rule().is_none());
        assert!(classify("[2001:db8::1]", 443).rule().is_none());
        assert!(classify("2001:db8::1", 443).rule().is_none());
    }

    #[test]
    fn a_non_web_port_is_kept_in_the_rule() {
        let s = classify("db.example.com", 5432);
        assert_eq!(s.rule(), Some("db.example.com:5432"));
        assert!(s.why().contains("not HTTP"));
        assert_eq!(classify("example.com", 80).rule(), Some("example.com"));
    }

    #[test]
    fn a_known_registry_says_why_it_is_ordinary() {
        let s = classify("files.pythonhosted.org", 443);
        assert!(s.why().contains("registry"), "{}", s.why());
        // Suffix entries match a subdomain as well as the bare name.
        assert!(classify("static.crates.io", 443).why().contains("registry"));
    }

    #[test]
    fn the_tier_decides_whether_box_allow_can_reach_it() {
        assert_eq!(AllowReach::from_claim(Some("container")), AllowReach::Proxy);
        assert_eq!(AllowReach::from_claim(Some("microvm")), AllowReach::Proxy);
        assert_eq!(
            AllowReach::from_claim(Some("supervised")),
            AllowReach::Policy
        );
        assert_eq!(AllowReach::from_claim(None), AllowReach::Unknown);
    }

    #[test]
    fn a_clamped_host_list_is_reported_rather_than_passed_off_as_complete() {
        let mut r = rec("a", "t", "make", &[("x.example.com", 443, 0, 1)]);
        r.egress.as_mut().unwrap().hosts_truncated = true;
        let a = advise(&receipts(vec![r], Some("container")), 1);
        assert!(
            a.warnings.iter().any(|w| w.contains("clamped")),
            "{:?}",
            a.warnings
        );
    }

    #[test]
    fn refusals_with_no_named_destination_are_not_silently_dropped() {
        let r = ExecRecord {
            id: "a3f1c2".into(),
            egress: Some(EgressSummary {
                denied: 3,
                ..EgressSummary::default()
            }),
            ..ExecRecord::default()
        };
        let a = advise(&receipts(vec![r], Some("container")), 1);
        assert!(a.is_empty());
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("no per-destination detail"))
        );
    }

    #[test]
    fn min_count_filters_the_long_tail() {
        let r = receipts(
            vec![rec(
                "a",
                "t",
                "make",
                &[
                    ("one.example.com", 443, 0, 1),
                    ("many.example.com", 443, 0, 9),
                ],
            )],
            Some("container"),
        );
        assert_eq!(advise(&r, 5).destinations.len(), 1);
    }

    #[test]
    fn recorded_text_cannot_steer_the_terminal_it_is_printed_to() {
        let d = sanitize("npm \u{1b}[31minstall\u{1b}[0m\u{202e}");
        assert!(!d.contains('\u{1b}'));
        assert!(!d.contains('\u{202e}'));
    }
}
