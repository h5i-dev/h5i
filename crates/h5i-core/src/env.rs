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
/// Per-env live-session registry dir (`live/<pid>.json`) — see [`LiveSession`].
const LIVE_DIR: &str = "live";
/// Worktree-root file the persona sources are baked into at create; loaded by
/// the agent via `@PERSONA.md` (Claude) or a read instruction (Codex).
const PERSONA_FILE: &str = "PERSONA.md";

pub const H5I_ENV_ID_VAR: &str = "H5I_ENV_ID";
pub const H5I_ENV_POLICY_DIGEST_VAR: &str = "H5I_ENV_POLICY_DIGEST";
pub const H5I_ENV_CAPTURE_SPOOL_VAR: &str = "H5I_ENV_CAPTURE_SPOOL";
pub const H5I_ENV_AUDIT_CAPTURE_VAR: &str = "H5I_ENV_AUDIT_CAPTURE";
pub const H5I_TEAM_VAR: &str = "H5I_TEAM";
/// In-box path to the per-env read-only inbound mailbox (host fans messages in;
/// the box reads via `h5i team agent inbox`/`--wait`/the team Stop hook).
pub const H5I_ENV_INBOX_VAR: &str = "H5I_ENV_INBOX";
/// The env's pinned base *tree* OID, exported into team-bound boxes so
/// `h5i team agent submit` can refuse a provably empty submission in-box
/// (the box can't read the sealed team refs to learn the base itself).
pub const H5I_ENV_BASE_TREE_VAR: &str = "H5I_ENV_BASE_TREE";
/// In-box mountpoints, identical on every image-backed tier so nothing running
/// *inside* a box needs to know whether it booted under Podman or a microVM.
const BOX_CAPTURE_SPOOL: &str = "/.h5i/spool";
const BOX_INBOX_MOUNT: &str = "/.h5i/inbox";
/// Inbox subdir under the env admin dir; mounted read-only into the box.
const ENV_INBOX_DIR: &str = "inbox";
#[cfg(unix)] // only the unix-gated RunLock references this
const RUN_LOCK_FILE: &str = "run.lock";
#[cfg(unix)] // only the unix-gated RunLock references this
const OBSERVERS_LOCK_FILE: &str = "observers.lock";

/// Advisory `flock`s that coordinate concurrent work on one environment. The
/// kernel releases a lock when the holding process exits — including on a crash
/// — so there are never stale locks to clear.
///
/// Two *independent* lock files implement the model "one read-write session
/// **plus** N read-only observers, and a worktree teardown that first drains the
/// observers":
///
/// - **`run.lock` — writer serialization.** [`RunLock::acquire`] takes an
///   exclusive (`LOCK_EX`) lock. Every mutating session/op holds it: `env run`,
///   a read-write `env shell`, `propose`, `apply`, `rebase`, `abort`, team sync.
///   A read-write session mutates the worktree, status file, captures list, and
///   manifest, which must never interleave — so at most one writer runs at once.
///   Observers do **not** take this lock, so a writer and observers coexist.
///
/// - **`observers.lock` — observer presence gate.** A read-only observer session
///   (`env shell --readonly`) holds a shared (`LOCK_SH`) lock for its whole life
///   ([`RunLock::acquire_observer`]); many coexist. It is *not* coupled to
///   `run.lock`, so an observer may attach while a read-write session is live.
///   The observer may then see torn reads — expected when watching work in
///   progress; write-isolation is enforced by the read-only Landlock/mount on
///   `$WORK`, never by this lock. The only thing that excludes an observer is a
///   **teardown**: an op that *removes* the worktree (`gc`, `rm`) first takes an
///   exclusive lock here via [`RunLock::acquire_teardown`], so the directory an
///   observer has mounted can never vanish underneath it.
///
/// A teardown op takes both locks, always in the order `run.lock` then
/// `observers.lock`: the exclusive `run.lock` still serializes it against other
/// writers, and the exclusive `observers.lock` drains observers. All locks are
/// non-blocking (`LOCK_NB`): a contended acquire refuses immediately with a
/// clear "busy" message rather than waiting, so no acquire order can deadlock.
#[cfg(unix)]
struct RunLock {
    _file: std::fs::File,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum LockMode {
    Exclusive,
    Shared,
}

/// Non-blocking probe: is a writer session (interactive shell, `run`, or a
/// mutating op) currently holding this env's `run.lock`? pub(crate): the
/// orchestra preflight uses it as its resident-session liveness heuristic.
/// A brief host op also holds the lock, so callers should sample more than
/// once before concluding either way — this is a heuristic, not a guarantee.
#[cfg(unix)]
pub fn writer_session_live(env_dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let path = env_dir.join(RUN_LOCK_FILE);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        true
    }
}

#[cfg(not(unix))]
pub fn writer_session_live(_env_dir: &Path) -> bool {
    false
}

/// Which role is taking the lock — selects the "busy" message on contention.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum LockRole {
    /// Exclusive `run.lock` for a mutating session/op (`run`, read-write `shell`,
    /// `propose`, `apply`, `rebase`, `abort`, …).
    Writer,
    /// Shared `observers.lock` for a read-only observer session.
    Observer,
    /// Exclusive `observers.lock` for a worktree teardown (`gc`/`rm`).
    Teardown,
}

#[cfg(unix)]
impl RunLock {
    /// Exclusive writer lock on `run.lock` — serializes mutating sessions/ops
    /// against each other. Does **not** exclude read-only observers.
    fn acquire(env_dir: &Path) -> Result<RunLock, H5iError> {
        Self::flock(env_dir, RUN_LOCK_FILE, LockMode::Exclusive, LockRole::Writer)
    }

    /// Shared observer-presence lock on `observers.lock` — coexists with other
    /// observers *and* with a live read-write session; excluded only by a
    /// teardown that is about to remove the worktree.
    fn acquire_observer(env_dir: &Path) -> Result<RunLock, H5iError> {
        Self::flock(
            env_dir,
            OBSERVERS_LOCK_FILE,
            LockMode::Shared,
            LockRole::Observer,
        )
    }

    /// Exclusive teardown lock on `observers.lock` — held by an op that removes
    /// the worktree (`gc`/`rm`) to drain live observers first. Refused (non-
    /// blocking) while any observer is attached.
    fn acquire_teardown(env_dir: &Path) -> Result<RunLock, H5iError> {
        Self::flock(
            env_dir,
            OBSERVERS_LOCK_FILE,
            LockMode::Exclusive,
            LockRole::Teardown,
        )
    }

    fn flock(
        env_dir: &Path,
        lock_file: &str,
        mode: LockMode,
        role: LockRole,
    ) -> Result<RunLock, H5iError> {
        use std::os::unix::io::AsRawFd;
        let path = env_dir.join(lock_file);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        let op = match mode {
            LockMode::Exclusive => libc::LOCK_EX,
            LockMode::Shared => libc::LOCK_SH,
        } | libc::LOCK_NB;
        let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                let msg = match role {
                    // Another writer (or a teardown's `run.lock` hold) is live.
                    // Observers never take `run.lock`, so they can't cause this.
                    LockRole::Writer => "environment is busy — another `h5i box run`/`shell` \
                         or lifecycle op (propose/apply/rebase/abort) holds it",
                    // A teardown holds `observers.lock` exclusively.
                    LockRole::Observer => "environment is being torn down (gc/rm) — a \
                         `--readonly` observer can attach only once that completes",
                    // Live read-only observers hold `observers.lock` shared.
                    LockRole::Teardown => "environment is busy — it has live `--readonly` \
                         observer session(s); this op removes the worktree and can proceed only \
                         once every observer exits",
                };
                return Err(H5iError::Metadata(msg.into()));
            }
            return Err(H5iError::with_path(err, &path));
        }
        Ok(RunLock { _file: file })
    }
}

/// Removes a read-only observer session's per-session scratch root
/// (`<env>/ro/<pid>/`) on drop — on every return path and on panic. The scratch
/// holds the observer's ephemeral HOME copy, `/tmp`, brokered secrets, and cargo
/// target; it is safe to remove once the confined child (whose mount namespace
/// held the binds) has exited.
struct SessionScratchGuard(Option<PathBuf>);

impl Drop for SessionScratchGuard {
    fn drop(&mut self) {
        if let Some(dir) = &self.0 {
            let _ = std::fs::remove_dir_all(dir);
        }
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
    /// Where the code came from: `repo`, `clone:<url>`, or `new`. A box that
    /// is not `repo` is **detached** — its git repository lives inside the box
    /// directory, the host repository was never touched, and `export` is the
    /// only way out. Defaults to `repo` so manifests written before this field
    /// existed keep their meaning.
    #[serde(default = "default_source")]
    pub source: String,
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
    /// sha256 over the env-local pinned service manifest (`services.json`),
    /// snapshotted at create from the base's `.h5i/env.toml`. `None` for envs
    /// created before services existed (or with no `[service.*]`). Pins the
    /// service declarations so an agent can't edit the worktree config to start
    /// a different long-lived command than the reviewer approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_digest: Option<String>,
    /// sha256 of the `PERSONA.md` baked from the profile's `persona = [...]`
    /// sources at create — provenance for the agent's standing working style.
    /// `None` when the profile declares no persona. The content lives in the
    /// worktree (git-excluded, so it never enters the agent's diff/commit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_digest: Option<String>,
    /// GitHub PR number this env tracks (`env create --pr`): the base is the
    /// PR's head, `parent_branch` its local `pr/<n>` tracking branch, and
    /// apply prints a push-back hint. Absent for ordinary envs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// The PR's head branch name on its source repo (via `gh`, best-effort) —
    /// the target of the push-back hint after apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_head_ref: Option<String>,
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

/// Validate a service name before it is used to build env-local paths
/// (`services/<name>.json`, `services/<name>.log`). Same strict slug rules as
/// [`validate_slug`], so a key like `../manifest` (path traversal) or one with a
/// `/` can never escape the services dir or overwrite an env-local file.
pub fn validate_service_name(name: &str) -> Result<(), H5iError> {
    validate_slug(name).map_err(|_| {
        H5iError::Metadata(format!(
            "invalid service name '{name}' — use lowercase letters, digits, '-', '_', '.' \
             (start alphanumeric, ≤64 chars, no '/' or '..')"
        ))
    })
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
    ];
    for (field, got, want) in checks {
        if *got != want {
            return Err(H5iError::Metadata(format!(
                "manifest {field} is not the canonical '{want}' (got '{}')",
                crate::redact::sanitize_display(got)
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

    let mut last_err: Option<git2::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        crate::refstore::cas_backoff(attempt);
        let tip = repo.refname_to_id(ENV_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let mut log =
            crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), EVENTS_FILE).unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&event_line);
        log.push('\n');

        let mut files: Vec<(&str, String)> = vec![(EVENTS_FILE, log)];
        if let (Some(m), Some(line)) = (manifest, &manifest_line) {
            let existing = crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
                .unwrap_or_default();
            files.push((MANIFESTS_FILE, upsert_jsonl_by_id(&existing, &m.id, line)));
        }
        if let (Some(m), Some(toml)) = (manifest, policy_toml) {
            let existing = crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE)
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
        let tree_oid = crate::refstore::build_tree(repo, base_tree.as_ref(), &file_refs)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = crate::refstore::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        match crate::refstore::cas_ref_update(repo, ENV_REF, tip, new_oid, &message) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(H5iError::Internal(format!(
        "h5i env: event {} for {} could not be appended after {MAX_ATTEMPTS} attempts{}",
        ev.event,
        ev.env_id,
        crate::refstore::cas_error_detail(&last_err)
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

    let mut last_err: Option<git2::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        crate::refstore::cas_backoff(attempt);
        let tip = repo.refname_to_id(ENV_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let mut log =
            crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), EVENTS_FILE).unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&event_line);
        log.push('\n');

        let manifests = remove_jsonl_by_id(
            &crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
                .unwrap_or_default(),
            &ev.env_id,
        );
        let policies = remove_jsonl_by_id(
            &crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE)
                .unwrap_or_default(),
            &ev.env_id,
        );

        let files: Vec<(&str, &str)> = vec![
            (EVENTS_FILE, log.as_str()),
            (MANIFESTS_FILE, manifests.as_str()),
            (POLICIES_FILE, policies.as_str()),
        ];
        let tree_oid = crate::refstore::build_tree(repo, base_tree.as_ref(), &files)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = crate::refstore::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        match crate::refstore::cas_ref_update(repo, ENV_REF, tip, new_oid, &message) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(H5iError::Internal(format!(
        "h5i env: removal of {} could not be committed after {MAX_ATTEMPTS} attempts{}",
        ev.env_id,
        crate::refstore::cas_error_detail(&last_err)
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
    let Some(raw) = crate::refstore::read_blob_from_tree(repo, Some(&tree), MANIFESTS_FILE) else {
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
    let Some(raw) = crate::refstore::read_blob_from_tree(repo, Some(&tree), POLICIES_FILE) else {
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
/// `h5i box list`/`status`/`diff`/`apply`. Worktrees are inherently local, so a
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
                crate::redact::sanitize_display(&m.id)
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
        if let Some(toml) = policies.get(&m.id) {
            // Guard against a ref whose manifest and policy blob were written by
            // different h5i versions/operations (e.g. an env id recreated after a
            // version bump): writing both would land an env whose
            // policy.resolved.toml doesn't match its pinned digest — surfacing
            // later as a confusing "tampered policy" failure. Verify first, with
            // the SAME check load_policy runs, and skip (don't write a broken env).
            let consistent = ResolvedPolicy::from_toml(toml)
                .and_then(|p| p.digest())
                .map(|d| d == m.policy_digest)
                .unwrap_or(false);
            if !consistent {
                eprintln!(
                    "warning: skipping shared env '{}' — its stored policy does not match the \
                     pinned digest (likely created by a different h5i version); recreate it: \
                     `h5i box rm {} --force` then `h5i box create`",
                    crate::redact::sanitize_display(&m.id),
                    crate::redact::sanitize_display(&m.slug)
                );
                continue;
            }
            save_manifest(h5i_root, &m)?;
            let path = dir.join(POLICY_RESOLVED_FILE);
            std::fs::write(&path, toml).map_err(|e| H5iError::with_path(e, &path))?;
        } else {
            save_manifest(h5i_root, &m)?;
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
    let Some(raw) = crate::refstore::read_blob_from_tree(repo, Some(&tree), EVENTS_FILE) else {
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
            crate::refstore::read_blob_from_tree(repo, tree.as_ref(), EVENTS_FILE).unwrap_or_default();
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
            crate::refstore::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE).unwrap_or_default();
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
            crate::refstore::read_blob_from_tree(repo, tree.as_ref(), POLICIES_FILE).unwrap_or_default();
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
    let tree_oid = crate::refstore::build_tree(repo, base_tree.as_ref(), &files)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = crate::refstore::signature(repo)?;
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

/// Ingest one meta tree (`events`/`manifests`/`policies`) into the accumulators,
/// optionally restricting to a set of env ids. With `filter = None` everything is
/// taken (used for the remote base — preserved wholesale); with `filter = Some`
/// only records for the matching envs are taken (used for the local side).
#[allow(clippy::too_many_arguments)]
fn ingest_meta_tree(
    repo: &Repository,
    tree: Option<&git2::Tree>,
    filter: Option<&HashSet<String>>,
    seen_events: &mut HashSet<String>,
    events: &mut Vec<EnvEvent>,
    manifests: &mut HashMap<String, EnvManifest>,
    policies: &mut std::collections::BTreeMap<String, String>,
) {
    let raw = crate::refstore::read_blob_from_tree(repo, tree, EVENTS_FILE).unwrap_or_default();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<EnvEvent>(line) {
            if filter.is_some_and(|f| !f.contains(&e.env_id)) {
                continue;
            }
            let key = format!("{}|{}|{}", e.env_id, e.ts, e.event);
            if seen_events.insert(key) {
                events.push(e);
            }
        }
    }
    let mraw = crate::refstore::read_blob_from_tree(repo, tree, MANIFESTS_FILE).unwrap_or_default();
    for line in mraw.lines() {
        if let Ok(m) = serde_json::from_str::<EnvManifest>(line) {
            if filter.is_some_and(|f| !f.contains(&m.id)) {
                continue;
            }
            match manifests.get(&m.id) {
                Some(existing) if existing.updated_at >= m.updated_at => {}
                _ => {
                    manifests.insert(m.id.clone(), m);
                }
            }
        }
    }
    let praw = crate::refstore::read_blob_from_tree(repo, tree, POLICIES_FILE).unwrap_or_default();
    for line in praw.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let (Some(id), Some(toml)) = (
                v.get("id").and_then(|i| i.as_str()),
                v.get("toml").and_then(|t| t.as_str()),
            ) {
                if filter.is_some_and(|f| !f.contains(id)) {
                    continue;
                }
                policies
                    .entry(id.to_string())
                    .or_insert_with(|| toml.to_string());
            }
        }
    }
}

/// Env ids (`env/<agent>/<slug>`) on this clone whose `parent_branch` is `branch`
/// — the envs a user created while on that human branch. Public so a
/// branch-scoped `h5i share push` can also carry these envs' evidence captures
/// (which live in `refs/h5i/objects`, tagged with the env's own code branch).
pub fn local_env_ids_for_branch(repo: &Repository, branch: &str) -> HashSet<String> {
    let tree = repo
        .find_reference(ENV_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());
    let mut ids = HashSet::new();
    if let Some(raw) = crate::refstore::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE) {
        for line in raw.lines() {
            if let Ok(m) = serde_json::from_str::<EnvManifest>(line) {
                if m.parent_branch == branch {
                    ids.insert(m.id);
                }
            }
        }
    }
    ids
}

/// The local code-branch refs (`refs/heads/h5i/env/<agent>/<slug>`) of the envs
/// forked from `branch`. Used by a branch-scoped `h5i share push` to carry only
/// those envs' code onto the hidden `refs/h5i/env/code/*` namespace.
pub fn scoped_code_branch_refs(repo: &Repository, branch: &str) -> Vec<String> {
    let ids = local_env_ids_for_branch(repo, branch);
    let tree = repo
        .find_reference(ENV_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());
    let mut refs = Vec::new();
    if let Some(raw) = crate::refstore::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE) {
        for line in raw.lines() {
            if let Ok(m) = serde_json::from_str::<EnvManifest>(line) {
                if ids.contains(&m.id) && !m.branch.is_empty() {
                    refs.push(m.branch);
                }
            }
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

/// Build the commit to push for a branch-scoped `h5i share push` of the env meta
/// ref (`refs/h5i/env/meta`): `base`'s state (the remote tip, or empty) unioned
/// with the local events/manifests/policies for the envs forked from `branch`
/// (their `parent_branch`).
///
/// Non-destructive: the full `base` is preserved (other branches' envs on the
/// remote survive); only this branch's envs are added. The new commit descends
/// from `base` (the push fast-forwards), or is a root with no remote tip.
/// Returns `Ok(None)` when there is nothing to push — no env forked from
/// `branch` and no `base`.
pub fn build_branch_scoped_merge(
    repo: &Repository,
    branch: &str,
    base: Option<git2::Oid>,
) -> Result<Option<git2::Oid>, H5iError> {
    let matching = local_env_ids_for_branch(repo, branch);
    if base.is_none() && matching.is_empty() {
        return Ok(None);
    }

    let mut seen_events: HashSet<String> = HashSet::new();
    let mut events: Vec<EnvEvent> = Vec::new();
    let mut manifests: HashMap<String, EnvManifest> = HashMap::new();
    let mut policies: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    // Base first, unfiltered — preserve everything already on the remote.
    let base_commit = match base {
        Some(oid) => Some(repo.find_commit(oid)?),
        None => None,
    };
    let base_tree = base_commit.as_ref().and_then(|c| c.tree().ok());
    ingest_meta_tree(
        repo,
        base_tree.as_ref(),
        None,
        &mut seen_events,
        &mut events,
        &mut manifests,
        &mut policies,
    );
    // Local side, restricted to envs forked from this branch.
    let local_tree = repo
        .find_reference(ENV_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());
    ingest_meta_tree(
        repo,
        local_tree.as_ref(),
        Some(&matching),
        &mut seen_events,
        &mut events,
        &mut manifests,
        &mut policies,
    );

    events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.env_id.cmp(&b.env_id)));
    let mut log = String::new();
    for e in &events {
        log.push_str(&serde_json::to_string(e)?);
        log.push('\n');
    }
    let mut mlog = String::new();
    {
        let mut v: Vec<&EnvManifest> = manifests.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        for m in v {
            mlog.push_str(&serde_json::to_string(m)?);
            mlog.push('\n');
        }
    }
    let mut plog = String::new();
    for (id, toml) in &policies {
        plog.push_str(&serde_json::to_string(
            &serde_json::json!({"id": id, "toml": toml}),
        )?);
        plog.push('\n');
    }

    let mut files: Vec<(&str, &str)> = vec![(EVENTS_FILE, &log)];
    if !mlog.is_empty() {
        files.push((MANIFESTS_FILE, &mlog));
    }
    if !plog.is_empty() {
        files.push((POLICIES_FILE, &plog));
    }
    let base_tree_for_build = base_commit.as_ref().and_then(|c| c.tree().ok());
    let tree_oid = crate::refstore::build_tree(repo, base_tree_for_build.as_ref(), &files)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = crate::refstore::signature(repo)?;
    let message = format!("h5i push: branch-scoped env ({branch})");
    let parents: Vec<&git2::Commit> = base_commit.iter().collect();
    Ok(Some(
        repo.commit(None, &sig, &sig, &message, &tree, &parents)?,
    ))
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
            "no environment named '{name}' (see `h5i box list`)"
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
             (expected {}, found {digest}) — refusing to run under a tampered policy. \
             If you did not edit it, the env was most likely created by a different h5i \
             version; recreate it: `h5i box rm {} --force` then `h5i box create …`",
            m.id, m.policy_digest, m.slug
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

/// Serde default for [`EnvManifest::source`]: manifests written before boxes
/// could be detached are all worktrees of the host repository.
fn default_source() -> String {
    "repo".to_string()
}

/// Where a box's code comes from.
///
/// `Repo` is the historical shape: a git worktree of the host repository, so
/// the box shares its object store and can be applied back onto a branch.
/// The other two are **detached**: the code is copied into a repository that
/// lives inside the box's own directory, the host repository is never touched,
/// and the only way out is `h5i box export`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BoxSource {
    /// A worktree of this repository (the default).
    #[default]
    Repo,
    /// Clone a remote repository into the box.
    Clone { url: String },
    /// An empty box: a fresh repository with one empty root commit.
    New,
}

impl BoxSource {
    /// The value recorded in the manifest.
    pub fn as_manifest_str(&self) -> String {
        match self {
            BoxSource::Repo => "repo".to_string(),
            BoxSource::Clone { url } => format!("clone:{url}"),
            BoxSource::New => "new".to_string(),
        }
    }

    /// Detached sources own their git repository; there is no parent branch in
    /// the host repository to propose or apply onto.
    pub fn is_detached(&self) -> bool {
        !matches!(self, BoxSource::Repo)
    }
}

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
    /// `--image`: container base image, overriding whatever the profile (or the
    /// repo-level `[container] image` default) declares. Visible to the
    /// isolation auto-pick, so `--image` alone makes the container tier a
    /// candidate for an otherwise imageless profile.
    pub image: Option<String>,
    /// Workspace backend. `auto` and `worktree` are accepted today.
    pub backend: String,
    /// Command evidence policy for wrapped in-env commands.
    pub audit_capture: sandbox::AuditCapture,
    /// Override the parent branch (short name) the env proposes/applies back
    /// onto. `None` derives it from the current HEAD. `--pr` sets it to the
    /// PR's local tracking branch — the review target is the PR, not whatever
    /// branch the operator happened to have checked out.
    pub parent_branch: Option<String>,
    /// GitHub PR number this env tracks (`env create --pr`), recorded in the
    /// manifest for review/push-back hints. The base itself is pinned via
    /// `from` like any other revision.
    pub pr: Option<u64>,
    /// The PR's head branch name on its source repo (via `gh`, best-effort).
    pub pr_head_ref: Option<String>,
    /// Where the code comes from. Defaults to a worktree of this repository.
    pub source: BoxSource,
}

impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            from: None,
            profile: None,
            isolation: None,
            image: None,
            backend: "auto".into(),
            audit_capture: sandbox::AuditCapture::Signal,
            parent_branch: None,
            pr: None,
            pr_head_ref: None,
            source: BoxSource::Repo,
        }
    }
}

/// Create an environment: pin the base, build the workspace for the requested
/// source, resolve + persist the policy, record the event.
/// Build a **detached** workspace: a git repository that lives inside the box
/// and shares nothing with the host repository.
///
/// Returns the pinned base commit and tree. The box's branch is created inside
/// its own repository, so every later operation (`run`, the mediated commit,
/// `diff`, `export`) works exactly as it does for a worktree box — those all
/// open `$WORK` directly.
///
/// Cloning runs the host's `git` against a URL the operator supplied, which is
/// the one moment host-side code touches remote content. Two guards, both
/// deliberate:
///
/// * `core.hooksPath` is pointed at an empty directory for the clone, so a
///   repository cannot ship a hook that runs on the host.
/// * The clone is shallow by default. It is a starting tree, not an archive,
///   and less history is less to parse.
fn init_detached_workspace(
    work: &Path,
    source: &BoxSource,
    branch_short: &str,
) -> Result<(git2::Oid, git2::Oid), H5iError> {
    match source {
        BoxSource::Repo => unreachable!("callers gate on is_detached()"),
        BoxSource::New => {
            let repo = Repository::init(work)?;
            let sig = crate::refstore::signature(&repo)?;
            // An empty root commit is the pinned base: a box that starts from
            // nothing still has an immutable point to diff against, so `export`
            // produces "everything the agent wrote" rather than nothing.
            let tree_oid = {
                let builder = repo.treebuilder(None)?;
                builder.write()?
            };
            let tree = repo.find_tree(tree_oid)?;
            let oid = repo.commit(Some("HEAD"), &sig, &sig, "h5i: empty box", &tree, &[])?;
            repo.branch(branch_short, &repo.find_commit(oid)?, false)?;
            repo.set_head(&format!("refs/heads/{branch_short}"))?;
            Ok((oid, tree_oid))
        }
        BoxSource::Clone { url } => {
            let hooks = work
                .parent()
                .unwrap_or(work)
                .join("clone-hooks-disabled");
            std::fs::create_dir_all(&hooks).map_err(|e| H5iError::with_path(e, &hooks))?;
            let out = std::process::Command::new("git")
                .arg("-c")
                .arg(format!("core.hooksPath={}", hooks.display()))
                .args(["clone", "--depth", "1", url])
                .arg(work)
                .output()
                .map_err(|e| H5iError::Metadata(format!("failed to invoke git clone: {e}")))?;
            if !out.status.success() {
                return Err(H5iError::Metadata(format!(
                    "cannot clone '{url}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let repo = Repository::open(work)?;
            let head = repo.head()?.peel_to_commit()?;
            let tree = head.tree()?.id();
            let oid = head.id();
            repo.branch(branch_short, &head, false)?;
            repo.set_head(&format!("refs/heads/{branch_short}"))?;
            // A shallow clone keeps its origin remote, which is a live network
            // handle inside the box. Drop it: the box reaches the network only
            // through the policy's egress allowlist, never through an inherited
            // remote the operator never saw.
            let _ = repo.remote_delete("origin");
            Ok((oid, tree))
        }
    }
}

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
            "branch {branch_full} already exists — `h5i box gc` keeps applied/aborted env \
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
                    _ => sandbox::effective_auto(workdir, agent_profile, false, opts.image.as_deref())?,
                };
                let mut prof = sandbox::load_profile(workdir, agent_profile, Some(claim))?;
                if let Some(img) = &opts.image {
                    prof.image = Some(img.clone());
                }
                let pol = sandbox::resolve(&prof, &sandbox::probe_host_for(claim))?;
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
            sandbox::effective_auto(workdir, profile_name, true, opts.image.as_deref())?
        }
        None => sandbox::effective_auto(workdir, profile_name, false, opts.image.as_deref())?,
    };

    // A browser box with no browser is a box whose first `agent-browser open`
    // fails with a confusing "not found". Refuse at create, where the message
    // can say what to install.
    if profile_name == "browser" && claim < sandbox::IsolationClaim::Container {
        // Kernel tiers reach the host filesystem, so the browser has to be
        // there. A container box brings its own in the image, and is checked by
        // the image existing at all.
        let (chrome, driver) = sandbox::browser_tooling_present();
        if !chrome || !driver {
            let mut missing = Vec::new();
            if !chrome {
                missing.push("a Chrome/Chromium build");
            }
            if !driver {
                missing.push("the `agent-browser` binary");
            }
            return Err(H5iError::Metadata(format!(
                "the `browser` profile needs {} on this host, and it is not there.\n  \
                 Install both with:  npm install -g agent-browser && agent-browser install\n  \
                 (or `cargo install agent-browser`), then create the box again.",
                missing.join(" and ")
            )));
        }
    }

    // Policy first (fail closed BEFORE any state is created on disk).
    let mut profile = sandbox::load_profile(workdir, profile_name, Some(claim))?;
    // `--image` has the strongest precedence; it lands in the profile before
    // resolve, so it is pinned in policy.resolved.toml and the digest like any
    // profile-declared image.
    if let Some(img) = &opts.image {
        profile.image = Some(img.clone());
    }
    let caps = sandbox::probe_host_for(claim);
    let mut policy = sandbox::resolve(&profile, &caps)?;
    policy.audit.capture = opts.audit_capture;
    // Functionally verify the confinement can actually run a command — capability
    // bits can be present while a hardened kernel still denies exec under the
    // full stack. Refuse here with a clear message rather than letting every
    // later `env run` fail on EACCES.
    sandbox::verify_exec(&policy)?;
    let policy_digest = policy.digest()?;

    let work_path = dir.join(WORK_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;

    // Where the code comes from decides the shape of the workspace.
    //
    // `repo`: a native git worktree of THIS repository, sharing its object
    // store, so the box can later be applied back onto a branch.
    //
    // `clone` / `new`: **detached**. The box gets a repository of its own
    // inside its directory; the host repository is neither read nor written
    // after this point, and `export` is the only way out. This is the shape
    // external code should always arrive in.
    let (base_commit_id, base_tree_id, parent_branch) = if opts.source.is_detached() {
        let (oid, tree) = init_detached_workspace(&work_path, &opts.source, &branch_short)?;
        (oid, tree, branch_short.clone())
    } else {
        let rev = opts.from.as_deref().unwrap_or("HEAD");
        let base_commit = repo
            .revparse_single(rev)
            .and_then(|o| o.peel_to_commit())
            .map_err(|e| H5iError::Metadata(format!("cannot resolve base revision '{rev}': {e}")))?;
        let base_tree = base_commit.tree()?.id();
        let parent_branch = opts.parent_branch.clone().unwrap_or_else(|| {
            repo.head()
                .ok()
                .and_then(|h| h.shorthand().map(str::to_owned))
                .unwrap_or_else(|| base_commit.id().to_string())
        });

        repo.branch(&branch_short, &base_commit, false)?;
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
            // `h5i box gc` is the only thing that unlocks+prunes it (and only
            // when applied/aborted).
            let _ = wt.lock(Some(&format!("h5i env {id} live")));
        }
        (base_commit.id(), base_tree, parent_branch)
    };

    // Pin service declarations from the base worktree into an env-local,
    // box-immutable manifest, recording its digest (review #1). Always Some for
    // new envs (even pinned-empty), so the legacy fallback below never applies.
    let service_digest = Some(pin_services_at_create(&work_path, &dir)?);

    // Bake the profile's persona sources into a single PERSONA.md at the
    // worktree root (the agent loads it via `@PERSONA.md`). Git-excluded so it
    // never enters the agent's diff/commit. Fail-closed: a missing source aborts
    // create rather than launching an agent with a silently-empty persona.
    let persona_digest = materialize_persona(&work_path, &profile.persona)?;

    // The viewer token, minted before anything inside the box has run. Minting
    // it lazily on the first `h5i box view` would mean minting it after an agent
    // had already had the run of the box; minting it here means the credential
    // for watching a box predates the box's first instruction. It lives in the
    // env directory, outside every path the box can write or read.
    crate::view::ensure_token(&dir)?;

    let manifest = EnvManifest {
        id: id.clone(),
        agent: agent.to_string(),
        slug: slug.to_string(),
        base_commit: base_commit_id.to_string(),
        base_tree: base_tree_id.to_string(),
        parent_branch,
        branch: branch_full,
        source: opts.source.as_manifest_str(),
        profile: profile.name.clone(),
        policy_digest: policy_digest.clone(),
        isolation_claim: policy.claim.as_str().to_string(),
        backend: backend.to_string(),
        created_at: now_ts(),
        updated_at: now_ts(),
        status: ST_CREATED.to_string(),
        captures: Vec::new(),
        service_digest,
        persona_digest,
        pr: opts.pr,
        pr_head_ref: opts.pr_head_ref.clone(),
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

/// Bake the profile's `persona = [...]` sources into a single `PERSONA.md` at
/// the worktree root. Sources ride in the repo at the pinned base, so they are
/// present in the freshly checked-out worktree; their contents are concatenated
/// in declared order, each under an HTML-comment header naming the source. The
/// file is then git-excluded (so it never appears in `env diff`/propose/commit,
/// even when `h5i init` did not add it to a tracked `.gitignore`). Returns the
/// sha256 of the written `PERSONA.md` for provenance, or `None` when the profile
/// declares no persona. Paths are validated (relative, no `..`) at policy load.
fn materialize_persona(work: &Path, persona: &[String]) -> Result<Option<String>, H5iError> {
    if persona.is_empty() {
        return Ok(None);
    }
    let mut body = String::new();
    for src in persona {
        let path = work.join(src);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            H5iError::Metadata(format!(
                "persona source '{src}' is not in the worktree ({}): {e} — commit it at the \
                 base revision or fix `persona` in .h5i/env.toml (fail-closed)",
                path.display()
            ))
        })?;
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("<!-- persona: {src} -->\n"));
        body.push_str(text.trim_end());
        body.push('\n');
    }
    let persona_md = work.join(PERSONA_FILE);
    std::fs::write(&persona_md, &body).map_err(|e| H5iError::with_path(e, &persona_md))?;
    exclude_in_worktree(work, PERSONA_FILE)?;
    Ok(Some(crate::refstore::sha256_hex(body.as_bytes())))
}

/// Idempotently add `pattern` to the worktree's git exclude file so a
/// machine-managed, untracked file (e.g. `PERSONA.md`) never shows as dirty.
/// Writes to the **common** `info/exclude` (what git actually consults for
/// excludes — shared across worktrees), so it holds even when the base commit's
/// tracked `.gitignore` predates the file.
fn exclude_in_worktree(work: &Path, pattern: &str) -> Result<(), H5iError> {
    let wt_repo = Repository::open(work)?;
    let info = wt_repo.commondir().join("info");
    std::fs::create_dir_all(&info).map_err(|e| H5iError::with_path(e, &info))?;
    let exclude = info.join("exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let line = format!("/{pattern}");
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&format!("{line}\n"));
    std::fs::write(&exclude, next).map_err(|e| H5iError::with_path(e, &exclude))?;
    Ok(())
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
    pub receipt: crate::receipt::ExecRecord,
}

/// Whether this env's workspace is materialized locally. A `false` means the
/// env was created on another clone and pulled here (no `work/`), or gc'd —
/// such an env supports review/apply (which operate on the pushed code branch)
/// but not run/propose/rebase (which need the worktree).
pub fn has_workspace(m: &EnvManifest, h5i_root: &Path) -> bool {
    m.work_dir(h5i_root).is_dir()
}

/// A detached box has no parent branch in this repository to land on, by
/// design: its code was copied in from somewhere else and the host repository
/// was never touched. `export` is its exit.
fn detached_err(m: &EnvManifest, op: &str) -> H5iError {
    H5iError::Metadata(format!(
        "{}: `{op}` needs a parent branch in this repository, and this box is detached \
         (source: {}). Use `h5i box export {}` and apply the patch wherever you want it.",
        m.id, m.source, m.slug
    ))
}

/// Is this box detached (its git repository lives inside the box)?
pub fn is_detached(m: &EnvManifest) -> bool {
    m.source != "repo"
}

/// A uniform error for operations that need a local worktree the env lacks.
fn no_workspace_err(m: &EnvManifest, op: &str) -> H5iError {
    H5iError::Metadata(format!(
        "{}: no local workspace for `{op}` — this environment lives on another clone (or was \
         gc'd). You can review it (`h5i box diff/status/inspect {}`) and `h5i box apply {}` \
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
    readonly: bool,
) -> Result<(), H5iError> {
    match policy.claim {
        IsolationClaim::Process | IsolationClaim::Supervised => {
            for p in box_git_plumbing(repo, m)? {
                let path = p.host.display().to_string();
                // A read-only observer session grants the whole in-box git
                // surface read-only — `git log`/`status`/`diff` still work, but
                // the box can write neither the worktree nor its refs/objects.
                if p.rw && !readonly {
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
        claim if claim.image_backed() => {
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
            // A bind-mounting runtime errors on a missing source (unlike
            // Landlock, which skips) — keep only what exists on the host.
            mounts.retain(|b| b.host.exists());
            policy.box_git = mounts;
        }
        _ => {}
    }
    Ok(())
}

fn prepare_cargo_env(
    work: &Path,
    policy: &ResolvedPolicy,
) -> Result<Vec<(String, String)>, H5iError> {
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

/// The character an image-backed tier's mount **spec string** reserves, and
/// which a host path therefore cannot contain: `,` for Podman's
/// `type=bind,source=…,target=…`, `:` for microsandbox's `SOURCE:DEST:OPTIONS`.
/// `None` for the kernel tiers, which pass paths as arguments rather than as
/// fields of a delimited string and so have no such hazard.
///
/// Callers use this to refuse a path they could not mount, instead of emitting a
/// spec that would mount something else.
fn mount_spec_separator(claim: IsolationClaim) -> Option<char> {
    match claim {
        IsolationClaim::Container => Some(','),
        IsolationClaim::Microvm => Some(':'),
        _ => None,
    }
}

/// Materialize per-env private paths (Idea 3): give each declared path its own
/// backing dir under the env's `private/` tree so concurrent envs of the same
/// repo never collide on inode-level locks / single-writer build caches. Wipes
/// non-persistent backings first, then records each `(backing → workspace-rel)`
/// pair on `policy.private_binds` (applied as bind mounts on the kernel tiers
/// and `--mount`s on container). At the kernel tiers it also Landlock-grants the
/// backing dir so access through the bind is allowed regardless of mount
/// topology. A no-op at the workspace tier (no mount namespace to bind in — the
/// shared worktree is the documented trade-off). Fail-closed on I/O errors.
fn prepare_private_paths(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
    work: &Path,
) -> Result<(), H5iError> {
    if policy.profile.private_paths.is_empty() || policy.claim < IsolationClaim::Process {
        return Ok(());
    }
    let private_root = m.dir(h5i_root).join("private");
    let kernel = matches!(
        policy.claim,
        IsolationClaim::Process | IsolationClaim::Supervised
    );
    for pp in policy.profile.private_paths.clone() {
        let rel = pp.path.trim_matches('/').to_string();
        // Backing dirs nest under private/ exactly as the rel path does — the
        // overlap lint guarantees distinct, non-shadowing subtrees.
        let backing = private_root.join(&rel);
        if !pp.persist {
            let _ = std::fs::remove_dir_all(&backing);
        }
        std::fs::create_dir_all(&backing).map_err(|e| H5iError::with_path(e, &backing))?;
        // The mountpoint must exist inside the worktree.
        let target = work.join(&rel);
        std::fs::create_dir_all(&target).map_err(|e| H5iError::with_path(e, &target))?;
        // The image-backed tiers carry the backing dir as a mount *spec string*,
        // and each runtime's syntax reserves a separator its paths cannot
        // contain: Podman's `--mount` splits on ',', microsandbox's
        // `SOURCE:DEST` on ':'. Fail closed if the env's host path holds one,
        // rather than silently dropping the (policy-required) isolation.
        if let Some(sep) = mount_spec_separator(policy.claim) {
            if backing.display().to_string().contains(sep) {
                return Err(H5iError::Metadata(format!(
                    "private_paths '{rel}': the env's backing path '{}' contains a '{sep}' which \
                     the {} tier's mount syntax cannot carry — move the repo out of that path \
                     (fail-closed)",
                    backing.display(),
                    policy.claim.as_str()
                )));
            }
        }
        if kernel {
            policy.profile.fs_write.push(backing.display().to_string());
        }
        policy
            .private_binds
            .push(sandbox::PrivateBind { backing, rel });
    }
    Ok(())
}

/// Longest `TMPDIR` that still leaves room for what a box builds underneath it.
///
/// macOS caps an `AF_UNIX` path at 104 bytes. The deepest thing h5i knows a box
/// puts under `TMPDIR` is Chrome's process-singleton socket, reached through
/// agent-browser's ephemeral profile directory:
///
/// ```text
///   <TMPDIR>/agent-browser-chrome-<uuid>/SingletonSocket
///            \__________ 58 __________/\_____ 16 _____/
/// ```
///
/// so a `TMPDIR` longer than this cannot host one. Not a hard guarantee for
/// every program — it is the budget h5i sizes its own scratch path against.
#[cfg(target_os = "macos")]
const TMPDIR_BUDGET: usize = 104 - 58 - 16;

/// Where a box's private `/tmp` is backed on disk.
///
/// Linux keeps it inside the env directory. The tier bind-mounts that directory
/// over `/tmp` in a private mount namespace, so the box sees the short literal
/// path and the backing's own depth is invisible to it.
///
/// macOS has no unprivileged bind mount, so `seatbelt::plan` re-expresses the
/// redirect as `TMPDIR` pointing at the backing — which means the backing's
/// length *is* what programs inside the box build their paths from. Nested in
/// the repository it never fits `TMPDIR_BUDGET`: `/.git/.h5i/env/<agent>/<slug>/tmp`
/// alone spends 26 of the ~30 bytes available before the repository path is
/// counted at all, so a browser box failed on every Mac with Chrome reporting
/// "Failed to create socket directory" — which reads as a permission error and
/// is really `AF_UNIX path too long`.
///
/// So on macOS the backing moves to a short path outside the repository, named
/// by digest so it stays stable for a given env (and distinct per read-only
/// observer, which passes its own logical path). Isolation is unchanged: it
/// comes from the directory being per-env, `0700`, and the only `/tmp` write
/// grant the policy carries — not from where it sits.
fn private_tmp_backing(logical: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let digest = crate::refstore::sha256_hex(logical.display().to_string().as_bytes());
        let short = PathBuf::from(format!("/tmp/h5i-{}", &digest[..12]));
        debug_assert!(short.display().to_string().len() <= TMPDIR_BUDGET);
        short
    }
    #[cfg(not(target_os = "macos"))]
    logical.to_path_buf()
}

/// Clear (or create) the box's private `/tmp` backing, `0700` and ours.
///
/// Two different failures were being conflated. The security question — has
/// another local user squatted this name in world-writable `/tmp`? — is settled
/// by looking at who owns the directory, so it is asked directly here. What was
/// asked instead was "did an exclusive create succeed", and that also fails for
/// an entirely ordinary reason: `remove_dir_all` races anything still writing
/// into the directory (a browser left running by an earlier box run keeps
/// recreating files under it), returns `ENOTEMPTY`, and the error was swallowed
/// by `let _ =`. The create then hit `EEXIST` and blamed another user for a
/// directory the caller owns — an unusable box with a misleading reason.
fn reset_private_tmp(backing: &Path) -> Result<(), H5iError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        // `symlink_metadata`: a symlink planted here must never be followed.
        if let Ok(md) = std::fs::symlink_metadata(backing) {
            let ours = md.is_dir() && !md.file_type().is_symlink() && md.uid() == unsafe { libc::getuid() };
            if !ours {
                return Err(H5iError::Metadata(format!(
                    "{} exists and is not a directory this user owns — refusing to use it as a \
                     box's private /tmp (fail-closed). Remove it and retry.",
                    backing.display()
                )));
            }
            // Ours: clear it. A failure here is not a security condition, so it
            // reports what it is rather than accusing anyone.
            if let Err(e) = std::fs::remove_dir_all(backing) {
                return Err(H5iError::Metadata(format!(
                    "could not clear this box's private /tmp at {} ({e}). Something is probably \
                     still writing to it — a browser or daemon left running by an earlier run of \
                     this box. Stop it, or remove the directory, and retry.",
                    backing.display()
                )));
            }
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(backing)
            .map_err(|e| H5iError::with_path(e, backing))?;
        // The mode is set at creation; re-assert it in case a permissive umask
        // or an intervening process widened it.
        std::fs::set_permissions(backing, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| H5iError::with_path(e, backing))?;
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::remove_dir_all(backing);
        std::fs::create_dir_all(backing).map_err(|e| H5iError::with_path(e, backing))?;
    }
    Ok(())
}

/// Give kernel-tier envs a private `/tmp` by binding an env-owned scratch dir
/// over the host path before Landlock is applied. Agent profiles used to grant
/// host-shared `/tmp` at process/supervised tiers; that creates an unnecessary
/// cross-agent rendezvous point. This replaces any real `/tmp` grant with the
/// backing dir, then reuses the absolute bind machinery to make `/tmp` resolve
/// to that backing inside the box.
fn prepare_private_tmp(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
    // `Some(dir)` overrides the `/tmp` backing (a read-only observer uses a
    // per-session `<env>/ro/<pid>/tmp` so concurrent observers don't share one
    // scratch dir). `None` → the persistent per-env `<env>/tmp`.
    backing_override: Option<&Path>,
) -> Result<(), H5iError> {
    if !matches!(
        policy.claim,
        IsolationClaim::Process | IsolationClaim::Supervised
    ) {
        return Ok(());
    }
    let had_tmp = policy.profile.fs_read.iter().any(|p| p == "/tmp")
        || policy.profile.fs_write.iter().any(|p| p == "/tmp");
    if !had_tmp {
        return Ok(());
    }
    let logical = match backing_override {
        Some(dir) => dir.to_path_buf(),
        None => m.dir(h5i_root).join("tmp"),
    };
    let backing = private_tmp_backing(&logical);
    if let Some(parent) = backing.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    reset_private_tmp(&backing)?;
    policy.profile.fs_read.retain(|p| p != "/tmp");
    policy.profile.fs_write.retain(|p| p != "/tmp");
    policy.profile.fs_write.push(backing.display().to_string());
    policy.home_binds.push(sandbox::HomeBind {
        backing,
        target: PathBuf::from("/tmp"),
    });
    Ok(())
}

/// Top-level entries pruned from the per-env HOME seed ([`seed_home_copy`]).
/// These are large, non-credential session/history/cache trees a fresh isolated
/// box does not need — e.g. Claude/Codex transcript stores, logs, and temporary
/// plugin caches. Skipping them copies **less** host data into the box (strictly
/// more private) while the copy-in/persist isolation invariant is untouched: the
/// box still gets its own writable copy of credentials/settings, the real HOME
/// is still only ever read. The default is *copy* — only these known-bloat names
/// are pruned — so any new credential file the runtime adds is seeded
/// automatically rather than silently dropped. Matched by exact name at the seed
/// root only.
const HOME_SEED_SKIP: &[&str] = &[
    "projects",        // Claude Code conversation transcripts (the bulk of the size)
    "todos",           // per-session todo lists
    "statsig",         // feature-flag / gate cache
    "shell-snapshots", // captured shell-env snapshots
    "shell_snapshots", // Codex captured shell-env snapshots
    "file-history",    // edit-history backups
    "history.jsonl",   // REPL command history
    "sessions",        // Codex conversation transcripts
    "log",             // Codex host logs
    "logs_2.sqlite",   // Codex host log database
    "logs_2.sqlite-shm",
    "logs_2.sqlite-wal",
    ".tmp", // Codex plugin/app temp cache
    "tmp",  // Codex temp cache
];

/// Entries the HOME seed **must not** carry into a box, by shape rather than by
/// size.
///
/// The seed exists so each box gets its own copy of the agent's session state
/// instead of racing the real files. It is not a reason to hand a box every
/// credential the runtime happens to keep next to that state. These are dropped
/// even though the runtime wrote them under its own directory: a box that needs
/// to authenticate does it through the host-side proxy, which never lets the
/// credential into the box at all (roadmap 5.5).
///
/// Deliberately *not* dropped: the runtime's own API token, where the runtime
/// cannot function without it and the profile already scopes egress to that
/// runtime's API. That trade is stated in `Profile::builtin_agent`.
const HOME_SEED_CREDENTIAL_SKIP: &[&str] = &[
    // Cloud and VCS credentials that tools drop into an agent's config dir.
    "credentials",
    "credentials.json",
    "credentials.toml",
    ".netrc",
    "netrc",
    // SSH material, wherever it turns up.
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "known_hosts",
    // Generic secret stores.
    ".env",
    "secrets.json",
    "secrets.toml",
    "keyring",
    "gh_token",
    "github_token",
];

/// Is `name` credential-shaped, and therefore not something to seed?
///
/// Matches the exact names above plus anything ending in a private-key
/// extension, so a key the runtime named after its host is still caught.
fn is_credential_shaped(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HOME_SEED_CREDENTIAL_SKIP.iter().any(|s| *s == lower)
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with("_rsa")
        || lower.ends_with("_ed25519")
}

/// Seed a per-env HOME copy from the real HOME, pruning the known-large,
/// non-credential top-level entries in [`HOME_SEED_SKIP`] and every
/// credential-shaped entry ([`is_credential_shaped`]). A single file (e.g.
/// `~/.claude.json`) is copied whole; a directory (e.g. `~/.claude`) is copied
/// entry-by-entry so the skip set can drop its immediate children before the
/// expensive recursion. Everything not skipped is copied via [`copy_tree`]
/// (modes preserved, symlinks skipped). Fail-closed on I/O.
fn seed_home_copy(src: &Path, dst: &Path) -> Result<(), H5iError> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| H5iError::with_path(e, src))?;
    if !meta.file_type().is_dir() {
        return copy_tree(src, dst);
    }
    std::fs::create_dir_all(dst).map_err(|e| H5iError::with_path(e, dst))?;
    for entry in std::fs::read_dir(src).map_err(|e| H5iError::with_path(e, src))? {
        let entry = entry.map_err(|e| H5iError::with_path(e, src))?;
        let name = entry.file_name();
        if HOME_SEED_SKIP
            .iter()
            .any(|s| std::ffi::OsStr::new(s) == name)
        {
            continue;
        }
        // Credential-shaped entries are dropped wherever they appear in the
        // seed, not just at the top level: the box authenticates through the
        // host-side proxy, so it has no use for a key it can read.
        if is_credential_shaped(&name.to_string_lossy()) {
            continue;
        }
        copy_tree(&entry.path(), &dst.join(&name))?;
    }
    Ok(())
}

/// Recursively copy a regular file or directory tree, preserving file modes
/// (`std::fs::copy` carries permissions — important for a `0600`
/// `.credentials.json`). Symlinks are skipped (a credential store is regular
/// files; we never follow a link out of the source tree). Fail-closed on I/O.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), H5iError> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| H5iError::with_path(e, src))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Ok(());
    }
    if ft.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| H5iError::with_path(e, dst))?;
        for entry in std::fs::read_dir(src).map_err(|e| H5iError::with_path(e, src))? {
            let entry = entry.map_err(|e| H5iError::with_path(e, src))?;
            let name = entry.file_name();
            // Deep in the tree too: a key under `~/.claude/plugins/…` is still
            // a key.
            if is_credential_shaped(&name.to_string_lossy()) {
                continue;
            }
            copy_tree(&src.join(&name), &dst.join(&name))?;
        }
    } else if ft.is_file() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        std::fs::copy(src, dst).map_err(|e| H5iError::with_path(e, dst))?;
    }
    Ok(())
}

/// Per-env credential/session isolation (#1). The built-in agent profiles grant
/// the box rw to the *real* `~/.claude`/`~/.claude.json` (Claude) or `~/.codex`
/// (Codex), so two concurrent agent boxes of the same runtime race on those
/// shared files — corrupting `~/.claude.json` session history, fighting over a
/// refreshed token. This redirects each such grant to a per-env *copy*: seed it
/// once from the real HOME (copy-in), persist it across this env's runs, grant the
/// copy rw, and bind it over the real absolute path inside the box's mount
/// namespace (`sandbox::build_confined_command`). The real HOME is only ever READ
/// (to seed) — never written — so an env can never clobber it (the chosen
/// reconciliation: copy-in only, persist per-env).
///
/// Kernel tiers only: the container backend's read-only rootfs never mounts host
/// HOME, so there is no shared inode to race there. A no-op at the workspace tier
/// (no mount namespace to bind in) and for non-agent profiles. A state path that
/// does not exist on the host is left as today's direct grant — we never create it
/// in the real HOME merely to have a mountpoint to bind over, so the common
/// logged-in case is fully isolated and the rare fresh-user case is no worse than
/// before. Fail-closed on I/O.
fn prepare_home_state(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
    home: Option<&Path>,
    // `Some(dir)` overrides the backing root (a read-only observer session uses
    // a per-session ephemeral `<env>/ro/<pid>/home` so concurrent observers
    // never race on the persistent per-env copy). `None` → the persistent
    // `<env>/home` used by read-write runs.
    home_root_override: Option<&Path>,
) -> Result<(), H5iError> {
    if !matches!(
        policy.claim,
        IsolationClaim::Process | IsolationClaim::Supervised
    ) {
        return Ok(());
    }
    let Some(runtime) = sandbox::AgentRuntime::from_profile_name(&policy.profile.name) else {
        return Ok(());
    };
    let Some(home) = home else {
        return Ok(());
    };
    let home_root = match home_root_override {
        Some(dir) => dir.to_path_buf(),
        None => m.dir(h5i_root).join("home"),
    };

    for state in runtime.state_write() {
        // Each grant is a `~/…` HOME path (`~/.claude`, `~/.claude.json`, `~/.codex`).
        let Some(rel) = state.strip_prefix("~/") else {
            continue;
        };
        let real = home.join(rel);
        // Only redirect paths that already exist: we never touch the real HOME, so
        // a missing one has no inode to bind over and keeps today's direct grant.
        if !real.exists() {
            continue;
        }
        // Backing copy keyed by the leaf path so `.claude` and `.claude.json` stay
        // distinct (`<env>/home/.claude`, `<env>/home/.claude.json`).
        let backing = home_root.join(rel);
        // Seed once (copy-in) and persist: only when absent, so a token refreshed
        // by a prior run of THIS env survives into the next. The seed prunes the
        // large non-credential trees (`~/.claude/projects`, caches — see
        // HOME_SEED_SKIP) so the first `env shell` doesn't copy hundreds of MB of
        // transcript history just to start.
        if !backing.exists() {
            seed_home_copy(&real, &backing)?;
        }
        // Drop the real-HOME grant, grant the per-env copy instead (defence in
        // depth: even if the bind were bypassed the box can't reach the real file).
        policy.profile.fs_write.retain(|w| w.as_str() != *state);
        policy.profile.fs_write.push(backing.display().to_string());
        policy.home_binds.push(sandbox::HomeBind {
            backing,
            target: real,
        });
    }
    Ok(())
}

/// The host-owned per-env inbound mailbox. Lives at `<env>/inbox/` and is
/// exposed to the box READ-ONLY (a Landlock read-grant on the kernel tiers, a
/// read-only bind mount on container). The host writes cross-agent messages
/// here at send time ([`fan_out_to_env_inbox`]); the box reads them but cannot
/// write — so a confined agent receives messages without any write access to
/// the shared coordination store (which stays sealed). Returns the env vars to
/// inject (`H5I_ENV_INBOX` → the in-box path).
fn prepare_env_inbox(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
) -> Result<Vec<(String, String)>, H5iError> {
    if policy.claim < IsolationClaim::Process {
        return Ok(Vec::new());
    }
    let inbox = env_inbox_dir(h5i_root, m);
    std::fs::create_dir_all(&inbox).map_err(|e| H5iError::with_path(e, &inbox))?;
    let inside = match policy.claim {
        claim if claim.image_backed() => {
            policy.env_inbox = Some(inbox);
            BOX_INBOX_MOUNT.to_string()
        }
        IsolationClaim::Process | IsolationClaim::Supervised => {
            // Read-only: the box may read its inbox, never write it.
            policy.profile.fs_read.push(inbox.display().to_string());
            inbox.display().to_string()
        }
        _ => return Ok(Vec::new()),
    };
    Ok(vec![(H5I_ENV_INBOX_VAR.to_string(), inside)])
}

/// Host path of an env's inbound mailbox dir (`<env>/inbox/`).
pub fn env_inbox_dir(h5i_root: &Path, m: &EnvManifest) -> PathBuf {
    m.dir(h5i_root).join(ENV_INBOX_DIR)
}

/// Box-writable "seen" cursor for the inbox, stored in the capture spool (the
/// inbox itself is read-only, so read-state can't live there). Ignored by the
/// spool ingest, whose record names use different prefixes.
pub fn read_inbox_cursor(spool: &Path) -> std::collections::BTreeSet<String> {
    let path = spool.join("team-inbox-seen.json");
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Persist the inbox "seen" cursor (best-effort; box-writable spool path).
pub fn write_inbox_cursor(
    spool: &Path,
    seen: &std::collections::BTreeSet<String>,
) -> Result<(), H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let path = spool.join("team-inbox-seen.json");
    let bytes = serde_json::to_vec(seen)?;
    std::fs::write(&path, bytes).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

/// Stage the box's write window for observation records.
///
/// This is the receipt-integrity boundary: the box is granted `<env>/spool`
/// and nothing else under the env directory. The receipt log itself
/// (`<env>/receipt.jsonl`) and the stored payloads (`<env>/receipts/`) sit one
/// level up, outside every grant, so a box can stage a *new* record but can
/// never rewrite one the host has already recorded. Host-side ingest is what
/// moves a staged record into the log, and it stamps the lane (`tee-shim`,
/// `inbox-capture`) so box-claimed evidence stays distinguishable from
/// host-observed evidence forever.
/// Offer this project's warm dependency caches to the box, read-only.
///
/// Only caches whose key matches the project's current lockfiles are offered
/// (`cache::mounts_for`), and the bind is read-only on every tier, so a cache
/// is never a mutable surface shared between boxes. `$HOME` inside the box is
/// the operator's home path on the kernel tiers, so `~`-relative targets are
/// expanded here rather than inside the box.
fn prepare_cache_mounts(h5i_root: &Path, workdir: &Path, policy: &mut ResolvedPolicy) {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    for (host, target) in crate::cache::mounts_for(h5i_root, workdir) {
        let target = match target.strip_prefix("~/") {
            Some(rest) => match &home {
                Some(h) => PathBuf::from(h).join(rest),
                // No HOME to anchor a `~` path against: skip rather than guess.
                None => continue,
            },
            None => PathBuf::from(&target),
        };
        // Landlock still governs reads, so the backing path needs a read grant
        // as well as the bind; without it the box would see an unreadable mount.
        policy.profile.fs_read.push(host.display().to_string());
        policy.ro_binds.push(sandbox::RoBind {
            backing: host,
            target,
        });
    }
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
        claim if claim.image_backed() => {
            policy.env_capture_spool = Some(spool);
            BOX_CAPTURE_SPOOL.to_string()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexHookTraceEvent {
    pub kind: String,
    pub message: String,
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

/// A staged (not-yet-ingested) in-box capture, read back from the spool by id.
pub struct StagedCapture {
    pub raw: Vec<u8>,
    pub meta: Option<InboxCaptureMeta>,
}

/// Read a staged in-box capture (`cap-<id>`) from a capture spool dir by the id
/// `h5i capture run` printed — before the host ingests it into refs/h5i/objects.
/// Pure (takes the spool path) so it's unit-testable. Returns None when the id
/// isn't a safe staged-capture id or the `.raw` file is gone (already ingested).
pub fn read_staged_capture_at(spool: &Path, id: &str) -> Option<StagedCapture> {
    // Defensive: the id becomes a filename, so reject anything but a `cap-…`
    // base of the alnum/`-` charset `write_inbox_capture_spool` produces.
    if !id.starts_with("cap-")
        || id.len() > 96
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let raw = std::fs::read(spool.join(format!("{id}.raw"))).ok()?;
    let meta = std::fs::read(spool.join(format!("{id}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    Some(StagedCapture { raw, meta })
}

/// Read a staged in-box capture by id, locating the spool from the env the host
/// injects (`$H5I_ENV_CAPTURE_SPOOL`). Returns None when not running in a box.
/// Lets an agent rehydrate the full raw output of a capture it just produced —
/// the host hasn't ingested it into refs/h5i/objects yet, so `resolve_manifest`
/// can't see it.
pub fn read_staged_capture(id: &str) -> Option<StagedCapture> {
    let spool = std::env::var_os(H5I_ENV_CAPTURE_SPOOL_VAR).map(PathBuf::from)?;
    read_staged_capture_at(&spool, id)
}

/// Pending-context filename inside the env capture spool. Distinct from the
/// `cmd-*`/`cap-*`/`codex-hook-*`/`note-*`/`ctxsnap-*` records the spool ingest
/// drains, so [`ingest_shell_spool`] leaves it alone.
const SPOOL_PENDING_CONTEXT: &str = "pending_context.json";

/// Is this process running **inside an env box**? True only when all three
/// host-injected markers are present (`H5I_ENV_ID` + `H5I_ENV_POLICY_DIGEST` +
/// `H5I_ENV_CAPTURE_SPOOL`) — the same trio that gates every other in-box
/// redirect, so a single stray var never flips a host process into box mode.
///
/// In-box, the `.git/.h5i` sidecar is sealed (kernel tiers: no write grant;
/// container: not mounted — the path is a bare read-only overlay dir), so any
/// code that would *initialize or repair* the host store must skip that work
/// in a box: the layout already exists host-side, and the box's own writes go
/// through the spool/inbox mounts instead.
pub fn in_env_box() -> bool {
    std::env::var_os(H5I_ENV_ID_VAR).is_some()
        && std::env::var_os(H5I_ENV_POLICY_DIGEST_VAR).is_some()
        && std::env::var_os(H5I_ENV_CAPTURE_SPOOL_VAR).is_some()
}

/// The pending-context file path **when running inside an env box**, or `None`
/// on the host. Inside a box the `.git/.h5i` sidecar is sealed (no read/write
/// grant), so the human prompt captured by the `UserPromptSubmit` hook can't
/// land there; it is redirected to the box-writable capture spool the host
/// injects (`$H5I_ENV_CAPTURE_SPOOL`), where the in-box `h5i capture commit`
/// reads it back. Gated on the same trio of vars as the in-box note spool
/// (`H5I_ENV_ID` + `H5I_ENV_POLICY_DIGEST` + `H5I_ENV_CAPTURE_SPOOL`) so a stray
/// spool var alone never diverts host prompt capture.
pub fn inbox_pending_context_path() -> Option<PathBuf> {
    inbox_pending_context_path_from(
        std::env::var_os(H5I_ENV_ID_VAR),
        std::env::var_os(H5I_ENV_POLICY_DIGEST_VAR),
        std::env::var_os(H5I_ENV_CAPTURE_SPOOL_VAR),
    )
}

/// Pure core of [`inbox_pending_context_path`] (env reads factored out so the
/// gating is unit-testable without racing on process-global env vars). All three
/// box markers must be present, else `None`.
fn inbox_pending_context_path_from(
    env_id: Option<std::ffi::OsString>,
    policy_digest: Option<std::ffi::OsString>,
    capture_spool: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if env_id.is_none() || policy_digest.is_none() {
        return None;
    }
    Some(PathBuf::from(capture_spool?).join(SPOOL_PENDING_CONTEXT))
}

/// Where this box's private `/tmp` actually is, as the box will see it.
///
/// The Linux tiers bind-mount a per-env directory over `/tmp` inside a private
/// mount namespace, so the literal path is already the private one. macOS has
/// neither unprivileged bind mounts nor mount namespaces, so `seatbelt::plan`
/// re-expresses that redirect as `TMPDIR` pointing at a per-env backing — and
/// rewrites the `/tmp` write grant to that backing, which leaves the *literal*
/// `/tmp` denied.
///
/// So a hardcoded `/tmp/<subdir>` is right on Linux and wrong on every Mac: the
/// agent-browser daemon died at startup with "Failed to create socket
/// directory: Operation not permitted", which is what a browser box looked like
/// on macOS before this.
fn box_tmp_root(policy: &ResolvedPolicy) -> String {
    if !cfg!(target_os = "macos") {
        return "/tmp".to_string();
    }
    policy
        .home_binds
        .iter()
        .find(|b| b.target == Path::new("/tmp"))
        .map(|b| b.backing.display().to_string())
        .unwrap_or_else(|| "/tmp".to_string())
}

/// Loopback ports this box is allowed to dial: the dynamic host ports of its
/// own **running** services.
///
/// The box's browser has to reach the dev server the box is running — that is
/// the whole point of a browser box — and on macOS loopback is the host's, so
/// it is denied wholesale unless a port is named. These ports were allocated by
/// h5i for this env's services, so naming them grants the box access to itself
/// and to nothing else on the interface.
///
/// Only **live** services count: a record whose process is gone would otherwise
/// keep a port open in the policy that some unrelated host process could later
/// bind. Re-read on every run, so starting a service and then using it works
/// without recreating the box.
fn live_service_ports(h5i_root: &Path, m: &EnvManifest) -> Vec<u16> {
    let svc_dir = services_dir(h5i_root, m);
    let Ok(entries) = std::fs::read_dir(&svc_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(rec) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<ServiceRecord>(&t).ok())
        else {
            continue;
        };
        if let Some(port) = rec.dynamic_port {
            if pid_alive(rec.pid) {
                out.push(port);
            }
        }
    }
    out
}

/// The `agent-browser` shim: launch Chrome ourselves, then attach to it.
///
/// agent-browser's own *launch* path does not work inside a Seatbelt sandbox.
/// The failure is not h5i's policy — it reproduces under `sandbox-exec` with a
/// fully permissive `(allow default)` profile, and disappears entirely when the
/// sandbox is removed — so no grant fixes it. Its *attach* path (`--cdp <port>`)
/// works inside a box today.
///
/// So this shim closes the gap: it makes sure a Chrome is running, and hands
/// agent-browser the port instead of letting it start one. Chrome is launched
/// detached (`setsid`) so it outlives the single `box run` that started it, and
/// its port is remembered in the env's own directory rather than in `TMPDIR`,
/// which h5i wipes at the start of every run.
///
/// Not a security control. It is on `PATH` in a directory the box can write, so
/// the box can replace it or call the real binary directly — the boundary is the
/// tier, exactly as it is for every other command in the box. It exists so the
/// browser works, not to constrain it.
///
/// `--cdp` and `--allowed-domains` are mutually exclusive upstream ("WebRTC
/// containment cannot be installed before existing page scripts run"), so
/// [`browser_env`] stops setting the domain list when the shim is in play. That
/// is a real reduction: agent-browser's in-process domain check is gone. The
/// tier's own egress enforcement — the actual boundary — is untouched.
/// One [`sandbox::chrome_exec_patterns`] entry as a single `sh` word.
///
/// Quoting is per segment, and it has to be: `*` must stay bare so the shell
/// globs the version directory, while `Google Chrome for Testing.app` must be
/// quoted or it becomes four words. A segment needing both is refused by
/// `no_pattern_segment_needs_quoting_and_globbing_at_once` in the sandbox crate.
fn shell_glob_word(pattern: &str) -> String {
    let (prefix, rest) = match pattern.strip_prefix("~/") {
        Some(rest) => ("\"$HOME\"/", rest),
        None => ("", pattern),
    };
    let segments: Vec<String> = rest
        .split('/')
        .map(|seg| {
            if seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '*'))
            {
                seg.to_string()
            } else {
                format!("\"{seg}\"")
            }
        })
        .collect();
    format!("{prefix}{}", segments.join("/"))
}

fn browser_shim_source(agent_browser: &str) -> String {
    // Generated from the sandbox crate's list rather than written out here: the
    // box may only read the paths that list is derived from, and a launcher
    // looking anywhere else is a box that fails after create said it was fine.
    let chrome_candidates: String = sandbox::chrome_exec_patterns()
        .iter()
        .map(|p| format!("    {} \\\n", shell_glob_word(p)))
        .collect();
    format!(
        r##"#!/bin/sh
# Generated by h5i. Launch Chrome, then attach agent-browser to it.
set -e
REAL='{agent_browser}'
STATE="${{H5I_BROWSER_STATE:?h5i did not set H5I_BROWSER_STATE}}"
PORT="${{H5I_BROWSER_CDP_PORT:?h5i did not set H5I_BROWSER_CDP_PORT}}"
UD="$STATE/chrome"

alive() {{
  curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/json/version"
}}

# Put Chrome in its own session so it survives the `box run` that started it:
# h5i `setsid`s the run and `killpg`s that group on exit. There is no `setsid(1)`
# on macOS, so fall back to perl's; if neither works, still launch — Chrome then
# lives only for this one command, which is worse but not broken.
#
# Every arm `exec`s, and that is load-bearing rather than tidiness. This function
# is only ever called backgrounded, so `exec` replaces *that* subshell: the `$!`
# recorded as `chrome.pid` is then Chrome itself. Without it the subshell stays
# alive as a launcher, `$!` names the launcher, and every later `kill` of that
# pid — `h5i box rm`'s stop-the-browser, and the host-side restart below — kills
# the launcher while Chrome, in its own session, carries on. (Called in the
# foreground it would replace the shim; it is not, and must not be.)
#
# One caveat, stated because a wrong pid is now a wrong `kill` rather than a
# harmless no-op: `setsid(1)` execs in place only when it is not already a
# process-group leader. Under a job-control shell (`set -m`) it would be one,
# fork, and `$!` would name a parent that exits at once. This script is
# `#!/bin/sh` and non-interactive, so job control is off and the pid recorded is
# Chrome's own — and `browser_pid` on the host re-checks that before signalling
# anything.
detach() {{
  if command -v setsid >/dev/null 2>&1; then
    exec setsid "$@"
  elif command -v perl >/dev/null 2>&1; then
    exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die "exec: $!"' -- "$@"
  else
    exec "$@"
  fi
}}

find_chrome() {{
  for c in \
{chrome_candidates}  ; do
    [ -x "$c" ] && {{ printf '%s' "$c"; return 0; }}
  done
  return 1
}}

# Where the box's only route out is h5i's host-side allowlist proxy, Chrome has
# to be *told*: it does not read `HTTPS_PROXY` on macOS (it asks the OS for the
# system proxy configuration), so it would open its own socket, be denied by the
# Seatbelt rule, and report `net::ERR_ACCESS_DENIED` — which reads as a page
# problem and is a route problem.
#
# Keyed on `H5I_EGRESS_PROXY`, not on `HTTPS_PROXY`: only a tier that actually
# runs the proxy sets it, where the conventional vars are ordinary shell state
# anything in the box may set for its own reasons. So on the Linux supervised
# tier (nftables, direct connects) nothing is added and Chrome is launched
# exactly as before, and that stays true whatever the box's own environment says.
#
# No bypass list: Chrome always bypasses loopback unless `<-loopback>` is passed,
# so the dev server under test is still reached directly and never through — or
# gated by — the allowlist.
PROXY_ARG=
if [ -n "${{{egress_var}:-}}" ]; then PROXY_ARG="--proxy-server=${egress_var}"; fi

# A running Chrome took its proxy address at launch and will never re-read it,
# so "already alive" is not on its own a reason to leave it alone: a browser
# that predates this route (an upgrade, or a run whose proxy moved) would keep
# failing every navigation until someone thought to restart it.
#
# The box cannot do that itself. Chrome deliberately outlives the run that
# started it, which puts it in a *previous* sandbox instance — Seatbelt's
# `(allow signal (target same-sandbox))` does not reach across that, so `kill`
# here is refused, and `agent-browser close` only ends a session it owns rather
# than the browser process. So this records the mismatch and the **host** acts
# on it, at the start of the next `h5i box run`/`box shell` (see
# `stop_stale_browser`) — which is where the pid is reachable. It says so rather
# than failing later with a proxy error that reads like a page problem.
HAD=$(cat "$STATE/chrome.proxy" 2>/dev/null || printf '')
if alive && [ "$PROXY_ARG" != "$HAD" ]; then
  printf '%s' "$PROXY_ARG" > "$STATE/chrome.restart"
  # Deliberately not "re-run this command": inside an interactive `box shell`
  # that re-enters this same shim in this same run and prints this same warning
  # forever. The restart happens between runs, so the run has to end first.
  echo "h5i: this box's browser predates its current route out and cannot reach the" >&2
  echo "     network through it. h5i restarts it at the start of the NEXT run, so" >&2
  echo "     end this one first: exit the box shell (or let this run finish) and" >&2
  echo "     start another. The browser profile — cookies, logins — is not kept." >&2
fi

alive || {{
  CHROME=$(find_chrome) || {{ echo "h5i: no Chrome/Chromium found for the browser profile" >&2; exit 1; }}
  rm -rf "$UD"; mkdir -p "$UD"
  # `--no-sandbox` is required, not a shortcut: macOS refuses to nest Seatbelt
  # profiles, so Chrome's own sandbox cannot initialise inside the box and its
  # renderers abort on startup without it. h5i's box is the boundary.
  detach "$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage \
      --disable-gpu --no-first-run --no-default-browser-check \
      ${{PROXY_ARG:+"$PROXY_ARG"}} \
      --user-data-dir="$UD" --remote-debugging-port="$PORT" about:blank \
      >"$STATE/chrome.log" 2>&1 &
  echo $! > "$STATE/chrome.pid"
  # What this Chrome can reach, recorded beside its pid: the next run compares
  # against it rather than assuming a live browser is a correctly-routed one.
  printf '%s' "$PROXY_ARG" > "$STATE/chrome.proxy"
  rm -f "$STATE/chrome.restart"
  i=0
  while [ $i -lt 100 ]; do
    alive && break
    i=$((i+1)); sleep 0.1
  done
  alive || {{ echo "h5i: Chrome did not come up on port $PORT" >&2; tail -5 "$STATE/chrome.log" >&2; exit 1; }}
}}

exec "$REAL" --cdp "$PORT" "$@"
"##,
        egress_var = sandbox::EGRESS_PROXY_VAR,
    )
}

/// Materialize the shim for a `browser` box and return the directory to put on
/// `PATH`. `None` for every other profile, and when no `agent-browser` is
/// installed (nothing to attach with).
/// Where the browser shim lives and which port its Chrome answers on.
pub struct BrowserShim {
    /// Goes on `PATH` ahead of the real `agent-browser`.
    pub dir: PathBuf,
    /// The one loopback port the policy grants for CDP.
    pub port: u16,
}

/// A loopback port held for the life of the env: read back from `file` when it
/// is already there, otherwise reserved once and written down. Both ports a
/// browser box depends on are memorised by something that outlives a single run
/// (Chrome's CDP endpoint, and the proxy address Chrome was launched with), so
/// neither can be re-drawn per run.
fn remembered_port(file: &Path, what: &str, avoid: &[u16]) -> Result<u16, H5iError> {
    // The file lives under `<env>/browser/state`, which the box is granted write
    // on (it is where the box's own Chrome records its pid and port), so the
    // value read back here is box-controlled. Nothing catastrophic follows from
    // that — a port the box picks is one it could bind itself, and the policy
    // only ever grants the port the host actually bound — but a `0` would ask
    // for an ephemeral port under the name of a pinned one, and a privileged
    // port would fail to bind on every run. Both are rejected in favour of
    // drawing a fresh port, which is also what a corrupt file gets.
    //
    // `avoid` is the ports this env already holds. A drawn port is found by
    // binding an ephemeral listener and dropping it, so the next draw can be
    // handed the same number back — two of this env's own ports colliding would
    // leave the second service unable to bind at all.
    if let Some(p) = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| t.trim().parse::<u16>().ok())
        .filter(|p| *p >= 1024 && !avoid.contains(p))
    {
        return Ok(p);
    }
    let fail = || H5iError::Metadata(format!("could not reserve a loopback port for {what}"));
    let mut p = alloc_free_port().ok_or_else(fail)?;
    for _ in 0..8 {
        if !avoid.contains(&p) {
            std::fs::write(file, p.to_string()).map_err(|e| H5iError::with_path(e, file))?;
            return Ok(p);
        }
        p = alloc_free_port().ok_or_else(fail)?;
    }
    Err(fail())
}

/// Stop a browser the box has asked to have restarted, at the one place that
/// can: the host.
///
/// Chrome takes its proxy address once, at launch, so a browser started before
/// this box's current route out cannot reach the network through it. The shim
/// notices (it compares what it would launch with against `chrome.proxy`) and
/// leaves a `chrome.restart` marker, because inside the box the browser is
/// unreachable: it deliberately outlives the run that started it, which puts it
/// in a *previous* sandbox instance, and Seatbelt's
/// `(allow signal (target same-sandbox))` does not reach across that.
///
/// Best-effort throughout — a browser that will not die is a stale browser, not
/// a broken run, and must not block the run the user asked for.
#[cfg_attr(not(unix), allow(unused_variables))]
fn stop_stale_browser(state: &Path) {
    let marker = state.join("chrome.restart");
    if !marker.exists() {
        return;
    }
    stop_browser(state);
    // The marker goes whether or not anything was stopped: it must not re-fire
    // every run. `chrome.proxy` goes with it — left behind it would claim the
    // next Chrome was launched with a route it never saw.
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(state.join("chrome.proxy"));
}

/// Every process that is **this box's** browser.
///
/// Found by scanning for the discriminator the host already knows — the
/// `--user-data-dir` this env's Chrome was launched with, a path no other
/// process has a reason to name — rather than by trusting `<state>/chrome.pid`.
/// Two reasons, and the second is the one that matters:
///
///  - The number in that file is written by the **box** (`<env>/browser/state`
///    is granted write; it has to be, it is where Chrome records its port), and
///    it would be handed to `kill` on the host, outside every sandbox.
///    `"-1".parse::<i32>()` is `Ok(-1)`, and `kill(-1, …)` signals every process
///    the user can signal. The pid is not the thing to trust here.
///  - It is also, for the population this restart exists for, simply wrong. A
///    browser started by a shim from before `detach` `exec`'d was recorded under
///    its *launcher's* pid — and that launcher's argv is the shim, not Chrome —
///    so a pid-keyed lookup finds nothing in exactly the case where a stale
///    browser is certain to exist.
///
/// Both checks earn their place. The user-data-dir is what makes a match *this
/// box's* browser rather than some Chrome belonging to this user; the executable
/// name is what keeps a shell command that merely mentions the flag (a `pkill`,
/// an editor, this project's own tests) from being mistaken for the browser.
/// `--type=` processes are Chrome's own helpers: they carry the same profile
/// path, and stopping the browser they belong to takes them with it.
///
/// `None` means the lookup itself failed — inconclusive, which a caller must not
/// read as "nothing is running".
#[cfg(unix)]
fn browser_pids(profile: &Path) -> Option<Vec<i32>> {
    let needle = format!("--user-data-dir={}", profile.display());
    // `-A -o pid=,command=` is the same spelling on macOS and Linux.
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,command="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = Vec::new();
    for line in text.lines() {
        let Some((pid, cmd)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid <= 1 || !cmd.contains(&needle) || cmd.contains("--type=") {
            continue;
        }
        // Everything before the first ` --` is the executable — which on macOS
        // is a path full of spaces, so it cannot be taken as the first word.
        let exec = cmd
            .split(" --")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if exec.contains("chrome") || exec.contains("chromium") {
            pids.push(pid);
        }
    }
    Some(pids)
}

/// Stop this box's browser and wait for it to actually be gone, so a caller can
/// rely on the CDP port being free when this returns.
///
/// Waiting is the point, and it does block: SIGTERM then up to 2s, SIGKILL then
/// up to 1s, synchronously. Chrome's exit is not instant, and the shim decides
/// whether to launch by polling that port — a restart that only *asked* Chrome
/// to stop races it, finds the dying browser alive, and marks it for restart all
/// over again, a loop that never converges. Three seconds once, against a
/// browser that would otherwise stay unreachable, is the right trade.
///
/// Best-effort in what it stops: a browser that will not die is a stale browser,
/// not a broken run, and must not fail the run the user asked for. Not
/// best-effort in what it *records*: `chrome.pid` is removed only once the
/// browser is confirmed gone. An inconclusive lookup leaves it, because deleting
/// the only handle to a browser that may well be alive is how a one-run
/// annoyance becomes a permanent one.
#[cfg_attr(not(unix), allow(unused_variables))]
fn stop_browser(state: &Path) {
    #[cfg(unix)]
    {
        // Also the early-out for every env that never had a browser: `rm` calls
        // this for all of them.
        let profile = state.join("chrome");
        if !profile.exists() {
            return;
        }
        let Some(pids) = browser_pids(&profile) else {
            return; // could not look — change nothing, keep the record
        };
        for pid in pids {
            // SIGTERM first: headless Chrome takes it as a request to quit and
            // shuts its own children down with it, where SIGKILL would leave
            // them orphaned. (Not to keep the profile dir consistent — the
            // relaunch clears it.)
            if !signal_and_wait(pid, libc::SIGTERM, 20) {
                // A browser that ignores SIGTERM would otherwise be re-detected
                // and re-warned about on every run, forever.
                signal_and_wait(pid, libc::SIGKILL, 10);
            }
        }
        if browser_pids(&profile).is_some_and(|p| p.is_empty()) {
            let _ = std::fs::remove_file(state.join("chrome.pid"));
        }
    }
}

/// Send `sig` to `pid`, then poll (100ms, up to `ticks`) until it is gone.
/// Returns whether it went.
#[cfg(unix)]
fn signal_and_wait(pid: i32, sig: i32, ticks: u32) -> bool {
    unsafe {
        libc::kill(pid, sig);
    }
    for _ in 0..ticks {
        if pid_gone(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    pid_gone(pid)
}

/// Whether `pid` no longer exists. `kill(pid, 0)` is the existence check — it
/// signals nothing — but a bare "the call failed" is not the same question:
/// `EPERM` means the process is very much alive and simply not ours to signal,
/// and reporting that as *stopped* would be the one answer this is asked to
/// avoid. Only `ESRCH` is gone. (`stop_browser` re-checks with `browser_pids`
/// regardless, so a survivor keeps its record either way.)
#[cfg(unix)]
fn pid_gone(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn prepare_browser_shim(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
) -> Result<Option<BrowserShim>, H5iError> {
    if policy.profile.name != "browser" {
        return Ok(None);
    }
    let Some(real) = sandbox::agent_browser_binary() else {
        return Ok(None);
    };
    let dir = m.dir(h5i_root).join("browser");
    let state = dir.join("state");
    std::fs::create_dir_all(&state).map_err(|e| H5iError::with_path(e, &state))?;
    // Before anything else: a browser the last run found stranded on an old
    // route is stopped here, so the shim launches a fresh one below.
    stop_stale_browser(&state);

    // The CDP port is picked host-side, remembered, and reused. It cannot be
    // per-run: Chrome outlives the `box run` that started it, so a fresh port on
    // the next run would be a port the still-running Chrome is not listening on
    // — and, worse, the only port the policy grants. Allocated once, then read
    // back for the life of the env.
    let port = remembered_port(&state.join("cdp-port"), "the box's browser", &[])?;
    policy.loopback_ports.push(port);
    // The egress allowlist proxy is remembered for the same reason, in the
    // other direction: that surviving Chrome memorises the proxy address it was
    // launched with, and on the macOS supervised tier that proxy is the box's
    // only route out. See [`sandbox::ResolvedPolicy::egress_proxy_port`].
    policy.egress_proxy_port = Some(remembered_port(
        &state.join("egress-port"),
        "the box's egress proxy",
        &[port],
    )?);
    let shim = dir.join("agent-browser");
    std::fs::write(&shim, browser_shim_source(&real)).map_err(|e| H5iError::with_path(e, &shim))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| H5iError::with_path(e, &shim))?;
    }
    // The box runs the shim and writes Chrome's port and profile under `state`.
    let d = dir.display().to_string();
    if !policy.profile.fs_read.contains(&d) {
        policy.profile.fs_read.push(d);
    }
    let s = state.display().to_string();
    if !policy.profile.fs_write.contains(&s) {
        policy.profile.fs_write.push(s);
    }
    Ok(Some(BrowserShim { dir, port }))
}

/// Environment a `browser` box needs, derived from the policy that is actually
/// enforced.
///
/// Two things are being done here, and both are policy decisions rather than
/// convenience:
///
/// * **`--allowed-domains` from `net.egress`.** The tier's own enforcement is
///   the boundary (nftables at `supervised`, the proxy at `container`); this is
///   a second, in-process layer, so a page that tries to pull from an off-list
///   host fails in the browser with a clear message instead of dying at the
///   packet level. Loopback is always added: the dev server under test is the
///   whole point, and it never appears in an egress allowlist.
/// * **AI features off.** `agent-browser chat` and the dashboard's AI panel
///   send page content to an external gateway. Inside a box that is an
///   exfiltration path with a friendly name, so the gateway credential is kept
///   out of the box entirely: it is absent from `env.pass` and never injected.
pub fn browser_env(policy: &ResolvedPolicy, shim: Option<&BrowserShim>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(BrowserShim { dir, port }) = shim {
        // Ahead of the real binary, so a bare `agent-browser` gets the shim.
        // `injected_env` is applied after the `env.pass` allowlist, so this PATH
        // wins over the host's.
        let host_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        out.push(("PATH".to_string(), format!("{}:{host_path}", dir.display())));
        out.push((
            "H5I_BROWSER_STATE".to_string(),
            dir.join("state").display().to_string(),
        ));
        // Explicitly the shim's own port, not "whichever loopback port came
        // first" — the policy also grants the box's live service ports.
        out.push(("H5I_BROWSER_CDP_PORT".to_string(), port.to_string()));
    }
    out.extend(browser_env_inner(policy, shim.is_some()));
    out
}

fn browser_env_inner(policy: &ResolvedPolicy, shimmed: bool) -> Vec<(String, String)> {
    let mut allowed: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into()];
    for host in &policy.profile.net_egress {
        // `net.egress` entries may carry a `:port`; the browser wants hosts.
        let host = match host.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => host.as_str(),
        };
        if !host.is_empty() && !allowed.iter().any(|a| a == host) {
            allowed.push(host.to_string());
        }
    }
    let mut v: Vec<(String, String)> = Vec::new();
    // Only when agent-browser is launching Chrome itself. Under the shim it
    // attaches with `--cdp`, which upstream refuses to combine with
    // `--allowed-domains`, so setting it would break every command rather than
    // add a layer. The tier's egress enforcement is unchanged either way; what
    // is lost is agent-browser's own in-process check. See `browser_shim_source`.
    if !shimmed {
        v.push(("AGENT_BROWSER_ALLOWED_DOMAINS".to_string(), allowed.join(",")));
    }
    v.extend(vec![
        // Headless. There is no `AGENT_BROWSER_HEADLESS` — agent-browser reads
        // `AGENT_BROWSER_HEADED` and headless is what it does when that is
        // falsey, so the way to pin headless is to pin *that* variable off.
        // Setting a variable the tool never reads is worse than setting
        // nothing: it reads like enforcement in a policy review and is not.
        //
        // It matters because a headed launch on a displayless box starts a
        // private Xvfb, which is a whole second process tree the profile never
        // granted, and the failure would arrive from Chrome rather than here.
        ("AGENT_BROWSER_HEADED".to_string(), "0".to_string()),
        // The daemon's control socket. Its default is `$XDG_RUNTIME_DIR`
        // (`/run/user/<uid>`), which no box has a write grant for — and the
        // failure is an opaque "Failed to create socket directory: Permission
        // denied" long after create said everything was fine. Point it at the
        // box's own `/tmp`, which every tier grants (and which the kernel tiers
        // redirect to a per-env scratch, so two boxes never share a socket).
        (
            "AGENT_BROWSER_SOCKET_DIR".to_string(),
            format!("{}/agent-browser", box_tmp_root(policy)),
        ),
        // Chat off. There is exactly one gate upstream and it is the presence
        // of `AI_GATEWAY_API_KEY`, so "off" is spelled by that variable being
        // absent: it is not in `env.pass` and nothing here injects it.
        //
        // Two things that look like they would do this and do not. Pinning the
        // key to an empty string *enables* chat, because the test is presence,
        // not value — `agent-browser doctor` inside a box reported "chat
        // enabled" for exactly that reason. And there is no
        // `AGENT_BROWSER_DISABLE_CHAT`; we set one for a while, and a variable
        // agent-browser never reads is a policy line that reviews as
        // enforcement while enforcing nothing. Absence is the whole mechanism.
        // Chrome's own sandbox needs the namespace syscalls our seccomp policy
        // denies, at every tier. h5i's box is the boundary; Chrome's is not
        // available inside it, and without this the renderer dies at startup.
        (
            "AGENT_BROWSER_ARGS".to_string(),
            "--no-sandbox --disable-dev-shm-usage".to_string(),
        ),
    ]);
    v
}

fn merged_env(a: &[(String, String)], b: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = a.to_vec();
    out.extend_from_slice(b);
    out
}

/// If this env is bound to a team persona (files written by `h5i team add-env`),
/// inject `H5I_AGENT=<persona>` and `H5I_TEAM=<run>` for scoped in-box requests.
/// The coordination refs and cursors remain host-only.
fn team_identity_env(m: &EnvManifest, h5i_root: &Path) -> Vec<(String, String)> {
    let Some((team, agent)) = team_binding(h5i_root, m) else {
        return Vec::new();
    };
    vec![
        ("H5I_AGENT".to_string(), agent),
        (H5I_TEAM_VAR.to_string(), team),
        // The base tree lets the in-box `team agent submit` detect "nothing to
        // review" *before* staging a request the host must refuse (an env
        // created by `team add-env` is pinned to the team base).
        (H5I_ENV_BASE_TREE_VAR.to_string(), m.base_tree.clone()),
    ]
}

pub fn team_binding(h5i_root: &Path, m: &EnvManifest) -> Option<(String, String)> {
    let dir = m.dir(h5i_root);
    let agent = std::fs::read_to_string(dir.join("team-identity")).ok()?;
    let team = std::fs::read_to_string(dir.join("team-run")).ok()?;
    let agent = agent.trim();
    let team = team.trim();
    if agent.is_empty() || team.is_empty() {
        None
    } else {
        Some((team.to_string(), agent.to_string()))
    }
}

/// A context snapshot staged from inside a box. The box can build the anchor
/// commit object (the `objects/` store is rw) but can't write
/// `refs/h5i/context-snapshots/*` (sealed ro), so the *ref creation* is deferred
/// to the host ingest — scoped to the env's own commits, like the note spool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshotSpool {
    /// The git commit this snapshot is linked to (range-guarded on ingest).
    pub git_sha: String,
    /// Short sha — the `refs/h5i/context-snapshots/<short>` ref leaf.
    pub short_sha: String,
    /// The pre-built anchor commit (already in the shared object store) the ref
    /// should point at.
    pub anchor_oid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSubmitSpool {
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

pub fn write_team_submit_spool(
    spool: &Path,
    request: &TeamSubmitSpool,
) -> Result<String, H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("team-submit-{}-{nanos}", std::process::id());
    let path = spool.join(format!("{base}.json"));
    let json = serde_json::to_vec(request)?;
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(base)
}

/// A boxed agent's staged peer-review (the outbound mirror of the inbound
/// inbox). The box can't write the host-only team store, so `h5i team review
/// submit` stages this; the host ingests it after the session, recording the
/// review under the box's identity-validated team binding (the box-written
/// `reviewer` is ignored — authority comes from the env binding, never a field
/// the box controls).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReviewSpool {
    pub target: String,
    pub body: String,
}

pub fn write_team_review_spool(
    spool: &Path,
    request: &TeamReviewSpool,
) -> Result<String, H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("team-review-{}-{nanos}", std::process::id());
    let path = spool.join(format!("{base}.json"));
    let json = serde_json::to_vec(request)?;
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(base)
}

/// One outbound data reply staged in-box by `h5i team agent reply` — the
/// box-side half of an orchestra `ask` turn: free-text/JSON addressed to the
/// host, ingested as an `agent_reply` team event (like the other spools, the
/// box writes *what*, never *who* — authority is the env binding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReplySpool {
    pub body: String,
}

pub fn write_team_reply_spool(
    spool: &Path,
    request: &TeamReplySpool,
) -> Result<String, H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("team-reply-{}-{nanos}", std::process::id());
    let path = spool.join(format!("{base}.json"));
    let json = serde_json::to_vec(request)?;
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(base)
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
    if claim.image_backed()
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

// ─── user egress allowlist (`h5i box allow`) ─────────────────────────────────

/// Path of the persistent, **host-side** user egress allowlist: one rule per
/// line (`api.example.com`, `.example.com`, `host:443`; `#` comments). Lives
/// under the user config dir — `$XDG_CONFIG_HOME/h5i/egress-allow`, defaulting
/// to `~/.config/h5i/egress-allow` — deliberately OUTSIDE the repo, `$WORK`,
/// and every box-granted path: an in-box agent must never be able to widen its
/// own allowlist (the kernel-tier grants don't include `~/.config/h5i`, and
/// the container's read-only rootfs never mounts host HOME).
pub fn user_allow_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("h5i").join("egress-allow"))
}

/// Read + normalize the user allowlist. A missing/unreadable file is simply
/// empty (fail-closed toward "no extra grants"); an invalid line is skipped
/// with a warning rather than failing the session that read it.
pub fn user_allow_list() -> Vec<String> {
    user_allow_list_at(user_allow_path().as_deref())
}

fn user_allow_list_at(path: Option<&Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match validate_egress_rule(line) {
            Ok(rule) => {
                if !out.contains(&rule) {
                    out.push(rule);
                }
            }
            Err(e) => eprintln!(
                "warning: ignoring invalid egress rule in {}: {e}",
                path.display()
            ),
        }
    }
    out
}

/// Validate + normalize (lowercase) one user egress rule. Accepted forms are
/// exactly what the proxy's `AllowList` understands: `host`, `.host` /
/// `*.host` (subdomain wildcard), each with an optional numeric `:port`
/// suffix. Everything else — URLs, paths, whitespace, IPv6 literals — is
/// rejected: this feeds a network policy, so intake is strict even where the
/// enforcing parser is lenient.
pub fn validate_egress_rule(raw: &str) -> Result<String, H5iError> {
    let rule = raw.trim().to_ascii_lowercase();
    let bad =
        |why: &str| Err(H5iError::Metadata(format!("invalid egress rule '{raw}': {why}")));
    if rule.is_empty() {
        return bad("empty rule");
    }
    if rule.len() > 260 {
        return bad("rule too long");
    }
    if rule.contains("://") || rule.contains('/') {
        return bad("must be a bare host[:port], not a URL or path");
    }
    if rule.chars().any(|c| c.is_whitespace() || c == ',') {
        return bad("whitespace and commas are not allowed");
    }
    let (host_part, port) = match rule.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h, Some(p)),
        Some(_) => return bad("only a numeric `:port` suffix is allowed"),
        None => (rule.as_str(), None),
    };
    if let Some(p) = port {
        if p.parse::<u16>().is_err() {
            return bad("port out of range");
        }
    }
    let host = host_part
        .strip_prefix("*.")
        .or_else(|| host_part.strip_prefix('.'))
        .unwrap_or(host_part);
    if host.is_empty() {
        return bad("empty host");
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
    {
        return bad("host may contain only letters, digits, '-', '.', '_'");
    }
    if host.starts_with('-') || host.starts_with('.') || host.ends_with('.') || host.contains("..")
    {
        return bad("malformed host");
    }
    Ok(rule)
}

/// Resolve the allowlist path for a **mutation**, refusing inside an env box:
/// the allowlist is host policy, and a confined agent must not widen its own
/// network grants (defense in depth on top of the fs grants, which never
/// include this path).
fn user_allow_guarded_path() -> Result<PathBuf, H5iError> {
    if std::env::var_os(H5I_ENV_ID_VAR).is_some() {
        return Err(H5iError::Metadata(
            "refusing to edit the user egress allowlist from inside an env box — `h5i box \
             allow` is host-side policy (a confined agent must not widen its own network \
             grants); run it on the host"
                .into(),
        ));
    }
    user_allow_path().ok_or_else(|| {
        H5iError::Metadata(
            "cannot resolve the user config dir — set $HOME or $XDG_CONFIG_HOME".into(),
        )
    })
}

/// Add a rule to the user allowlist. Returns `(added, path)`; `added` is false
/// when the rule was already present.
pub fn user_allow_add(raw: &str) -> Result<(bool, PathBuf), H5iError> {
    let rule = validate_egress_rule(raw)?;
    let path = user_allow_guarded_path()?;
    let mut rules = user_allow_list_at(Some(&path));
    if rules.iter().any(|r| r == &rule) {
        return Ok((false, path));
    }
    rules.push(rule);
    write_user_allow(&path, &rules)?;
    Ok((true, path))
}

/// Remove a rule from the user allowlist. Returns `(removed, path)`.
pub fn user_allow_remove(raw: &str) -> Result<(bool, PathBuf), H5iError> {
    let rule = validate_egress_rule(raw)?;
    let path = user_allow_guarded_path()?;
    let mut rules = user_allow_list_at(Some(&path));
    let before = rules.len();
    rules.retain(|r| r != &rule);
    if rules.len() == before {
        return Ok((false, path));
    }
    write_user_allow(&path, &rules)?;
    Ok((true, path))
}

fn write_user_allow(path: &Path, rules: &[String]) -> Result<(), H5iError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let mut text = String::from(
        "# h5i user egress allowlist — extra hosts merged into container-tier envs whose\n\
         # profile already sets net.egress. Managed by `h5i box allow`; hand-edits kept.\n",
    );
    for r in rules {
        text.push_str(r);
        text.push('\n');
    }
    // Temp-file + rename so a concurrent session never reads a half-written list.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| H5iError::with_path(e, &tmp))?;
    std::fs::rename(&tmp, path).map_err(|e| H5iError::with_path(e, path))?;
    Ok(())
}

/// Merge the host-side user allowlist into the session policy and announce the
/// enforced egress scope. The extras apply ONLY where the proxy enforces them:
/// the container tier, on a profile that already declares `net.egress`
/// (deny-all is never widened from outside the digested policy; the kernel
/// tiers have no domain allowlist to widen). Explained, not silent: the
/// effective list is printed at session start so an in-box
/// `403 Blocked by network policy` is self-diagnosing.
fn apply_user_egress(policy: &mut sandbox::ResolvedPolicy) {
    let user = user_allow_list();
    let enforced =
        policy.claim.enforces_egress_allowlist() && !policy.profile.net_egress.is_empty();
    if enforced {
        policy.user_egress_allow = user
            .into_iter()
            .filter(|u| {
                !policy
                    .profile
                    .net_egress
                    .iter()
                    .any(|p| p.trim().eq_ignore_ascii_case(u))
            })
            .collect();
        announce_egress(policy);
    } else if policy.claim.enforces_egress_allowlist() && !user.is_empty() {
        eprintln!(
            "note: {} `h5i box allow` rule(s) ignored — profile '{}' sets no net.egress \
             (a deny-all profile is never widened from outside the policy)",
            user.len(),
            policy.profile.name
        );
    }
}

/// One line at session start explaining the enforced egress scope — and, since
/// the two tiers that enforce it do so by different mechanisms with different
/// holes, *how* it is enforced. A `403` from a proxy and a dropped packet are
/// diagnosed differently, and the line is the only place the box's operator is
/// told which one to expect.
fn announce_egress(policy: &sandbox::ResolvedPolicy) {
    const SHOW: usize = 8;
    let profile = &policy.profile.net_egress;
    let mut line = profile
        .iter()
        .map(|s| s.trim())
        .take(SHOW)
        .collect::<Vec<_>>()
        .join(", ");
    let more = profile.len().saturating_sub(SHOW);
    if more > 0 {
        line.push_str(&format!(" (+{more} more)"));
    }
    let user_part = if policy.user_egress_allow.is_empty() {
        String::new()
    } else {
        format!(
            "  + user allow: {} (via `h5i box allow`)",
            policy.user_egress_allow.join(", ")
        )
    };
    let how = match policy.claim {
        IsolationClaim::Microvm => "address-enforced in the VM netstack, everything else dropped",
        _ => "proxy-enforced, everything else 403",
    };
    eprintln!("⦿ egress ({how}): {line}{user_part}");
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
    run_inner(repo, h5i_root, m, argv, None)
}

/// [`run`], plus one **writable** cache bind.
///
/// The only caller is `h5i box cache refresh`, which runs an ecosystem's fetch
/// step in a box with no agent in it. Keeping it a separate entry point rather
/// than a policy field an ordinary profile could set means an agent session can
/// never reach this path: there is nothing it could write in `.h5i/env.toml` to
/// make its own cache writable.
pub fn run_with_cache_write(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    argv: &[String],
    // `(host cache dir, path inside the box)`.
    cache_write: (&Path, &Path),
) -> Result<RunOutcome, H5iError> {
    run_inner(repo, h5i_root, m, argv, Some(cache_write))
}

fn run_inner(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    argv: &[String],
    cache_write: Option<(&Path, &Path)>,
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
    // Register in the live-session registry for the run's duration, so
    // list/status/the dashboard can tell a live run from a stale status.
    let _live = LiveGuard::register(
        &m.dir(h5i_root),
        "run",
        Some(crate::secrets::redact_text(&argv.join(" "))),
    );

    // The stored policy, digest-verified, then re-resolved against a fresh
    // host probe (fail closed if the host can no longer satisfy the claim).
    let mut policy = load_policy(h5i_root, m)?;
    // Structural grants (like the implicit `$WORK` rw): the worktree must be a
    // functional git checkout inside the box.
    grant_box_git(repo, m, &work, &mut policy, false)?;
    prepare_private_paths(h5i_root, m, &mut policy, &work)?;
    prepare_private_tmp(h5i_root, m, &mut policy, None)?;
    let browser_shim = prepare_browser_shim(h5i_root, m, &mut policy)?;
    policy.loopback_ports.extend(live_service_ports(h5i_root, m));
    // Declared in the profile (`[profile.X.net] loopback`), so a dev server the
    // box runs itself is reachable from the box's own browser.
    let declared = policy.profile.loopback_ports.clone();
    policy.loopback_ports.extend(declared);
    prepare_home_state(
        h5i_root,
        m,
        &mut policy,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        None,
    )?;
    let env_capture_env = prepare_env_capture_spool(h5i_root, m, &mut policy)?;
    match cache_write {
        // A refresh box: this one cache is writable, at the same path the
        // read-only mount will later expose, so what is fetched is exactly what
        // a later box sees.
        Some((host, target)) => {
            policy.profile.fs_write.push(host.display().to_string());
            policy.cache_write = Some(sandbox::RoBind {
                backing: host.to_path_buf(),
                target: target.to_path_buf(),
            });
        }
        None => prepare_cache_mounts(h5i_root, &work, &mut policy),
    }
    let env_inbox_env = prepare_env_inbox(h5i_root, m, &mut policy)?;
    let cargo_env = prepare_cargo_env(&work, &policy)?;
    // Host-side `h5i box allow` extras + the explained-egress line.
    apply_user_egress(&mut policy);

    // Broker any declared secrets BEFORE marking the env running, so a
    // fail-closed grant (missing source, unsupported inject) aborts cleanly
    // without leaving the env in 'running'. `brokered` lives for the whole run;
    // its Drop guard unlinks any file-injected secrets on every exit path.
    let secret_dir = m.dir(h5i_root).join("secrets");
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered = crate::secrets_broker::broker(
        &policy.profile.secret_grants,
        &secret_dir,
        is_workspace,
        policy.profile.allow_command_extractors,
    )?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    // Authenticated egress the profile declares (5.5). Host-side credentials
    // resolve here and stay here; the box only ever learns the proxy's origin.
    // Held for the run: dropping a handle shuts its proxy down.
    let (_auth_handles, auth_env) = crate::container::engage_auth_grants(
        &policy.profile,
        policy.claim >= IsolationClaim::Supervised,
    )?;

    let injected_env = merged_env(
        &merged_env(
            &merged_env(&merged_env(&brokered.env, &env_capture_env), &cargo_env),
            &env_inbox_env,
        ),
        &merged_env(
            &team_identity_env(m, h5i_root),
            &merged_env(
                &if policy.profile.name == "browser" {
                    browser_env(&policy, browser_shim.as_ref())
                } else {
                    Vec::new()
                },
                &auth_env,
            ),
        ),
    );

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

    // Browser evidence: when this run drove the browser, ask the page what
    // happened before recording the run. The drain executes in the same box
    // under the same policy, so it is confined like anything else, and it runs
    // at a moment h5i picks rather than one the agent picks.
    //
    // Two gates, and both matter. The run has to have touched the browser, and
    // a browser has to still be live — the drain command would otherwise *start*
    // one, so a `cargo test` in a browser box would launch Chrome just to be
    // told the console was empty, and report a clean page it never looked at.
    //
    // The drain reuses this run's already-prepared policy rather than going
    // through `dev run` again, which is what makes it see the run's own browser:
    // a fresh `dev run` re-runs `prepare_private_tmp`, wiping the scratch that
    // holds the daemon's socket, and would get a new session with empty buffers.
    let env_dir_path = m.dir(h5i_root);
    let browser_evidence = crate::browser::run_touched_browser(argv)
        .filter(|verb| crate::browser::verb_wants_drain(verb))
        .filter(|_| crate::browser::browser_is_live(&env_dir_path))
        .map(|verb| {
            crate::browser::collect(&env_dir_path, &verb, |drain_argv| {
                let out = sandbox::run_with_env(&policy, &work, drain_argv, &injected_env).ok()?;
                // A non-zero drain is a browser that is gone or wedged. Report
                // that as `unavailable`, never as a clean page.
                (out.exit_code == Some(0)).then_some(out.stdout)
            })
        });

    // Read HEAD from the WORKTREE repo so the tree recorded is the env's.
    let wt_repo = Repository::open(&work)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        source: "host-env-run".into(),
        cmd: Some(argv.join(" ")),
        cwd: Some(work.display().to_string()),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        wall_ms: u64::try_from(outcome.wall_ms).ok(),
        cpu_ms: u64::try_from(outcome.cpu_ms).ok(),
        max_rss_kb: outcome.max_rss_kb.and_then(|kb| u64::try_from(kb).ok()),
        git_tree: head_tree,
        files: Vec::new(),
        // Network egress verdicts (container tier's allowlist proxy); `None` for
        // workspace/process. Host observed: the box never supplies this.
        egress: outcome.egress.clone(),
        browser: browser_evidence,
    };
    let captured = crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, &raw)?;
    let capture_id = captured.id.clone();

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
        receipt: captured,
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
    command: &[String],
    // A read-only observer session: `$WORK` is granted read-only, the box gets a
    // per-session ephemeral HOME/tmp/secrets so concurrent observers never race,
    // and no env state (status / captures / manifest) is mutated. Serialized
    // with a shared lock so N observers coexist but none overlaps a read-write
    // session. `false` → an ordinary read-write session (unchanged).
    readonly: bool,
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

    // An observer takes the shared observer-presence lock (many coexist, and it
    // does not exclude a live read-write session); a read-write session takes
    // the exclusive writer lock (`run.lock`, serialized against other writers).
    // So one read-write shell and N observers coexist. An observer may see torn
    // reads of a worktree a writer is mutating — expected when watching work in
    // progress; write-isolation is enforced by the read-only $WORK mount, not
    // this lock. Only a worktree teardown (gc/rm) drains observers.
    #[cfg(unix)]
    let _run_lock = if readonly {
        RunLock::acquire_observer(&m.dir(h5i_root))?
    } else {
        RunLock::acquire(&m.dir(h5i_root))?
    };
    // Register in the live-session registry for the session's duration (an
    // observer registers too — "who is watching" is part of the live picture).
    let _live = LiveGuard::register(
        &m.dir(h5i_root),
        if readonly { "observe" } else { "shell" },
        (!command.is_empty()).then(|| crate::secrets::redact_text(&command.join(" "))),
    );

    let mut policy = load_policy(h5i_root, m)?;

    // Fail closed: a read-only session must run on a tier that can actually pin
    // `$WORK` read-only. The workspace tier has no mount namespace / Landlock to
    // enforce with, and a read-only container worktree mount is a follow-up — so
    // refuse rather than hand back an "observer" that could still write.
    if readonly
        && !matches!(
            policy.claim,
            IsolationClaim::Process | IsolationClaim::Supervised
        )
    {
        return Err(H5iError::Metadata(format!(
            "`env shell --readonly` needs a kernel-enforced worktree \
             (isolation=process or supervised); {} resolved to '{}', which cannot pin \
             $WORK read-only — refusing rather than granting an unenforced read-only \
             session (fail-closed). Use a normal `env shell`, or re-create with \
             --isolation process/supervised.",
            m.id,
            policy.claim.as_str()
        )));
    }
    policy.work_readonly = readonly;

    // A read-only observer's writable state (ephemeral HOME copy, /tmp, brokered
    // secrets, cargo target) lives in a per-session scratch keyed by pid, so
    // concurrent observers never collide; it is wiped when the session ends
    // (SessionScratchGuard, on every return path). Read-write runs use the
    // persistent per-env dirs unchanged.
    let session_root = if readonly {
        let root = m.dir(h5i_root).join("ro").join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&root); // clear any stale (pid-reuse) leftovers
        std::fs::create_dir_all(&root).map_err(|e| H5iError::with_path(e, &root))?;
        Some(root)
    } else {
        None
    };
    let _scratch = SessionScratchGuard(session_root.clone());

    // Same structural grants as `run`: an interactive boxed agent lives in
    // this worktree and must be able to use git / h5i context inside it. Under
    // --readonly the git surface is granted read-only.
    grant_box_git(repo, m, &work, &mut policy, readonly)?;
    // Per-env private-path binds give read-write runs writable, non-colliding
    // build caches; an observer sees the real worktree read-only and skips them.
    if !readonly {
        prepare_private_paths(h5i_root, m, &mut policy, &work)?;
    }
    prepare_private_tmp(
        h5i_root,
        m,
        &mut policy,
        session_root.as_deref().map(|r| r.join("tmp")).as_deref(),
    )?;
    let browser_shim = prepare_browser_shim(h5i_root, m, &mut policy)?;
    policy.loopback_ports.extend(live_service_ports(h5i_root, m));
    // Declared in the profile (`[profile.X.net] loopback`), so a dev server the
    // box runs itself is reachable from the box's own browser.
    let declared = policy.profile.loopback_ports.clone();
    policy.loopback_ports.extend(declared);
    prepare_home_state(
        h5i_root,
        m,
        &mut policy,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        session_root.as_deref().map(|r| r.join("home")).as_deref(),
    )?;
    // An observer captures nothing (it changes nothing) — no capture spool.
    let env_capture_env = if readonly {
        Vec::new()
    } else {
        prepare_env_capture_spool(h5i_root, m, &mut policy)?
    };
    let env_inbox_env = prepare_env_inbox(h5i_root, m, &mut policy)?;
    prepare_cache_mounts(h5i_root, &work, &mut policy);
    let cargo_env = match &session_root {
        // `$WORK` is read-only for an observer, so cargo's default target dir
        // (`$WORK/.h5i/cargo-target`) is unwritable — point it at the scratch.
        Some(root) => {
            let target = root.join("cargo-target");
            std::fs::create_dir_all(&target).map_err(|e| H5iError::with_path(e, &target))?;
            if policy.claim >= IsolationClaim::Process {
                policy.profile.fs_write.push(target.display().to_string());
                vec![("CARGO_TARGET_DIR".to_string(), target.display().to_string())]
            } else {
                Vec::new()
            }
        }
        None => prepare_cargo_env(&work, &policy)?,
    };
    let secret_dir = match &session_root {
        Some(root) => root.join("secrets"),
        None => m.dir(h5i_root).join("secrets"),
    };
    let is_workspace = matches!(policy.claim, IsolationClaim::Workspace);
    let brokered = crate::secrets_broker::broker(
        &policy.profile.secret_grants,
        &secret_dir,
        is_workspace,
        policy.profile.allow_command_extractors,
    )?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    let injected_env = merged_env(
        &merged_env(
            &merged_env(&merged_env(&brokered.env, &env_capture_env), &cargo_env),
            &env_inbox_env,
        ),
        &merged_env(
            &team_identity_env(m, h5i_root),
            &if policy.profile.name == "browser" {
                browser_env(&policy, browser_shim.as_ref())
            } else {
                Vec::new()
            },
        ),
    );
    // Host-side `h5i box allow` extras + the explained-egress line.
    apply_user_egress(&mut policy);

    // No command given → launch an interactive shell. Rather than inherit the
    // host `~/.bashrc` (which, under confinement, routinely references tools the
    // sandbox blocks — e.g. `~/.local/bin/powerline-shell`), bash is launched
    // with a generated *plain* rcfile by default; a profile may pin a custom one
    // via `[profile.X.shell] rcfile = "…"`. May Landlock-grant the generated rc.
    let launch = if command.is_empty() {
        default_shell_argv(h5i_root, m, &mut policy, &work)?
    } else {
        ShellLaunch::argv(command.to_vec())
    };
    let argv = launch.argv;
    // Whatever the shell launch itself needs (zsh's `$ZDOTDIR`) is merged last so
    // it is the value the shell actually starts with.
    let injected_env = merged_env(&injected_env, &launch.env);

    // A read-only observer must not touch env state: an idle/created env stays
    // in its status, and a concurrent observer must never flip it to running and
    // back. It records an append-only `observe` event instead (no manifest
    // write) — auditable, CAS-safe, and harmless if it races another observer.
    if readonly {
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "observe".into(),
                detail: Some("read-only shell (open)".into()),
                capture: None,
            },
        )?;
    } else {
        set_status(
            repo,
            h5i_root,
            m,
            ST_RUNNING,
            "status",
            Some("running (shell)".into()),
            None,
        )?;
    }
    // No managed-settings injection: the in-box hook it carried rewrote agent
    // commands into `h5i capture run`, which no longer exists. The container
    // tee shim is the observation floor, and it needs no agent cooperation.
    let session = match sandbox::run_interactive(&policy, &work, &argv, &injected_env, None) {
        Ok(outcome) => outcome,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            if !readonly {
                set_status(
                    repo,
                    h5i_root,
                    m,
                    ST_IDLE,
                    "status",
                    Some("idle (shell failed to start)".into()),
                    None,
                )?;
            }
            return Err(e);
        }
    };
    let exit_code = session.exit_code;
    if let Err(e) = protected_hook_configs.finish() {
        if !readonly {
            set_status(
                repo,
                h5i_root,
                m,
                ST_IDLE,
                "violation",
                Some(e.to_string()),
                None,
            )?;
        }
        return Err(e);
    }

    // A read-only observer changes nothing, so there is no observation spool to
    // ingest and no status to transition — it closes with an append-only
    // `observe` event carrying the exit code (secrets redacted).
    if readonly {
        let safe_cmd = crate::secrets::redact_text(&argv.join(" "));
        append_event(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "observe".into(),
                detail: Some(format!("read-only shell cmd=`{safe_cmd}` exit={exit_code}")),
                capture: None,
            },
        )?;
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
        return Ok(exit_code);
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

    // The session's egress verdicts (container tier's allowlist proxy) become
    // evidence exactly like a captured run's — an interactive session must not
    // be a network blind spot. Recorded only when the proxy saw traffic;
    // best-effort (a failed capture warns, never breaks the session).
    let egress_capture = match session.egress.as_ref() {
        Some(eg) if eg.allowed + eg.denied > 0 => {
            match capture_shell_egress(h5i_root, m, &work, eg, exit_code) {
                Ok(id) => {
                    m.captures.push(id.clone());
                    Some(id)
                }
                Err(e) => {
                    eprintln!("warning: shell egress capture failed: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    let egress_note = session
        .egress
        .as_ref()
        .filter(|eg| eg.allowed + eg.denied > 0)
        .map(|eg| format!(" egress={}ok/{}denied", eg.allowed, eg.denied))
        .unwrap_or_default();

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
            "interactive cmd=`{safe_cmd}` exit={exit_code}{observed_note}{egress_note}"
        )),
        egress_capture,
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

/// Persist an interactive session's egress tally as an env-tagged capture. The
/// raw payload is a small human-readable rendering; the queryable data rides
/// in `Manifest::egress` and the synthesized `egress-denied` findings (see
/// `objects::capture`), so `recall search <host>` covers shell sessions too.
fn capture_shell_egress(
    h5i_root: &Path,
    m: &EnvManifest,
    work: &Path,
    eg: &crate::sandbox_policy::EgressSummary,
    exit_code: i32,
) -> Result<String, H5iError> {
    let mut raw = format!(
        "interactive session egress: {} allowed, {} denied\n",
        eg.allowed, eg.denied
    );
    for h in &eg.hosts {
        raw.push_str(&format!(
            "  {}:{}  allowed={} denied={}\n",
            h.host, h.port, h.allowed, h.denied
        ));
    }
    let wt_repo = Repository::open(work)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        source: "host-env-shell".into(),
        cmd: Some(format!("env shell {}", m.id)),
        cwd: Some(work.display().to_string()),
        exit_code: Some(exit_code),
        git_tree: head_tree,
        egress: Some(eg.clone()),
        ..Default::default()
    };
    Ok(
        crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, raw.as_bytes())?.id,
    )
}

// ─── interactive shell rc ────────────────────────────────────────────────────

/// How to launch the default interactive shell: its argv, plus the environment
/// the launch itself needs (today only zsh's `$ZDOTDIR`, which cannot be set
/// from an rc file because it is read before any rc is sourced).
#[derive(Debug)]
struct ShellLaunch {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

impl ShellLaunch {
    /// A launch that needs nothing injected — every shell but zsh.
    fn argv(argv: Vec<String>) -> Self {
        ShellLaunch {
            argv,
            env: Vec::new(),
        }
    }
}

/// Build the argv for a default (no-command) interactive `env shell` session.
///
/// On the **kernel tiers** the box runs against the host filesystem, so the host
/// `$SHELL` is the right binary. On the **image-backed** tiers it is not a
/// binary that exists at all — see [`box_shell_argv`].
///
/// For **bash** the host `~/.bashrc` is *not* sourced by default — under
/// confinement it routinely calls tools the sandbox blocks (e.g.
/// `~/.local/bin/powerline-shell`), spraying `Permission denied` noise. Instead
/// bash is pointed at:
///   - a **custom** rcfile when the profile sets `[shell] rcfile` — resolved
///     relative to `$WORK` (the worktree), so it is version-controlled and
///     reachable in the box on every tier without an extra grant; or
///   - a generated **plain** rcfile (clear prompt, a couple of aliases, and an
///     optional `~/.h5i_envrc` hook), written under the env's private dir and
///     Landlock-granted read on the kernel tiers.
///
/// **zsh** — the macOS default, so the common case on a Mac host — gets the same
/// treatment through the only knob it has: `$ZDOTDIR`. zsh has no `--rcfile`; it
/// takes its startup files from `$ZDOTDIR` (falling back to `$HOME`), so pointing
/// that at a generated dir both skips the host `~/.zshrc` and moves `$HISTFILE`
/// off the real `~/.zsh_history` — which is *outside every grant* in a box, so
/// zsh's history lock fails on it (`locking failed … operation not permitted`)
/// once at startup and again after every command. See [`write_plain_zshrc`].
///
/// Other shells fall through to a bare `[$SHELL, "-i"]`; the image-backed tiers
/// (whose shell *and* rc come from the image, not the host) fall through to
/// [`box_shell_argv`].
fn default_shell_argv(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
    work: &Path,
) -> Result<ShellLaunch, H5iError> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    shell_launch(h5i_root, m, policy, work, &shell)
}

/// [`default_shell_argv`] with the host shell passed in rather than read from the
/// environment, so the tests can cover every shell on any host without mutating
/// a process-wide variable other tests read.
fn shell_launch(
    h5i_root: &Path,
    m: &EnvManifest,
    policy: &mut ResolvedPolicy,
    work: &Path,
    shell: &str,
) -> Result<ShellLaunch, H5iError> {
    let shell = shell.to_string();
    let shell_name = Path::new(&shell).file_name().unwrap_or_default().to_owned();
    let is_bash = shell_name == "bash";
    let is_zsh = shell_name == "zsh";
    let bare = ShellLaunch::argv(vec![shell.clone(), "-i".to_string()]);

    // The box's shell + its rc come from the image, not the host — the host
    // `~/.bashrc` is never sourced there, so there is nothing to neutralize and
    // a host-path rcfile would not resolve in-box. Honor neither default here,
    // and do not carry the host `$SHELL` in either: it names a host binary.
    if policy.claim.image_backed() {
        if policy.profile.shell_rcfile.is_some() {
            eprintln!(
                "   note: [shell] rcfile is ignored at isolation={} \
                 (the shell rc comes from the image)",
                policy.claim.as_str()
            );
        }
        return Ok(ShellLaunch::argv(box_shell_argv()));
    }

    // The custom rcfile, when the profile pins one. Both shells we generate an rc
    // for can honour it — bash directly (`--rcfile`), zsh by sourcing it from the
    // generated `$ZDOTDIR/.zshrc`, since zsh has no equivalent flag.
    let custom_rc = match policy.profile.shell_rcfile.clone() {
        Some(rc) if is_bash || is_zsh => Some(resolve_work_rcfile(work, &rc)?),
        Some(_) => {
            eprintln!(
                "   note: [shell] rcfile only applies to bash and zsh; $SHELL is '{shell}' \
                 — ignoring"
            );
            return Ok(bare);
        }
        None => None,
    };

    if is_zsh {
        let z = write_plain_zshrc(h5i_root, m, custom_rc.as_deref())?;
        // Kernel tiers enforce a Landlock read allowlist. The generated rc dir is
        // read-only — it is the host's word about how the session starts, and the
        // box must not be able to rewrite its own next startup — while the history
        // dir is granted write: zsh creates a lock file beside `$HISTFILE`, so the
        // grant has to be the directory, not the file. (Workspace is unconfined;
        // image-backed tiers returned above.)
        if matches!(
            policy.claim,
            IsolationClaim::Process | IsolationClaim::Supervised
        ) {
            policy.profile.fs_read.push(z.zdotdir.clone());
            policy.profile.fs_write.push(z.histdir.clone());
        }
        return Ok(ShellLaunch {
            argv: vec![shell, "-i".into()],
            // zsh resolves `$ZDOTDIR` before it sources anything, so this has to
            // arrive in the environment — an rc-file assignment would be too late.
            env: vec![("ZDOTDIR".to_string(), z.zdotdir)],
        });
    }

    if !is_bash {
        // We only know how to inject a plain rc for bash and zsh; other shells
        // keep their normal startup (they source their own host files).
        return Ok(bare);
    }

    if let Some(rcpath) = custom_rc {
        return Ok(ShellLaunch::argv(vec![
            shell,
            "--rcfile".into(),
            rcpath,
            "-i".into(),
        ]));
    }

    let rcpath = write_plain_bashrc(h5i_root, m)?;
    // Kernel tiers enforce a Landlock read allowlist: grant the generated rc so
    // bash can read it. (Workspace is unconfined; container returned above.)
    if matches!(
        policy.claim,
        IsolationClaim::Process | IsolationClaim::Supervised
    ) {
        policy.profile.fs_read.push(rcpath.clone());
    }
    Ok(ShellLaunch::argv(vec![
        shell,
        "--rcfile".into(),
        rcpath,
        "-i".into(),
    ]))
}

/// The default interactive shell **inside an image-backed box** (container,
/// microVM).
///
/// `$SHELL` names a *host* binary: on a stock macOS it is `/bin/zsh`, which the
/// box's Linux rootfs does not carry — passing it in ends the session at exec
/// before the first prompt (`/.msb/scripts/h5i-env: exec: /bin/zsh: not found`).
/// The shell has to come from the image, exactly like the rc does, and the image
/// is the only thing that knows which shells it has. So ask it at start-up
/// rather than guess host-side: prefer `bash` (both shipped agent images carry
/// it, `containers/entrypoint.sh` falls back to it, and the container tier's
/// observation shim shadows it), and fall back to the one shell every image is
/// guaranteed to have — the same `/bin/sh` this probe is already running in.
///
/// Both arms `exec`, so the probing `sh` is *replaced* rather than left behind
/// as a parent: the TTY, the signals and the exit code reach the real shell
/// exactly as if it had been launched directly.
fn box_shell_argv() -> Vec<String> {
    vec![
        "/bin/sh".into(),
        "-c".into(),
        "if command -v bash >/dev/null 2>&1; then exec bash -i; else exec /bin/sh -i; fi".into(),
    ]
}

/// Resolve a profile `[shell] rcfile` (relative to `$WORK`) to an absolute path,
/// fail-closed: it must stay inside the worktree (no absolute paths, no `..`
/// escape) and must exist. Keeps the rc inside the one always-mounted, granted
/// subtree so it resolves in the box on every tier.
fn resolve_work_rcfile(work: &Path, rel: &str) -> Result<String, H5iError> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(H5iError::Metadata(format!(
            "[shell] rcfile '{rel}' must be relative to the worktree, not an absolute path"
        )));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(H5iError::Metadata(format!(
            "[shell] rcfile '{rel}' must not escape the worktree with '..'"
        )));
    }
    let full = work.join(p);
    if !full.is_file() {
        return Err(H5iError::Metadata(format!(
            "[shell] rcfile '{rel}' not found in the worktree (expected at {})",
            full.display()
        )));
    }
    Ok(full.display().to_string())
}

/// Write the generated plain bash rcfile into the env's private dir and return
/// its absolute path. Idempotent (rewritten each session so a re-`create` or an
/// edited env id stays in sync).
fn write_plain_bashrc(h5i_root: &Path, m: &EnvManifest) -> Result<String, H5iError> {
    let dir = m.dir(h5i_root).join("shell");
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = dir.join("rc.bash");
    // The env id can contain '/' (agent/slug); harmless inside single quotes.
    let body = format!(
        "# Generated by `h5i box shell` — a plain default rc.\n\
         # The host ~/.bashrc is intentionally NOT sourced inside the confined box\n\
         # (it tends to reference tools the sandbox blocks, e.g. powerline-shell).\n\
         # To customize: set `[shell] rcfile = \"…\"` (relative to the worktree) in\n\
         # .h5i/env.toml, or drop extra shell config in ~/.h5i_envrc (sourced below).\n\
         PS1='h5i:{id} \\w \\$ '\n\
         alias ll='ls -alF'\n\
         alias la='ls -A'\n\
         alias ls='ls --color=auto' 2>/dev/null\n\
         [ -f \"$HOME/.h5i_envrc\" ] && . \"$HOME/.h5i_envrc\"\n",
        id = m.id,
    );
    std::fs::write(&path, body).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(path.display().to_string())
}

/// Single-quote a value for a POSIX shell, `'` included (`'` → `'\''`).
///
/// The bash rc only ever interpolates the env id, which cannot carry a quote.
/// The zsh rc interpolates *paths* — the env dir, and a profile-pinned rcfile —
/// and a path may contain anything a filesystem allows. Unquoted, a `'` in one
/// ends the string and the rest of the line is read as code.
fn sq(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// The two paths a generated zsh startup needs: the `$ZDOTDIR` the rc lives in
/// (granted read) and the directory `$HISTFILE` lives in (granted write).
struct ZshDirs {
    zdotdir: String,
    histdir: String,
}

/// Write the generated plain zsh rc into the env's private dir and return the
/// dirs to point `$ZDOTDIR` at and to grant. Idempotent, like the bash one.
///
/// Two problems are solved by the one mechanism, because zsh gives us only one:
///
///  1. **History.** zsh's default `$HISTFILE` is `${ZDOTDIR:-$HOME}/.zsh_history`
///     — the operator's real history file, which no box grants (nor should: it is
///     a log of everything they have ever typed on the host). zsh does not treat
///     that as fatal, but it *does* announce it at startup and again after every
///     command — `zsh: locking failed for ~/.zsh_history: operation not
///     permitted` — which buries the actual output of the session.
///  2. **The host `~/.zshrc`**, for the same reason bash's is skipped: under
///     confinement a real one (oh-my-zsh, a prompt framework, version managers)
///     reaches for tools and cache dirs the sandbox blocks, and the failures land
///     on the same line as the prompt.
///
/// Setting `$ZDOTDIR` moves both: zsh reads `$ZDOTDIR/.zshenv` and
/// `$ZDOTDIR/.zshrc` instead of the host's, and macOS's `/etc/zshrc` — which is
/// still sourced, and which is what sets `$HISTFILE` in the first place — points
/// at the new dir on its own. The generated rc then sets `$HISTFILE` explicitly
/// anyway, so the history lands in the writable dir on hosts whose global rc
/// says nothing about it.
///
/// The rc dir stays read-only and the history dir is separate: the box gets a
/// per-env, persistent shell history, and still cannot rewrite the rc that starts
/// its next session.
fn write_plain_zshrc(
    h5i_root: &Path,
    m: &EnvManifest,
    custom_rc: Option<&str>,
) -> Result<ZshDirs, H5iError> {
    let root = m.dir(h5i_root).join("shell");
    let zdotdir = root.join("zdotdir");
    let histdir = root.join("history");
    for d in [&zdotdir, &histdir] {
        std::fs::create_dir_all(d).map_err(|e| H5iError::with_path(e, d))?;
    }
    let histfile = histdir.join("zsh_history");
    // Sourced last, so it wins over the plain defaults above it — same order the
    // bash rc gives `~/.h5i_envrc`.
    let custom = match custom_rc {
        Some(rc) => format!("source {}\n", sq(rc)),
        None => String::new(),
    };
    let body = format!(
        "# Generated by `h5i box shell` — a plain default rc.\n\
         # The host ~/.zshrc is intentionally NOT sourced inside the confined box\n\
         # (it tends to reference tools the sandbox blocks), and the host history\n\
         # file is outside every grant — hence this $ZDOTDIR.\n\
         # To customize: set `[shell] rcfile = \"…\"` (relative to the worktree) in\n\
         # .h5i/env.toml, or drop extra shell config in ~/.h5i_envrc (sourced below).\n\
         HISTFILE={histfile}\n\
         HISTSIZE=2000\n\
         SAVEHIST=1000\n\
         # zsh renices background jobs by default and the box cannot call\n\
         # setpriority(2), so every `cmd &` would answer `zsh: nice(5) failed:\n\
         # operation not permitted`. The renice buys the session nothing.\n\
         unsetopt bgnice\n\
         PROMPT='h5i:{id} %~ %# '\n\
         alias ll='ls -alF'\n\
         alias la='ls -A'\n\
         [ -f \"$HOME/.h5i_envrc\" ] && . \"$HOME/.h5i_envrc\"\n\
         {custom}",
        histfile = sq(&histfile.display().to_string()),
        id = m.id,
    );
    let path = zdotdir.join(".zshrc");
    std::fs::write(&path, body).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(ZshDirs {
        zdotdir: zdotdir.display().to_string(),
        histdir: histdir.display().to_string(),
    })
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
        let input = crate::receipt::RecordInput {
            env_id: m.id.clone(),
            policy_digest: Some(m.policy_digest.clone()),
            source: "tee-shim".into(),
            cmd: Some(safe_cmd.clone()),
            cwd: Some(work.display().to_string()),
            exit_code,
            git_tree: head_tree.clone(),
            ..Default::default()
        };
        let captured = crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, &raw)?;
        m.captures.push(captured.id.clone());
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
                capture: Some(captured.id.clone()),
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
        let files: Vec<String> = meta.files.into_iter().take(64).collect();
        let input = crate::receipt::RecordInput {
            env_id: m.id.clone(),
            policy_digest: Some(m.policy_digest.clone()),
            source: "inbox-capture".into(),
            cmd: Some(safe_cmd.clone()),
            cwd: meta.cwd,
            exit_code: meta.exit_code,
            git_tree: head_tree.clone(),
            files,
            ..Default::default()
        };
        let captured = crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, &raw)?;
        m.captures.push(captured.id.clone());
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
                capture: Some(captured.id.clone()),
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

    // Captures ingested above only live in this mutable manifest until the
    // caller's final status write. Team submission ingest reloads the env
    // manifest, so persist first or the submission misses same-spool evidence.
    save_manifest(h5i_root, m)?;
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
    // h5i's own private-path redirects are not the agent's work. On macOS they
    // are symlinks in the worktree (no bind mounts), so without this the patch
    // carries a `120000` entry pointing at h5i's per-env storage — and this
    // patch is what `export` writes for a human to `git apply`.
    let private_rels = private_path_rels(h5i_root, m);
    let render = |diff: git2::Diff| -> Result<String, H5iError> {
        if stat_only {
            let stats = diff.stats()?;
            let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
            return Ok(buf.as_str().unwrap_or("").to_string());
        }
        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |d, _h, line| {
            if delta_is_private(&d, &private_rels) {
                return true; // skip this delta, keep walking
            }
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

#[derive(Debug, Clone, Serialize)]
pub struct DiffStatFile {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffStatReport {
    pub files: Vec<DiffStatFile>,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Structured diffstat for the env's changes against its pinned base.
pub fn diffstat_report(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
) -> Result<DiffStatReport, H5iError> {
    // As in [`diff`]: h5i's private-path redirects are h5i artifacts, so they
    // are neither listed nor counted. Totals are summed from the surviving
    // files rather than taken from `diff.stats()`, which counts every delta.
    let private_rels = private_path_rels(h5i_root, m);
    let render = |diff: git2::Diff| -> Result<DiffStatReport, H5iError> {
        let mut files = Vec::new();
        let delta_count = diff.deltas().len();
        for idx in 0..delta_count {
            let Some(delta) = diff.get_delta(idx) else {
                continue;
            };
            if delta_is_private(&delta, &private_rels) {
                continue;
            }
            let (_, insertions, deletions) = git2::Patch::from_diff(&diff, idx)?
                .map(|patch| patch.line_stats())
                .transpose()?
                .unwrap_or((0, 0, 0));
            let path = if matches!(delta.status(), git2::Delta::Deleted) {
                delta.old_file().path().or_else(|| delta.new_file().path())
            } else {
                delta.new_file().path().or_else(|| delta.old_file().path())
            }
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
            files.push(DiffStatFile {
                path,
                insertions,
                deletions,
            });
        }
        Ok(DiffStatReport {
            files_changed: files.len(),
            insertions: files.iter().map(|f| f.insertions).sum(),
            deletions: files.iter().map(|f| f.deletions).sum(),
            files,
        })
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
    /// A detached box: its code came from outside this repository, so there is
    /// no parent here that could drift.
    Detached,
}

impl Drift {
    pub fn is_current(&self) -> bool {
        // A detached box has nothing to drift from, so it is never stale.
        matches!(self, Drift::UpToDate | Drift::Detached)
    }
    /// Stable machine kind — the string clients filter/badge on.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Drift::UpToDate => "up-to-date",
            Drift::ParentAhead { .. } => "parent-ahead",
            Drift::Diverged { .. } => "diverged",
            Drift::ParentGone => "parent-gone",
            Drift::Detached => "detached",
        }
    }
    /// One-line human summary.
    pub fn summary(&self) -> String {
        match self {
            Drift::UpToDate => "up to date with parent".into(),
            Drift::ParentAhead { commits, tip } => format!(
                "parent advanced {commits} commit{} (now {}) — `h5i box rebase` to refresh the base",
                if *commits == 1 { "" } else { "s" },
                &tip[..12.min(tip.len())]
            ),
            Drift::Diverged { tip } => format!(
                "parent diverged from the base (now {}) — `h5i box rebase` will 3-way merge",
                &tip[..12.min(tip.len())]
            ),
            Drift::ParentGone => "parent branch is gone".into(),
            Drift::Detached => "detached box — no parent in this repository".into(),
        }
    }
}

/// Compute how `m`'s pinned base relates to its parent branch's current tip.
pub fn drift(repo: &Repository, m: &EnvManifest) -> Drift {
    if is_detached(m) {
        return Drift::Detached;
    }
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
    // Reconcile the durable status against the live registry: a `running`
    // manifest with no live writer is a crash leftover, and saying so beats
    // letting the reader trust it.
    let live = live_sessions(&m.dir(h5i_root));
    let has_writer = live.iter().any(|s| live_is_writer(&s.kind));
    let stale_note = if m.status == ST_RUNNING && !has_writer {
        "  (stale — no live session holds this env; the writer likely crashed)"
    } else {
        ""
    };
    out.push_str(&format!("  status   : {}{}\n", m.status, stale_note));
    for s in &live {
        out.push_str(&format!(
            "  live     : {} pid {} since {}{}\n",
            s.kind,
            s.pid,
            s.started_at,
            s.command
                .as_ref()
                .map(|c| format!(" — {c}"))
                .unwrap_or_default()
        ));
    }
    out.push_str(&format!("  agent    : {}\n", m.agent));
    if is_detached(m) {
        // Say it plainly: this box's code came from outside, and nothing it
        // does can reach the repository you are standing in.
        out.push_str(&format!(
            "  source   : {} (detached — this repository is not involved)\n",
            crate::redact::sanitize_display(&m.source)
        ));
    }
    out.push_str(&format!(
        "  base     : {} (from {})\n",
        &m.base_commit[..12.min(m.base_commit.len())],
        m.parent_branch
    ));
    out.push_str(&format!("  branch   : {}\n", m.branch));
    out.push_str(&format!(
        "  policy   : profile={} isolation={} digest={}\n",
        m.profile,
        m.isolation_claim,
        &m.policy_digest[..12.min(m.policy_digest.len())]
    ));
    // Resolved policy details when readable (digest-verified).
    if let Ok(policy) = load_policy(h5i_root, m) {
        let p = &policy.profile;
        // `enforce:` is a claim about the host, so a cap this tier cannot apply
        // here is marked rather than printed as though it held. Darwin applies
        // neither `mem` nor `procs` at the kernel tiers; saying "mem=8192MiB"
        // unqualified there reads as a containment property that does not exist.
        let limits = crate::sandbox::limit_support(policy.claim);
        let unenforced = |on: bool| if on { "" } else { "*" };
        out.push_str(&format!(
            "  enforce  : net.mode={:?} fs.write={:?} mem={}{} procs={}{} wall={}s{}{}\n",
            p.net_mode,
            p.fs_write,
            p.mem_bytes
                .map(|b| format!("{}MiB", b / 1024 / 1024))
                .unwrap_or_else(|| "∞".into()),
            unenforced(limits.mem || p.mem_bytes.is_none()),
            p.max_procs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "∞".into()),
            unenforced(limits.procs || p.max_procs.is_none()),
            p.wall_secs,
            p.fsize_bytes
                .map(|b| format!(" fsize={}MiB", b / 1024 / 1024))
                .unwrap_or_default(),
            p.cpu_secs.map(|s| format!(" cpu={s}s")).unwrap_or_default(),
        ));
        // One footnote beats a caveat on every number, and it names the reason
        // rather than leaving the reader to infer it from a marker.
        let declared_unenforced = (!limits.mem && p.mem_bytes.is_some())
            || (!limits.procs && p.max_procs.is_some());
        if declared_unenforced {
            out.push_str(&format!(
                "             * declared, NOT enforced at the {} tier on this host{}\n",
                policy.claim.as_str(),
                if cfg!(target_os = "macos") && policy.claim < IsolationClaim::Container {
                    " (Darwin has no cgroups; use isolation=container or microvm for a real cap)"
                } else {
                    ""
                }
            ));
        }
        if !p.tools.is_empty() {
            out.push_str(&format!("  tools    : {}\n", p.tools.join(", ")));
        }
        // Named, not implied: these are held out of `diff`, `propose` and the
        // exported patch, and a reviewer should see that from the status page
        // rather than deduce it from an absence.
        if !p.private_paths.is_empty() {
            out.push_str(&format!(
                "  private  : {} (per-env; excluded from diff/export)\n",
                p.private_paths
                    .iter()
                    .map(|pp| pp.path.trim_matches('/'))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !p.net_egress.is_empty() {
            out.push_str(&format!("  egress   : {}", p.net_egress.join(", ")));
            if policy.claim.enforces_egress_allowlist() {
                let extras: Vec<String> = user_allow_list()
                    .into_iter()
                    .filter(|u| !p.net_egress.iter().any(|e| e.trim().eq_ignore_ascii_case(u)))
                    .collect();
                if !extras.is_empty() {
                    out.push_str(&format!("  (+ h5i box allow: {})", extras.join(", ")));
                }
            }
            out.push('\n');
        }
    }
    let evidence_detail = if m.captures.is_empty() {
        String::new()
    } else {
        let sources = evidence_sources_by_lane(h5i_root, m)
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
    }
    let d = drift(repo, m);
    let marker = if d.is_current() { "✓" } else { "⚠" };
    out.push_str(&format!("  drift    : {marker} {}\n", d.summary()));
    out
}

// ─── doctor (enforcement-readiness, Idea 0) ──────────────────────────────────

/// One readiness check in a [`DoctorReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Short check name (`policy`, `enforcement`, `workspace`, …).
    pub name: String,
    /// `true` — green; `false` — a problem the reviewer should see.
    pub ok: bool,
    /// `true` when a `!ok` result is advisory (e.g. a pulled env with no
    /// workspace), not a hard fault — rendered `⚠` and kept out of `healthy`.
    #[serde(default)]
    pub warn: bool,
    /// Human detail.
    pub detail: String,
}

/// Per-env enforcement-readiness + structural-health report (`h5i box doctor`).
/// Answers "can this env actually enforce its isolation claim *here*, and is it
/// structurally intact?" — the per-env home for the functional `verify_exec`
/// self-test (bits present ≠ confinement can exec).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub env_id: String,
    pub isolation_claim: String,
    pub checks: Vec<DoctorCheck>,
    /// `true` when no non-advisory check failed.
    pub healthy: bool,
}

/// Run all readiness checks for one env. Read-only: probes host capabilities and
/// inspects refs/disk, never mutates the env.
pub fn doctor(repo: &Repository, h5i_root: &Path, m: &EnvManifest) -> DoctorReport {
    let mut checks: Vec<DoctorCheck> = Vec::new();
    macro_rules! chk {
        ($n:expr, $ok:expr, $w:expr, $d:expr) => {
            checks.push(DoctorCheck {
                name: $n.into(),
                ok: $ok,
                warn: $w,
                detail: $d,
            })
        };
    }

    // 1. Policy integrity — on-disk policy still matches the pinned digest.
    match load_policy(h5i_root, m) {
        Ok(_) => chk!(
            "policy",
            true,
            false,
            format!(
                "policy.resolved.toml verifies against pinned digest {}",
                &m.policy_digest[..12.min(m.policy_digest.len())]
            )
        ),
        Err(e) => chk!("policy", false, false, format!("{e}")),
    }

    // 2. Enforcement readiness — can the host actually run this claim?
    match IsolationClaim::parse(&m.isolation_claim) {
        Ok(claim) => {
            let caps = sandbox::probe_host();
            match claim {
                IsolationClaim::Workspace => chk!(
                    "enforcement",
                    true,
                    false,
                    "workspace tier needs no kernel confinement".into()
                ),
                IsolationClaim::Microvm => match caps.microvm_runtime.as_deref() {
                    Some(rt) => chk!(
                        "enforcement",
                        true,
                        false,
                        format!("microVM runtime present ({rt}) and the host can virtualize")
                    ),
                    // Name the missing half. "Install msb" and "enable nested
                    // virtualization" are different problems with different fixes.
                    None => chk!(
                        "enforcement",
                        false,
                        false,
                        sandbox::microvm_unavailable_detail()
                    ),
                },
                IsolationClaim::Container | IsolationClaim::HardenedContainer => {
                    if let Some(rt) = caps.container_runtime.as_deref() {
                        chk!(
                            "enforcement",
                            true,
                            false,
                            format!("rootless container runtime present ({rt})")
                        );
                    } else {
                        chk!(
                            "enforcement",
                            false,
                            false,
                            "no rootless container runtime (podman) on host".into()
                        );
                    }
                }
                // process / supervised: the bits can be present while a hardened
                // kernel still denies exec — functional self-test is authoritative.
                _ => {
                    let probe = sandbox::Profile::builtin("doctor", claim);
                    match sandbox::resolve(&probe, &caps).and_then(|pol| sandbox::verify_exec(&pol))
                    {
                        Ok(()) => chk!(
                            "enforcement",
                            true,
                            false,
                            format!("{} tier functionally runnable here", claim.as_str())
                        ),
                        Err(e) => chk!(
                            "enforcement",
                            false,
                            false,
                            format!("{} tier NOT runnable here: {e}", claim.as_str())
                        ),
                    }
                }
            }
        }
        Err(e) => chk!(
            "enforcement",
            false,
            false,
            format!("unknown isolation claim: {e}")
        ),
    }

    // 3. Workspace — present for live envs, advisory-absent for pulled/gc'd ones.
    if has_workspace(m, h5i_root) {
        chk!("workspace", true, false, "git worktree present".into());
    } else {
        chk!(
            "workspace",
            false,
            true,
            "no work/ dir (pulled or gc'd env) — diff/apply fall back to the branch tip".into()
        );
    }

    // 4. Code branch present.
    match repo.find_reference(&m.branch) {
        Ok(_) => chk!("code-branch", true, false, m.branch_short().to_string()),
        Err(_) => chk!(
            "code-branch",
            false,
            false,
            format!("code branch {} is missing", m.branch_short())
        ),
    }

    // 5. Base drift vs parent.
    let d = drift(repo, m);
    match &d {
        Drift::UpToDate => chk!("base-drift", true, false, d.summary()),
        Drift::ParentGone => chk!("base-drift", false, true, d.summary()),
        _ => chk!("base-drift", true, true, d.summary()),
    }

    // 7. Evidence captures recorded (informational).
    chk!(
        "evidence",
        true,
        false,
        format!(
            "{} capture{} recorded",
            m.captures.len(),
            if m.captures.len() == 1 { "" } else { "s" }
        )
    );

    let healthy = checks.iter().all(|c| c.ok || c.warn);
    DoctorReport {
        env_id: m.id.clone(),
        isolation_claim: m.isolation_claim.clone(),
        checks,
        healthy,
    }
}

// ─── secrets legibility (Idea 1) ─────────────────────────────────────────────

/// Dry-run status of one declared secret grant — config + whether it currently
/// resolves, **never the value** (only a fingerprint when resolvable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretStatus {
    pub name: String,
    pub source: String,
    pub inject: String,
    pub ttl: Option<String>,
    /// `ok` | `command (not evaluated)` | `error: …`.
    pub status: String,
    /// `sha256:<12>` when resolvable (env:/file:), else `None`.
    pub fingerprint: Option<String>,
}

/// Resolve each declared grant's *status* without injecting it — the read-only
/// surface behind `h5i box secrets`. `command:` extractors are never executed
/// here (they have host-side side effects); they show as "not evaluated".
pub fn secrets_status(policy: &ResolvedPolicy) -> Vec<SecretStatus> {
    policy
        .profile
        .secret_grants
        .iter()
        .map(|g| {
            let source = g.source_or_default();
            let inject = g.inject_or_default().to_string();
            let (status, fingerprint) = if source.starts_with("command:") {
                ("command (not evaluated)".to_string(), None)
            } else {
                // Dry-run resolution: read-only, value used only for a
                // fingerprint and immediately dropped, never surfaced.
                match crate::secrets_broker::resolve_value(g, false) {
                    Ok(v) => (
                        "ok".to_string(),
                        Some(crate::secrets_broker::fingerprint(&v)),
                    ),
                    Err(e) => (format!("error: {e}"), None),
                }
            };
            SecretStatus {
                name: g.name.clone(),
                source,
                inject,
                ttl: g.ttl.clone(),
                status,
                fingerprint,
            }
        })
        .collect()
}

/// Plain-text rendering of [`secrets_status`].
pub fn render_secrets(env_id: &str, rows: &[SecretStatus]) -> String {
    let mut out = String::new();
    out.push_str(&format!("── secrets for {env_id} ──\n"));
    if rows.is_empty() {
        out.push_str("  (no secret grants declared in this env's profile)\n");
        return out;
    }
    for s in rows {
        let ttl = s
            .ttl
            .as_deref()
            .map(|t| format!(" ttl={t}"))
            .unwrap_or_default();
        let fp = s
            .fingerprint
            .as_deref()
            .map(|f| format!("  {f}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<20} source={} inject={}{}  [{}]{}\n",
            s.name, s.source, s.inject, ttl, s.status, fp
        ));
    }
    out
}

// ─── services (Idea 3.5) + dynamic ports (Idea 2) ────────────────────────────

fn default_logs() -> bool {
    true
}

/// A long-lived service declared in the env's `.h5i/env.toml`
/// (`[service.web] command = "npm run dev" port = 3000`). The command runs
/// inside the env's sandbox via `sh -c`; `port`, when set, gets a per-env dynamic
/// host port allocated and injected at start (Idea 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    pub command: String,
    /// Declared in-box port the service binds. Presence triggers dynamic-port
    /// allocation + injection; the service is expected to honor `PORT` /
    /// `H5I_ENV_PORT_<NAME>`.
    #[serde(default)]
    pub port: Option<u16>,
    /// Advisory in v1 (no auto-restart yet).
    #[serde(default)]
    pub restart: Option<String>,
    /// Capture the service log as an h5i object on stop (default true).
    #[serde(default = "default_logs")]
    pub logs: bool,
}

#[derive(Debug, Deserialize)]
struct ServiceFileToml {
    #[serde(default)]
    service: std::collections::BTreeMap<String, ServiceDef>,
}

// ─── live-session registry (the env control-plane groundwork) ───────────────

/// One live `env run` / `env shell` session's on-disk record — the daemon-free
/// registry under `.git/.h5i/env/<agent>/<slug>/live/<pid>.json`, mirroring
/// the `services/` pid-registry pattern. Written by the session holding the
/// run/observer lock; removed on clean exit; a crash leaves the file and the
/// reader reconciles by PID identity (`pid_alive`), not timestamps.
///
/// **Informational only, never authoritative for security:** grants derive
/// exclusively from the identity-validated manifest + digested policy; the
/// registry exists so `env list`/`status`/the dashboard can tell a live
/// session from a stale `running` status (a SIGKILLed session never resets
/// its manifest status). The `live/` dir is host state — it is not part of
/// the box's fs grants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub pid: u32,
    /// Session kind: `run` (captured exec), `shell` (read-write interactive),
    /// or `observe` (read-only observer).
    pub kind: String,
    /// RFC3339 UTC start time (display only — liveness is PID-based).
    pub started_at: String,
    /// What the session is executing (secret-redacted), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Kinds that hold the exclusive writer lock (a live one of these means the
/// env is genuinely busy, not just observed).
pub fn live_is_writer(kind: &str) -> bool {
    matches!(kind, "run" | "shell")
}

/// RAII registration of the calling process in an env's live registry.
/// Best-effort on both ends: failing to write never blocks a session, and
/// `Drop` removal failing just leaves a record the next reader reconciles.
struct LiveGuard {
    path: PathBuf,
}

impl LiveGuard {
    fn register(env_dir: &Path, kind: &str, command: Option<String>) -> LiveGuard {
        let dir = env_dir.join(LIVE_DIR);
        let _ = std::fs::create_dir_all(&dir);
        let pid = std::process::id();
        let path = dir.join(format!("{pid}.json"));
        let rec = LiveSession {
            pid,
            kind: kind.to_string(),
            started_at: now_ts(),
            command,
        };
        if let Ok(json) = serde_json::to_string(&rec) {
            let _ = std::fs::write(&path, json);
        }
        LiveGuard { path }
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The env's live sessions: scan `live/`, keep records whose PID is alive,
/// and best-effort unlink crash leftovers (dead PIDs, unparseable files).
/// PID-identity staleness — same trade-off as the services registry (a reused
/// PID can briefly read as alive; the next scan after it exits heals it).
pub fn live_sessions(env_dir: &Path) -> Vec<LiveSession> {
    let dir = env_dir.join(LIVE_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&p)
            .ok()
            .and_then(|text| serde_json::from_str::<LiveSession>(&text).ok());
        match parsed {
            Some(rec) if pid_alive(rec.pid) => out.push(rec),
            _ => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.pid.cmp(&b.pid)));
    out
}

/// A running service's on-disk record — the daemon-free pid registry under
/// `.git/.h5i/env/<agent>/<slug>/services/<name>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub name: String,
    pub pid: u32,
    pub command: String,
    pub started_at: String,
    pub port: Option<u16>,
    /// Allocated per-env host port, injected as `H5I_ENV_PORT_<NAME>` (Idea 2).
    pub dynamic_port: Option<u16>,
    pub log: String,
}

/// A service's record plus liveness — for `env service status` / `env ports`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    #[serde(flatten)]
    pub record: ServiceRecord,
    pub alive: bool,
}

fn services_dir(h5i_root: &Path, m: &EnvManifest) -> PathBuf {
    m.dir(h5i_root).join("services")
}

/// Whether `pid` is still alive (signal 0 probe).
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Allocate a free loopback TCP port by binding `:0` and reading it back.
fn alloc_free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// `web-test` → `WEB_TEST` (an env-var-safe upper-case key).
fn env_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Parse the `[service.*]` table from one `.h5i/env.toml`. Empty when the file
/// is absent or declares no services. Every service name is validated
/// fail-closed (it becomes an env-local path component) so a traversing key like
/// `../manifest` is rejected at parse/pin time, not turned into a path.
fn parse_services_file(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, ServiceDef>, H5iError> {
    if !path.is_file() {
        return Ok(std::collections::BTreeMap::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| H5iError::with_path(e, path))?;
    let parsed: ServiceFileToml = toml::from_str(&text)?;
    for name in parsed.service.keys() {
        validate_service_name(name)?;
    }
    Ok(parsed.service)
}

/// sha256 over the canonical (sorted, re-serialized) service manifest — stable
/// regardless of on-disk formatting, so the pin compares by content.
fn service_defs_digest(defs: &std::collections::BTreeMap<String, ServiceDef>) -> String {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_string(defs).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(json.as_bytes());
    format!("{:x}", h.finalize())
}

/// Env-local pinned service manifest (immutable from the box — under
/// `.git/.h5i`, never in `$WORK` or the box_git grants).
fn pinned_services_path(h5i_root: &Path, m: &EnvManifest) -> PathBuf {
    m.dir(h5i_root).join("services.json")
}

/// Snapshot the base worktree's `[service.*]` into the env-local pinned manifest
/// at create, returning the digest to record in the manifest (review #1: service
/// declarations must be policy-pinned, not read from mutable workspace content).
///
/// ALWAYS writes `services.json` and records a digest — even for the empty set —
/// so a new env with no services is still *pinned-empty*, not mistaken for a
/// legacy (pre-pinning) env. Without this, a no-service env would record a `None`
/// digest and fall back to reading the mutable worktree config, letting an agent
/// add `[service.*]` after create and start it unpinned. Fail-closed on a
/// malformed services section.
fn pin_services_at_create(work_path: &Path, env_dir: &Path) -> Result<String, H5iError> {
    let defs = parse_services_file(&work_path.join(".h5i/env.toml"))?;
    let json = serde_json::to_string_pretty(&defs)?;
    let path = env_dir.join("services.json");
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(service_defs_digest(&defs))
}

/// Load the env's service declarations from the **pinned** env-local manifest,
/// verifying its content digest against the one recorded at create — so an agent
/// editing the (writable) worktree `.h5i/env.toml` after create can't change
/// which long-lived command a service runs. Falls back to the worktree/repo
/// config only for envs created before pinning existed (no recorded digest).
fn load_service_defs(
    h5i_root: &Path,
    m: &EnvManifest,
) -> Result<std::collections::BTreeMap<String, ServiceDef>, H5iError> {
    let pinned = pinned_services_path(h5i_root, m);
    if pinned.is_file() {
        let text = std::fs::read_to_string(&pinned).map_err(|e| H5iError::with_path(e, &pinned))?;
        let defs: std::collections::BTreeMap<String, ServiceDef> = serde_json::from_str(&text)?;
        if let Some(expected) = &m.service_digest {
            let got = service_defs_digest(&defs);
            if &got != expected {
                return Err(H5iError::Metadata(format!(
                    "pinned service manifest for {} does not match its recorded digest \
                     (expected {expected}, found {got}) — refusing to start a service under a \
                     tampered manifest (fail-closed)",
                    m.id
                )));
            }
        }
        return Ok(defs);
    }
    // Back-compat: an env created before service pinning has no pinned file or
    // recorded digest. Fall back to the worktree (then repo-root) config.
    if m.service_digest.is_none() {
        for path in [
            m.work_dir(h5i_root).join(".h5i/env.toml"),
            h5i_root
                .parent()
                .and_then(|p| p.parent())
                .map(|w| w.join(".h5i/env.toml"))
                .unwrap_or_default(),
        ] {
            let defs = parse_services_file(&path)?;
            if !defs.is_empty() {
                return Ok(defs);
            }
        }
    }
    Ok(std::collections::BTreeMap::new())
}

fn service_record_path(svc_dir: &Path, name: &str) -> PathBuf {
    svc_dir.join(format!("{name}.json"))
}

fn read_service_record(svc_dir: &Path, name: &str) -> Option<ServiceRecord> {
    let text = std::fs::read_to_string(service_record_path(svc_dir, name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Start service `name` as a confined background process. Allocates + injects a
/// dynamic host port when the def declares one. Fail-closed: refuses if the
/// service is already running or the env has no workspace.
pub fn service_start(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
) -> Result<ServiceRecord, H5iError> {
    validate_service_name(name)?;
    let defs = load_service_defs(h5i_root, m)?;
    let def = defs.get(name).ok_or_else(|| {
        H5iError::Metadata(format!(
            "no service '{name}' declared in .h5i/env.toml ([service.{name}])"
        ))
    })?;
    let svc_dir = services_dir(h5i_root, m);
    std::fs::create_dir_all(&svc_dir).map_err(|e| H5iError::with_path(e, &svc_dir))?;
    if let Some(rec) = read_service_record(&svc_dir, name) {
        if pid_alive(rec.pid) {
            return Err(H5iError::Metadata(format!(
                "service '{name}' is already running (pid {}) — stop it first",
                rec.pid
            )));
        }
    }
    let work = m.work_dir(h5i_root);
    if !work.is_dir() {
        return Err(H5iError::Metadata(
            "env has no workspace (pulled or gc'd) — cannot start a service".into(),
        ));
    }
    let mut policy = load_policy(h5i_root, m)?;
    grant_box_git(repo, m, &work, &mut policy, false)?;
    prepare_private_paths(h5i_root, m, &mut policy, &work)?;
    prepare_private_tmp(h5i_root, m, &mut policy, None)?;
    prepare_home_state(
        h5i_root,
        m,
        &mut policy,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        None,
    )?;

    let mut injected: Vec<(String, String)> = Vec::new();
    let dynamic_port = if def.port.is_some() {
        let p = alloc_free_port().ok_or_else(|| {
            H5iError::Metadata("could not allocate a free host port for the service".into())
        })?;
        let key = env_key(name);
        injected.push((format!("H5I_ENV_PORT_{key}"), p.to_string()));
        injected.push((format!("{key}_DYNAMIC_PORT"), p.to_string()));
        // PORT is the de-facto convention many dev servers read.
        injected.push(("PORT".into(), p.to_string()));
        Some(p)
    } else {
        None
    };

    let log = svc_dir.join(format!("{name}.log"));
    let argv = vec!["sh".to_string(), "-c".to_string(), def.command.clone()];
    let pid = sandbox::spawn_background(&policy, &work, &argv, &injected, &log)?;

    let rec = ServiceRecord {
        name: name.to_string(),
        pid,
        command: def.command.clone(),
        started_at: now_ts(),
        port: def.port,
        dynamic_port,
        log: log.display().to_string(),
    };
    std::fs::write(
        service_record_path(&svc_dir, name),
        serde_json::to_string_pretty(&rec)?,
    )
    .map_err(|e| H5iError::with_path(e, service_record_path(&svc_dir, name)))?;

    let port_note = dynamic_port
        .map(|p| format!(" port={p}"))
        .unwrap_or_default();
    // Record the (redacted) pinned command so a reviewer sees exactly what ran,
    // not just a pid — the command is from the digest-verified pinned manifest.
    let safe_cmd = crate::secrets::redact_text(&def.command);
    append_event(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: "service".into(),
            detail: Some(format!(
                "start {name} pid={pid}{port_note} cmd=`{safe_cmd}`"
            )),
            capture: None,
        },
    )?;
    Ok(rec)
}

/// Stop service `name`: SIGTERM the process group, escalate to SIGKILL, then
/// (if `logs`) ingest the service log as an h5i object capture and record a
/// `service` event with the evidence pointer. Removes the pid record.
pub fn service_stop(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
) -> Result<Option<String>, H5iError> {
    validate_service_name(name)?;
    let svc_dir = services_dir(h5i_root, m);
    let rec = read_service_record(&svc_dir, name).ok_or_else(|| {
        H5iError::Metadata(format!("service '{name}' is not running (no record)"))
    })?;

    #[cfg(unix)]
    {
        let pgid = rec.pid as i32;
        if pid_alive(rec.pid) {
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
            // Brief grace period, then escalate.
            for _ in 0..30 {
                if !pid_alive(rec.pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if pid_alive(rec.pid) {
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
    }

    // Logs-as-capture (Idea 3.5): the service log becomes searchable evidence,
    // tagged to the env + policy digest, secrets redacted. Best-effort.
    let defs = load_service_defs(h5i_root, m).unwrap_or_default();
    let want_logs = defs.get(name).map(|d| d.logs).unwrap_or(true);
    let mut capture_id = None;
    let log_path = PathBuf::from(&rec.log);
    if want_logs && log_path.is_file() {
        if let Ok(raw) = std::fs::read(&log_path) {
            if !raw.is_empty() {
                let work = m.work_dir(h5i_root);
                if let Ok(wt_repo) = Repository::open(&work) {
                    let head_tree = wt_repo
                        .head()
                        .ok()
                        .and_then(|h| h.peel_to_tree().ok())
                        .map(|t| t.id().to_string());
                    let input = crate::receipt::RecordInput {
                        env_id: m.id.clone(),
                        policy_digest: Some(m.policy_digest.clone()),
                        source: "service-log".into(),
                        cmd: Some(format!("service:{name} {}", rec.command)),
                        cwd: Some(work.display().to_string()),
                        git_tree: head_tree,
                        ..Default::default()
                    };
                    if let Ok(c) =
                        crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, &raw)
                    {
                        capture_id = Some(c.id.clone());
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_file(service_record_path(&svc_dir, name));
    let _ = std::fs::remove_file(&log_path);
    append_event(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: "service".into(),
            detail: Some(format!("stop {name} pid={}", rec.pid)),
            capture: capture_id.clone(),
        },
    )?;
    Ok(capture_id)
}

/// Status of every recorded service for this env (record + liveness).
pub fn service_status(h5i_root: &Path, m: &EnvManifest) -> Vec<ServiceStatus> {
    let svc_dir = services_dir(h5i_root, m);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&svc_dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(name) = p.file_stem().and_then(|s| s.to_str()) {
            if let Some(record) = read_service_record(&svc_dir, name) {
                let alive = pid_alive(record.pid);
                out.push(ServiceStatus { record, alive });
            }
        }
    }
    out.sort_by(|a, b| a.record.name.cmp(&b.record.name));
    out
}

/// Tail of a running service's log file.
pub fn service_logs(
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
    tail: usize,
) -> Result<String, H5iError> {
    validate_service_name(name)?;
    let svc_dir = services_dir(h5i_root, m);
    let rec = read_service_record(&svc_dir, name)
        .ok_or_else(|| H5iError::Metadata(format!("service '{name}' is not running")))?;
    let text = std::fs::read_to_string(&rec.log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail);
    Ok(lines[start..].join("\n"))
}

/// Render the fleet of services for `env service status`.
pub fn render_services(env_id: &str, rows: &[ServiceStatus]) -> String {
    let mut out = String::new();
    out.push_str(&format!("── services for {env_id} ──\n"));
    if rows.is_empty() {
        out.push_str("  (no services running; declare [service.<name>] in .h5i/env.toml)\n");
        return out;
    }
    for s in rows {
        let live = if s.alive { "running" } else { "dead" };
        let port = s
            .record
            .dynamic_port
            .map(|p| format!(" injected PORT={p}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<16} {:<8} pid={}{}  `{}`\n",
            s.record.name, live, s.record.pid, port, s.record.command
        ));
    }
    out
}

/// Render the per-env port map for `env ports` (Idea 2). These are **injected**
/// ports: h5i allocates a free host port per service and passes it in as
/// `PORT` / `H5I_ENV_PORT_<NAME>`. There is **no host→box forwarder in v1** — a
/// port is reachable only if the service binds the injected value (the
/// host-port "checkout"/forwarding layer is deferred). The URL is therefore
/// shown as conditional, never a guarantee.
pub fn render_ports(env_id: &str, rows: &[ServiceStatus]) -> String {
    let mut out = String::new();
    out.push_str(&format!("── injected ports for {env_id} ──\n"));
    let with_ports: Vec<&ServiceStatus> = rows
        .iter()
        .filter(|s| s.record.dynamic_port.is_some())
        .collect();
    if with_ports.is_empty() {
        out.push_str("  (no running service has a declared port)\n");
        return out;
    }
    out.push_str(
        "  per-env port injected as PORT / H5I_ENV_PORT_<NAME>; reachable only if the\n  \
         service binds it (no host→box forwarder in v1)\n",
    );
    out.push_str(&format!(
        "  {:<16} {:<10} {:<10} {}\n",
        "SERVICE", "DECLARED", "INJECTED", "URL (if the service binds the injected port)"
    ));
    for s in with_ports {
        let injected = s.record.dynamic_port.unwrap();
        out.push_str(&format!(
            "  {:<16} {:<10} {:<10} http://127.0.0.1:{}\n",
            s.record.name,
            s.record
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".into()),
            injected,
            injected
        ));
    }
    out
}

/// Plain-text rendering of a [`DoctorReport`] (the CLI adds color).
pub fn render_doctor(r: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("── env doctor: {} ──\n", r.env_id));
    out.push_str(&format!("  isolation claim : {}\n", r.isolation_claim));
    for c in &r.checks {
        let mark = if c.ok {
            "✓"
        } else if c.warn {
            "⚠"
        } else {
            "✗"
        };
        out.push_str(&format!("  {mark} {:<15} {}\n", c.name, c.detail));
    }
    let verdict = if r.healthy {
        "healthy"
    } else {
        "UNHEALTHY — resolve the ✗ checks above"
    };
    out.push_str(&format!("  ───\n  verdict: {verdict}\n"));
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
    /// Count of tee-shim observation records (`cmd-*.cmd`).
    shim: usize,
}

impl SpoolPending {
    fn total(&self) -> usize {
        self.captures.len() + self.shim
    }
    /// "2 capture, 3 shim" — omitting zero lanes.
    fn breakdown(&self) -> String {
        let mut parts = Vec::new();
        if !self.captures.is_empty() {
            parts.push(format!("{} capture", self.captures.len()));
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
    h5i_root: &Path,
    m: &EnvManifest,
) -> std::collections::BTreeMap<String, usize> {
    let dir = env_dir(h5i_root, &m.agent, &m.slug);
    let by_id: std::collections::HashMap<String, String> = crate::receipt::list(&dir)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id, r.source))
        .collect();
    let mut by_source = std::collections::BTreeMap::<String, usize>::new();
    for id in &m.captures {
        let source = by_id.get(id).cloned().unwrap_or_else(|| "unknown".into());
        *by_source.entry(source).or_default() += 1;
    }
    by_source
}

/// Subject lines of the env commits in `base..env_tip`, oldest first, for the
/// squash message a `--patch` apply mints.
fn env_commit_subjects(repo: &Repository, base: git2::Oid, env_tip: git2::Oid) -> Vec<String> {
    let mut subjects = Vec::new();
    let Ok(mut walk) = repo.revwalk() else {
        return subjects;
    };
    let _ = walk.push(env_tip);
    let _ = walk.hide(base);
    // Oldest → newest so the folded squash message reads in commit order.
    let _ = walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE);
    for oid in walk.flatten() {
        if let Ok(commit) = repo.find_commit(oid) {
            let subject = commit.summary().unwrap_or("").trim();
            if !subject.is_empty() {
                subjects.push(format!("{} {}", &oid.to_string()[..7], subject));
            }
        }
    }
    subjects
}

// ─── inspect (§9) ───────────────────────────────────────────────────────────

/// Render one of an environment's evidence captures: its structured findings
/// (or text summary), exit code, policy digest, and any redactions. The
/// capture must belong to this env — a capture id from another env is refused
/// so `inspect` can't be used to read unrelated evidence.
pub fn inspect_manifest(
    h5i_root: &Path,
    m: &EnvManifest,
    capture_id: &str,
) -> Result<crate::receipt::ExecRecord, H5iError> {
    match crate::receipt::find(&env_dir(h5i_root, &m.agent, &m.slug), capture_id) {
        Ok(rec) => Ok(rec),
        Err(e) => {
            // Receipts are stored per environment, so a handle that resolves
            // nowhere here may still be another env's evidence. Say so by name
            // rather than "not found": a reviewer holding a capture id from a
            // sibling env should learn whose it is, not think it vanished.
            if let Some(owner) = owning_env_of_capture(h5i_root, capture_id) {
                return Err(H5iError::Metadata(format!(
                    "capture {} is not evidence for {} (it belongs to {})",
                    capture_id, m.id, owner
                )));
            }
            Err(e)
        }
    }
}

/// Which environment, if any, recorded `capture_id`. Scans the sibling env
/// directories; best-effort and read-only.
fn owning_env_of_capture(h5i_root: &Path, capture_id: &str) -> Option<String> {
    for m in list(h5i_root) {
        let dir = env_dir(h5i_root, &m.agent, &m.slug);
        if let Ok(rec) = crate::receipt::find(&dir, capture_id) {
            return Some(rec.env_id);
        }
    }
    None
}

/// Render one of an environment's evidence receipts: the command, its exit
/// code, the policy that was enforced, the egress verdicts, any redactions,
/// and the stored payload. The receipt must belong to this env — an id from
/// another env is refused so `inspect` can't be used to read unrelated
/// evidence.
pub fn inspect(h5i_root: &Path, m: &EnvManifest, capture_id: &str) -> Result<String, H5iError> {
    let rec = inspect_manifest(h5i_root, m, capture_id)?;
    let dir = env_dir(h5i_root, &m.agent, &m.slug);
    let raw = crate::receipt::raw_bytes(&dir, capture_id).unwrap_or_default();
    Ok(crate::receipt::render(&rec, &raw))
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
    /// The latest receipt's command, secret-redacted.
    pub last_cmd: Option<String>,
    /// Which lane observed the latest receipt (`host-env-run`, `tee-shim`, …).
    pub last_source: Option<String>,
    /// Denied egress attempts on the latest receipt, when the tier reports it.
    pub last_egress_denied: Option<u64>,
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
        let latest = m.captures.last().and_then(|cap| {
            crate::receipt::find(&env_dir(h5i_root, &m.agent, &m.slug), cap).ok()
        });
        rows.push(CompareRow {
            id: m.id,
            status: m.status,
            base_commit: m.base_commit,
            files_changed,
            insertions,
            deletions,
            last_exit: latest.as_ref().and_then(|r| r.exit_code),
            last_cmd: latest.as_ref().and_then(|r| r.cmd.clone()),
            last_source: latest.as_ref().map(|r| r.source.clone()),
            last_egress_denied: latest.as_ref().and_then(|r| r.egress.as_ref()).map(|e| e.denied),
        });
    }
    Ok(rows)
}

/// `(files_changed, insertions, deletions)` of an env's changes vs. its pinned
/// base. Uses the worktree when present, else the env branch tip (so pulled
/// "remote" envs still compare).
pub(crate) fn diffstat_numbers(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
) -> Option<(usize, usize, usize)> {
    // Delegates rather than re-deriving the numbers from `diff.stats()`. This
    // used to be a third independent copy of the same walk, and it counted the
    // deltas [`diffstat_report`] excludes — so an export summary said "2 file(s)"
    // over a one-file patch on macOS, where h5i's private-path symlink is a
    // delta. One implementation, one answer.
    let r = diffstat_report(repo, h5i_root, m).ok()?;
    Some((r.files_changed, r.insertions, r.deletions))
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
        let run = match (&r.last_cmd, r.last_exit) {
            (Some(cmd), exit) => {
                let denied = match r.last_egress_denied {
                    Some(n) if n > 0 => format!(" [egress denied {n}]"),
                    _ => String::new(),
                };
                format!(
                    "`{}` exit {}{denied}",
                    truncate_cmd(cmd, 40),
                    exit.map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                )
            }
            _ => "— (no run yet)".to_string(),
        };
        out.push_str(&format!(
            "  {:<26} {:<9} {:>7} {:>7} {:>7}  {}\n",
            r.id, r.status, r.files_changed, r.insertions, r.deletions, run
        ));
    }
    out.push_str("\nPick a winner with `h5i box diff <name>` / `h5i box inspect <name> --capture <id>`, then `h5i box apply <name>`.\n");
    out
}

/// Single-line, length-capped rendering of a command for a table cell.
fn truncate_cmd(cmd: &str, max: usize) -> String {
    let flat: String = crate::redact::sanitize_display(cmd).chars().take(max).collect();
    if cmd.chars().count() > max {
        format!("{flat}…")
    } else {
        flat
    }
}

/// The worktree-relative paths this env declares as private (per-env caches and
/// build dirs). Empty when the policy cannot be read — the caller then treats
/// nothing as private, which is the conservative direction: a path is shown to
/// the reviewer rather than hidden from them.
fn private_path_rels(h5i_root: &Path, m: &EnvManifest) -> Vec<String> {
    load_policy(h5i_root, m)
        .map(|p| {
            p.profile
                .private_paths
                .iter()
                .map(|pp| pp.path.trim_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The private paths this env keeps out of `diff`, `propose` and the exported
/// patch, for surfacing in **human** output.
///
/// Deliberately not folded into [`diff`]'s own return value: that string is what
/// `export` writes to `patch.diff`, and a note appended to it would be a note
/// inside a patch. Callers print this beside the diff, never within it.
///
/// The filter itself is right — a private path is a per-env cache, and on macOS
/// it is a symlink into h5i's own storage — but `private_paths` is repo-supplied
/// config, and a tool whose promise is "the only thing that comes out is a patch
/// you reviewed" should say which paths it declined to show rather than leave it
/// to be inferred.
pub fn private_paths_excluded(h5i_root: &Path, m: &EnvManifest) -> Vec<String> {
    private_path_rels(h5i_root, m)
}

/// Does this diff delta describe a private path on either side?
fn delta_is_private(d: &git2::DiffDelta<'_>, rels: &[String]) -> bool {
    if rels.is_empty() {
        return false;
    }
    [d.new_file().path(), d.old_file().path()]
        .into_iter()
        .flatten()
        .any(|p| is_under_private_path(p, rels))
}

/// Is `path` one of `rels`, or inside one? Compared by path component, so a
/// private `cache` never swallows a sibling called `cache-keys`.
fn is_under_private_path(path: &Path, rels: &[String]) -> bool {
    let p = path.to_string_lossy();
    let p = p.trim_end_matches('/');
    rels.iter()
        .any(|r| p == r.as_str() || p.strip_prefix(r.as_str()).is_some_and(|t| t.starts_with('/')))
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
    // h5i's own private-path artifacts are never the agent's work product.
    // Linux hides them by accident — a bind mount over an empty directory is
    // not a git entry — but macOS has no bind mounts, so the redirect is a
    // *symlink* at the worktree path. Without this it stages as a new `120000`
    // entry whose content is a host-absolute path into h5i's env storage, and
    // `export` then hands the reviewer a patch that, applied, drops that
    // symlink into their repository. Skipping it explicitly makes the intent
    // the same on both platforms.
    let private_rels = private_path_rels(h5i_root, m);

    {
        let mut cb = |path: &Path, _matched: &[u8]| -> i32 {
            if is_under_private_path(path, &private_rels) {
                return 1; // skip, and not a violation: h5i created it
            }
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
        // The SAME filter on the update pass. Today this is belt-and-braces
        // rather than a hole being closed: `add_all` with a `"*"` pathspec
        // already visits tracked entries, so a path that escapes `$WORK` through
        // a symlinked parent is caught above and aborts the commit before this
        // runs. But that safety is a property of libgit2's pathspec semantics,
        // not of anything stated here, and the update pass re-stages tracked
        // paths on its own terms. An invariant enforced on one of two staging
        // paths is an invariant that depends on the other one never changing.
        index.update_all(
            ["*"].iter(),
            Some(&mut cb as &mut git2::IndexMatchedPath),
        )?;
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
    let sig = crate::refstore::signature(&wt_repo)?;
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

/// Snapshot the env worktree onto its branch for a **team submission** — the
/// mediated-commit counterpart to `propose`, so `team agent submit` freezes the
/// agent's working-tree edits instead of the (often unadvanced) branch tip.
///
/// **Best-effort, unlike `propose`.** A team submit is ingested *while the
/// agent's box is still alive* — the team Stop hook keeps boxes running and
/// `team sync` drains the spool mid-round — so the box holds the env run lock
/// for its whole session. `propose` fails on a contended lock (it is a
/// deliberate state transition that must not race a live run); a team submit
/// must NOT. A well-behaved agent has already committed its work in-box (the
/// branch tip is correct and needs no snapshot), so on lock contention we fall
/// back to the existing branch tip rather than failing the submit. An
/// *uncommitted* worktree behind a live box is captured later by the at-exit
/// ingest, which runs once the lock frees.
///
/// Returns `Ok(None)` — no snapshot taken — when the env has no local worktree
/// (a *pulled* reviewer clone rides the already-shared branch tip), when the box
/// is alive (lock contended), or when the worktree already matches the tip.
pub fn snapshot_for_submit(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
) -> Result<Option<git2::Oid>, H5iError> {
    if !m.work_dir(h5i_root).is_dir() {
        return Ok(None);
    }
    #[cfg(unix)]
    let _run_lock = match RunLock::acquire(&m.dir(h5i_root)) {
        Ok(lock) => lock,
        // Box alive (the normal mid-round case) — don't fail the submit; the
        // already-committed branch tip is what we freeze, and any uncommitted
        // worktree is picked up by the at-exit ingest with no contention.
        Err(_) => return Ok(None),
    };
    mediated_commit(repo, h5i_root, m)
}

/// Commit the current worktree onto its checked-out branch from *inside* an env
/// box — the in-box analogue of [`snapshot_for_submit`]. `team agent submit`
/// calls this so the agent's edits are frozen even when they were never
/// `git add`/committed (the common case: an agent writes files and submits
/// without committing). The host **cannot** do this for a live box: the box
/// holds the env run lock for its whole session and writing its index from the
/// host would race the agent's own git use — so the box, which has a functional
/// checkout (rw on its own env branch + objects via `box_git_plumbing`) and runs
/// with the worktree as its CWD, snapshots itself here.
///
/// Returns `Ok(Some(oid))` for a fresh snapshot, `Ok(None)` when the worktree
/// already matches the branch tip (a well-behaved agent that committed in-box)
/// or there is no git checkout to commit. An `Err` means the box tried but
/// *couldn't* commit — e.g. a `box_git_plumbing` grant is too narrow. The caller
/// must surface that error (and continue: a failed snapshot must never block the
/// submit), because a silently-swallowed failure here is exactly what makes an
/// agent's work vanish into a "no changes to review" no-op.
pub fn commit_box_worktree() -> Result<Option<git2::Oid>, H5iError> {
    commit_worktree_at(Path::new("."))
}

/// In-box: does the current checkout's HEAD tree equal `tree_hex`? Paired with
/// [`commit_box_worktree`] returning `None` (worktree == HEAD) and the exported
/// `$H5I_ENV_BASE_TREE`, this lets `h5i team agent submit` prove a submission
/// would be empty (tree identical to the pinned base) and refuse in front of
/// the agent — instead of staging a spool request the host is guaranteed to
/// reject. Any doubt (no checkout, unreadable HEAD, malformed hex) answers
/// `false`, so the submit proceeds and the host stays the authority.
pub fn head_tree_matches(tree_hex: &str) -> bool {
    head_tree_matches_at(Path::new("."), tree_hex)
}

fn head_tree_matches_at(path: &Path, tree_hex: &str) -> bool {
    let Ok(repo) = Repository::discover(path) else {
        return false;
    };
    let Ok(head) = repo.head().and_then(|h| h.peel_to_commit()) else {
        return false;
    };
    head.tree_id().to_string() == tree_hex
}

fn commit_worktree_at(path: &Path) -> Result<Option<git2::Oid>, H5iError> {
    let repo = match Repository::discover(path) {
        Ok(r) if !r.is_bare() => r,
        // No checkout to snapshot (not an error worth surfacing).
        _ => return Ok(None),
    };
    let head = repo.head()?.peel_to_commit()?;
    let mut index = repo.index()?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    index.update_all(["*"].iter(), None)?;
    // `write_tree` writes the tree (and any new blobs) to the object db from the
    // *in-memory* index — it does NOT require the on-disk index file to be
    // rewritten. We deliberately commit from this without an `index.write()`
    // first: the commit needs only objects (rw) + the env branch ref (rw), both
    // granted in-box, whereas persisting the index file (`index.lock` →
    // `index`) is the one step the proven-working `h5i capture commit` path
    // never exercises (its index was written by the agent's `git add`), and the
    // step most likely to EACCES under a tight box layout. So land the commit
    // first, then refresh the index best-effort.
    let tree_oid = index.write_tree()?;
    if head.tree_id() == tree_oid {
        return Ok(None); // worktree already committed — nothing to snapshot
    }
    let tree = repo.find_tree(tree_oid)?;
    let sig = crate::refstore::signature(&repo)?;
    let oid = repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "h5i team: in-box submit snapshot",
        &tree,
        &[&head],
    )?;
    // Keep a later in-box `git status` clean. Best-effort: the commit already
    // landed, so an index-write EACCES must not fail (or undo) the snapshot.
    let _ = index.write();
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
        "\nReview with `h5i box diff {}`, then `h5i box apply {}` (reviewer-selected; never automatic).\n",
        m.slug, m.slug
    ));
    Ok(brief)
}

/// A copy-pasteable runbook for resolving a source-code conflict from *inside*
/// the env's sandbox. `rebase`/`apply` refuse cleanly (no markers, full
/// rollback), so there is no `git merge --continue` state to resume; the user
/// re-does the merge by hand in the box, where the worktree has a functional
/// git checkout (rw on its own branch + objects, ro on the parent ref). Merging
/// the parent into the env branch in-box makes a later `apply` fast-forward.
fn conflict_runbook(m: &EnvManifest) -> String {
    format!(
        "to resolve: `h5i box shell {slug}`, then inside the box \
         `git merge {parent}` — fix the conflicts, `git add` the files, \
         `git commit` — exit, then `h5i box apply {slug}`",
        slug = m.slug,
        parent = m.parent_branch,
    )
}

/// Apply a proposed env onto its parent branch. Explicit, reviewer-driven:
/// requires the parent branch checked out and a clean tracked working tree.
/// `--patch` squashes the env's diff into one commit; the default `--merge`
/// fast-forwards or creates a two-parent merge commit. Conflicts refuse.
pub fn apply(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    patch_mode: bool,
) -> Result<String, H5iError> {
    if is_detached(m) {
        return Err(detached_err(m, "apply"));
    }
    // Serialize the PROPOSED→APPLIED transition (reads the env state, mutates
    // the manifest) against any concurrent run/shell on the same env.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
    if m.status != ST_PROPOSED {
        return Err(H5iError::Metadata(format!(
            "{} is '{}' — run `h5i box propose {}` first (apply is never automatic)",
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

    // A `--patch` apply squashes the env commits into one new commit, so their
    // subject lines are folded forward into the squash message (oldest first);
    // merge and fast-forward apply preserve the env commits as they are.
    let folded_subjects = if patch_mode {
        env_commit_subjects(repo, base_oid, env_tip.id())
    } else {
        Vec::new()
    };

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
                "apply refused — merge conflicts in: {}. Rebase the env (`h5i box rebase {}`), or {}.",
                paths.join(", "),
                m.slug,
                conflict_runbook(m)
            )));
        }
        let tree = repo.find_tree(idx.write_tree_to(repo)?)?;
        let sig = crate::refstore::signature(repo)?;
        let msg = if patch_mode {
            let mut msg = format!("h5i box apply --patch: {} → {}", m.id, m.parent_branch);
            if !folded_subjects.is_empty() {
                msg.push_str("\n\nSquashed env commits:\n");
                for s in &folded_subjects {
                    msg.push_str("  ");
                    msg.push_str(s);
                    msg.push('\n');
                }
            }
            msg
        } else {
            format!("h5i box apply: merge {} → {}", m.id, m.parent_branch)
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
        &format!("h5i box apply: {}", m.id),
    )?;

    // Evidence summary on the `applied` event, linking the env's receipts to the
    // commit they now live on (the event log resolves env → result).
    let lanes = evidence_sources_by_lane(h5i_root, m)
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
        "{} applied onto {} ({}{})",
        m.id,
        m.parent_branch,
        &new_commit.to_string()[..12],
        if base_oid == parent_tip.id() && !patch_mode {
            ", fast-forward"
        } else {
            ""
        },
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
    if is_detached(m) {
        return Err(detached_err(m, "rebase"));
    }
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
            "rebase refused — conflicts against the new base in: {}. Either apply against the \
             old base (`h5i box apply {}`), or {}.",
            paths.join(", "),
            m.slug,
            conflict_runbook(m)
        )));
    }
    let merged_tree = wt_repo.find_tree(idx.write_tree_to(&wt_repo)?)?;

    // Commit the rebased state on the env branch: a 2-parent commit (env work +
    // new parent tip) so provenance shows what it was folded onto.
    let sig = crate::refstore::signature(&wt_repo)?;
    let rebased = wt_repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("h5i box rebase: {} onto {}", m.id, m.parent_branch),
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
        // Drain read-only observers before removing this env's worktree: an
        // observer has it mounted (a `--readonly` shell that attached while the
        // env was still live can outlast the apply/abort that finalized it), so
        // the prune must not yank the directory out from under it. Non-blocking:
        // if observers are attached we skip this env and reclaim it on a later
        // sweep, exactly as we do on a failed prune.
        #[cfg(unix)]
        let _teardown = match RunLock::acquire_teardown(&m.dir(h5i_root)) {
            Ok(g) => g,
            Err(_) => continue,
        };
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
            "{} is still live (status: {}) — abort it first (`h5i box abort {}`) or pass \
             --force to remove it anyway",
            m.id, m.status, m.slug
        )));
    }

    // Serialize against a concurrent read-write session and drain read-only
    // observers before destroying the worktree + branches: a writer may be
    // mid-run and an observer has the worktree mounted. Acquire `run.lock`
    // first, then the observer teardown lock (the documented order). Both are
    // non-blocking — rm refuses "busy" rather than yanking the worktree from
    // under a live session (even `--force`, which only overrides the *status*
    // guard above, never live sessions). The locks are held until rm returns,
    // so removing the env dir (which holds the lock files) at the end is safe.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;
    #[cfg(unix)]
    let _teardown = RunLock::acquire_teardown(&m.dir(h5i_root))?;

    // 1. Reclaim the workspace. Must precede the branch delete: git refuses to
    //    delete a branch still checked out in a registered worktree.
    prune_workspace(repo, h5i_root, m)?;

    // 2. Delete the code branch. Tolerate a missing ref (a pulled or
    //    already-half-removed env may lack one locally).
    if let Ok(mut r) = repo.find_reference(&m.branch) {
        r.delete()?;
    }

    // 3. Record the removal AND strip the manifest/policy from refs/h5i/env
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
    // A `browser` box leaves Chrome running on purpose — it has to outlive the
    // `box run` that started it — so removing the box has to stop it, or the
    // process outlives everything that knows about it. Best-effort: a survivor
    // is untidy, not unsafe, and must not block the removal the user asked for.
    //
    // Through `stop_browser`, which checks the recorded pid still names *this
    // box's* browser before signalling anything. The pid is written by the box
    // and read here on the host, outside every sandbox: taken at face value,
    // `-1` would turn "remove this env" into "SIGTERM everything I own". See
    // [`browser_pid`].
    stop_browser(&dir.join("browser/state"));
    // On macOS the private `/tmp` backing lives outside the env dir (see
    // [`private_tmp_backing`]), so erasing the env dir no longer takes it with
    // it. Best-effort: a leftover scratch dir is tidiness, not correctness, and
    // must not block the removal the user asked for.
    let _ = std::fs::remove_dir_all(private_tmp_backing(&dir.join("tmp")));
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
    fn egress_rule_validation_accepts_proxy_forms_only() {
        // The three forms the proxy's AllowList understands, normalized.
        assert_eq!(validate_egress_rule(" API.Example.com ").unwrap(), "api.example.com");
        assert_eq!(validate_egress_rule(".example.com").unwrap(), ".example.com");
        assert_eq!(validate_egress_rule("*.example.com").unwrap(), "*.example.com");
        assert_eq!(validate_egress_rule("github.com:443").unwrap(), "github.com:443");
        // Strict intake: URLs, paths, whitespace, malformed hosts, bad ports.
        for bad in [
            "",
            "https://example.com",
            "example.com/path",
            "two hosts",
            "a,b",
            "example.com:notaport",
            "example.com:99999",
            "-leading.example",
            ".",
            "a..b",
            "trailing.example.",
            "::1",
        ] {
            assert!(validate_egress_rule(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn live_registry_registers_and_reconciles_dead_pids() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("envdir");
        {
            let _g = LiveGuard::register(&env_dir, "shell", Some("bash".into()));
            let live = live_sessions(&env_dir);
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].pid, std::process::id());
            assert_eq!(live[0].kind, "shell");
            assert!(live_is_writer(&live[0].kind));
            assert!(!live_is_writer("observe"));
        }
        // A cleanly-dropped guard removed its record.
        assert!(live_sessions(&env_dir).is_empty());

        // Crash leftovers: a dead PID's record and an unparseable file are
        // reconciled away on read (PID identity, not timestamps).
        let live_dir = env_dir.join(LIVE_DIR);
        std::fs::create_dir_all(&live_dir).unwrap();
        let dead = LiveSession {
            // Far above any real pid_max, and positive as i32 (kill probe).
            pid: 2_147_483_646,
            kind: "run".into(),
            started_at: now_ts(),
            command: None,
        };
        std::fs::write(
            live_dir.join("2147483646.json"),
            serde_json::to_string(&dead).unwrap(),
        )
        .unwrap();
        std::fs::write(live_dir.join("garbage.json"), "not json").unwrap();
        assert!(live_sessions(&env_dir).is_empty());
        assert!(!live_dir.join("2147483646.json").exists());
        assert!(!live_dir.join("garbage.json").exists());
    }

    #[test]
    fn user_allow_file_round_trips_and_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h5i").join("egress-allow");
        write_user_allow(&path, &["pypi.org".into(), ".github.com:443".into()]).unwrap();
        assert_eq!(
            user_allow_list_at(Some(&path)),
            vec!["pypi.org".to_string(), ".github.com:443".to_string()]
        );
        // Comments, blanks, dupes, and invalid lines are tolerated on read
        // (fail-closed toward fewer grants, never toward aborting a session).
        std::fs::write(
            &path,
            "# comment\n\npypi.org\nPYPI.ORG\nhttps://not-a-host\npypi.org\n",
        )
        .unwrap();
        assert_eq!(user_allow_list_at(Some(&path)), vec!["pypi.org".to_string()]);
        // Missing file → empty, not an error.
        assert!(user_allow_list_at(Some(&dir.path().join("absent"))).is_empty());
    }

    #[test]
    fn resolve_work_rcfile_accepts_in_tree_and_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path();
        std::fs::create_dir_all(work.join(".h5i")).unwrap();
        std::fs::write(work.join(".h5i/box.bashrc"), "PS1='x '\n").unwrap();

        // A real file inside the worktree resolves to its absolute path.
        let got = resolve_work_rcfile(work, ".h5i/box.bashrc").unwrap();
        assert_eq!(got, work.join(".h5i/box.bashrc").display().to_string());

        // Absolute, `..`-escaping, and missing all fail closed.
        assert!(resolve_work_rcfile(work, "/etc/passwd").is_err());
        assert!(resolve_work_rcfile(work, "../outside.bashrc").is_err());
        assert!(resolve_work_rcfile(work, ".h5i/../../etc/x").is_err());
        assert!(resolve_work_rcfile(work, "does-not-exist.bashrc").is_err());
    }

    #[test]
    fn write_plain_bashrc_is_self_contained_and_skips_host_bashrc() {
        let h5i_root = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");

        let path = write_plain_bashrc(h5i_root.path(), &m).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Carries the env id in the prompt, an optional ~/.h5i_envrc hook, and
        // never sources the host ~/.bashrc.
        assert!(body.contains(&format!("h5i:{}", m.id)));
        assert!(body.contains("$HOME/.h5i_envrc"));
        assert!(!body.contains(".bashrc\""));
        assert!(path.ends_with("shell/rc.bash"));
    }

    // A box's rootfs is the image's, so the host `$SHELL` is a path to a binary
    // that is simply not there: a stock macOS `$SHELL=/bin/zsh` used to end the
    // session at exec ("/bin/zsh: not found") before the first prompt. The shell
    // must be resolved inside the box instead — on every image-backed tier, and
    // regardless of what the profile says about rc files (which are the image's
    // business too).
    #[test]
    fn image_backed_tiers_take_their_shell_from_the_image_not_the_host() {
        use crate::sandbox::Profile;
        let h5i_root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");
        let host_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());

        for claim in [IsolationClaim::Container, IsolationClaim::Microvm] {
            for rcfile in [None, Some(".h5i/box.bashrc".to_string())] {
                let mut pol = ResolvedPolicy::new(claim, Profile::builtin("default", claim));
                pol.profile.shell_rcfile = rcfile.clone();
                let argv = default_shell_argv(h5i_root.path(), &m, &mut pol, work.path())
                    .unwrap()
                    .argv;

                assert_eq!(argv[0], "/bin/sh", "{claim:?} launches via the image's sh");
                assert_eq!(argv[1], "-c");
                // bash when the image has it, the guaranteed /bin/sh otherwise —
                // and `exec` either way, so the probe leaves no parent behind.
                assert!(argv[2].contains("exec bash -i"), "{argv:?}");
                assert!(argv[2].contains("exec /bin/sh -i"), "{argv:?}");
                // Nothing host-side rides along: no host $SHELL, and no
                // host-path rcfile (which would not resolve in-box).
                if host_shell != "/bin/sh" {
                    assert!(!argv.contains(&host_shell), "host shell leaked: {argv:?}");
                }
                assert!(!argv.iter().any(|a| a == "--rcfile"), "{argv:?}");
            }
        }
    }

    // The kernel tiers DO run against the host filesystem, so there the host
    // shell is the right one — the fix above must not have flattened them.
    #[test]
    fn kernel_tiers_keep_the_host_shell() {
        use crate::sandbox::Profile;
        let h5i_root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");
        let host_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());

        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        let launch = default_shell_argv(h5i_root.path(), &m, &mut pol, work.path()).unwrap();
        assert_eq!(launch.argv[0], host_shell);
        assert_eq!(launch.argv.last().unwrap(), "-i");
    }

    // zsh is the macOS default shell, so this is the ordinary case on a Mac host:
    // its `$HISTFILE` defaults into the operator's real HOME, which no box grants,
    // and zsh reports the failed lock at startup and after every command. The
    // launch must move both the rc and the history into the env's own dir.
    #[test]
    fn zsh_gets_a_generated_zdotdir_so_history_lands_inside_the_box() {
        use crate::sandbox::Profile;
        let h5i_root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");

        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin("default", IsolationClaim::Supervised),
        );
        let launch = shell_launch(h5i_root.path(), &m, &mut pol, work.path(), "/bin/zsh").unwrap();

        assert_eq!(launch.argv, vec!["/bin/zsh".to_string(), "-i".to_string()]);
        // `$ZDOTDIR` must arrive in the environment: zsh resolves it before it
        // sources anything, so no rc file could set it in time.
        let (_, zdotdir) = launch
            .env
            .iter()
            .find(|(k, _)| k == "ZDOTDIR")
            .expect("ZDOTDIR injected");
        let rc = std::fs::read_to_string(Path::new(zdotdir).join(".zshrc")).unwrap();
        let histdir = m.dir(h5i_root.path()).join("shell").join("history");
        assert!(
            rc.contains(&format!("HISTFILE='{}/zsh_history'", histdir.display())),
            "history redirected off the host's: {rc}"
        );
        assert!(rc.contains(&format!("h5i:{}", m.id)));
        assert!(rc.contains("$HOME/.h5i_envrc"));

        // The kernel tiers enforce an allowlist, so both dirs need a grant — and
        // only the history one is writable: the box keeps its history across
        // sessions but cannot rewrite the rc that starts the next one.
        assert!(pol.profile.fs_read.iter().any(|p| p == zdotdir));
        assert!(!pol.profile.fs_write.iter().any(|p| p == zdotdir));
        assert!(pol.profile.fs_write.iter().any(|p| Path::new(p) == histdir));
    }

    /// The zsh rc interpolates paths, and a path may contain a quote — the bash
    /// rc never had to care because it only ever interpolates the env id.
    #[test]
    fn the_generated_zshrc_survives_a_quote_in_a_path() {
        let h5i_root = tempfile::Builder::new()
            .prefix("it's-a-dir")
            .tempdir()
            .unwrap();
        let m = canonical_manifest("claude", "demo");

        let z = write_plain_zshrc(h5i_root.path(), &m, Some("/tmp/o'clock.zshrc")).unwrap();
        let rc = std::fs::read_to_string(Path::new(&z.zdotdir).join(".zshrc")).unwrap();

        // Read back through a real shell: the quoting is only right if the value
        // the shell ends up with is the path we meant.
        let line = rc
            .lines()
            .find(|l| l.starts_with("HISTFILE="))
            .expect("HISTFILE line");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{line}; printf '%s' \"$HISTFILE\""))
            .output()
            .expect("run sh");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            Path::new(&z.histdir)
                .join("zsh_history")
                .display()
                .to_string()
        );

        let src = rc.lines().find(|l| l.starts_with("source ")).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", &src["source ".len()..]))
            .output()
            .expect("run sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/tmp/o'clock.zshrc");
    }

    // zsh has no `--rcfile`, so a profile's `[shell] rcfile` reaches it by being
    // sourced from the generated rc — last, so it wins over the plain defaults.
    #[test]
    fn zsh_sources_a_profile_pinned_rcfile_from_the_generated_rc() {
        use crate::sandbox::Profile;
        let h5i_root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");
        std::fs::create_dir_all(work.path().join(".h5i")).unwrap();
        std::fs::write(work.path().join(".h5i/box.zshrc"), "PROMPT='custom %# '\n").unwrap();

        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin("default", IsolationClaim::Supervised),
        );
        pol.profile.shell_rcfile = Some(".h5i/box.zshrc".into());
        let launch = shell_launch(h5i_root.path(), &m, &mut pol, work.path(), "/bin/zsh").unwrap();

        let (_, zdotdir) = launch.env.iter().find(|(k, _)| k == "ZDOTDIR").unwrap();
        let rc = std::fs::read_to_string(Path::new(zdotdir).join(".zshrc")).unwrap();
        let pinned = work.path().join(".h5i/box.zshrc");
        assert!(
            rc.contains(&format!("source '{}'", pinned.display())),
            "{rc}"
        );
        // Sourced after the defaults, or the pin would not win.
        assert!(
            rc.find("source '").unwrap() > rc.find("PROMPT=").unwrap(),
            "{rc}"
        );
    }

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
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "d".repeat(64),
            isolation_claim: "workspace".into(),
            backend: "worktree".into(),
            created_at: now_ts(),
            updated_at: now_ts(),
            status: ST_IDLE.into(),
            captures: vec![],
            service_digest: None,
            persona_digest: None,
            pr: None,
            pr_head_ref: None,
        }
    }

    #[test]
    fn snapshot_for_submit_is_best_effort_under_run_lock() {
        // A team submit is ingested while the agent's box is still alive, so the
        // box holds the env run lock. Unlike propose, snapshot_for_submit must
        // NOT fail on contention — it falls back to the branch tip (Ok(None)) so
        // the submission still records; the regression this guards turned every
        // mid-round `team sync` into a silently-dropped submission.
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let repo = git2::Repository::init(h5i_root.join("repo")).unwrap();
        let m = canonical_manifest("claude", "fix");
        // A worktree dir must exist or the function short-circuits before the lock.
        std::fs::create_dir_all(m.work_dir(h5i_root)).unwrap();

        // Simulate a live `env shell` holding the per-env lock.
        let _held = RunLock::acquire(&m.dir(h5i_root)).unwrap();

        // propose-style ops refuse under contention; snapshot_for_submit defers.
        let got = snapshot_for_submit(&repo, h5i_root, &m)
            .expect("submit snapshot must not fail when the box holds the lock");
        assert!(got.is_none(), "contended snapshot falls back to the branch tip");
    }

    #[test]
    fn commit_box_worktree_snapshots_untracked_edits() {
        // The in-box submit path: an agent writes files but never commits, so
        // the worktree is dirty/untracked. commit_box_worktree must fold those
        // onto the branch tip so the host freezes real work — not the base.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let base = commit_file(&repo, "README.md", "hello\n");

        // Untracked file, exactly like codex's `?? quick_sort.py`.
        std::fs::write(dir.path().join("quick_sort.py"), "def quick_sort():\n    pass\n")
            .unwrap();

        let oid = commit_worktree_at(dir.path())
            .expect("commit must not error")
            .expect("dirty worktree must commit");
        assert_ne!(oid, base, "branch must advance off base");
        let tree = repo.find_commit(oid).unwrap().tree().unwrap();
        assert!(tree.get_path(Path::new("quick_sort.py")).is_ok());
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), oid);

        // Idempotent: a clean worktree is a no-op (well-behaved already-committed agent).
        assert!(commit_worktree_at(dir.path()).unwrap().is_none());
    }

    #[test]
    fn head_tree_matches_proves_an_empty_submission() {
        // The in-box `team agent submit` refusal: nothing snapshotted AND the
        // branch tip tree still equals the exported base tree → provably empty.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let base = commit_file(&repo, "README.md", "hello\n");
        let base_tree = repo.find_commit(base).unwrap().tree_id().to_string();

        assert!(head_tree_matches_at(dir.path(), &base_tree));

        // Real work breaks the match — the submit must proceed.
        commit_file(&repo, "feature.txt", "ok\n");
        assert!(!head_tree_matches_at(dir.path(), &base_tree));

        // Doubt answers false (host stays the authority): malformed hex.
        assert!(!head_tree_matches_at(dir.path(), "not-a-tree-oid"));
    }

    fn commit_file(repo: &git2::Repository, name: &str, body: &str) -> git2::Oid {
        let work = repo.workdir().unwrap();
        std::fs::write(work.join(name), body).unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new(name)).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@e.com").unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let prefs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &prefs)
            .unwrap()
    }

    #[test]
    fn conflict_runbook_points_at_in_box_resolution() {
        // The refuse-and-rollback design leaves no `git merge --continue` state,
        // so the error text must hand the user the full in-box runbook: which
        // env to shell into, the parent to merge, and the apply to finish with.
        let m = canonical_manifest("claude", "auth-fix");
        let rb = conflict_runbook(&m);
        assert!(
            rb.contains("h5i box shell auth-fix"),
            "names the env shell: {rb}"
        );
        assert!(
            rb.contains("git merge main"),
            "names the parent merge: {rb}"
        );
        assert!(
            rb.contains("h5i box apply auth-fix"),
            "names the finishing apply: {rb}"
        );
    }

    #[test]
    fn read_staged_capture_round_trips_and_rejects_unsafe_ids() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        let meta = InboxCaptureMeta {
            cmd: "h5i team artifact show x --diff".into(),
            cwd: None,
            exit_code: Some(0),
            files: vec![],
            cmd_argv: vec![],
        };
        let id = write_inbox_capture_spool(&spool, &meta, b"FULL DIFF\nLINE 2\n").unwrap();
        // The id `capture run` printed rehydrates the full raw + meta from the spool.
        let staged = read_staged_capture_at(&spool, &id).expect("staged capture present");
        assert_eq!(staged.raw, b"FULL DIFF\nLINE 2\n");
        assert_eq!(staged.meta.unwrap().cmd, "h5i team artifact show x --diff");
        // Unknown / non-cap / path-traversal ids return None (never touch disk).
        assert!(read_staged_capture_at(&spool, "cap-does-not-exist").is_none());
        assert!(read_staged_capture_at(&spool, "note-abc").is_none());
        assert!(read_staged_capture_at(&spool, "cap-../../etc/passwd").is_none());
    }

    #[test]
    fn inbox_pending_context_path_gated_on_all_three_box_markers() {
        use std::ffi::OsString;
        let spool = OsString::from("/tmp/spool");
        let id = OsString::from("env/human/x");
        let dig = OsString::from("digest");

        // All three present → redirected into the spool's pending_context.json.
        let p = inbox_pending_context_path_from(
            Some(id.clone()),
            Some(dig.clone()),
            Some(spool.clone()),
        )
        .expect("all markers set → Some");
        assert_eq!(p, PathBuf::from("/tmp/spool").join(SPOOL_PENDING_CONTEXT));

        // Any missing marker → None (host uses the normal .git/.h5i path).
        assert!(inbox_pending_context_path_from(None, Some(dig.clone()), Some(spool.clone())).is_none());
        assert!(inbox_pending_context_path_from(Some(id.clone()), None, Some(spool.clone())).is_none());
        // Env id + digest present but no spool dir → None (nowhere box-writable).
        assert!(inbox_pending_context_path_from(Some(id), Some(dig), None).is_none());
    }

    #[test]
    fn write_team_submit_spool_records_scoped_request() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        let base = write_team_submit_spool(
            &spool,
            &TeamSubmitSpool {
                commit: Some("HEAD".into()),
                summary: Some("ready".into()),
            },
        )
        .unwrap();
        assert!(base.starts_with("team-submit-"));
        let raw = std::fs::read(spool.join(format!("{base}.json"))).unwrap();
        let request: TeamSubmitSpool = serde_json::from_slice(&raw).unwrap();
        assert_eq!(request.commit.as_deref(), Some("HEAD"));
        assert_eq!(request.summary.as_deref(), Some("ready"));
    }

    #[test]
    fn write_team_review_spool_records_scoped_request() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        let base = write_team_review_spool(
            &spool,
            &TeamReviewSpool {
                target: "codex-fix".into(),
                body: "looks good".into(),
            },
        )
        .unwrap();
        assert!(base.starts_with("team-review-"));
        let raw = std::fs::read(spool.join(format!("{base}.json"))).unwrap();
        let request: TeamReviewSpool = serde_json::from_slice(&raw).unwrap();
        assert_eq!(request.target, "codex-fix");
        assert_eq!(request.body, "looks good");
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
        // libgit2 resolves symlinks when it records the repository path, so
        // `commondir()` comes back fully realpath'd. On Linux that changes
        // nothing, but macOS puts the temp dir under `/var/folders/...`, and
        // `/var` is a firmlink to `/private/var` — so the grant would read
        // `/private/var/...` while an expectation built from `dir.path()` says
        // `/var/...`, and the string compare below would fail for a reason that
        // has nothing to do with the grant being right. Compare like for like.
        let root = root.canonicalize().unwrap();
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
        grant_box_git(&repo, &m, &work, &mut pol, false).unwrap();

        let codex = root.join(".codex").display().to_string();
        let claude = root.join(".claude").display().to_string();
        // Existing project-config dirs are READ-granted, never write-granted.
        assert!(
            pol.profile.fs_read.contains(&codex),
            "main-repo .codex read: {:?}",
            pol.profile.fs_read
        );
        assert!(
            pol.profile.fs_read.contains(&claude),
            "main-repo .claude read"
        );
        assert!(!pol.profile.fs_write.contains(&codex), "stays immutable");
        assert!(!pol.profile.fs_write.contains(&claude), "stays immutable");

        // An absent dir is not granted (no phantom grant), and container leaves
        // fs lists alone (it doesn't share the host repo tree).
        std::fs::remove_dir_all(root.join(".claude")).unwrap();
        let mut pol2 = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut pol2, false).unwrap();
        assert!(
            !pol2.profile.fs_read.contains(&claude),
            "absent dir not granted"
        );
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
        grant_box_git(&repo, &m, &work, &mut pol, false).unwrap();
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
        grant_box_git(&repo, &m, &work, &mut pol, false).unwrap();
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
        grant_box_git(&repo, &m, &work, &mut pol, false).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn the_private_tmp_is_cleared_when_ours_and_refused_when_not() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();

        // Fresh: created 0700.
        let fresh = td.path().join("fresh");
        reset_private_tmp(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );

        // Ours with leftovers: cleared rather than adopted, and 0700 again even
        // though the stale directory had been widened.
        std::fs::write(fresh.join("stale"), b"x").unwrap();
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).unwrap();
        reset_private_tmp(&fresh).unwrap();
        assert!(!fresh.join("stale").exists(), "leftovers survived the reset");
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );

        // A symlink planted at the path is refused rather than followed — the
        // point of the ownership check, since on macOS this lives in
        // world-writable `/tmp`.
        let victim = td.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("keep"), b"important").unwrap();
        let planted = td.path().join("planted");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        assert!(reset_private_tmp(&planted).is_err(), "followed a symlink");
        assert!(victim.join("keep").exists(), "the symlink target was cleared");
    }

    #[cfg(unix)]
    #[test]
    fn prepare_private_tmp_redirects_shared_tmp_to_env_backing() {
        use crate::sandbox::{AgentRuntime, Profile};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let m = canonical_manifest("claude", "tmp");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );
        assert!(pol.profile.fs_write.iter().any(|w| w == "/tmp"));

        prepare_private_tmp(h5i_root, &m, &mut pol, None).unwrap();

        // Where the backing actually lands: inside the env dir on Linux, and at
        // a short path outside the repository on macOS, where `TMPDIR` *is* the
        // path programs build on and `AF_UNIX` caps it at 104 bytes. Asking
        // [`private_tmp_backing`] keeps the test honest about the platform
        // instead of pinning the Linux layout on both.
        let backing = private_tmp_backing(&m.dir(h5i_root).join("tmp"));
        assert!(backing.is_dir(), "{backing:?}");
        // `#[cfg]`, not `cfg!`: `TMPDIR_BUDGET` only exists on macOS, and a
        // runtime `cfg!` still has to compile on every platform.
        #[cfg(target_os = "macos")]
        {
            let len = backing.display().to_string().len();
            assert!(len <= TMPDIR_BUDGET, "TMPDIR too long for AF_UNIX: {len}");
        }
        assert_eq!(
            std::fs::metadata(&backing).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(!pol.profile.fs_read.iter().any(|w| w == "/tmp"));
        assert!(!pol.profile.fs_write.iter().any(|w| w == "/tmp"));
        assert!(pol
            .profile
            .fs_write
            .iter()
            .any(|w| w == &backing.display().to_string()));
        let tmp_bind = pol
            .home_binds
            .iter()
            .find(|b| b.target.as_path() == Path::new("/tmp"))
            .unwrap();
        assert_eq!(tmp_bind.backing, backing);
    }

    // ─── read-only observer (`env shell --readonly`) ────────────────────────

    /// The two-lock model: one read-write session **plus** N observers coexist
    /// (independent lock files), two writers still exclude each other, and a
    /// worktree teardown (gc/rm) waits for every observer to drain.
    #[cfg(unix)]
    #[test]
    fn locks_allow_one_writer_plus_many_observers_and_teardown_drains_observers() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path();

        // A read-write session and two observers all hold their locks at once —
        // the writer (run.lock) and observers (observers.lock) do not exclude
        // each other.
        let w = RunLock::acquire(env_dir).unwrap();
        let r1 = RunLock::acquire_observer(env_dir).unwrap();
        let r2 = RunLock::acquire_observer(env_dir).unwrap();

        // A second writer is still refused: run.lock serializes writers.
        assert!(
            RunLock::acquire(env_dir).is_err(),
            "a second read-write session must be refused while one holds run.lock"
        );
        // A teardown is refused while observers are live: it must not prune the
        // worktree out from under them.
        assert!(
            RunLock::acquire_teardown(env_dir).is_err(),
            "a teardown must be refused while observers hold observers.lock"
        );

        // The live writer does not block a teardown's observers.lock; only the
        // observers do. Drop the writer — a teardown is still refused.
        drop(w);
        assert!(
            RunLock::acquire_teardown(env_dir).is_err(),
            "observers alone must still block a teardown after the writer exits"
        );

        // Drain the observers → a teardown (and a fresh writer) can proceed.
        drop(r1);
        drop(r2);
        let td = RunLock::acquire_teardown(env_dir).unwrap();
        // While a teardown holds observers.lock exclusively, a new observer is
        // refused (the worktree is being removed).
        assert!(
            RunLock::acquire_observer(env_dir).is_err(),
            "an observer must be refused while a teardown is removing the worktree"
        );
        drop(td);
        // Once the teardown exits, an observer can attach again.
        RunLock::acquire_observer(env_dir).unwrap();
    }

    /// A read-only observer's HOME redirect lands in the caller-supplied
    /// per-session root, not the persistent per-env `<env>/home` — so concurrent
    /// observers never share (and race on) one credential copy.
    #[cfg(unix)]
    #[test]
    fn prepare_home_state_session_override_uses_session_root() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = fake_claude_home(h5i_root);
        let m = canonical_manifest("claude", "obs");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let session_home = m.dir(h5i_root).join("ro/4242/home");
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), Some(&session_home)).unwrap();

        // Backing copies were seeded under the SESSION root, not <env>/home.
        assert!(session_home.join(".claude/.credentials.json").exists());
        assert!(!m.dir(h5i_root).join("home").exists());
        // Every home bind's backing is under the session root.
        assert!(!pol.home_binds.is_empty());
        for b in &pol.home_binds {
            assert!(
                b.backing.starts_with(&session_home),
                "backing {:?} must be under the per-session root",
                b.backing
            );
        }
    }

    /// Under `--readonly`, the in-box git surface is granted read-only: the
    /// worktree-writable git dirs a read-write session would get (the admin
    /// `worktrees/<wt>` dir, `objects`, the env branch) are all read grants, so
    /// the box cannot commit or rewrite refs.
    #[cfg(unix)]
    #[test]
    fn grant_box_git_readonly_grants_no_writable_git_paths() {
        use crate::sandbox::Profile;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let repo = git2::Repository::init(&root).unwrap();
        let git_dir = repo.commondir().to_path_buf();
        let m = canonical_manifest("claude", "fix");
        let work = root.join(".git/.h5i/env/claude/fix/work");
        std::fs::create_dir_all(&work).unwrap();

        let git_writes = |pol: &ResolvedPolicy| -> Vec<String> {
            pol.profile
                .fs_write
                .iter()
                .filter(|w| Path::new(w).starts_with(&git_dir))
                .cloned()
                .collect()
        };

        // Control: a read-write session gets writable git paths (objects, its
        // env ref ns, the worktree admin dir).
        let mut rw = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut rw, false).unwrap();
        assert!(
            !git_writes(&rw).is_empty(),
            "a read-write session must get writable git paths (control)"
        );

        // Observer: every git-surface grant is read-only.
        let mut ro = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        grant_box_git(&repo, &m, &work, &mut ro, true).unwrap();
        assert!(
            git_writes(&ro).is_empty(),
            "a read-only observer must get NO writable git paths: {:?}",
            git_writes(&ro)
        );
    }

    // ─── per-env credential/session isolation (#1) ──────────────────────────

    /// Build a fake host HOME with the Claude runtime's state: a `.claude` dir
    /// (with a 0600 credentials file) and a `.claude.json` session file.
    #[cfg(unix)]
    fn fake_claude_home(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let cred = home.join(".claude/.credentials.json");
        std::fs::write(&cred, "{\"token\":\"real-secret\"}").unwrap();
        std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(home.join(".claude.json"), "{\"session\":1}").unwrap();
        home
    }

    /// Build a fake host HOME with the Codex runtime's state: a `.codex` dir
    /// with auth/config plus large transcript/log/temp caches.
    #[cfg(unix)]
    fn fake_codex_home(root: &Path) -> PathBuf {
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), "{\"token\":\"real-secret\"}").unwrap();
        std::fs::write(home.join(".codex/config.toml"), "model = \"gpt-5\"\n").unwrap();
        home
    }

    #[cfg(unix)]
    #[test]
    fn prepare_home_state_redirects_agent_creds_to_per_env_copy() {
        use crate::sandbox::{AgentRuntime, Profile};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = fake_claude_home(h5i_root);
        let m = canonical_manifest("claude", "auth");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        // Both state paths redirected to per-env backing copies under <env>/home.
        assert_eq!(pol.home_binds.len(), 2, "{:?}", pol.home_binds);
        let backing_root = m.dir(h5i_root).join("home");
        for b in &pol.home_binds {
            assert!(b.backing.starts_with(&backing_root), "{:?}", b.backing);
        }
        let claude = pol
            .home_binds
            .iter()
            .find(|b| b.target == home.join(".claude"))
            .unwrap();
        // Copy-in actually happened, content + mode preserved.
        let copied = claude.backing.join(".credentials.json");
        assert_eq!(
            std::fs::read_to_string(&copied).unwrap(),
            "{\"token\":\"real-secret\"}"
        );
        assert_eq!(
            std::fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            0o600,
            "credential mode must survive the copy-in"
        );

        // The real-HOME grants are dropped; the backing copies are granted instead.
        assert!(!pol.profile.fs_write.iter().any(|w| w == "~/.claude"));
        assert!(!pol.profile.fs_write.iter().any(|w| w == "~/.claude.json"));
        assert!(pol
            .profile
            .fs_write
            .iter()
            .any(|w| w == &claude.backing.display().to_string()));

        // The real HOME is never written — its files are exactly as seeded.
        assert_eq!(
            std::fs::read_to_string(home.join(".claude.json")).unwrap(),
            "{\"session\":1}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_home_state_seed_prunes_bloat_keeps_credentials() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = fake_claude_home(h5i_root);
        // Add a large non-credential tree (transcripts) that must NOT be seeded,
        // plus a settings file that must be.
        std::fs::create_dir_all(home.join(".claude/projects/some-proj")).unwrap();
        std::fs::write(
            home.join(".claude/projects/some-proj/session.jsonl"),
            "transcript",
        )
        .unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{\"k\":1}").unwrap();
        let m = canonical_manifest("claude", "auth");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        let backing = m.dir(h5i_root).join("home/.claude");
        // Credentials + settings seeded.
        assert!(backing.join(".credentials.json").exists());
        assert!(backing.join("settings.json").exists());
        // The transcript tree was pruned — not copied into the box seed.
        assert!(
            !backing.join("projects").exists(),
            "the large projects/ tree must be pruned from the per-env seed"
        );
        // The real HOME still has its transcripts (only ever read, never touched).
        assert!(home.join(".claude/projects/some-proj/session.jsonl").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_home_state_seed_prunes_codex_bloat_keeps_credentials() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = fake_codex_home(h5i_root);
        for path in [
            ".codex/sessions/2026/07/session.jsonl",
            ".codex/log/run.log",
            ".codex/shell_snapshots/snap.sh",
            ".codex/.tmp/plugins/cache.bin",
            ".codex/tmp/arg0/file",
        ] {
            let path = home.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "cache").unwrap();
        }
        for path in [
            ".codex/history.jsonl",
            ".codex/logs_2.sqlite",
            ".codex/logs_2.sqlite-shm",
            ".codex/logs_2.sqlite-wal",
        ] {
            std::fs::write(home.join(path), "cache").unwrap();
        }
        std::fs::create_dir_all(home.join(".codex/rules")).unwrap();
        std::fs::write(home.join(".codex/rules/default.rules"), "rules").unwrap();
        let m = canonical_manifest("codex", "auth");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Codex),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        let backing = m.dir(h5i_root).join("home/.codex");
        assert!(backing.join("auth.json").exists());
        assert!(backing.join("config.toml").exists());
        assert!(backing.join("rules/default.rules").exists());
        for pruned in [
            "sessions",
            "log",
            "shell_snapshots",
            ".tmp",
            "tmp",
            "history.jsonl",
            "logs_2.sqlite",
            "logs_2.sqlite-shm",
            "logs_2.sqlite-wal",
        ] {
            assert!(
                !backing.join(pruned).exists(),
                "Codex HOME seed should prune {pruned}"
            );
        }
        assert!(home.join(".codex/sessions/2026/07/session.jsonl").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_home_state_persists_and_does_not_reseed() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = fake_claude_home(h5i_root);
        let m = canonical_manifest("claude", "auth");
        // Pre-seed the backing with in-box state (a token refreshed by a prior run
        // of this env) — prepare must NOT clobber it from the real HOME.
        let backing = m.dir(h5i_root).join("home/.claude.json");
        std::fs::create_dir_all(backing.parent().unwrap()).unwrap();
        std::fs::write(&backing, "{\"session\":99}").unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        assert_eq!(
            std::fs::read_to_string(&backing).unwrap(),
            "{\"session\":99}",
            "an existing per-env copy must persist, not be re-seeded from real HOME"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_home_state_skips_missing_paths_and_keeps_direct_grant() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        // HOME has .claude but NO .claude.json — the missing one keeps its grant.
        let home = h5i_root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let m = canonical_manifest("claude", "auth");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        assert_eq!(
            pol.home_binds.len(),
            1,
            "only the existing path is redirected"
        );
        // The missing path is left as today's direct grant (never created in HOME).
        assert!(pol.profile.fs_write.iter().any(|w| w == "~/.claude.json"));
        assert!(!home.join(".claude.json").exists());
    }

    #[test]
    fn prepare_home_state_is_noop_for_non_agent_profiles() {
        use crate::sandbox::Profile;
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let m = canonical_manifest("claude", "build");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            Profile::builtin("default", IsolationClaim::Supervised),
        );
        let before = pol.profile.fs_write.clone();

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        assert!(pol.home_binds.is_empty());
        assert_eq!(
            pol.profile.fs_write, before,
            "non-agent fs_write must be untouched"
        );
    }

    #[test]
    fn prepare_home_state_is_noop_at_workspace_tier() {
        use crate::sandbox::{AgentRuntime, Profile};
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let m = canonical_manifest("claude", "auth");
        std::fs::create_dir_all(m.dir(h5i_root)).unwrap();
        // Workspace tier has no mount namespace to bind in — must stay a no-op.
        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Workspace,
            Profile::builtin_agent(IsolationClaim::Workspace, AgentRuntime::Claude),
        );

        prepare_home_state(h5i_root, &m, &mut pol, Some(&home), None).unwrap();

        assert!(pol.home_binds.is_empty());
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
            apply(&repo, h5i_root, &mut m, false).map(|_| String::new()),
            "apply",
        );
        let e = abort(&repo, h5i_root, &mut m).expect_err("abort");
        assert!(format!("{e}").contains("busy"), "abort: {e}");
    }

    /// The box's staging window is its spool and nothing above it. This is the
    /// whole receipt-integrity argument: `receipt.jsonl` and `receipts/` are
    /// siblings of `spool/`, so a grant that stops at the spool cannot reach a
    /// record the host already wrote.
    #[test]
    fn the_capture_spool_grant_never_reaches_the_receipt_store() {
        let td = tempfile::tempdir().unwrap();
        let h5i_root = td.path();
        let m = canonical_manifest("claude", "sealed");
        let mut policy = ResolvedPolicy::new(
            IsolationClaim::Process,
            crate::sandbox::Profile::builtin("default", IsolationClaim::Process),
        );

        let before = policy.profile.fs_write.len();
        prepare_env_capture_spool(h5i_root, &m, &mut policy).unwrap();
        let added: Vec<&String> = policy.profile.fs_write[before..].iter().collect();
        assert_eq!(added.len(), 1, "exactly one grant is added: {added:?}");

        let granted = std::path::Path::new(added[0]);
        let dir = env_dir(h5i_root, &m.agent, &m.slug);
        assert_eq!(granted, dir.join("spool"));
        assert!(!dir.join("receipt.jsonl").starts_with(granted));
        assert!(!dir.join("receipts").starts_with(granted));
    }

    /// The shim used to carry its own hand-written list of Chrome locations,
    /// and on Linux it only ever checked three `/usr/bin` paths. A host whose
    /// Chrome was a Playwright build — a path the profile explicitly grants —
    /// passed `create` and then failed in the box with "no Chrome/Chromium
    /// found". The list now comes from the sandbox crate; this pins that.
    #[test]
    fn the_shim_looks_everywhere_the_box_is_allowed_to_look() {
        let script = browser_shim_source("/usr/local/bin/agent-browser");
        for pattern in crate::sandbox::chrome_exec_patterns() {
            let word = shell_glob_word(pattern);
            assert!(
                script.contains(&word),
                "the shim would never try {pattern}\n{script}"
            );
        }
        // The exact shape this regressed on.
        assert!(script.contains(r#""$HOME"/.cache/ms-playwright/chromium-*/chrome-linux/chrome"#));
    }

    /// Per-segment quoting: a glob that gets quoted stops globbing, and a path
    /// with spaces that does not get quoted becomes several words.
    #[test]
    fn shell_words_quote_spaces_and_leave_globs_bare() {
        assert_eq!(
            shell_glob_word("~/.cache/ms-playwright/chromium-*/chrome-linux/chrome"),
            r#""$HOME"/.cache/ms-playwright/chromium-*/chrome-linux/chrome"#
        );
        assert_eq!(
            shell_glob_word("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            r#"/Applications/"Google Chrome.app"/Contents/MacOS/"Google Chrome""#
        );
        assert_eq!(shell_glob_word("/usr/bin/chromium"), "/usr/bin/chromium");
    }

    /// The list is generated into a shell script, so a bad word is a shim that
    /// does not parse — and the box would see a syntax error instead of a
    /// browser. `sh -n` is the same parser that will run it.
    /// The port files live under a directory the box can write (it is where the
    /// box's own Chrome records its pid and port), so a read-back value is
    /// box-controlled and has to be checked rather than handed to `bind`.
    #[test]
    fn a_remembered_port_is_reused_but_never_a_nonsense_one() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("port");

        // Drawn once, then reused verbatim.
        let first = remembered_port(&f, "test", &[]).unwrap();
        assert!(first >= 1024);
        assert_eq!(remembered_port(&f, "test", &[]).unwrap(), first);

        // `0` asks bind for an ephemeral port under the name of a pinned one,
        // and a privileged port cannot be bound at all: both are redrawn, and
        // the redraw is written back so it sticks.
        for bad in ["0", "80", "not-a-port", ""] {
            std::fs::write(&f, bad).unwrap();
            let p = remembered_port(&f, "test", &[]).unwrap();
            assert!(p >= 1024, "{bad} must not be honoured, got {p}");
            assert_eq!(std::fs::read_to_string(&f).unwrap(), p.to_string());
        }

        // A port this env already holds is redrawn too — an allocation is a
        // bind-and-drop, so the same number can come back twice.
        let held = remembered_port(&f, "test", &[]).unwrap();
        let other = remembered_port(&f, "test", &[held]).unwrap();
        assert_ne!(other, held);
    }

    /// The box cannot stop its own browser once that browser has outlived the
    /// run that started it (it is in a previous sandbox instance, which
    /// Seatbelt's same-sandbox signal grant does not reach), so it leaves a
    /// marker and the host does it here.
    #[test]
    fn a_marked_browser_is_stopped_host_side_and_the_marker_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        // The profile dir is what says this env ever had a browser at all.
        std::fs::create_dir_all(state.join("chrome")).unwrap();

        // No marker: nothing is touched, including the pid file.
        std::fs::write(state.join("chrome.pid"), "1").unwrap();
        std::fs::write(state.join("chrome.proxy"), "--proxy-server=x").unwrap();
        stop_stale_browser(state);
        assert!(state.join("chrome.proxy").exists(), "no marker → no action");
        assert!(state.join("chrome.pid").exists());

        // Marked, and nothing of this box's is running: the recorded route goes
        // with the marker, so the next Chrome is not mistaken for one launched
        // with a route it never saw — and the pid goes too, because the browser
        // is now *confirmed* gone.
        std::fs::write(state.join("chrome.restart"), "--proxy-server=y").unwrap();
        stop_stale_browser(state);
        assert!(!state.join("chrome.restart").exists());
        assert!(!state.join("chrome.proxy").exists());
        assert!(!state.join("chrome.pid").exists());

        // Idempotent, and harmless with no pid recorded at all.
        std::fs::write(state.join("chrome.restart"), "").unwrap();
        stop_stale_browser(state);
        assert!(!state.join("chrome.restart").exists());
    }

    /// A long-lived stand-in for the box's browser: an executable whose name
    /// reads as Chrome, running on this env's profile dir — the two things the
    /// host matches on.
    ///
    /// `/bin/sh -c '…'` and not `sleep` directly: `sleep` rejects the
    /// `--user-data-dir` argument and exits immediately, which would leave every
    /// assertion below true because nothing was ever running. The script is two
    /// commands for a second reason of the same kind — a shell given a single
    /// one `exec`s it, replacing itself with a `sleep` whose argv no longer
    /// carries the flag the host matches on.
    ///
    /// Returns the pid and a thread that reaps it. The stand-in is this test's
    /// own child, so without a `wait` it lingers as a zombie that `kill(pid, 0)`
    /// still finds, and the stop under test would burn its full SIGTERM and
    /// SIGKILL timeouts before giving up on a process that is already dead.
    #[cfg(unix)]
    fn spawn_fake_browser(exe: &Path, profile: &Path) -> (i32, std::thread::JoinHandle<()>) {
        let child = std::process::Command::new(exe)
            .args(["-c", "sleep 10; true"])
            .arg(format!("--user-data-dir={}", profile.display()))
            .spawn()
            .expect("spawn the stand-in browser");
        let pid = child.id() as i32;
        let reaper = std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
        (pid, reaper)
    }

    /// Every process whose argv names this profile dir, with none of
    /// `browser_pids`'s judgement applied — the positive control for the tests
    /// below, so a negative result cannot pass because `ps` had not caught up.
    #[cfg(unix)]
    fn pids_naming_profile(profile: &Path) -> Vec<i32> {
        let needle = format!("--user-data-dir={}", profile.display());
        let out = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,command="])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim_start().split_once(char::is_whitespace))
            .filter(|(_, cmd)| cmd.contains(&needle))
            .filter_map(|(pid, _)| pid.parse::<i32>().ok())
            .collect()
    }

    /// Poll `f` for up to ~2s. Returns whether it ever held.
    #[cfg(unix)]
    fn eventually(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..40 {
            if f() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// `pid_gone` answers "no such process", not "the call failed". The
    /// difference is `EPERM` — a process that exists and is simply not ours to
    /// signal — and reporting that as gone would let `signal_and_wait` claim it
    /// stopped a browser it never touched.
    ///
    /// No second uid needed: pid 1 is normally unsignalable by an ordinary
    /// process, and a reaped child's pid is genuinely absent. Where pid 1 *is*
    /// signalable by us — as root, and equally in a PID namespace whose init
    /// shares this uid — `kill(1, 0)` succeeds instead of failing, both readings
    /// answer "alive", and this degrades to a tautology rather than a false
    /// failure. The note it prints says which case it landed in, so a silent
    /// tautology is not mistaken for coverage.
    #[cfg(unix)]
    #[test]
    fn pid_gone_tells_no_such_process_apart_from_not_allowed() {
        // ESRCH: a child that has exited *and* been reaped, so the pid is free.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead = child.id() as i32;
        child.wait().unwrap();
        assert!(pid_gone(dead), "a reaped child's pid is gone");

        // EPERM: pid 1 is alive and not ours. The `kill` is made here as well as
        // inside `pid_gone` so the test can say which case it actually covered.
        if unsafe { libc::kill(1, 0) } == 0 {
            eprintln!(
                "note: pid 1 is signalable here (root, or a PID namespace sharing this uid) \
                 — the EPERM half of this test is inert"
            );
        } else {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM),
                "expected pid 1 to be unsignalable, not absent"
            );
        }
        assert!(
            !pid_gone(1),
            "a process we may not signal is alive, not gone"
        );
    }

    /// The case the restart exists for: a browser started by a shim from before
    /// `detach` `exec`'d, so `chrome.pid` names the *launcher* and not Chrome.
    /// A pid-keyed lookup finds nothing there — and "nothing to stop" would mean
    /// the warning repeats on every run forever, which is the opposite of what
    /// the manual promises.
    #[cfg(unix)]
    #[test]
    fn a_browser_recorded_under_the_wrong_pid_is_still_found_and_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let profile = state.join("chrome");
        std::fs::create_dir_all(&profile).unwrap();

        let exe = state.join("chrome-for-testing");
        std::os::unix::fs::symlink("/bin/sh", &exe).unwrap();
        let (pid, reaper) = spawn_fake_browser(&exe, &profile);
        // The test only means anything if the stand-in is actually running and
        // actually recognised before the stop is asked for.
        assert!(
            eventually(|| browser_pids(&profile).unwrap().contains(&pid)),
            "the stand-in browser was never running, so this test asserts nothing"
        );

        // What the old shim left behind: a launcher's pid, long since exited or
        // belonging to something that is not the browser.
        std::fs::write(state.join("chrome.pid"), "999999").unwrap();
        std::fs::write(state.join("chrome.restart"), "--proxy-server=x").unwrap();

        stop_stale_browser(state);

        assert!(
            browser_pids(&profile).unwrap().is_empty(),
            "the browser must be stopped even though the recorded pid was wrong"
        );
        assert!(
            !state.join("chrome.pid").exists(),
            "confirmed gone → cleared"
        );
        assert!(!state.join("chrome.restart").exists());
        reaper.join().unwrap();
    }

    /// A guard against reintroducing a pid-keyed lookup. Under the current
    /// design nothing reads `chrome.pid` to decide what to signal, so `-1`
    /// cannot reach `kill` by construction — but it could once, and `kill(-1, …)`
    /// signals every process this user owns.
    #[cfg(unix)]
    #[test]
    fn a_hostile_pid_file_never_becomes_a_signal() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        std::fs::create_dir_all(state.join("chrome")).unwrap();

        for hostile in ["-1", "0", "1", "-999", "not-a-pid", ""] {
            std::fs::write(state.join("chrome.pid"), hostile).unwrap();
            std::fs::write(state.join("chrome.restart"), "").unwrap();
            stop_stale_browser(state);
        }
        assert!(!state.join("chrome.restart").exists());
    }

    /// A process that merely *mentions* the profile path is not the browser — a
    /// `pkill`, an editor, or this project's own tests would otherwise match.
    #[cfg(unix)]
    #[test]
    fn only_a_browser_binary_matches_the_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("chrome");
        std::fs::create_dir_all(&profile).unwrap();

        let (pid, reaper) = spawn_fake_browser(Path::new("/bin/sh"), &profile);
        // Positive control first: the process is running and `ps` can see it
        // naming this profile dir. Without this the assertion below would pass
        // just as happily against a process that never started.
        assert!(
            eventually(|| pids_naming_profile(&profile).contains(&pid)),
            "the stand-in was never visible, so the rejection below proves nothing"
        );
        assert!(
            browser_pids(&profile).unwrap().is_empty(),
            "a shell naming the flag is not this box's browser"
        );

        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        reaper.join().unwrap();
    }

    #[test]
    fn the_generated_shim_is_valid_shell() {
        let script = browser_shim_source("/usr/local/bin/agent-browser");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent-browser");
        std::fs::write(&path, &script).expect("write");

        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg(&path)
            .output()
            .expect("run sh -n");
        assert!(
            out.status.success(),
            "shim does not parse: {}\n{script}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Chrome ignores the environment's proxy settings on macOS, so the shim has
    /// to pass `--proxy-server` explicitly — and it must key that off h5i's own
    /// variable, not `HTTPS_PROXY`, which anything in the box may set for its own
    /// reasons. Gated, so a tier that runs no proxy (Linux supervised: nftables,
    /// direct connects) launches Chrome exactly as it did before.
    #[test]
    fn the_shim_passes_chrome_the_proxy_only_when_the_tier_set_one() {
        let script = browser_shim_source("/usr/local/bin/agent-browser");
        assert!(
            script.contains(&format!("--proxy-server=${}", sandbox::EGRESS_PROXY_VAR)),
            "{script}"
        );
        assert!(
            script.contains(&format!("[ -n \"${{{}:-}}\" ]", sandbox::EGRESS_PROXY_VAR)),
            "the flag must be gated on the variable: {script}"
        );
        // The prose above the gate still names `HTTPS_PROXY` (it explains why
        // Chrome ignoring it is the problem); what must not appear is a *use* of
        // it — those vars are box-settable, so the shim cannot key off them.
        assert!(
            !script.contains("$HTTPS_PROXY") && !script.contains("${HTTPS_PROXY"),
            "the shim must not read the conventional proxy vars: {script}"
        );

        // A Chrome that predates the current route must not be left
        // alive-but-unreachable: it records what it was launched with, and a
        // mismatch leaves the marker the host acts on before the next run.
        assert!(script.contains("chrome.proxy"), "{script}");
        assert!(
            script.contains("[ \"$PROXY_ARG\" != \"$HAD\" ]"),
            "the recorded route must be compared against the current one: {script}"
        );
        assert!(
            script.contains("chrome.restart"),
            "a mismatch must be recorded for the host: {script}"
        );
        // The pid recorded has to be Chrome's own, or every later `kill` of it
        // (the host restart, `box rm`) reaches a launcher instead.
        assert!(script.contains("exec setsid"), "{script}");
        assert!(script.contains("exec perl"), "{script}");

        // And it is genuinely absent when the variable is not set: run the
        // gate itself under `sh` and read back what it would have passed.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gate.sh");
        let gate: String = script
            .lines()
            .skip_while(|l| !l.starts_with("PROXY_ARG="))
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{gate}\nprintf '%s' \"${{PROXY_ARG:-}}\"\n")).unwrap();

        let unset = std::process::Command::new("sh")
            .arg(&path)
            .env_remove(sandbox::EGRESS_PROXY_VAR)
            .output()
            .expect("run gate");
        assert!(unset.stdout.is_empty(), "{:?}", unset.stdout);

        let set = std::process::Command::new("sh")
            .arg(&path)
            .env(sandbox::EGRESS_PROXY_VAR, "http://127.0.0.1:8123")
            .output()
            .expect("run gate");
        assert_eq!(
            String::from_utf8_lossy(&set.stdout),
            "--proxy-server=http://127.0.0.1:8123"
        );
    }

    #[test]
    fn browser_env_allows_loopback_and_the_policy_egress_only() {
        let mut policy = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            crate::sandbox::Profile::builtin("browser", IsolationClaim::Supervised),
        );
        policy.profile.net_egress = vec!["api.anthropic.com".into(), "example.com:8443".into()];

        let env: std::collections::HashMap<String, String> =
            browser_env(&policy, None).into_iter().collect();
        let allowed = &env["AGENT_BROWSER_ALLOWED_DOMAINS"];

        // The dev server under test never appears in an egress allowlist, so
        // loopback is always added.
        assert!(allowed.contains("localhost"), "{allowed}");
        assert!(allowed.contains("127.0.0.1"), "{allowed}");
        // Policy hosts carry over, with any :port stripped.
        assert!(allowed.contains("api.anthropic.com"), "{allowed}");
        assert!(allowed.contains("example.com"), "{allowed}");
        assert!(!allowed.contains("8443"), "ports are not domains: {allowed}");
        // Nothing else.
        assert!(!allowed.contains("github.com"), "{allowed}");
    }

    #[test]
    fn browser_env_refuses_the_ai_gateway_rather_than_omitting_it() {
        let policy = ResolvedPolicy::new(
            IsolationClaim::Supervised,
            crate::sandbox::Profile::builtin("browser", IsolationClaim::Supervised),
        );
        let env: std::collections::HashMap<String, String> =
            browser_env(&policy, None).into_iter().collect();

        // Absent, not empty. agent-browser tests for presence, so an empty
        // value would *enable* chat — the opposite of the intent, and exactly
        // what a box reported before this was fixed.
        assert!(!env.iter().any(|(k, _)| k == "AI_GATEWAY_API_KEY"));
        // And nothing that only *looks* like it turns chat off. There is no
        // `AGENT_BROWSER_DISABLE_CHAT` upstream, so setting one would be a
        // policy line that reviews as enforcement and enforces nothing.
        assert!(!env.iter().any(|(k, _)| k == "AGENT_BROWSER_DISABLE_CHAT"));
        // Headless is spelled by pinning the variable agent-browser actually
        // reads to a falsey value — there is no `AGENT_BROWSER_HEADLESS`.
        assert_eq!(env.get("AGENT_BROWSER_HEADED").map(String::as_str), Some("0"));
        assert!(!env.iter().any(|(k, _)| k == "AGENT_BROWSER_HEADLESS"));
        // The daemon socket must land somewhere the box can write; its default
        // ($XDG_RUNTIME_DIR) is not granted on any tier. The literal `/tmp` is
        // the right answer only where `/tmp` is bind-mounted per env — on macOS
        // it is denied outright and the per-env backing is the writable path,
        // so the expectation follows [`box_tmp_root`] rather than a constant.
        let expected = format!("{}/agent-browser", box_tmp_root(&policy));
        assert_eq!(
            env.get("AGENT_BROWSER_SOCKET_DIR").map(String::as_str),
            Some(expected.as_str())
        );
        // And once a private `/tmp` redirect exists, macOS must follow it rather
        // than the literal `/tmp`, which is denied there. Conditional because a
        // policy without the redirect legitimately falls back to `/tmp` — that
        // is [`box_tmp_root`]'s documented default, not a bug to assert against.
        if policy.home_binds.iter().any(|b| b.target == Path::new("/tmp")) {
            assert!(
                !cfg!(target_os = "macos") || !expected.starts_with("/tmp/agent-browser"),
                "macOS must not point the socket at the denied literal /tmp: {expected}"
            );
        }
    }

    #[test]
    fn the_home_seed_leaves_credentials_behind() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real");
        let copy = td.path().join("copy");
        std::fs::create_dir_all(real.join("plugins/inner")).unwrap();

        // State the box legitimately needs …
        std::fs::write(real.join("settings.json"), b"{}").unwrap();
        // … and credentials it does not, at the top level and buried.
        std::fs::write(real.join("credentials.json"), b"secret").unwrap();
        std::fs::write(real.join(".netrc"), b"machine x").unwrap();
        std::fs::write(real.join("deploy.pem"), b"-----BEGIN").unwrap();
        std::fs::write(real.join("plugins/inner/id_ed25519"), b"key").unwrap();

        seed_home_copy(&real, &copy).unwrap();

        assert!(copy.join("settings.json").is_file(), "state is seeded");
        for gone in [
            "credentials.json",
            ".netrc",
            "deploy.pem",
            "plugins/inner/id_ed25519",
        ] {
            assert!(
                !copy.join(gone).exists(),
                "credential-shaped entry was seeded into the box: {gone}"
            );
        }
    }

    #[test]
    fn credential_shapes_are_matched_by_name_and_extension() {
        for yes in [
            "credentials",
            "CREDENTIALS.TOML",
            ".netrc",
            "id_rsa",
            "server.pem",
            "tls.key",
            "bundle.p12",
            "backup_ed25519",
        ] {
            assert!(is_credential_shaped(yes), "should be refused: {yes}");
        }
        for no in ["settings.json", "config.toml", "history.jsonl", "notes.md"] {
            assert!(!is_credential_shaped(no), "should be seeded: {no}");
        }
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
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "d".repeat(64),
            isolation_claim: "workspace".into(),
            backend: "worktree".into(),
            created_at: now_ts(),
            updated_at: now_ts(),
            status: ST_CREATED.into(),
            captures: vec!["cap1".into()],
            service_digest: None,
            persona_digest: None,
            pr: None,
            pr_head_ref: None,
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
        std::fs::write(
            work.join("examples/sub/.git"),
            "gitdir: ../.git/modules/sub\n",
        )
        .unwrap();
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
        assert_eq!(
            v.len(),
            1,
            "only the unregistered nested repo flagged: {v:?}"
        );
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
                source: "repo".into(),
                profile: "default".into(),
                policy_digest: "d".repeat(64),
                isolation_claim: "workspace".into(),
                backend: "worktree".into(),
                created_at: now_ts(),
                updated_at: now_ts(),
                status: ST_CREATED.into(),
                captures: Vec::new(),
                service_digest: None,
                persona_digest: None,
                pr: None,
                pr_head_ref: None,
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

    // ── build_branch_scoped_merge / scoped_code_branch_refs ─────────────────

    fn manifest_on_branch(agent: &str, slug: &str, parent_branch: &str) -> EnvManifest {
        let mut m = canonical_manifest(agent, slug);
        m.parent_branch = parent_branch.into();
        m
    }

    fn write_env(repo: &Repository, m: &EnvManifest) {
        append_env_commit(
            repo,
            &EnvEvent {
                ts: now_ts(),
                env_id: m.id.clone(),
                agent: m.agent.clone(),
                event: "create".into(),
                detail: None,
                capture: None,
            },
            Some(m),
            Some("# policy\n"),
        )
        .unwrap();
    }

    fn manifest_ids_in(repo: &Repository, oid: git2::Oid) -> Vec<String> {
        let tree = repo.find_commit(oid).unwrap().tree().unwrap();
        let raw =
            crate::refstore::read_blob_from_tree(repo, Some(&tree), MANIFESTS_FILE).unwrap_or_default();
        let mut ids: Vec<String> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<EnvManifest>(l).unwrap().id)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn scoped_merge_keeps_only_envs_forked_from_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_env(&repo, &manifest_on_branch("claude", "feat-work", "feature"));
        write_env(&repo, &manifest_on_branch("claude", "main-work", "main"));

        let oid = build_branch_scoped_merge(&repo, "feature", None)
            .unwrap()
            .expect("feature has an env");
        assert_eq!(manifest_ids_in(&repo, oid), vec!["env/claude/feat-work"]);
    }

    #[test]
    fn scoped_merge_preserves_envs_already_on_base() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        // "remote" base has another branch's env.
        write_env(&repo, &manifest_on_branch("codex", "other-work", "other"));
        let base = repo.refname_to_id(ENV_REF).unwrap();
        // Local adds a feature env (and an unrelated main env).
        write_env(&repo, &manifest_on_branch("claude", "feat-work", "feature"));
        write_env(&repo, &manifest_on_branch("claude", "main-work", "main"));

        let oid = build_branch_scoped_merge(&repo, "feature", Some(base))
            .unwrap()
            .unwrap();
        assert_eq!(
            manifest_ids_in(&repo, oid),
            vec!["env/claude/feat-work", "env/codex/other-work"],
            "base env preserved, feature added, unrelated main excluded"
        );
        assert_eq!(repo.find_commit(oid).unwrap().parent_id(0).unwrap(), base);
    }

    #[test]
    fn scoped_code_branch_refs_lists_only_matching_envs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_env(&repo, &manifest_on_branch("claude", "feat-work", "feature"));
        write_env(&repo, &manifest_on_branch("claude", "main-work", "main"));

        let refs = scoped_code_branch_refs(&repo, "feature");
        assert_eq!(refs, vec!["refs/heads/h5i/env/claude/feat-work"]);
    }

    #[test]
    fn scoped_merge_none_when_no_env_for_branch_and_no_base() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_env(&repo, &manifest_on_branch("claude", "main-work", "main"));
        assert!(build_branch_scoped_merge(&repo, "feature", None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn materialize_persona_concatenates_excludes_and_digests() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path();
        git2::Repository::init(work).unwrap();
        std::fs::create_dir_all(work.join("plugin/persona")).unwrap();
        std::fs::write(work.join("plugin/persona/architect.md"), "# Architect\nThink first.\n").unwrap();
        std::fs::write(work.join("plugin/persona/careful.md"), "Be careful.\n").unwrap();

        // Empty list → no file, no digest.
        assert_eq!(materialize_persona(work, &[]).unwrap(), None);
        assert!(!work.join(PERSONA_FILE).exists());

        // Two sources → concatenated in order with per-source headers.
        let sources = vec![
            "plugin/persona/architect.md".to_string(),
            "plugin/persona/careful.md".to_string(),
        ];
        let digest = materialize_persona(work, &sources).unwrap().expect("a digest");
        let body = std::fs::read_to_string(work.join(PERSONA_FILE)).unwrap();
        assert!(body.contains("<!-- persona: plugin/persona/architect.md -->"));
        assert!(body.contains("# Architect"));
        // Order is preserved: architect appears before careful.
        assert!(body.find("# Architect").unwrap() < body.find("Be careful.").unwrap());
        assert_eq!(digest, crate::refstore::sha256_hex(body.as_bytes()));

        // PERSONA.md is git-excluded so it never shows as a worktree change.
        let exclude =
            std::fs::read_to_string(work.join(".git/info/exclude")).unwrap_or_default();
        assert!(exclude.lines().any(|l| l.trim() == "/PERSONA.md"));
        let wt = Repository::open(work).unwrap();
        assert!(wt.status_should_ignore(Path::new(PERSONA_FILE)).unwrap());

        // A missing source fails closed.
        assert!(materialize_persona(work, &["plugin/persona/nope.md".to_string()]).is_err());
    }
}
