//! The attention model — backend-computed triage, shared by every client.
//!
//! `h5i status`, the web workbench (`h5i serve`), and MCP all consume the
//! same two projections computed here, so "what needs me?" has exactly one
//! deterministic answer per repo state:
//!
//! - [`AttentionItem`] — one thing that may need a human, with its
//!   `priority`, the `reasons` behind it (never hidden), `evidence`
//!   references each stamped with an [`Authority`], the read-only
//!   `commands` that act on it, and per-identity seen-state.
//! - [`WorkItem`] — the selectable unit of work (an env or a team run),
//!   with a lifecycle and its unseen-attention count.
//!
//! **Authority** is the product's epistemic vocabulary: every claim says
//! *how it is known* — `enforced` (the kernel/proxy acted), `verified`
//! (neutral re-execution), `observed` (live pid / host-side record),
//! `reported` (a hook or the agent said so), `inferred` (deterministic
//! classifier), `unknown` (no evidence exists). An absent claim is shown
//! as absent, never dressed as success.
//!
//! **Seen-ness makes the queue drain.** "Done" and "idle" are the same
//! state distinguished only by attention, so cursors are stored
//! per-identity (`<h5i_root>/attention/<identity>.json`, like `msg`'s
//! per-agent views) and shared by CLI and web. A cursor records the
//! `occurred_at` it saw; an item whose `occurred_at` moves past the cursor
//! re-arms as unseen (new evidence re-raises the flag).
//!
//! The rule functions are pure (`env_attention`, `team_attention`, …) so
//! the triage logic is unit-testable without a repo; [`report`] is the
//! thin glue that feeds them from a live repository.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::env::{self, EnvEvent, EnvManifest, LiveSession};
use crate::error::H5iError;
use crate::repository::H5iRepository;
use crate::risk::{EnvRisk, Severity};
use crate::team::TeamRun;

/// How a claim is known. The order is trust order, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Authority {
    /// The kernel or egress proxy acted — a denial actually fired.
    Enforced,
    /// Neutrally re-executed in a fresh sandboxed worktree.
    Verified,
    /// Seen by the host: live pid, lock, spool, or PTY record.
    Observed,
    /// Claimed by a hook or the agent itself — honest but unaudited.
    Reported,
    /// Produced by a deterministic classifier over evidence.
    Inferred,
    /// No evidence exists, or the host lacks the capability to produce it.
    Unknown,
}

/// A pointer from a claim to the journaled evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// `capture` | `event` | `verification` | `session` | `message`.
    pub kind: String,
    /// Capture/object id, event timestamp, verification id, pid, …
    pub id: String,
    pub authority: Authority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Attention classes, most urgent first. Deterministic — the UI never
/// re-ranks; it renders this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Boundary event, failed verifier, stale writer — investigate first.
    Critical,
    /// A decision only a human can make: proposed env, verdict pending.
    Decision,
    /// Unread cross-agent communication addressed to this identity.
    Communication,
    /// Live agents — awareness, not action.
    Active,
    /// Drift, gaps — worth knowing, never urgent.
    Info,
}

/// What the entity behind an item is: `env` | `team` | `msg`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
}

/// One thing that may need a human. `id` is stable across recomputations
/// so seen-cursors can target it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub id: String,
    pub priority: Priority,
    /// `blocked` (needs you) | `working` (live) | `info`.
    pub state: String,
    pub entity: EntityRef,
    pub title: String,
    /// Why this is here — always populated, never hidden.
    pub reasons: Vec<String>,
    pub evidence: Vec<EvidenceRef>,
    /// Read-only affordance: the exact commands to act in a terminal.
    pub commands: Vec<String>,
    /// RFC3339; also the re-arm watermark for seen-cursors.
    pub occurred_at: String,
    /// When this identity saw the item at its current watermark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
}

/// A seat on a work item (an agent bound to an env).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatView {
    pub agent: String,
    pub env_id: String,
    /// `working` (live writer) | `idle` | `enrolled` (team seat, liveness
    /// tracked on its env's own work item).
    pub status: String,
}

/// The selectable unit of work: an env or a team run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// `env/<agent>/<slug>` or `team/<id>` — doubles as the URL id.
    pub id: String,
    /// `env` | `team`.
    pub kind: String,
    pub title: String,
    /// env: `draft|working|review|applied|aborted` (from status);
    /// team: the engine phase verbatim, or `decided` once a verdict exists.
    pub lifecycle: String,
    pub seats: Vec<SeatView>,
    pub updated_at: String,
    /// Unseen attention items pointing at this entity.
    pub unseen: usize,
}

/// The complete projection — what `h5i status --json` prints and
/// `/api/attention` serves. CLI and web are byte-identical by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionReport {
    pub generated_at: String,
    /// Whose seen-cursor was applied.
    pub identity: String,
    pub items: Vec<AttentionItem>,
    pub work_items: Vec<WorkItem>,
}

// ── pure rules ──────────────────────────────────────────────────────────────

fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef { kind: kind.into(), id: id.into() }
}

/// Findings whose kind names an actual denial are enforcement evidence;
/// everything else out of the classifier is inference. Deterministic and
/// documented rather than clever.
fn finding_authority(kind: &str) -> Authority {
    if kind.contains("violation") || kind.contains("denied") || kind.contains("blocked") {
        Authority::Enforced
    } else {
        Authority::Inferred
    }
}

/// Attention raised by one env: boundary pressure, stale writer, proposal
/// awaiting review, live work, drift.
pub fn env_attention(
    m: &EnvManifest,
    risk: &EnvRisk,
    events: &[EnvEvent],
    live: &[LiveSession],
    stale_running: bool,
    drift_kind: &str,
) -> Vec<AttentionItem> {
    let mut items = Vec::new();
    let last_ts = events.last().map(|e| e.ts.clone()).unwrap_or_else(|| m.updated_at.clone());

    // Critical: enforcement fired / critical pressure.
    let critical: Vec<_> = risk
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .collect();
    if !critical.is_empty() {
        let occurred = risk
            .last_denial_ts
            .clone()
            .or_else(|| critical.iter().filter_map(|f| f.event_ts.clone()).max())
            .unwrap_or_else(|| last_ts.clone());
        items.push(AttentionItem {
            id: format!("boundary:{}", m.id),
            priority: Priority::Critical,
            state: "blocked".into(),
            entity: entity("env", &m.id),
            title: format!("{}: boundary pressure ({})", m.slug, risk.score),
            reasons: critical
                .iter()
                .take(3)
                .map(|f| format!("{}: {}", f.title, f.detail))
                .collect(),
            evidence: critical
                .iter()
                .map(|f| EvidenceRef {
                    kind: if f.capture_id.is_some() { "capture" } else { "event" }.into(),
                    id: f
                        .capture_id
                        .clone()
                        .or_else(|| f.event_ts.clone())
                        .unwrap_or_else(|| f.kind.clone()),
                    authority: finding_authority(&f.kind),
                    note: Some(f.evidence.clone()),
                })
                .collect(),
            commands: vec![
                format!("h5i env status {}", m.slug),
                format!("h5i recall objects --env {}", m.slug),
            ],
            occurred_at: occurred,
            seen_at: None,
        });
    }

    // Critical: durable status says running, but no live writer holds the env.
    if stale_running {
        items.push(AttentionItem {
            id: format!("stale-writer:{}", m.id),
            priority: Priority::Critical,
            state: "blocked".into(),
            entity: entity("env", &m.id),
            title: format!("{}: stale writer", m.slug),
            reasons: vec![
                "status is `running` but no live writer session holds the env — \
                 the session likely crashed"
                    .into(),
            ],
            evidence: vec![EvidenceRef {
                kind: "session".into(),
                id: "none-live".into(),
                authority: Authority::Observed,
                note: Some("pid registry has no verified writer".into()),
            }],
            commands: vec![format!("h5i env shell {} -- true", m.slug)],
            occurred_at: last_ts.clone(),
            seen_at: None,
        });
    }

    // Decision: a proposal is waiting on a reviewer.
    if m.status == env::ST_PROPOSED {
        let proposed_ts = events
            .iter()
            .rev()
            .find(|e| e.event == "proposed")
            .map(|e| e.ts.clone())
            .unwrap_or_else(|| last_ts.clone());
        items.push(AttentionItem {
            id: format!("review:{}", m.id),
            priority: Priority::Decision,
            state: "blocked".into(),
            entity: entity("env", &m.id),
            title: format!("{}: proposed, awaiting review", m.slug),
            reasons: vec!["a mediated commit is ready; apply is reviewer-selected, never automatic".into()],
            evidence: vec![EvidenceRef {
                kind: "event".into(),
                id: proposed_ts.clone(),
                authority: Authority::Reported,
                note: Some("propose event on the env log".into()),
            }],
            commands: vec![
                format!("h5i env diff {}", m.slug),
                format!("h5i env apply {}", m.slug),
            ],
            occurred_at: proposed_ts,
            seen_at: None,
        });
    }

    // Active: live writer sessions (observed via the pid registry).
    for s in live.iter().filter(|s| env::live_is_writer(&s.kind)) {
        items.push(AttentionItem {
            id: format!("working:{}:{}", m.id, s.pid),
            priority: Priority::Active,
            state: "working".into(),
            entity: entity("env", &m.id),
            title: format!("{}: {} session live", m.slug, s.kind),
            reasons: vec![s
                .command
                .clone()
                .unwrap_or_else(|| format!("{} session since {}", s.kind, s.started_at))],
            evidence: vec![EvidenceRef {
                kind: "session".into(),
                id: s.pid.to_string(),
                authority: Authority::Observed,
                note: Some(format!("pid-verified, started {}", s.started_at)),
            }],
            commands: vec![format!("h5i env shell {} --readonly", m.slug)],
            occurred_at: s.started_at.clone(),
            seen_at: None,
        });
    }

    // Info: the base drifted under the env.
    if drift_kind != "up-to-date" && m.status != env::ST_APPLIED && m.status != env::ST_ABORTED {
        items.push(AttentionItem {
            id: format!("drift:{}", m.id),
            priority: Priority::Info,
            state: "info".into(),
            entity: entity("env", &m.id),
            title: format!("{}: base {}", m.slug, drift_kind),
            reasons: vec![format!(
                "parent branch `{}` moved since the base was pinned",
                m.parent_branch
            )],
            evidence: vec![EvidenceRef {
                kind: "event".into(),
                id: "drift".into(),
                authority: Authority::Observed,
                note: Some("computed from git refs, not reported state".into()),
            }],
            commands: vec![format!("h5i env rebase {}", m.slug)],
            occurred_at: last_ts,
            seen_at: None,
        });
    }

    items
}

/// Attention raised by one team run: failed verifiers, verdict pending.
pub fn team_attention(run: &TeamRun) -> Vec<AttentionItem> {
    // A recorded verdict resolves the run, including any verifier failures
    // from earlier attempts. The run detail remains the audit surface for
    // that history; attention only projects current unresolved conditions.
    if run.verdict.is_some() {
        return Vec::new();
    }

    let mut items = Vec::new();

    // One current candidate per enrolled agent. `latest_submission_id` is
    // populated by team status; the fallback keeps legacy/hand-authored runs
    // deterministic by applying the same round/time/id ordering.
    let current_submissions: Vec<_> = run
        .agents
        .iter()
        .filter_map(|agent| {
            agent
                .latest_submission_id
                .as_deref()
                .and_then(|id| {
                    run.submissions
                        .iter()
                        .find(|s| s.id == id && s.round == run.current_round)
                })
                .or_else(|| {
                    run.submissions
                        .iter()
                        .filter(|s| {
                            s.owner_agent == agent.agent_id && s.round == run.current_round
                        })
                        .max_by(|a, b| {
                            a.round
                                .cmp(&b.round)
                                .then(a.submitted_at.cmp(&b.submitted_at))
                                .then(a.id.cmp(&b.id))
                        })
                })
        })
        .collect();
    let current_ids: BTreeSet<&str> =
        current_submissions.iter().map(|s| s.id.as_str()).collect();

    // Verification ids end in their creation timestamp, so the greatest id
    // is the latest neutral re-execution for a submission (the same ordering
    // used by team::default_verdict). An older failure must not survive a
    // later passing verification or a superseding submission.
    let mut latest_verifications: BTreeMap<&str, &crate::team::TeamVerification> =
        BTreeMap::new();
    for verification in &run.verifications {
        if !current_ids.contains(verification.submission_id.as_str()) {
            continue;
        }
        let slot = latest_verifications
            .entry(verification.submission_id.as_str())
            .or_insert(verification);
        if verification.id > slot.id {
            *slot = verification;
        }
    }

    for v in latest_verifications
        .values()
        .copied()
        .filter(|v| !v.tests_passed || !v.applies_cleanly)
    {
        let what = if !v.applies_cleanly { "does not apply cleanly" } else { "tests failed" };
        items.push(AttentionItem {
            id: format!("verify-fail:{}:{}", run.id, v.id),
            priority: Priority::Critical,
            state: "blocked".into(),
            entity: entity("team", &run.id),
            title: format!("{}: {}'s candidate {}", run.name, v.owner_agent, what),
            reasons: vec![format!(
                "neutral re-execution of `{}` under {} isolation: {}",
                v.command.join(" "),
                v.isolation,
                what
            )],
            evidence: vec![EvidenceRef {
                kind: "verification".into(),
                id: v.id.clone(),
                authority: Authority::Verified,
                note: Some(format!("round {}", v.round)),
            }],
            commands: vec![format!("h5i team compare {}", run.id)],
            occurred_at: run.created_at.clone(),
            seen_at: None,
        });
    }

    let all_agents_submitted =
        !run.agents.is_empty() && current_submissions.len() == run.agents.len();
    if all_agents_submitted {
        let verified = latest_verifications
            .values()
            .filter(|v| v.tests_passed && v.applies_cleanly)
            .count();
        let latest = current_submissions
            .iter()
            .map(|s| s.submitted_at.clone())
            .max()
            .unwrap_or_else(|| run.created_at.clone());
        let mut evidence: Vec<EvidenceRef> = current_submissions
            .iter()
            .map(|s| EvidenceRef {
                kind: "capture".into(),
                id: s.id.clone(),
                authority: Authority::Reported,
                note: Some(format!("submitted by {}", s.owner_agent)),
            })
            .collect();
        evidence.extend(latest_verifications.values().map(|v| EvidenceRef {
            kind: "verification".into(),
            id: v.id.clone(),
            authority: Authority::Verified,
            note: None,
        }));
        items.push(AttentionItem {
            id: format!("decide:{}", run.id),
            priority: Priority::Decision,
            state: "blocked".into(),
            entity: entity("team", &run.id),
            title: format!(
                "{}: {} candidate(s) ready, no verdict",
                run.name,
                current_submissions.len()
            ),
            reasons: vec![format!(
                "{} of {} candidates verified; finalize records the verdict",
                verified,
                current_submissions.len()
            )],
            evidence,
            commands: vec![
                format!("h5i team compare {}", run.id),
                format!("h5i team finalize {}", run.id),
            ],
            occurred_at: latest,
            seen_at: None,
        });
    }

    items
}

/// Unread cross-agent messages for this identity. Not seen-cursored here:
/// `h5i msg inbox` is the read state, so the item drains by reading.
pub fn msg_attention(unread: usize, identity: &str, now: &str) -> Option<AttentionItem> {
    if unread == 0 {
        return None;
    }
    Some(AttentionItem {
        id: format!("msg-unread:{identity}"),
        priority: Priority::Communication,
        state: "blocked".into(),
        entity: entity("msg", identity),
        title: format!("{unread} unread message(s)"),
        reasons: vec!["another agent is waiting on a reply".into()],
        evidence: vec![EvidenceRef {
            kind: "message".into(),
            id: format!("unread:{unread}"),
            authority: Authority::Reported,
            note: None,
        }],
        commands: vec!["h5i msg inbox".into()],
        occurred_at: now.into(),
        seen_at: None,
    })
}

/// The env → work-item projection.
pub fn env_work_item(m: &EnvManifest, live: &[LiveSession]) -> WorkItem {
    let lifecycle = match m.status.as_str() {
        env::ST_CREATED => "draft",
        env::ST_RUNNING => "working",
        env::ST_PROPOSED => "review",
        env::ST_APPLIED => "applied",
        env::ST_ABORTED => "aborted",
        other => other,
    };
    let working = live.iter().any(|s| env::live_is_writer(&s.kind));
    WorkItem {
        id: m.id.clone(),
        kind: "env".into(),
        title: m.slug.clone(),
        lifecycle: lifecycle.into(),
        seats: vec![SeatView {
            agent: m.agent.clone(),
            env_id: m.id.clone(),
            status: if working { "working" } else { "idle" }.into(),
        }],
        updated_at: m.updated_at.clone(),
        unseen: 0,
    }
}

/// The team-run → work-item projection.
pub fn team_work_item(run: &TeamRun) -> WorkItem {
    let lifecycle = if run.verdict.is_some() { "decided".to_string() } else { run.phase.clone() };
    WorkItem {
        id: format!("team/{}", run.id),
        kind: "team".into(),
        title: run.name.clone(),
        lifecycle,
        seats: run
            .agents
            .iter()
            .map(|a| SeatView {
                agent: a.agent_id.clone(),
                env_id: a.env_id.clone(),
                status: "enrolled".into(),
            })
            .collect(),
        updated_at: run
            .submissions
            .iter()
            .map(|s| s.submitted_at.clone())
            .max()
            .unwrap_or_else(|| run.created_at.clone()),
        unseen: 0,
    }
}

// ── seen-cursors ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    version: u32,
    /// item id → the `occurred_at` watermark that was seen.
    seen: BTreeMap<String, String>,
}

fn cursor_path(h5i_root: &Path, identity: &str) -> std::path::PathBuf {
    // Identities are agent names (`claude`, `codex`, `host`) — sanitize so a
    // hostile identity can't traverse out of the attention dir.
    let safe: String = identity
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    h5i_root.join("attention").join(format!("{safe}.json"))
}

fn load_cursor(h5i_root: &Path, identity: &str) -> Cursor {
    std::fs::read(cursor_path(h5i_root, identity))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Stamp `seen_at` on items the cursor has seen *at their current
/// watermark* — an item whose `occurred_at` advanced past the cursor
/// re-arms as unseen.
pub fn apply_seen(items: &mut [AttentionItem], h5i_root: &Path, identity: &str, now: &str) {
    let cursor = load_cursor(h5i_root, identity);
    for item in items.iter_mut() {
        if let Some(seen_watermark) = cursor.seen.get(&item.id) {
            if seen_watermark.as_str() >= item.occurred_at.as_str() {
                item.seen_at = Some(now.to_string());
            }
        }
    }
}

/// Record that `identity` has seen the given items (all items when `only`
/// is `None`) at their current watermarks. The one mutation the assurance
/// plane is allowed: it changes attention state, never the repo.
pub fn mark_seen(
    h5i_root: &Path,
    identity: &str,
    items: &[AttentionItem],
    only: Option<&[String]>,
) -> Result<usize, H5iError> {
    let mut cursor = load_cursor(h5i_root, identity);
    cursor.version = 1;
    let mut marked = 0;
    for item in items {
        if only.map(|ids| ids.iter().any(|id| id == &item.id)).unwrap_or(true) {
            cursor.seen.insert(item.id.clone(), item.occurred_at.clone());
            marked += 1;
        }
    }
    let path = cursor_path(h5i_root, identity);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| H5iError::Metadata(format!("attention cursor dir: {e}")))?;
    }
    let body = serde_json::to_vec_pretty(&cursor)
        .map_err(|e| H5iError::Metadata(format!("attention cursor encode: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| H5iError::Metadata(format!("attention cursor write: {e}")))?;
    Ok(marked)
}

// ── the report (glue over a live repo) ──────────────────────────────────────

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Compute the full attention report for one identity (explicit >
/// `$H5I_AGENT` > stored msg identity > `host`), with seen-cursors applied
/// and work items carrying their unseen counts.
pub fn report(repo: &H5iRepository, identity: Option<&str>) -> AttentionReport {
    let identity = crate::msg::resolve_identity(&repo.h5i_root, identity)
        .unwrap_or_else(|_| "host".to_string());
    let git = repo.git();
    let h5i_root = &repo.h5i_root;
    let now = now_rfc3339();

    let mut items: Vec<AttentionItem> = Vec::new();
    let mut work_items: Vec<WorkItem> = Vec::new();

    for m in env::list(h5i_root) {
        let events = env::read_events(git, Some(&m.id));
        let captures: Vec<crate::objects::Manifest> = m
            .captures
            .iter()
            .filter_map(|id| crate::objects::resolve_manifest(git, id).ok())
            .collect();
        let policy = env::load_policy(h5i_root, &m).ok().map(|rp| rp.profile);
        let risk = crate::risk::classify_env(&m, policy.as_ref(), &events, &captures);
        let live = env::live_sessions(&m.dir(h5i_root));
        let stale_running =
            m.status == env::ST_RUNNING && !live.iter().any(|s| env::live_is_writer(&s.kind));
        let drift = env::drift(git, &m);
        items.extend(env_attention(&m, &risk, &events, &live, stale_running, drift.kind_str()));
        work_items.push(env_work_item(&m, &live));
    }

    if let Ok(runs) = crate::team::list(git) {
        for run in &runs {
            items.extend(team_attention(run));
            work_items.push(team_work_item(run));
        }
    }

    if let Ok(unread) = crate::msg::unread_count(git, h5i_root, &identity) {
        items.extend(msg_attention(unread, &identity, &now));
    }

    apply_seen(&mut items, h5i_root, &identity, &now);
    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.occurred_at.cmp(&a.occurred_at))
    });
    for wi in work_items.iter_mut() {
        let entity_id = if wi.kind == "team" {
            wi.id.trim_start_matches("team/").to_string()
        } else {
            wi.id.clone()
        };
        wi.unseen = items
            .iter()
            .filter(|i| i.entity.id == entity_id && i.seen_at.is_none())
            .count();
    }
    work_items.sort_by(|a, b| b.unseen.cmp(&a.unseen).then(b.updated_at.cmp(&a.updated_at)));

    AttentionReport { generated_at: now, identity, items, work_items }
}

/// Find one item by id — the `--explain` lookup.
pub fn find<'r>(report: &'r AttentionReport, id: &str) -> Option<&'r AttentionItem> {
    report.items.iter().find(|i| i.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::{Finding, Lane};
    use crate::team::{TeamAgent, TeamArtifact, TeamVerdict, TeamVerification};

    fn manifest(status: &str) -> EnvManifest {
        EnvManifest {
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
            isolation_claim: "workspace".into(),
            backend: "worktree".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            status: status.into(),
            captures: vec![],
            service_digest: None,
            persona_digest: None,
            pr: None,
            pr_head_ref: None,
        }
    }

    fn no_risk() -> EnvRisk {
        EnvRisk {
            score: 0,
            level: Severity::Info,
            findings: vec![],
            lane_counts: Default::default(),
            last_denial_ts: None,
        }
    }

    fn critical_risk(kind: &str) -> EnvRisk {
        EnvRisk {
            score: 60,
            level: Severity::Critical,
            findings: vec![Finding {
                severity: Severity::Critical,
                lane: Lane::Net,
                kind: kind.into(),
                title: "Boundary blocked".into(),
                detail: "egress to crates.io refused".into(),
                evidence: "CONNECT crates.io:443 -> 403".into(),
                capture_id: Some("cap9".into()),
                event_ts: Some("2026-01-03T00:00:00Z".into()),
            }],
            lane_counts: Default::default(),
            last_denial_ts: Some("2026-01-03T00:00:00Z".into()),
        }
    }

    fn run(verdict: bool, verified_ok: bool) -> TeamRun {
        TeamRun {
            id: "r1".into(),
            name: "fix-auth".into(),
            base_oid: "0".into(),
            created_by: "host".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            phase: "review".into(),
            current_round: 1,
            max_rounds: 2,
            agents: vec![TeamAgent {
                agent_id: "claude".into(),
                env_id: "env/claude/x".into(),
                runtime: None,
                model: None,
                effort: None,
                isolation_claim: "workspace".into(),
                policy_digest: "d".into(),
                branch_ref: "refs/heads/h5i/env/claude/x".into(),
                worktree_known_local: true,
                latest_submission_id: Some("sub1".into()),
                state: "enrolled".into(),
            }],
            submissions: vec![TeamArtifact {
                id: "sub1".into(),
                owner_agent: "claude".into(),
                round: 1,
                env_id: "env/claude/x".into(),
                commit_oid: "c".into(),
                tree_oid: "t".into(),
                capture_ids: vec![],
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                submitted_at: "2026-01-04T00:00:00Z".into(),
                summary: None,
                independent: true,
                influence_event_ids: vec![],
                influence_artifact_ids: vec![],
            }],
            verifications: vec![TeamVerification {
                id: "v1".into(),
                submission_id: "sub1".into(),
                owner_agent: "claude".into(),
                round: 1,
                command: vec!["cargo".into(), "test".into()],
                applies_cleanly: true,
                tests_passed: verified_ok,
                isolation: "supervised".into(),
                capture_id: None,
                failure: None,
                sealed_from: None,
                sealed_tree_oid: None,
                sealed_paths: vec![],
                sealed_overridden: vec![],
            }],
            verdict: verdict.then(|| TeamVerdict {
                selected_submission: Some("sub1".into()),
                method: "tests_then_smallest_diff".into(),
                decided_by: "host".into(),
                can_auto_apply: true,
                reasons: vec![],
            }),
        }
    }

    #[test]
    fn proposed_env_raises_a_decision_item() {
        let m = manifest(env::ST_PROPOSED);
        let items = env_attention(&m, &no_risk(), &[], &[], false, "up-to-date");
        let item = items.iter().find(|i| i.id == "review:env/claude/x").unwrap();
        assert_eq!(item.priority, Priority::Decision);
        assert_eq!(item.state, "blocked");
        assert!(item.commands.iter().any(|c| c.contains("env apply")));
        assert_eq!(item.evidence[0].authority, Authority::Reported);
    }

    #[test]
    fn enforcement_findings_carry_enforced_authority_and_probing_stays_inferred() {
        let m = manifest(env::ST_RUNNING);
        let live = [LiveSession {
            pid: 1,
            kind: "shell".into(),
            started_at: "2026-01-01T01:00:00Z".into(),
            command: None,
        }];
        let enforced =
            env_attention(&m, &critical_risk("egress-denied"), &[], &live, false, "up-to-date");
        let b = enforced.iter().find(|i| i.id.starts_with("boundary:")).unwrap();
        assert_eq!(b.evidence[0].authority, Authority::Enforced);
        assert_eq!(b.occurred_at, "2026-01-03T00:00:00Z");
        assert!(!b.reasons.is_empty(), "reasons are never hidden");

        let probed =
            env_attention(&m, &critical_risk("privilege-tool"), &[], &live, false, "up-to-date");
        let b = probed.iter().find(|i| i.id.starts_with("boundary:")).unwrap();
        assert_eq!(b.evidence[0].authority, Authority::Inferred);
    }

    #[test]
    fn stale_writer_is_critical_and_observed() {
        let m = manifest(env::ST_RUNNING);
        let items = env_attention(&m, &no_risk(), &[], &[], true, "up-to-date");
        let s = items.iter().find(|i| i.id.starts_with("stale-writer:")).unwrap();
        assert_eq!(s.priority, Priority::Critical);
        assert_eq!(s.evidence[0].authority, Authority::Observed);
    }

    #[test]
    fn live_writer_is_active_and_observers_are_not() {
        let m = manifest(env::ST_RUNNING);
        let live = [
            LiveSession {
                pid: 7,
                kind: "shell".into(),
                started_at: "2026-01-01T01:00:00Z".into(),
                command: Some("claude".into()),
            },
            LiveSession {
                pid: 8,
                kind: "observe".into(),
                started_at: "2026-01-01T01:00:00Z".into(),
                command: None,
            },
        ];
        let items = env_attention(&m, &no_risk(), &[], &live, false, "up-to-date");
        let working: Vec<_> = items.iter().filter(|i| i.id.starts_with("working:")).collect();
        assert_eq!(working.len(), 1, "observer sessions raise no item");
        assert_eq!(working[0].priority, Priority::Active);
        assert_eq!(working[0].state, "working");
    }

    #[test]
    fn drift_is_informational_and_suppressed_after_apply() {
        let open = env_attention(
            &manifest(env::ST_RUNNING), &no_risk(), &[], &[], false, "parent-ahead",
        );
        assert!(open.iter().any(|i| i.id.starts_with("drift:")));
        let closed = env_attention(
            &manifest(env::ST_APPLIED), &no_risk(), &[], &[], false, "parent-ahead",
        );
        assert!(!closed.iter().any(|i| i.id.starts_with("drift:")));
    }

    #[test]
    fn team_without_verdict_needs_a_decision_and_decided_team_is_quiet() {
        let pending = team_attention(&run(false, true));
        let d = pending.iter().find(|i| i.id == "decide:r1").unwrap();
        assert_eq!(d.priority, Priority::Decision);
        assert!(d.evidence.iter().any(|e| e.authority == Authority::Verified));
        assert!(d.evidence.iter().any(|e| e.authority == Authority::Reported));

        assert!(team_attention(&run(true, true)).is_empty());
    }

    #[test]
    fn failed_verifier_is_critical_with_verified_authority() {
        let items = team_attention(&run(false, false));
        let f = items.iter().find(|i| i.id == "verify-fail:r1:v1").unwrap();
        assert_eq!(f.priority, Priority::Critical);
        assert_eq!(f.evidence[0].authority, Authority::Verified);
        assert!(f.reasons[0].contains("cargo test"));
    }

    #[test]
    fn latest_passing_verification_supersedes_an_older_failure() {
        let mut current = run(false, false);
        let mut passing = current.verifications[0].clone();
        passing.id = "v2".into();
        passing.tests_passed = true;
        current.verifications.push(passing);

        let items = team_attention(&current);
        assert!(
            items.iter().all(|i| i.priority != Priority::Critical),
            "an older failed re-execution is audit history, not a live blocker"
        );
        let decision = items.iter().find(|i| i.id == "decide:r1").unwrap();
        assert_eq!(
            decision.reasons,
            vec!["1 of 1 candidates verified; finalize records the verdict"]
        );
    }

    #[test]
    fn resubmission_replaces_the_old_candidate_in_attention() {
        let mut current = run(false, false);
        let mut replacement = current.submissions[0].clone();
        replacement.id = "sub2".into();
        replacement.commit_oid = "c2".into();
        replacement.submitted_at = "2026-01-05T00:00:00Z".into();
        current.submissions.push(replacement);
        current.agents[0].latest_submission_id = Some("sub2".into());

        let mut passing = current.verifications[0].clone();
        passing.id = "v2".into();
        passing.submission_id = "sub2".into();
        passing.tests_passed = true;
        current.verifications.push(passing);

        let items = team_attention(&current);
        assert!(items.iter().all(|i| i.priority != Priority::Critical));
        let decision = items.iter().find(|i| i.id == "decide:r1").unwrap();
        assert!(decision.title.contains("1 candidate(s)"));
        assert!(decision.evidence.iter().any(|e| e.id == "sub2"));
        assert!(
            decision
                .evidence
                .iter()
                .all(|e| e.id != "sub1" && e.id != "v1")
        );
    }

    #[test]
    fn decision_waits_for_every_enrolled_agent_to_submit() {
        let mut current = run(false, true);
        let mut waiting = current.agents[0].clone();
        waiting.agent_id = "codex".into();
        waiting.env_id = "env/codex/x".into();
        waiting.latest_submission_id = None;
        current.agents.push(waiting);

        assert!(
            team_attention(&current).iter().all(|i| i.id != "decide:r1"),
            "one early submission is not yet a host decision point"
        );
    }

    #[test]
    fn a_new_round_does_not_project_previous_round_candidates() {
        let mut current = run(false, false);
        current.current_round = 2;

        assert!(
            team_attention(&current).is_empty(),
            "round-one submissions and failures are history once round two opens"
        );
    }

    #[test]
    fn verdict_resolves_failures_from_earlier_attempts() {
        let current = run(true, false);
        assert!(team_attention(&current).is_empty());
    }

    #[test]
    fn no_unread_messages_raises_nothing() {
        assert!(msg_attention(0, "claude", "now").is_none());
        let some = msg_attention(2, "claude", "now").unwrap();
        assert_eq!(some.priority, Priority::Communication);
    }

    #[test]
    fn env_lifecycle_maps_to_workbench_language() {
        assert_eq!(env_work_item(&manifest(env::ST_CREATED), &[]).lifecycle, "draft");
        assert_eq!(env_work_item(&manifest(env::ST_PROPOSED), &[]).lifecycle, "review");
        let wi = env_work_item(&manifest(env::ST_RUNNING), &[]);
        assert_eq!(wi.lifecycle, "working");
        assert_eq!(wi.seats[0].status, "idle");
    }

    #[test]
    fn team_work_item_reports_decided_once_a_verdict_exists() {
        assert_eq!(team_work_item(&run(false, true)).lifecycle, "review");
        assert_eq!(team_work_item(&run(true, true)).lifecycle, "decided");
        assert_eq!(team_work_item(&run(false, true)).updated_at, "2026-01-04T00:00:00Z");
    }

    #[test]
    fn seen_cursor_drains_and_rearms_on_a_newer_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut items = env_attention(
            &manifest(env::ST_PROPOSED), &no_risk(), &[], &[], false, "up-to-date",
        );
        // Unseen before any cursor exists.
        apply_seen(&mut items, root, "host", "2026-01-05T00:00:00Z");
        assert!(items[0].seen_at.is_none());

        // Marking seen drains it…
        mark_seen(root, "host", &items, None).unwrap();
        apply_seen(&mut items, root, "host", "2026-01-05T00:01:00Z");
        assert!(items[0].seen_at.is_some());

        // …and a newer watermark re-arms it.
        items[0].occurred_at = "2026-09-09T00:00:00Z".into();
        items[0].seen_at = None;
        apply_seen(&mut items, root, "host", "2026-01-05T00:02:00Z");
        assert!(items[0].seen_at.is_none(), "new evidence re-raises the flag");
    }

    #[test]
    fn mark_seen_can_target_a_subset() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut items = env_attention(
            &manifest(env::ST_PROPOSED), &no_risk(), &[], &[], true, "up-to-date",
        );
        assert!(items.len() >= 2);
        let only = vec![items[0].id.clone()];
        let marked = mark_seen(root, "host", &items, Some(&only)).unwrap();
        assert_eq!(marked, 1);
        apply_seen(&mut items, root, "host", "now");
        assert!(items[0].seen_at.is_some());
        assert!(items[1].seen_at.is_none());
    }

    #[test]
    fn hostile_identity_cannot_escape_the_attention_dir() {
        let p = cursor_path(Path::new("/root"), "../../etc/passwd");
        assert!(p.starts_with("/root/attention"));
        assert!(!p.to_string_lossy().contains(".."));
    }
}
