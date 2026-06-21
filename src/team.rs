//! `h5i team` — phased, Git-backed evidence publication over existing envs.
//!
//! P0 is intentionally manual: a team groups already-created `h5i env`s,
//! snapshots submissions as immutable commit/tree/capture pointers, freezes the
//! run, and compares candidates. Later phases can add dispatch, discussion,
//! verification, finalization, and apply on top of the same event log.

use crate::env;
use crate::error::H5iError;
use crate::msg;
use crate::objects;
use crate::sandbox;
use crate::token_filter::{FilterConfig, OutputKind};
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const TEAM_REF_PREFIX: &str = "refs/h5i/team/";
const EVENTS_FILE: &str = "events.jsonl";
const MAX_ATTEMPTS: usize = 64;

pub const PHASE_DRAFT: &str = "draft";
pub const PHASE_SEALED_SUBMIT: &str = "sealed_submit";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamEvent {
    pub id: String,
    pub ts: String,
    pub actor: String,
    pub kind: String,
    pub run_id: String,
    #[serde(default)]
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_after: Option<String>,
    pub idempotency_key: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamAgent {
    pub agent_id: String,
    pub display_label: String,
    pub env_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub isolation_claim: String,
    pub policy_digest: String,
    pub branch_ref: String,
    pub worktree_known_local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_submission_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamArtifact {
    pub id: String,
    pub owner_agent: String,
    pub round: u32,
    pub env_id: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub capture_ids: Vec<String>,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub independent: bool,
    #[serde(default)]
    pub influence_event_ids: Vec<String>,
    #[serde(default)]
    pub influence_artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamVerification {
    pub id: String,
    pub submission_id: String,
    pub owner_agent: String,
    pub round: u32,
    pub command: Vec<String>,
    pub applies_cleanly: bool,
    pub tests_passed: bool,
    /// The isolation tier the verifier command actually ran under
    /// (`workspace`/`process`/`supervised`/`container`) — audit of how
    /// sandboxed the neutral re-execution really was.
    #[serde(default = "isolation_unknown")]
    pub isolation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

fn isolation_unknown() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamVerdict {
    pub selected_submission: Option<String>,
    pub method: String,
    pub decided_by: String,
    pub can_auto_apply: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamApplyResult {
    pub submission_id: String,
    pub source_commit_oid: String,
    pub target_commit_oid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamWorkerReport {
    pub worker_id: String,
    pub inspected: usize,
    pub finalized: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamRun {
    pub id: String,
    pub name: String,
    pub base_oid: String,
    pub created_by: String,
    pub created_at: String,
    pub phase: String,
    pub current_round: u32,
    pub max_rounds: u32,
    pub agents: Vec<TeamAgent>,
    pub submissions: Vec<TeamArtifact>,
    #[serde(default)]
    pub verifications: Vec<TeamVerification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<TeamVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamCompareRow {
    pub agent_id: String,
    pub env_id: String,
    pub submitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    pub status: String,
    pub base_commit: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub last_exit: Option<i32>,
    pub last_tool: Option<String>,
    pub last_result: Option<String>,
    pub last_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamGrant {
    pub reviewer: String,
    pub target: String,
    pub round: u32,
    pub artifact_kinds: Vec<String>,
    pub artifact_ids: Vec<String>,
    pub phase_bound: String,
    pub granted_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamReview {
    pub reviewer: String,
    pub target: String,
    pub round: u32,
    pub body: String,
    #[serde(default)]
    pub referenced_artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamDiscussion {
    pub thread_id: String,
    pub sender: String,
    pub recipients: Vec<String>,
    pub round: u32,
    pub body: String,
    #[serde(default)]
    pub referenced_artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamStatus {
    pub run: TeamRun,
    pub events: Vec<TeamEvent>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn new_event_id(kind: &str, idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"\0");
    h.update(idempotency_key.as_bytes());
    h.update(b"\0");
    h.update(now().as_bytes());
    let hex = format!("{:x}", h.finalize());
    hex[..16].to_string()
}

pub fn validate_slug(slug: &str) -> Result<(), H5iError> {
    env::validate_slug(slug)
}

pub fn validate_agent_id(agent_id: &str) -> Result<(), H5iError> {
    env::validate_agent(agent_id)
}

// ── Current-team context (a local, per-clone convenience like git's HEAD) ─────
// Lets `h5i team <verb>` omit the run id; the flat CLI stays canonical and
// scriptable. Stored as a plain file under the on-disk h5i root — NOT in a ref
// (it is a local pointer, never shared).

/// Path of the per-clone "current team" pointer.
pub fn current_path(h5i_root: &Path) -> PathBuf {
    h5i_root.join("team").join("current")
}

/// The current team id, if one is set (and non-empty).
pub fn get_current(h5i_root: &Path) -> Option<String> {
    std::fs::read_to_string(current_path(h5i_root))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Pin `run_id` as the current team.
pub fn set_current(h5i_root: &Path, run_id: &str) -> Result<(), H5iError> {
    let p = current_path(h5i_root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(&p, format!("{run_id}\n")).map_err(|e| H5iError::with_path(e, &p))
}

/// Clear the current-team pointer (no-op if unset).
pub fn clear_current(h5i_root: &Path) -> Result<(), H5iError> {
    let p = current_path(h5i_root);
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| H5iError::with_path(e, &p))?;
    }
    Ok(())
}

/// Resolve a team id from an explicit arg, falling back to the current team.
/// Errors if neither is available.
pub fn resolve_run(h5i_root: &Path, arg: Option<String>) -> Result<String, H5iError> {
    match arg {
        Some(t) if !t.trim().is_empty() => Ok(t),
        _ => get_current(h5i_root).ok_or_else(|| {
            H5iError::Metadata(
                "no team given and no current team set — pass the team or run `h5i team use <name>`"
                    .into(),
            )
        }),
    }
}

fn refname(run_id: &str) -> Result<String, H5iError> {
    validate_slug(run_id)?;
    Ok(format!("{TEAM_REF_PREFIX}{run_id}"))
}

#[allow(clippy::too_many_arguments)]
fn event(
    run_id: &str,
    actor: &str,
    kind: &str,
    round: u32,
    phase_before: Option<String>,
    phase_after: Option<String>,
    idempotency_key: String,
    payload: serde_json::Value,
) -> TeamEvent {
    TeamEvent {
        id: new_event_id(kind, &idempotency_key),
        ts: now(),
        actor: actor.to_string(),
        kind: kind.to_string(),
        run_id: run_id.to_string(),
        round,
        parent_event_id: None,
        phase_before,
        phase_after,
        idempotency_key,
        payload,
    }
}

pub fn append_event(repo: &Repository, ev: &TeamEvent) -> Result<(), H5iError> {
    let refname = refname(&ev.run_id)?;
    let line = serde_json::to_string(ev)?;
    let message = format!("h5i team {}: {}", ev.run_id, ev.kind);

    for _ in 0..MAX_ATTEMPTS {
        let tip = repo.refname_to_id(&refname).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let mut log =
            objects::read_blob_from_tree(repo, base_tree.as_ref(), EVENTS_FILE).unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&line);
        log.push('\n');

        let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &[(EVENTS_FILE, &log)])?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = objects::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        let cas_ok = match tip {
            None => repo.reference(&refname, new_oid, false, &message).is_ok(),
            Some(old) => repo
                .reference_matching(&refname, new_oid, true, old, &message)
                .is_ok(),
        };
        if cas_ok {
            return Ok(());
        }
    }

    Err(H5iError::Internal(format!(
        "h5i team: event {} for {} could not be appended after {MAX_ATTEMPTS} attempts",
        ev.kind, ev.run_id
    )))
}

pub fn read_events(repo: &Repository, run_id: &str) -> Result<Vec<TeamEvent>, H5iError> {
    let refname = refname(run_id)?;
    let reference = repo
        .find_reference(&refname)
        .map_err(|_| H5iError::Metadata(format!("no team named '{run_id}'")))?;
    let commit = repo.find_commit(
        reference
            .target()
            .ok_or_else(|| H5iError::Metadata(format!("{refname} has no target")))?,
    )?;
    let tree = commit.tree()?;
    let raw = objects::read_blob_from_tree(repo, Some(&tree), EVENTS_FILE).unwrap_or_default();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let ev: TeamEvent = serde_json::from_str(line)?;
        if seen.insert(ev.id.clone()) {
            out.push(ev);
        }
    }
    out.sort_by(|a, b| {
        a.parent_event_id
            .cmp(&b.parent_event_id)
            .then(a.ts.cmp(&b.ts))
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

pub fn list(repo: &Repository) -> Result<Vec<TeamRun>, H5iError> {
    let mut out = Vec::new();
    let refs = repo.references()?;
    for r in refs.flatten() {
        let Some(name) = r.name() else {
            continue;
        };
        let Some(run_id) = name.strip_prefix(TEAM_REF_PREFIX) else {
            continue;
        };
        if validate_slug(run_id).is_err() {
            continue;
        }
        if let Ok(status) = status(repo, run_id) {
            out.push(status.run);
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

pub fn status(repo: &Repository, run_id: &str) -> Result<TeamStatus, H5iError> {
    let events = read_events(repo, run_id)?;
    let run = project(run_id, &events)?;
    Ok(TeamStatus { run, events })
}

fn project(run_id: &str, events: &[TeamEvent]) -> Result<TeamRun, H5iError> {
    let create = events
        .iter()
        .find(|e| e.kind == "created")
        .ok_or_else(|| H5iError::Metadata(format!("team '{run_id}' has no created event")))?;
    let name = create
        .payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(run_id)
        .to_string();
    let base_oid = create
        .payload
        .get("base_oid")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let max_rounds = create
        .payload
        .get("max_rounds")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    let mut phase = PHASE_DRAFT.to_string();
    let mut current_round = 1;
    let mut agents: BTreeMap<String, TeamAgent> = BTreeMap::new();
    let mut submissions: BTreeMap<String, TeamArtifact> = BTreeMap::new();
    let mut verifications: BTreeMap<String, TeamVerification> = BTreeMap::new();
    let mut verdict: Option<TeamVerdict> = None;

    for ev in events {
        if ev.run_id != run_id {
            continue;
        }
        if let Some(after) = ev.phase_after.as_deref() {
            phase = after.to_string();
        }
        if ev.round > current_round {
            current_round = ev.round;
        }
        match ev.kind.as_str() {
            "agent_added" => {
                let agent: TeamAgent = serde_json::from_value(ev.payload.clone())?;
                agents.insert(agent.agent_id.clone(), agent);
            }
            "submitted" => {
                let artifact: TeamArtifact = serde_json::from_value(ev.payload.clone())?;
                submissions.insert(artifact.id.clone(), artifact);
            }
            "verified" => {
                let verification: TeamVerification = serde_json::from_value(ev.payload.clone())?;
                verifications.insert(verification.id.clone(), verification);
            }
            "verdict" | "no_verdict" => {
                verdict = Some(serde_json::from_value(ev.payload.clone())?);
            }
            _ => {}
        }
    }

    for agent in agents.values_mut() {
        if let Some(sub) = submissions
            .values()
            .filter(|s| s.owner_agent == agent.agent_id)
            .max_by(|a, b| a.round.cmp(&b.round).then(a.id.cmp(&b.id)))
        {
            agent.latest_submission_id = Some(sub.id.clone());
            agent.state = "submitted".into();
        }
    }

    Ok(TeamRun {
        id: run_id.to_string(),
        name,
        base_oid,
        created_by: create.actor.clone(),
        created_at: create.ts.clone(),
        phase,
        current_round,
        max_rounds,
        agents: agents.into_values().collect(),
        submissions: submissions.into_values().collect(),
        verifications: verifications.into_values().collect(),
        verdict,
    })
}

pub fn create(
    repo: &Repository,
    run_id: &str,
    name: &str,
    base: &str,
    max_rounds: u32,
    actor: &str,
) -> Result<TeamRun, H5iError> {
    validate_slug(run_id)?;
    if repo.find_reference(&refname(run_id)?).is_ok() {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' already exists"
        )));
    }
    let base_obj = repo.revparse_single(base)?;
    let base_commit = base_obj.peel_to_commit()?;
    let ev = event(
        run_id,
        actor,
        "created",
        1,
        None,
        Some(PHASE_DRAFT.to_string()),
        format!("created:{run_id}"),
        serde_json::json!({
            "name": name,
            "base_oid": base_commit.id().to_string(),
            "max_rounds": max_rounds.max(1),
        }),
    );
    append_event(repo, &ev)?;
    Ok(status(repo, run_id)?.run)
}

#[allow(clippy::too_many_arguments)]
pub fn add_env(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    env_name: &str,
    agent_id: &str,
    runtime: Option<String>,
    model: Option<String>,
    role: Option<String>,
    actor: &str,
) -> Result<TeamRun, H5iError> {
    validate_agent_id(agent_id)?;
    let current = status(repo, run_id)?.run;
    if current.phase != PHASE_DRAFT {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' is in phase '{}' — add-env is only allowed in draft",
            current.phase
        )));
    }
    if current.agents.iter().any(|a| a.agent_id == agent_id) {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' already has agent '{agent_id}'"
        )));
    }
    let m = env::find(h5i_root, env_name)?;
    let agent = TeamAgent {
        agent_id: agent_id.to_string(),
        display_label: role.clone().unwrap_or_else(|| agent_id.to_string()),
        env_id: m.id.clone(),
        runtime,
        model,
        role,
        isolation_claim: m.isolation_claim.clone(),
        policy_digest: m.policy_digest.clone(),
        branch_ref: m.branch.clone(),
        worktree_known_local: m.work_dir(h5i_root).exists(),
        latest_submission_id: None,
        state: "working".into(),
    };
    let ev = event(
        run_id,
        actor,
        "agent_added",
        current.current_round,
        Some(current.phase),
        None,
        format!("agent_added:{run_id}:{agent_id}"),
        serde_json::to_value(agent)?,
    );
    append_event(repo, &ev)?;
    // Bind the env to this team persona. env run/shell reads these host-owned
    // files and injects H5I_AGENT/H5I_TEAM for scoped in-box requests.
    let env_dir = m.dir(h5i_root);
    let identity_path = env_dir.join("team-identity");
    std::fs::write(&identity_path, format!("{agent_id}\n"))
        .map_err(|e| H5iError::with_path(e, &identity_path))?;
    let team_path = env_dir.join("team-run");
    std::fs::write(&team_path, format!("{run_id}\n"))
        .map_err(|e| H5iError::with_path(e, &team_path))?;
    Ok(status(repo, run_id)?.run)
}

pub fn submit(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    agent_id: &str,
    commit: Option<&str>,
    summary: Option<String>,
    actor: &str,
) -> Result<TeamArtifact, H5iError> {
    let current = status(repo, run_id)?.run;
    if current.phase != PHASE_DRAFT
        && current.phase != PHASE_SEALED_SUBMIT
        && current.phase != "discuss"
    {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' is in phase '{}' — submit is only allowed before compare/review",
            current.phase
        )));
    }
    let agent = current
        .agents
        .iter()
        .find(|a| a.agent_id == agent_id)
        .ok_or_else(|| H5iError::Metadata(format!("team '{run_id}' has no agent '{agent_id}'")))?;
    let m = env::find(h5i_root, &agent.env_id)?;
    let commit_oid = match commit {
        Some(c) => repo.revparse_single(c)?.peel_to_commit()?.id(),
        None => repo.refname_to_id(&m.branch)?,
    };
    let commit_obj = repo.find_commit(commit_oid)?;
    let tree_oid = commit_obj.tree_id();
    let env_rows = env::compare(repo, h5i_root, std::slice::from_ref(&m.id))?;
    let row = env_rows
        .first()
        .ok_or_else(|| H5iError::Internal("env compare returned no row".into()))?;
    let id = format!(
        "sub-{}-r{}-{}",
        agent_id,
        current.current_round,
        &commit_oid.to_string()[..12]
    );
    let events = read_events(repo, run_id)?;
    let mut influence_event_ids = Vec::new();
    let mut influence_artifact_ids = Vec::new();
    for ev in &events {
        if ev.kind != "discussion_msg" || ev.round != current.current_round {
            continue;
        }
        let recipients = ev
            .payload
            .get("recipients")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if recipients.iter().any(|r| r.as_str() == Some(agent_id)) {
            influence_event_ids.push(ev.id.clone());
            if let Some(ids) = ev
                .payload
                .get("referenced_artifact_ids")
                .and_then(|v| v.as_array())
            {
                for id in ids.iter().filter_map(|v| v.as_str()) {
                    influence_artifact_ids.push(id.to_string());
                }
            }
        }
    }
    influence_artifact_ids.sort();
    influence_artifact_ids.dedup();
    let artifact = TeamArtifact {
        id,
        owner_agent: agent_id.to_string(),
        round: current.current_round,
        env_id: m.id.clone(),
        commit_oid: commit_oid.to_string(),
        tree_oid: tree_oid.to_string(),
        capture_ids: m.captures.clone(),
        files_changed: row.files_changed,
        insertions: row.insertions,
        deletions: row.deletions,
        summary,
        independent: influence_event_ids.is_empty(),
        influence_event_ids,
        influence_artifact_ids,
    };
    let ev = event(
        run_id,
        actor,
        "submitted",
        current.current_round,
        Some(current.phase),
        None,
        format!("submitted:{run_id}:{agent_id}:{}", artifact.commit_oid),
        serde_json::to_value(&artifact)?,
    );
    append_event(repo, &ev)?;
    Ok(artifact)
}

pub fn freeze(
    repo: &Repository,
    run_id: &str,
    allow_missing: bool,
    actor: &str,
) -> Result<TeamRun, H5iError> {
    let current = status(repo, run_id)?.run;
    if current.phase != PHASE_DRAFT {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' is in phase '{}' — freeze is only allowed in draft",
            current.phase
        )));
    }
    let submitted: BTreeSet<&str> = current
        .submissions
        .iter()
        .map(|s| s.owner_agent.as_str())
        .collect();
    let missing: Vec<&str> = current
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .filter(|id| !submitted.contains(id))
        .collect();
    if !missing.is_empty() && !allow_missing {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' has missing submissions: {} (use --allow-missing to record a partial freeze)",
            missing.join(", ")
        )));
    }
    let ev = event(
        run_id,
        actor,
        "frozen",
        current.current_round,
        Some(current.phase),
        Some(PHASE_SEALED_SUBMIT.to_string()),
        format!("frozen:{run_id}:{}", current.current_round),
        serde_json::json!({ "missing_agents": missing }),
    );
    append_event(repo, &ev)?;
    Ok(status(repo, run_id)?.run)
}

pub fn compare(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
) -> Result<Vec<TeamCompareRow>, H5iError> {
    let current = status(repo, run_id)?.run;
    let names: Vec<String> = current.agents.iter().map(|a| a.env_id.clone()).collect();
    let env_rows = env::compare(repo, h5i_root, &names)?;
    let by_env: BTreeMap<String, env::CompareRow> =
        env_rows.into_iter().map(|r| (r.id.clone(), r)).collect();
    let latest_by_agent: BTreeMap<String, &TeamArtifact> = current
        .submissions
        .iter()
        .map(|s| (s.owner_agent.clone(), s))
        .collect();
    let mut out = Vec::new();
    for agent in &current.agents {
        let row = by_env
            .get(&agent.env_id)
            .ok_or_else(|| H5iError::Metadata(format!("missing env row for {}", agent.env_id)))?;
        let sub = latest_by_agent.get(&agent.agent_id).copied();
        out.push(TeamCompareRow {
            agent_id: agent.agent_id.clone(),
            env_id: agent.env_id.clone(),
            submitted: sub.is_some(),
            submission_id: sub.map(|s| s.id.clone()),
            status: row.status.clone(),
            base_commit: row.base_commit.clone(),
            files_changed: row.files_changed,
            insertions: row.insertions,
            deletions: row.deletions,
            last_exit: row.last_exit,
            last_tool: row.last_tool.clone(),
            last_result: row.last_result.clone(),
            last_counts: row.last_counts.clone(),
        });
    }
    Ok(out)
}

pub fn dispatch(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    prompt: &str,
    actor: &str,
) -> Result<Vec<msg::Message>, H5iError> {
    let current = status(repo, run_id)?.run;
    let mut sent = Vec::new();
    for agent in &current.agents {
        let message = msg::send_msg(
            repo,
            h5i_root,
            actor,
            &agent.agent_id,
            prompt,
            msg::SendOpts {
                kind: Some("ASK".into()),
                links: Some(serde_json::json!({
                    "team": run_id,
                    "round": current.current_round,
                    "agent_id": agent.agent_id,
                })),
                ..Default::default()
            },
        )?;
        sent.push(message);
    }
    let ev = event(
        run_id,
        actor,
        "dispatched",
        current.current_round,
        Some(current.phase),
        Some("dispatched".into()),
        format!("dispatched:{run_id}:{}:{}", current.current_round, prompt),
        serde_json::json!({
            "message_ids": sent.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "agent_ids": sent.iter().map(|m| m.to.clone()).collect::<Vec<_>>(),
        }),
    );
    append_event(repo, &ev)?;
    Ok(sent)
}

pub fn grant_review(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    reviewer: &str,
    target: &str,
    artifact_kinds: Vec<String>,
    actor: &str,
) -> Result<TeamGrant, H5iError> {
    validate_agent_id(reviewer)?;
    validate_agent_id(target)?;
    let current = status(repo, run_id)?.run;
    if !current.agents.iter().any(|a| a.agent_id == reviewer) {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' has no reviewer '{reviewer}'"
        )));
    }
    if !current.agents.iter().any(|a| a.agent_id == target) {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' has no target '{target}'"
        )));
    }
    let allowed: BTreeSet<&str> = ["diff", "summary", "tests", "test-status"]
        .into_iter()
        .collect();
    for k in &artifact_kinds {
        if !allowed.contains(k.as_str()) {
            return Err(H5iError::Metadata(format!(
                "artifact kind '{k}' is not grantable in P1 (allowed: diff, summary, tests, test-status)"
            )));
        }
    }
    let artifact_ids: Vec<String> = current
        .submissions
        .iter()
        .filter(|s| s.owner_agent == target && s.round == current.current_round)
        .map(|s| s.id.clone())
        .collect();
    if artifact_ids.is_empty() {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' has no round {} submission for target '{target}'",
            current.current_round
        )));
    }
    let body = format!(
        "Review {target}'s team submission for {run_id}. Artifacts granted: {}. Artifact ids: {}.",
        artifact_kinds.join(","),
        artifact_ids.join(",")
    );
    let message = msg::send_msg(
        repo,
        h5i_root,
        actor,
        reviewer,
        &body,
        msg::SendOpts {
            kind: Some("REVIEW_REQUEST".into()),
            focus: artifact_ids.clone(),
            links: Some(serde_json::json!({
                "team": run_id,
                "round": current.current_round,
                "reviewer": reviewer,
                "target": target,
                "artifact_ids": artifact_ids,
                "artifact_kinds": artifact_kinds,
            })),
            ..Default::default()
        },
    )?;
    let grant = TeamGrant {
        reviewer: reviewer.into(),
        target: target.into(),
        round: current.current_round,
        artifact_kinds,
        artifact_ids,
        phase_bound: current.phase.clone(),
        granted_by: actor.into(),
        message_id: Some(message.id),
    };
    let ev = event(
        run_id,
        actor,
        "review_granted",
        current.current_round,
        Some(current.phase),
        None,
        format!(
            "review_granted:{run_id}:{reviewer}:{target}:{}",
            grant.round
        ),
        serde_json::to_value(&grant)?,
    );
    append_event(repo, &ev)?;
    Ok(grant)
}

pub fn submit_review(
    repo: &Repository,
    run_id: &str,
    reviewer: &str,
    target: &str,
    body: String,
    actor: &str,
) -> Result<TeamReview, H5iError> {
    validate_agent_id(reviewer)?;
    validate_agent_id(target)?;
    let current = status(repo, run_id)?.run;
    let referenced_artifacts: Vec<String> = current
        .submissions
        .iter()
        .filter(|s| s.owner_agent == target && s.round == current.current_round)
        .map(|s| s.id.clone())
        .collect();
    let review = TeamReview {
        reviewer: reviewer.into(),
        target: target.into(),
        round: current.current_round,
        body,
        referenced_artifacts,
    };
    let ev = event(
        run_id,
        actor,
        "review_submitted",
        current.current_round,
        Some(current.phase),
        None,
        format!(
            "review_submitted:{run_id}:{reviewer}:{target}:{}",
            current.current_round
        ),
        serde_json::to_value(&review)?,
    );
    append_event(repo, &ev)?;
    Ok(review)
}

#[allow(clippy::too_many_arguments)]
pub fn discuss(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    sender: &str,
    recipients: Vec<String>,
    body: String,
    referenced_artifact_ids: Vec<String>,
    actor: &str,
) -> Result<TeamDiscussion, H5iError> {
    validate_agent_id(sender)?;
    if recipients.is_empty() {
        return Err(H5iError::Metadata(
            "team discuss requires at least one recipient".into(),
        ));
    }
    for r in &recipients {
        validate_agent_id(r)?;
    }
    let current = status(repo, run_id)?.run;
    // Independence-first (invariant 1): discussion may only happen AFTER the run
    // is frozen, so every agent's first attempt is sealed and independent before
    // any cross-agent influence is possible. A discuss in `draft` would let
    // agents contaminate each other before any independent submission exists.
    if current.phase != PHASE_SEALED_SUBMIT && current.phase != "discuss" {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' is in phase '{}' — discussion is only allowed after `h5i team freeze` \
             (sealed_submit), so the first attempt stays independent",
            current.phase
        )));
    }
    if !current.agents.iter().any(|a| a.agent_id == sender) {
        return Err(H5iError::Metadata(format!(
            "team '{run_id}' has no sender '{sender}'"
        )));
    }
    for r in &recipients {
        if !current.agents.iter().any(|a| a.agent_id == *r) {
            return Err(H5iError::Metadata(format!(
                "team '{run_id}' has no recipient '{r}'"
            )));
        }
    }
    let thread_id = format!("team-{run_id}-r{}-discussion", current.current_round);
    let mut message_ids = Vec::new();
    for recipient in &recipients {
        let message = msg::send_msg(
            repo,
            h5i_root,
            sender,
            recipient,
            &body,
            msg::SendOpts {
                kind: Some("ASK".into()),
                thread_id: Some(thread_id.clone()),
                focus: referenced_artifact_ids.clone(),
                links: Some(serde_json::json!({
                    "team": run_id,
                    "round": current.current_round,
                    "discussion": true,
                    "referenced_artifact_ids": referenced_artifact_ids.clone(),
                })),
                ..Default::default()
            },
        )?;
        message_ids.push(message.id);
    }
    let discussion = TeamDiscussion {
        thread_id,
        sender: sender.into(),
        recipients,
        round: current.current_round,
        body,
        referenced_artifact_ids,
        message_id: message_ids.first().cloned(),
    };
    let ev = event(
        run_id,
        actor,
        "discussion_msg",
        current.current_round,
        Some(current.phase),
        Some("discuss".into()),
        format!(
            "discussion_msg:{run_id}:{}:{}",
            discussion.sender,
            message_ids.join(",")
        ),
        serde_json::to_value(&discussion)?,
    );
    append_event(repo, &ev)?;
    Ok(discussion)
}

fn run_git(repo_workdir: &Path, args: &[&str]) -> Result<std::process::Output, H5iError> {
    Command::new("git")
        .arg("-C")
        .arg(repo_workdir)
        .args(args)
        .output()
        .map_err(H5iError::from)
}

/// Resolve the confinement the neutral verifier runs the candidate command under:
/// the fail-closed `default` build/test profile at the requested tier (or, when
/// `requested` is None/`auto`, the strongest tier this host can actually enforce).
/// If the chosen kernel tier isn't runnable here (e.g. AppArmor-restricted userns
/// on CI), fall back to the unconfined `workspace` tier rather than failing the
/// whole verification — the returned tier name records what really happened.
fn verifier_policy(
    repo_workdir: &Path,
    requested: Option<&str>,
) -> Result<(sandbox::ResolvedPolicy, String), H5iError> {
    let caps = sandbox::probe_host();
    let claim = match requested {
        Some(s) if !s.is_empty() && !s.eq_ignore_ascii_case("auto") => {
            sandbox::IsolationClaim::parse(s)?
        }
        _ => sandbox::effective_auto(repo_workdir, "default", false)
            .unwrap_or(sandbox::IsolationClaim::Workspace),
    };
    let profile = sandbox::load_profile(repo_workdir, "default", Some(claim))?;
    let policy = sandbox::resolve(&profile, &caps)?;
    // Bits present != confinement can exec. If the kernel tier can't actually run
    // here, drop to workspace so verification still completes (and is labeled so).
    if policy.claim != sandbox::IsolationClaim::Workspace && sandbox::verify_exec(&policy).is_err() {
        let profile = sandbox::load_profile(
            repo_workdir,
            "default",
            Some(sandbox::IsolationClaim::Workspace),
        )?;
        let policy = sandbox::resolve(&profile, &caps)?;
        let claim = policy.claim.as_str().to_string();
        return Ok((policy, claim));
    }
    let claim = policy.claim.as_str().to_string();
    Ok((policy, claim))
}

pub fn verify(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    agent_id: &str,
    command: Vec<String>,
    isolation: Option<&str>,
    actor: &str,
) -> Result<TeamVerification, H5iError> {
    if command.is_empty() {
        return Err(H5iError::Metadata("team verify requires a command".into()));
    }
    let current = status(repo, run_id)?.run;
    let submission = current
        .submissions
        .iter()
        .filter(|s| s.owner_agent == agent_id)
        .max_by(|a, b| a.round.cmp(&b.round).then(a.id.cmp(&b.id)))
        .ok_or_else(|| {
            H5iError::Metadata(format!(
                "team '{run_id}' has no submission for agent '{agent_id}'"
            ))
        })?
        .clone();
    let repo_workdir = repo
        .workdir()
        .ok_or_else(|| H5iError::Metadata("team verify requires a non-bare repository".into()))?;
    let verify_root = h5i_root.join("team").join(run_id).join("verify");
    std::fs::create_dir_all(&verify_root).map_err(|e| H5iError::with_path(e, &verify_root))?;
    let verify_dir = verify_root.join(&submission.id);
    if verify_dir.exists() {
        let _ = run_git(
            repo_workdir,
            &[
                "worktree",
                "remove",
                "--force",
                verify_dir.to_string_lossy().as_ref(),
            ],
        );
        if verify_dir.exists() {
            std::fs::remove_dir_all(&verify_dir)
                .map_err(|e| H5iError::with_path(e, &verify_dir))?;
        }
    }

    let mut failure = None;
    let add = run_git(
        repo_workdir,
        &[
            "worktree",
            "add",
            "--detach",
            verify_dir.to_string_lossy().as_ref(),
            &current.base_oid,
        ],
    )?;
    let mut applies_cleanly = add.status.success();
    if !applies_cleanly {
        failure = Some(String::from_utf8_lossy(&add.stderr).trim().to_string());
    }

    if applies_cleanly {
        let pick = run_git(
            &verify_dir,
            &["cherry-pick", "--no-commit", &submission.commit_oid],
        )?;
        applies_cleanly = pick.status.success();
        if !applies_cleanly {
            let mut msg = String::from_utf8_lossy(&pick.stderr).trim().to_string();
            if msg.is_empty() {
                msg = String::from_utf8_lossy(&pick.stdout).trim().to_string();
            }
            failure = Some(msg);
        }
    }

    let mut tests_passed = false;
    let mut capture_id = None;
    // The tier the verifier actually ran under (recorded for audit). When the
    // candidate doesn't apply we never execute, so it stays "skipped".
    let mut isolation_used = "skipped".to_string();
    if applies_cleanly {
        // Run the verifier under fail-closed build/test confinement (the `default`
        // profile) scoped to the throwaway verify worktree — never on the bare
        // host. The tier is the requested one (or the strongest the host can
        // enforce), with a graceful fall-back to the unconfined workspace tier so
        // a verifier still runs on a host without kernel confinement (CI/macOS).
        let (policy, claim) = verifier_policy(repo_workdir, isolation)?;
        isolation_used = claim;
        let exec = sandbox::run(&policy, &verify_dir, &command)?;
        tests_passed = exec.exit_code == Some(0) && !exec.timed_out;
        if exec.timed_out {
            failure = Some("verifier command exceeded the policy wall-clock limit".into());
        }
        let mut raw = Vec::with_capacity(exec.stdout.len() + exec.stderr.len() + 32);
        raw.extend_from_slice(&exec.stdout);
        if !exec.stderr.is_empty() {
            if !raw.is_empty() && !raw.ends_with(b"\n") {
                raw.push(b'\n');
            }
            raw.extend_from_slice(b"\n--- stderr ---\n");
            raw.extend_from_slice(&exec.stderr);
        }
        let cmd_string = command.join(" ");
        let outcome = objects::capture(
            repo,
            h5i_root,
            &raw,
            objects::CaptureOptions {
                kind: OutputKind::Auto,
                cmd: Some(cmd_string),
                cwd: Some(verify_dir.to_string_lossy().to_string()),
                exit_code: exec.exit_code,
                git_tree: Some(submission.tree_oid.clone()),
                files: Vec::new(),
                cmd_argv: command.clone(),
                filter: FilterConfig {
                    cmd: Some(command.clone()),
                    ..Default::default()
                },
                env_id: Some(format!("team/{run_id}/{}", submission.id)),
                policy_digest: None,
                evidence_source: Some(format!("team-verifier:{isolation_used}")),
                egress: exec.egress.clone(),
                redact: false,
            },
        )?;
        capture_id = Some(outcome.manifest.id);
    }
    let _ = run_git(
        repo_workdir,
        &[
            "worktree",
            "remove",
            "--force",
            verify_dir.to_string_lossy().as_ref(),
        ],
    );

    let verification = TeamVerification {
        id: format!(
            "ver-{}-{}",
            submission.id,
            now().replace([':', '.', '-'], "")
        ),
        submission_id: submission.id.clone(),
        owner_agent: submission.owner_agent.clone(),
        round: submission.round,
        command,
        applies_cleanly,
        tests_passed,
        isolation: isolation_used,
        capture_id,
        failure,
    };
    let ev = event(
        run_id,
        actor,
        "verified",
        current.current_round,
        Some(current.phase),
        Some("verified".into()),
        format!("verified:{run_id}:{}", verification.id),
        serde_json::to_value(&verification)?,
    );
    append_event(repo, &ev)?;
    Ok(verification)
}

pub fn finalize(repo: &Repository, run_id: &str, actor: &str) -> Result<TeamVerdict, H5iError> {
    let current = status(repo, run_id)?.run;
    let mut latest: BTreeMap<String, &TeamVerification> = BTreeMap::new();
    for v in &current.verifications {
        latest.insert(v.submission_id.clone(), v);
    }
    let mut eligible: Vec<(&TeamArtifact, &TeamVerification)> = current
        .submissions
        .iter()
        .filter_map(|s| latest.get(&s.id).map(|v| (s, *v)))
        .filter(|(_, v)| v.applies_cleanly && v.tests_passed)
        .collect();
    eligible.sort_by(|(a, _), (b, _)| {
        a.files_changed
            .cmp(&b.files_changed)
            .then(a.insertions.cmp(&b.insertions))
            .then(a.deletions.cmp(&b.deletions))
            .then(a.id.cmp(&b.id))
    });
    const METHOD: &str = "rule:VerifierTestsPass,AppliesCleanly,SmallestDiff";
    // Anti-gaming: a verdict is only apples-to-apples if every eligible candidate
    // was judged by the SAME verifier command. Otherwise one candidate could be
    // waved through with a weaker command (e.g. `true`) than its rivals. Refuse to
    // pick a winner across divergent commands rather than crown a gamed candidate.
    let divergent_command = eligible
        .iter()
        .any(|(_, v)| v.command != eligible[0].1.command);
    let verdict = if eligible.is_empty() {
        TeamVerdict {
            selected_submission: None,
            method: METHOD.into(),
            decided_by: "team-policy".into(),
            can_auto_apply: false,
            reasons: vec!["no candidate has passing verifier evidence".into()],
        }
    } else if divergent_command {
        let commands: BTreeSet<String> =
            eligible.iter().map(|(_, v)| v.command.join(" ")).collect();
        TeamVerdict {
            selected_submission: None,
            method: METHOD.into(),
            decided_by: "team-policy".into(),
            can_auto_apply: false,
            reasons: vec![format!(
                "candidates were verified with different commands ({}) — not comparable; \
                 re-verify every candidate with one command",
                commands.into_iter().collect::<Vec<_>>().join(" | ")
            )],
        }
    } else {
        let (winner, verification) = eligible[0];
        TeamVerdict {
            selected_submission: Some(winner.id.clone()),
            method: METHOD.into(),
            decided_by: "team-policy".into(),
            can_auto_apply: true,
            reasons: vec![
                format!("{} applies cleanly", winner.id),
                format!(
                    "{} verifier tests passed via `{}` ({})",
                    winner.id,
                    verification.command.join(" "),
                    verification.id
                ),
                "smallest diff among verifier-passing candidates".into(),
            ],
        }
    };
    let kind = if verdict.selected_submission.is_some() {
        "verdict"
    } else {
        "no_verdict"
    };
    let ev = event(
        run_id,
        actor,
        kind,
        current.current_round,
        Some(current.phase),
        Some("verdict".into()),
        format!("verdict:{run_id}:{}", current.current_round),
        serde_json::to_value(&verdict)?,
    );
    append_event(repo, &ev)?;
    Ok(verdict)
}

fn ensure_clean_worktree(repo: &Repository) -> Result<(), H5iError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    if statuses.is_empty() {
        Ok(())
    } else {
        Err(H5iError::Metadata(
            "team apply requires a clean working tree".into(),
        ))
    }
}

pub fn apply_winner(
    repo: &Repository,
    h5i_root: &Path,
    run_id: &str,
    winner: Option<&str>,
    force: bool,
    actor: &str,
) -> Result<TeamApplyResult, H5iError> {
    ensure_clean_worktree(repo)?;
    let current = status(repo, run_id)?.run;
    let selected = match winner {
        Some(id) => id.to_string(),
        None => current
            .verdict
            .as_ref()
            .and_then(|v| v.selected_submission.clone())
            .ok_or_else(|| {
                H5iError::Metadata(format!(
                    "team '{run_id}' has no selected verdict (pass --winner or run `h5i team finalize`)"
                ))
            })?,
    };
    if !force {
        let verdict = current.verdict.as_ref().ok_or_else(|| {
            H5iError::Metadata("team apply without --force requires a verifier verdict".into())
        })?;
        if verdict.selected_submission.as_deref() != Some(selected.as_str())
            || !verdict.can_auto_apply
        {
            return Err(H5iError::Metadata(
                "team apply refused: selected submission is not covered by an auto-applicable verifier verdict (use --force to override)".into(),
            ));
        }
    }
    let submission = current
        .submissions
        .iter()
        .find(|s| s.id == selected)
        .ok_or_else(|| {
            H5iError::Metadata(format!("team '{run_id}' has no submission '{selected}'"))
        })?
        .clone();
    let repo_workdir = repo
        .workdir()
        .ok_or_else(|| H5iError::Metadata("team apply requires a non-bare repository".into()))?;
    let patch_dir = h5i_root.join("team").join(run_id).join("apply");
    std::fs::create_dir_all(&patch_dir).map_err(|e| H5iError::with_path(e, &patch_dir))?;
    let patch_path = patch_dir.join(format!("{}.patch", submission.id));
    let diff = run_git(
        repo_workdir,
        &[
            "diff",
            "--binary",
            &current.base_oid,
            &submission.commit_oid,
        ],
    )?;
    if !diff.status.success() {
        return Err(H5iError::Git(git2::Error::from_str(
            &String::from_utf8_lossy(&diff.stderr),
        )));
    }
    std::fs::write(&patch_path, &diff.stdout).map_err(|e| H5iError::with_path(e, &patch_path))?;
    let apply = run_git(
        repo_workdir,
        &["apply", "--index", patch_path.to_string_lossy().as_ref()],
    )?;
    if !apply.status.success() {
        let failure = String::from_utf8_lossy(&apply.stderr).trim().to_string();
        let ev = event(
            run_id,
            actor,
            "apply_conflict",
            current.current_round,
            Some(current.phase),
            None,
            format!("apply_conflict:{run_id}:{}", submission.id),
            serde_json::json!({ "submission_id": submission.id, "failure": failure }),
        );
        append_event(repo, &ev)?;
        return Err(H5iError::Metadata(format!(
            "team apply failed for {}: {}",
            submission.id,
            String::from_utf8_lossy(&apply.stderr).trim()
        )));
    }

    let mut index = repo.index()?;
    index.read(true)?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let parent_oid = repo.head()?.peel_to_commit()?.id();
    let parent = repo.find_commit(parent_oid)?;
    let sig = objects::signature(repo)?;
    let target_oid = repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("Apply h5i team {run_id} winner {}", submission.id),
        &tree,
        &[&parent],
    )?;
    let result = TeamApplyResult {
        submission_id: submission.id.clone(),
        source_commit_oid: submission.commit_oid.clone(),
        target_commit_oid: target_oid.to_string(),
    };
    let ev = event(
        run_id,
        actor,
        "applied",
        current.current_round,
        Some(current.phase),
        Some("applied".into()),
        format!("applied:{run_id}:{}", result.target_commit_oid),
        serde_json::to_value(&result)?,
    );
    append_event(repo, &ev)?;
    Ok(result)
}

fn lease_active(events: &[TeamEvent], worker_id: &str, ttl_secs: i64) -> bool {
    let mut latest: Option<&TeamEvent> = None;
    for ev in events.iter().filter(|e| e.kind == "lease_acquired") {
        if latest
            .map(|l| l.ts.as_str() < ev.ts.as_str())
            .unwrap_or(true)
        {
            latest = Some(ev);
        }
    }
    let Some(ev) = latest else {
        return false;
    };
    if ev.payload.get("worker_id").and_then(|v| v.as_str()) == Some(worker_id) {
        return false;
    }
    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&ev.ts) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(ts.with_timezone(&chrono::Utc))
        .num_seconds()
        < ttl_secs
}

pub fn worker_once(
    repo: &Repository,
    worker_id: &str,
    lease_ttl_secs: i64,
    actor: &str,
) -> Result<TeamWorkerReport, H5iError> {
    validate_agent_id(worker_id)?;
    let runs = list(repo)?;
    let mut report = TeamWorkerReport {
        worker_id: worker_id.into(),
        inspected: runs.len(),
        finalized: Vec::new(),
        skipped: Vec::new(),
    };
    for run in runs {
        let team_status = status(repo, &run.id)?;
        if lease_active(&team_status.events, worker_id, lease_ttl_secs) {
            report.skipped.push(format!("{}: leased", run.id));
            continue;
        }
        let lease = event(
            &run.id,
            actor,
            "lease_acquired",
            run.current_round,
            Some(run.phase.clone()),
            None,
            format!("lease:{}:{worker_id}:{}", run.id, now()),
            serde_json::json!({ "worker_id": worker_id, "ttl_secs": lease_ttl_secs }),
        );
        append_event(repo, &lease)?;
        let refreshed = status(repo, &run.id)?.run;
        if refreshed.verdict.is_some() {
            report
                .skipped
                .push(format!("{}: already finalized", run.id));
            continue;
        }
        if refreshed.submissions.is_empty() {
            report.skipped.push(format!("{}: no submissions", run.id));
            continue;
        }
        let verified: BTreeSet<&str> = refreshed
            .verifications
            .iter()
            .map(|v| v.submission_id.as_str())
            .collect();
        if refreshed
            .submissions
            .iter()
            .all(|s| verified.contains(s.id.as_str()))
        {
            let verdict = finalize(repo, &run.id, actor)?;
            if verdict.selected_submission.is_some() {
                report.finalized.push(run.id);
            } else {
                report.skipped.push(format!("{}: no verdict", run.id));
            }
        } else {
            report
                .skipped
                .push(format!("{}: waiting for verifier evidence", run.id));
        }
    }
    Ok(report)
}

pub fn render_status(status: &TeamStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "team {} ({})\n  phase   : {}\n  base    : {}\n  agents  : {}\n  submits : {}\n",
        status.run.id,
        status.run.name,
        status.run.phase,
        &status.run.base_oid[..12.min(status.run.base_oid.len())],
        status.run.agents.len(),
        status.run.submissions.len()
    ));
    for a in &status.run.agents {
        out.push_str(&format!(
            "  - {:<18} {:<12} {}{}\n",
            a.agent_id,
            a.state,
            a.env_id,
            a.latest_submission_id
                .as_ref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default()
        ));
    }
    out
}

pub fn render_compare(rows: &[TeamCompareRow]) -> String {
    if rows.is_empty() {
        return "No team agents.\n".into();
    }
    let mut out = String::new();
    out.push_str("agent                 submitted  files  +     -     latest\n");
    out.push_str("────────────────────────────────────────────────────────────\n");
    for r in rows {
        let latest = match (&r.last_tool, &r.last_result, r.last_exit) {
            (Some(tool), Some(result), Some(exit)) => format!("{tool} {result} (exit {exit})"),
            (Some(tool), _, Some(exit)) => format!("{tool} exit {exit}"),
            (_, _, Some(exit)) => format!("exit {exit}"),
            _ => "no capture".into(),
        };
        out.push_str(&format!(
            "{:<21} {:<9} {:>5} {:>5} {:>5}  {}\n",
            r.agent_id,
            if r.submitted { "yes" } else { "no" },
            r.files_changed,
            r.insertions,
            r.deletions,
            latest
        ));
    }
    out
}

fn short_oid(s: &str) -> &str {
    &s[..12.min(s.len())]
}

pub fn render_list(runs: &[TeamRun]) -> String {
    if runs.is_empty() {
        return "No teams. Create one: h5i team create <name>\n".into();
    }
    let mut out = String::new();
    out.push_str("team                  phase          agents  submits  base\n");
    out.push_str("────────────────────────────────────────────────────────────\n");
    for r in runs {
        out.push_str(&format!(
            "{:<21} {:<14} {:>6} {:>8}  {}\n",
            r.id,
            r.phase,
            r.agents.len(),
            r.submissions.len(),
            short_oid(&r.base_oid)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Oid;
    use std::fs;

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("Test", "test@example.com").unwrap()
    }

    fn commit_file(repo: &Repository, name: &str, body: &str) -> Oid {
        let work = repo.workdir().unwrap();
        fs::write(work.join(name), body).unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new(name)).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let parents = match repo.head().ok().and_then(|h| h.target()) {
            Some(oid) => vec![repo.find_commit(oid).unwrap()],
            None => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig(), &sig(), "commit", &tree, &parent_refs)
            .unwrap()
    }

    fn write_env(h5i_root: &Path, m: &env::EnvManifest) {
        let dir = m.dir(h5i_root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(m).unwrap(),
        )
        .unwrap();
    }

    fn manifest(repo: &Repository, h5i_root: &Path, agent: &str, slug: &str) -> env::EnvManifest {
        let head = repo.head().unwrap().target().unwrap().to_string();
        let branch = format!("refs/heads/h5i/env/{agent}/{slug}");
        repo.reference(&branch, Oid::from_str(&head).unwrap(), true, "env")
            .unwrap();
        let m = env::EnvManifest {
            id: format!("env/{agent}/{slug}"),
            agent: agent.into(),
            slug: slug.into(),
            base_commit: head.clone(),
            base_tree: repo
                .find_commit(Oid::from_str(&head).unwrap())
                .unwrap()
                .tree_id()
                .to_string(),
            parent_branch: "main".into(),
            branch,
            parent_context_branch: "main".into(),
            context_branch: format!("env/{agent}/{slug}"),
            profile: "workspace".into(),
            policy_digest: "policy".into(),
            isolation_claim: "workspace".into(),
            backend: "worktree".into(),
            created_at: now(),
            updated_at: now(),
            status: env::ST_IDLE.into(),
            captures: vec![],
            service_digest: None,
        };
        write_env(h5i_root, &m);
        m
    }

    #[test]
    fn create_add_submit_freeze_projects_from_events() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        manifest(&repo, h5i_root, "codex", "fix");

        create(&repo, "run1", "run1", "HEAD", 1, "human").unwrap();
        add_env(
            &repo,
            h5i_root,
            "run1",
            "env/codex/fix",
            "codex-fix",
            Some("codex".into()),
            None,
            Some("implementer".into()),
            "human",
        )
        .unwrap();
        let sub = submit(
            &repo,
            h5i_root,
            "run1",
            "codex-fix",
            None,
            Some("done".into()),
            "codex",
        )
        .unwrap();
        assert_eq!(sub.owner_agent, "codex-fix");
        let run = freeze(&repo, "run1", false, "human").unwrap();
        assert_eq!(run.phase, PHASE_SEALED_SUBMIT);
        assert_eq!(run.submissions.len(), 1);
        assert_eq!(
            run.agents[0].latest_submission_id.as_deref(),
            Some(sub.id.as_str())
        );
    }

    #[test]
    fn freeze_refuses_missing_submission() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        manifest(&repo, h5i_root, "codex", "fix");

        create(&repo, "run2", "run2", "HEAD", 1, "human").unwrap();
        add_env(
            &repo,
            h5i_root,
            "run2",
            "fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        let err = freeze(&repo, "run2", false, "human").unwrap_err();
        assert!(format!("{err}").contains("missing submissions"));
    }

    #[test]
    fn add_env_writes_team_identity_for_inbox_routing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hi\n");
        let h5i_root = dir.path();
        let m = manifest(&repo, h5i_root, "codex", "fix");
        create(&repo, "run-i", "run-i", "HEAD", 1, "human").unwrap();
        add_env(
            &repo, h5i_root, "run-i", "env/codex/fix", "codex-impl", None, None, None, "human",
        )
        .unwrap();
        let id = std::fs::read_to_string(m.dir(h5i_root).join("team-identity")).unwrap();
        assert_eq!(id.trim(), "codex-impl");
        let team = std::fs::read_to_string(m.dir(h5i_root).join("team-run")).unwrap();
        assert_eq!(team.trim(), "run-i");
    }

    #[test]
    fn current_team_pointer_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(get_current(root).is_none());
        // No arg + no current → error; an explicit arg always wins.
        assert!(resolve_run(root, None).is_err());
        assert_eq!(resolve_run(root, Some("x".into())).unwrap(), "x");
        // Set → get/resolve fall back to it; explicit still overrides.
        set_current(root, "demo").unwrap();
        assert_eq!(get_current(root).as_deref(), Some("demo"));
        assert_eq!(resolve_run(root, None).unwrap(), "demo");
        assert_eq!(resolve_run(root, Some("other".into())).unwrap(), "other");
        // Empty arg is treated as absent → falls back to current.
        assert_eq!(resolve_run(root, Some("  ".into())).unwrap(), "demo");
        clear_current(root).unwrap();
        assert!(get_current(root).is_none());
        clear_current(root).unwrap(); // idempotent
    }

    #[test]
    fn dispatch_grant_and_review_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        manifest(&repo, h5i_root, "codex", "fix");
        manifest(&repo, h5i_root, "claude", "fix");

        create(&repo, "run3", "run3", "HEAD", 1, "human").unwrap();
        add_env(
            &repo,
            h5i_root,
            "run3",
            "env/codex/fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        add_env(
            &repo,
            h5i_root,
            "run3",
            "env/claude/fix",
            "claude-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        submit(&repo, h5i_root, "run3", "codex-fix", None, None, "codex").unwrap();

        let sent = dispatch(&repo, h5i_root, "run3", "do the task", "human").unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|m| m.kind.as_deref() == Some("ASK")));

        let grant = grant_review(
            &repo,
            h5i_root,
            "run3",
            "claude-fix",
            "codex-fix",
            vec!["diff".into(), "summary".into()],
            "human",
        )
        .unwrap();
        assert_eq!(grant.reviewer, "claude-fix");
        assert_eq!(grant.artifact_ids.len(), 1);
        assert!(grant.message_id.is_some());

        let review = submit_review(
            &repo,
            "run3",
            "claude-fix",
            "codex-fix",
            "looks good".into(),
            "claude",
        )
        .unwrap();
        assert_eq!(review.referenced_artifacts, grant.artifact_ids);

        let events = read_events(&repo, "run3").unwrap();
        assert!(events.iter().any(|e| e.kind == "dispatched"));
        assert!(events.iter().any(|e| e.kind == "review_granted"));
        assert!(events.iter().any(|e| e.kind == "review_submitted"));
    }

    #[test]
    fn verify_and_finalize_selects_passing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        let m = manifest(&repo, h5i_root, "codex", "fix");
        let candidate = commit_file(&repo, "feature.txt", "ok\n");
        repo.reference(&m.branch, candidate, true, "candidate")
            .unwrap();

        create(&repo, "run4", "run4", "HEAD~1", 1, "human").unwrap();
        add_env(
            &repo,
            h5i_root,
            "run4",
            "env/codex/fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        let sub = submit(&repo, h5i_root, "run4", "codex-fix", None, None, "codex").unwrap();
        let verification = verify(
            &repo,
            h5i_root,
            "run4",
            "codex-fix",
            vec!["sh".into(), "-c".into(), "test -f feature.txt".into()],
            Some("workspace"),
            "human",
        )
        .unwrap();
        assert_eq!(verification.submission_id, sub.id);
        assert!(verification.applies_cleanly);
        assert!(verification.tests_passed);
        assert!(verification.capture_id.is_some());
        // The verifier ran sandboxed under the requested tier and recorded it.
        assert_eq!(verification.isolation, "workspace");

        let verdict = finalize(&repo, "run4", "human").unwrap();
        assert_eq!(
            verdict.selected_submission.as_deref(),
            Some(sub.id.as_str())
        );
        assert!(verdict.can_auto_apply);
    }

    #[test]
    fn apply_winner_replays_submission_diff_and_records_target_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let base = commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path().join(".git").join(".h5i-test");
        fs::create_dir_all(&h5i_root).unwrap();
        let m = manifest(&repo, &h5i_root, "codex", "fix");
        let candidate = commit_file(&repo, "feature.txt", "ok\n");
        repo.reference(&m.branch, candidate, true, "candidate")
            .unwrap();

        create(&repo, "run5", "run5", &base.to_string(), 1, "human").unwrap();
        add_env(
            &repo,
            &h5i_root,
            "run5",
            "env/codex/fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        let sub = submit(&repo, &h5i_root, "run5", "codex-fix", None, None, "codex").unwrap();
        verify(
            &repo,
            &h5i_root,
            "run5",
            "codex-fix",
            vec!["sh".into(), "-c".into(), "test -f feature.txt".into()],
            Some("workspace"),
            "human",
        )
        .unwrap();
        finalize(&repo, "run5", "human").unwrap();

        let base_obj = repo.find_object(base, None).unwrap();
        repo.reset(&base_obj, git2::ResetType::Hard, None).unwrap();

        let applied = apply_winner(&repo, &h5i_root, "run5", None, false, "human").unwrap();
        assert_eq!(applied.submission_id, sub.id);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id().to_string(), applied.target_commit_oid);
        let tree = head.tree().unwrap();
        assert!(tree.get_path(Path::new("feature.txt")).is_ok());

        let status = status(&repo, "run5").unwrap();
        assert_eq!(status.run.phase, "applied");
    }

    #[test]
    fn discussion_marks_later_submission_as_influenced() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        let codex = manifest(&repo, h5i_root, "codex", "fix");
        let claude = manifest(&repo, h5i_root, "claude", "fix");
        let codex_commit = commit_file(&repo, "codex.txt", "ok\n");
        repo.reference(&codex.branch, codex_commit, true, "codex")
            .unwrap();
        let claude_commit = commit_file(&repo, "claude.txt", "ok\n");
        repo.reference(&claude.branch, claude_commit, true, "claude")
            .unwrap();

        create(&repo, "run6", "run6", "HEAD~2", 1, "human").unwrap();
        add_env(
            &repo,
            h5i_root,
            "run6",
            "env/codex/fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        add_env(
            &repo,
            h5i_root,
            "run6",
            "env/claude/fix",
            "claude-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        let codex_sub = submit(&repo, h5i_root, "run6", "codex-fix", None, None, "codex").unwrap();
        // Both agents submit their INDEPENDENT first attempts, then the run is
        // sealed before any discussion is permitted (independence-first).
        submit(&repo, h5i_root, "run6", "claude-fix", None, None, "claude").unwrap();
        freeze(&repo, "run6", false, "human").unwrap();
        discuss(
            &repo,
            h5i_root,
            "run6",
            "codex-fix",
            vec!["claude-fix".into()],
            "consider this approach".into(),
            vec![codex_sub.id],
            "human",
        )
        .unwrap();
        // claude revises AFTER the discussion → influenced, no longer independent.
        let claude_sub =
            submit(&repo, h5i_root, "run6", "claude-fix", None, None, "claude").unwrap();
        assert!(!claude_sub.independent);
        assert!(!claude_sub.influence_event_ids.is_empty());
        assert!(!claude_sub.influence_artifact_ids.is_empty());
    }

    #[test]
    fn discuss_refused_before_freeze() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        manifest(&repo, h5i_root, "codex", "fix");
        manifest(&repo, h5i_root, "claude", "fix");
        create(&repo, "run-d", "run-d", "HEAD", 1, "human").unwrap();
        add_env(&repo, h5i_root, "run-d", "env/codex/fix", "codex-fix", None, None, None, "human")
            .unwrap();
        add_env(&repo, h5i_root, "run-d", "env/claude/fix", "claude-fix", None, None, None, "human")
            .unwrap();
        // draft → discussion forbidden (first attempts not yet sealed).
        let err = discuss(
            &repo,
            h5i_root,
            "run-d",
            "codex-fix",
            vec!["claude-fix".into()],
            "hi".into(),
            vec![],
            "codex-fix",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("only allowed after"));
        // after freeze → permitted.
        freeze(&repo, "run-d", true, "human").unwrap();
        let d = discuss(
            &repo,
            h5i_root,
            "run-d",
            "codex-fix",
            vec!["claude-fix".into()],
            "hi".into(),
            vec![],
            "codex-fix",
        )
        .unwrap();
        assert_eq!(d.sender, "codex-fix");
    }

    #[test]
    fn finalize_refuses_divergent_verifier_commands() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path();
        manifest(&repo, h5i_root, "a1", "fix");
        manifest(&repo, h5i_root, "a2", "fix");
        create(&repo, "run-v", "run-v", "HEAD", 1, "human").unwrap();
        add_env(&repo, h5i_root, "run-v", "env/a1/fix", "a1", None, None, None, "human").unwrap();
        add_env(&repo, h5i_root, "run-v", "env/a2/fix", "a2", None, None, None, "human").unwrap();

        // Hand-craft two passing submissions + verifications with DIFFERENT commands.
        for (agent, sid) in [("a1", "sub-a1"), ("a2", "sub-a2")] {
            let art = TeamArtifact {
                id: sid.into(),
                owner_agent: agent.into(),
                round: 1,
                env_id: format!("env/{agent}/fix"),
                commit_oid: "0".repeat(40),
                tree_oid: "0".repeat(40),
                capture_ids: vec![],
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                summary: None,
                independent: true,
                influence_event_ids: vec![],
                influence_artifact_ids: vec![],
            };
            let ev = event(
                "run-v",
                "human",
                "submitted",
                1,
                None,
                None,
                format!("submitted:run-v:{agent}"),
                serde_json::to_value(&art).unwrap(),
            );
            append_event(&repo, &ev).unwrap();
        }
        let verifs = [
            ("ver-a1", "sub-a1", "a1", vec!["cargo".to_string(), "test".to_string()]),
            ("ver-a2", "sub-a2", "a2", vec!["true".to_string()]),
        ];
        for (vid, sid, agent, command) in verifs {
            let v = TeamVerification {
                id: vid.into(),
                submission_id: sid.into(),
                owner_agent: agent.into(),
                round: 1,
                command,
                applies_cleanly: true,
                tests_passed: true,
                isolation: "workspace".into(),
                capture_id: None,
                failure: None,
            };
            let ev = event(
                "run-v",
                "human",
                "verified",
                1,
                None,
                Some("verified".into()),
                format!("verified:run-v:{vid}"),
                serde_json::to_value(&v).unwrap(),
            );
            append_event(&repo, &ev).unwrap();
        }
        let verdict = finalize(&repo, "run-v", "human").unwrap();
        assert!(verdict.selected_submission.is_none());
        assert!(!verdict.can_auto_apply);
        assert!(verdict.reasons.iter().any(|r| r.contains("different commands")));
    }

    #[test]
    fn worker_once_finalizes_verifier_ready_run() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(&repo, "README.md", "hello\n");
        let h5i_root = dir.path().join(".git").join(".h5i-test");
        fs::create_dir_all(&h5i_root).unwrap();
        let m = manifest(&repo, &h5i_root, "codex", "fix");
        let candidate = commit_file(&repo, "feature.txt", "ok\n");
        repo.reference(&m.branch, candidate, true, "candidate")
            .unwrap();

        create(&repo, "run7", "run7", "HEAD~1", 1, "human").unwrap();
        add_env(
            &repo,
            &h5i_root,
            "run7",
            "env/codex/fix",
            "codex-fix",
            None,
            None,
            None,
            "human",
        )
        .unwrap();
        submit(&repo, &h5i_root, "run7", "codex-fix", None, None, "codex").unwrap();
        verify(
            &repo,
            &h5i_root,
            "run7",
            "codex-fix",
            vec!["sh".into(), "-c".into(), "test -f feature.txt".into()],
            Some("workspace"),
            "human",
        )
        .unwrap();

        let report = worker_once(&repo, "worker-one", 300, "worker-one").unwrap();
        assert_eq!(report.finalized, vec!["run7"]);
        let status = status(&repo, "run7").unwrap();
        assert!(status
            .run
            .verdict
            .as_ref()
            .unwrap()
            .selected_submission
            .is_some());
        assert!(status.events.iter().any(|e| e.kind == "lease_acquired"));
    }
}
