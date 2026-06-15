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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::H5iError;
use crate::objects;
use crate::sandbox::{self, BoxGitPath, IsolationClaim, ResolvedPolicy};

/// Git ref holding the shareable env state: the append-only event log plus the
/// per-env manifests and resolved policies (so `h5i push`/`pull` carry an
/// environment to another clone for the cross-agent review loop, design §11).
///
/// Everything env-related lives under one `refs/h5i/env/` namespace: this state
/// ref at `…/meta`, the code transport at `refs/h5i/env/code/*`. The state ref
/// is `…/meta` (not the bare leaf `refs/h5i/env`) because git's ref store cannot
/// hold a leaf at `refs/h5i/env` and refs under `refs/h5i/env/` at once.
pub const ENV_REF: &str = "refs/h5i/env/meta";
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

pub const H5I_ENV_ID_VAR: &str = "H5I_ENV_ID";
pub const H5I_ENV_POLICY_DIGEST_VAR: &str = "H5I_ENV_POLICY_DIGEST";
pub const H5I_ENV_CAPTURE_SPOOL_VAR: &str = "H5I_ENV_CAPTURE_SPOOL";
pub const H5I_ENV_AUDIT_CAPTURE_VAR: &str = "H5I_ENV_AUDIT_CAPTURE";
const CONTAINER_CAPTURE_SPOOL: &str = "/.h5i/spool";
#[cfg(unix)] // only the unix-gated RunLock references this
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
        self.branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&self.branch)
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
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string()
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
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
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

/// Validate a manifest imported from the shared ref (`refs/h5i/env`) BEFORE its
/// `agent`/`slug` are used to compute on-disk paths. Pulled manifests are
/// untrusted peer data: the local `create` path runs `validate_agent`/
/// `validate_slug`, but [`materialize_from_ref`] would otherwise feed `agent`/
/// `slug` straight into [`env_dir`] — a crafted `..`/absolute component would
/// write outside `.git/.h5i/env`. The identity fields are deterministic
/// (`create` always derives them from agent+slug), so anything other than the
/// exact canonical shape is rejected fail-closed.
fn validate_imported_manifest(m: &EnvManifest) -> Result<(), H5iError> {
    validate_agent(&m.agent)?;
    validate_slug(&m.slug)?;
    let checks = [
        ("id", &m.id, format!("env/{}/{}", m.agent, m.slug)),
        (
            "branch",
            &m.branch,
            format!("refs/heads/{BRANCH_PREFIX}{}/{}", m.agent, m.slug),
        ),
        (
            "context_branch",
            &m.context_branch,
            format!("env/{}/{}", m.agent, m.slug),
        ),
    ];
    for (field, got, want) in checks {
        if *got != want {
            return Err(H5iError::Metadata(format!(
                "manifest {field} is not the canonical '{want}' (got '{}')",
                crate::msg::sanitize_display(got)
            )));
        }
    }
    Ok(())
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
            let existing = objects::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
                .unwrap_or_default();
            files.push((MANIFESTS_FILE, upsert_jsonl_by_id(&existing, &m.id, line)));
        }
        if let (Some(m), Some(toml)) = (manifest, policy_toml) {
            let existing = objects::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE)
                .unwrap_or_default();
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
            Some(old) => repo
                .reference_matching(ENV_REF, new_oid, true, old, &message)
                .is_ok(),
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
            Some(old) => repo
                .reference_matching(ENV_REF, new_oid, true, old, &message)
                .is_ok(),
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

/// Read every env manifest stored in the state ref.
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

/// Read every resolved policy stored in the state ref as `(env_id, toml)`.
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
        // Untrusted peer data: validate identity/path components before any of
        // them reach the filesystem. Skip (don't abort the whole sync) a bad
        // manifest so one poisoned line can't suppress every legitimate env.
        if let Err(e) = validate_imported_manifest(&m) {
            eprintln!(
                "warning: skipping shared env manifest '{}': {e}",
                crate::msg::sanitize_display(&m.id)
            );
            continue;
        }
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
    let mut manifests: std::collections::HashMap<String, EnvManifest> =
        std::collections::HashMap::new();
    // policies: id → toml (first seen wins; immutable).
    let mut policies: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for oid in [local_oid, incoming_oid] {
        let tree = repo.find_commit(oid)?.tree().ok();
        let raw =
            objects::read_blob_from_tree(repo, tree.as_ref(), EVENTS_FILE).unwrap_or_default();
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
        let mraw =
            objects::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE).unwrap_or_default();
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
        let praw =
            objects::read_blob_from_tree(repo, tree.as_ref(), POLICIES_FILE).unwrap_or_default();
        for line in praw.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let (Some(id), Some(toml)) = (
                    v.get("id").and_then(|i| i.as_str()),
                    v.get("toml").and_then(|t| t.as_str()),
                ) {
                    policies
                        .entry(id.to_string())
                        .or_insert_with(|| toml.to_string());
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
        plog.push_str(&serde_json::to_string(
            &serde_json::json!({"id": id, "toml": toml}),
        )?);
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
    Ok(repo.commit(
        None,
        &sig,
        &sig,
        "h5i pull: union-merge of refs/h5i/env",
        &tree,
        &parents,
    )?)
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
        .filter(|m| m.id == name || m.id == format!("env/{name}") || m.slug == name)
        .collect();
    match matches.len() {
        0 => Err(H5iError::Metadata(format!(
            "no environment named '{name}' (see `h5i env list`)"
        ))),
        1 => Ok(matches[0].clone()),
        _ => Err(H5iError::Metadata(format!(
            "'{name}' is ambiguous — qualify it: {}",
            matches
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
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
    /// Command evidence policy for wrapped in-env commands.
    pub audit_capture: sandbox::AuditCapture,
}

impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            from: None,
            profile: None,
            isolation: None,
            backend: "auto".into(),
            audit_capture: sandbox::AuditCapture::Signal,
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
        return Err(H5iError::Metadata(format!(
            "environment {id} already exists"
        )));
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
        Some(sandbox::IsolationRequest::Auto) => {
            sandbox::effective_auto(workdir, profile_name, true)?
        }
        None => sandbox::effective_auto(workdir, profile_name, false)?,
    };

    // Policy first (fail closed BEFORE any state is created on disk).
    let profile = sandbox::load_profile(workdir, profile_name, Some(claim))?;
    let caps = sandbox::probe_host();
    let mut policy = sandbox::resolve(&profile, &caps)?;
    policy.audit.capture = opts.audit_capture;
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
        let wt = repo
            .worktree(&wt_name, &work_path, Some(&wt_opts))
            .map_err(|e| H5iError::Metadata(format!("worktree creation failed for {id}: {e}")))?;
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
        &format!(
            "h5i environment {id} (profile {}, isolation {})",
            profile.name,
            policy.claim.as_str()
        ),
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
    std::fs::write(&policy_path, &policy_toml).map_err(|e| H5iError::with_path(e, &policy_path))?;
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

// ─── in-box git plumbing grants ──────────────────────────────────────────────

/// The repo-`.git` plumbing surface that makes the env's worktree a
/// *functional* git checkout from inside the box. Consumed per backend by
/// [`grant_box_git`]: Landlock grants at process/supervised, identical-path
/// bind mounts at container.
///
/// `$WORK` alone is not enough: a native worktree's plumbing lives outside it.
/// `$WORK/.git` is a pointer file into `<repo>/.git/worktrees/<wt>` (HEAD,
/// index, `commondir`), which in turn points at the shared `<repo>/.git`
/// (objects, refs, config). With no grant, every `git`/`h5i` invocation inside
/// the box dies on EACCES — which libgit2 renders as a misleading
/// `GIT_ELOCKED` ("failed open - '…/commondir' is locked").
///
/// The grants restore exactly the surface a boxed agent needs, nothing more:
///
/// - **rw** `worktrees/<wt>` — this env's own admin dir (HEAD, index, reflog,
///   the `h5i/HEAD` context pin).
/// - **rw** `objects` — the content-addressed store. It is shared: a hostile
///   box can add garbage or delete loose objects (an *availability* risk,
///   recoverable from any clone), but it cannot move a ref it is not granted,
///   so history integrity is preserved.
/// - **rw** the parent dir of the env's own branch ref, plus its reflog dir —
///   loose-ref updates create `<slug>.lock` siblings, so the grant must be the
///   directory. Scope: the box can move *its own agent's* env branches under
///   `refs/heads/h5i/env/<agent>/` and nothing else in `refs/heads`.
/// - **rw** `refs/h5i/context` — the reasoning store, so in-box
///   `h5i context init/trace/commit` works (`init` records the goal on the
///   `main` context branch). Context is a shared advisory record, already
///   union-merged across clones — not a protected code ref.
/// - **ro** `HEAD`, `config`, `packed-refs`, `refs`, `info` — the minimum
///   reads `git status`/`commit` need. A repo-local `config` carrying
///   credentials in remote URLs becomes readable in-box; it stays strictly
///   read-only (a writable `core.fsmonitor`/`hooksPath` would execute code on
///   the host the next time *anyone* runs git there).
/// - **ro** `~/.gitconfig` + `~/.config/git` — git *dies* (not skips) when an
///   existing global config can't be opened: Landlock lets the `access()`
///   probe pass on DAC bits, then the open fails and git reports "unknown
///   error occurred while reading the configuration files". The agent profile
///   already grants these (commit identity); deny-home profiles get exactly
///   these two paths and nothing else under `$HOME` (`~/.git-credentials`
///   stays out — it is only consulted by credential helpers on network ops).
/// - **ro** the main repo's `Cargo.toml` when it exists. Cargo walks upward
///   from nested env worktrees looking for a workspace root; without read
///   access, ordinary `cargo build`/`cargo test` fails before compiling.
///
/// Deliberately **not** granted: `.git` itself, `hooks`, `refs/h5i/env` (a box
/// that could rewrite manifests/policies could widen its own sandbox on the
/// next run), the env's manifest/policy dir beside `$WORK`, and the on-disk
/// h5i stores (`.h5i/claims`, notes, msg) — captures, claims and messages stay
/// host-mediated evidence channels by design.
///
/// Two invariants:
/// - Paths derive only from the identity-validated manifest and the host repo
///   handle — never from box-writable state (the `$WORK/.git` pointer file is
///   exactly the kind of thing a previous run could have rewritten to point
///   anywhere).
/// - Missing rw dirs are (re)created here. The Landlock builder skips
///   non-existent grant paths — the right fail-closed default for *policy*
///   paths, but for these structural grants a silent skip would brick in-box
///   git again (e.g. after a host-side `git pack-refs` pruned the loose-ref
///   directory).
fn box_git_plumbing(repo: &Repository, m: &EnvManifest) -> Result<Vec<BoxGitPath>, H5iError> {
    let git_dir = repo.commondir().to_path_buf();
    // `refs/heads/h5i/env/<agent>` — `m.branch` is identity-validated against
    // agent+slug, so this parent can never leave the env namespace.
    let branch_parent = Path::new(&m.branch)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            H5iError::Metadata(format!("{}: malformed branch ref '{}'", m.id, m.branch))
        })?
        .to_path_buf();

    // ro before rw: `refs` (ro) is the parent of two rw entries, and the
    // container backend mounts in list order (nested binds need the parent
    // mounted first; the kernel tiers don't care — Landlock rules are a set).
    let mut paths: Vec<BoxGitPath> = ["HEAD", "config", "packed-refs", "refs", "info"]
        .iter()
        .map(|p| BoxGitPath {
            host: git_dir.join(p),
            rw: false,
        })
        .collect();
    let rw: Vec<PathBuf> = vec![
        git_dir.join("worktrees").join(m.worktree_name()),
        git_dir.join("objects"),
        git_dir.join(&branch_parent),
        git_dir.join("logs").join(&branch_parent),
        git_dir.join("refs/h5i/context"),
    ];
    for d in &rw {
        std::fs::create_dir_all(d).map_err(|e| H5iError::with_path(e, d))?;
    }
    paths.extend(rw.into_iter().map(|host| BoxGitPath { host, rw: true }));
    Ok(paths)
}

/// Apply the in-box git plumbing to a loaded policy, per backend:
///
/// - **process/supervised:** appended as Landlock grants (`fs.read`/`fs.write`),
///   plus ro `~/.gitconfig` + `~/.config/git` — git dies (not skips) on an
///   existing-but-unreadable global config under Landlock.
/// - **container:** stashed on `policy.box_git`; the backend bind-mounts each
///   path at its *identical host path* inside the container, so the worktree's
///   gitdir/commondir pointer files resolve. `$WORK` is dual-mounted at its
///   host path too (the admin dir's `gitdir` back-pointer names it — libgit2
///   resolves the workdir through it). No `~/.gitconfig` here: the host HOME
///   is deliberately not mounted, and a *missing* global config is skippable.
/// - **workspace:** unconfined — nothing to do.
fn grant_box_git(
    repo: &Repository,
    m: &EnvManifest,
    work: &Path,
    policy: &mut ResolvedPolicy,
) -> Result<(), H5iError> {
    match policy.claim {
        IsolationClaim::Process | IsolationClaim::Supervised => {
            for p in box_git_plumbing(repo, m)? {
                let path = p.host.display().to_string();
                if p.rw {
                    policy.profile.fs_write.push(path);
                } else {
                    policy.profile.fs_read.push(path);
                }
            }
            // Tilde paths expand inside the sandbox builder; missing are skipped.
            policy
                .profile
                .fs_read
                .extend(["~/.gitconfig".to_string(), "~/.config/git".to_string()]);
            // The env worktree is nested inside the main repo, so agent runtimes
            // (claude/codex) discover the PROJECT config by walking up to the
            // main repo's `.claude`/`.codex`. Grant READ so discovery works —
            // and so the observation hook defined there actually loads — without
            // granting write (the agent still cannot disable a project hook).
            // `commondir().parent()` is the main repo root whether `repo` is the
            // main handle or a worktree handle.
            if let Some(main_root) = repo.commondir().parent() {
                for d in [".claude", ".codex"] {
                    let p = main_root.join(d);
                    if p.is_dir() {
                        policy.profile.fs_read.push(p.display().to_string());
                    }
                }
                let cargo_manifest = main_root.join("Cargo.toml");
                if cargo_manifest.is_file() {
                    policy
                        .profile
                        .fs_read
                        .push(cargo_manifest.display().to_string());
                }
            }
        }
        IsolationClaim::Container => {
            let mut mounts = box_git_plumbing(repo, m)?;
            mounts.push(BoxGitPath {
                host: work.to_path_buf(),
                rw: true,
            });
            if let Some(main_root) = repo.commondir().parent() {
                let cargo_manifest = main_root.join("Cargo.toml");
                if cargo_manifest.is_file() {
                    mounts.push(BoxGitPath {
                        host: cargo_manifest,
                        rw: false,
                    });
                }
            }
            // Podman errors on a missing bind source (unlike Landlock, which
            // skips) — keep only what exists on the host.
            mounts.retain(|b| b.host.exists());
            policy.box_git = mounts;
        }
        _ => {}
    }
    Ok(())
}

fn prepare_cargo_env(work: &Path, policy: &ResolvedPolicy) -> Result<Vec<(String, String)>, H5iError> {
    if policy.claim < IsolationClaim::Process {
        return Ok(Vec::new());
    }
    let h5i_dir = work.join(".h5i");
    let target_dir = h5i_dir.join("cargo-target");
    std::fs::create_dir_all(&target_dir).map_err(|e| H5iError::with_path(e, &target_dir))?;
    Ok(vec![(
        "CARGO_TARGET_DIR".to_string(),
        target_dir.display().to_string(),
    )])
}

fn prepare_env_capture_spool(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
) -> Result<Vec<(String, String)>, H5iError> {
    if policy.claim < IsolationClaim::Process {
        return Ok(Vec::new());
    }
    let spool = m.dir(h5i_root).join("spool");
    std::fs::create_dir_all(&spool).map_err(|e| H5iError::with_path(e, &spool))?;
    let spool_inside = match policy.claim {
        IsolationClaim::Container => {
            policy.env_capture_spool = Some(spool);
            CONTAINER_CAPTURE_SPOOL.to_string()
        }
        IsolationClaim::Process | IsolationClaim::Supervised => {
            policy.profile.fs_write.push(spool.display().to_string());
            spool.display().to_string()
        }
        _ => return Ok(Vec::new()),
    };
    Ok(vec![
        (H5I_ENV_ID_VAR.to_string(), m.id.clone()),
        (
            H5I_ENV_POLICY_DIGEST_VAR.to_string(),
            m.policy_digest.clone(),
        ),
        (H5I_ENV_CAPTURE_SPOOL_VAR.to_string(), spool_inside),
        (
            H5I_ENV_AUDIT_CAPTURE_VAR.to_string(),
            policy.audit.capture.as_str().to_string(),
        ),
    ])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxCaptureMeta {
    pub cmd: String,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub cmd_argv: Vec<String>,
}

pub fn write_inbox_capture_spool(
    spool: &Path,
    meta: &InboxCaptureMeta,
    raw: &[u8],
) -> Result<String, H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("cap-{}-{nanos}", std::process::id());
    let raw_path = spool.join(format!("{base}.raw"));
    let meta_path = spool.join(format!("{base}.json"));
    std::fs::write(&raw_path, raw).map_err(|e| H5iError::with_path(e, &raw_path))?;
    let meta_json = serde_json::to_vec(meta)?;
    std::fs::write(&meta_path, meta_json).map_err(|e| H5iError::with_path(e, &meta_path))?;
    Ok(base)
}

fn merged_env(a: &[(String, String)], b: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = a.to_vec();
    out.extend_from_slice(b);
    out
}

/// Stage an in-box `h5i commit` note for host ingest. The notes ref
/// (`refs/h5i/notes`) is sealed in the box, so the commit lands on the env
/// branch but its `H5iCommitRecord` JSON is written here; the host applies it
/// (scoped to the env branch) on the next [`ingest_shell_spool`]. The filename
/// carries the commit oid so the ingest can dedup/validate it.
pub fn write_note_spool(spool: &Path, oid: &str, record_json: &str) -> Result<(), H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    // `oid` is a git hex id; constrain the filename to that charset defensively.
    let safe: String = oid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(64)
        .collect();
    if safe.is_empty() {
        return Err(H5iError::Metadata("empty commit oid for note spool".into()));
    }
    let path = spool.join(format!("note-{safe}.json"));
    std::fs::write(&path, record_json).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

const PROTECTED_HOOK_CONFIGS: &[&str] = &[".claude/settings.json", ".codex/config.toml"];

enum ProtectedHookScope {
    Worktree,
    Home,
}

struct ProtectedHookConfig {
    label: String,
    path: PathBuf,
    original: Option<Vec<u8>>,
    sentinel_created: bool,
    parent_created: bool,
}

struct ProtectedHookConfigGuard {
    files: Vec<ProtectedHookConfig>,
}

impl ProtectedHookConfigGuard {
    fn prepare(work: &Path, claim: IsolationClaim) -> Result<Self, H5iError> {
        if claim < IsolationClaim::Process {
            return Ok(Self { files: Vec::new() });
        }
        let mut files = Vec::new();
        for rel in PROTECTED_HOOK_CONFIGS {
            let path = work.join(rel);
            push_protected_hook_config(
                &mut files,
                rel.to_string(),
                path,
                claim,
                ProtectedHookScope::Worktree,
            )?;
        }
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for rel in PROTECTED_HOOK_CONFIGS {
                push_protected_hook_config(
                    &mut files,
                    format!("~/{rel}"),
                    home.join(rel),
                    claim,
                    ProtectedHookScope::Home,
                )?;
            }
        }
        Ok(Self { files })
    }

    fn finish(self) -> Result<(), H5iError> {
        let mut touched = Vec::new();
        for f in self.files {
            match &f.original {
                Some(original) => {
                    let current = std::fs::read(&f.path).ok();
                    if current.as_deref() != Some(original.as_slice()) {
                        if let Some(parent) = f.path.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| H5iError::with_path(e, parent))?;
                        }
                        std::fs::write(&f.path, original)
                            .map_err(|e| H5iError::with_path(e, &f.path))?;
                        touched.push(f.label);
                    }
                }
                None => {
                    let exists = f.path.exists();
                    let unchanged_sentinel =
                        f.sentinel_created && std::fs::read(&f.path).ok().as_deref() == Some(b"");
                    if exists {
                        remove_path_any(&f.path)?;
                        if !unchanged_sentinel {
                            touched.push(f.label);
                        }
                    }
                    if f.parent_created {
                        if let Some(parent) = f.path.parent() {
                            let _ = std::fs::remove_dir(parent);
                        }
                    }
                }
            }
        }
        if touched.is_empty() {
            Ok(())
        } else {
            Err(H5iError::Metadata(format!(
                "sandbox refused protected hook config modification: {}",
                touched.join(", ")
            )))
        }
    }
}

fn push_protected_hook_config(
    files: &mut Vec<ProtectedHookConfig>,
    label: String,
    path: PathBuf,
    claim: IsolationClaim,
    scope: ProtectedHookScope,
) -> Result<(), H5iError> {
    let original = std::fs::read(&path).ok();
    let mut sentinel_created = false;
    let mut parent_created = false;
    if claim == IsolationClaim::Container
        && matches!(scope, ProtectedHookScope::Worktree)
        && original.is_none()
    {
        if let Some(parent) = path.parent() {
            parent_created = !parent.exists();
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        std::fs::write(&path, b"").map_err(|e| H5iError::with_path(e, &path))?;
        sentinel_created = true;
    }
    files.push(ProtectedHookConfig {
        label,
        path,
        original,
        sentinel_created,
        parent_created,
    });
    Ok(())
}

fn remove_path_any(path: &Path) -> Result<(), H5iError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(H5iError::with_path(e, path)),
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| H5iError::with_path(e, path))
    } else {
        std::fs::remove_file(path).map_err(|e| H5iError::with_path(e, path))
    }
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
    let mut policy = load_policy(h5i_root, m)?;
    // Structural grants (like the implicit `$WORK` rw): the worktree must be a
    // functional git checkout inside the box.
    grant_box_git(repo, m, &work, &mut policy)?;
    let env_capture_env = prepare_env_capture_spool(h5i_root, m, &mut policy)?;
    let cargo_env = prepare_cargo_env(&work, &policy)?;

    // Broker any declared secrets BEFORE marking the env running, so a
    // fail-closed grant (missing source, unsupported inject) aborts cleanly
    // without leaving the env in 'running'. `brokered` lives for the whole run;
    // its Drop guard unlinks any file-injected secrets on every exit path.
    let secret_dir = m.dir(h5i_root).join("secrets");
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered =
        crate::secrets_broker::broker(&policy.profile.secret_grants, &secret_dir, is_workspace)?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    let injected_env = merged_env(&merged_env(&brokered.env, &env_capture_env), &cargo_env);

    set_status(
        repo,
        h5i_root,
        m,
        ST_RUNNING,
        "status",
        Some("running".into()),
        None,
    )?;
    let result = sandbox::run_with_env(&policy, &work, argv, &injected_env);
    // Whatever happened, leave the running state before propagating errors.
    let outcome = match result {
        Ok(o) => o,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            set_status(
                repo,
                h5i_root,
                m,
                ST_IDLE,
                "status",
                Some("idle (run failed to start)".into()),
                None,
            )?;
            return Err(e);
        }
    };
    if let Err(e) = protected_hook_configs.finish() {
        set_status(
            repo,
            h5i_root,
            m,
            ST_IDLE,
            "violation",
            Some(e.to_string()),
            None,
        )?;
        return Err(e);
    }

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
        evidence_source: Some("host-env-run".into()),
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
    let observed = match ingest_shell_spool(repo, h5i_root, m) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("warning: env observation ingest failed: {e}");
            0
        }
    };
    // The event log (refs/h5i/env) travels via `h5i push`, so the command —
    // which can carry a credential passed as an argument — is scrubbed before
    // it lands in the detail, exactly like the capture's cmd field.
    let safe_cmd = crate::secrets::redact_text(&argv.join(" "));
    let rss = outcome
        .max_rss_kb
        .map(|kb| format!(" rss={}MiB", kb / 1024))
        .unwrap_or_default();
    let observed_note = if observed > 0 {
        format!(" observed={observed}")
    } else {
        String::new()
    };
    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "exec",
        Some(format!(
            "cmd=`{}` exit={} wall={}ms cpu={}ms{}{}{}",
            safe_cmd,
            outcome
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            outcome.wall_ms,
            outcome.cpu_ms,
            rss,
            if outcome.timed_out { " timed-out" } else { "" },
            observed_note
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

    let mut policy = load_policy(h5i_root, m)?;
    // Same structural grants as `run`: an interactive boxed agent lives in
    // this worktree and must be able to use git / h5i context inside it.
    grant_box_git(repo, m, &work, &mut policy)?;
    let env_capture_env = prepare_env_capture_spool(h5i_root, m, &mut policy)?;
    let cargo_env = prepare_cargo_env(&work, &policy)?;
    let secret_dir = m.dir(h5i_root).join("secrets");
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered =
        crate::secrets_broker::broker(&policy.profile.secret_grants, &secret_dir, is_workspace)?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    let injected_env = merged_env(&merged_env(&brokered.env, &env_capture_env), &cargo_env);

    set_status(
        repo,
        h5i_root,
        m,
        ST_RUNNING,
        "status",
        Some("running (shell)".into()),
        None,
    )?;
    let exit_code = match sandbox::run_interactive(&policy, &work, argv, &injected_env) {
        Ok(code) => code,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            set_status(
                repo,
                h5i_root,
                m,
                ST_IDLE,
                "status",
                Some("idle (shell failed to start)".into()),
                None,
            )?;
            return Err(e);
        }
    };
    if let Err(e) = protected_hook_configs.finish() {
        set_status(
            repo,
            h5i_root,
            m,
            ST_IDLE,
            "violation",
            Some(e.to_string()),
            None,
        )?;
        return Err(e);
    }

    // Ingest the session's observation spool (supervised exec log / container
    // tee-shim records) into tagged captures BEFORE the final status event, so
    // the manifest it persists already lists them. Best-effort: a failed
    // ingest warns and never breaks the session.
    let observed = match ingest_shell_spool(repo, h5i_root, m) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("warning: shell observation ingest failed: {e}");
            0
        }
    };

    let safe_cmd = crate::secrets::redact_text(&argv.join(" "));
    let observed_note = if observed > 0 {
        format!(" observed={observed}")
    } else {
        String::new()
    };
    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "shell",
        Some(format!(
            "interactive cmd=`{safe_cmd}` exit={exit_code}{observed_note}"
        )),
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

// ─── shell-spool ingest (in-box observation evidence) ────────────────────────

/// Ingest caps. Container-tier spool contents are written by the **box** (the
/// tee shim) and are untrusted: bound entry count and sizes, accept regular
/// files only, never follow a symlink, and redact before anything is stored or
/// displayed. The supervised tier's `exec.jsonl` is supervisor-written (the box
/// can't reach it) but shares the same caps for uniformity.
const SPOOL_MAX_ENTRIES: usize = 200;
const SPOOL_MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const SPOOL_MAX_CMD_BYTES: u64 = 64 * 1024;

/// Read one spool file defensively: regular file only (symlinks rejected),
/// capped at `cap` bytes with an explicit truncation marker.
fn read_spool_capped(p: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(p).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let f = std::fs::File::open(p).ok()?;
    let mut buf = Vec::new();
    f.take(cap).read_to_end(&mut buf).ok()?;
    if meta.len() > cap {
        buf.extend_from_slice(b"\n----- h5i: spool entry truncated -----\n");
    }
    Some(buf)
}

/// Ingest the env's observation spool (`<env>/spool/`) into tagged captures —
/// the evidence an interactive **container** session leaves behind:
/// `cmd-<pid>-<n>.{cmd,out,err,exit}`, the container tee-shim's records (one per
/// top-level `sh -c`/`bash -c` the in-box agent ran).
///
/// Each becomes a secret-redacted `objects` capture tagged with the env id +
/// policy digest (same provenance stream as `env run` execs) plus an `exec`
/// event, and the spool files are removed. Returns how many captures landed.
fn ingest_shell_spool(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
) -> Result<usize, H5iError> {
    let spool = m.dir(h5i_root).join("spool");
    if !spool.is_dir() {
        return Ok(0);
    }
    let work = m.work_dir(h5i_root);
    let wt_repo = Repository::open(&work)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let mut count = 0usize;

    // The container tee-shim records. Filenames are box-controlled: accept
    // only the shim's `cmd-…` shape with a conservative charset.
    let mut bases: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&spool) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(base) = name.strip_suffix(".cmd") {
                let ok = base.starts_with("cmd-")
                    && base.len() <= 64
                    && base.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
                if ok {
                    bases.push(base.to_string());
                }
            }
        }
    }
    bases.sort();
    let dropped = bases.len().saturating_sub(SPOOL_MAX_ENTRIES);
    for base in bases.iter().take(SPOOL_MAX_ENTRIES) {
        let path_of = |ext: &str| spool.join(format!("{base}.{ext}"));
        let cmd_text = read_spool_capped(&path_of("cmd"), SPOOL_MAX_CMD_BYTES)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let stdout = read_spool_capped(&path_of("out"), SPOOL_MAX_OUTPUT_BYTES).unwrap_or_default();
        let stderr = read_spool_capped(&path_of("err"), SPOOL_MAX_OUTPUT_BYTES).unwrap_or_default();
        let exit_code = read_spool_capped(&path_of("exit"), 16)
            .and_then(|b| String::from_utf8_lossy(&b).trim().parse::<i32>().ok());

        // Compose the raw payload exactly like `env run` (stdout + labeled
        // stderr block) so summaries and `recall` views look identical.
        let mut raw = stdout;
        if !stderr.is_empty() {
            if !raw.is_empty() && !raw.ends_with(b"\n") {
                raw.push(b'\n');
            }
            raw.extend_from_slice(b"\n----- stderr -----\n");
            raw.extend_from_slice(&stderr);
        }

        // The command string is box-controlled: redact secrets, flatten to one
        // line, and cap it before it lands in a manifest or event detail.
        let safe_cmd: String = crate::secrets::redact_text(&cmd_text)
            .replace(['\n', '\r'], " ")
            .chars()
            .take(300)
            .collect();
        // A whitespace split of the observed command is only a *hint* for the
        // structured-parser pick (pytest/cargo adapters) — never executed.
        let argv_hint: Vec<String> = cmd_text
            .split_whitespace()
            .take(8)
            .map(str::to_string)
            .collect();
        let opts = objects::CaptureOptions {
            kind: crate::token_filter::OutputKind::Auto,
            cmd: Some(safe_cmd.clone()),
            cwd: Some(work.display().to_string()),
            exit_code,
            git_tree: head_tree.clone(),
            files: Vec::new(),
            cmd_argv: argv_hint.clone(),
            filter: crate::token_filter::FilterConfig {
                cmd: Some(argv_hint),
                ..Default::default()
            },
            env_id: Some(m.id.clone()),
            policy_digest: Some(m.policy_digest.clone()),
            evidence_source: Some("tee-shim".into()),
            egress: None,
            redact: true,
        };
        let captured = objects::capture(&wt_repo, h5i_root, &raw, opts)?;
        m.captures.push(captured.manifest.id.clone());
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "exec".into(),
                detail: Some(format!(
                    "observed in shell: cmd=`{safe_cmd}` exit={}",
                    exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into())
                )),
                capture: Some(captured.manifest.id.clone()),
            },
        )?;
        for ext in ["cmd", "out", "err", "exit"] {
            let _ = std::fs::remove_file(path_of(ext));
        }
        count += 1;
    }

    // In-box `h5i capture run` records. These are written by the boxed process
    // into the same quarantined spool and materialized by the host here.
    let mut cap_bases: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&spool) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(base) = name.strip_suffix(".json") {
                let ok = base.starts_with("cap-")
                    && base.len() <= 96
                    && base.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
                if ok {
                    cap_bases.push(base.to_string());
                }
            }
        }
    }
    cap_bases.sort();
    let cap_dropped = cap_bases.len().saturating_sub(SPOOL_MAX_ENTRIES);
    for base in cap_bases.iter().take(SPOOL_MAX_ENTRIES) {
        let path_of = |ext: &str| spool.join(format!("{base}.{ext}"));
        let meta_bytes = match read_spool_capped(&path_of("json"), SPOOL_MAX_CMD_BYTES) {
            Some(b) => b,
            None => continue,
        };
        let meta: InboxCaptureMeta = match serde_json::from_slice(&meta_bytes) {
            Ok(m) => m,
            Err(_) => {
                let _ = std::fs::remove_file(path_of("json"));
                let _ = std::fs::remove_file(path_of("raw"));
                continue;
            }
        };
        let raw = read_spool_capped(&path_of("raw"), SPOOL_MAX_OUTPUT_BYTES).unwrap_or_default();
        let safe_cmd: String = crate::secrets::redact_text(&meta.cmd)
            .replace(['\n', '\r'], " ")
            .chars()
            .take(300)
            .collect();
        let argv_hint: Vec<String> = meta.cmd_argv.into_iter().take(16).collect();
        let files: Vec<String> = meta.files.into_iter().take(64).collect();
        let opts = objects::CaptureOptions {
            kind: crate::token_filter::OutputKind::Auto,
            cmd: Some(safe_cmd.clone()),
            cwd: meta.cwd,
            exit_code: meta.exit_code,
            git_tree: head_tree.clone(),
            files,
            cmd_argv: argv_hint.clone(),
            filter: crate::token_filter::FilterConfig {
                cmd: Some(argv_hint),
                ..Default::default()
            },
            env_id: Some(m.id.clone()),
            policy_digest: Some(m.policy_digest.clone()),
            evidence_source: Some("inbox-capture".into()),
            egress: None,
            redact: true,
        };
        let captured = objects::capture(&wt_repo, h5i_root, &raw, opts)?;
        m.captures.push(captured.manifest.id.clone());
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "exec".into(),
                detail: Some(format!(
                    "inbox capture: cmd=`{safe_cmd}` exit={} source=inbox-capture",
                    meta.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into())
                )),
                capture: Some(captured.manifest.id.clone()),
            },
        )?;
        let _ = std::fs::remove_file(path_of("json"));
        let _ = std::fs::remove_file(path_of("raw"));
        count += 1;
    }
    if dropped > 0 {
        // No silent caps: the event log must say coverage was bounded.
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "exec-log".into(),
                detail: Some(format!(
                    "spool ingest capped at {SPOOL_MAX_ENTRIES}: {dropped} record(s) dropped"
                )),
                capture: None,
            },
        )?;
    }
    if cap_dropped > 0 {
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "exec-log".into(),
                detail: Some(format!(
                    "inbox capture spool capped at {SPOOL_MAX_ENTRIES}: {cap_dropped} record(s) dropped"
                )),
                capture: None,
            },
        )?;
    }

    // In-box `h5i commit` notes. The box can land a commit on its own env
    // branch but can't write `refs/h5i/notes`; the note is staged here and
    // applied host-side, **scoped to commits reachable from the env branch** so
    // a box can't attach provenance to arbitrary commits (e.g. `main`). The
    // note's fields are agent-claimed, exactly like a normal `h5i commit`.
    let env_tip = repo
        .find_reference(&m.branch)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .map(|c| c.id());
    let base_oid = git2::Oid::from_str(&m.base_commit).ok();
    // A commit is the env's OWN iff it's reachable from the env tip but NOT from
    // the pinned base — i.e. in the range `base..env_tip`. This excludes the
    // inherited history (base, `main`, ancestors) so a box can only stamp
    // commits it actually created, never arbitrary historical ones.
    let in_env_range = |oid: git2::Oid| -> bool {
        let Some(tip) = env_tip else { return false };
        let reachable = tip == oid || repo.graph_descendant_of(tip, oid).unwrap_or(false);
        let inherited = base_oid
            .map(|b| b == oid || repo.graph_descendant_of(b, oid).unwrap_or(false))
            .unwrap_or(false);
        reachable && !inherited
    };
    let mut note_bases: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&spool) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(base) = name.strip_suffix(".json") {
                let ok = base.starts_with("note-")
                    && base.len() <= 96
                    && base.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
                if ok {
                    note_bases.push(base.to_string());
                }
            }
        }
    }
    note_bases.sort();
    let note_dropped = note_bases.len().saturating_sub(SPOOL_MAX_ENTRIES);
    for base in note_bases.iter().take(SPOOL_MAX_ENTRIES) {
        let path = spool.join(format!("{base}.json"));
        let bytes = match read_spool_capped(&path, SPOOL_MAX_CMD_BYTES) {
            Some(b) => b,
            None => continue,
        };
        let record: crate::metadata::H5iCommitRecord = match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        let oid = match git2::Oid::from_str(&record.git_oid) {
            Ok(o) => o,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        // Scope guard: only the env's OWN commits (base..env_tip) may be stamped.
        if !in_env_range(oid) {
            append_event(
                repo,
                &EnvEvent {
                    ts: now_ts(),
                    env_id: m.id.clone(),
                    agent: m.agent.clone(),
                    event: "exec-log".into(),
                    detail: Some(format!(
                        "rejected in-box commit note for {} — not an env-owned commit",
                        &record.git_oid[..12.min(record.git_oid.len())]
                    )),
                    capture: None,
                },
            )?;
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let sig = objects::signature(repo)?;
        let json = String::from_utf8_lossy(&bytes);
        match repo.note(
            &sig,
            &sig,
            Some(crate::repository::H5I_NOTES_REF),
            oid,
            &json,
            true,
        ) {
            Ok(_) => {
                append_event(
                    repo,
                    &EnvEvent {
                        ts: now_ts(),
                        env_id: m.id.clone(),
                        agent: m.agent.clone(),
                        event: "note".into(),
                        detail: Some(format!(
                            "in-box commit note applied to {}",
                            &record.git_oid[..12.min(record.git_oid.len())]
                        )),
                        capture: None,
                    },
                )?;
                count += 1;
            }
            Err(e) => eprintln!("warning: applying in-box commit note failed: {e}"),
        }
        let _ = std::fs::remove_file(&path);
    }
    if note_dropped > 0 {
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "exec-log".into(),
                detail: Some(format!(
                    "in-box commit note spool capped at {SPOOL_MAX_ENTRIES}: {note_dropped} dropped"
                )),
                capture: None,
            },
        )?;
    }
    Ok(count)
}

// ─── diff ───────────────────────────────────────────────────────────────────

/// Unified diff of the env's changes against its pinned base tree.
///
/// When the worktree is present (the originating clone) this is the live
/// working-tree diff (committed + uncommitted, including untracked files).
/// When it is absent (a pulled "remote" env, or after gc) it falls back to the
/// **committed** state on the env's code branch — i.e. what `propose`
/// snapshotted — so a reviewer on another clone sees exactly the proposed diff.
pub fn diff(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    stat_only: bool,
) -> Result<String, H5iError> {
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
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = wt_repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
        render(diff)
    } else {
        // Remote/no-worktree: diff base_tree → env branch tip (the committed,
        // proposed state) using the shared object store.
        let base_tree = repo.find_tree(git2::Oid::from_str(&m.base_tree)?)?;
        let tip_tree = repo
            .find_reference(&m.branch)
            .map_err(|_| {
                H5iError::Metadata(format!(
                    "{}: env code branch '{}' is not present locally — `h5i pull` it first",
                    m.id, m.branch
                ))
            })?
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
        return Drift::Diverged {
            tip: tip.to_string(),
        };
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
        Drift::ParentAhead {
            tip: tip.to_string(),
            commits,
        }
    } else {
        Drift::Diverged {
            tip: tip.to_string(),
        }
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
            p.mem_bytes
                .map(|b| format!("{}MiB", b / 1024 / 1024))
                .unwrap_or_else(|| "∞".into()),
            p.max_procs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "∞".into()),
            p.wall_secs,
            p.fsize_bytes
                .map(|b| format!(" fsize={}MiB", b / 1024 / 1024))
                .unwrap_or_default(),
            p.cpu_secs.map(|s| format!(" cpu={s}s")).unwrap_or_default(),
        ));
        if !p.tools.is_empty() {
            out.push_str(&format!("  tools    : {}\n", p.tools.join(", ")));
        }
    }
    let evidence_detail = if m.captures.is_empty() {
        String::new()
    } else {
        let sources = evidence_sources_by_lane(repo, m)
            .into_iter()
            .map(|(source, n)| format!("{source}={n}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(": {} [{}]", m.captures.join(", "), sources)
    };
    out.push_str(&format!(
        "  evidence : {} capture(s){}\n",
        m.captures.len(),
        evidence_detail
    ));
    // Staged-but-not-yet-ingested spool evidence (visible mid-session, before
    // the host materializes it at run/shell end).
    let pending = scan_spool_pending(h5i_root, m);
    if pending.total() > 0 {
        out.push_str(&format!(
            "  pending  : {} staged in spool ({}) — host-ingested on run/shell end\n",
            pending.total(),
            pending.breakdown(),
        ));
        for cmd in pending.captures.iter().take(5) {
            out.push_str(&format!("             ↳ capture `{cmd}`\n"));
        }
        for oid in pending.notes.iter().take(5) {
            out.push_str(&format!(
                "             ↳ note for {}\n",
                &oid[..12.min(oid.len())]
            ));
        }
    }
    let d = drift(repo, m);
    let marker = if d.is_current() { "✓" } else { "⚠" };
    out.push_str(&format!("  drift    : {marker} {}\n", d.summary()));
    out
}

/// Evidence staged in the env's spool but not yet materialized into the object
/// store / notes ref (an in-box `h5i capture run`/`commit` or a tee-shim record
/// the host ingests at the next `run`/`shell` end). Surfaced by `status` so
/// in-flight evidence during a long interactive session is visible, not opaque.
#[derive(Default)]
struct SpoolPending {
    /// Redacted commands of staged in-box captures (`cap-*.json`).
    captures: Vec<String>,
    /// Commit oids of staged in-box notes (`note-*.json`).
    notes: Vec<String>,
    /// Count of tee-shim observation records (`cmd-*.cmd`).
    shim: usize,
}

impl SpoolPending {
    fn total(&self) -> usize {
        self.captures.len() + self.notes.len() + self.shim
    }
    /// "2 capture, 1 note, 3 shim" — omitting zero lanes.
    fn breakdown(&self) -> String {
        let mut parts = Vec::new();
        if !self.captures.is_empty() {
            parts.push(format!("{} capture", self.captures.len()));
        }
        if !self.notes.is_empty() {
            parts.push(format!("{} note", self.notes.len()));
        }
        if self.shim > 0 {
            parts.push(format!("{} shim", self.shim));
        }
        parts.join(", ")
    }
}

/// Scan the env's spool for staged-but-not-ingested records. Best-effort and
/// concurrency-tolerant: a missing spool, an unreadable or half-written record
/// (the box may be writing it now) is simply skipped, never an error.
fn scan_spool_pending(h5i_root: &Path, m: &EnvManifest) -> SpoolPending {
    let mut p = SpoolPending::default();
    let spool = m.dir(h5i_root).join("spool");
    let Ok(rd) = std::fs::read_dir(&spool) else {
        return p;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(base) = name.strip_suffix(".json") {
            if base.starts_with("cap-") {
                let cmd = std::fs::read(e.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<InboxCaptureMeta>(&b).ok())
                    .map(|meta| meta.cmd)
                    .unwrap_or_default();
                let safe: String = crate::secrets::redact_text(&cmd)
                    .replace(['\n', '\r'], " ")
                    .chars()
                    .take(120)
                    .collect();
                p.captures.push(safe);
            } else if base.starts_with("note-") {
                let oid = std::fs::read(e.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<crate::metadata::H5iCommitRecord>(&b).ok())
                    .map(|r| r.git_oid)
                    .unwrap_or_default();
                p.notes.push(oid);
            }
        } else if name.starts_with("cmd-") && name.ends_with(".cmd") {
            p.shim += 1;
        }
    }
    p
}

/// Count the env's captures by trust lane (`host-env-run`, `inbox-capture`,
/// `tee-shim`, `unknown`). Shared by `status` and the apply provenance note so
/// they always agree. An unresolvable capture id counts as `unknown` rather
/// than being dropped.
fn evidence_sources_by_lane(
    repo: &Repository,
    m: &EnvManifest,
) -> std::collections::BTreeMap<String, usize> {
    let mut by_source = std::collections::BTreeMap::<String, usize>::new();
    for id in &m.captures {
        let source = objects::resolve_manifest(repo, id)
            .ok()
            .and_then(|manifest| manifest.evidence_source)
            .unwrap_or_else(|| "unknown".into());
        *by_source.entry(source).or_default() += 1;
    }
    by_source
}

/// Max capture ids inlined into an apply provenance note (the full count is
/// always recorded; `recall objects --env` has the complete list).
const APPLY_PROVENANCE_CAP: usize = 64;

/// Build the provenance stamped onto a commit produced by `h5i env apply`.
/// Derived **only** from the identity-validated env manifest — never from
/// box-writable state — and preserves the per-lane evidence breakdown so
/// host-verified and box-claimed evidence stay distinguishable on the parent.
fn build_env_provenance(repo: &Repository, m: &EnvManifest) -> crate::metadata::EnvProvenance {
    crate::metadata::EnvProvenance {
        env_id: m.id.clone(),
        agent: m.agent.clone(),
        isolation_claim: m.isolation_claim.clone(),
        policy_digest: m.policy_digest.clone(),
        base_commit: m.base_commit.clone(),
        captures: m
            .captures
            .iter()
            .take(APPLY_PROVENANCE_CAP)
            .cloned()
            .collect(),
        captures_total: m.captures.len(),
        evidence_sources: evidence_sources_by_lane(repo, m),
    }
}

/// Stamp the commit `apply` produced on the parent branch with an h5i note that
/// links it to the env and summarizes the (labeled) evidence carried forward —
/// so the parent-branch commit is self-describing. Best-effort: a note failure
/// must not undo an already-applied merge, so it returns a human note rather
/// than erroring. Idempotent by construction (apply runs once per env — the
/// `ST_PROPOSED` guard — and the note is written with `force`).
fn stamp_apply_provenance(repo: &Repository, m: &EnvManifest, applied: git2::Oid) -> String {
    let prov = build_env_provenance(repo, m);
    let parent_oid = repo
        .find_commit(applied)
        .ok()
        .filter(|c| c.parent_count() > 0)
        .and_then(|c| c.parent_id(0).ok())
        .map(|o| o.to_string());
    let record = crate::metadata::H5iCommitRecord {
        git_oid: applied.to_string(),
        parent_oid,
        ai_metadata: None,
        test_metrics: None,
        ast_hashes: None,
        timestamp: chrono::Utc::now(),
        caused_by: Vec::new(),
        decisions: Vec::new(),
        env_provenance: Some(prov.clone()),
    };
    let sig = match objects::signature(repo) {
        Ok(s) => s,
        Err(e) => return format!("WARNING: apply note skipped (no signature: {e})"),
    };
    let json = match serde_json::to_string(&record) {
        Ok(j) => j,
        Err(e) => return format!("WARNING: apply note skipped (serialize: {e})"),
    };
    match repo.note(&sig, &sig, Some(crate::repository::H5I_NOTES_REF), applied, &json, true) {
        Ok(_) => {
            let lanes = prov
                .evidence_sources
                .iter()
                .map(|(s, n)| format!("{s}={n}"))
                .collect::<Vec<_>>()
                .join(", ");
            let lanes = if lanes.is_empty() { "none".into() } else { lanes };
            format!("provenance note on {}: {} capture(s) [{}]", &applied.to_string()[..12], prov.captures_total, lanes)
        }
        Err(e) => format!("WARNING: apply provenance note failed ({e})"),
    }
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
        manifest
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into())
    ));
    if let Some(d) = &manifest.policy_digest {
        out.push_str(&format!("  policy   : {}\n", &d[..12.min(d.len())]));
    }
    if let Some(source) = &manifest.evidence_source {
        out.push_str(&format!("  source   : {source}\n"));
    }
    if !manifest.redactions.is_empty() {
        out.push_str(&format!(
            "  redacted : {}\n",
            manifest.redactions.join(", ")
        ));
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
        let (files_changed, insertions, deletions) =
            diffstat_numbers(repo, h5i_root, &m).unwrap_or((0, 0, 0));
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
fn diffstat_numbers(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
) -> Option<(usize, usize, usize)> {
    let triple = |diff: &git2::Diff| {
        diff.stats()
            .ok()
            .map(|s| (s.files_changed(), s.insertions(), s.deletions()))
    };
    let work = m.work_dir(h5i_root);
    if work.is_dir() {
        let wt_repo = Repository::open(&work).ok()?;
        let base_tree = wt_repo
            .find_tree(git2::Oid::from_str(&m.base_tree).ok()?)
            .ok()?;
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = wt_repo
            .diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))
            .ok()?;
        triple(&diff)
    } else {
        let base_tree = repo
            .find_tree(git2::Oid::from_str(&m.base_tree).ok()?)
            .ok()?;
        let tip_tree = repo.find_reference(&m.branch).ok()?.peel_to_tree().ok()?;
        let diff = repo
            .diff_tree_to_tree(Some(&base_tree), Some(&tip_tree), None)
            .ok()?;
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
                    .filter(|(k, _)| {
                        matches!(k.as_str(), "passed" | "failed" | "errors" | "warnings")
                    })
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                format!(
                    "{tool} {} (exit {}){}",
                    result.clone().unwrap_or_default(),
                    exit.map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    if counts.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", counts.join(" "))
                    }
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
    let canon_work = work
        .canonicalize()
        .map_err(|e| H5iError::with_path(e, &work))?;

    // The env branch tip is the host-controlled base for this mediated commit.
    // Any gitlink it already carries is an upstream submodule the env inherited
    // at create time — not something the agent introduced (the agent never
    // drives `git` at process+ tiers; every commit on this branch came through
    // *this* function, which only ever lets through gitlinks already in HEAD).
    // We let those round-trip unchanged while still refusing any gitlink the
    // agent *added* or *re-pointed*.
    let head = wt_repo.head()?.peel_to_commit()?;
    let base_gitlinks = base_gitlinks(&head.tree()?);

    // Pre-walk for nested git repositories. libgit2 either errors opaquely or
    // records a submodule gitlink when `add_all` meets a directory containing
    // `.git` — both are wrong here. Detect them OURSELVES, first, and refuse
    // with a precise diagnostic (fail closed). Registered submodules from the
    // base tree are gitlink boundaries and are exempt from the walk.
    let mut violations: Vec<String> = scan_nested_git(&canon_work, &base_gitlinks);
    if !violations.is_empty() {
        return Err(record_commit_violation(repo, m, violations));
    }

    let mut index = wt_repo.index()?;

    {
        let mut cb = |path: &Path, _matched: &[u8]| -> i32 {
            match staged_path_violation(&canon_work, path) {
                None => 0, // stage it
                Some(v) => {
                    violations.push(v);
                    1 // skip — but any violation fails the commit below
                }
            }
        };
        index.add_all(
            ["*"].iter(),
            git2::IndexAddOption::DEFAULT,
            Some(&mut cb as &mut git2::IndexMatchedPath),
        )?;
        index.update_all(["*"].iter(), None)?;
    }

    // Post-stage sweep: reject submodule gitlink entries (mode 160000) that
    // libgit2 may have recorded for a nested repo — an agent could otherwise
    // smuggle a pointer to an arbitrary commit. A gitlink that is byte-identical
    // to the base tree (same path, same OID) is a pre-existing upstream
    // submodule and round-trips; anything *new* or *re-pointed* fails closed.
    for entry in index.iter() {
        if entry.mode == 0o160000 {
            let path = String::from_utf8_lossy(&entry.path).into_owned();
            if base_gitlinks.get(&path) == Some(&entry.id) {
                continue; // unchanged upstream submodule
            }
            violations.push(format!(
                "{path}: nested git repository (gitlink) — not allowed in a mediated commit"
            ));
        }
    }

    if !violations.is_empty() {
        return Err(record_commit_violation(repo, m, violations));
    }

    let tree_oid = index.write_tree()?;
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

/// Gitlinks (mode 160000) recorded in `tree`, keyed by repo-relative path →
/// committed OID. These are the upstream submodules the env inherited from its
/// base; the mediated commit lets them round-trip unchanged (see
/// [`mediated_commit`]). Paths use git's forward-slash form.
fn base_gitlinks(tree: &git2::Tree) -> HashMap<String, git2::Oid> {
    let mut out = HashMap::new();
    // `dir` is the parent prefix ("" at the root, "examples/" one level down).
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.filemode() == 0o160000 {
            if let Some(name) = entry.name() {
                out.insert(format!("{dir}{name}"), entry.id());
            }
        }
        git2::TreeWalkResult::Ok
    });
    out
}

/// Walk the worktree (without following symlinks) and report every nested
/// `.git` entry — a directory (embedded repo) or file (gitlink) anywhere
/// below the root. The root's own `.git` gitlink is the worktree's plumbing
/// and is exempt; so is any registered upstream submodule (a path present as a
/// gitlink in `base_gitlinks`), whose entire subtree is a boundary owned by the
/// submodule, not the parent commit.
fn scan_nested_git(work: &Path, base_gitlinks: &HashMap<String, git2::Oid>) -> Vec<String> {
    fn rel(path: &Path, root: &Path) -> String {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
    fn walk(dir: &Path, root: &Path, base: &HashMap<String, git2::Oid>, out: &mut Vec<String>) {
        // A registered submodule is a gitlink boundary: its whole subtree belongs
        // to the submodule, not the parent. Skip it wholesale — the gitlink
        // itself round-trips through the post-stage sweep, validated by OID.
        if dir != root && base.contains_key(&rel(dir, root)) {
            return;
        }
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
                walk(&path, root, base, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(work, work, base_gitlinks, &mut out);
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
    // Hold the per-env lock for the whole mediated commit + status write: a
    // concurrent `env run`/`shell` mutates the same worktree and manifest, and
    // its terminal IDLE write would otherwise clobber the PROPOSED we set here.
    // Taken before the status check so a LIVE run fails fast ("busy") while a
    // stale `running` left by a crashed run (flock released on death) still lets
    // propose through. See ST_RUNNING in the accepted set below.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
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
    brief.push_str(&format!(
        "  base    : {} (from {})\n",
        &m.base_commit[..12],
        m.parent_branch
    ));
    brief.push_str(&format!("  branch  : {}\n", m.branch));
    brief.push_str(&format!(
        "  policy  : profile={} isolation={} digest={}\n",
        m.profile,
        m.isolation_claim,
        &m.policy_digest[..12.min(m.policy_digest.len())]
    ));
    brief.push_str(&format!(
        "  evidence: {} capture(s): {}\n",
        m.captures.len(),
        m.captures.join(", ")
    ));
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
    // Serialize the PROPOSED→APPLIED transition (reads the env state, mutates
    // the manifest) against any concurrent run/shell on the same env.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
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
        set_status(
            repo,
            h5i_root,
            m,
            ST_APPLIED,
            "applied",
            Some("no-op (no divergence)".into()),
            None,
        )?;
        return Ok(format!(
            "{}: nothing to apply (env tip == parent tip)",
            m.id
        ));
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

    // Stamp the applied commit with env provenance (links it back to the env +
    // a labeled evidence summary) so the parent-branch commit is
    // self-describing. Best-effort — the merge is already committed.
    let prov_note = stamp_apply_provenance(repo, m, new_commit);

    // Fold the env's reasoning back into the parent context branch. The code
    // is already applied — a context-merge failure is surfaced, not fatal.
    let ctx_note =
        match crate::ctx::gcc_merge_into(workdir, &m.parent_context_branch, &m.context_branch) {
            Ok(_) => format!(
                "context '{}' merged into '{}'",
                m.context_branch, m.parent_context_branch
            ),
            Err(e) => format!(
                "WARNING: context merge-back failed ({e}) — run `h5i context merge {}` manually",
                m.context_branch
            ),
        };

    // Evidence summary on the `applied` event, linking the env's captures to the
    // commit they now live on (the dashboards/event log resolve env → result).
    let lanes = evidence_sources_by_lane(repo, m)
        .into_iter()
        .map(|(s, n)| format!("{s}={n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let evidence_note = if m.captures.is_empty() {
        String::new()
    } else {
        format!(" evidence={} [{}]", m.captures.len(), lanes)
    };

    set_status(
        repo,
        h5i_root,
        m,
        ST_APPLIED,
        "applied",
        Some(format!(
            "{} {} → {} ({new_commit}){evidence_note}",
            if patch_mode { "patch" } else { "merge" },
            m.branch_short(),
            m.parent_branch
        )),
        None,
    )?;
    Ok(format!(
        "{} applied onto {} ({}{})\n{}\n{}",
        m.id,
        m.parent_branch,
        &new_commit.to_string()[..12],
        if base_oid == parent_tip.id() && !patch_mode {
            ", fast-forward"
        } else {
            ""
        },
        prov_note,
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
pub fn rebase(repo: &Repository, h5i_root: &Path, m: &mut EnvManifest) -> Result<String, H5iError> {
    // Rebase force-checks-out the worktree and re-pins the base in the manifest;
    // serialize against a concurrent `env run`/`shell` exactly like propose.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
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
            return Ok(format!(
                "{} is already on its parent tip — nothing to rebase",
                m.id
            ))
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
        if m.status == ST_CREATED {
            ST_CREATED
        } else {
            ST_IDLE
        },
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
    // Mutates the manifest status; serialize against a concurrent run/shell so
    // a run's terminal IDLE write can't clobber the ABORTED set here (a live run
    // holds the lock → abort waits/fails "busy" until it ends or is killed).
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
    if m.status == ST_APPLIED {
        return Err(H5iError::Metadata(format!(
            "{} is already applied — nothing to abort",
            m.id
        )));
    }
    set_status(
        repo,
        h5i_root,
        m,
        ST_ABORTED,
        "aborted",
        Some("manifest preserved for forensics".into()),
        None,
    )
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
    let live = matches!(
        m.status.as_str(),
        ST_CREATED | ST_RUNNING | ST_IDLE | ST_PROPOSED
    );
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
    if agent_dir
        .read_dir()
        .map(|mut d| d.next().is_none())
        .unwrap_or(false)
    {
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

    // A manifest in the exact canonical shape `create` always produces.
    fn canonical_manifest(agent: &str, slug: &str) -> EnvManifest {
        EnvManifest {
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
            status: ST_IDLE.into(),
            captures: vec![],
        }
    }

    #[test]
    fn write_note_spool_sanitizes_filename_and_rejects_empty_oid() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        // A normal hex oid → `note-<oid>.json`.
        let oid = "a".repeat(40);
        write_note_spool(&spool, &oid, "{\"x\":1}").unwrap();
        assert!(spool.join(format!("note-{oid}.json")).is_file());
        // A hostile "oid" with path/shell chars is stripped to its alnum run.
        write_note_spool(&spool, "../../evil-#$", "{}").unwrap();
        let names: Vec<String> = std::fs::read_dir(&spool)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| n.starts_with("note-") && n.ends_with(".json")));
        assert!(!names.iter().any(|n| n.contains("..") || n.contains('/') || n.contains('#')));
        // An all-non-alnum oid leaves nothing to name → error, no file written.
        assert!(write_note_spool(&spool, "../", "{}").is_err());
    }

    #[test]
    fn build_env_provenance_caps_captures_and_counts_lanes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut m = canonical_manifest("claude", "fix");
        // 100 capture ids; none resolve in this fresh repo → all "unknown".
        m.captures = (0..100).map(|i| format!("env/claude/fix/cap{i}")).collect();

        let prov = build_env_provenance(&repo, &m);
        // Identity fields come straight from the (validated) manifest.
        assert_eq!(prov.env_id, "env/claude/fix");
        assert_eq!(prov.agent, "claude");
        assert_eq!(prov.isolation_claim, "workspace");
        assert_eq!(prov.base_commit, "c".repeat(40));
        // Inlined ids are capped; the true total is preserved.
        assert_eq!(prov.captures.len(), APPLY_PROVENANCE_CAP);
        assert_eq!(prov.captures_total, 100);
        // Unresolvable ids are counted as `unknown`, never dropped.
        assert_eq!(prov.evidence_sources.get("unknown"), Some(&100));

        // No captures → empty lanes, zero total.
        let mut empty = canonical_manifest("claude", "fix");
        empty.captures.clear();
        let prov = build_env_provenance(&repo, &empty);
        assert_eq!(prov.captures_total, 0);
        assert!(prov.evidence_sources.is_empty());
    }

    #[test]
    fn imported_manifest_validation_rejects_traversal_and_identity_tampering() {
        // Canonical (what `create` produces) passes.
        assert!(validate_imported_manifest(&canonical_manifest("claude", "fix")).is_ok());

        // Traversal in the fields that become filesystem paths — the core of the
        // path-escape: `env_dir(.., agent, slug)` joins them unchecked.
        let mut m = canonical_manifest("claude", "fix");
        m.agent = "../../../../tmp/evil".into();
        assert!(
            validate_imported_manifest(&m).is_err(),
            "traversal agent rejected"
        );
        let mut m = canonical_manifest("claude", "fix");
        m.slug = "../escape".into();
        assert!(
            validate_imported_manifest(&m).is_err(),
            "traversal slug rejected"
        );

        // Identity fields must match the shape derived from agent/slug even when
        // agent/slug are individually valid — defeats a manifest whose
        // id/branch/context point elsewhere (e.g. spoofing another env's files).
        for tamper in [
            |m: &mut EnvManifest| m.id = "env/claude/other".into(),
            |m: &mut EnvManifest| m.branch = "refs/heads/main".into(),
            |m: &mut EnvManifest| m.context_branch = "env/claude/other".into(),
        ] {
            let mut m = canonical_manifest("claude", "fix");
            tamper(&mut m);
            assert!(
                validate_imported_manifest(&m).is_err(),
                "identity mismatch rejected"
            );
        }
    }

    // In-box git grants: the exact plumbing surface a boxed agent needs to use
    // git/h5i in its worktree — and nothing protected (`.git` root, hooks,
    // `refs/h5i/env` meta, the manifest dir).
    #[test]
    fn box_git_grants_cover_worktree_plumbing_and_nothing_protected() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path().join("repo")).unwrap();
        let git_dir = repo.commondir().to_path_buf();
        let m = canonical_manifest("claude", "fix");

        let paths = box_git_plumbing(&repo, &m).unwrap();
        let ro: Vec<String> = paths
            .iter()
            .filter(|p| !p.rw)
            .map(|p| p.host.display().to_string())
            .collect();
        let rw: Vec<String> = paths
            .iter()
            .filter(|p| p.rw)
            .map(|p| p.host.display().to_string())
            .collect();
        // List order doubles as container mount order: the ro parent `refs`
        // must precede the rw entries bind-nested under it.
        let refs_pos = paths
            .iter()
            .position(|p| !p.rw && p.host.ends_with("refs"))
            .unwrap();
        let nested_pos = paths
            .iter()
            .position(|p| p.host.ends_with("refs/h5i/context"))
            .unwrap();
        assert!(
            refs_pos < nested_pos,
            "parent `refs` must come before nested rw children"
        );

        let has = |v: &[String], suffix: &str| v.iter().any(|p| p.ends_with(suffix));
        // Reads: repo metadata files/dirs, never `.git` itself.
        for want in ["/HEAD", "/config", "/packed-refs", "/refs", "/info"] {
            assert!(has(&ro, want), "ro grant {want} missing: {ro:?}");
        }
        assert!(
            !ro.iter().chain(rw.iter()).any(|p| Path::new(p) == git_dir),
            "the .git dir itself must never be granted"
        );
        // Writes: own admin dir, objects, own agent's ref ns (+ reflog), context ns.
        for want in [
            "/worktrees/h5i-env-claude-fix",
            "/objects",
            "/refs/heads/h5i/env/claude",
            "/logs/refs/heads/h5i/env/claude",
            "/refs/h5i/context",
        ] {
            assert!(has(&rw, want), "rw grant {want} missing: {rw:?}");
        }
        // Protected surfaces stay out of every grant.
        for never in ["hooks", "refs/h5i/env", "manifest", "policy"] {
            assert!(
                !ro.iter().chain(rw.iter()).any(|p| p.ends_with(never)),
                "protected path '{never}' must not be granted"
            );
        }
        // rw dirs exist afterwards (the Landlock builder skips missing paths,
        // which would silently brick in-box git)…
        for d in &rw {
            assert!(Path::new(d).is_dir(), "rw grant {d} not materialized");
        }
        // …including RE-creation after a host-side `git pack-refs` pruned the
        // loose-ref dir.
        std::fs::remove_dir_all(git_dir.join("refs/heads/h5i")).unwrap();
        box_git_plumbing(&repo, &m).unwrap();
        assert!(
            git_dir.join("refs/heads/h5i/env/claude").is_dir(),
            "pruned ref dir recreated"
        );
    }

    // The nested worktree means agent runtimes discover the PROJECT config by
    // walking up to the main repo root; the box must be able to READ it (so
    // config discovery + the observation hook work) but not write it.
    #[test]
    fn grant_box_git_reads_main_repo_project_config() {
        use crate::sandbox::Profile;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo"); // == commondir().parent()
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(root.join(".codex/config.toml"), "[hooks]\n").unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let m = canonical_manifest("claude", "fix");
        let work = root.join(".git/.h5i/env/claude/fix/work");
        std::fs::create_dir_all(&work).unwrap();

        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut pol).unwrap();

        let codex = root.join(".codex").display().to_string();
        let claude = root.join(".claude").display().to_string();
        // Existing project-config dirs are READ-granted, never write-granted.
        assert!(pol.profile.fs_read.contains(&codex), "main-repo .codex read: {:?}", pol.profile.fs_read);
        assert!(pol.profile.fs_read.contains(&claude), "main-repo .claude read");
        assert!(!pol.profile.fs_write.contains(&codex), "stays immutable");
        assert!(!pol.profile.fs_write.contains(&claude), "stays immutable");

        // An absent dir is not granted (no phantom grant), and container leaves
        // fs lists alone (it doesn't share the host repo tree).
        std::fs::remove_dir_all(root.join(".claude")).unwrap();
        let mut pol2 = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut pol2).unwrap();
        assert!(!pol2.profile.fs_read.contains(&claude), "absent dir not granted");
    }

    // The same plumbing is applied per backend: Landlock grants (+ global
    // gitconfig reads) at process/supervised; identical-path bind mounts on
    // `policy.box_git` (incl. the `$WORK` dual mount, exists-filtered, fs
    // lists untouched) at container; nothing at workspace.
    #[test]
    fn grant_box_git_applies_per_backend() {
        use crate::sandbox::Profile;
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path().join("repo")).unwrap();
        std::fs::write(
            dir.path().join("repo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let m = canonical_manifest("claude", "fix");
        let work = dir.path().join("repo/.git/.h5i/env/claude/fix/work");
        std::fs::create_dir_all(&work).unwrap();

        // process: fs grants + ~/.gitconfig, box_git untouched.
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut pol).unwrap();
        assert!(pol.profile.fs_write.iter().any(|p| p.ends_with("/objects")));
        assert!(pol.profile.fs_read.iter().any(|p| p == "~/.gitconfig"));
        assert!(
            pol.profile
                .fs_read
                .iter()
                .any(|p| p.ends_with("/repo/Cargo.toml")),
            "cargo workspace discovery needs parent Cargo.toml read: {:?}",
            pol.profile.fs_read
        );
        assert!(
            pol.box_git.is_empty(),
            "kernel tiers use fs grants, not mounts"
        );

        // container: mounts on box_git (work included, all existing), fs lists untouched.
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Container,
            Profile::builtin("default", IsolationClaim::Container),
        );
        let (read_before, write_before) =
            (pol.profile.fs_read.clone(), pol.profile.fs_write.clone());
        grant_box_git(&repo, &m, &work, &mut pol).unwrap();
        assert!(!pol.box_git.is_empty());
        assert!(
            pol.box_git.iter().any(|b| b.rw && b.host == work),
            "container must dual-mount $WORK at its host path: {:?}",
            pol.box_git
        );
        assert!(
            pol.box_git
                .iter()
                .any(|b| !b.rw && b.host.ends_with("Cargo.toml")),
            "container must bind parent Cargo.toml for workspace discovery: {:?}",
            pol.box_git
        );
        assert!(
            pol.box_git.iter().all(|b| b.host.exists()),
            "podman needs existing sources"
        );
        assert!(
            !pol.box_git
                .iter()
                .any(|b| b.host.to_string_lossy().contains('~')),
            "no tilde paths in mounts (host HOME is not the container's)"
        );
        assert_eq!(pol.profile.fs_read, read_before);
        assert_eq!(pol.profile.fs_write, write_before);

        // workspace: unconfined, nothing applied.
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Workspace,
            Profile::builtin("default", IsolationClaim::Workspace),
        );
        let read_before = pol.profile.fs_read.clone();
        grant_box_git(&repo, &m, &work, &mut pol).unwrap();
        assert!(pol.box_git.is_empty());
        assert_eq!(pol.profile.fs_read, read_before);
    }

    #[test]
    fn prepare_cargo_env_keeps_target_outputs_inside_worktree() {
        use crate::sandbox::Profile;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );

        let env = prepare_cargo_env(&work, &pol).unwrap();
        let target = env
            .iter()
            .find(|(k, _)| k == "CARGO_TARGET_DIR")
            .map(|(_, v)| PathBuf::from(v))
            .unwrap();
        assert!(target.starts_with(&work), "{target:?}");
        assert!(target.is_dir(), "{target:?}");
        assert!(
            env.iter().all(|(k, _)| k != "CARGO_INSTALL_ROOT"),
            "cargo install is not part of the default sandbox workflow: {env:?}"
        );
    }

    // Fix for the propose/rebase-vs-run race: every worktree/manifest-mutating
    // review op takes the per-env lock first, so a live run (which holds it)
    // makes them fail fast instead of racing the run's writes. The lock is the
    // first statement in each, so it refuses before touching repo/worktree.
    #[cfg(unix)]
    #[test]
    fn review_ops_refuse_while_run_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let repo = git2::Repository::init(h5i_root.join("repo")).unwrap();
        let mut m = canonical_manifest("claude", "fix");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();

        // Simulate a live `env run`/`shell` holding the per-env lock.
        let _held = RunLock::acquire(&m.dir(h5i_root)).unwrap();

        let busy = |r: Result<String, H5iError>, who: &str| {
            let e = r.expect_err(who);
            assert!(
                format!("{e}").contains("busy"),
                "{who}: expected busy, got: {e}"
            );
        };
        busy(propose(&repo, h5i_root, &mut m), "propose");
        busy(rebase(&repo, h5i_root, &mut m), "rebase");
        busy(
            apply(&repo, h5i_root, h5i_root, &mut m, false).map(|_| String::new()),
            "apply",
        );
        let e = abort(&repo, h5i_root, &mut m).expect_err("abort");
        assert!(format!("{e}").contains("busy"), "abort: {e}");
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
        let bare = EnvEvent {
            detail: None,
            capture: None,
            ..e
        };
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
    fn scan_nested_git_exempts_registered_submodules_only() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        // Two checked-out nested repos: one is a registered base submodule, the
        // other is an embedded repo the agent dropped in.
        std::fs::create_dir_all(work.join("examples/sub")).unwrap();
        std::fs::create_dir_all(work.join("vendor/dep")).unwrap();
        std::fs::write(work.join("examples/sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        std::fs::write(work.join("vendor/dep/.git"), "gitdir: elsewhere\n").unwrap();
        let canon = work.canonicalize().unwrap();

        // No base submodules → every nested repo is a violation (legacy behavior).
        let empty: HashMap<String, git2::Oid> = HashMap::new();
        assert_eq!(scan_nested_git(&canon, &empty).len(), 2);

        // Register examples/sub as a base gitlink → only it is exempt; the
        // agent-introduced vendor/dep still fails closed.
        let mut base = HashMap::new();
        base.insert("examples/sub".to_string(), git2::Oid::zero());
        let v = scan_nested_git(&canon, &base);
        assert_eq!(v.len(), 1, "only the unregistered nested repo flagged: {v:?}");
        assert!(v[0].contains("vendor/dep"), "{v:?}");
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
        assert_eq!(
            find(h5i_root, "env/claude/fix").unwrap().id,
            "env/claude/fix"
        );
        // Unknown name errors.
        assert!(find(h5i_root, "ghost").is_err());
    }
}
