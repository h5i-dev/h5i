//! h5i environments (`h5i env`) — the triple fusion of a code branch, a
//! reasoning (context) branch, and a policy manifest (docs/environments-design.md §3).
//!
//! An environment is a Git-addressed, policy-confined, fully-observed unit of
//! agent work:
//!
//! ```text
//!    git branch / tree    ← the CODE       (refs/heads/h5i/env/<agent>/<slug>)
//!  + h5i context branch   ← the REASONING  (refs/h5i/context/env/<agent>/<slug>)
//!  + env manifest         ← POLICY + PROVENANCE (refs/h5i/env + .git/.h5i/env/…)
//! ```
//!
//! Storage (§8) reuses existing machinery: every `env run` is a tagged
//! `objects` capture (the evidence log), the event log in `refs/h5i/env` is
//! the same CAS-append + union-merge pattern as `refs/h5i/msg` /
//! `refs/h5i/objects`, and the workspace backend is the **native git
//! worktree** placed under `.git/.h5i/env/<agent>/<slug>/work` (§4).
//!
//! Lifecycle (§9): created → running → idle → proposed → applied | aborted,
//! then `gc` reclaims the workspace while retaining the manifest for
//! forensics. `apply` NEVER happens implicitly — `propose` surfaces, a
//! reviewer applies.

use git2::{build::CheckoutBuilder, Repository};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::H5iError;
use crate::objects;
use crate::sandbox::{self, IsolationClaim, ResolvedPolicy};

/// Git ref holding the shareable env state: the append-only event log plus the
/// per-env manifests and resolved policies (so `h5i push`/`pull` carry an
/// environment to another clone for the cross-agent review loop, design §11).
pub const ENV_REF: &str = "refs/h5i/env";
/// File inside the ref's tree holding the event log (one JSON object per line).
pub const EVENTS_FILE: &str = "events.jsonl";
/// File inside the ref's tree holding the manifests (one `EnvManifest` per
/// line, keyed by id — the mutable per-env record).
pub const MANIFESTS_FILE: &str = "manifests.jsonl";
/// File inside the ref's tree holding resolved policies (one `{id, toml}` per
/// line — immutable after create).
pub const POLICIES_FILE: &str = "policies.jsonl";
/// Directory under the h5i sidecar root holding per-env state.
pub const ENV_DIR: &str = "env";
/// Prefix (under `refs/heads/`) of every env code branch.
pub const BRANCH_PREFIX: &str = "h5i/env/";

const MANIFEST_FILE: &str = "manifest.json";
const POLICY_RESOLVED_FILE: &str = "policy.resolved.toml";
const STATUS_FILE: &str = "status";
const WORK_DIR: &str = "work";
const RUN_LOCK_FILE: &str = "run.lock";

/// An exclusive, advisory `flock` on `<env>/run.lock` that serializes
/// `h5i env run` for one environment. The kernel releases the lock when the
/// holding process exits — including a crash — so there are no stale locks to
/// clear. Concurrent runs would otherwise race on the status file and the
/// captures list and corrupt the manifest.
#[cfg(unix)]
struct RunLock {
    _file: std::fs::File,
}

#[cfg(unix)]
impl RunLock {
    fn acquire(env_dir: &Path) -> Result<RunLock, H5iError> {
        use std::os::unix::io::AsRawFd;
        let path = env_dir.join(RUN_LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(H5iError::Metadata(
                    "environment is busy — another `h5i env run` holds its lock".into(),
                ));
            }
            return Err(H5iError::with_path(err, &path));
        }
        Ok(RunLock { _file: file })
    }
}

// ─── status state machine (§9) ──────────────────────────────────────────────

pub const ST_CREATED: &str = "created";
pub const ST_RUNNING: &str = "running";
pub const ST_IDLE: &str = "idle";
pub const ST_PROPOSED: &str = "proposed";
pub const ST_APPLIED: &str = "applied";
pub const ST_ABORTED: &str = "aborted";

// ─── data model (§8) ────────────────────────────────────────────────────────

/// The env manifest — small, points at evidence, never inlines it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvManifest {
    /// `env/<agent>/<slug>`.
    pub id: String,
    /// Requesting agent (`$H5I_AGENT`).
    pub agent: String,
    pub slug: String,
    /// Immutable pinned base (exact commit + tree, never "current dirty tree").
    pub base_commit: String,
    pub base_tree: String,
    /// Git branch this env forked from / proposes back onto (short name).
    pub parent_branch: String,
    /// The env's own code branch (full ref, `refs/heads/h5i/env/…`).
    pub branch: String,
    /// Context branch to merge reasoning findings back into on apply.
    pub parent_context_branch: String,
    /// The env's own reasoning branch (name under `refs/h5i/context/`).
    pub context_branch: String,
    pub profile: String,
    /// sha256 of `policy.resolved.toml` as enforced.
    pub policy_digest: String,
    /// Resolved claim (workspace|process|…) — what the host could actually satisfy.
    pub isolation_claim: String,
    /// Workspace backend (`worktree` today; pluggable later).
    pub backend: String,
    pub created_at: String,
    /// Last-persisted timestamp (RFC3339). Bumped on every save; the union-merge
    /// tiebreak when the same env is edited on two clones (newest wins).
    #[serde(default)]
    pub updated_at: String,
    pub status: String,
    /// Object ids in `refs/h5i/objects` — the evidence, oldest first.
    #[serde(default)]
    pub captures: Vec<String>,
}

impl EnvManifest {
    pub fn dir(&self, h5i_root: &Path) -> PathBuf {
        env_dir(h5i_root, &self.agent, &self.slug)
    }

    pub fn work_dir(&self, h5i_root: &Path) -> PathBuf {
        self.dir(h5i_root).join(WORK_DIR)
    }

    /// Short branch name (without `refs/heads/`).
    pub fn branch_short(&self) -> &str {
        self.branch.strip_prefix("refs/heads/").unwrap_or(&self.branch)
    }

    /// The libgit2 worktree registration name (flat, unique per env).
    pub fn worktree_name(&self) -> String {
        format!("h5i-env-{}-{}", self.agent, self.slug)
    }
}

/// One line in the append-only `refs/h5i/env` event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvEvent {
    /// RFC3339 UTC, microsecond precision (lexically sortable).
    pub ts: String,
    pub env_id: String,
    pub agent: String,
    /// created | exec | status | proposed | applied | aborted | gc
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Capture (object) id for `exec` events — the evidence pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
}

fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

pub fn env_dir(h5i_root: &Path, agent: &str, slug: &str) -> PathBuf {
    h5i_root.join(ENV_DIR).join(agent).join(slug)
}

/// Validate an env slug (it becomes a ref component, a directory name, and a
/// worktree name — keep it boring). Lowercase alnum plus `-` `_` `.`, must
/// start alphanumeric, no slashes, max 64 chars.
pub fn validate_slug(slug: &str) -> Result<(), H5iError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && !slug.ends_with(".lock")
        && slug != "."
        && slug != "..";
    if ok {
        Ok(())
    } else {
        Err(H5iError::Metadata(format!(
            "invalid env name '{slug}' — use lowercase letters, digits, '-', '_', '.' \
             (start alphanumeric, ≤64 chars, no '/')"
        )))
    }
}

/// Validate an agent identity before it is used to build a ref component
/// (`refs/heads/h5i/env/<agent>/<slug>`), a directory name (`env_dir` joins it
/// unchecked), and a worktree name. `msg::validate_name` already constrains the
/// charset to `[A-Za-z0-9._-]`, but that still admits `.`, `..`, and
/// leading-dot names — which are path traversal here (`env_dir(.., "..", slug)`
/// escapes the env root) and invalid git ref components. Reject them
/// fail-closed, mirroring [`validate_slug`].
pub fn validate_agent(agent: &str) -> Result<(), H5iError> {
    let ok = !agent.is_empty()
        && agent.len() <= 64
        && !agent.contains('/')
        && !agent.contains('\\')
        && !agent.contains("..")
        && !agent.starts_with('.')
        && !agent.ends_with(".lock")
        && agent
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(H5iError::Metadata(format!(
            "invalid agent name '{agent}' — letters, digits, '-', '_', '.' only \
             (≤64 chars, must not start with '.', contain '..', or end '.lock')"
        )))
    }
}

// ─── event log: CAS append + union merge (same pattern as objects/msg) ──────

/// Replace (or append) the single JSONL line whose parsed `id` field equals
/// `id`. Lines are kept sorted by id so the blob is deterministic.
fn upsert_jsonl_by_id(existing: &str, id: &str, new_line: &str) -> String {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for line in existing.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(line_id) = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_owned))
        {
            map.insert(line_id, line.to_string());
        }
    }
    map.insert(id.to_string(), new_line.to_string());
    let mut out = String::new();
    for line in map.values() {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Drop the single JSONL line whose parsed `id` field equals `id`, preserving
/// the others verbatim and in order. Inverse of [`upsert_jsonl_by_id`]; powers
/// the manifest/policy strip in [`rm`].
fn remove_jsonl_by_id(existing: &str, id: &str) -> String {
    let mut out = String::new();
    for line in existing.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let line_id = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_owned));
        if line_id.as_deref() == Some(id) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Append one event to `refs/h5i/env` with compare-and-swap semantics. Thin
/// wrapper over [`append_env_commit`] for callers with no manifest to mirror
/// (only `gc`, which records an event without changing the manifest body).
pub fn append_event(repo: &Repository, ev: &EnvEvent) -> Result<(), H5iError> {
    append_env_commit(repo, ev, None, None)
}

/// Atomically record an env event AND mirror the env's manifest (and, on
/// create, its resolved policy) into `refs/h5i/env`, so the whole environment
/// travels with `h5i push`/`pull`. One CAS commit updates `events.jsonl`
/// (append), `manifests.jsonl` (upsert by id), and `policies.jsonl` (upsert,
/// write-once). Retries on a lost race.
pub fn append_env_commit(
    repo: &Repository,
    ev: &EnvEvent,
    manifest: Option<&EnvManifest>,
    policy_toml: Option<&str>,
) -> Result<(), H5iError> {
    const MAX_ATTEMPTS: usize = 64;
    let event_line = serde_json::to_string(ev)?;
    let manifest_line = match manifest {
        Some(m) => Some(serde_json::to_string(m)?),
        None => None,
    };
    let message = format!("h5i env: {} {}", ev.event, ev.env_id);

    for _ in 0..MAX_ATTEMPTS {
        let tip = repo.refname_to_id(ENV_REF).ok();
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
        log.push_str(&event_line);
        log.push('\n');

        let mut files: Vec<(&str, String)> = vec![(EVENTS_FILE, log)];
        if let (Some(m), Some(line)) = (manifest, &manifest_line) {
            let existing =
                objects::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE).unwrap_or_default();
            files.push((MANIFESTS_FILE, upsert_jsonl_by_id(&existing, &m.id, line)));
        }
        if let (Some(m), Some(toml)) = (manifest, policy_toml) {
            let existing =
                objects::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE).unwrap_or_default();
            // Only write a policy once (it is immutable after create).
            if !existing.lines().any(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s == m.id))
                    .unwrap_or(false)
            }) {
                let entry = serde_json::to_string(&serde_json::json!({"id": m.id, "toml": toml}))?;
                let mut updated = existing;
                if !updated.is_empty() && !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push_str(&entry);
                updated.push('\n');
                files.push((POLICIES_FILE, updated));
            }
        }

        let file_refs: Vec<(&str, &str)> = files.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &file_refs)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = objects::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        let cas_ok = match tip {
            None => repo.reference(ENV_REF, new_oid, false, &message).is_ok(),
            Some(old) => repo.reference_matching(ENV_REF, new_oid, true, old, &message).is_ok(),
        };
        if cas_ok {
            return Ok(());
        }
    }
    Err(H5iError::Internal(format!(
        "h5i env: event {} for {} could not be appended after {MAX_ATTEMPTS} attempts",
        ev.event, ev.env_id
    )))
}

/// Append a `removed` event AND strip the env's manifest + policy lines from
/// `refs/h5i/env`, in one CAS commit. This is what makes [`rm`] durable on this
/// clone: [`materialize_from_ref`] runs at the top of every `env` command and
/// would otherwise rewrite the on-disk manifest straight back from the ref. The
/// `removed` event stays in the append-only log as the audit trail. (Across
/// clones this is not a tombstone — a `pull` from a peer that still holds the
/// manifest re-introduces it via union-merge; a distributed delete is a
/// separate, larger change.)
fn append_removed_and_strip(repo: &Repository, ev: &EnvEvent) -> Result<(), H5iError> {
    const MAX_ATTEMPTS: usize = 64;
    let event_line = serde_json::to_string(ev)?;
    let message = format!("h5i env: removed {}", ev.env_id);

    for _ in 0..MAX_ATTEMPTS {
        let tip = repo.refname_to_id(ENV_REF).ok();
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
        log.push_str(&event_line);
        log.push('\n');

        let manifests = remove_jsonl_by_id(
            &objects::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
                .unwrap_or_default(),
            &ev.env_id,
        );
        let policies = remove_jsonl_by_id(
            &objects::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE)
                .unwrap_or_default(),
            &ev.env_id,
        );

        let files: Vec<(&str, &str)> = vec![
            (EVENTS_FILE, log.as_str()),
            (MANIFESTS_FILE, manifests.as_str()),
            (POLICIES_FILE, policies.as_str()),
        ];
        let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &files)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = objects::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        let cas_ok = match tip {
            None => repo.reference(ENV_REF, new_oid, false, &message).is_ok(),
            Some(old) => repo.reference_matching(ENV_REF, new_oid, true, old, &message).is_ok(),
        };
        if cas_ok {
            return Ok(());
        }
    }
    Err(H5iError::Internal(format!(
        "h5i env: removal of {} could not be committed after {MAX_ATTEMPTS} attempts",
        ev.env_id
    )))
}

/// Read every env manifest stored in `refs/h5i/env`.
pub fn read_ref_manifests(repo: &Repository) -> Vec<EnvManifest> {
    let Some(tree) = repo
        .find_reference(ENV_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok())
    else {
        return Vec::new();
    };
    let Some(raw) = objects::read_blob_from_tree(repo, Some(&tree), MANIFESTS_FILE) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<EnvManifest>(l).ok())
        .collect()
}

/// Read every resolved policy stored in `refs/h5i/env` as `(env_id, toml)`.
pub fn read_ref_policies(repo: &Repository) -> Vec<(String, String)> {
    let Some(tree) = repo
        .find_reference(ENV_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok())
    else {
        return Vec::new();
    };
    let Some(raw) = objects::read_blob_from_tree(repo, Some(&tree), POLICIES_FILE) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| {
            Some((
                v.get("id")?.as_str()?.to_string(),
                v.get("toml")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Materialize env manifests + policies from `refs/h5i/env` onto disk for any
/// env that is absent locally, or whose ref copy is newer (`updated_at`). This
/// is what lets a `h5i pull` make another clone's environments appear in
/// `h5i env list`/`status`/`diff`/`apply`. Worktrees are inherently local, so a
/// materialized ("remote") env has no `work/`; review/apply operate on the
/// pushed code branch instead.
pub fn materialize_from_ref(repo: &Repository, h5i_root: &Path) -> Result<usize, H5iError> {
    let policies: std::collections::HashMap<String, String> =
        read_ref_policies(repo).into_iter().collect();
    let mut written = 0usize;
    for m in read_ref_manifests(repo) {
        let dir = env_dir(h5i_root, &m.agent, &m.slug);
        let local_newer = load_manifest_at(&dir)
            .ok()
            .map(|local| local.updated_at >= m.updated_at)
            .unwrap_or(false);
        if local_newer {
            continue;
        }
        save_manifest(h5i_root, &m)?;
        if let Some(toml) = policies.get(&m.id) {
            let path = dir.join(POLICY_RESOLVED_FILE);
            std::fs::write(&path, toml).map_err(|e| H5iError::with_path(e, &path))?;
        }
        written += 1;
    }
    Ok(written)
}

/// Read every event, oldest first. Optionally filtered to one env.
pub fn read_events(repo: &Repository, env_id: Option<&str>) -> Vec<EnvEvent> {
    let Some(reference) = repo.find_reference(ENV_REF).ok() else {
        return Vec::new();
    };
    let Some(tree) = reference.peel_to_commit().ok().and_then(|c| c.tree().ok()) else {
        return Vec::new();
    };
    let Some(raw) = objects::read_blob_from_tree(repo, Some(&tree), EVENTS_FILE) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<EnvEvent>(l).ok())
        .filter(|e| env_id.is_none_or(|id| e.env_id == id))
        .collect()
}

/// Reconcile two divergent `refs/h5i/env` tips. Three files travel in this
/// ref; each merges so `h5i pull` never drops data:
///
/// - `events.jsonl` — append-only: union by `(env_id, ts, event)`.
/// - `manifests.jsonl` — mutable per env: union by `id`, newest `updated_at`
///   wins (lets an `apply` on one clone propagate back).
/// - `policies.jsonl` — immutable after create: union by `id`, keep either.
///
/// Mirrors [`crate::objects::union_merge_commits`].
pub fn union_merge_commits(
    repo: &Repository,
    local_oid: git2::Oid,
    incoming_oid: git2::Oid,
) -> Result<git2::Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    // events: append-only union.
    let mut seen: HashSet<String> = HashSet::new();
    let mut events: Vec<EnvEvent> = Vec::new();
    // manifests: id → newest manifest.
    let mut manifests: std::collections::HashMap<String, EnvManifest> = std::collections::HashMap::new();
    // policies: id → toml (first seen wins; immutable).
    let mut policies: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for oid in [local_oid, incoming_oid] {
        let tree = repo.find_commit(oid)?.tree().ok();
        let raw = objects::read_blob_from_tree(repo, tree.as_ref(), EVENTS_FILE).unwrap_or_default();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<EnvEvent>(line) {
                let key = format!("{}|{}|{}", e.env_id, e.ts, e.event);
                if seen.insert(key) {
                    events.push(e);
                }
            }
        }
        let mraw = objects::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE).unwrap_or_default();
        for line in mraw.lines() {
            if let Ok(m) = serde_json::from_str::<EnvManifest>(line) {
                match manifests.get(&m.id) {
                    Some(existing) if existing.updated_at >= m.updated_at => {}
                    _ => {
                        manifests.insert(m.id.clone(), m);
                    }
                }
            }
        }
        let praw = objects::read_blob_from_tree(repo, tree.as_ref(), POLICIES_FILE).unwrap_or_default();
        for line in praw.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let (Some(id), Some(toml)) = (
                    v.get("id").and_then(|i| i.as_str()),
                    v.get("toml").and_then(|t| t.as_str()),
                ) {
                    policies.entry(id.to_string()).or_insert_with(|| toml.to_string());
                }
            }
        }
    }
    events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.env_id.cmp(&b.env_id)));

    let mut log = String::new();
    for e in &events {
        log.push_str(&serde_json::to_string(e)?);
        log.push('\n');
    }
    let mut mlog = String::new();
    for m in {
        let mut v: Vec<&EnvManifest> = manifests.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    } {
        mlog.push_str(&serde_json::to_string(m)?);
        mlog.push('\n');
    }
    let mut plog = String::new();
    for (id, toml) in &policies {
        plog.push_str(&serde_json::to_string(&serde_json::json!({"id": id, "toml": toml}))?);
        plog.push('\n');
    }

    let base_tree = local_commit.tree().ok();
    let mut files: Vec<(&str, &str)> = vec![(EVENTS_FILE, &log)];
    if !mlog.is_empty() {
        files.push((MANIFESTS_FILE, &mlog));
    }
    if !plog.is_empty() {
        files.push((POLICIES_FILE, &plog));
    }
    let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &files)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = objects::signature(repo)?;
    let parents = [&local_commit, &incoming_commit];
    Ok(repo.commit(None, &sig, &sig, "h5i pull: union-merge of refs/h5i/env", &tree, &parents)?)
}

// ─── manifest persistence ───────────────────────────────────────────────────

pub fn save_manifest(h5i_root: &Path, m: &EnvManifest) -> Result<(), H5iError> {
    let dir = m.dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = dir.join(MANIFEST_FILE);
    std::fs::write(&path, serde_json::to_string_pretty(m)?)
        .map_err(|e| H5iError::with_path(e, &path))?;
    std::fs::write(dir.join(STATUS_FILE), format!("{}\n", m.status))
        .map_err(|e| H5iError::with_path(e, dir.join(STATUS_FILE)))?;
    Ok(())
}

fn load_manifest_at(dir: &Path) -> Result<EnvManifest, H5iError> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(serde_json::from_str(&text)?)
}

/// All env manifests on this clone, ordered by creation time.
pub fn list(h5i_root: &Path) -> Vec<EnvManifest> {
    let mut out = Vec::new();
    let root = h5i_root.join(ENV_DIR);
    let Ok(agents) = std::fs::read_dir(&root) else {
        return out;
    };
    for agent in agents.flatten() {
        let Ok(slugs) = std::fs::read_dir(agent.path()) else {
            continue;
        };
        for slug in slugs.flatten() {
            if let Ok(m) = load_manifest_at(&slug.path()) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

/// Resolve a user-supplied env name: `<slug>`, `<agent>/<slug>`, or the full
/// `env/<agent>/<slug>`. A bare slug must be unambiguous across agents.
pub fn find(h5i_root: &Path, name: &str) -> Result<EnvManifest, H5iError> {
    let name = name.trim().trim_matches('/');
    let all = list(h5i_root);
    let matches: Vec<&EnvManifest> = all
        .iter()
        .filter(|m| {
            m.id == name || m.id == format!("env/{name}") || m.slug == name
        })
        .collect();
    match matches.len() {
        0 => Err(H5iError::Metadata(format!(
            "no environment named '{name}' (see `h5i env list`)"
        ))),
        1 => Ok(matches[0].clone()),
        _ => Err(H5iError::Metadata(format!(
            "'{name}' is ambiguous — qualify it: {}",
            matches.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Load the stored resolved policy for `m`, verifying it against the digest
/// pinned in the manifest (tamper-evident).
pub fn load_policy(h5i_root: &Path, m: &EnvManifest) -> Result<ResolvedPolicy, H5iError> {
    let path = m.dir(h5i_root).join(POLICY_RESOLVED_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    let policy = ResolvedPolicy::from_toml(&text)?;
    let digest = policy.digest()?;
    if digest != m.policy_digest {
        return Err(H5iError::Metadata(format!(
            "policy.resolved.toml for {} does not match the digest pinned in its manifest \
             (expected {}, found {digest}) — refusing to run under a tampered policy",
            m.id, m.policy_digest
        )));
    }
    Ok(policy)
}

fn set_status(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    status: &str,
    event: &str,
    detail: Option<String>,
    capture: Option<String>,
) -> Result<(), H5iError> {
    m.status = status.to_string();
    m.updated_at = now_ts();
    save_manifest(h5i_root, m)?;
    // Mirror the updated manifest into refs/h5i/env (shareable) in the same
    // commit as the event, so a `h5i push` carries the new state.
    append_env_commit(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: event.to_string(),
            detail,
            capture,
        },
        Some(m),
        None,
    )
}

// ─── create (§9) ────────────────────────────────────────────────────────────

pub struct CreateOpts {
    /// Base revision (default HEAD). Pinned immutably at creation.
    pub from: Option<String>,
    /// Policy profile name in `.h5i/env.toml`. `None` auto-picks: the built-in
    /// `agent` profile (agent-in-box) when this host can enforce it, else the
    /// fail-closed `default`. An explicit name is fail-closed (refused if it
    /// cannot be instantiated, never substituted).
    pub profile: Option<String>,
    /// `--isolation` request. `Some(Claim)` is fail-closed (refused if unmet);
    /// `Some(Auto)` or `None` auto-picks the strongest runnable tier.
    pub isolation: Option<sandbox::IsolationRequest>,
    /// Workspace backend. `auto` and `worktree` are accepted today.
    pub backend: String,
}

impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            from: None,
            profile: None,
            isolation: None,
            backend: "auto".into(),
        }
    }
}

/// Create an environment: pin the base, create the code branch + worktree,
/// fork the reasoning branch, resolve + persist the policy, record the event.
pub fn create(
    repo: &Repository,
    h5i_root: &Path,
    workdir: &Path,
    agent: &str,
    slug: &str,
    opts: CreateOpts,
) -> Result<EnvManifest, H5iError> {
    validate_slug(slug)?;
    validate_agent(agent)?;
    let backend = match opts.backend.as_str() {
        "auto" | "worktree" => "worktree",
        other => {
            return Err(H5iError::Metadata(format!(
                "workspace backend '{other}' is not available in this build (worktree only; \
                 branchfs is a later, opt-in phase)"
            )))
        }
    };

    let id = format!("env/{agent}/{slug}");
    let dir = env_dir(h5i_root, agent, slug);
    let branch_short = format!("{BRANCH_PREFIX}{agent}/{slug}");
    let branch_full = format!("refs/heads/{branch_short}");
    if dir.exists() {
        return Err(H5iError::Metadata(format!("environment {id} already exists")));
    }
    if repo.find_reference(&branch_full).is_ok() {
        return Err(H5iError::Metadata(format!(
            "branch {branch_full} already exists — `h5i env gc` keeps applied/aborted env \
             branches for provenance; pick a new name"
        )));
    }

    // Resolve the profile name. Unspecified prefers the built-in agent-in-box
    // profile *scoped to the creating runtime* (`agent-claude`/`agent-codex`):
    // a Claude box must not get Codex's credentials or OpenAI egress, so the
    // box is pinned to whoever built it. `env shell` is the agent-in-box, and a
    // box that cannot run an agent is the wrong default — but the profile is
    // only enforceable when its net.egress has a supervised/container tier, so
    // a pinned weaker `--isolation` (or a host without the stack) falls back to
    // the fail-closed `default`. Same pattern as the isolation auto-pick below:
    // explicit = fail-closed, unspecified = best runnable.
    let agent_profile = sandbox::AgentRuntime::from_identity(agent).profile_name();
    let profile_name: &str = match &opts.profile {
        Some(p) => p.as_str(),
        None => {
            let agent_runnable = (|| -> Result<(), H5iError> {
                let claim = match opts.isolation {
                    Some(sandbox::IsolationRequest::Claim(c)) => c,
                    _ => sandbox::effective_auto(workdir, agent_profile, false)?,
                };
                let prof = sandbox::load_profile(workdir, agent_profile, Some(claim))?;
                let pol = sandbox::resolve(&prof, &sandbox::probe_host())?;
                sandbox::verify_exec(&pol)
            })()
            .is_ok();
            if agent_runnable {
                agent_profile
            } else {
                "default"
            }
        }
    };

    // Resolve the isolation claim. Explicit `--isolation <tier>` is fail-closed;
    // `auto` / unspecified picks the strongest tier the host can actually run
    // (secure-by-default). The chosen tier is then pinned into the policy below.
    let claim = match opts.isolation {
        Some(sandbox::IsolationRequest::Claim(c)) => c,
        Some(sandbox::IsolationRequest::Auto) => sandbox::effective_auto(workdir, profile_name, true)?,
        None => sandbox::effective_auto(workdir, profile_name, false)?,
    };

    // Policy first (fail closed BEFORE any state is created on disk).
    let profile = sandbox::load_profile(workdir, profile_name, Some(claim))?;
    let caps = sandbox::probe_host();
    let policy = sandbox::resolve(&profile, &caps)?;
    // Functionally verify the confinement can actually run a command — capability
    // bits can be present while a hardened kernel still denies exec under the
    // full stack. Refuse here with a clear message rather than letting every
    // later `env run` fail on EACCES.
    sandbox::verify_exec(&policy)?;
    let policy_digest = policy.digest()?;

    // Pin the immutable base.
    let rev = opts.from.as_deref().unwrap_or("HEAD");
    let base_commit = repo
        .revparse_single(rev)
        .and_then(|o| o.peel_to_commit())
        .map_err(|e| H5iError::Metadata(format!("cannot resolve base revision '{rev}': {e}")))?;
    let base_tree = base_commit.tree()?.id();
    let parent_branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_owned))
        .unwrap_or_else(|| base_commit.id().to_string());

    // Code branch + native git worktree (§4). The worktree lives under
    // `.git/.h5i/env/<agent>/<slug>/work`, invisible to the main working tree.
    repo.branch(&branch_short, &base_commit, false)?;
    let work_path = dir.join(WORK_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let wt_name = format!("h5i-env-{agent}-{slug}");
    {
        let branch_ref = repo.find_reference(&branch_full)?;
        let mut wt_opts = git2::WorktreeAddOptions::new();
        wt_opts.reference(Some(&branch_ref));
        let wt = repo.worktree(&wt_name, &work_path, Some(&wt_opts)).map_err(|e| {
            H5iError::Metadata(format!("worktree creation failed for {id}: {e}"))
        })?;
        // Lock the worktree for the env's whole life so a stray
        // `git worktree prune` can't reclaim a live env out from under it;
        // `h5i env gc` is the only thing that unlocks+prunes it (and only when
        // applied/aborted).
        let _ = wt.lock(Some(&format!("h5i env {id} live")));
    }

    // Reasoning branch: fork from the parent worktree's current context branch
    // WITHOUT switching the parent, then pin the env worktree onto it.
    let parent_ctx = crate::ctx::current_branch(workdir);
    let env_ctx = format!("env/{agent}/{slug}");
    crate::ctx::fork_branch_no_switch(
        repo,
        &env_ctx,
        &parent_ctx,
        &format!("h5i environment {id} (profile {}, isolation {})", profile.name, policy.claim.as_str()),
    )?;
    let wt_repo = Repository::open(&work_path)?;
    crate::ctx::pin_worktree_context(&wt_repo, &env_ctx)?;

    let manifest = EnvManifest {
        id: id.clone(),
        agent: agent.to_string(),
        slug: slug.to_string(),
        base_commit: base_commit.id().to_string(),
        base_tree: base_tree.to_string(),
        parent_branch,
        branch: branch_full,
        parent_context_branch: parent_ctx,
        context_branch: env_ctx,
        profile: profile.name.clone(),
        policy_digest: policy_digest.clone(),
        isolation_claim: policy.claim.as_str().to_string(),
        backend: backend.to_string(),
        created_at: now_ts(),
        updated_at: now_ts(),
        status: ST_CREATED.to_string(),
        captures: Vec::new(),
    };

    let policy_toml = policy.to_toml()?;
    let policy_path = dir.join(POLICY_RESOLVED_FILE);
    std::fs::write(&policy_path, &policy_toml)
        .map_err(|e| H5iError::with_path(e, &policy_path))?;
    save_manifest(h5i_root, &manifest)?;
    // Mirror the manifest AND the resolved policy into refs/h5i/env so the
    // whole environment is shareable from creation.
    append_env_commit(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: id,
            agent: agent.to_string(),
            event: "created".into(),
            detail: Some(format!(
                "base={} profile={} isolation={} backend={backend}",
                &manifest.base_commit[..12.min(manifest.base_commit.len())],
                manifest.profile,
                manifest.isolation_claim
            )),
            capture: None,
        },
        Some(&manifest),
        Some(&policy_toml),
    )?;
    Ok(manifest)
}

// ─── run (§9): capture-wrapped, policy-enforced ─────────────────────────────

pub struct RunOutcome {
    /// Object id of the evidence capture in `refs/h5i/objects`.
    pub capture_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Wall-clock duration (ms).
    pub wall_ms: u128,
    /// CPU time consumed (ms).
    pub cpu_ms: u128,
    /// Peak resident set size (KiB), when the platform reports it.
    pub max_rss_kb: Option<i64>,
    /// The capture manifest (for rendering).
    pub manifest: objects::Manifest,
}

/// Whether this env's workspace is materialized locally. A `false` means the
/// env was created on another clone and pulled here (no `work/`), or gc'd —
/// such an env supports review/apply (which operate on the pushed code branch)
/// but not run/propose/rebase (which need the worktree).
pub fn has_workspace(m: &EnvManifest, h5i_root: &Path) -> bool {
    m.work_dir(h5i_root).is_dir()
}

/// A uniform error for operations that need a local worktree the env lacks.
fn no_workspace_err(m: &EnvManifest, op: &str) -> H5iError {
    H5iError::Metadata(format!(
        "{}: no local workspace for `{op}` — this environment lives on another clone (or was \
         gc'd). You can review it (`h5i env diff/status/inspect {}`) and `h5i env apply {}` \
         from the pushed code branch, but run/propose/rebase need the originating clone.",
        m.id, m.slug, m.slug
    ))
}

/// Run `argv` inside the env's worktree under its pinned policy, and record
/// the execution as evidence (a tagged capture). Every exec is captured —
/// provenance is the point (§8) — regardless of output size.
pub fn run(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    argv: &[String],
) -> Result<RunOutcome, H5iError> {
    match m.status.as_str() {
        ST_CREATED | ST_RUNNING | ST_IDLE => {}
        other => {
            return Err(H5iError::Metadata(format!(
                "{} is '{other}' — `env run` is only valid before propose/apply/abort",
                m.id
            )))
        }
    }
    let work = m.work_dir(h5i_root);
    if !work.is_dir() {
        return Err(no_workspace_err(m, "env run"));
    }

    // Serialize concurrent runs of THIS env (status + captures are mutated
    // below and must not interleave). Held for the duration of the run.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;

    // The stored policy, digest-verified, then re-resolved against a fresh
    // host probe (fail closed if the host can no longer satisfy the claim).
    let policy = load_policy(h5i_root, m)?;

    // Broker any declared secrets BEFORE marking the env running, so a
    // fail-closed grant (missing source, unsupported inject) aborts cleanly
    // without leaving the env in 'running'. `brokered` lives for the whole run;
    // its Drop guard unlinks any file-injected secrets on every exit path.
    let secret_dir = m.dir(h5i_root).join("secrets");
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered =
        crate::secrets_broker::broker(&policy.profile.secret_grants, &secret_dir, is_workspace)?;

    set_status(repo, h5i_root, m, ST_RUNNING, "status", Some("running".into()), None)?;
    let result = sandbox::run_with_env(&policy, &work, argv, &brokered.env);
    // Whatever happened, leave the running state before propagating errors.
    let outcome = match result {
        Ok(o) => o,
        Err(e) => {
            set_status(repo, h5i_root, m, ST_IDLE, "status", Some("idle (run failed to start)".into()), None)?;
            return Err(e);
        }
    };

    // Compose the raw payload exactly like `h5i capture run` (stdout, then a
    // labeled stderr block), plus an explicit marker when the wall-clock
    // kill fired — the evidence must say WHY the run ended.
    let mut raw: Vec<u8> = Vec::with_capacity(outcome.stdout.len() + outcome.stderr.len() + 64);
    raw.extend_from_slice(&outcome.stdout);
    if !outcome.stderr.is_empty() {
        if !raw.is_empty() && !raw.ends_with(b"\n") {
            raw.push(b'\n');
        }
        raw.extend_from_slice(b"\n----- stderr -----\n");
        raw.extend_from_slice(&outcome.stderr);
    }
    if outcome.timed_out {
        raw.extend_from_slice(b"\n----- h5i env: killed by wall-clock limit -----\n");
    }

    // Scrub brokered secret values from the evidence by exact match, on top of
    // the pattern-based redaction the capture already applies — a token echoed
    // to stdout must never reach refs/h5i/objects even if it matches no pattern.
    if !brokered.redactions.is_empty() {
        let mut text = String::from_utf8_lossy(&raw).into_owned();
        for v in &brokered.redactions {
            text = text.replace(v, "[redacted secret]");
        }
        raw = text.into_bytes();
    }

    // Capture against the WORKTREE repo so branch/diff context is the env's.
    let wt_repo = Repository::open(&work)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let filter = crate::token_filter::FilterConfig {
        cmd: Some(argv.to_vec()),
        ..Default::default()
    };
    let capture_opts = objects::CaptureOptions {
        kind: crate::token_filter::OutputKind::Auto,
        cmd: Some(argv.join(" ")),
        cwd: Some(work.display().to_string()),
        exit_code: outcome.exit_code,
        git_tree: head_tree,
        files: Vec::new(),
        cmd_argv: argv.to_vec(),
        filter,
        env_id: Some(m.id.clone()),
        policy_digest: Some(m.policy_digest.clone()),
        // Network egress verdicts (container tier's allowlist proxy); `None` for
        // workspace/process. This is the dashboard's NET-lane evidence.
        egress: outcome.egress.clone(),
        // Evidence is shared via `h5i objects push` — scrub secrets from the
        // stored blob and summary before it is written (design §7).
        redact: true,
    };
    let captured = objects::capture(&wt_repo, h5i_root, &raw, capture_opts)?;
    let capture_id = captured.manifest.id.clone();

    m.captures.push(capture_id.clone());
    // The event log (refs/h5i/env) travels via `h5i push`, so the command —
    // which can carry a credential passed as an argument — is scrubbed before
    // it lands in the detail, exactly like the capture's cmd field.
    let safe_cmd = crate::secrets::redact_text(&argv.join(" "));
    let rss = outcome
        .max_rss_kb
        .map(|kb| format!(" rss={}MiB", kb / 1024))
        .unwrap_or_default();
    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "exec",
        Some(format!(
            "cmd=`{}` exit={} wall={}ms cpu={}ms{}{}",
            safe_cmd,
            outcome.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            outcome.wall_ms,
            outcome.cpu_ms,
            rss,
            if outcome.timed_out { " timed-out" } else { "" }
        )),
        Some(capture_id.clone()),
    )?;

    // Audit each delivered secret grant (id + source + inject + fingerprint —
    // never the value), tied to the capture it was used in.
    for rec in &brokered.records {
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "secret".into(),
                detail: Some(rec.detail()),
                capture: Some(capture_id.clone()),
            },
        )?;
    }

    Ok(RunOutcome {
        capture_id,
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        wall_ms: outcome.wall_ms,
        cpu_ms: outcome.cpu_ms,
        max_rss_kb: outcome.max_rss_kb,
        manifest: captured.manifest,
    })
}

// ─── shell (agent-in-box) ────────────────────────────────────────────────────

/// Run an **interactive** session (a shell, or a coding agent) inside the env,
/// confined by the box. stdio is inherited (a real terminal), so every command
/// the session spawns is contained by construction — the enforcement no longer
/// relies on the agent prefixing each command with `env run`. Unlike [`run`]
/// nothing is captured (it's interactive); a single `shell` event records that a
/// session ran and its exit code. Returns the child's exit code.
pub fn shell(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    argv: &[String],
) -> Result<i32, H5iError> {
    match m.status.as_str() {
        ST_CREATED | ST_RUNNING | ST_IDLE => {}
        other => {
            return Err(H5iError::Metadata(format!(
                "{} is '{other}' — `env shell` is only valid before propose/apply/abort",
                m.id
            )))
        }
    }
    let work = m.work_dir(h5i_root);
    if !work.is_dir() {
        return Err(no_workspace_err(m, "env shell"));
    }

    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;

    let policy = load_policy(h5i_root, m)?;
    let secret_dir = m.dir(h5i_root).join("secrets");
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered =
        crate::secrets_broker::broker(&policy.profile.secret_grants, &secret_dir, is_workspace)?;

    set_status(repo, h5i_root, m, ST_RUNNING, "status", Some("running (shell)".into()), None)?;
    let exit_code = match sandbox::run_interactive(&policy, &work, argv, &brokered.env) {
        Ok(code) => code,
        Err(e) => {
            set_status(repo, h5i_root, m, ST_IDLE, "status", Some("idle (shell failed to start)".into()), None)?;
            return Err(e);
        }
    };

    let safe_cmd = crate::secrets::redact_text(&argv.join(" "));
    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "shell",
        Some(format!("interactive cmd=`{safe_cmd}` exit={exit_code}")),
        None,
    )?;

    // Audit each delivered secret grant (id + source + inject + fingerprint).
    for rec in &brokered.records {
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "secret".into(),
                detail: Some(rec.detail()),
                capture: None,
            },
        )?;
    }
    Ok(exit_code)
}

// ─── diff ───────────────────────────────────────────────────────────────────

/// Unified diff of the env's changes against its pinned base tree.
///
/// When the worktree is present (the originating clone) this is the live
/// working-tree diff (committed + uncommitted, including untracked files).
/// When it is absent (a pulled "remote" env, or after gc) it falls back to the
/// **committed** state on the env's code branch — i.e. what `propose`
/// snapshotted — so a reviewer on another clone sees exactly the proposed diff.
pub fn diff(repo: &Repository, h5i_root: &Path, m: &EnvManifest, stat_only: bool) -> Result<String, H5iError> {
    let render = |diff: git2::Diff| -> Result<String, H5iError> {
        if stat_only {
            let stats = diff.stats()?;
            let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
            return Ok(buf.as_str().unwrap_or("").to_string());
        }
        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |_d, _h, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                out.push(line.origin());
            }
            out.push_str(&String::from_utf8_lossy(line.content()));
            true
        })?;
        Ok(out)
    };

    let work = m.work_dir(h5i_root);
    if work.is_dir() {
        let wt_repo = Repository::open(&work)?;
        let base_tree = wt_repo.find_tree(git2::Oid::from_str(&m.base_tree)?)?;
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true);
        let diff = wt_repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
        render(diff)
    } else {
        // Remote/no-worktree: diff base_tree → env branch tip (the committed,
        // proposed state) using the shared object store.
        let base_tree = repo.find_tree(git2::Oid::from_str(&m.base_tree)?)?;
        let tip_tree = repo
            .find_reference(&m.branch)
            .map_err(|_| H5iError::Metadata(format!(
                "{}: env code branch '{}' is not present locally — `h5i pull` it first",
                m.id, m.branch
            )))?
            .peel_to_tree()?;
        let mut opts = git2::DiffOptions::new();
        let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&tip_tree), Some(&mut opts))?;
        render(diff)
    }
}

// ─── base drift (§9) ────────────────────────────────────────────────────────

/// How an env's pinned base relates to its parent branch's current tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Drift {
    /// The parent branch still points at the env's pinned base.
    UpToDate,
    /// The parent advanced; the base is an ancestor of the new tip.
    /// `commits` is how many commits the parent is ahead — `env rebase` can
    /// fast-forward the env's base onto it.
    ParentAhead { tip: String, commits: usize },
    /// The parent diverged or was rewound (the base is not an ancestor of the
    /// tip). Manual intervention; `rebase` still attempts a 3-way merge.
    Diverged { tip: String },
    /// The parent branch no longer exists (renamed/deleted).
    ParentGone,
}

impl Drift {
    pub fn is_current(&self) -> bool {
        matches!(self, Drift::UpToDate)
    }
    /// One-line human summary.
    pub fn summary(&self) -> String {
        match self {
            Drift::UpToDate => "up to date with parent".into(),
            Drift::ParentAhead { commits, tip } => format!(
                "parent advanced {commits} commit{} (now {}) — `h5i env rebase` to refresh the base",
                if *commits == 1 { "" } else { "s" },
                &tip[..12.min(tip.len())]
            ),
            Drift::Diverged { tip } => format!(
                "parent diverged from the base (now {}) — `h5i env rebase` will 3-way merge",
                &tip[..12.min(tip.len())]
            ),
            Drift::ParentGone => "parent branch is gone".into(),
        }
    }
}

/// Compute how `m`'s pinned base relates to its parent branch's current tip.
pub fn drift(repo: &Repository, m: &EnvManifest) -> Drift {
    let Ok(reference) = repo.find_reference(&format!("refs/heads/{}", m.parent_branch)) else {
        return Drift::ParentGone;
    };
    let Some(tip) = reference.peel_to_commit().ok().map(|c| c.id()) else {
        return Drift::ParentGone;
    };
    let Ok(base) = git2::Oid::from_str(&m.base_commit) else {
        return Drift::Diverged { tip: tip.to_string() };
    };
    if tip == base {
        return Drift::UpToDate;
    }
    // base an ancestor of tip → parent simply moved forward.
    if repo.graph_descendant_of(tip, base).unwrap_or(false) {
        let commits = repo
            .graph_ahead_behind(tip, base)
            .map(|(ahead, _)| ahead)
            .unwrap_or(0);
        Drift::ParentAhead { tip: tip.to_string(), commits }
    } else {
        Drift::Diverged { tip: tip.to_string() }
    }
}

// ─── status (human view) ────────────────────────────────────────────────────

/// A human-readable status report for one environment: identity, lifecycle,
/// the policy actually enforced, evidence, and base drift.
pub fn status_report(repo: &Repository, h5i_root: &Path, m: &EnvManifest) -> String {
    let mut out = String::new();
    out.push_str(&format!("── {} ──\n", m.id));
    out.push_str(&format!("  status   : {}\n", m.status));
    out.push_str(&format!("  agent    : {}\n", m.agent));
    out.push_str(&format!(
        "  base     : {} (from {})\n",
        &m.base_commit[..12.min(m.base_commit.len())],
        m.parent_branch
    ));
    out.push_str(&format!("  branch   : {}\n", m.branch));
    out.push_str(&format!("  context  : {}\n", m.context_branch));
    out.push_str(&format!(
        "  policy   : profile={} isolation={} digest={}\n",
        m.profile,
        m.isolation_claim,
        &m.policy_digest[..12.min(m.policy_digest.len())]
    ));
    // Resolved policy details when readable (digest-verified).
    if let Ok(policy) = load_policy(h5i_root, m) {
        let p = &policy.profile;
        out.push_str(&format!(
            "  enforce  : net.mode={:?} fs.write={:?} mem={} procs={} wall={}s{}{}\n",
            p.net_mode,
            p.fs_write,
            p.mem_bytes.map(|b| format!("{}MiB", b / 1024 / 1024)).unwrap_or_else(|| "∞".into()),
            p.max_procs.map(|n| n.to_string()).unwrap_or_else(|| "∞".into()),
            p.wall_secs,
            p.fsize_bytes.map(|b| format!(" fsize={}MiB", b / 1024 / 1024)).unwrap_or_default(),
            p.cpu_secs.map(|s| format!(" cpu={s}s")).unwrap_or_default(),
        ));
        if !p.tools.is_empty() {
            out.push_str(&format!("  tools    : {}\n", p.tools.join(", ")));
        }
    }
    out.push_str(&format!(
        "  evidence : {} capture(s){}\n",
        m.captures.len(),
        if m.captures.is_empty() { String::new() } else { format!(": {}", m.captures.join(", ")) }
    ));
    let d = drift(repo, m);
    let marker = if d.is_current() { "✓" } else { "⚠" };
    out.push_str(&format!("  drift    : {marker} {}\n", d.summary()));
    out
}

// ─── inspect (§9) ───────────────────────────────────────────────────────────

/// Render one of an environment's evidence captures: its structured findings
/// (or text summary), exit code, policy digest, and any redactions. The
/// capture must belong to this env — a capture id from another env is refused
/// so `inspect` can't be used to read unrelated evidence.
pub fn inspect(repo: &Repository, m: &EnvManifest, capture_id: &str) -> Result<String, H5iError> {
    let manifest = objects::resolve_manifest(repo, capture_id)?;
    if manifest.env_id.as_deref() != Some(m.id.as_str()) {
        return Err(H5iError::Metadata(format!(
            "capture {} is not evidence for {} (it belongs to {})",
            capture_id,
            m.id,
            manifest.env_id.as_deref().unwrap_or("<none>")
        )));
    }
    let mut out = String::new();
    out.push_str(&format!("── Capture {} ({}) ──\n", manifest.id, m.id));
    if let Some(cmd) = &manifest.cmd {
        out.push_str(&format!("  cmd      : {cmd}\n"));
    }
    out.push_str(&format!(
        "  exit     : {}\n",
        manifest.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
    ));
    if let Some(d) = &manifest.policy_digest {
        out.push_str(&format!("  policy   : {}\n", &d[..12.min(d.len())]));
    }
    if !manifest.redactions.is_empty() {
        out.push_str(&format!("  redacted : {}\n", manifest.redactions.join(", ")));
    }
    out.push_str(&format!(
        "  raw      : {} bytes, {} lines (object {})\n",
        manifest.raw_size, manifest.raw_lines, manifest.raw_oid
    ));
    out.push('\n');
    match &manifest.structured {
        Some(s) => out.push_str(&crate::structured::render_compact(s)),
        None => out.push_str(&manifest.summary),
    }
    out.push('\n');
    Ok(out)
}

// ─── the arena: compare N envs from one base (§9) ───────────────────────────

/// One environment's row in a comparison: how much it changed and how its
/// latest run fared. The reviewer-comparison resolution the design calls out
/// as h5i-unique — `msg` coordinates the agents, `objects` supplies each env's
/// test results, and this folds them into one view.
#[derive(Debug, Clone, Serialize)]
pub struct CompareRow {
    pub id: String,
    pub status: String,
    pub base_commit: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    /// Latest run's exit code, if any run has happened.
    pub last_exit: Option<i32>,
    /// The tool of the latest capture (e.g. `pytest`, `cargo`), if structured.
    pub last_tool: Option<String>,
    /// The latest capture's structured status (e.g. `pass`/`fail`).
    pub last_result: Option<String>,
    /// Selected counts from the latest capture (e.g. passed/failed), compacted.
    pub last_counts: std::collections::BTreeMap<String, u64>,
}

/// Build comparison rows for the named environments.
pub fn compare(
    repo: &Repository,
    h5i_root: &Path,
    names: &[String],
) -> Result<Vec<CompareRow>, H5iError> {
    let mut rows = Vec::new();
    for name in names {
        let m = find(h5i_root, name)?;
        let (files_changed, insertions, deletions) = diffstat_numbers(repo, h5i_root, &m).unwrap_or((0, 0, 0));
        let (last_exit, last_tool, last_result, last_counts) = match m.captures.last() {
            Some(cap) => match objects::resolve_manifest(repo, cap) {
                Ok(man) => {
                    let (tool, result, counts) = match &man.structured {
                        Some(s) => (
                            Some(s.tool.clone()),
                            Some(format!("{:?}", s.status).to_lowercase()),
                            s.counts.clone(),
                        ),
                        None => (None, None, Default::default()),
                    };
                    (man.exit_code, tool, result, counts)
                }
                Err(_) => (None, None, None, Default::default()),
            },
            None => (None, None, None, Default::default()),
        };
        rows.push(CompareRow {
            id: m.id,
            status: m.status,
            base_commit: m.base_commit,
            files_changed,
            insertions,
            deletions,
            last_exit,
            last_tool,
            last_result,
            last_counts,
        });
    }
    Ok(rows)
}

/// `(files_changed, insertions, deletions)` of an env's changes vs. its pinned
/// base. Uses the worktree when present, else the env branch tip (so pulled
/// "remote" envs still compare).
fn diffstat_numbers(repo: &Repository, h5i_root: &Path, m: &EnvManifest) -> Option<(usize, usize, usize)> {
    let triple = |diff: &git2::Diff| {
        diff.stats()
            .ok()
            .map(|s| (s.files_changed(), s.insertions(), s.deletions()))
    };
    let work = m.work_dir(h5i_root);
    if work.is_dir() {
        let wt_repo = Repository::open(&work).ok()?;
        let base_tree = wt_repo.find_tree(git2::Oid::from_str(&m.base_tree).ok()?).ok()?;
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = wt_repo
            .diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))
            .ok()?;
        triple(&diff)
    } else {
        let base_tree = repo.find_tree(git2::Oid::from_str(&m.base_tree).ok()?).ok()?;
        let tip_tree = repo.find_reference(&m.branch).ok()?.peel_to_tree().ok()?;
        let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&tip_tree), None).ok()?;
        triple(&diff)
    }
}

/// Render comparison rows as a human-readable table, flagging when the
/// environments do not share a common base (so the comparison is apples-to-
/// apples only when they do).
pub fn render_compare(rows: &[CompareRow]) -> String {
    let mut out = String::new();
    let distinct_bases: HashSet<&str> = rows.iter().map(|r| r.base_commit.as_str()).collect();
    out.push_str("── Arena: environment comparison ──\n");
    if distinct_bases.len() > 1 {
        out.push_str(
            "  ⚠ environments do NOT share a base commit — diffs are not directly comparable\n",
        );
    } else if let Some(b) = distinct_bases.iter().next() {
        out.push_str(&format!("  common base: {}\n", &b[..12.min(b.len())]));
    }
    out.push_str(&format!(
        "  {:<26} {:<9} {:>7} {:>7} {:>7}  {}\n",
        "env", "status", "files", "+", "-", "latest run"
    ));
    for r in rows {
        let run = match (&r.last_tool, r.last_exit, &r.last_result) {
            (Some(tool), exit, result) => {
                let counts: Vec<String> = r
                    .last_counts
                    .iter()
                    .filter(|(k, _)| matches!(k.as_str(), "passed" | "failed" | "errors" | "warnings"))
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                format!(
                    "{tool} {} (exit {}){}",
                    result.clone().unwrap_or_default(),
                    exit.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
                    if counts.is_empty() { String::new() } else { format!(" [{}]", counts.join(" ")) }
                )
            }
            _ => "— (no run yet)".to_string(),
        };
        out.push_str(&format!(
            "  {:<26} {:<9} {:>7} {:>7} {:>7}  {}\n",
            r.id, r.status, r.files_changed, r.insertions, r.deletions, run
        ));
    }
    out.push_str("\nPick a winner with `h5i env diff <name>` / `h5i env inspect <name> --capture <id>`, then `h5i env apply <name>`.\n");
    out
}

// ─── mediated commit (§4 — the critical security boundary) ─────────────────

/// Snapshot the env worktree onto the env branch **host-side**: h5i stages and
/// commits; the agent never drives `git` at `process`+ tiers. Every staged
/// path is validated against the canonicalized-`$WORK` allowlist invariant —
/// symlink escapes, nested `.git` repos / submodule gitlinks, and `..`
/// traversal are rejected and the whole commit **fails closed**.
///
/// Returns `Ok(None)` when the worktree is identical to the branch tip.
///
/// `repo` is the primary repository (not the worktree): a fail-closed boundary
/// trip is recorded as a `violation` event in `refs/h5i/env` so the refusal is a
/// permanent, shareable part of the env's provenance — the single
/// highest-confidence "agent probed the sandbox" signal the dashboard surfaces.
pub fn mediated_commit(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
) -> Result<Option<git2::Oid>, H5iError> {
    let work = m.work_dir(h5i_root);
    if !work.is_dir() {
        return Err(no_workspace_err(m, "propose/rebase"));
    }
    let wt_repo = Repository::open(&work)?;
    let canon_work = work.canonicalize().map_err(|e| H5iError::with_path(e, &work))?;

    // Pre-walk for nested git repositories. libgit2 either errors opaquely or
    // records a submodule gitlink when `add_all` meets a directory containing
    // `.git` — both are wrong here. Detect them OURSELVES, first, and refuse
    // with a precise diagnostic (fail closed).
    let mut violations: Vec<String> = scan_nested_git(&canon_work);
    if !violations.is_empty() {
        return Err(record_commit_violation(repo, m, violations));
    }

    let mut index = wt_repo.index()?;

    {
        let mut cb = |path: &Path, _matched: &[u8]| -> i32 {
            match staged_path_violation(&canon_work, path) {
                None => 0,    // stage it
                Some(v) => {
                    violations.push(v);
                    1 // skip — but any violation fails the commit below
                }
            }
        };
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, Some(&mut cb as &mut git2::IndexMatchedPath))?;
        index.update_all(["*"].iter(), None)?;
    }

    // Post-stage sweep: reject submodule gitlink entries (mode 160000) that
    // libgit2 may have recorded for a nested repo — an agent could otherwise
    // smuggle a pointer to an arbitrary commit.
    for entry in index.iter() {
        if entry.mode == 0o160000 {
            violations.push(format!(
                "{}: nested git repository (gitlink) — not allowed in a mediated commit",
                String::from_utf8_lossy(&entry.path)
            ));
        }
    }

    if !violations.is_empty() {
        return Err(record_commit_violation(repo, m, violations));
    }

    let tree_oid = index.write_tree()?;
    let head = wt_repo.head()?.peel_to_commit()?;
    if head.tree_id() == tree_oid {
        return Ok(None);
    }
    index.write()?;
    let tree = wt_repo.find_tree(tree_oid)?;
    let sig = objects::signature(&wt_repo)?;
    let oid = wt_repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("h5i env: mediated commit ({})", m.id),
        &tree,
        &[&head],
    )?;
    Ok(Some(oid))
}

/// Record a mediated-commit boundary trip as a `violation` event, then build the
/// fail-closed error to return. A boundary trip is the highest-confidence
/// sandbox-probe signal (enforcement actually fired), so it is persisted to
/// `refs/h5i/env` — durable and shareable via `h5i push` — not just surfaced as
/// a transient CLI error. Event-append failures never mask the refusal itself.
fn record_commit_violation(
    repo: &Repository,
    m: &EnvManifest,
    violations: Vec<String>,
) -> H5iError {
    let detail = format!(
        "mediated commit refused (fail-closed) — {} path violation(s): {}",
        violations.len(),
        // Redact: a path can embed a secret; this travels via `h5i push`.
        crate::secrets::redact_text(&violations.join("; "))
    );
    let _ = append_event(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: "violation".into(),
            detail: Some(detail),
            capture: None,
        },
    );
    H5iError::Metadata(format!(
        "mediated commit refused (fail-closed) — {} path violation(s):\n  - {}",
        violations.len(),
        violations.join("\n  - ")
    ))
}

/// Walk the worktree (without following symlinks) and report every nested
/// `.git` entry — a directory (embedded repo) or file (gitlink) anywhere
/// below the root. The root's own `.git` gitlink is the worktree's plumbing
/// and is exempt.
fn scan_nested_git(work: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name.eq_ignore_ascii_case(".git") {
                if dir == root {
                    continue; // the worktree's own gitlink
                }
                out.push(format!(
                    "{}: nested git repository — not allowed in a mediated commit",
                    path.strip_prefix(root).unwrap_or(&path).display()
                ));
                continue;
            }
            let Ok(md) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if md.is_dir() {
                walk(&path, root, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(work, work, &mut out);
    out
}

/// The path filter behind the mediated-commit invariant. `rel` is the
/// repo-relative path libgit2 wants to stage; returns a human-readable
/// violation, or `None` when the path is safe.
fn staged_path_violation(canon_work: &Path, rel: &Path) -> Option<String> {
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(c) => {
                if c.eq_ignore_ascii_case(".git") {
                    return Some(format!("{}: contains a '.git' component", rel.display()));
                }
            }
            std::path::Component::ParentDir => {
                return Some(format!("{}: '..' traversal", rel.display()));
            }
            std::path::Component::CurDir => {}
            _ => return Some(format!("{}: non-relative path", rel.display())),
        }
    }
    let abs = canon_work.join(rel);
    let md = match std::fs::symlink_metadata(&abs) {
        Ok(md) => md,
        Err(_) => return Some(format!("{}: vanished while staging", rel.display())),
    };
    if md.file_type().is_symlink() {
        // A symlink is stored AS a link blob (never followed) — safe even when
        // its target points outside $WORK.
        return None;
    }
    // Canonicalize to catch directory-symlink traversal: the file's real
    // location must stay under $WORK.
    match abs.canonicalize() {
        Ok(canon) if canon.starts_with(canon_work) => None,
        Ok(canon) => Some(format!(
            "{}: escapes $WORK via symlinked parent (resolves to {})",
            rel.display(),
            canon.display()
        )),
        Err(e) => Some(format!("{}: cannot canonicalize ({e})", rel.display())),
    }
}

// ─── propose / apply / abort / gc (§9) ──────────────────────────────────────

/// Mediated-commit the worktree, mark the env `proposed`, and return a review
/// brief. Never touches the parent branch.
pub fn propose(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
) -> Result<String, H5iError> {
    match m.status.as_str() {
        ST_CREATED | ST_RUNNING | ST_IDLE | ST_PROPOSED => {}
        other => {
            return Err(H5iError::Metadata(format!(
                "{} is '{other}' — nothing to propose",
                m.id
            )))
        }
    }
    let commit = mediated_commit(repo, h5i_root, m)?;
    let stat = diff(repo, h5i_root, m, true).unwrap_or_default();
    set_status(
        repo,
        h5i_root,
        m,
        ST_PROPOSED,
        "proposed",
        Some(match commit {
            Some(oid) => format!("snapshot={oid}"),
            None => "no new changes (worktree == branch tip)".into(),
        }),
        None,
    )?;

    let mut brief = String::new();
    brief.push_str(&format!("── Proposal: {} ──\n", m.id));
    brief.push_str(&format!("  base    : {} (from {})\n", &m.base_commit[..12], m.parent_branch));
    brief.push_str(&format!("  branch  : {}\n", m.branch));
    brief.push_str(&format!(
        "  policy  : profile={} isolation={} digest={}\n",
        m.profile,
        m.isolation_claim,
        &m.policy_digest[..12.min(m.policy_digest.len())]
    ));
    brief.push_str(&format!("  evidence: {} capture(s): {}\n", m.captures.len(), m.captures.join(", ")));
    let d = drift(repo, m);
    if !d.is_current() {
        brief.push_str(&format!("  drift   : ⚠ {}\n", d.summary()));
    }
    if !stat.trim().is_empty() {
        brief.push_str("  diff    :\n");
        for line in stat.lines() {
            brief.push_str(&format!("    {line}\n"));
        }
    } else {
        brief.push_str("  diff    : (no changes against base)\n");
    }
    brief.push_str(&format!(
        "\nReview with `h5i env diff {}`, then `h5i env apply {}` (reviewer-selected; never automatic).\n",
        m.slug, m.slug
    ));
    Ok(brief)
}

/// Apply a proposed env onto its parent branch. Explicit, reviewer-driven:
/// requires the parent branch checked out and a clean tracked working tree.
/// `--patch` squashes the env's diff into one commit; the default `--merge`
/// fast-forwards or creates a two-parent merge commit. Conflicts refuse.
/// Afterwards the env's reasoning branch is merged back into the parent
/// context branch.
pub fn apply(
    repo: &Repository,
    h5i_root: &Path,
    workdir: &Path,
    m: &mut EnvManifest,
    patch_mode: bool,
) -> Result<String, H5iError> {
    if m.status != ST_PROPOSED {
        return Err(H5iError::Metadata(format!(
            "{} is '{}' — run `h5i env propose {}` first (apply is never automatic)",
            m.id, m.status, m.slug
        )));
    }

    // The reviewer must be ON the parent branch with a clean tracked tree.
    let head = repo.head()?;
    let current = head.shorthand().unwrap_or("").to_string();
    if current != m.parent_branch {
        return Err(H5iError::Metadata(format!(
            "apply runs from the parent branch '{}' (currently on '{current}') — check it out first",
            m.parent_branch
        )));
    }
    let mut st_opts = git2::StatusOptions::new();
    st_opts.include_untracked(false).include_ignored(false);
    let statuses = repo.statuses(Some(&mut st_opts))?;
    if !statuses.is_empty() {
        return Err(H5iError::Metadata(
            "working tree has uncommitted tracked changes — commit or stash them before `env apply`"
                .into(),
        ));
    }

    let parent_tip = head.peel_to_commit()?;
    let env_tip = repo.find_reference(&m.branch)?.peel_to_commit()?;
    if env_tip.id() == parent_tip.id() {
        set_status(repo, h5i_root, m, ST_APPLIED, "applied", Some("no-op (no divergence)".into()), None)?;
        return Ok(format!("{}: nothing to apply (env tip == parent tip)", m.id));
    }

    let base_oid = repo.merge_base(parent_tip.id(), env_tip.id())?;
    let new_commit: git2::Oid = if !patch_mode && base_oid == parent_tip.id() {
        // Fast-forward.
        env_tip.id()
    } else {
        let base_tree = repo.find_commit(base_oid)?.tree()?;
        let parent_tree = parent_tip.tree()?;
        let env_tree = env_tip.tree()?;
        let mut idx = repo.merge_trees(&base_tree, &parent_tree, &env_tree, None)?;
        if idx.has_conflicts() {
            let paths: Vec<String> = idx
                .conflicts()?
                .filter_map(|c| c.ok())
                .filter_map(|c| {
                    c.our
                        .as_ref()
                        .or(c.their.as_ref())
                        .or(c.ancestor.as_ref())
                        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
                })
                .collect();
            return Err(H5iError::Metadata(format!(
                "apply refused — merge conflicts in: {} (rebase the env or resolve on the env branch)",
                paths.join(", ")
            )));
        }
        let tree = repo.find_tree(idx.write_tree_to(repo)?)?;
        let sig = objects::signature(repo)?;
        let msg = if patch_mode {
            format!("h5i env apply --patch: {} → {}", m.id, m.parent_branch)
        } else {
            format!("h5i env apply: merge {} → {}", m.id, m.parent_branch)
        };
        let parents: Vec<&git2::Commit> = if patch_mode {
            vec![&parent_tip]
        } else {
            vec![&parent_tip, &env_tip]
        };
        repo.commit(None, &sig, &sig, &msg, &tree, &parents)?
    };

    // Update the (clean, pre-verified) working tree + index to the merged
    // result, THEN move the branch ref — moving the ref first and calling
    // checkout_head afterwards is the documented libgit2 anti-pattern.
    let obj = repo.find_object(new_commit, None)?;
    let mut co = CheckoutBuilder::new();
    co.safe();
    repo.checkout_tree(&obj, Some(&mut co))?;
    repo.reference(
        &format!("refs/heads/{}", m.parent_branch),
        new_commit,
        true,
        &format!("h5i env apply: {}", m.id),
    )?;

    // Fold the env's reasoning back into the parent context branch. The code
    // is already applied — a context-merge failure is surfaced, not fatal.
    let ctx_note = match crate::ctx::gcc_merge_into(workdir, &m.parent_context_branch, &m.context_branch)
    {
        Ok(_) => format!("context '{}' merged into '{}'", m.context_branch, m.parent_context_branch),
        Err(e) => format!(
            "WARNING: context merge-back failed ({e}) — run `h5i context merge {}` manually",
            m.context_branch
        ),
    };

    set_status(
        repo,
        h5i_root,
        m,
        ST_APPLIED,
        "applied",
        Some(format!(
            "{} {} → {} ({new_commit})",
            if patch_mode { "patch" } else { "merge" },
            m.branch_short(),
            m.parent_branch
        )),
        None,
    )?;
    Ok(format!(
        "{} applied onto {} ({}{})\n{}",
        m.id,
        m.parent_branch,
        &new_commit.to_string()[..12],
        if base_oid == parent_tip.id() && !patch_mode { ", fast-forward" } else { "" },
        ctx_note
    ))
}

/// Rebase the environment onto its parent branch's current tip (§9 — "the
/// parent must not mutate under active envs; if it does, h5i detects and offers
/// rebase"). The pinned base is immutable by default; this is the *sanctioned*
/// re-pin.
///
/// Steps: snapshot the worktree via the mediated commit, 3-way merge the env's
/// changes onto the new parent tip (refusing on conflict — resolve on the env
/// branch), commit the rebased state on the env branch, re-pin
/// `base_commit`/`base_tree` to the parent tip, and refresh the worktree to the
/// rebased tree. Only valid before propose/apply.
pub fn rebase(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
) -> Result<String, H5iError> {
    match m.status.as_str() {
        ST_CREATED | ST_RUNNING | ST_IDLE => {}
        other => {
            return Err(H5iError::Metadata(format!(
                "{} is '{other}' — rebase is only valid before propose/apply",
                m.id
            )))
        }
    }
    match drift(repo, m) {
        Drift::UpToDate => {
            return Ok(format!("{} is already on its parent tip — nothing to rebase", m.id))
        }
        Drift::ParentGone => {
            return Err(H5iError::Metadata(format!(
                "{}: parent branch '{}' is gone — cannot rebase",
                m.id, m.parent_branch
            )))
        }
        _ => {}
    }

    // Snapshot the worktree onto the env branch (host-side, path-allowlisted).
    mediated_commit(repo, h5i_root, m)?;

    let work = m.work_dir(h5i_root);
    let wt_repo = Repository::open(&work)?;
    let env_tip = wt_repo.head()?.peel_to_commit()?;
    let parent_tip = repo
        .find_reference(&format!("refs/heads/{}", m.parent_branch))?
        .peel_to_commit()?;
    // Re-open the parent tip in the worktree repo (shared object store) so all
    // objects are reachable from one handle.
    let parent_tip = wt_repo.find_commit(parent_tip.id())?;
    let old_base = wt_repo.find_commit(git2::Oid::from_str(&m.base_commit)?)?;

    // 3-way merge: ancestor = old base, ours = parent tip, theirs = env work.
    let mut idx = wt_repo.merge_trees(
        &old_base.tree()?,
        &parent_tip.tree()?,
        &env_tip.tree()?,
        None,
    )?;
    if idx.has_conflicts() {
        let paths: Vec<String> = idx
            .conflicts()?
            .filter_map(|c| c.ok())
            .filter_map(|c| {
                c.our
                    .as_ref()
                    .or(c.their.as_ref())
                    .or(c.ancestor.as_ref())
                    .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            })
            .collect();
        return Err(H5iError::Metadata(format!(
            "rebase refused — conflicts against the new base in: {} (resolve on the env branch, \
             or apply against the old base)",
            paths.join(", ")
        )));
    }
    let merged_tree = wt_repo.find_tree(idx.write_tree_to(&wt_repo)?)?;

    // Commit the rebased state on the env branch: a 2-parent commit (env work +
    // new parent tip) so provenance shows what it was folded onto.
    let sig = objects::signature(&wt_repo)?;
    let rebased = wt_repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("h5i env rebase: {} onto {}", m.id, m.parent_branch),
        &merged_tree,
        &[&env_tip, &parent_tip],
    )?;

    // Refresh the worktree to the rebased tree (it's clean after the mediated
    // commit), then re-pin the base to the parent tip.
    let obj = wt_repo.find_object(rebased, None)?;
    let mut co = CheckoutBuilder::new();
    co.force();
    wt_repo.checkout_tree(&obj, Some(&mut co))?;

    m.base_commit = parent_tip.id().to_string();
    m.base_tree = parent_tip.tree()?.id().to_string();

    set_status(
        repo,
        h5i_root,
        m,
        if m.status == ST_CREATED { ST_CREATED } else { ST_IDLE },
        "rebased",
        Some(format!(
            "onto {} ({})",
            m.parent_branch,
            &parent_tip.id().to_string()[..12]
        )),
        None,
    )?;
    Ok(format!(
        "{} rebased onto {} ({}) — base re-pinned",
        m.id,
        m.parent_branch,
        &parent_tip.id().to_string()[..12]
    ))
}

/// Stop the env: mark it aborted and preserve the manifest + workspace for
/// forensics (`gc` reclaims the workspace later).
pub fn abort(repo: &Repository, h5i_root: &Path, m: &mut EnvManifest) -> Result<(), H5iError> {
    if m.status == ST_APPLIED {
        return Err(H5iError::Metadata(format!("{} is already applied — nothing to abort", m.id)));
    }
    set_status(repo, h5i_root, m, ST_ABORTED, "aborted", Some("manifest preserved for forensics".into()), None)
}

/// Prune one env's git worktree registration and remove its `work/` directory.
/// Idempotent: a missing worktree or `work/` is a no-op. Shared by `gc`
/// (workspace-only reclaim) and `rm` (full removal). Returns `Err` if the
/// worktree prune itself fails, leaving the workspace in place for a retry.
fn prune_workspace(repo: &Repository, h5i_root: &Path, m: &EnvManifest) -> Result<(), H5iError> {
    if let Ok(wt) = repo.find_worktree(&m.worktree_name()) {
        // The worktree is locked for the env's life; we are intentionally
        // reclaiming it now, so override the lock (locked(true)).
        let _ = wt.unlock();
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).locked(true).working_tree(true);
        wt.prune(Some(&mut opts))?;
    }
    let work = m.work_dir(h5i_root);
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| H5iError::with_path(e, &work))?;
    }
    Ok(())
}

/// Reclaim workspaces of applied/aborted envs: prune the git worktree and
/// remove the `work/` directory. Manifests, policies, branches, context
/// branches, and captures are all retained — provenance is never gc'd here.
pub fn gc(repo: &Repository, h5i_root: &Path) -> Result<Vec<String>, H5iError> {
    let mut reclaimed = Vec::new();
    for mut m in list(h5i_root) {
        if m.status != ST_APPLIED && m.status != ST_ABORTED {
            continue;
        }
        if !m.work_dir(h5i_root).exists() {
            continue;
        }
        // A failed prune leaves this env for a later sweep rather than aborting
        // the whole gc; skip it and keep going.
        if prune_workspace(repo, h5i_root, &m).is_err() {
            continue;
        }
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "gc".into(),
                detail: Some("workspace reclaimed, manifest retained".into()),
                capture: None,
            },
        )?;
        save_manifest(h5i_root, &m)?; // status unchanged; refresh status file
        reclaimed.push(std::mem::take(&mut m.id));
    }
    Ok(reclaimed)
}

/// Permanently remove an environment from this clone: prune its worktree,
/// delete its code branch (`refs/heads/h5i/env/…`) and reasoning branch
/// (`refs/h5i/context/env/…`), and erase its on-disk dir (manifest, policy,
/// status). Unlike `gc` (workspace only) and `abort` (status only), this
/// destroys the *local* provenance — the env's manifest + policy lines are
/// stripped from `refs/h5i/env` (otherwise [`materialize_from_ref`], run at the
/// top of every `env` command, would rewrite the on-disk manifest right back),
/// leaving only the append-only `removed` event as the record. This removal is
/// local: a later `pull` from a peer that still holds the manifest can
/// re-introduce it via union-merge (no cross-clone tombstone yet).
///
/// `force` is required to remove a still-live env (created/running/idle/
/// proposed); applied/aborted envs remove freely.
pub fn rm(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    force: bool,
) -> Result<(), H5iError> {
    let live = matches!(m.status.as_str(), ST_CREATED | ST_RUNNING | ST_IDLE | ST_PROPOSED);
    if live && !force {
        return Err(H5iError::Metadata(format!(
            "{} is still live (status: {}) — abort it first (`h5i env abort {}`) or pass \
             --force to remove it anyway",
            m.id, m.status, m.slug
        )));
    }

    // 1. Reclaim the workspace. Must precede the branch delete: git refuses to
    //    delete a branch still checked out in a registered worktree.
    prune_workspace(repo, h5i_root, m)?;

    // 2. Delete the code branch and 3. the reasoning branch. Tolerate a missing
    //    ref (a pulled or already-half-removed env may lack one locally).
    if let Ok(mut r) = repo.find_reference(&m.branch) {
        r.delete()?;
    }
    let ctx_ref = crate::ctx::branch_ref(&m.context_branch);
    if let Ok(mut r) = repo.find_reference(&ctx_ref) {
        r.delete()?;
    }

    // 4. Record the removal AND strip the manifest/policy from refs/h5i/env
    //    BEFORE erasing the dir, so a failure on step 5 leaves the on-disk
    //    manifest for a retry (and so a re-materialize can't resurrect it).
    append_removed_and_strip(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: "removed".into(),
            detail: Some("workspace + branches + manifest erased locally".into()),
            capture: None,
        },
    )?;

    // 5. Erase the on-disk env dir (manifest, policy, status, leftovers), then
    //    tidy the now-empty agent dir.
    let dir = m.dir(h5i_root);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    }
    let agent_dir = h5i_root.join(ENV_DIR).join(&m.agent);
    if agent_dir.read_dir().map(|mut d| d.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(&agent_dir);
    }
    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_jsonl_replaces_by_id_and_keeps_others_sorted() {
        let existing = "{\"id\":\"b\",\"v\":1}\n{\"id\":\"a\",\"v\":1}\n";
        // Replace b, keep a; output sorted by id.
        let out = upsert_jsonl_by_id(existing, "b", "{\"id\":\"b\",\"v\":2}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"id\":\"a\""));
        assert!(lines[1].contains("\"v\":2"), "b replaced: {out}");
        // Insert a new id.
        let out = upsert_jsonl_by_id(&out, "c", "{\"id\":\"c\",\"v\":9}");
        assert_eq!(out.lines().count(), 3);
        assert!(out.lines().last().unwrap().contains("\"id\":\"c\""));
    }

    #[test]
    fn remove_jsonl_drops_only_the_matching_id() {
        let existing = "{\"id\":\"a\",\"v\":1}\n{\"id\":\"b\",\"v\":1}\n{\"id\":\"c\",\"v\":1}\n";
        let out = remove_jsonl_by_id(existing, "b");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "one line dropped: {out}");
        assert!(lines[0].contains("\"id\":\"a\"") && lines[1].contains("\"id\":\"c\""));
        // Removing an absent id is a no-op; an empty input stays empty.
        assert_eq!(remove_jsonl_by_id(existing, "z"), existing);
        assert_eq!(remove_jsonl_by_id("", "a"), "");
        // Removing the sole line yields the empty blob.
        assert_eq!(remove_jsonl_by_id("{\"id\":\"a\",\"v\":1}\n", "a"), "");
    }

    #[test]
    fn slug_validation() {
        assert!(validate_slug("fix-auth").is_ok());
        assert!(validate_slug("a").is_ok());
        assert!(validate_slug("v2.1_hotfix").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("Fix-Auth").is_err());
        assert!(validate_slug("a/b").is_err());
        assert!(validate_slug("-leading").is_err());
        assert!(validate_slug(".hidden").is_err());
        assert!(validate_slug("x.lock").is_err());
        assert!(validate_slug(&"x".repeat(65)).is_err());
    }

    #[test]
    fn agent_validation_blocks_traversal_and_bad_refs() {
        assert!(validate_agent("claude").is_ok());
        assert!(validate_agent("codex-1").is_ok());
        assert!(validate_agent("a.b_c").is_ok());
        // Path traversal / ref-escape shapes that msg::validate_name admits.
        assert!(validate_agent("").is_err());
        assert!(validate_agent(".").is_err());
        assert!(validate_agent("..").is_err());
        assert!(validate_agent("../x").is_err());
        assert!(validate_agent("a/b").is_err());
        assert!(validate_agent("a\\b").is_err());
        assert!(validate_agent(".hidden").is_err());
        assert!(validate_agent("x.lock").is_err());
        assert!(validate_agent(&"a".repeat(65)).is_err());
    }

    #[test]
    fn event_serde_roundtrip() {
        let e = EnvEvent {
            ts: now_ts(),
            env_id: "env/claude/x".into(),
            agent: "claude".into(),
            event: "exec".into(),
            detail: Some("cmd=`true` exit=0".into()),
            capture: Some("abcd1234".into()),
        };
        let line = serde_json::to_string(&e).unwrap();
        let back: EnvEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.env_id, e.env_id);
        assert_eq!(back.capture, e.capture);
        // Optional fields are omitted when absent (the log stays lean).
        let bare = EnvEvent { detail: None, capture: None, ..e };
        let line = serde_json::to_string(&bare).unwrap();
        assert!(!line.contains("detail"));
        assert!(!line.contains("capture"));
    }

    #[test]
    fn manifest_serde_roundtrip() {
        let m = EnvManifest {
            id: "env/claude/fix".into(),
            agent: "claude".into(),
            slug: "fix".into(),
            base_commit: "c".repeat(40),
            base_tree: "t".repeat(40),
            parent_branch: "main".into(),
            branch: "refs/heads/h5i/env/claude/fix".into(),
            parent_context_branch: "main".into(),
            context_branch: "env/claude/fix".into(),
            profile: "default".into(),
            policy_digest: "d".repeat(64),
            isolation_claim: "workspace".into(),
            backend: "worktree".into(),
            created_at: now_ts(),
            updated_at: now_ts(),
            status: ST_CREATED.into(),
            captures: vec!["cap1".into()],
        };
        let text = serde_json::to_string_pretty(&m).unwrap();
        let back: EnvManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.branch_short(), "h5i/env/claude/fix");
        assert_eq!(back.worktree_name(), "h5i-env-claude-fix");
        assert_eq!(back.captures, m.captures);
    }

    #[test]
    fn staged_path_filter_rejects_the_dangerous_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("src/main.rs"), "fn main() {}").unwrap();
        let canon = work.canonicalize().unwrap();

        // Ordinary file: fine.
        assert!(staged_path_violation(&canon, Path::new("src/main.rs")).is_none());

        // `.git` components: rejected (gitlink/hooks smuggling).
        assert!(staged_path_violation(&canon, Path::new(".git")).is_some());
        assert!(staged_path_violation(&canon, Path::new("vendor/.git/config")).is_some());

        // `..` traversal: rejected.
        assert!(staged_path_violation(&canon, Path::new("../escape.txt")).is_some());

        // Vanished file: rejected (TOCTOU).
        assert!(staged_path_violation(&canon, Path::new("nope.txt")).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn staged_path_filter_handles_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "s3cret").unwrap();
        let canon = work.canonicalize().unwrap();

        // A symlink itself is stored as a link blob, never followed — allowed.
        symlink(outside.join("secret.txt"), work.join("link.txt")).unwrap();
        assert!(staged_path_violation(&canon, Path::new("link.txt")).is_none());

        // A file REACHED THROUGH a symlinked directory escapes $WORK — rejected.
        symlink(&outside, work.join("sneaky")).unwrap();
        let v = staged_path_violation(&canon, Path::new("sneaky/secret.txt"));
        assert!(v.is_some(), "dir-symlink traversal must be rejected");
        assert!(v.unwrap().contains("escapes $WORK"));
    }

    #[test]
    fn find_disambiguates() {
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        for (agent, slug) in [("claude", "fix"), ("codex", "fix"), ("claude", "perf")] {
            let m = EnvManifest {
                id: format!("env/{agent}/{slug}"),
                agent: agent.into(),
                slug: slug.into(),
                base_commit: "c".repeat(40),
                base_tree: "t".repeat(40),
                parent_branch: "main".into(),
                branch: format!("refs/heads/h5i/env/{agent}/{slug}"),
                parent_context_branch: "main".into(),
                context_branch: format!("env/{agent}/{slug}"),
                profile: "default".into(),
                policy_digest: "d".repeat(64),
                isolation_claim: "workspace".into(),
                backend: "worktree".into(),
                created_at: now_ts(),
                updated_at: now_ts(),
                status: ST_CREATED.into(),
                captures: Vec::new(),
            };
            save_manifest(h5i_root, &m).unwrap();
        }
        // Unique slug resolves bare.
        assert_eq!(find(h5i_root, "perf").unwrap().id, "env/claude/perf");
        // Ambiguous slug requires qualification.
        let err = find(h5i_root, "fix").unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "{err}");
        assert_eq!(find(h5i_root, "codex/fix").unwrap().id, "env/codex/fix");
        assert_eq!(find(h5i_root, "env/claude/fix").unwrap().id, "env/claude/fix");
        // Unknown name errors.
        assert!(find(h5i_root, "ghost").is_err());
    }
}
