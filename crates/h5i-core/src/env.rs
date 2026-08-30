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
/// Capture-spool subdir under the env admin dir; the box's one writable window.
const ENV_SPOOL_DIR: &str = "spool";
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

/// Serializes **service** operations for one box, and nothing else.
///
/// Distinct from [`RunLock`] on purpose: that one is held by an `env shell` for
/// the whole interactive session, so serializing services on it meant
/// `box service start` failed outright whenever an agent session was open — at
/// every tier, including the kernel ones with no guest to race over. What
/// actually needs serializing is two service operations touching one box's
/// records; guest creation serializes itself in the sandbox layer.
///
/// Blocking rather than fail-fast: these operations are short, and a caller
/// that waits 40 ms is better than one that tells the user to try again.
struct ServiceLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

impl ServiceLock {
    fn acquire(env_dir: &Path) -> Result<Self, H5iError> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            std::fs::create_dir_all(env_dir).map_err(|e| H5iError::with_path(e, env_dir))?;
            let path = env_dir.join("services.lock");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|e| H5iError::with_path(e, &path))?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(H5iError::Io(std::io::Error::last_os_error()));
            }
            Ok(ServiceLock { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = env_dir;
            Ok(ServiceLock {})
        }
    }
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
        Self::flock(
            env_dir,
            RUN_LOCK_FILE,
            LockMode::Exclusive,
            LockRole::Writer,
        )
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
                    LockRole::Writer => {
                        "environment is busy — another `h5i box run`/`shell` \
                         or lifecycle op (propose/apply/rebase/abort) holds it"
                    }
                    // A teardown holds `observers.lock` exclusively.
                    LockRole::Observer => {
                        "environment is being torn down (gc/rm) — a \
                         `--readonly` observer can attach only once that completes"
                    }
                    // Live read-only observers hold `observers.lock` shared.
                    LockRole::Teardown => {
                        "environment is busy — it has live `--readonly` \
                         observer session(s); this op removes the worktree and can proceed only \
                         once every observer exits"
                    }
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
    /// sha256 of `policy.effective.json` as written at create — the enforced
    /// kernel-tier configuration for the canonical captured-run shape
    /// (ROADMAP.md §P1). `None` for tiers with no kernel-mechanism dump
    /// (workspace/container/microvm, and everything off Linux) and for envs
    /// from before it existed. Runs rewrite the file at the apply seam and pin
    /// that run's digest in its capture record; this is the create-time
    /// baseline, so a difference between the two is host drift made visible,
    /// not tampering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_digest: Option<String>,
    /// The filesystem-authority validator's verdict on the create-time
    /// effective config (ROADMAP.md §P2): the effective grants are a subset
    /// of the declared policy, writes were declared writable, no read-only
    /// overlay is writable, and (Unix) no grant escapes the worktree by a
    /// symlink. `None` for tiers with no kernel-mechanism dump and for envs
    /// from before it existed. Rendered in `box status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_authority: Option<crate::fs_authority::AuthorityVerdict>,
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
    /// The machine this box runs on, when it is not this one: the SHA-256 of
    /// that runner's pinned SSH host key (ROADMAP.md R6).
    ///
    /// Identity rather than a label. A name can be re-pointed at different
    /// hardware tomorrow, and a box bound to one would silently follow; a host
    /// key cannot be, so a reinstalled machine is honestly a different runner.
    /// Validated as an object id on import, beside `base_commit` and
    /// `policy_digest`, because it decides which machine a later operation
    /// talks to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// That runner's display name, for humans and command lines. Never
    /// identity: see `runner_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
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
    // The three object-id fields, because every surface that shows a manifest
    // abbreviates them and an abbreviation is a slice. `create` writes a git
    // OID and a sha256; a peer's ref can carry whatever it likes, and the
    // caller has already committed to *skipping* a bad manifest rather than
    // aborting the sync — so anything that is not an id is refused here rather
    // than left to panic in a renderer three commands later.
    let mut ids: Vec<(&str, &String)> = vec![
        ("base_commit", &m.base_commit),
        ("base_tree", &m.base_tree),
        ("policy_digest", &m.policy_digest),
    ];
    // Present only for a box that lives on another machine, and then it decides
    // which machine every later operation talks to — so it is guarded here
    // rather than left to `sanitize_display` on the way to a terminal.
    if let Some(runner_id) = &m.runner_id {
        ids.push(("runner_id", runner_id));
    }
    // The display name is peer data too. It is resolved against this machine's
    // paired runners — which name-checks it again — but it also reaches a
    // receipt and a terminal, so it is pinned to the same shape here rather
    // than trusted to be pinned somewhere downstream.
    if let Some(runner) = &m.runner {
        let ok = !runner.is_empty()
            && runner.len() <= 64
            && runner
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && runner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err(H5iError::Metadata(format!(
                "manifest runner name is not one this machine could have paired (got '{}')",
                crate::redact::sanitize_display(runner)
            )));
        }
    }
    for (field, got) in ids {
        let ok = (7..=64).contains(&got.len()) && got.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(H5iError::Metadata(format!(
                "manifest {field} is not an object id (got '{}')",
                crate::redact::sanitize_display(got)
            )));
        }
    }
    Ok(())
}

/// The first `n` characters of an identifier, for display.
///
/// `&id[..12]` is what every abbreviating site used, and it panics two ways on a
/// manifest this machine did not write: when the field is shorter than the
/// slice, and when the byte index lands inside a multi-byte character. A
/// manifest arrives from a peer through `refs/h5i/env`, so `h5i box list` —
/// which abbreviates every manifest it can see — aborted on one crafted line.
/// That is the same "one poisoned line suppresses every legitimate env" that
/// [`materialize_from_ref`] skips bad manifests specifically to avoid.
///
/// Counted in characters rather than bytes: for the hex ids this is used on the
/// two agree, and for anything else it is the answer a reader expects.
pub fn short(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((at, _)) => &s[..at],
        None => s,
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

    let mut last_err: Option<git2::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        crate::refstore::cas_backoff(attempt);
        let tip = repo.refname_to_id(ENV_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let mut log = crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), EVENTS_FILE)
            .unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&event_line);
        log.push('\n');

        let mut files: Vec<(&str, String)> = vec![(EVENTS_FILE, log)];
        if let (Some(m), Some(line)) = (manifest, &manifest_line) {
            let existing =
                crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
                    .unwrap_or_default();
            files.push((MANIFESTS_FILE, upsert_jsonl_by_id(&existing, &m.id, line)));
        }
        if let (Some(m), Some(toml)) = (manifest, policy_toml) {
            let existing =
                crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), POLICIES_FILE)
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

        let mut log = crate::refstore::read_blob_from_tree(repo, base_tree.as_ref(), EVENTS_FILE)
            .unwrap_or_default();
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
        let raw = crate::refstore::read_blob_from_tree(repo, tree.as_ref(), EVENTS_FILE)
            .unwrap_or_default();
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
        let mraw = crate::refstore::read_blob_from_tree(repo, tree.as_ref(), MANIFESTS_FILE)
            .unwrap_or_default();
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
        let praw = crate::refstore::read_blob_from_tree(repo, tree.as_ref(), POLICIES_FILE)
            .unwrap_or_default();
        for line in praw.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                && let (Some(id), Some(toml)) = (
                    v.get("id").and_then(|i| i.as_str()),
                    v.get("toml").and_then(|t| t.as_str()),
                )
            {
                policies
                    .entry(id.to_string())
                    .or_insert_with(|| toml.to_string());
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
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
            && let (Some(id), Some(toml)) = (
                v.get("id").and_then(|i| i.as_str()),
                v.get("toml").and_then(|t| t.as_str()),
            )
        {
            if filter.is_some_and(|f| !f.contains(id)) {
                continue;
            }
            policies
                .entry(id.to_string())
                .or_insert_with(|| toml.to_string());
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
            if let Ok(m) = serde_json::from_str::<EnvManifest>(line)
                && m.parent_branch == branch
            {
                ids.insert(m.id);
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
            if let Ok(m) = serde_json::from_str::<EnvManifest>(line)
                && ids.contains(&m.id) && !m.branch.is_empty()
            {
                refs.push(m.branch);
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
    atomic_write(
        &dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(m)?.as_bytes(),
    )?;
    atomic_write(&dir.join(STATUS_FILE), format!("{}\n", m.status).as_bytes())?;
    Ok(())
}

/// Write a state file so a reader sees either the old contents or the new ones,
/// never a truncated middle.
///
/// `fs::write` truncates first, and every reader of these files is
/// unsynchronised: `list`/`find`/`status`/the console all read them, and
/// `materialize_from_ref` runs at the top of every env command. A torn
/// `manifest.json` made `load_manifest_at` fail, which `list` turns into
/// "environment does not exist" for a live box — and worse, in
/// `materialize_from_ref` it reads as "local is not newer", so the on-disk
/// manifest is overwritten from the ref copy and local status/captures are
/// lost. Rename is atomic within a filesystem, and these files never leave the
/// env directory.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), H5iError> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| H5iError::with_path(e, &tmp))?;
    std::fs::rename(&tmp, path).map_err(|e| H5iError::with_path(e, path))
}

fn load_manifest_at(dir: &Path) -> Result<EnvManifest, H5iError> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    let m: EnvManifest = serde_json::from_str(&text)?;
    // The same identity check `materialize_from_ref` runs, at the point every
    // manifest is *read* rather than only where one is imported.
    //
    // Its doc says the fields are validated "BEFORE its `agent`/`slug` are used
    // to compute on-disk paths" — and that held for the import and not for the
    // read. Everything downstream calls `m.dir(h5i_root)`, which is
    // `env_dir(root, m.agent, m.slug)` joined unchecked, and one of the things
    // downstream is `rm`'s `remove_dir_all`. `list` walks directory names; it
    // is the manifest's own fields that become the path.
    validate_imported_manifest(&m)?;
    // And they have to name the directory they were found in. A manifest is
    // identified by where it lives, so one that describes a different env —
    // copied, restored from a backup, or hand-edited — is not this env's
    // manifest whatever it says.
    let here = dir
        .parent()
        .map(|p| (p.file_name(), dir.file_name()))
        .unwrap_or((None, None));
    let want = (
        Some(std::ffi::OsStr::new(m.agent.as_str())),
        Some(std::ffi::OsStr::new(m.slug.as_str())),
    );
    if here != want {
        return Err(H5iError::Metadata(format!(
            "manifest at {} names '{}' — a manifest is identified by where it lives, and this              one describes a different environment (fail-closed)",
            path.display(),
            crate::redact::sanitize_display(&m.id)
        )));
    }
    Ok(m)
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
    let mut policy = policy;
    // Runtime-only, and set here so every caller gets it: a box that declares
    // services must not have its microVM guest stopped for idleness, or the
    // services die with it. Read from the *declared* set rather than from what
    // is running, because the guest's idle bound is fixed when it is created
    // and services start later.
    //
    // From the **pinned** manifest only, never the worktree fallback that
    // `load_service_defs` allows for pre-pinning envs: that file lives inside
    // `$WORK`, which the box can write. This value feeds `--idle-timeout`,
    // which is hashed into the guest's name, so reading it from a box-writable
    // path would let an in-box agent change its own guest's identity — the next
    // command would resolve to a new name and reap the running guest, killing
    // whatever was in it. Nothing the box controls belongs in that hash.
    // Unknown counts as "may host services". Every env created since service
    // pinning has a manifest, so this is the pre-pinning tail — and there the
    // two errors are not symmetric: guessing false costs a killed dev server,
    // guessing true costs a guest that lives until `box rm`.
    policy.hosts_services = pinned_service_defs(h5i_root, m).is_none_or(|d| !d.is_empty());
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
    /// `--engine`: which browser engine a `browser` box runs, overriding the
    /// profile. Same precedence as `--image`: it lands in the profile before
    /// resolve, so it is pinned in the digest. There is no `auto` and no
    /// fallback — an engine that cannot serve a page fails and names the
    /// recreate, because a silent switch would change the box's capability
    /// surface without a decision.
    pub engine: Option<sandbox::BrowserEngine>,
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
    /// Run this box on a paired runner instead of this machine, by display
    /// name. The identity that reaches the manifest comes from the runner
    /// itself (`RemoteCreated::runner_id`), not from this string.
    pub runner: Option<String>,
}

impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            engine: None,
            from: None,
            profile: None,
            isolation: None,
            image: None,
            backend: "auto".into(),
            audit_capture: sandbox::AuditCapture::Signal,
            parent_branch: None,
            runner: None,
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
            let hooks = work.parent().unwrap_or(work).join("clone-hooks-disabled");
            std::fs::create_dir_all(&hooks).map_err(|e| H5iError::with_path(e, &hooks))?;
            let out = std::process::Command::new("git")
                .args(clone_argv(&hooks, url, work))
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

/// The `git` argv for cloning a box's source, built here so it can be read as a
/// whole and tested without a network.
///
/// This clone runs on the **host**, unconfined, on a string somebody typed or
/// pasted. Three things make that safe, and two of them were missing:
///
/// * **`core.hooksPath`** at an empty directory, so a hostile repository cannot
///   ship a hook that runs here. This was already the case.
///
/// * **`protocol.ext.allow=never`.** `ext::` is not a URL, it is a command:
///   `git clone 'ext::sh -c …'` runs it. Git refuses that transport by default,
///   but the default is *config*, and an operator whose `~/.gitconfig` carries
///   `protocol.ext.allow = always` — a setting people do add — turned a pasted
///   "repository URL" into host command execution. Verified both ways against
///   git 2.50: permissive config runs the command, and a later `-c` pinning it
///   to `never` refuses it.
///
/// * **`--end-of-options` before the URL.** Otherwise git reads a leading `-`
///   as an option, so `--upload-pack=<cmd>` is an argument rather than a
///   repository. `source::resolve_pr_base` has carried exactly this guard, with
///   exactly this reasoning, since it was written; the clone path never got it.
///   Today the injected option is defanged by the destination argument that
///   follows it (an empty directory is not a repository, so the clone fails
///   before a transport opens) — which is an accident of argument order, not a
///   property to rest a host boundary on.
fn clone_argv(hooks: &Path, url: &str, work: &Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("-c"),
        OsString::from(format!("core.hooksPath={}", hooks.display())),
        OsString::from("-c"),
        OsString::from("protocol.ext.allow=never"),
        OsString::from("clone"),
        OsString::from("--depth"),
        OsString::from("1"),
        OsString::from("--end-of-options"),
        OsString::from(url),
        work.as_os_str().to_os_string(),
    ]
}

/// The registered worktree name for an env, matching `EnvManifest::worktree_name`
/// before a manifest exists to ask.
fn manifest_worktree_name(agent: &str, slug: &str) -> String {
    format!("h5i-env-{agent}-{slug}")
}

/// The files a directory under `.git/worktrees/` must hold to be a worktree
/// registration rather than a leftover. libgit2 requires all three before it
/// will even list the entry (`is_worktree_dir`); git's own `worktree prune`
/// drops an entry the moment `gitdir` is gone.
const WORKTREE_REG_FILES: [&str; 3] = ["gitdir", "commondir", "HEAD"];

/// Drop directories under `.git/worktrees/` that are not worktree
/// registrations, and report how many went.
///
/// This is not tidiness, it is a prerequisite for creating *any* worktree.
/// libgit2's `git_worktree_lookup` fails on such a directory, and
/// `git_repository_foreach_worktree` propagates that failure as a **truthy**
/// return (`error = lookup(...) < 0` — the comparison lands in `error`, so a
/// failure arrives as `1`, never as `GIT_ENOTFOUND`). Its one caller is
/// `git_branch_is_checked_out`, which reads `== 1` as "yes, checked out". So a
/// single stale directory makes libgit2 answer *checked out* for **every**
/// branch in the repository, and `git_worktree_add` then refuses every
/// worktree with
///
/// ```text
/// reference refs/heads/<branch> is already checked out
/// ```
///
/// — a repo-wide, permanent break of `box create` caused by an empty directory
/// nothing is using, reported against a branch that was created seconds ago
/// and is checked out nowhere (libgit2 1.9.2 / git2 0.20). Sweeping first both
/// heals a repo already in that state and keeps `create` from tripping over a
/// leftover of its own.
///
/// Only invalid entries go. A *valid* registration whose working tree has
/// vanished is left alone: it is still a worktree as far as git is concerned,
/// and whether the env behind it is still wanted is `gc`/`rm`'s question, not
/// this function's.
fn sweep_invalid_worktree_registrations(repo: &Repository) -> usize {
    let Ok(entries) = std::fs::read_dir(repo.commondir().join("worktrees")) else {
        return 0;
    };
    let mut swept = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || WORKTREE_REG_FILES.iter().all(|f| path.join(f).is_file()) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            swept += 1;
        }
    }
    swept
}

/// Undo a half-built env if `create` fails before the manifest exists.
///
/// Only that window needs it: once `manifest.json` is on disk the env is
/// resolvable by name and `h5i box rm` can finish the job.
struct CreateRollback<'a> {
    repo: &'a Repository,
    h5i_root: &'a Path,
    dir: PathBuf,
    work: PathBuf,
    worktree: String,
    /// The env branch, when `create` made one (a detached box's repository
    /// lives inside `work` and goes with the directory).
    branch: Option<String>,
    armed: bool,
}

impl Drop for CreateRollback<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Best effort throughout: this runs while another error is propagating,
        // and a failed cleanup must not mask it.
        if let Ok(wt) = self.repo.find_worktree(&self.worktree) {
            let _ = wt.unlock();
        }
        let _ = std::fs::remove_dir_all(&self.work);
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune", "--expire=now"])
            .current_dir(self.repo.commondir())
            .output();
        // …and again without git: a half-written registration left by a
        // `worktree_add` that failed part-way is the exact thing that poisons
        // every later `create` (see `sweep_invalid_worktree_registrations`), so
        // clearing it must not depend on a `git` binary being on PATH.
        sweep_invalid_worktree_registrations(self.repo);
        if let Some(b) = &self.branch
            && let Ok(mut r) = self.repo.find_branch(b, git2::BranchType::Local)
        {
            let _ = r.delete();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = self.h5i_root;
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
    create_with_remote(repo, h5i_root, workdir, agent, slug, opts, None)
}

/// [`create`], optionally placing the box on another machine.
///
/// The remote path is the same function up to the point where a local box would
/// grow a worktree (ROADMAP.md R7): the base commit is pinned, the branch is
/// created, the profile is resolved and its digest taken, all here — and then,
/// instead of a worktree, the source goes across as a bundle and the runner
/// builds the box. Everything that decides *what* the box is stays on this
/// machine; only the execution moves.
///
/// `remote` is a trait object rather than a runner name because this crate has
/// no transport in it; see [`crate::placement`].
pub fn create_with_remote(
    repo: &Repository,
    h5i_root: &Path,
    workdir: &Path,
    agent: &str,
    slug: &str,
    opts: CreateOpts,
    remote: Option<&dyn crate::placement::RemoteRunner>,
) -> Result<EnvManifest, H5iError> {
    validate_slug(slug)?;
    validate_agent(agent)?;
    // A remote box has no worktree on this machine, so calling its backend
    // `worktree` would be a statement about a directory that does not exist.
    let backend = match (opts.backend.as_str(), remote.is_some()) {
        (_, true) => "runner",
        ("auto" | "worktree", false) => "worktree",
        (other, false) => {
            return Err(H5iError::Metadata(format!(
                "workspace backend '{other}' is not available in this build (worktree only; \
                 branchfs is a later, opt-in phase)"
            )))
        }
    };

    // Refused here rather than part-way through: a detached source has its
    // repository inside the box, and moving that across is the export
    // milestone's problem, not this one's.
    if remote.is_some() && opts.source.is_detached() {
        return Err(H5iError::Metadata(format!(
            "a `{}` source cannot be placed on a runner yet — clone and new boxes build their \
             own repository inside the box, and sending one across is a later milestone. \
             Create it here, or use this repository as the source.",
            opts.source.as_manifest_str()
        )));
    }

    let id = format!("env/{agent}/{slug}");
    let dir = env_dir(h5i_root, agent, slug);
    let branch_short = format!("{BRANCH_PREFIX}{agent}/{slug}");
    let branch_full = format!("refs/heads/{branch_short}");
    if dir.exists() {
        // A directory with no `manifest.json` is not an environment — it is
        // what a `create` that died before the manifest landed leaves behind.
        // Reporting it as "already exists" sends the reader to `box rm`, which
        // resolves envs *through* the manifest and so cannot see this one, and
        // `list` cannot either: the name is simply burnt. An empty leftover has
        // nothing in it to lose, so reclaim it and carry on; one that still
        // holds a workspace is left for a human to look at, with the paths
        // named rather than implied.
        let orphan = !dir.join(MANIFEST_FILE).exists();
        let has_work = dir.join(WORK_DIR).exists();
        if orphan && !has_work {
            std::fs::remove_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
        } else if orphan {
            return Err(H5iError::Metadata(format!(
                "{} holds a workspace but no manifest — a leftover from a `create` that failed \
                 part-way, which `h5i box rm` cannot resolve. Remove it by hand (and its branch, \
                 `git branch -D {branch_short}`), or pick another name",
                dir.display()
            )));
        } else {
            return Err(H5iError::Metadata(format!(
                "environment {id} already exists"
            )));
        }
    }
    if repo.find_reference(&branch_full).is_ok() {
        return Err(H5iError::Metadata(format!(
            "branch {branch_full} already exists with no environment {id} behind it — either \
             `h5i box gc` kept it for provenance after the env was applied/aborted, or a `create` \
             failed and left it. Reuse the name with `git branch -D {branch_short}` first, or \
             pick a new one"
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
                    _ => sandbox::effective_auto(
                        workdir,
                        agent_profile,
                        false,
                        opts.image.as_deref(),
                    )?,
                };
                let mut prof = sandbox::load_profile(workdir, agent_profile, Some(claim))?;
                if let Some(img) = &opts.image {
                    sandbox::validate_image(img)?;
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

    // Policy first (fail closed BEFORE any state is created on disk).
    let mut profile = sandbox::load_profile(workdir, profile_name, Some(claim))?;
    // `--image` has the strongest precedence; it lands in the profile before
    // resolve, so it is pinned in policy.resolved.toml and the digest like any
    // profile-declared image.
    if let Some(img) = &opts.image {
        // `load_profile` validated the profile's own image; `--image` lands
        // after it, so it needs the same gate or the strongest-precedence
        // source is the one nothing checks.
        sandbox::validate_image(img)?;
        profile.image = Some(img.clone());
    }
    // `--engine` has the same precedence as `--image`: it lands in the profile
    // before resolve, so the engine a box runs is pinned in
    // `policy.resolved.toml` and in the digest rather than being whatever
    // happened to be installed on the day it ran.
    if let Some(engine) = opts.engine {
        profile.engine = Some(engine);
    }

    // A browser box with no browser is a box whose first `open` fails with a
    // confusing "not found". Refuse at create, where the message can name what
    // to install — and check the engine the profile actually pinned, because
    // "a browser is present" is not the same question as "the engine this box
    // is pinned to is present".
    //
    // Kernel tiers reach the host filesystem, so the engine has to be there. A
    // container box brings its own in the image, and is checked by the image
    // existing at all.
    if claim < sandbox::IsolationClaim::Container
        && let Some(engine) = profile.engine
    {
        let missing = sandbox::engine_tooling_missing(engine);
        if !missing.is_empty() {
            let (_, install) = engine.required_tooling();
            // "pick another engine" has to name one that is actually *another*
            // engine and actually runnable here — a hint that offers the engine
            // the reader just failed on reads as a bug in the tool, and one that
            // offers an equally-missing engine only costs them a second attempt.
            let fallback = sandbox::BROWSER_ENGINES
                .iter()
                .copied()
                .find(|e| *e != engine && sandbox::engine_tooling_missing(*e).is_empty());
            let alternative = match fallback {
                Some(e) => format!(
                    "Then create the box again, or run it on the engine this host already has: \
                     `--engine {}`.",
                    e.as_str()
                ),
                None => "Then create the box again — no other browser engine is installed here \
                         either, so there is nothing to fall back to (fail-closed)."
                    .to_string(),
            };
            return Err(H5iError::Metadata(format!(
                "the `{}` profile is pinned to the `{}` engine, which needs {} on this host, \
                 and it is not there.\n  Install with:  {}\n  {}",
                profile.name,
                engine.as_str(),
                missing.join(" and "),
                install,
                alternative
            )));
        }
    }

    let caps = sandbox::probe_host_for(claim);
    let mut policy = sandbox::resolve(&profile, &caps)?;
    policy.audit.capture = opts.audit_capture;
    // Functionally verify the confinement can actually run a command — capability
    // bits can be present while a hardened kernel still denies exec under the
    // full stack. Refuse here with a clear message rather than letting every
    // later `env run` fail on EACCES.
    // ROADMAP.md R12's refusal, which was documented and not implemented.
    //
    // Secret *values* never cross — a `SecretGrant` carries a name and a source
    // descriptor, never a value — so nothing of this machine's leaks. What
    // happens instead is worse than a leak in one way: the runner resolves
    // those descriptors against *its own* environment, so a box could be handed
    // the runner's credential in place of the user's, or silently handed none
    // at all while its policy says it has one. Both are the silent weakening
    // R1 says must never happen, so the profile is refused with the milestone
    // named.
    if remote.is_some() {
        let wants_secrets = !policy.profile.secrets.is_empty() || !policy.profile.secret_grants.is_empty();
        let wants_auth = !policy.profile.auth.is_empty();
        if wants_secrets || wants_auth {
            return Err(H5iError::Metadata(format!(
                "profile `{}` needs {} on a runner, and h5i will not send credentials to \
                 another machine. A broker that keeps them here is a later milestone; until \
                 then a runner box runs builds, tests and commands rather than anything that \
                 authenticates. Use a profile without them, or create this box locally.",
                policy.profile.name,
                match (wants_secrets, wants_auth) {
                    (true, true) => "secrets and an authenticated API",
                    (true, false) => "secrets",
                    _ => "an authenticated API",
                }
            )));
        }
    }

    sandbox::verify_exec(&policy)?;
    let policy_digest = policy.digest()?;

    let work_path = dir.join(WORK_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;

    // Armed *before* the branch and the worktree exist, not after they both do.
    //
    // The window it used to open on was the one that actually bites: making the
    // worktree is the step most likely to fail (a poisoned registration
    // directory, a full disk, a name git refuses), and failing there left the
    // branch and `<env>/` behind with no manifest — so `list` showed nothing,
    // `rm` could not resolve the name, and the same `create` retried reported
    // "already exists" for an env that never existed. `branch` is filled in
    // below only once `repo.branch` has actually created one (a detached box's
    // repository lives inside `work` and goes with the directory).
    let mut rollback = CreateRollback {
        repo,
        h5i_root,
        dir: dir.clone(),
        work: work_path.clone(),
        worktree: manifest_worktree_name(agent, slug),
        branch: None,
        armed: true,
    };

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
            .map_err(|e| {
                // A fresh `git init` has a HEAD that names a branch with no
                // commit behind it. "revspec 'HEAD' not found" is technically
                // that, but only to a reader who already knows it — name the
                // real precondition and the command that satisfies it. An
                // explicit `--from` that fails keeps the literal diagnosis:
                // there, "revision not found" is the right one.
                let head_unborn = matches!(
                    repo.head(),
                    Err(ref he) if he.code() == git2::ErrorCode::UnbornBranch
                );
                if opts.from.is_none() && head_unborn {
                    H5iError::Metadata(
                        "this repository has no commits yet — the box branches from HEAD, \
                         so make an initial commit first, e.g. \
                         `git commit --allow-empty -m \"initial commit\"`"
                            .into(),
                    )
                } else {
                    H5iError::Metadata(format!("cannot resolve base revision '{rev}': {e}"))
                }
            })?;
        let base_tree = base_commit.tree()?.id();
        let parent_branch = opts.parent_branch.clone().unwrap_or_else(|| {
            repo.head()
                .ok()
                .and_then(|h| h.shorthand().map(str::to_owned))
                .unwrap_or_else(|| base_commit.id().to_string())
        });

        repo.branch(&branch_short, &base_commit, false)?;
        rollback.branch = Some(branch_short.clone());
        let wt_name = format!("h5i-env-{agent}-{slug}");
        // A remote box has no worktree here: its source goes across as a bundle
        // and is materialised on the runner. Which also dissolves the hardest
        // part of the local path — the identical-path git plumbing binds exist
        // only because a local box shares this repository's inodes, and a
        // remote one shares nothing (ROADMAP.md R7).
        if remote.is_none() {
            // One directory under `.git/worktrees/` that is not a worktree makes
            // libgit2 report *every* branch as already checked out, so the add
            // below would fail for this env and every future one. Clear those
            // before asking (see `sweep_invalid_worktree_registrations`).
            sweep_invalid_worktree_registrations(repo);
            let branch_ref = repo.find_reference(&branch_full)?;
            let mut wt_opts = git2::WorktreeAddOptions::new();
            wt_opts.reference(Some(&branch_ref));
            let wt = repo
                .worktree(&wt_name, &work_path, Some(&wt_opts))
                .map_err(|e| {
                    H5iError::Metadata(format!("worktree creation failed for {id}: {e}"))
                })?;
            // Lock the worktree for the env's whole life so a stray
            // `git worktree prune` can't reclaim a live env out from under it;
            // `h5i box gc` is the only thing that unlocks+prunes it (and only
            // when applied/aborted).
            let _ = wt.lock(Some(&format!("h5i env {id} live")));
        }
        (base_commit.id(), base_tree, parent_branch)
    };

    // The box itself, on the other machine. Everything that decides *what* it
    // is has already happened here; this is where the execution moves.
    let placed = match remote {
        None => None,
        Some(runner) => {
            let repo_path = repo.workdir().ok_or_else(|| {
                H5iError::Metadata(
                    "a runner box needs a repository with a working directory to bundle from"
                        .into(),
                )
            })?;
            let base = base_commit_id.to_string();
            let box_id = crate::placement::remote_box_id(&id);
            let policy_json = serde_json::to_value(&policy).map_err(|e| {
                H5iError::Metadata(format!("could not serialise the resolved policy: {e}"))
            })?;
            let spec = crate::placement::RemoteCreateSpec {
                box_id: &box_id,
                isolation: policy.claim.as_str(),
                image: policy.profile.image.as_deref(),
                policy_json,
                policy_digest: &policy_digest,
                source: Some(crate::placement::RemoteSource {
                    repo: repo_path,
                    base_commit: &base,
                }),
            };
            let created = runner.create(&spec)?;

            // The runner echoes the digest of the policy it actually enforced,
            // and the box is not accepted unless it matches. That turns "the
            // runner silently enforced an older policy" from a possibility into
            // a detected fault (R7). The client checks this too; checking it
            // here as well is what makes the *manifest* honest, since this is
            // the value the manifest is about to pin.
            if created.policy_digest != policy_digest {
                // The box is on the runner and this side is refusing it, so it
                // goes away again. Best effort — the lease would reap it in a
                // couple of hours regardless — but leaving a box holding a copy
                // of somebody's source on a machine with no record of it is not
                // a thing to shrug at when one more RPC closes it.
                let _ = runner.destroy(&box_id);
                return Err(H5iError::Metadata(format!(
                    "runner `{}` built the box under a different policy than the one resolved \
                     here — expected {policy_digest}, it enforced {}. The box was not recorded, \
                     and was removed from the runner.",
                    runner.name(),
                    created.policy_digest
                )));
            }
            Some(created)
        }
    };

    // From here on the worktree, the branch and `<env>/` all exist, but the
    // manifest — the thing `list`/`find`/`rm` resolve an env *through* — does
    // not yet. Several fail-closed steps sit in between (a malformed
    // `[service.*]` table, a persona source missing at the base revision), and
    // without a rollback their failure left a registered+locked worktree and a
    // branch that `create` refuses to reuse and `rm` cannot see, recoverable
    // only by hand with `git worktree prune` and `git branch -D`. `rollback`,
    // armed above, undoes exactly what has been built so far unless it is
    // disarmed once the manifest lands.

    // Pin service declarations from the base worktree into an env-local,
    // box-immutable manifest, recording its digest (review #1). Always Some for
    // new envs (even pinned-empty), so the legacy fallback below never applies.
    // Both of the next two read and write the *worktree*, which a runner box
    // does not have on this machine.
    //
    // `materialize_persona` was not merely wrong here, it was fatal: it writes
    // `PERSONA.md` into the work directory, so any repository whose profile
    // declares a persona failed every `--runner` create — after paying for the
    // whole source transfer, and leaving the box on the runner. Skipping is the
    // honest answer rather than an omission: a persona and a pinned service set
    // are things the box's own tree carries, and carrying them across is the
    // milestone that runs services on a runner.
    // Always `Some`, including for a runner box. `None` is the sentinel that
    // means "an env from before pinning existed", and it re-arms a fallback
    // that reads the repo-root `.h5i/env.toml` — so making it `None` here
    // silently reverted the invariant an earlier fix established, for one class
    // of box, while the comment above still claimed the invariant held.
    //
    // Pinned-empty is also the honest value rather than a convenient one: a
    // service cannot run on a runner (`service start` refuses for lack of a
    // workspace), so "no services will run in this box" is true. `parse_services_file`
    // returns an empty set for a missing file, which is what a remote box's
    // absent work directory produces.
    let service_digest = Some(pin_services_at_create(&work_path, &dir)?);

    // Bake the profile's persona sources into a single PERSONA.md at the
    // worktree root (the agent loads it via `@PERSONA.md`). Git-excluded so it
    // never enters the agent's diff/commit. Fail-closed: a missing source aborts
    // create rather than launching an agent with a silently-empty persona.
    let persona_digest = if remote.is_some() {
        None
    } else {
        materialize_persona(&work_path, &profile.persona)?
    };

    // The viewer token, minted before anything inside the box has run. Minting
    // it lazily on the first `h5i box view` would mean minting it after an agent
    // had already had the run of the box; minting it here means the credential
    // for watching a box predates the box's first instruction. It lives in the
    // env directory, outside every path the box can write or read.
    crate::view::ensure_token(&dir)?;

    // The effective baseline describes what a *local* kernel-tier invocation
    // would apply — Landlock grants and bind mounts against paths on this
    // machine. A box on a runner has none of those here, and its work directory
    // does not exist on this side at all, so computing one would be describing
    // a confinement nobody is going to enforce.
    let baseline = if remote.is_some() {
        None
    } else {
        write_effective_baseline(&policy, &dir, &work_path)?
    };
    let (effective_digest, fs_authority) = match baseline {
        Some((digest, verdict)) => (Some(digest), verdict),
        None => (None, None),
    };

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
        effective_digest,
        fs_authority,
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
        runner_id: placed.as_ref().map(|p| p.runner_id.clone()),
        runner: placed.as_ref().map(|p| p.runner.clone()),
    };

    // Grouped so a runner box's remote half can be cleaned up if this side
    // cannot record it. Locally these are the same `?`s they always were.
    let mut policy_toml = String::new();
    let save_result = (|| -> Result<(), H5iError> {

        policy_toml = policy.to_toml()?;
        let policy_path = dir.join(POLICY_RESOLVED_FILE);
        std::fs::write(&policy_path, &policy_toml)
            .map_err(|e| H5iError::with_path(e, &policy_path))?;
        save_manifest(h5i_root, &manifest)?;
        Ok(())
    })();
    // The env is resolvable now: `rm` and `gc` can clean up anything that fails
    // after this point, so stop unwinding on drop.
    // Everything above this point can still fail, and for a runner box a
    // failure here means the box exists over there with nothing here to
    // remember it. The lease reaps it eventually and a retry is idempotent, so
    // this is tidiness rather than correctness — but an orphan holding a copy
    // of someone's source is worth one more RPC.
    if let (Some(runner), Some(_)) = (remote, placed.as_ref())
        && let Err(e) = save_result.as_ref()
    {
        let _ = runner.destroy(&crate::placement::remote_box_id(&id));
        return Err(H5iError::Metadata(format!(
            "{id} could not be recorded on this machine ({e}), so it was removed from the \
             runner as well"
        )));
    }
    save_result?;
    rollback.armed = false;
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
                short(&manifest.base_commit, 12),
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

/// Write the create-time `policy.effective.json` baseline (ROADMAP.md §P1):
/// the enforced kernel-tier configuration for the canonical captured-run
/// shape, produced by the same `compute_effective` that
/// `build_confined_command` applies at run time. `None` when the tier has no
/// kernel-mechanism dump (workspace, container, microvm) or off Linux — the
/// schema describes Landlock/seccomp/namespaces and nothing else, so those
/// tiers are excluded rather than half-described.
#[cfg(target_os = "linux")]
fn write_effective_baseline(
    policy: &ResolvedPolicy,
    env_dir: &Path,
    work: &Path,
) -> Result<Option<(String, Option<crate::fs_authority::AuthorityVerdict>)>, H5iError> {
    let Some(shape) = crate::effective::captured_run_shape(policy.claim, &policy.profile) else {
        return Ok(None);
    };
    let caps = sandbox::probe_host_for(policy.claim);
    let work = work.canonicalize().map_err(|e| H5iError::with_path(e, work))?;
    let cfg = crate::effective::compute_effective(
        policy,
        &work,
        caps.landlock_abi.unwrap_or(1),
        &shape,
    );
    let digest = cfg.write_to(&env_dir.join(crate::effective::EFFECTIVE_CONFIG_FILE))?;
    // Opt-in only (§P2): with `H5I_FS_AUTHORITY_ENFORCE` unset, no verdict is
    // computed or recorded, so the manifest is byte-for-byte as before.
    let verdict = crate::fs_authority::enforce_enabled()
        .then(|| crate::effective::validate_effective(policy, &work, &cfg));
    Ok(Some((digest, verdict)))
}

#[cfg(not(target_os = "linux"))]
fn write_effective_baseline(
    _policy: &ResolvedPolicy,
    _env_dir: &Path,
    _work: &Path,
) -> Result<Option<(String, Option<crate::fs_authority::AuthorityVerdict>)>, H5iError> {
    Ok(None)
}

/// sha256 of the env's `policy.effective.json` as the just-finished invocation
/// left it — the digest a capture record pins (§P1). `None` when no kernel
/// tier wrote one. Hashed from the file bytes, not recomputed: the record
/// attests to what is on disk.
#[cfg(target_os = "linux")]
fn effective_digest_of(env_dir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(env_dir.join(crate::effective::EFFECTIVE_CONFIG_FILE)).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(format!("{:x}", h.finalize()))
}

#[cfg(not(target_os = "linux"))]
fn effective_digest_of(_env_dir: &Path) -> Option<String> {
    None
}

/// Other boxes **of this repository** whose effective Landlock grants
/// overlap this env's, as `env/<id> via <path>` strings for the capture
/// record. Per-repo on purpose and by construction: [`list`] walks this
/// repo's `.h5i/env`, so a box of a *different* repo on the same host is
/// outside the scan — the receipt must not read as a host-wide claim.
/// "Materialized" means their `policy.effective.json` exists — a pulled or
/// gc'd box has none and cannot run here. One more honesty bound: each
/// neighbor's dump reflects its *latest invocation's shape*, so a box whose
/// last session was a readonly shell shows a narrower rw set until its next
/// ordinary run rewrites the dump. Both directions are
/// checked (influence has no preferred direction on a console), and an empty
/// answer cites the machine-checked noninterference theorem; see the field
/// docs on [`crate::receipt::ExecRecord::fs_overlap`] for the claim's exact
/// scope. Best-effort on read errors: a corrupt neighbor dump is skipped,
/// never a reason to fail this box's run.
#[cfg(target_os = "linux")]
fn fs_overlap_with_boxes(h5i_root: &Path, m: &EnvManifest) -> Vec<String> {
    let read = |dir: &Path| -> Option<crate::effective::EffectiveConfig> {
        let text =
            std::fs::read_to_string(dir.join(crate::effective::EFFECTIVE_CONFIG_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    };
    let Some(mine) = read(&m.dir(h5i_root)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for other in list(h5i_root) {
        if other.id == m.id {
            continue;
        }
        let Some(theirs) = read(&other.dir(h5i_root)) else {
            continue;
        };
        let hit = crate::effective::interferes(&mine, &theirs)
            .or_else(|| crate::effective::interferes(&theirs, &mine));
        if let Some((path, _)) = hit {
            out.push(format!("{} via {}", other.id, path));
        }
    }
    out.sort();
    out
}

#[cfg(not(target_os = "linux"))]
fn fs_overlap_with_boxes(_h5i_root: &Path, _m: &EnvManifest) -> Vec<String> {
    Vec::new()
}

/// Bake the profile's `persona = [...]` sources into a single `PERSONA.md` at
/// the worktree root. Sources ride in the repo at the pinned base, so they are
/// present in the freshly checked-out worktree; their contents are concatenated
/// in declared order, each under an HTML-comment header naming the source. The
/// file is then git-excluded (so it never appears in `env diff`/propose/commit,
/// even when `h5i init` did not add it to a tracked `.gitignore`). Returns the
/// sha256 of the written `PERSONA.md` for provenance, or `None` when the profile
/// declares no persona. Paths are validated (relative, no `..`) at policy load.
/// Largest persona source that will be baked in. A standing instruction is
/// prose; anything past this is not one, and the file is repo-supplied.
const MAX_PERSONA_BYTES: u64 = 1024 * 1024;

/// Read `rel` under `work` **without following a symlink at any component**.
///
/// `validate_profile` pins a persona source inside `$WORK`: relative, no `..`,
/// no absolute path. What it cannot do is *resolve* it — and both the entry and
/// the worktree contents are repo-supplied, so a branch that ships `notes.md` as
/// a symlink to `~/.ssh/id_rsa` turns a valid-looking entry into a read of the
/// operator's key. Git checks symlinks out faithfully, and `read_to_string`
/// follows them.
///
/// The consequence is worse here than for most reads: what is read is
/// concatenated into `PERSONA.md` *inside the box*, which the agent is told to
/// open. A host file would be handed to the agent by the mechanism whose whole
/// purpose is to tell it how to behave.
///
/// `private_paths` has the same shape and got `create_dirs_within` for exactly
/// this reason — "it never *resolves* the path". This is the read side of that
/// argument, and it is bounded as well, because the source is repo-supplied.
/// Resolve `rel` under `work`, refusing a symlink at any component.
///
/// The check both [`read_within_work`] and [`resolve_work_rcfile`] need: each
/// takes a repo-declared, `validate`-approved relative path and then does
/// something with the file it names, and neither validator resolves anything.
fn resolve_within_work(work: &Path, rel: &str) -> std::io::Result<PathBuf> {
    let mut cur = work.to_path_buf();
    let comps: Vec<&str> = rel
        .trim_matches('/')
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if comps.is_empty() {
        return Err(std::io::Error::other("empty path"));
    }
    for c in &comps {
        cur.push(c);
        if std::fs::symlink_metadata(&cur)?.file_type().is_symlink() {
            return Err(std::io::Error::other(format!(
                "'{}' is a symlink, and h5i will not follow one out of the workspace \
                 (fail-closed)",
                cur.display()
            )));
        }
    }
    Ok(cur)
}

fn read_within_work(work: &Path, rel: &str) -> std::io::Result<String> {
    use std::io::Read;
    let cur = resolve_within_work(work, rel)?;
    // `O_NOFOLLOW` as well as the walk: the walk is a check and this is the
    // open, and between them is a window a repo's own build step could use.
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let f = opts.open(&cur)?;
    if !f.metadata()?.file_type().is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut buf = String::new();
    f.take(MAX_PERSONA_BYTES).read_to_string(&mut buf)?;
    Ok(buf)
}

fn materialize_persona(work: &Path, persona: &[String]) -> Result<Option<String>, H5iError> {
    if persona.is_empty() {
        return Ok(None);
    }
    let mut body = String::new();
    for src in persona {
        let path = work.join(src);
        let text = read_within_work(work, src).map_err(|e| {
            H5iError::Metadata(format!(
                "persona source '{src}' is not readable in the worktree ({}): {e} — commit it \
                 at the base revision or fix `persona` in .h5i/env.toml (fail-closed)",
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

/// Does this box live on another machine?
///
/// Keyed on `runner_id` rather than on `backend`, because the identity is the
/// thing every later operation actually needs: `backend` says what kind of
/// workspace it has, and this says which machine to ask.
pub fn is_remote(m: &EnvManifest) -> bool {
    m.runner_id.is_some()
}

/// A uniform refusal for operations that would need a local workspace a runner
/// box does not have here.
///
/// Distinct from [`no_workspace_err`], which is about a box whose clone is
/// elsewhere: this box is fine, it is simply on a machine this milestone cannot
/// yet run commands on, and saying so beats a message about a missing
/// directory.
pub fn remote_unsupported_err(m: &EnvManifest, op: &str) -> H5iError {
    H5iError::Metadata(format!(
        "{}: `{op}` does not work on a runner box yet — this box runs on `{}`, and running \
         commands there is the next milestone. `h5i box status {}` and `h5i box ls` work now, \
         and `h5i box rm {}` removes it from both sides.",
        m.id,
        m.runner.as_deref().unwrap_or("another machine"),
        m.slug,
        m.slug
    ))
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

// ─── opening the env worktree (host side) ────────────────────────────────────

/// Open the env's worktree repository, refusing any handle that is not this
/// box's own.
///
/// Plain `Repository::open(work)` trusts two things the box can write: the
/// `$WORK/.git` pointer file, which lives in the box's rw workspace, and the
/// worktree admin dir (`HEAD`, `commondir`, `gitdir`), which [`box_git_plumbing`]
/// grants rw so in-box git keeps working. A box that rewrites either one
/// redirects every host-side git operation that follows. The consequence is
/// worst in [`mediated_commit`], which would stage the box's tree into whatever
/// repository the pointer names and commit it onto whatever ref its HEAD names
/// — landing unreviewed work on the parent branch without `apply` ever running.
///
/// So the invariant [`box_git_plumbing`] states for grant computation — never
/// derive host behaviour from box-writable state — is enforced here for every
/// host-side open: the handle must sit on the manifest's branch, and its object
/// store must be the one this box was created against.
fn open_env_worktree(h5i_root: &Path, m: &EnvManifest) -> Result<Repository, H5iError> {
    let work = m.work_dir(h5i_root);
    let wt_repo = Repository::open(&work)?;
    verify_env_worktree(h5i_root, &wt_repo, m)?;
    Ok(wt_repo)
}

/// The two checks behind [`open_env_worktree`], split out so the refusal can be
/// unit-tested against a deliberately redirected worktree.
fn verify_env_worktree(
    h5i_root: &Path,
    wt_repo: &Repository,
    m: &EnvManifest,
) -> Result<(), H5iError> {
    // Canonicalize both sides: the comparison has to survive symlinked repo
    // paths (`/tmp` on macOS is the standing example) without being loosened
    // into a prefix match.
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let got = canon(wt_repo.commondir());

    // 1. Same object store. An attached box is a worktree of this repository,
    //    so it shares the common dir; a detached box carries its own repository
    //    inside the env directory and must stay inside it.
    let ok = if is_detached(m) {
        got.starts_with(canon(&m.dir(h5i_root)))
    } else {
        // `h5i_root` is `<repo>/.git/.h5i` (see `storage::h5i_root_for_repo`),
        // so its parent is the common dir this box belongs to.
        h5i_root.parent().is_some_and(|d| got == canon(d))
    };
    if !ok {
        return Err(H5iError::Metadata(format!(
            "{}: the box's worktree points at a git directory that is not this box's \
             ({}). `$WORK/.git` and the worktree admin dir are writable inside the box, so \
             h5i refuses to run a host-side git operation through a redirected pointer \
             (fail-closed). Recreate the box to continue.",
            m.id,
            got.display()
        )));
    }

    // 2. On its own branch. Without this a rewritten worktree HEAD would let a
    //    mediated commit land on the parent branch.
    let head_ref = wt_repo
        .head()
        .ok()
        .and_then(|h| h.name().map(str::to_string));
    if head_ref.as_deref() != Some(m.branch.as_str()) {
        return Err(H5iError::Metadata(format!(
            "{}: the box's worktree is on {} but this box owns {}. The worktree HEAD is \
             writable inside the box, so h5i refuses to commit through it (fail-closed). \
             Recreate the box to continue.",
            m.id,
            head_ref.as_deref().unwrap_or("a detached HEAD"),
            m.branch
        )));
    }
    Ok(())
}

// ─── in-box git plumbing grants ──────────────────────────────────────────────

/// The repo-`.git` plumbing that makes the env's worktree a *functional* git
/// checkout from inside the box. Consumed per backend by [`grant_box_git`]:
/// Landlock grants at process/supervised, identical-path bind mounts at
/// container.
///
/// `$WORK` alone is not enough. `$WORK/.git` is a pointer file into
/// `<repo>/.git/worktrees/<wt>`, which points at the shared `<repo>/.git` for
/// objects, refs and config. With no grant every `git`/`h5i` call in the box
/// dies on EACCES, which libgit2 renders as a misleading `GIT_ELOCKED`.
///
/// Granted, and nothing more:
///
/// - **rw** `worktrees/<wt>`, this env's own admin dir.
/// - **rw** `objects`. Shared, so a hostile box can add garbage or delete loose
///   objects, an availability risk recoverable from any clone. It cannot move a
///   ref it is not granted, so history integrity holds.
/// - **rw** the parent dir of the env's own branch ref plus its reflog dir.
///   Loose-ref updates create `<slug>.lock` siblings, so the grant has to be the
///   directory. The box moves its own agent's branches under
///   `refs/heads/h5i/env/<agent>/` and nothing else in `refs/heads`.
/// - **rw** `refs/h5i/context`, so in-box `h5i context init/trace/commit` works.
///   Context is a shared advisory record, union-merged across clones, not a
///   protected code ref.
/// - **ro** `HEAD`, `config`, `packed-refs`, `refs`, `info`, the minimum
///   `git status`/`commit` read. A repo-local `config` carrying credentials in
///   remote URLs becomes readable in-box, so it stays strictly read-only: a
///   writable `core.fsmonitor`/`hooksPath` would execute code on the host the
///   next time *anyone* ran git there.
/// - **ro** `~/.gitconfig` and `~/.config/git`. Git *dies* rather than skips
///   when an existing global config cannot be opened: Landlock lets the
///   `access()` probe pass on DAC bits, then the open fails and git reports
///   "unknown error occurred while reading the configuration files".
///   Deny-home profiles get these two paths and nothing else under `$HOME`;
///   `~/.git-credentials` stays out, being consulted only by credential helpers
///   on network operations.
/// - **ro** the main repo's `Cargo.toml` where it exists, since cargo walks
///   upward from nested env worktrees looking for a workspace root.
///
/// Deliberately **not** granted: `.git` itself, `hooks`, `refs/h5i/env` (a box
/// that could rewrite manifests or policies could widen its own sandbox on the
/// next run), the env's manifest/policy dir beside `$WORK`, and the on-disk h5i
/// stores (`.h5i/claims`, notes, msg), which stay host-mediated evidence
/// channels by design.
///
/// Two invariants:
/// - Paths derive only from the identity-validated manifest and the host repo
///   handle, never from box-writable state. The `$WORK/.git` pointer file is
///   exactly the kind of thing a previous run could have rewritten.
/// - Missing rw dirs are recreated here. The Landlock builder skips
///   non-existent grant paths, which is the right fail-closed default for
///   *policy* paths, but for these structural grants a silent skip would brick
///   in-box git again, for instance after a host-side `git pack-refs` pruned
///   the loose-ref directory.
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
        // The mountpoint must exist inside the worktree — and *stay* inside it.
        // `rel` and the worktree are both repo-supplied, so a symlinked
        // ancestor would otherwise put the mountpoint (and the bind's rw grant)
        // on an arbitrary host path.
        sandbox::create_private_mountpoint(work, &rel)?;
        // The image-backed tiers carry the backing dir as a mount *spec string*,
        // and each runtime's syntax reserves a separator its paths cannot
        // contain: Podman's `--mount` splits on ',', microsandbox's
        // `SOURCE:DEST` on ':'. Fail closed if the env's host path holds one,
        // rather than silently dropping the (policy-required) isolation.
        if let Some(sep) = mount_spec_separator(policy.claim)
            && backing.display().to_string().contains(sep)
        {
            return Err(H5iError::Metadata(format!(
                "private_paths '{rel}': the env's backing path '{}' contains a '{sep}' which \
                 the {} tier's mount syntax cannot carry — move the repo out of that path \
                 (fail-closed)",
                backing.display(),
                policy.claim.as_str()
            )));
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
            let ours = md.is_dir()
                && !md.file_type().is_symlink()
                && md.uid() == unsafe { libc::getuid() };
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
    // Wiping the shared per-env scratch out from under a running service would
    // delete a live dev server's `/tmp` mid-flight: services outlive the
    // session that started them, and nothing else coordinates the two. Reuse
    // the directory instead — the point of the reset is a clean slate per run,
    // and a box with a service running is by definition not starting clean.
    //
    // A per-session observer backing (`backing_override`) is nobody else's, so
    // it is always reset.
    let shared = backing_override.is_none();
    if shared && service_status(h5i_root, m).iter().any(|s| s.alive) {
        std::fs::create_dir_all(&backing).map_err(|e| H5iError::with_path(e, &backing))?;
    } else {
        reset_private_tmp(&backing)?;
    }
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
        // Carry the source directory's mode across. `create_dir_all` uses
        // 0777 & ~umask (typically 0755), so a 0700 `~/.codex` became a 0755
        // copy: `std::fs::copy` preserves the *file's* mode, but a config file
        // at 0644 was relying on its parent directory for protection, which is
        // the common case for an agent credential.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o7777;
            std::fs::set_permissions(dst, std::fs::Permissions::from_mode(mode))
                .map_err(|e| H5iError::with_path(e, dst))?;
        }
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

/// Host path of an env's capture spool (`<env>/spool/`) — the box's one
/// writable window. Its counterpart to [`env_inbox_dir`]: the inbox is what the
/// box may read, this is what it may write, and everything else under the env
/// directory is outside both grants.
pub fn env_capture_spool_dir(h5i_root: &Path, m: &EnvManifest) -> PathBuf {
    m.dir(h5i_root).join(ENV_SPOOL_DIR)
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
    let spool = env_capture_spool_dir(h5i_root, m);
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
        // Deliberately `dynamic_port` and **not** [`service_port`], which falls
        // back to the declared port: these become *host-side* loopback grants,
        // and a guest service's port is bound inside the box's own network
        // stack, so granting it on the host would open a port belonging to
        // nothing. The asymmetry is the correctness, not an oversight to tidy.
        //
        // Liveness goes through [`service_alive`] all the same. It used to call
        // `pid_alive` directly, which was safe only because `dynamic_port` is
        // `None` for guest records today — an accident of ordering, not an
        // invariant, and one that port forwarding would quietly break by
        // testing a guest pid against the host's pid table.
        if let Some(port) = rec.dynamic_port
            && service_alive(&rec)
        {
            out.push(port);
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

# The daemon goes where h5i is mediating, not where this CLI would put it.
#
# h5i owns the socket named by AGENT_BROWSER_SOCKET_DIR and forwards to the one
# below, so every verb passes a policy check on the way through. Starting the
# daemon here rather than letting the CLI start it is the whole trick: a daemon
# the CLI starts binds the mediated path itself, and then there is nothing in
# front of it.
if [ -n "$H5I_BROWSER_DAEMON_DIR" ]; then
  if [ ! -S "$H5I_BROWSER_DAEMON_DIR/default.sock" ]; then
    mkdir -p "$H5I_BROWSER_DAEMON_DIR"
    # `open about:blank` rather than a read verb: every agent-browser command
    # starts the daemon, but only some of them are commands — `url` and
    # `status` are not, and a failed start leaves no daemon and no clue.
    #
    # Output is kept, not discarded: if this fails the mirroring below is
    # skipped, h5i's socket is left with no .version/.config beside it, and the
    # CLI concludes the mediator is a stale daemon and replaces it — so the run
    # continues completely unmediated. That has to be loud.
    AGENT_BROWSER_SOCKET_DIR="$H5I_BROWSER_DAEMON_DIR" \
      "$REAL" --cdp "$PORT" open about:blank >"$STATE/daemon-start.log" 2>&1 || true
    i=0
    while [ $i -lt 100 ] && [ ! -S "$H5I_BROWSER_DAEMON_DIR/default.sock" ]; do
      i=$((i+1)); sleep 0.1
    done
    if [ ! -S "$H5I_BROWSER_DAEMON_DIR/default.sock" ]; then
      echo "h5i: the browser daemon did not start, so this session is NOT mediated:" >&2
      echo "     the control lock and the profile's browser deny list are not enforced." >&2
      tail -5 "$STATE/daemon-start.log" >&2 2>/dev/null || true
      exit 1
    fi
  fi
  # The CLI decides whether a daemon is already up by reading these beside the
  # socket. Without them it concludes the mediator is a stale daemon, asks it
  # to shut down, and starts its own — unmediated.
  if [ -S "$H5I_BROWSER_DAEMON_DIR/default.sock" ]; then
    mkdir -p "$AGENT_BROWSER_SOCKET_DIR"
    for f in version config stream; do
      if [ -f "$H5I_BROWSER_DAEMON_DIR/default.$f" ]; then
        cp "$H5I_BROWSER_DAEMON_DIR/default.$f" \
           "$AGENT_BROWSER_SOCKET_DIR/default.$f" 2>/dev/null || true
      fi
    done
  fi
fi

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

/// The two directories a `browser` box uses, and the trust line between them.
///
/// `state` is granted **write**: it is where the box's own Chrome records its
/// pid and port. `dir` is granted **read** only, and is where the host keeps
/// the loopback ports it reserved — those decide what
/// `policy.loopback_ports` will grant, so they must not be box-writable.
fn browser_dirs(h5i_root: &Path, m: &EnvManifest) -> (PathBuf, PathBuf) {
    let dir = m.dir(h5i_root).join("browser");
    let state = dir.join("state");
    (dir, state)
}

/// A loopback port held for the life of the env: read back from `file` when it
/// is already there, otherwise reserved once and written down. Both ports a
/// browser box depends on are memorised by something that outlives a single run
/// (Chrome's CDP endpoint, and the proxy address Chrome was launched with), so
/// neither can be re-drawn per run.
fn remembered_port(file: &Path, what: &str, avoid: &[u16]) -> Result<u16, H5iError> {
    // `file` MUST live outside every write grant the box holds. The value read
    // back here is pushed into `policy.loopback_ports`, which Seatbelt renders
    // as `(allow network-outbound (remote ip "localhost:<port>"))` — so a box
    // that could write it would be choosing which host loopback service its own
    // next session may reach (the operator's Postgres, another box's dev
    // server). That is why these files sit in `<env>/browser`, which the box is
    // granted read on, and not in `<env>/browser/state`, which it can write.
    //
    // A `0` would still ask for an ephemeral port under the name of a pinned
    // one and a privileged port would fail to bind on every run, so both are
    // rejected in favour of drawing a fresh port — as is a corrupt file.
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
    // The shim launches Chrome and attaches agent-browser to it. An engine h5i
    // runs itself has no Chrome to launch and no agent-browser to attach, so a
    // shim here would put a launcher for a browser this box does not use at
    // the front of its PATH.
    if !policy
        .profile
        .engine
        .map(|e| e.driven_by_agent_browser())
        .unwrap_or(true)
    {
        return Ok(None);
    }
    // Not on an image-backed tier. The shim lives host-side and is not mounted
    // into the image, and `browser_env` prepends the *host* PATH, whose entries
    // do not exist in the guest — so a container browser box started with dead
    // leading PATH entries and a shim it could not execute. `create` allows
    // this configuration, so it has to be handled rather than assumed away.
    if policy.claim.image_backed() {
        return Ok(None);
    }
    let Some(real) = sandbox::agent_browser_binary() else {
        return Ok(None);
    };
    let (dir, state) = browser_dirs(h5i_root, m);
    std::fs::create_dir_all(&state).map_err(|e| H5iError::with_path(e, &state))?;
    // Before anything else: a browser the last run found stranded on an old
    // route is stopped here, so the shim launches a fresh one below.
    stop_stale_browser(&state);

    // The CDP port is picked host-side, remembered, and reused. It cannot be
    // per-run: Chrome outlives the `box run` that started it, so a fresh port on
    // the next run would be a port the still-running Chrome is not listening on
    // — and, worse, the only port the policy grants. Allocated once, then read
    // back for the life of the env.
    // Reserved in `dir`, not `state`: the box has write on `state` and only read
    // on `dir`, and these two numbers decide which host loopback ports the
    // policy will grant.
    let port = remembered_port(&dir.join("cdp-port"), "the box's browser", &[])?;
    policy.loopback_ports.push(port);
    // The egress allowlist proxy is remembered for the same reason, in the
    // other direction: that surviving Chrome memorises the proxy address it was
    // launched with, and on the macOS supervised tier that proxy is the box's
    // only route out. See [`sandbox::ResolvedPolicy::egress_proxy_port`].
    policy.egress_proxy_port = Some(remembered_port(
        &dir.join("egress-port"),
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
    let engine = policy.profile.engine;
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
    // An engine agent-browser cannot drive gets none of its variables: every
    // one of them would be a policy line that reviews as enforcement while
    // enforcing nothing.
    if !engine.map(|e| e.driven_by_agent_browser()).unwrap_or(true) {
        return browser_light_env(policy, &allowed);
    }

    let mut v: Vec<(String, String)> = Vec::new();
    // Only when agent-browser is launching Chrome itself. Under the shim it
    // attaches with `--cdp`, which upstream refuses to combine with
    // `--allowed-domains`, so setting it would break every command rather than
    // add a layer. The tier's egress enforcement is unchanged either way; what
    // is lost is agent-browser's own in-process check. See `browser_shim_source`.
    if !shimmed {
        v.push((
            "AGENT_BROWSER_ALLOWED_DOMAINS".to_string(),
            allowed.join(","),
        ));
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
        // Where the shim puts the real daemon, so h5i's listener above it has
        // something to forward to (M8). Box-visible because the daemon runs in
        // the box; see `browser_proxy` on why that is enforcement rather than
        // containment.
        (
            "H5I_BROWSER_DAEMON_DIR".to_string(),
            format!("{}/{}", box_tmp_root(policy), DAEMON_DIR_NAME),
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
    ]);

    // Chrome's own sandbox needs the namespace syscalls our seccomp policy
    // denies, at every tier. h5i's box is the boundary; Chrome's is not
    // available inside it, and without this the renderer dies at startup.
    //
    // Lightpanda is not Chrome and upstream *refuses* the combination — "Custom
    // Chrome arguments (--args) are not supported with Lightpanda" — so setting
    // it there would break every command rather than harden anything.
    if !matches!(engine, Some(sandbox::BrowserEngine::Lightpanda)) {
        v.push((
            "AGENT_BROWSER_ARGS".to_string(),
            "--no-sandbox --disable-dev-shm-usage".to_string(),
        ));
    }

    // Name the engine only when it is not the default. agent-browser does read
    // this one (verified: it selects lightpanda and writes it to `.engine`),
    // which is the bar every variable here has to clear.
    if let Some(sandbox::BrowserEngine::Lightpanda) = engine {
        v.push(("AGENT_BROWSER_ENGINE".to_string(), "lightpanda".to_string()));
    }

    v
}

/// Environment for an engine h5i runs itself, rather than through
/// agent-browser.
///
/// The two variables here are the same two policy decisions the agent-browser
/// path makes, expressed to a tool that reads them: what the page may reach,
/// and where the request log goes. The receipts path is the interesting one —
/// `h5i-browser-light` refuses to fetch when it cannot write its log, so
/// pointing it at the box's own spool is what makes that guarantee h5i's
/// rather than the engine's alone.
fn browser_light_env(policy: &ResolvedPolicy, allowed: &[String]) -> Vec<(String, String)> {
    vec![
        // agent-browser's own in-process allowlist, kept even though this
        // engine does not read it. Its binaries stay granted (the grant list
        // is host discovery, not per-engine), so an agent in an h5i-light box
        // can still invoke agent-browser directly — and if it does, this is
        // the only thing standing between it and any host on the internet.
        // Dropping it because "our engine ignores it" would have removed a
        // control from the box that was pinned to the *safer* engine.
        (
            "AGENT_BROWSER_ALLOWED_DOMAINS".to_string(),
            allowed.join(","),
        ),
        // Headless for the same reason: a headed launch on a displayless box
        // starts an Xvfb the profile never granted.
        ("AGENT_BROWSER_HEADED".to_string(), "0".to_string()),
        ("H5I_BROWSER_ALLOW".to_string(), allowed.join(",")),
        (
            "H5I_BROWSER_RECEIPTS".to_string(),
            format!("{}/browser-requests.jsonl", box_tmp_root(policy)),
        ),
        // Where `serve` should advertise its port. The viewers find a stream
        // by scanning for `*.stream` under the socket directory, so writing it
        // there is what lets `h5i box view` attach to this engine without
        // knowing anything about it.
        (
            "H5I_BROWSER_STREAM_FILE".to_string(),
            format!("{}/agent-browser/h5i-light.stream", box_tmp_root(policy)),
        ),
        // Where the agent's own verbs go. The console's agent-actions pane is
        // fed by the mediator, and this engine has no mediator to feed it —
        // `engage_browser_mediation` returns `None` for any engine agent-browser
        // cannot drive. Without this the pane renders empty for a session an
        // agent is actively driving, which reads as "the agent did nothing".
        // The rows it produces are box-claimed, not host-observed, because the
        // engine is the browser and there is no socket between them to watch.
        (
            "H5I_BROWSER_ACTIONS".to_string(),
            format!("{}/browser-actions.jsonl", box_tmp_root(policy)),
        ),
    ]
}

#[cfg(test)]
mod browser_engine_env_tests {
    use super::*;
    use crate::sandbox::{AgentRuntime, BrowserEngine, IsolationClaim, Profile};

    // Built directly rather than through `sandbox::resolve`, which for the
    // supervised claim probes the *real* mediation stack and refuses where it
    // is absent — a CI runner cannot unshare NEWNET, so resolving there
    // panicked every test below on a host property none of them are about.
    // What they are about (which variables a browser box is handed) is a pure
    // function of the profile, and `resolve` returns this same value.
    fn policy_for(engine: BrowserEngine) -> ResolvedPolicy {
        let mut profile =
            Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);
        profile.engine = Some(engine);
        ResolvedPolicy::new(IsolationClaim::Supervised, profile)
    }

    fn names(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn the_mediator_binds_where_the_box_is_told_to_look() {
        // The bug this pins: `engage_browser_mediation` derived its path from
        // `<env>/tmp` while the box was told `box_tmp_root`. They coincide on
        // Linux kernel tiers — which is all the manual verification covered —
        // and diverge on macOS, so mediation silently did not happen there.
        let policy = policy_for(BrowserEngine::Chromium);
        let env_dir = std::path::Path::new("/some/env/dir");

        let told = browser_env_inner(&policy, false)
            .into_iter()
            .find(|(k, _)| k == "AGENT_BROWSER_SOCKET_DIR")
            .map(|(_, v)| v)
            .expect("the box is told a socket dir");
        assert_eq!(
            told,
            format!("{}/agent-browser", box_tmp_root(&policy)),
            "the box side must come from box_tmp_root"
        );

        // With a `/tmp` redirect recorded, the host side must be its backing —
        // not the host's own /tmp, and not a path reconstructed from the
        // profile's grants (which have been rewritten by then).
        let mut redirected = policy.clone();
        redirected.home_binds.push(crate::sandbox::HomeBind {
            backing: std::path::PathBuf::from("/some/env/dir/tmp"),
            target: std::path::PathBuf::from("/tmp"),
        });
        assert_eq!(
            host_tmp_root(&redirected, env_dir),
            Some(std::path::PathBuf::from("/some/env/dir/tmp")),
            "the mediator must follow the recorded /tmp redirect"
        );

        // With no redirect there is no *private* /tmp, and mediating on the
        // host's shared one would let two boxes steal each other's socket.
        let mut plain = policy.clone();
        plain
            .home_binds
            .retain(|b| b.target != std::path::Path::new("/tmp"));
        assert_eq!(
            host_tmp_root(&plain, env_dir),
            None,
            "a shared host /tmp must disable mediation, not host a global socket"
        );
    }

    #[test]
    fn an_image_backed_tier_has_no_host_side_tmp_to_mediate() {
        // A container's /tmp is in the image, so there is no host path to bind.
        // Returning a path anyway is what produces a mediator nobody connects
        // to, which reads as enforcement and is not.
        let mut profile = Profile::builtin_browser(IsolationClaim::Container, AgentRuntime::Claude);
        profile.engine = Some(BrowserEngine::Chromium);
        profile.image = Some("example:latest".to_string());
        // Same reason as `policy_for`: resolving the container claim needs
        // rootless Podman on the host, which a runner does not have — and a
        // test that quietly asserts nothing where the runtime is missing is
        // exactly the coverage this file lost.
        let policy = ResolvedPolicy::new(IsolationClaim::Container, profile);
        assert!(
            host_tmp_root(&policy, std::path::Path::new("/e")).is_none(),
            "image-backed tiers must report no host-side /tmp"
        );
    }

    #[test]
    fn our_own_engine_gets_its_own_variables() {
        let env = browser_env_inner(&policy_for(BrowserEngine::H5iLight), false);
        assert!(names(&env).contains(&"H5I_BROWSER_ALLOW"));
        assert!(names(&env).contains(&"H5I_BROWSER_RECEIPTS"));
        // The daemon dir is agent-browser's and means nothing here.
        assert!(
            !names(&env).contains(&"H5I_BROWSER_DAEMON_DIR"),
            "{:?}",
            names(&env)
        );
    }

    #[test]
    fn an_h5i_light_box_keeps_agent_browsers_allowlist_because_chrome_stays_reachable() {
        // The trap: `browser_read_grants` is host discovery, so Chrome and
        // agent-browser stay granted in *every* browser box regardless of the
        // pinned engine. Emitting no AGENT_BROWSER_* at all therefore did not
        // mean "agent-browser cannot run here" — it meant "if it runs, it runs
        // with no domain allowlist", in the box chosen for being safer.
        let env = browser_env_inner(&policy_for(BrowserEngine::H5iLight), false);
        let allowed = env
            .iter()
            .find(|(k, _)| k == "AGENT_BROWSER_ALLOWED_DOMAINS")
            .map(|(_, v)| v.clone())
            .expect("the in-process allowlist must survive the engine switch");
        assert!(allowed.contains("localhost"), "{allowed}");
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "AGENT_BROWSER_HEADED")
                .map(|(_, v)| v.as_str()),
            Some("0"),
            "headless must stay pinned too"
        );
    }

    #[test]
    fn our_engine_inherits_the_boxs_egress_as_its_allowlist() {
        let env = browser_env_inner(&policy_for(BrowserEngine::H5iLight), false);
        let allow = env
            .iter()
            .find(|(k, _)| k == "H5I_BROWSER_ALLOW")
            .map(|(_, v)| v.clone())
            .expect("allow list");
        // Loopback is the dev server and never appears in an egress allowlist.
        assert!(allow.contains("localhost"), "{allow}");
        assert!(allow.contains("127.0.0.1"), "{allow}");
    }

    #[test]
    fn lightpanda_does_not_get_chrome_arguments_it_refuses() {
        // Upstream: "Custom Chrome arguments (--args) are not supported with
        // Lightpanda" — setting it breaks every command rather than hardening.
        let env = browser_env_inner(&policy_for(BrowserEngine::Lightpanda), false);
        assert!(
            !names(&env).contains(&"AGENT_BROWSER_ARGS"),
            "{:?}",
            names(&env)
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "AGENT_BROWSER_ENGINE")
                .map(|(_, v)| v.as_str()),
            Some("lightpanda")
        );
    }

    #[test]
    fn chromium_keeps_the_no_sandbox_arguments_and_names_no_engine() {
        let env = browser_env_inner(&policy_for(BrowserEngine::Chromium), false);
        assert!(
            names(&env).contains(&"AGENT_BROWSER_ARGS"),
            "{:?}",
            names(&env)
        );
        // The default needs no naming, and naming it would be one more string
        // to keep in step with upstream's own default.
        assert!(!names(&env).contains(&"AGENT_BROWSER_ENGINE"));
    }
}

/// The **host-side** path of the box's `/tmp`, or `None` when the box's `/tmp`
/// is not reachable from the host at all.
///
/// [`box_tmp_root`] answers the box's question ("what path do I use?"), which
/// is not the same answer: on Linux the box says `/tmp` while the host sees
/// `<env>/tmp`, and on macOS both say the private backing. Confusing the two
/// is how a mediator ends up bound to a path nobody connects to — bind
/// succeeds, nothing is listening where the box looks, and enforcement is
/// silently absent.
///
/// `None` for image-backed tiers: a container's `/tmp` lives in the image, so
/// there is no host path to bind and the caller must say so rather than
/// binding a decoy.
/// Host-side path of the request log `h5i-browser-light` writes, when this box
/// has an engine that writes one and a `/tmp` the host can reach.
///
/// The box side is `H5I_BROWSER_RECEIPTS` ([`browser_light_env`]); this is the
/// same file seen from outside, which is what lets the console read the log
/// without asking the box for it. `None` rather than a guess when the engine is
/// not ours (Chromium's requests are the best-effort Fetch lane, a different
/// source with a different grade) or when there is no private `/tmp` to read
/// through — an image-backed tier keeps its `/tmp` inside the image.
/// Not [`host_tmp_root`], and the difference is the whole reason this exists:
/// that one answers a **live run's** question by reading `home_binds`, which is
/// `#[serde(skip)]` and therefore empty in any policy loaded back from disk. A
/// reader (the console) asking it would get `None` for every box and render an
/// empty stream for a session that had one. So the path comes from
/// [`private_tmp_backing`] — the same function `prepare_private_tmp` uses to
/// place the backing, so this calls the source of truth rather than
/// reconstructing a condition from grants that have since been rewritten.
///
/// The file need not exist: a box whose agent has not browsed yet has no log,
/// and the caller reads that as an empty stream rather than an error.
pub fn browser_request_log(h5i_root: &Path, m: &EnvManifest) -> Option<PathBuf> {
    let policy = load_policy(h5i_root, m).ok()?;
    // Only our own engine writes this log. Chromium's requests are the
    // best-effort Fetch lane — a different source with a different grade, and
    // pointing this at a box running Chromium would label that lane fail-closed.
    if policy.profile.engine? != crate::sandbox::BrowserEngine::H5iLight {
        return None;
    }
    // An image-backed tier keeps `/tmp` inside the image, so there is no host
    // path to read through.
    if policy.claim.image_backed() {
        return None;
    }
    // `None` where the box's `/tmp` is the host's.
    //
    // The leaf name is fixed by `browser_light_env`, which injects this path
    // into the box as `H5I_BROWSER_RECEIPTS` built from `box_tmp_root` and this
    // literal — so it cannot be qualified here without moving the injection
    // with it. Unqualified in a shared `/tmp` it is a world-writable path at a
    // well-known name: a second box, or any local user, can create it first and
    // have their rows rendered as this box's receipts, in a lane whose whole
    // claim is that its rows are evidence.
    //
    // An earlier comment here said the case was unreachable because these are
    // gated on the `browser` profile, which is supervised. That was wrong. The
    // gate above is on the *engine*, and `--engine h5i --isolation workspace`
    // sets one without the other.
    //
    // So it is refused, which an audit already renders honestly as a log this
    // machine cannot read. Qualifying both sides is the better answer and is a
    // change to the injection, not to this.
    let backing = box_tmp_on_host(h5i_root, m, &policy)?;
    if !tmp_is_redirected(&policy) {
        return None;
    }
    Some(backing.join("browser-requests.jsonl"))
}

/// Host-side path of the action log the resident session writes, when this box
/// runs our own engine.
///
/// The sibling of [`browser_request_log`], and `None` for the same two reasons:
/// only this engine writes one, and an image-backed tier keeps `/tmp` inside
/// the image where the host cannot read it.
///
/// Deliberately **not** [`crate::browser_proxy::actions_log`], which is the
/// mediator's own file in the env directory. Two sources, two lanes: that one
/// is what h5i watched cross a socket, this one is what the box says it did.
/// Pointing them at one path would launder a box-claimed row into a
/// host-observed pane, which is the exact confusion the lane split exists to
/// prevent.
pub fn browser_action_log(h5i_root: &Path, m: &EnvManifest) -> Option<PathBuf> {
    let policy = load_policy(h5i_root, m).ok()?;
    if policy.profile.engine? != crate::sandbox::BrowserEngine::H5iLight {
        return None;
    }
    if policy.claim.image_backed() {
        return None;
    }
    // Refused in a shared `/tmp`, for the reason `browser_request_log` gives:
    // the writer's path is injected as `H5I_BROWSER_ACTIONS` from the same
    // literal, so it cannot be qualified from this side alone.
    let backing = box_tmp_on_host(h5i_root, m, &policy)?;
    if !tmp_is_redirected(&policy) {
        return None;
    }
    Some(backing.join("browser-actions.jsonl"))
}

/// A file in the box's private `/tmp`, named in **both** views: what the box
/// calls it, and what this machine calls it.
///
/// The two are different on Linux (`/tmp/x` in the box is `<env>/tmp/x` here)
/// and the same on macOS (both say the private backing), which is exactly the
/// confusion that puts a listener at a path nobody connects to. Returning the
/// pair from one function means a caller cannot pick up one and use it as the
/// other.
///
/// `None` for an image-backed tier: its `/tmp` lives inside the image, so there
/// is no host path and a caller must say so rather than watching a decoy.
pub fn box_tmp_file(
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
) -> Option<(PathBuf, Option<PathBuf>)> {
    let policy = load_policy(h5i_root, m).ok()?;
    // The same leaf in both views, because they name one directory entry: at
    // the redirected tiers it is the bare name in a directory the box owns, and
    // at the workspace tier it carries the env id because that directory is the
    // host's own `/tmp`, shared with every other box on the machine.
    let leaf = box_tmp_leaf(m, name, tmp_is_redirected(&policy));
    let in_box = PathBuf::from(box_tmp_root(&policy)).join(&leaf);
    // The host view is the half that can be missing; the box always has a path
    // and it is always the qualified one. Returning `None` for the pair instead
    // made the caller invent its own bare `/tmp/<name>`, which is the shared,
    // unqualified path `box_tmp_leaf` exists to avoid — the collision, put back
    // by the fix for it.
    let on_host = box_tmp_on_host(h5i_root, m, &policy).map(|dir| dir.join(&leaf));
    Some((in_box, on_host))
}

/// Host-side path of the control file the box's resident session advertises.
///
/// The box side needs no path at all: `browser_light_env` points
/// `H5I_BROWSER_STREAM_FILE` into the box's own `/tmp`, `serve` defaults its
/// control file to that name with a `.control` extension, and `session` defaults
/// to the control file beside the stream file. So an engine started inside a box
/// with no flags, and a verb carried in with no flags, already agree.
///
/// What the host cannot do without this is *see* whether the engine is there.
/// That is what a session record needs — not to reach the engine, which happens
/// by carrying the verb into the box, but to answer `h5i browser list` without
/// opening a socket per row.
///
/// `None` for the same two reasons as its siblings: another engine writes no
/// such file, and an image-backed tier keeps `/tmp` where the host cannot look.
pub fn browser_control_file(h5i_root: &Path, m: &EnvManifest) -> Option<PathBuf> {
    let policy = load_policy(h5i_root, m).ok()?;
    if policy.profile.engine? != crate::sandbox::BrowserEngine::H5iLight {
        return None;
    }
    if policy.claim.image_backed() {
        return None;
    }
    // Refused in a shared `/tmp`, for the reason `browser_request_log` gives:
    // the daemon derives this directory from `H5I_BROWSER_STREAM_FILE`, which
    // is injected from the same literal.
    let backing = box_tmp_on_host(h5i_root, m, &policy)?;
    if !tmp_is_redirected(&policy) {
        return None;
    }
    Some(backing.join("agent-browser").join("h5i-light.control"))
}

/// The box's `/tmp`, as **this machine** sees it, from a *freshly loaded*
/// policy.
///
/// [`prepare_private_tmp`] gives a box a `/tmp` of its own at two tiers and
/// **only** two, and does nothing at the others — so at the workspace tier a
/// box writes to the host's real `/tmp`. Every reader of a file in a box's
/// `/tmp` used to assume the redirect always happened and watch
/// `<env>/tmp/...`, a directory nothing would ever create: `browser open --in`
/// on a workspace box started an engine that came up correctly, put its control
/// socket at `/tmp/h5i-browser.sock`, and was declared dead thirty seconds
/// later because h5i was watching somewhere else.
///
/// **The policy must not have been prepared yet.** That is the difference
/// between this and [`host_tmp_root`], which reads the recorded `home_bind`
/// instead and says in its own comment why re-deriving the condition broke it:
/// once `prepare_private_tmp` has run, the bare `/tmp` grant has been rewritten
/// to the backing path and this predicate answers "no redirect" for a box that
/// has one. Here the policy comes straight from [`load_policy`], the grants are
/// as the profile wrote them, and the predicate is the same one the preparer
/// will apply.
fn box_tmp_on_host(h5i_root: &Path, m: &EnvManifest, policy: &ResolvedPolicy) -> Option<PathBuf> {
    if tmp_is_redirected(policy) {
        return Some(private_tmp_backing(&m.dir(h5i_root).join("tmp")));
    }
    // No redirect, and the answer then depends on which tier it is rather than
    // on "not a container". At the workspace tier the box shares this machine's
    // namespaces, so what it calls `/tmp` is what this machine calls it. At a
    // hardened container it does not: that tier is not `image_backed()`, so it
    // reaches here, and handing back the host's literal `/tmp` would name a
    // directory with no relationship to the box's — worse than the nonexistent
    // path this used to return, because that one merely read as empty.
    match policy.claim {
        IsolationClaim::Workspace => Some(PathBuf::from(box_tmp_root(policy))),
        _ => None,
    }
}

/// A file name for a box's `/tmp`, qualified when that `/tmp` is shared.
///
/// With a redirect, `<env>/tmp` already belongs to one box and a bare name in
/// it is unique. Without one — the workspace tier — the directory is the
/// host's own `/tmp`, so a bare name is shared by every box on the machine and
/// by every other process on it. Two workspace boxes would then bind one
/// control socket and write one session's logs over another's, and `/tmp` is
/// world-writable so an unqualified name is also one any local user can create
/// first.
///
/// **Only for names h5i gives to both sides.** [`box_tmp_file`]'s pair is
/// handed straight to the engine as `--control-socket` / `--receipts` /
/// `--actions`, so qualifying it moves the writer and the reader together. The
/// three `browser_*` readers above are not like that: their writer's path is
/// injected into the box as an environment variable built from a hardcoded
/// literal, so qualifying the reader alone would put it at a path nothing
/// writes — the mismatch this module already had once. Those refuse to answer
/// in a shared `/tmp` instead.
///
/// The env id rather than the box's name, because the id is what does not move.
fn box_tmp_leaf(m: &EnvManifest, name: &str, redirected: bool) -> String {
    if redirected {
        return name.to_string();
    }
    let slug: String = m
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{name}-{slug}")
}

/// Whether this policy's `/tmp` is redirected to a directory of the box's own.
///
/// The predicate [`prepare_private_tmp`] applies, named once so the readers
/// that have to agree with it cannot drift from it.
fn tmp_is_redirected(policy: &ResolvedPolicy) -> bool {
    matches!(
        policy.claim,
        IsolationClaim::Process | IsolationClaim::Supervised
    ) && (policy.profile.fs_read.iter().any(|p| p == "/tmp")
        || policy.profile.fs_write.iter().any(|p| p == "/tmp"))
}

fn host_tmp_root(policy: &ResolvedPolicy, _env_dir: &Path) -> Option<PathBuf> {
    if policy.claim.image_backed() {
        return None;
    }
    // Read the mapping, do not re-derive it. `prepare_private_tmp` records the
    // `/tmp` redirect as a `home_bind` (target `/tmp`, backing `<env>/tmp` on
    // Linux, the short `/tmp/h5i-<digest>` on macOS), and that entry is the
    // only thing that knows whether the redirect actually applied for this
    // policy. An earlier version of this function reconstructed the condition
    // from `fs_read`/`fs_write` and got it wrong — by the time mediation is
    // engaged the bare `/tmp` grant has been rewritten to the backing path, so
    // the check said "no private tmp", the mediator bound the *host's* real
    // `/tmp`, and enforcement silently did not happen. Same class of bug as
    // the one this whole function exists to fix.
    if let Some(bind) = policy
        .home_binds
        .iter()
        .find(|b| b.target == Path::new("/tmp"))
    {
        return Some(bind.backing.clone());
    }
    // No redirect: the box uses the host's own `/tmp`, shared with every other
    // box and every other process on the machine. Mediating there would put a
    // *host-global* socket at a well-known name — two browser boxes would
    // unlink and rebind each other's rendezvous, so one box's verbs would be
    // judged against the other's deny list and recorded in the other's
    // receipt, and any same-uid process could connect and drive the browser.
    // A control that can be silently stolen is worse than one that is
    // honestly absent, so there is no mediation without a private /tmp.
    None
}

/// The daemon's session name. agent-browser defaults to `default`, and h5i
/// does not set one, so both sides can agree on it without a variable whose
/// spelling nobody has verified.
const DAEMON_SESSION: &str = "default";

/// Directory (under the box's `/tmp`) where the shim starts the real daemon.
/// One definition, shared with `browser`, which has to know the same path to
/// tell a real daemon from h5i's own listener.
use crate::browser::DAEMON_DIR_NAME;

/// The line that separates a captured run's stdout from its stderr.
///
/// Part of the evidence format, not decoration: a receipt has one output field
/// and a reader has to be able to tell the two streams apart in it. It is
/// public because the split has to be undone somewhere — `browser read --in`
/// puts a page back on stdout and its request log back on stderr — and two
/// spellings of a separator is how that quietly stops working.
pub const STDERR_BANNER: &str = "\n----- stderr -----\n";

/// Start mediating the browser daemon's socket for the duration of a run.
///
/// The daemon keeps running on a path the box has no grant for; the path the
/// box *is* given carries h5i's listener. Two details make the CLI accept it,
/// both learned by driving the real thing (see `browser_proxy`): the sibling
/// files it checks (`.version`, `.config`, `.stream`) are mirrored into the
/// visible directory, and the daemon has to have been started with the same
/// `AGENT_BROWSER_*` environment the box's CLI will compute, or the CLI
/// decides the daemon is stale and tries to replace it.
///
/// Returns `None` — never an error — when there is nothing to mediate yet: no
/// daemon has been started, or this engine is not driven by agent-browser. A
/// browser box whose agent has not opened anything must still be able to run.
fn engage_browser_mediation(
    policy: &ResolvedPolicy,
    env_dir: &Path,
) -> Option<crate::browser_proxy::MediatorHandle> {
    if policy.profile.name != "browser" {
        return None;
    }
    if !policy
        .profile
        .engine
        .map(|e| e.driven_by_agent_browser())
        .unwrap_or(true)
    {
        return None;
    }

    // Where the box looks, and where the shim keeps the real daemon — as the
    // *host* sees them. Derived from the same mapping `browser_env_inner` uses
    // for the box side, so the two cannot drift apart into a mediator nobody
    // connects to.
    let Some(tmp) = host_tmp_root(policy, env_dir) else {
        eprintln!(
            "h5i: this box has no private /tmp the host can reach (image-backed tier, or a \
             profile without a /tmp grant), so browser actions cannot be mediated and the \
             control lock remains advisory for this session."
        );
        return None;
    };
    let visible = tmp.join("agent-browser");
    let private = tmp.join(DAEMON_DIR_NAME);

    // Bound before the box runs, and before any daemon exists. The shim starts
    // the daemon on the private path and mirrors the files the CLI checks; if
    // h5i waited for that to happen first, the box's own first call would find
    // the mediated path empty and start an unmediated daemon on it.
    let upstream = private.join(format!("{DAEMON_SESSION}.sock"));

    let policy_actions =
        crate::browser_proxy::ActionPolicy::deny_all_of(policy.profile.browser_deny.clone());
    match crate::browser_proxy::spawn(
        &visible.join(format!("{DAEMON_SESSION}.sock")),
        &upstream,
        env_dir,
        policy_actions,
    ) {
        Ok(handle) => Some(handle),
        Err(e) => {
            // Fail loudly but do not fail the run: the lock was advisory before
            // this existed, and a browser box that cannot start should say so
            // rather than become unusable.
            eprintln!("h5i: browser mediation could not start ({e}); the control lock is advisory for this run");
            None
        }
    }
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

pub fn write_team_reply_spool(spool: &Path, request: &TeamReplySpool) -> Result<String, H5iError> {
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
            push_protected_hook_config(&mut files, rel.to_string(), path, claim)?;
        }
        // Deliberately NOT the host's own `~/.claude` / `~/.codex`.
        //
        // The box cannot write them in the first place: at the kernel tiers
        // `prepare_home_state` has already bind-redirected those directories to
        // a per-env copy, and the container tiers never mount host $HOME. So a
        // difference at exit could only ever be a *host-side* change — the
        // operator using Claude Code on the same machine during a long box
        // session, or a second box's guard.
        //
        // What the guard then did with that difference was destructive: restore
        // the pre-session content over the operator's edit, or, if the file had
        // not existed at session start, delete it outright — and fail the
        // session with a sandbox-violation error for something the sandbox
        // never did. Two concurrent boxes did it to each other.
        //
        // Worktree scope stays: `$WORK` is genuinely box-writable, and that is
        // the file the observation hook is defined in.
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
                    if f.parent_created
                        && let Some(parent) = f.path.parent()
                    {
                        let _ = std::fs::remove_dir(parent);
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
) -> Result<(), H5iError> {
    let original = std::fs::read(&path).ok();
    let mut sentinel_created = false;
    let mut parent_created = false;
    if claim.image_backed() && original.is_none() {
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
    let bad = |why: &str| {
        Err(H5iError::Metadata(format!(
            "invalid egress rule '{raw}': {why}"
        )))
    };
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
    if let Some(p) = port
        && p.parse::<u16>().is_err()
    {
        return bad("port out of range");
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
/// The env vars [`prepare_box_reach`] wants injected: `(capture spool, inbox)`.
type BoxReachEnv = (Vec<(String, String)>, Vec<(String, String)>);

/// The runtime-only grants that decide **what a box can reach**: its capture
/// spool, its inbox, its warm caches, and the host-side egress extras.
///
/// One function because the microvm tier hashes exactly these into its guest's
/// name. Two paths that prepare different sets resolve to different guests, and
/// creating one reaps the other — so a `box run` would silently kill a service
/// started moments earlier, and neither would look wrong on its own. That is
/// not hypothetical: it is what this function was extracted to fix.
///
/// Returns the env vars the capture spool and inbox want injected, in the order
/// `run` already injected them.
fn prepare_box_reach(
    h5i_root: &Path,
    m: &EnvManifest,
    work: &Path,
    policy: &mut sandbox::ResolvedPolicy,
    cache_write: Option<(&Path, &Path)>,
    capture_spool: bool,
) -> Result<BoxReachEnv, H5iError> {
    // An observer session captures nothing (it changes nothing), so it gets no
    // spool. Every other difference between callers would be a different guest,
    // which is why this is a parameter rather than a second copy of the block.
    let capture_env = if capture_spool {
        prepare_env_capture_spool(h5i_root, m, policy)?
    } else {
        Vec::new()
    };
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
        None => prepare_cache_mounts(h5i_root, work, policy),
    }
    let inbox_env = prepare_env_inbox(h5i_root, m, policy)?;
    // Host-side `h5i box allow` extras. Part of "what it can reach" and so part
    // of the guest's identity — a box whose allowlist widened must not be
    // served the guest that was enforcing the narrower one.
    apply_user_egress(policy);
    Ok((capture_env, inbox_env))
}

fn apply_user_egress(policy: &mut sandbox::ResolvedPolicy) {
    let user = user_allow_list();
    // `scopes_egress`, not `!net_egress.is_empty()`: a blank entry is a `Vec`
    // element and not a rule, so `net.egress = [""]` is a deny-all that the
    // length test reported as "the profile sets net.egress" — and the host-side
    // allow list was then merged into a box meant to reach nothing. SECURITY.md
    // states the property this restores: the list "merges into a profile that
    // already sets `net.egress` and never widens a deny-all one".
    let enforced = policy.claim.enforces_egress_allowlist() && policy.profile.scopes_egress();
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
/// Say which declared resource caps this tier cannot apply. h5i's rule is that
/// it never silently downgrades; a profile written to bound a runaway build
/// should not discover at 3am that the bound was dropped on this backend.
fn announce_unmapped_resources(policy: &sandbox::ResolvedPolicy) {
    if policy.claim != IsolationClaim::Container {
        return;
    }
    for note in crate::container::unmapped_resources(&policy.profile) {
        eprintln!("⦿ note: {note} is not enforceable at isolation=container and was not applied");
    }
}

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
    // Say the cost of the allowlist plan out loud. Reaching a host-side proxy
    // from a rootless container means `slirp4netns:allow_host_loopback=true`,
    // which exposes *every* host loopback service at the gateway address — not
    // just the proxy port. Choosing the allowlist therefore widens the box's
    // reach compared with plain NAT, and a reader deserves to know that from
    // the tier itself rather than from the source. (The supervised tiers do not
    // share this: nftables narrows the jail to the proxy port, and Seatbelt
    // refuses host loopback wholesale.)
    if policy.claim == IsolationClaim::Container {
        eprintln!(
            "⦿ note: the allowlist proxy is reached over host loopback, so host services on \
             127.0.0.1 are reachable from this box at the gateway address. The allowlist \
             governs what leaves the host, not what the box can reach on it."
        );
    }
}

/// Engage the profile's `[[auth]]` grants for a session, at the address this
/// tier can actually reach. Shared by `run` and `shell` — `shell` did not call
/// this at all, so an interactive agent silently lost the authenticated egress
/// a captured `box run` was given, which is backwards: the interactive session
/// is where the agent works.
fn engage_grants_for(
    policy: &sandbox::ResolvedPolicy,
) -> Result<crate::container::AuthGrantEngagement, H5iError> {
    if policy.profile.auth.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    match crate::container::grant_host_addr(policy.claim) {
        Some(addr) => crate::container::engage_auth_grants(&policy.profile, true, addr),
        None => Err(H5iError::Metadata(format!(
            "profile '{}' declares an [[auth]] grant, but a box at isolation '{}' cannot reach \
             a host-side grant proxy on this platform — the netns jail opens no port for it. \
             Use isolation=container (or microvm), or drop the grant (fail-closed).",
            policy.profile.name,
            policy.claim.as_str()
        ))),
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
    run_inner(repo, h5i_root, m, argv, None)
}

/// Run a command in a box that lives on a runner.
///
/// Deliberately a separate function rather than a branch inside [`run_inner`]:
/// almost nothing they share is real. A local run resolves binds, opens the
/// env worktree, computes the effective configuration and reads a tree from
/// disk; a remote one does none of those, because none of them describes
/// anything on this machine. Threading a placement through the local path
/// would have meant an `if` at every one of those steps, each of which is a
/// place for the two to drift.
///
/// What they *do* share is the part that matters: the receipt. The same
/// evidence, in the same store, under a lane that says where it was observed
/// (ROADMAP.md R10).
pub fn run_remote(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    argv: &[String],
    runner: &dyn crate::placement::RemoteRunner,
) -> Result<RunOutcome, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    // The same lifecycle gate `run_inner` applies. Without it a box that had
    // been applied or aborted could be run again — the box lives on the runner
    // until its lease or a `box rm` — and the run would rewrite a terminal
    // status back to `idle`, in the manifest and in `refs/h5i/env`, where it
    // travels to other clones as an ordinary run event.
    match m.status.as_str() {
        ST_CREATED | ST_RUNNING | ST_IDLE => {}
        other => {
            return Err(H5iError::Metadata(format!(
                "{}: status is '{other}' — `box run` is only valid before propose/apply/abort",
                m.id
            )));
        }
    }

    let env_dir_path = env_dir(h5i_root, &m.agent, &m.slug);
    // `RunLock` is flock, so it exists on Unix only — the same guard every other
    // writer in this file carries. Elsewhere the serialization is absent rather
    // than faked, which is the pre-existing property of this lock and not
    // something the remote path gets to decide differently.
    #[cfg(unix)]
    let _lock = RunLock::acquire(&m.dir(h5i_root))?;

    let box_id = crate::placement::remote_box_id(&m.id);
    let result = runner.exec(&crate::placement::RemoteExec {
        box_id: &box_id,
        argv,
        cwd: None,
        env: &[],
        timeout_secs: None,
    })?;

    let mut raw = result.stdout.clone();
    raw.extend_from_slice(&result.stderr);
    if result.output_truncated {
        // Said, because the runner said it. The receipt's own `raw_truncated`
        // is computed from this machine's cap and would record `false` for a
        // log the runner had already cut — a truncated log stored as a complete
        // one is the thing the flag exists to prevent.
        raw.extend_from_slice(
            b"\n[h5i: the runner truncated this output before sending it]\n",
        );
    }

    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        // Absent by construction, not by omission: the effective configuration
        // describes a kernel-tier invocation on *this* host, and there was not
        // one. A value here would be a measurement of the wrong machine.
        effective_digest: None,
        // Likewise empty rather than computed. `fs_overlap` answers "which
        // other boxes on this host share a writable path with this one", and a
        // box on another machine shares none of them.
        fs_overlap: Vec::new(),
        source: crate::placement::RUNNER_OBSERVED_LANE.to_string(),
        cmd: Some(argv.join(" ")),
        // The runner's path, said as such. A local-looking path here would be
        // a directory somebody could try to `cd` into.
        cwd: Some(crate::redact::sanitize_display(&format!(
            "{} (on {})",
            result.cwd,
            m.runner.as_deref().unwrap_or("runner")
        ))),
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        wall_ms: Some(result.wall_ms),
        cpu_ms: Some(result.cpu_ms),
        max_rss_kb: result.max_rss_kb.and_then(|kb| u64::try_from(kb).ok()),
        // Absent, not the base tree. This field means "the HEAD tree the run
        // was taken against", and every local producer supplies the live one.
        // Supplying the tree the box was *built* from would make every receipt
        // from a runner box carry one identical value — indistinguishable, to a
        // reader or a later tool, from a box where nothing ever changed. The
        // real answer is not knowable from here until an export brings it back,
        // and `None` is what the other producers that cannot know it already
        // use.
        git_tree: None,
        files: Vec::new(),
        egress: result.egress.clone(),
        browser: None,
        share: None,
        // Absent for the same reason `effective_digest` is: the
        // runtime-detection lane watches processes on *this* kernel, and the
        // run happened on another machine's. A block here would be a
        // measurement of the wrong host, and an empty one would read as a
        // quiet box. Extending the runner protocol to carry the far side's
        // block is future work, and until it exists the honest answer is to
        // say nothing rather than to say nothing happened.
        runtime: None,
    };
    let captured = crate::receipt::append(&env_dir_path, input, &raw)?;
    m.captures.push(captured.id.clone());

    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "run",
        Some(crate::redact::sanitize_display(&argv.join(" "))),
        Some(captured.id.clone()),
    )?;

    Ok(RunOutcome {
        capture_id: captured.id.clone(),
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        wall_ms: result.wall_ms as u128,
        cpu_ms: result.cpu_ms as u128,
        max_rss_kb: result.max_rss_kb,
        receipt: captured,
    })
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
        // A runner box reaching this function means the caller did not route it
        // to `run_remote`, which is a bug here rather than a limitation to
        // explain away — so it says so, instead of blaming the box.
        return Err(if is_remote(m) {
            H5iError::Metadata(format!(
                "{}: this box runs on `{}` and was not routed to its runner — \
                 use `h5i box run`, which does",
                m.id,
                m.runner.as_deref().unwrap_or("a runner")
            ))
        } else {
            no_workspace_err(m, "env run")
        });
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
    // Move this box's forum mail for as long as the run lasts: drain what the
    // agent posts, deliver what its peers post back. A no-op — and no thread —
    // when the box is not on a forum. Declared here so it outlives the run and
    // makes one final pass on the way out.
    let _forum = crate::forum_tender::SessionTender::start(repo.path(), h5i_root, m);

    // The stored policy, digest-verified, then re-resolved against a fresh
    // host probe (fail closed if the host can no longer satisfy the claim).
    let mut policy = load_policy(h5i_root, m)?;
    // Structural grants (like the implicit `$WORK` rw): the worktree must be a
    // functional git checkout inside the box.
    grant_box_git(repo, m, &work, &mut policy, false)?;
    prepare_private_paths(h5i_root, m, &mut policy, &work)?;
    prepare_private_tmp(h5i_root, m, &mut policy, None)?;
    let browser_shim = prepare_browser_shim(h5i_root, m, &mut policy)?;
    policy
        .loopback_ports
        .extend(live_service_ports(h5i_root, m));
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
    let (env_capture_env, env_inbox_env) =
        prepare_box_reach(h5i_root, m, &work, &mut policy, cache_write, true)?;
    let cargo_env = prepare_cargo_env(&work, &policy)?;
    announce_unmapped_resources(&policy);
    // §P1: a kernel-tier invocation serializes what it enforces to
    // `policy.effective.json`, written inside `build_confined_command` at the
    // apply seam; this run's capture record pins the digest below.
    #[cfg(target_os = "linux")]
    {
        policy.effective_out =
            Some(m.dir(h5i_root).join(crate::effective::EFFECTIVE_CONFIG_FILE));
    }

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
        &crate::secrets_broker::fingerprint_key(h5i_root)?,
    )?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    // Authenticated egress the profile declares (5.5). Host-side credentials
    // resolve here and stay here; the box only ever learns the proxy's origin.
    // Held for the run: dropping a handle shuts its proxy down.
    let (_auth_handles, auth_env) = engage_grants_for(&policy)?;
    // Every browser verb the agent issues walks through this for the length of
    // the run; dropping the handle takes the socket back down (M8).
    let browser_mediator = engage_browser_mediation(&policy, &m.dir(h5i_root));

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
    // The kernel-observed lane, started BEFORE the payload is spawned so the
    // payload's own `execve` is the first thing it sees (ROADMAP.md D6). A
    // refusal here is fatal only when the profile said `require = true`.
    let watch = match start_watch(&policy, h5i_root, &work) {
        Ok(w) => w,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            set_status(
                repo,
                h5i_root,
                m,
                ST_IDLE,
                "status",
                Some("idle (refused: unwatched)".into()),
                None,
            )?;
            return Err(e);
        }
    };
    let result = sandbox::run_with_env(&policy, &work, argv, &injected_env);
    // Stopped before anything else touches the box, so the block describes the
    // command and not the bookkeeping that follows it — the browser drain
    // below runs `sandbox::run_with_env` again, and its syscalls are h5i's
    // work, not the box's.
    let runtime_evidence = finish_watch(watch);
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
        raw.extend_from_slice(STDERR_BANNER.as_bytes());
        raw.extend_from_slice(&outcome.stderr);
    }
    if outcome.timed_out {
        raw.extend_from_slice(b"\n----- h5i env: killed by wall-clock limit -----\n");
    }

    // Scrub brokered secret values from the evidence by exact match, on top of
    // the pattern-based redaction the capture already applies — a token echoed
    // to stdout must never reach refs/h5i/objects even if it matches no pattern.
    raw = scrub_exact(&raw, &brokered.redactions);

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
        // Resolved from the policy, not derived from the env dir: `<env>/tmp` is
        // only the box's /tmp on Linux kernel tiers, and getting it wrong here
        // skips the drain entirely and reports a run that browsed as clean.
        .filter(|_| {
            host_tmp_root(&policy, &env_dir_path)
                .map(|root| crate::browser::browser_is_live(&root))
                .unwrap_or(false)
        })
        .map(|verb| {
            let tmp_root =
                host_tmp_root(&policy, &env_dir_path).unwrap_or_else(|| env_dir_path.join("tmp"));
            crate::browser::collect(&env_dir_path, &tmp_root, &verb, |drain_argv| {
                let out = sandbox::run_with_env(&policy, &work, drain_argv, &injected_env).ok()?;
                // A non-zero drain is a browser that is gone or wedged. Report
                // that as `unavailable`, never as a clean page.
                (out.exit_code == Some(0)).then_some(out.stdout)
            })
        });

    // What the mediator saw, in its own host-observed lane. Written before the
    // run's own receipt so the actions are on the log in the order they
    // happened.
    if let Some(mediator) = &browser_mediator {
        crate::browser_proxy::record_actions(
            &env_dir_path,
            &m.id,
            &m.policy_digest,
            &mediator.actions(),
        );
    }

    // Read HEAD from the WORKTREE repo so the tree recorded is the env's.
    let wt_repo = open_env_worktree(h5i_root, m)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        effective_digest: effective_digest_of(&env_dir_path),
        fs_overlap: fs_overlap_with_boxes(h5i_root, m),
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
        // Not a share. That lane is written by `h5i-share`, which sits above
        // this crate.
        share: None,
        // Kernel observed. Absent when the profile did not ask to be watched;
        // present with its reason when it asked and the probe could not
        // attach, because a missing block and a quiet box must never look the
        // same.
        runtime: runtime_evidence,
    };
    let captured = crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, &raw)?;
    let capture_id = captured.id.clone();

    m.captures.push(capture_id.clone());
    let observed = match ingest_shell_spool(repo, h5i_root, m, &brokered.redactions) {
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
        return Err(if is_remote(m) {
            // Truer than "no local workspace": the box exists and is healthy,
            // it is simply on a machine this milestone cannot run commands on
            // yet. A message about a missing directory would send someone
            // looking for a bug that is not there.
            remote_unsupported_err(m, "env shell")
        } else {
            no_workspace_err(m, "env shell")
        });
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
    // The forum tender, for the length of the session. An interactive shell is
    // where a human most often watches two agents talk, so this is the session
    // that most wants its mail moving.
    let _forum = crate::forum_tender::SessionTender::start(repo.path(), h5i_root, m);

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
        let root = m
            .dir(h5i_root)
            .join("ro")
            .join(std::process::id().to_string());
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
    policy
        .loopback_ports
        .extend(live_service_ports(h5i_root, m));
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
    // Through the same function `run` and `service start` use. Open-coding the
    // same four steps here worked only for as long as the two lists happened to
    // match: the next mount added to `prepare_box_reach` would have been absent
    // from a session, giving it a different guest name and reaping the guest
    // `box run` was using — with any service in it.
    let (env_capture_env, env_inbox_env) =
        prepare_box_reach(h5i_root, m, &work, &mut policy, None, !readonly)?;
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
        &crate::secrets_broker::fingerprint_key(h5i_root)?,
    )?;
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;
    // The interactive session gets the profile's declared authenticated egress
    // too — this is where the agent actually works, and `run` had it while
    // `shell` silently did not. Held for the session: dropping a handle shuts
    // its proxy down.
    let (_auth_handles, auth_env) = engage_grants_for(&policy)?;
    // ...and the browser mediator, for exactly the same reason the line above
    // exists: `run` had it while `shell` silently did not. An interactive
    // session is where a human is most likely to take control, so a shell that
    // left the lock unenforced was the worst place to leave the gap.
    let browser_mediator = engage_browser_mediation(&policy, &m.dir(h5i_root));
    let injected_env = merged_env(
        &merged_env(
            &merged_env(&merged_env(&brokered.env, &env_capture_env), &cargo_env),
            &merged_env(&env_inbox_env, &auth_env),
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
    // `apply_user_egress` already ran inside `prepare_box_reach`.
    announce_unmapped_resources(&policy);
    // §P1: the interactive session serializes what it enforces too — its
    // capture record pins the digest, same as a run.
    #[cfg(target_os = "linux")]
    {
        policy.effective_out =
            Some(m.dir(h5i_root).join(crate::effective::EFFECTIVE_CONFIG_FILE));
    }

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
    // Same lane, same ordering, for the interactive path. An interactive
    // session is where an agent does the most unobserved work — the tee shim
    // only sees commands a cooperating shell reported — so this is where a
    // kernel-observed second opinion is worth the most.
    let watch = match start_watch(&policy, h5i_root, &work) {
        Ok(w) => w,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            if !readonly {
                set_status(
                    repo,
                    h5i_root,
                    m,
                    ST_IDLE,
                    "status",
                    Some("idle (refused: unwatched)".into()),
                    None,
                )?;
            }
            return Err(e);
        }
    };
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
    let runtime_evidence = finish_watch(watch);
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
    let observed = match ingest_shell_spool(repo, h5i_root, m, &brokered.redactions) {
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

    // What the kernel saw during the session, as its own record. A session is
    // one shell and many commands, so the block is a summary of the whole
    // session rather than of any one of them — which is exactly what makes it
    // useful next to the tee shim's per-command records, and exactly why it is
    // a separate record instead of being folded into one of theirs.
    let runtime_note = match runtime_evidence {
        Some(ev) => {
            let note = format!(" runtime={}", ev.summary());
            match capture_shell_runtime(h5i_root, m, &work, ev, exit_code) {
                Ok(id) => m.captures.push(id),
                Err(e) => eprintln!("warning: shell runtime capture failed: {e}"),
            }
            note
        }
        None => String::new(),
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
            "interactive cmd=`{safe_cmd}` exit={exit_code}{observed_note}{egress_note}{runtime_note}"
        )),
        egress_capture,
    )?;

    // What the mediator decided during the session, in its own host-observed
    // lane. An interactive session is exactly where a human takes control, so
    // this is the record that says whether the lock held.
    if let Some(mediator) = &browser_mediator {
        crate::browser_proxy::record_actions(
            &m.dir(h5i_root),
            &m.id,
            &m.policy_digest,
            &mediator.actions(),
        );
    }

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

// ─── runtime detection: the kernel-observed lane (ROADMAP.md D1–D14) ────────

/// Build the collector's configuration from a resolved policy, or `None` when
/// the profile did not ask to be watched.
///
/// Everything the rules need is derived from the policy that is actually in
/// force, never from a global idea of what is suspicious: a box that was
/// *granted* `unix_sockets` is not reported for using them, and a box with an
/// unrestricted network is not reported for using it.
fn detect_config(
    policy: &crate::sandbox_policy::ResolvedPolicy,
    h5i_root: &Path,
    work: &Path,
) -> Option<h5i_bpf::DetectConfig> {
    let d = &policy.profile.detect;
    if !d.enabled {
        return None;
    }
    // `NetMode` is `deny|host`; the third state a rule cares about — "there is
    // an allowlist" — is the profile declaring egress hosts, which is what
    // puts a CONNECT proxy in front of the box.
    let net_mode = if policy.profile.scopes_egress() {
        "proxy"
    } else {
        match policy.profile.net_mode {
            crate::sandbox_policy::NetMode::Deny => "deny",
            crate::sandbox_policy::NetMode::Host => "allow",
        }
    };
    // The box's home, which on every kernel tier is this user's: `HOME` is in
    // the default `env.pass` and no tier below `container` gives the box one of
    // its own. Empty when unset, and `kernel_prefixes` then contributes no
    // home-relative prefixes rather than watching `/.ssh`, which is a real path
    // and not the one anybody meant.
    let home = std::env::var("HOME").unwrap_or_default();
    let control_dir = h5i_root.display().to_string();

    let mut cfg = h5i_bpf::DetectConfig {
        tier: h5i_bpf::Tier::parse(policy.claim.as_str()),
        buffer_kb: d.buffer_kb,
        rules: d.rules.clone(),
        context: h5i_bpf::RuleContext {
            net_mode: net_mode.to_string(),
            unix_sockets: policy.profile.unix_sockets,
            workspace: work.display().to_string(),
            home: home.clone(),
            control_dir: control_dir.clone(),
            // The addresses a proxied box is *supposed* to dial. Loopback is
            // already exempt in the rule itself, and every proxy h5i runs binds
            // loopback, so this stays empty on the tiers this lane covers
            // fully. It exists for the container tier, whose proxy is reached
            // at a gateway address — and that tier is `partial` for other
            // reasons anyway.
            proxy_peers: Vec::new(),
            enabled: Default::default(),
        },
        prefixes: h5i_bpf::kernel_prefixes(&home, &control_dir),
        open_all: false,
    };
    // Resolve here as well as inside `Watch::start`, so `context.enabled` is
    // populated for callers that inspect the config (and so `want_dotenv` is
    // answered from the resolved set rather than the selector strings).
    let _ = cfg.resolve();
    Some(cfg)
}

/// Start watching a run.
///
/// `Ok(None)` means the profile did not ask. `Err` means it asked with
/// `require = true` and the probe could not attach — the run is refused rather
/// than performed unwatched, which is the whole point of that switch.
fn start_watch(
    policy: &crate::sandbox_policy::ResolvedPolicy,
    h5i_root: &Path,
    work: &Path,
) -> Result<Option<h5i_bpf::Watch>, H5iError> {
    let Some(cfg) = detect_config(policy, h5i_root, work) else {
        return Ok(None);
    };
    let watch = h5i_bpf::Watch::start(cfg);
    if !watch.is_live() {
        let why = watch.refusal().unwrap_or("no reason given").to_string();
        if policy.profile.detect.require {
            return Err(H5iError::Metadata(format!(
                "this box's profile sets `[detect] require = true`, and the runtime-detection \
                 probe could not attach: {why}\n           \
                 Fix the cause above, or set `require = false` in `[profile.{}.detect]` to run \
                 unwatched (the receipt will say so).",
                policy.profile.name
            )));
        }
        // Not fatal, and not silent either: the block that reaches the receipt
        // carries this same reason, so the run is recorded as unwatched rather
        // than as quiet.
        eprintln!("note: runtime detection unavailable — {why}");
    }
    Ok(Some(watch))
}

/// Stop watching. `None` when nothing was.
fn finish_watch(watch: Option<h5i_bpf::Watch>) -> Option<h5i_bpf::RuntimeEvidence> {
    watch.map(|w| w.finish())
}

/// Persist what the kernel-observed lane saw during an interactive session.
///
/// Its own record rather than a field on somebody else's, because the thing it
/// describes is the *session*: a shell runs many commands, the tee shim writes
/// one record per command it managed to see, and this is the one observer that
/// covers the gaps between them. Written even when nothing fired — a watched
/// session with no detections is a result, and it is a different result from a
/// session nobody watched.
fn capture_shell_runtime(
    h5i_root: &Path,
    m: &EnvManifest,
    work: &Path,
    runtime: h5i_bpf::RuntimeEvidence,
    exit_code: i32,
) -> Result<String, H5iError> {
    let mut raw = format!("runtime detection ({}): {}\n", runtime.lane, runtime.summary());
    raw.push_str(&format!(
        "scope={} coverage={} seen={} lost={} filtered={}\n",
        runtime.scope,
        runtime.coverage.as_str(),
        runtime.events_seen,
        runtime.events_lost,
        runtime.events_filtered
    ));
    if let Some(why) = &runtime.coverage_reason {
        raw.push_str(&format!("note: {why}\n"));
    }
    for d in &runtime.detections {
        raw.push_str(&format!(
            "\n[{}] {} — {} ({} match{})\n",
            d.severity.as_str(),
            d.rule,
            d.title,
            d.count,
            if d.count == 1 { "" } else { "es" }
        ));
        for ex in &d.examples {
            raw.push_str(&format!("    {ex}\n"));
        }
        if d.examples_truncated {
            raw.push_str("    …\n");
        }
    }
    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        effective_digest: effective_digest_of(&m.dir(h5i_root)),
        fs_overlap: fs_overlap_with_boxes(h5i_root, m),
        source: "host-env-shell".into(),
        cmd: Some(format!("env shell {}", m.id)),
        cwd: Some(work.display().to_string()),
        exit_code: Some(exit_code),
        runtime: Some(runtime),
        ..Default::default()
    };
    Ok(crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, raw.as_bytes())?.id)
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
    let wt_repo = open_env_worktree(h5i_root, m)?;
    let head_tree = wt_repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|t| t.id().to_string());
    let input = crate::receipt::RecordInput {
        env_id: m.id.clone(),
        policy_digest: Some(m.policy_digest.clone()),
        effective_digest: effective_digest_of(&m.dir(h5i_root)),
        fs_overlap: fs_overlap_with_boxes(h5i_root, m),
        source: "host-env-shell".into(),
        cmd: Some(format!("env shell {}", m.id)),
        cwd: Some(work.display().to_string()),
        exit_code: Some(exit_code),
        git_tree: head_tree,
        egress: Some(eg.clone()),
        ..Default::default()
    };
    Ok(crate::receipt::append(&env_dir(h5i_root, &m.agent, &m.slug), input, raw.as_bytes())?.id)
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
    // Resolved, not just validated. The two checks above are the same pair the
    // persona sources get, and they have the same blind spot: a repo shipping
    // this path as a symlink puts a file from outside the worktree in front of
    // `bash --rcfile`, which *sources* it. `is_file()` follows links and would
    // have said yes.
    let full = resolve_within_work(work, rel).map_err(|e| {
        H5iError::Metadata(format!(
            "[shell] rcfile '{rel}' is not readable in the worktree: {e}"
        ))
    })?;
    if !std::fs::symlink_metadata(&full).is_ok_and(|md| md.is_file()) {
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

/// Replace every occurrence of each `secrets` value in `raw`, on the **bytes**.
///
/// This used to go `String::from_utf8_lossy(&raw).into_owned()` → `str::replace`
/// → `into_bytes()`, which is two mistakes at once:
///
/// * A binary payload came back **rewritten**. Every byte that is not valid
///   UTF-8 became U+FFFD, and `receipt::append` then digested and sized *that*
///   — so `raw_oid` and `raw_size` described bytes the run never produced,
///   whenever any secret happened to be brokered. The redaction module's own
///   rule is that storage keeps the exact bytes; only rendering is sanitised.
/// * It was a round trip through a lossy decoder to do a search that never
///   needed one. A credential is a byte string and matching it as one is both
///   exact and cheaper.
///
/// The marker is the same text the string version used, so nothing downstream
/// has to learn a new one.
fn scrub_exact(raw: &[u8], secrets: &[String]) -> Vec<u8> {
    const MARKER: &[u8] = b"[redacted secret]";
    let mut out = raw.to_vec();
    for secret in secrets {
        let needle = secret.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let mut next = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if out[i..].starts_with(needle) {
                next.extend_from_slice(MARKER);
                i += needle.len();
            } else {
                next.push(out[i]);
                i += 1;
            }
        }
        out = next;
    }
    out
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
    // `symlink_metadata` then `open` would be two resolutions of a path in a
    // directory the box writes: it can stat as a regular file and be a symlink
    // by the time we open it. Open first with O_NOFOLLOW, then `fstat` that
    // descriptor, so the thing we check is the thing we read.
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let f = opts.open(p).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
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
///
/// `secrets` is the run's brokered values, scrubbed by exact match on top of the
/// pattern-based redaction `receipt::append` applies. `env run` has done this
/// since the broker existed — "a token echoed to stdout must never reach
/// refs/h5i/objects even if it matches no pattern" — and this lane, which is the
/// one an interactive agent actually works in, was not given the same list.
fn ingest_shell_spool(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    secrets: &[String],
) -> Result<usize, H5iError> {
    let spool = env_capture_spool_dir(h5i_root, m);
    if !spool.is_dir() {
        return Ok(0);
    }
    let work = m.work_dir(h5i_root);
    let wt_repo = open_env_worktree(h5i_root, m)?;
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
            raw.extend_from_slice(STDERR_BANNER.as_bytes());
            raw.extend_from_slice(&stderr);
        }

        let raw = scrub_exact(&raw, secrets);

        // The command string is box-controlled: redact secrets, flatten to one
        // line, and cap it before it lands in a manifest or event detail. The
        // brokered values go too — a credential passed on a command line is at
        // least as likely as one echoed to stdout.
        let cmd_text = String::from_utf8_lossy(&scrub_exact(cmd_text.as_bytes(), secrets))
            .into_owned();
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
        // The same exact-value scrub the tee-shim branch above gets. These are
        // two branches of one function reading one spool, and a credential does
        // not care which of them recorded it.
        let raw = scrub_exact(
            &read_spool_capped(&path_of("raw"), SPOOL_MAX_OUTPUT_BYTES).unwrap_or_default(),
            secrets,
        );
        let meta_cmd =
            String::from_utf8_lossy(&scrub_exact(meta.cmd.as_bytes(), secrets)).into_owned();
        let safe_cmd: String = crate::secrets::redact_text(&meta_cmd)
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
        let wt_repo = open_env_worktree(h5i_root, m)?;
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
        // A remote box's branch exists from creation and points at the base until a
    // propose advances it. Diffing base against base renders an empty patch and
    // exits zero — "this box changed nothing" for a box that may have rewritten
    // its whole tree on the runner. An empty answer that looks like a fact is
    // worse than a refusal.
    if is_remote(m) {
        let tip = repo
            .find_reference(&m.branch)
            .and_then(|r| r.peel_to_commit())
            .map(|c| c.tree_id().to_string())
            .unwrap_or_default();
        if tip == m.base_tree {
            return Err(H5iError::Metadata(format!(
                "{}: this box runs on `{}` and its work has not been brought home yet, so \
                 there is nothing here to diff. `h5i box propose {}` fetches it.",
                m.id,
                m.runner.as_deref().unwrap_or("a runner"),
                m.slug
            )));
        }
    }
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
        let wt_repo = open_env_worktree(h5i_root, m)?;
        let base_tree = wt_repo.find_tree(git2::Oid::from_str(&m.base_tree)?)?;
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = wt_repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
        render(diff)
    } else {
        // A remote box's branch exists from creation and points at the base until a
    // propose advances it. Diffing base against base renders an empty patch and
    // exits zero — "this box changed nothing" for a box that may have rewritten
    // its whole tree on the runner. An empty answer that looks like a fact is
    // worse than a refusal.
    if is_remote(m) {
        let tip = repo
            .find_reference(&m.branch)
            .and_then(|r| r.peel_to_commit())
            .map(|c| c.tree_id().to_string())
            .unwrap_or_default();
        if tip == m.base_tree {
            return Err(H5iError::Metadata(format!(
                "{}: this box runs on `{}` and its work has not been brought home yet, so \
                 there is nothing here to diff. `h5i box propose {}` fetches it.",
                m.id,
                m.runner.as_deref().unwrap_or("a runner"),
                m.slug
            )));
        }
    }
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
    // A manifest is not always one this machine wrote: `h5i pull` materialises
    // a peer's from `refs/h5i/env`, and `validate_imported_manifest` pins the
    // identity fields and the object ids and nothing else. Everything variable
    // below is therefore box- or peer-supplied text on its way to a terminal,
    // which is the surface an escape sequence acts on — `m.source` was already
    // cleaned here for exactly that reason, and its neighbours were not.
    use crate::redact::sanitize_display as clean;
    let mut out = String::new();
    // Said first, because everything below it is about a machine that is not
    // this one. `box create` deliberately suppresses the work path for a runner
    // box so nobody tries to `cd` into a directory that was never made; this is
    // the command those users are pointed at, and it used to print local paths,
    // local grants and local limits without ever mentioning the runner.
    if let (Some(runner), Some(id)) = (&m.runner, &m.runner_id) {
        out.push_str(&format!("  runs on  : {} ({})\n", clean(runner), short(id, 12)));
        out.push_str(
            "             the workspace and the confinement below are enforced there,              not here\n",
        );
    }
    out.push_str(&format!("── {} ──\n", clean(&m.id)));
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
    out.push_str(&format!("  status   : {}{}\n", clean(&m.status), stale_note));
    for s in &live {
        out.push_str(&format!(
            "  live     : {} pid {} since {}{}\n",
            clean(&s.kind),
            s.pid,
            clean(&s.started_at),
            s.command
                .as_ref()
                .map(|c| format!(" — {}", clean(c)))
                .unwrap_or_default()
        ));
    }
    out.push_str(&format!("  agent    : {}\n", clean(&m.agent)));
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
        short(&m.base_commit, 12),
        clean(&m.parent_branch)
    ));
    out.push_str(&format!("  branch   : {}\n", clean(&m.branch)));
    out.push_str(&format!(
        "  policy   : profile={} isolation={} digest={}\n",
        clean(&m.profile),
        clean(&m.isolation_claim),
        short(&m.policy_digest, 12)
    ));
    // What this box has been shown by other agents. Not a verdict on the text —
    // h5i does not claim to tell a hostile message from an ordinary one — but a
    // fact a reviewer needs before treating this box's output as evidence about
    // this box alone. A box that never appears here read nothing a peer wrote.
    if let Some(influence) = crate::forum_tender::peer_influence(h5i_root, m) {
        out.push_str(&format!(
            "  forum    : peer-influenced since {} by {}\n",
            clean(&influence.since),
            influence
                .senders
                .iter()
                .map(|s| clean(s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        out.push_str(
            "             its output reflects that conversation; verify with a box that read none of it\n",
        );
    }
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
        // The line above is the DIGESTED policy — what `policy.resolved.toml`
        // pinned. Every session then adds structural grants after that digest
        // is verified (`grant_box_git`, the spool, the per-env HOME, cache
        // mounts, the private /tmp), and they are not re-digested. A reviewer
        // asking "what could this box touch?" was being shown an fs.write list
        // that omits the git object store and the box's own worktree admin dir
        // — the grants that matter most. Name them, as a separate section so it
        // stays clear which half is digest-pinned.
        out.push_str(
            "  + at run : <env>/spool, <env>/home, <env>/tmp, cache mounts, and the box's git              plumbing (.git/objects rw, .git/worktrees/<wt> rw, refs/heads/h5i/env/<agent> rw,              .git/config ro)
             these are added per session, after the policy digest              is checked, and are not part of it
",
        );
        // One footnote beats a caveat on every number, and it names the reason
        // rather than leaving the reader to infer it from a marker.
        let declared_unenforced =
            (!limits.mem && p.mem_bytes.is_some()) || (!limits.procs && p.max_procs.is_some());
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
        // The runtime-detection lane, and — the part that matters — whether
        // this host can actually deliver it. A profile that says
        // `enabled = true` on a machine with no `CAP_BPF` is watching nothing,
        // and a status page that printed only the profile's intent would be
        // the exact "reads like enforcement, enforces nothing" failure this
        // product keeps finding in itself.
        if p.detect.enabled {
            let caps = crate::bpf::probe_host();
            out.push_str(&format!(
                "  detect   : rules={} buffer={}KiB{}\n",
                p.detect.rules.join(","),
                p.detect.buffer_kb,
                if p.detect.require {
                    " require=true"
                } else {
                    ""
                }
            ));
            match caps.unavailable_reason() {
                None => {
                    let (cov, why) = crate::bpf::Tier::parse(policy.claim.as_str()).coverage();
                    out.push_str(&format!(
                        "             kernel-observed, coverage={} on the {} tier{}\n",
                        cov.as_str(),
                        policy.claim.as_str(),
                        why.map(|w| format!(" — {w}")).unwrap_or_default()
                    ));
                }
                Some(why) => out.push_str(&format!(
                    "             NOT watching on this host — {}{}\n",
                    clean(&why),
                    caps.fix
                        .as_deref()
                        .map(|f| format!("\n             fix: {f}"))
                        .unwrap_or_default()
                )),
            }
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
                    .filter(|u| {
                        !p.net_egress
                            .iter()
                            .any(|e| e.trim().eq_ignore_ascii_case(u))
                    })
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
        format!(": {} [{}]", clean(&m.captures.join(", ")), clean(&sources))
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
            out.push_str(&format!("             ↳ capture `{}`\n", clean(cmd)));
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
                short(&m.policy_digest, 12)
            )
        ),
        Err(e) => chk!("policy", false, false, format!("{e}")),
    }

    // 2. Enforcement readiness — can the host actually run this claim?
    //
    // For a runner box the honest answer is that this machine is not the one
    // confining it, and probing here would answer about the wrong kernel. The
    // dangerous direction is the green one: a remote `supervised` box inspected
    // from a host that also does `supervised` would report "functionally
    // runnable here" and `healthy: true` — a false assurance produced by
    // measuring somebody else's machine. `h5i runner probe` is the question
    // that has an answer.
    if is_remote(m) {
        chk!(
            "enforcement",
            true,
            true,
            format!(
                "this box is confined on `{}`, not here — `h5i runner probe {}` asks the \
                 machine that is doing it",
                m.runner.as_deref().unwrap_or("a runner"),
                m.runner.as_deref().unwrap_or("<name>")
            )
        );
    } else {
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
    }

    // 3. Workspace — present for live envs, advisory-absent for pulled/gc'd ones.
    if has_workspace(m, h5i_root) {
        chk!("workspace", true, false, "git worktree present".into());
    } else if is_remote(m) {
        // Neither pulled nor gc'd: this box's workspace is on another machine
        // and was never meant to be here.
        chk!(
            "workspace",
            true,
            true,
            format!(
                "the workspace is on `{}` — `h5i box propose {}` brings the work home",
                m.runner.as_deref().unwrap_or("a runner"),
                m.slug
            )
        );
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
    /// `fp:<12>` (keyed, see [`crate::secrets_broker::fingerprint`]) when
    /// resolvable (env:/file:), else `None`.
    pub fingerprint: Option<String>,
}

/// Resolve each declared grant's *status* without injecting it — the read-only
/// surface behind `h5i box secrets`. `command:` extractors are never executed
/// here (they have host-side side effects); they show as "not evaluated".
pub fn secrets_status(h5i_root: &Path, policy: &ResolvedPolicy) -> Vec<SecretStatus> {
    // Best-effort: a fingerprint the reviewer cannot compare is better than one
    // they can grind, so a key we cannot mint drops the fingerprint entirely.
    let fp_key = crate::secrets_broker::fingerprint_key(h5i_root).ok();
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
                        fp_key
                            .as_ref()
                            .map(|k| crate::secrets_broker::fingerprint(k, &v)),
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
    use crate::redact::sanitize_display as clean;
    let mut out = String::new();
    out.push_str(&format!("── secrets for {} ──\n", clean(env_id)));
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
        // `source` is repo-supplied and only its *prefix* is validated
        // (`env:`/`file:`/`command:`), so everything after it is a free string
        // from `.h5i/env.toml` on its way to a terminal. So are `ttl` and the
        // status text, which quotes an error.
        out.push_str(&format!(
            "  {:<20} source={} inject={}{}  [{}]{}\n",
            clean(&s.name),
            clean(&s.source),
            clean(&s.inject),
            clean(&ttl),
            clean(&s.status),
            clean(&fp)
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
    /// The kernel's own start time for this pid, in clock ticks since boot
    /// (field 22 of `/proc/<pid>/stat`).
    ///
    /// A pid alone is not an identity. A crashed session leaves its record
    /// behind, the kernel hands that number to something else, and `kill(pid,
    /// 0)` says yes — so the registry reports a live session belonging to an
    /// unrelated process of the same user. For most readers that is a cosmetic
    /// staleness that the next scan heals. For `h5i box share` it is not:
    /// `box_pid` walks that pid's descendants looking for a network namespace,
    /// and if the impostor or one of its children has one, the share enters
    /// *that* namespace and publishes `127.0.0.1:<port>` from it — precisely
    /// the wrong-port exposure the namespace check exists to refuse.
    ///
    /// Start time closes it: the pair (pid, start time) is unique for as long
    /// as the process lives, and the kernel cannot reissue it. `None` for a
    /// record written by an older h5i, which reads as "cannot verify" — see
    /// [`live_identity_holds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ticks: Option<u64>,
}

/// The kernel's start time for a pid, in clock ticks since boot.
///
/// Field 22 of `/proc/<pid>/stat`, parsed from the *end* of the line: field 2
/// is the executable name in parentheses and may itself contain spaces and
/// parentheses, so splitting the whole line on whitespace is how this gets
/// read wrong.
#[cfg(target_os = "linux")]
pub fn proc_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    // Fields 3.. follow the closing parenthesis, so field 22 is the 20th here.
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// The same identity, from Darwin's process table.
///
/// `PROC_PIDTBSDINFO` carries `pbi_start_tvsec`/`pbi_start_tvusec`, the wall
/// time the process began, which serves the purpose the Linux tick count serves
/// — the pair (pid, start time) is unique for as long as the process lives and
/// the kernel cannot reissue it. The unit differs and nothing compares across
/// platforms: a record is only ever checked against a pid on the machine that
/// wrote it.
///
/// Answering `None` here was not neutral, and that is why this exists. macOS
/// wrote `started_ticks: None` into every live record, `session_pid_verified`
/// skips records without one, and `h5i box share` asks for the verified
/// answer — so the pid-reuse hardening turned into a total refusal on macOS,
/// with `h5i box share` reporting "has no session running" for boxes whose
/// session was running the whole time. A platform that cannot verify identity
/// fails a check written to be strict; the fix is to let it verify.
#[cfg(target_os = "macos")]
pub fn proc_start_ticks(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `proc_pidinfo` writes at most `size` bytes into `info`, and the
    // return value is checked to be exactly that before anything is read.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got != size {
        return None;
    }
    // Microseconds since the epoch, which is monotone enough for an identity:
    // it is compared only against itself, never used as a duration.
    Some(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn proc_start_ticks(_pid: u32) -> Option<u64> {
    None
}

/// Is this record still about the process it was written for?
///
/// `true` when the recorded start time matches the pid's, and `true` when
/// there is nothing to compare — an older record, or a platform with no
/// `/proc`. Callers that cannot tolerate the unverifiable case check
/// `started_ticks.is_some()` themselves; `h5i box share` does.
pub fn live_identity_holds(rec: &LiveSession) -> bool {
    match (rec.started_ticks, proc_start_ticks(rec.pid)) {
        (Some(recorded), Some(now)) => recorded == now,
        _ => true,
    }
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
            // Written so a later reader can tell this process from whatever
            // inherits its pid after a crash. See the field's own comment for
            // what that costs `h5i box share`.
            started_ticks: proc_start_ticks(pid),
        };
        if let Ok(json) = serde_json::to_string(&rec) {
            // Atomic: `live_sessions` unlinks anything it cannot parse, so a
            // reader catching a half-written record would delete a healthy
            // session's registration and leave the box reported as "stale — no
            // live session holds this env" for the rest of its life.
            let _ = atomic_write(&path, json.as_bytes());
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
        // A record whose name is a live pid is not ours to delete, whatever it
        // parses as: writes are atomic now, but a half-written file from an
        // older h5i (or any transient read error) must not cost a running
        // session its registration. Reap only what is provably gone.
        let owner = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok());
        let parsed = std::fs::read_to_string(&p)
            .ok()
            .and_then(|text| serde_json::from_str::<LiveSession>(&text).ok());
        match parsed {
            Some(rec) if pid_alive(rec.pid) => out.push(rec),
            Some(_) => {
                let _ = std::fs::remove_file(&p);
            }
            None => {
                if !owner.is_some_and(pid_alive) {
                    let _ = std::fs::remove_file(&p);
                }
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
    /// The service's session-leader pid, **in the namespace `runtime` names**.
    /// Never signal this without dispatching on `runtime` first: a guest pid
    /// handed to `kill(2)` names an unrelated host process.
    pub pid: u32,
    pub command: String,
    pub started_at: String,
    pub port: Option<u16>,
    /// Allocated per-env host port, injected as `H5I_ENV_PORT_<NAME>` (Idea 2).
    /// `None` at the microvm tier, where the box has its own network stack and
    /// so nothing to collide with — the service binds its declared port.
    pub dynamic_port: Option<u16>,
    pub log: String,
    /// Where `pid` lives. Defaulted, so records written before the microvm tier
    /// gained services still parse — they were all host processes.
    #[serde(default)]
    pub runtime: ServiceRuntime,
}

/// Which world a [`ServiceRecord`]'s pid belongs to.
///
/// A pid only means something inside the pid namespace that issued it. A guest
/// pid and a host pid are not two values of one kind; they are the same
/// integers naming unrelated processes. This exists so that no code path can
/// signal one believing it is the other — `service_stop` signals a process
/// *group*, so getting it wrong would take out an unrelated tree on the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ServiceRuntime {
    /// A host process, signalable directly. Every tier but `microvm`.
    #[default]
    Host,
    /// A process inside the box's warm microVM guest, reachable only through
    /// the runtime. `sandbox` is the guest's name; if it is no longer the box's
    /// current guest, the service is dead by construction — a policy change
    /// rotated the guest out from under it.
    ///
    /// `boot` is that guest's kernel boot identity. The name alone is not
    /// enough: a guest keeps it across `stop`/`start` while its pids restart
    /// from 1, so without this a stale record could match an unrelated process
    /// in the guest's next life — refusing a start that should succeed, and
    /// signalling a process group that was never ours.
    Guest {
        sandbox: String,
        #[serde(default)]
        boot: String,
    },
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

/// The port this service actually listens on, host-allocated or declared.
///
/// The kernel tiers allocate one per env and inject it, because every box
/// shares the host's network and two would collide. A microvm box has its own
/// stack, so nothing is allocated and the declared port is the answer — which
/// still has to be *reported*, or the tier that just gained services would be
/// the one whose ports never show up.
fn service_port(rec: &ServiceRecord) -> Option<u16> {
    rec.dynamic_port.or(rec.port)
}

/// Is this service still running, wherever its pid lives?
///
/// The single place liveness is decided, so no caller can reach for
/// [`pid_alive`] with a pid that is not a host pid. At the microvm tier the
/// question is asked of the guest, and a guest that no longer exists answers
/// it: the service died when the guest was rotated or removed.
fn service_alive(rec: &ServiceRecord) -> bool {
    match &rec.runtime {
        ServiceRuntime::Host => pid_alive(rec.pid),
        ServiceRuntime::Guest { sandbox, boot } => {
            h5i_sandbox::microvm::service_alive(sandbox, rec.pid, boot)
        }
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
    atomic_write(&env_dir.join("services.json"), json.as_bytes())?;
    Ok(service_defs_digest(&defs))
}

/// Load the env's service declarations from the **pinned** env-local manifest,
/// verifying its content digest against the one recorded at create — so an agent
/// editing the (writable) worktree `.h5i/env.toml` after create can't change
/// which long-lived command a service runs. Falls back to the worktree/repo
/// config only for envs created before pinning existed (no recorded digest).
/// The service definitions pinned at box creation, or `None` when this env
/// predates pinning. Deliberately does **not** fall back to the worktree copy:
/// callers that must not read box-writable input use this one.
fn pinned_service_defs(
    h5i_root: &Path,
    m: &EnvManifest,
) -> Option<std::collections::BTreeMap<String, ServiceDef>> {
    let pinned = pinned_services_path(h5i_root, m);
    let text = std::fs::read_to_string(&pinned).ok()?;
    let defs: std::collections::BTreeMap<String, ServiceDef> = serde_json::from_str(&text).ok()?;
    // The digest, like `load_service_defs` checks on the same file. This
    // function is the one documented as being for "callers that must not read
    // box-writable input", and it was reading the file without the check that
    // establishes the file is the one that was pinned — a weaker guarantee than
    // its sibling's, under a stronger claim.
    //
    // A mismatch answers `None`, which `load_policy` reads as "may host
    // services". That is the conservative direction the caller's own comment
    // names: guessing false costs a killed dev server, guessing true costs a
    // guest that lives until `box rm`.
    if let Some(expected) = &m.service_digest
        && &service_defs_digest(&defs) != expected
    {
        return None;
    }
    Some(defs)
}

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
    // The definition is read under the same lock that the start runs under, so
    // a `.h5i/env.toml` edited mid-start cannot produce a record describing a
    // command other than the one that ran.
    let _svc_lock = ServiceLock::acquire(&m.dir(h5i_root))?;
    let defs = load_service_defs(h5i_root, m)?;
    let def = defs
        .get(name)
        .ok_or_else(|| {
            H5iError::Metadata(format!(
                "no service '{name}' declared in .h5i/env.toml ([service.{name}])"
            ))
        })?
        .clone();
    start_service_inner(repo, h5i_root, m, name, &def)
}

/// Start a long-lived in-box process from a definition the **caller** holds,
/// rather than one declared in the box's `.h5i/env.toml`.
///
/// This exists for one caller: a browser session placed in a box
/// (`h5i browser open --in`). A resident browser is a service in every way
/// that matters here — it outlives the command that started it, it must not
/// hold the writer lock, and it wants the pid registry and the log capture —
/// but it is **not** something the repository declares. Requiring a
/// `[service.…]` block would mean `--in` could only ever work in a repository
/// that had been edited to permit it, and writing that block ourselves would
/// mean h5i editing the user's tree to run a command.
///
/// Everything else is identical, deliberately: same lock, same policy
/// preparation, same `spawn_background`, same record, same event. A second
/// launch path for in-box processes is a second set of grants to keep in step,
/// which is the kind of drift this codebase keeps writing tests against.
pub fn service_start_with_def(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
    def: &ServiceDef,
) -> Result<ServiceRecord, H5iError> {
    validate_service_name(name)?;
    let _svc_lock = ServiceLock::acquire(&m.dir(h5i_root))?;
    start_service_inner(repo, h5i_root, m, name, def)
}

/// The body both start paths share. **The service lock is the caller's**: it is
/// taken before the definition is resolved, because resolving it is part of
/// what the lock protects.
fn start_service_inner(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    name: &str,
    def: &ServiceDef,
) -> Result<ServiceRecord, H5iError> {
    let svc_dir = services_dir(h5i_root, m);
    std::fs::create_dir_all(&svc_dir).map_err(|e| H5iError::with_path(e, &svc_dir))?;
    if let Some(rec) = read_service_record(&svc_dir, name)
        && service_alive(&rec)
    {
        return Err(H5iError::Metadata(format!(
            "service '{name}' is already running (pid {}) — stop it first",
            rec.pid
        )));
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
    // The same reach `run` grants, and it must be the same or the microvm tier
    // gives this path its own guest and reaps the one `box run` is using — with
    // whatever services were running in it. The injected vars are dropped: a
    // service is not a captured run and has no receipt to write into.
    let (_capture_env, _inbox_env) =
        prepare_box_reach(h5i_root, m, &work, &mut policy, None, true)?;
    // And the same interactive config lockdown, for the same reason twice over:
    // a service must not be able to rewrite the agent's hook config any more
    // than a run can, *and* those files being present is what puts their mounts
    // in the microvm create argv. Prepared only here, they would be absent, the
    // argv would differ by two mounts, and this path would quietly get a guest
    // of its own — the failure that made this call necessary.
    let protected_hook_configs = ProtectedHookConfigGuard::prepare(&work, policy.claim)?;

    // A guest has its own network stack, so two boxes cannot collide on a port
    // and there is nothing for a dynamic allocation to solve. The service binds
    // the port it declares, and `PORT` says so. On the kernel tiers, where every
    // box shares the host's network, the allocation is what keeps them apart.
    let in_guest = policy.claim == sandbox::IsolationClaim::Microvm;
    let mut injected: Vec<(String, String)> = Vec::new();
    let dynamic_port = match (def.port, in_guest) {
        (Some(declared), true) => {
            let key = env_key(name);
            injected.push((format!("H5I_ENV_PORT_{key}"), declared.to_string()));
            injected.push((format!("{key}_DYNAMIC_PORT"), declared.to_string()));
            injected.push(("PORT".into(), declared.to_string()));
            None
        }
        (Some(_), false) => {
            let p = alloc_free_port().ok_or_else(|| {
                H5iError::Metadata("could not allocate a free host port for the service".into())
            })?;
            let key = env_key(name);
            injected.push((format!("H5I_ENV_PORT_{key}"), p.to_string()));
            injected.push((format!("{key}_DYNAMIC_PORT"), p.to_string()));
            // PORT is the de-facto convention many dev servers read.
            injected.push(("PORT".into(), p.to_string()));
            Some(p)
        }
        (None, _) => None,
    };

    // At the microvm tier the log is written by the guest into a directory
    // mounted for that purpose, so the host reads it at a path of the tier's
    // choosing rather than one it created here.
    let log = if in_guest {
        h5i_sandbox::microvm::service_log_path(&work, name)?
    } else {
        svc_dir.join(format!("{name}.log"))
    };
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let argv = vec!["sh".to_string(), "-c".to_string(), def.command.clone()];
    // Restored on **both** paths. `prepare` writes empty sentinel configs into
    // `$WORK` at the image-backed tiers when none exist, so returning early
    // through `?` would leave them there for good: they show up in the user's
    // `git status`, and the next guard reads the empty file as the original and
    // reports a spurious tamper when the agent later writes a real one.
    let handle = match sandbox::spawn_background(&policy, &work, &argv, &injected, &log, name) {
        Ok(h) => h,
        Err(e) => {
            let _ = protected_hook_configs.finish();
            return Err(e);
        }
    };
    // The service keeps running: at the microvm tier its mounts were fixed when
    // the guest was created, and at the kernel tiers the lockdown is a property
    // of the launched process, not of the files afterwards.
    if let Err(e) = protected_hook_configs.finish() {
        eprintln!("service start: could not restore the agent hook config: {e}");
    }

    let rec = ServiceRecord {
        name: name.to_string(),
        pid: handle.pid,
        command: def.command.clone(),
        started_at: now_ts(),
        port: def.port,
        dynamic_port,
        log: log.display().to_string(),
        runtime: match (handle.sandbox, handle.boot) {
            (Some(sandbox), Some(boot)) => ServiceRuntime::Guest { sandbox, boot },
            _ => ServiceRuntime::Host,
        },
    };
    atomic_write(
        &service_record_path(&svc_dir, name),
        serde_json::to_string_pretty(&rec)?.as_bytes(),
    )?;

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
                "start {name} pid={}{where_}{port_note} cmd=`{safe_cmd}`",
                rec.pid,
                where_ = match &rec.runtime {
                    // A reader of the event log must be able to tell a host pid
                    // from a guest pid; the numbers alone cannot.
                    ServiceRuntime::Guest { sandbox, .. } => format!(" guest={sandbox}"),
                    ServiceRuntime::Host => String::new(),
                }
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
    // Same lock as `service_start`: a stop racing a start for one name could
    // otherwise read the old record, signal the old pid, and then delete the
    // record the concurrent start had just written — orphaning the service it
    // started.
    let _svc_lock = ServiceLock::acquire(&m.dir(h5i_root))?;
    let svc_dir = services_dir(h5i_root, m);
    let rec = read_service_record(&svc_dir, name).ok_or_else(|| {
        H5iError::Metadata(format!("service '{name}' is not running (no record)"))
    })?;

    // TERM the whole process group, grace, then KILL — identically on both
    // sides of the boundary. What differs is *who* may interpret the pid: the
    // host branch is guarded by `ServiceRuntime::Host` and never sees a guest
    // pid, which if signalled here would take out an unrelated host process
    // group that merely shares the number.
    match &rec.runtime {
        ServiceRuntime::Host => {
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
        }
        ServiceRuntime::Guest { sandbox, boot } => {
            // Tri-state on purpose. Reading "the runtime did not answer" as
            // "already dead" would skip the signal and then delete the record
            // below, leaving the service running in the guest with nothing on
            // the host that knows it exists. Refuse instead, and keep the
            // record so the stop can be retried.
            let alive = h5i_sandbox::microvm::service_state(sandbox, rec.pid, boot).ok_or_else(|| {
                H5iError::Metadata(format!(
                    "could not ask the microVM guest '{sandbox}' whether service '{name}' is \
                     still running — refusing to drop its record, because that would orphan a \
                     live service. Retry once the runtime responds."
                ))
            })?;
            if alive {
                h5i_sandbox::microvm::service_signal(sandbox, rec.pid, "TERM");
                // The guest and its boot were just verified and cannot change
                // under us here, so the wait polls the pid alone. Re-running
                // the full check would make ninety runtime round trips out of
                // a thirty-iteration grace period.
                let rt = h5i_sandbox::microvm::runtime();
                let running = |rt: &_| h5i_sandbox::microvm::service_pid_running(rt, sandbox, rec.pid);
                if let Some(rt) = rt {
                    for _ in 0..30 {
                        if !running(&rt) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    if running(&rt) {
                        h5i_sandbox::microvm::service_signal(sandbox, rec.pid, "KILL");
                    }
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
    if want_logs
        && log_path.is_file()
        && let Ok(raw) = std::fs::read(&log_path)
        && !raw.is_empty()
    {
        let work = m.work_dir(h5i_root);
        if let Ok(wt_repo) = open_env_worktree(h5i_root, m) {
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
        if let Some(name) = p.file_stem().and_then(|s| s.to_str())
            && let Some(record) = read_service_record(&svc_dir, name)
        {
            let alive = service_alive(&record);
            out.push(ServiceStatus { record, alive });
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
    let text = read_tail(Path::new(&rec.log), SERVICE_LOG_TAIL_BYTES);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail);
    // Sanitised, like every other box-written string that reaches a terminal.
    // A service is `sh -c '<command>'` writing to this file, so the bytes are
    // the box's — and `h5i box service logs` prints the result straight to
    // stdout, which is where an escape sequence executes. `sanitize_block`
    // rather than `sanitize_display`: a log is meant to have lines.
    Ok(crate::redact::sanitize_block(&lines[start..].join("\n")))
}

/// Most of a service log `service logs` will read.
///
/// The file is written by a long-lived command *inside the box*, so its size is
/// the box's decision, and `read_to_string` on it was an unbounded host
/// allocation to show the last fifty lines of a dev server. Reading the tail is
/// also what the caller asked for.
const SERVICE_LOG_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The last `cap` bytes of `path`, starting at a line boundary.
///
/// An empty string for anything unreadable — this is a display path, and a
/// service whose log has been rotated out from under it is not an error worth
/// failing the command over.
fn read_tail(path: &Path, cap: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(cap);
    if from > 0 && f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.take(cap).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    // A seek into the middle of the file lands mid-line; drop the fragment so
    // the first line shown is a whole one rather than a tail of one.
    if from > 0
        && let Some(nl) = buf.iter().position(|b| *b == b'\n')
    {
        buf.drain(..=nl);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Render the fleet of services for `env service status`.
pub fn render_services(env_id: &str, rows: &[ServiceStatus]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "── services for {} ──\n",
        crate::redact::sanitize_display(env_id)
    ));
    if rows.is_empty() {
        out.push_str("  (no services running; declare [service.<name>] in .h5i/env.toml)\n");
        return out;
    }
    for s in rows {
        let live = if s.alive { "running" } else { "dead" };
        let port = s
            .record
            .dynamic_port
            .or(s.record.port)
            .map(|p| format!(" PORT={p}"))
            .unwrap_or_default();
        // The name and the command are repo-supplied policy (`[service.<name>]`
        // in `.h5i/env.toml`) on their way to a terminal, so they are cleaned
        // like every other such string. The command is additionally
        // secret-scrubbed where it is *recorded*; this is the display side.
        out.push_str(&format!(
            "  {:<16} {:<8} pid={}{}  `{}`\n",
            crate::redact::sanitize_display(&s.record.name),
            live,
            s.record.pid,
            port,
            crate::redact::sanitize_display(&s.record.command)
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
    out.push_str(&format!(
        "── injected ports for {} ──\n",
        crate::redact::sanitize_display(env_id)
    ));
    // A guest service has no *injected* host port — its box owns a whole
    // network stack, so it binds the port it declared and nothing was allocated
    // to keep it from colliding. Filtering on `dynamic_port` alone therefore
    // hid the port for exactly the tier that just gained services.
    let with_ports: Vec<&ServiceStatus> = rows
        .iter()
        .filter(|s| service_port(&s.record).is_some())
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
        let port = service_port(&s.record).unwrap_or_default();
        // In a guest the port is bound inside the box's own network stack, so
        // it is not a host URL. Saying so beats printing one that cannot work.
        let url = match &s.record.runtime {
            ServiceRuntime::Guest { .. } => "in the box's network (see `box share`)".to_string(),
            ServiceRuntime::Host => format!("http://127.0.0.1:{port}"),
        };
        out.push_str(&format!(
            "  {:<16} {:<10} {:<10} {}\n",
            crate::redact::sanitize_display(&s.record.name),
            s.record
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".into()),
            port,
            url
        ));
    }
    out
}

/// Plain-text rendering of a [`DoctorReport`] (the CLI adds color).
pub fn render_doctor(r: &DoctorReport) -> String {
    let mut out = String::new();
    // `isolation_claim` comes off the manifest, and a manifest can arrive from
    // a peer through `refs/h5i/env` — `validate_imported_manifest` pins the
    // identity fields and the object ids, not this one. `detail` quotes paths
    // and errors.
    use crate::redact::sanitize_display as clean;
    out.push_str(&format!("── env doctor: {} ──\n", clean(&r.env_id)));
    out.push_str(&format!("  isolation claim : {}\n", clean(&r.isolation_claim)));
    for c in &r.checks {
        let mark = if c.ok {
            "✓"
        } else if c.warn {
            "⚠"
        } else {
            "✗"
        };
        out.push_str(&format!(
            "  {mark} {:<15} {}\n",
            clean(&c.name),
            clean(&c.detail)
        ));
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
    let spool = env_capture_spool_dir(h5i_root, m);
    let Ok(rd) = std::fs::read_dir(&spool) else {
        return p;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(base) = name.strip_suffix(".json") {
            if base.starts_with("cap-") {
                // Through the same capped, symlink-refusing reader the ingest
                // path uses. A plain `fs::read` here followed a symlink to
                // /dev/zero and had no size cap, so a box could hang or OOM
                // `h5i box status` — which the console polls.
                let cmd = read_spool_capped(&e.path(), SPOOL_MAX_CMD_BYTES)
                    .and_then(|b| serde_json::from_slice::<InboxCaptureMeta>(&b).ok())
                    .map(|meta| meta.cmd)
                    .unwrap_or_default();
                // Secret-scrubbed *and* display-sanitised. The box writes this
                // file, `status_report` prints the string to a terminal, and
                // flattening the two line breaks was not the same thing as
                // dropping the escape that moves the cursor over the lines
                // above it — `h5i box status` is where a reviewer reads what a
                // live box has staged, so it is precisely the screen worth
                // rewriting.
                let safe: String = crate::redact::sanitize_display(
                    &crate::secrets::redact_text(&cmd),
                )
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
        let latest = m
            .captures
            .last()
            .and_then(|cap| crate::receipt::find(&env_dir(h5i_root, &m.agent, &m.slug), cap).ok());
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
            last_egress_denied: latest
                .as_ref()
                .and_then(|r| r.egress.as_ref())
                .map(|e| e.denied),
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
        out.push_str(&format!(
            "  common base: {}\n",
            crate::redact::sanitize_display(short(b, 12))
        ));
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
        // `last_cmd` goes through `truncate_cmd`, which sanitises. Its
        // neighbours come off the same manifest and did not.
        out.push_str(&format!(
            "  {:<26} {:<9} {:>7} {:>7} {:>7}  {}\n",
            crate::redact::sanitize_display(&r.id),
            crate::redact::sanitize_display(&r.status),
            r.files_changed,
            r.insertions,
            r.deletions,
            run
        ));
    }
    out.push_str("\nPick a winner with `h5i box diff <name>` / `h5i box inspect <name> --capture <id>`, then `h5i box apply <name>`.\n");
    out
}

/// Single-line, length-capped rendering of a command for a table cell.
fn truncate_cmd(cmd: &str, max: usize) -> String {
    let flat: String = crate::redact::sanitize_display(cmd)
        .chars()
        .take(max)
        .collect();
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
    rels.iter().any(|r| {
        p == r.as_str()
            || p.strip_prefix(r.as_str())
                .is_some_and(|t| t.starts_with('/'))
    })
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
        return Err(if is_remote(m) {
            // Truer than "no local workspace": the box exists and is healthy,
            // it is simply on a machine this milestone cannot run commands on
            // yet. A message about a missing directory would send someone
            // looking for a bug that is not there.
            remote_unsupported_err(m, "propose/rebase")
        } else {
            no_workspace_err(m, "propose/rebase")
        });
    }
    let wt_repo = open_env_worktree(h5i_root, m)?;
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
        index.update_all(["*"].iter(), Some(&mut cb as &mut git2::IndexMatchedPath))?;
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
        violations
            .iter()
            .map(|v| crate::redact::sanitize_display(v))
            .collect::<Vec<_>>()
            .join("\n  - ")
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
        if entry.filemode() == 0o160000
            && let Some(name) = entry.name()
        {
            out.insert(format!("{dir}{name}"), entry.id());
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
/// Freeze a runner box's work into a host-authored commit on its branch.
///
/// The remote counterpart of [`propose`], and the shape of R9 in one function:
///
/// 1. The runner commits what the box has and hands back a thin bundle.
/// 2. It is unpacked into a **throwaway repository with its own object
///    database** and inspected there — a ref namespace withholds reachability,
///    not presence, so it is not a quarantine.
/// 3. Only the surviving tree crosses, and **this side writes the commit**.
///    The runner's history and authorship are discarded by construction: the
///    host repository only ever contains commits the host itself authored.
///
/// After it, everything downstream is the local path unchanged — [`diff`]
/// already reads the branch through the object store when there is no
/// worktree, and every gate in [`apply`] is object-store work.
pub fn propose_remote(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    runner: &dyn crate::placement::RemoteRunner,
) -> Result<String, H5iError> {
    // Unix only, like every other writer's hold in this file.
    #[cfg(unix)]
    let _lock = RunLock::acquire(&m.dir(h5i_root))?;
    if !matches!(
        m.status.as_str(),
        ST_CREATED | ST_RUNNING | ST_IDLE | ST_PROPOSED
    ) {
        return Err(H5iError::Metadata(format!(
            "{}: cannot propose from status '{}'",
            m.id, m.status
        )));
    }

    let scratch = tempfile::tempdir()
        .map_err(|e| H5iError::Metadata(format!("could not stage the export: {e}")))?;
    let bundle = scratch.path().join("export.bundle");
    let box_id = crate::placement::remote_box_id(&m.id);
    let described = runner.export(&box_id, &bundle)?;

    let private = private_path_rels(h5i_root, m);
    let accepted = crate::quarantine::import_tree(
        repo,
        &bundle,
        &m.base_commit,
        &described.tip_tree,
        &private,
    )?;

    let (tree_oid, private_dropped) = match accepted {
        crate::quarantine::Inspected::Refused { violations } => {
            // Recorded the way a local mediated commit records one, so a
            // refusal on a runner reads like a refusal anywhere else.
            return Err(record_commit_violation(repo, m, violations));
        }
        crate::quarantine::Inspected::Accepted {
            tree,
            private_dropped,
        } => (tree, private_dropped),
    };

    // The commit this side authors, over a tree a scan reached.
    let parent = repo.find_reference(&m.branch)?.peel_to_commit()?;
    let snapshot = if parent.tree_id() == tree_oid {
        None
    } else {
        let tree = repo.find_tree(tree_oid)?;
        let sig = crate::refstore::signature(repo)?;
        Some(repo.commit(
            Some(&m.branch),
            &sig,
            &sig,
            &format!("h5i env: mediated commit ({})", m.id),
            &tree,
            &[&parent],
        )?)
    };

    let stat = diff(repo, h5i_root, m, true).unwrap_or_default();
    let detail = match &snapshot {
        Some(oid) => format!("snapshot={oid} runner={}", m.runner.as_deref().unwrap_or("?")),
        None => "no new changes (the box's tree matches the branch tip)".to_string(),
    };
    set_status(repo, h5i_root, m, ST_PROPOSED, "proposed", Some(detail), None)?;

    let mut brief = String::new();
    brief.push_str(&format!("{}: proposed\n", m.id));
    brief.push_str(&format!(
        "  runner   {}\n",
        m.runner.as_deref().unwrap_or("?")
    ));
    brief.push_str(&format!("  base     {}\n", short(&m.base_commit, 12)));
    brief.push_str(&format!("  branch   {}\n", m.branch));
    if !private_dropped.is_empty() {
        brief.push_str(&format!(
            "  private  {} path(s) held back by policy\n",
            private_dropped.len()
        ));
    }
    if !described.has_changes {
        brief.push_str("  note     the box's tree is identical to its base\n");
    }
    if !stat.trim().is_empty() {
        brief.push_str(&stat);
    }
    Ok(brief)
}

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
        short(&m.base_commit, 12),
        m.parent_branch
    ));
    brief.push_str(&format!("  branch  : {}\n", m.branch));
    brief.push_str(&format!(
        "  policy  : profile={} isolation={} digest={}\n",
        m.profile,
        m.isolation_claim,
        short(&m.policy_digest, 12)
    ));
    if let Some(a) = &m.fs_authority {
        let mark = |b: bool| if b { "ok" } else { "FAIL" };
        let sym = match a.symlink_clean {
            Some(true) => "ok",
            Some(false) => "FAIL",
            None => "n/a",
        };
        brief.push_str(&format!(
            "  authorit: fs-subset={} writes-confined={} cache-ro={} symlink-clean={}\n",
            mark(a.fs_subset),
            mark(a.writes_confined),
            mark(a.cache_readonly),
            sym
        ));
    }
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

/// Refuse a lifecycle operation on a box somebody is connected to.
///
/// `rm` learned this first, and the other verbs did not: `abort`, `apply` and
/// `rebase` all took `run.lock` and none of them took any notice of a share, so
/// with no writer session holding the lock they ran straight through. `abort`
/// printed success and `box ls` said `aborted` while a public tunnel URL and a
/// valid ticket kept pointing at the box. `rebase` is the sharpest of the
/// three: it force-checks-out the worktree, which changes the files under the
/// dev server a visitor is looking at.
///
/// There is no `--force` on these the way there is on `rm`, and none is added
/// here: `h5i box share stop <name>` is a documented verb that always works,
/// and `--force` on it clears even a wedged record — so the way out is never
/// more than one command, and it is the command that tells the other person
/// their access has ended rather than pulling it out from under them.
/// Take the share gate and refuse if this box is being shared.
///
/// The two together, because separately they are a check and then a gap. See
/// [`crate::share_record::ShareGate`]. The returned guard must be held for the
/// whole operation: dropping it early puts the gap back.
fn hold_gate_unless_shared(
    h5i_root: &Path,
    m: &EnvManifest,
    verb: &str,
) -> Result<crate::share_record::ShareGate, H5iError> {
    let gate = crate::share_record::share_gate(&m.dir(h5i_root))?;
    refuse_if_shared(h5i_root, m, verb)?;
    Ok(gate)
}

fn refuse_if_shared(h5i_root: &Path, m: &EnvManifest, verb: &str) -> Result<(), H5iError> {
    let Some(s) = crate::share_record::read_live(&m.dir(h5i_root)) else {
        return Ok(());
    };
    // **Any live share process**, not only one that could admit somebody new.
    //
    // `is_admitting` answers "could a fresh connection get in", and that was
    // the wrong question by exactly the width of a drain. A connection already
    // authorized stays up until its per-connection revocation poll runs — up
    // to a second — and teardown happens after that; and the serving process
    // does not even notice its writer session has gone until `BOX_POLL`, three
    // seconds later. So with the writer just exited (`run.lock` free) and the
    // last grant just expired, `abort`/`rebase`/`apply` walked through this
    // guard and changed the box while a visitor was still uploading. That is
    // the outcome the guard exists to prevent, arriving through it.
    //
    // A live share process is therefore exclusionary until it has cleared its
    // record, which it does only after its transport is down and `quiesce` has
    // returned. The cost is a teardown the user might not have needed; the
    // remedy is one documented command, named below.
    if s.winding_up {
        return Err(H5iError::Metadata(format!(
            "{} is being shared by pid {} and that share is already shutting down — it will be \
             gone in a moment. Run `h5i box {verb} {}` again then.",
            m.id, s.pid, m.slug
        )));
    }
    if s.starting {
        return Err(H5iError::Metadata(format!(
            "{} is about to be shared: pid {} has claimed it and is setting up its transport. \
             `{verb}` now would change the box out from under whoever is sent the invite. \
             Wait for it, or stop it: `h5i box share stop {}`.",
            m.id, s.pid, m.slug
        )));
    }
    // Naming `--force` as well, because the two verbs read the file with
    // different rules: this one goes through `share_record`, which requires
    // every field it knows about, and `share stop` goes through `h5i-share`.
    // A record one accepts and the other does not leaves two refusals pointing
    // at each other — the verb says stop the share, and `share stop` says the
    // box is not being shared — and `--force` is the way out of that.
    //
    // Which way `share_record` fails is worth being exact about, because an
    // earlier version of this comment had it backwards: a file it cannot fully
    // parse reads as *no share at all*, so this returns `Ok` and the verb goes
    // ahead. That is deliberate (see the module docs: a malformed file must not
    // wedge `box rm` forever) and it is not the cautious direction. It is
    // defensible only because a record the share itself cannot read admits
    // nobody either — the bridge denies every ticket against it — so the box is
    // not gaining visitors while this decision is made. Connections already
    // open are the gap, and they are the reason this is written down.
    Err(H5iError::Metadata(format!(
        "{} is being shared right now by pid {} — somebody outside may be connected to it, and \
         `{verb}` would change the box under them. Stop the share first: \
         `h5i box share stop {}` (or `--force` on that, if nothing really is).",
        m.id, s.pid, m.slug
    )))
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
    // Above the lock. Below it this was unreachable: a live share implies a
    // live writer session, so `run.lock` is held and "environment is busy"
    // fired first — the same mistake `rm`'s first version made, one function
    // away from the comment explaining it.
    //
    // Above the *status* guards too, and deliberately left there: moving it
    // below them would put it behind `run.lock` again for `apply`, whose own
    // ordering ("busy" wins over "wrong status") is pinned by a test and is
    // not this change's to alter.
    // Held for the whole operation, not checked and let go: see
    // `hold_gate_unless_shared`. A share cannot claim this box while this
    // guard is alive, so "no share when we looked" stays true while we act.
    let _share_gate = hold_gate_unless_shared(h5i_root, m, "apply")?;
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
    // Held for the whole operation, not checked and let go: see
    // `hold_gate_unless_shared`. A share cannot claim this box while this
    // guard is alive, so "no share when we looked" stays true while we act.
    let _share_gate = hold_gate_unless_shared(h5i_root, m, "rebase")?;
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

    let wt_repo = open_env_worktree(h5i_root, m)?;
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
    // Held for the whole operation, not checked and let go: see
    // `hold_gate_unless_shared`. A share cannot claim this box while this
    // guard is alive, so "no share when we looked" stays true while we act.
    let _share_gate = hold_gate_unless_shared(h5i_root, m, "abort")?;
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
    } else {
        // `find_worktree` failing means the registration is there but no longer
        // readable as a worktree — and leaving it is not the harmless no-op the
        // early `if let Ok` reads as: libgit2 turns one such directory into
        // "every branch is already checked out", which breaks `box create` for
        // the whole repository (see `sweep_invalid_worktree_registrations`).
        // Reclaiming the workspace has to reclaim that too.
        let reg = repo.commondir().join("worktrees").join(m.worktree_name());
        if reg.is_dir() && !WORKTREE_REG_FILES.iter().all(|f| reg.join(f).is_file()) {
            std::fs::remove_dir_all(&reg).map_err(|e| H5iError::with_path(e, &reg))?;
        }
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
        // A runner box has no work dir here and is not therefore reclaimable
        // *locally*; skipping it silently meant `box gc` never mentioned the
        // one kind of box that is still consuming something. The remote half is
        // `h5i runner gc`, and saying so beats saying nothing.
        if is_remote(&m) {
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
        //
        // Both locks, in the documented order (run.lock then observers.lock) —
        // see the invariant at the top of this module. `gc` took only the
        // teardown lock, so it could prune and `remove_dir_all` an env while a
        // writer held `run.lock` for it. The window was narrow (only applied or
        // aborted envs are swept) but that was incidental, not designed.
        #[cfg(unix)]
        let _run_lock = match RunLock::acquire(&m.dir(h5i_root)) {
            Ok(g) => g,
            Err(_) => continue,
        };
        #[cfg(unix)]
        let _teardown = match RunLock::acquire_teardown(&m.dir(h5i_root)) {
            Ok(g) => g,
            Err(_) => continue,
        };
        // Skipped rather than refused, because this is a bulk sweep: one box
        // being shared is no reason to stop reclaiming the others. Same shape
        // as the observer case above. A share of an already-applied or aborted
        // box is unusual, and pruning its worktree out from under a visitor is
        // not something to do quietly.
        if crate::share_record::read_live(&m.dir(h5i_root)).is_some() {
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
/// delete its code branch (`refs/heads/h5i/env/…`), and erase its on-disk dir
/// (manifest, policy, status).
///
/// There is no reasoning branch to delete. `refs/h5i/context/*` was a pre-pivot
/// namespace; nothing in the workspace reads or writes it any more, and the
/// structural grant that still handed the box rw on it (letting a box create
/// arbitrary refs in the host repo, pinning objects against gc, for a feature
/// that no longer exists) is gone with this change. Unlike `gc` (workspace only) and `abort` (status only), this
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
    // Checked *before* the status guard below. A box being shared is almost
    // always also `running`, so behind that guard this message was
    // unreachable — the operator was told to abort the box and never that
    // somebody outside was connected to it, which is the more surprising fact
    // and the one that changes what they do next.
    //
    // A share is not a session, so neither that guard nor the two locks
    // further down see it. `--force` still removes the box, because that is
    // what `--force` is for, but it says what it is doing.
    //
    // Held across the removal, and that is not tidiness. A share's claim
    // happens after its transport setup, so a `box share` can be forty-five
    // seconds into waiting for a tunnel URL with nothing on disk to see. `rm`
    // then found no record, deleted the environment directory — the share's
    // own lock file with it — and the delayed claim, arriving afterwards,
    // called `create_dir_all` on the parent it had just erased and wrote
    // `share.json` into a recreated tree. The share announced a public
    // endpoint for a box that no longer existed, shut down three seconds later
    // on its next writer poll, and left a directory with a receipt in it and
    // no manifest, which `box ls`, `share ls` and `gc` all answer "no
    // environment named that" for and only `rm -rf` can clear.
    let _share_gate = crate::share_record::share_gate(&m.dir(h5i_root))?;
    let shared = crate::share_record::read_live(&m.dir(h5i_root));
    if let Some(s) = &shared
        && !force
    {
        return Err(H5iError::Metadata(format!(
            "{} is being shared right now by pid {} — somebody outside may be connected \
             to it. Stop the share first (`h5i box share stop {}`), or pass --force to \
             remove the box anyway.",
            m.id, s.pid, m.slug
        )));
    }

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

    // Said here, not beside the check above. Up there it printed and *then*
    // `rm` failed on a busy lock, so the operator was told a share would end
    // itself in a few seconds while nothing had been removed and the share was
    // still serving — and a shared box normally does have a live session, so
    // that was the ordinary case rather than a corner.
    if let Some(s) = &shared {
        eprintln!(
            "box rm: {} was being shared by pid {}; that share will notice within a few \
             seconds and end itself.",
            m.id, s.pid
        );
    }

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
    // Same rationale one tier down: a `microvm` box keeps a warm guest alive
    // between commands, so removing the box has to remove the VM or it outlives
    // everything that knows about it — holding its memory and its disk until
    // some later run's sweep happens to notice the workspace is gone. Done here
    // rather than left to that sweep because *now* is when we still know which
    // box the guest belonged to. Best-effort and keyed on this box's own
    // workspace path, so it can only ever match this box's guest.
    h5i_sandbox::microvm::remove_guest_for_workspace(&m.work_dir(h5i_root));
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

    // ─── placement (ROADMAP.md R7) ───────────────────────────────────────────

    /// A repository with one commit, for the remote-create tests.
    fn placement_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        {
            let mut cfg = repo.config().expect("config");
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "t@example.com").unwrap();
        }
        std::fs::write(dir.path().join("a.txt"), b"one").unwrap();
        {
            // Scoped so the tree and signature are dropped before `repo` moves.
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("a.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "one", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn a_runner_box_records_the_machine_and_grows_no_worktree() {
        // The remote path is the local one up to where a worktree would appear:
        // the base is pinned, the branch exists, the policy is resolved and
        // digested — and then the box is somewhere else.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");

        let m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "remote-demo",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create on a runner");

        assert_eq!(m.runner.as_deref(), Some("pi5"));
        assert_eq!(
            m.runner_id.as_deref(),
            Some(fake.runner_id.as_str()),
            "the manifest pins the machine, not the label"
        );
        assert_eq!(m.backend, "runner", "its workspace is not a worktree here");
        assert!(is_remote(&m));

        // The branch exists on this side — the base is pinned here even though
        // the execution is not.
        assert!(repo.find_reference(&m.branch).is_ok());
        assert!(!m.base_commit.is_empty());

        // And no worktree was made.
        assert!(
            !m.work_dir(&h5i_root).exists(),
            "a runner box has no worktree on this machine"
        );
        let asked = fake.created.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].0.starts_with("env-human-remote-demo-"));
    }

    #[test]
    fn a_local_box_still_records_no_runner() {
        // The field is absent rather than empty for every box that was already
        // possible, so existing manifests and existing JSON are unchanged.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let m = create(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "local-demo",
            CreateOpts::default(),
        )
        .expect("create");

        assert!(m.runner_id.is_none());
        assert!(m.runner.is_none());
        assert!(!is_remote(&m));
        assert_eq!(m.backend, "worktree");

        let json = serde_json::to_value(&m).unwrap();
        assert!(
            json.get("runner_id").is_none(),
            "an absent placement must not appear in the JSON contract"
        );
    }

    #[test]
    fn a_runner_that_enforced_another_policy_is_refused_and_records_nothing() {
        // The check that turns "the runner silently enforced an older policy"
        // into a detected fault (R7 step 4).
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let mut fake = crate::placement::fake::FakeRunner::new("pi5");
        fake.lie_with_digest = Some("f".repeat(64));

        let err = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "liar",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect_err("a different policy must not be accepted");
        assert!(
            format!("{err}").contains("different policy"),
            "the refusal says why: {err}"
        );

        // And the box is not recorded, so nothing points at a machine holding
        // something we refused.
        assert!(find(&h5i_root, "liar").is_err());
    }

    #[test]
    fn a_runner_that_fails_leaves_no_branch_or_manifest_behind() {
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let mut fake = crate::placement::fake::FakeRunner::new("pi5");
        fake.fail_with = Some("the runner is on fire".into());

        let err = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "doomed",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect_err("the create failed");
        assert!(format!("{err}").contains("on fire"));

        assert!(find(&h5i_root, "doomed").is_err(), "no manifest");
        assert!(
            repo.find_branch("h5i/human/doomed", git2::BranchType::Local)
                .is_err(),
            "and the rollback took the branch with it"
        );
    }

    #[test]
    fn a_profile_needing_credentials_cannot_be_placed_on_a_runner() {
        // R12's refusal, which was written down and not implemented. Secret
        // values never cross — a grant carries a name and a source descriptor —
        // but the runner would resolve those descriptors against *its* own
        // environment, handing the box the runner's credential or none at all
        // while its policy says otherwise. Both are the silent weakening R1
        // forbids.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");

        // A repo policy that asks for a secret.
        let policy_dir = dir.path().join(".h5i");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(
            policy_dir.join("env.toml"),
            "[profile.needy]\nisolation = \"workspace\"\nsecrets = [\"API_KEY\"]\n",
        )
        .unwrap();

        let err = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "needy-box",
            CreateOpts {
                runner: Some("pi5".into()),
                profile: Some("needy".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect_err("a profile needing credentials must not be placed on a runner");
        let text = format!("{err}");
        assert!(text.contains("secrets"), "names what it needs: {text}");
        assert!(
            text.contains("will not send credentials to another machine"),
            "and why: {text}"
        );
        assert!(
            fake.created.lock().unwrap().is_empty(),
            "and the runner was never asked"
        );
    }

    #[test]
    fn a_detached_source_cannot_be_placed_on_a_runner_yet() {
        // Refused up front rather than part-way through: a clone or a new box
        // builds its repository inside the box, and sending one across is the
        // export milestone's problem.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");

        let err = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "detached",
            CreateOpts {
                runner: Some("pi5".into()),
                source: BoxSource::New,
                ..Default::default()
            },
            Some(&fake),
        )
        .expect_err("detached sources are not placeable yet");
        assert!(format!("{err}").contains("later milestone"), "{err}");
        assert!(
            fake.created.lock().unwrap().is_empty(),
            "and the runner was never asked"
        );
    }

    #[test]
    fn operations_that_need_a_workspace_say_the_box_is_elsewhere() {
        // A message about a missing directory would send someone looking for a
        // bug that is not there.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "elsewhere",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");

        let err = remote_unsupported_err(&m, "box shell");
        let text = format!("{err}");
        assert!(text.contains("pi5"), "names the machine: {text}");
        assert!(text.contains("next milestone"), "and what is missing");
        assert!(!text.contains("no local workspace"), "not the other message");
    }

    #[test]
    fn a_pulled_manifest_naming_an_impossible_runner_is_refused() {
        // A manifest can arrive from another clone through `refs/h5i/env`, so
        // every field of it is peer data. The runner name is resolved against
        // this machine's paired runners *and* lands in a receipt, so it is
        // pinned to the shape a paired name can have rather than trusted to be
        // checked wherever it is next used.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let mut m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "checked",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");
        assert!(validate_imported_manifest(&m).is_ok());

        for bad in [
            "../../etc",
            "a/b",
            ".hidden",
            "-lead",
            "name\u{1b}[2Jforged",
            "",
            &"x".repeat(65),
        ] {
            m.runner = Some(bad.to_string());
            assert!(
                validate_imported_manifest(&m).is_err(),
                "`{bad}` must not survive as a runner name"
            );
        }
    }

    #[test]
    fn a_remote_run_is_filed_under_the_runner_observed_lane() {
        // The evidence is the same shape as a local run's; what differs is the
        // lane, which is the whole of R10.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let mut m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "runs",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");

        let out = run_remote(
            &repo,
            &h5i_root,
            &mut m,
            &["echo".to_string(), "hi".to_string()],
            &fake,
        )
        .expect("run on the runner");

        assert_eq!(out.exit_code, Some(0));
        assert_eq!(
            out.receipt.source,
            crate::placement::RUNNER_OBSERVED_LANE,
            "not host-observed, and not box-claimed"
        );
        assert_eq!(fake.execed.lock().unwrap().len(), 1);

        // The three fields with host-local provenance are absent rather than
        // computed against the wrong machine.
        assert!(out.receipt.effective_digest.is_none());
        assert!(out.receipt.fs_overlap.is_empty());
        assert!(
            out.receipt.cwd.as_deref().is_some_and(|c| c.contains("pi5")),
            "the path is named as the runner's: {:?}",
            out.receipt.cwd
        );

        // And it is on the box's own receipt log, like any other run.
        let listed = crate::receipt::list(&env_dir(&h5i_root, &m.agent, &m.slug)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, out.capture_id);
    }

    #[test]
    fn a_terminal_status_cannot_be_reset_by_running_on_the_runner() {
        // The box lives on the runner until its lease or a `box rm`, so without
        // a gate an applied box could be run again — rewriting a terminal
        // status back to `idle` in the manifest and in `refs/h5i/env`, where it
        // travels to other clones as an ordinary run.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let mut m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "gated",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");

        // Before a terminal state it runs.
        assert!(run_remote(&repo, &h5i_root, &mut m, &["true".to_string()], &fake).is_ok());

        m.status = ST_APPLIED.to_string();
        let err = match run_remote(&repo, &h5i_root, &mut m, &["true".to_string()], &fake) {
            Err(e) => e,
            Ok(_) => panic!("an applied box must not be runnable"),
        };
        assert!(format!("{err}").contains("only valid before"), "{err}");
        assert_eq!(m.status, ST_APPLIED, "and the status is untouched");
    }

    #[test]
    fn a_remote_box_that_has_not_proposed_refuses_to_diff_rather_than_answering_empty() {
        // The branch exists from creation and points at the base, so diffing
        // base against base renders an empty patch and exits zero — "this box
        // changed nothing" for a box that may have rewritten its whole tree.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "undiffed",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");

        let err = diff(&repo, &h5i_root, &m, false)
            .expect_err("an empty answer that looks like a fact is worse than a refusal");
        let text = format!("{err}");
        assert!(text.contains("has not been brought home"), "{text}");
        assert!(text.contains("box propose"), "and says what to do: {text}");
    }

    // The counter under test lives in the console module, which is behind the
    // `web` feature; the lane string it keys on is not. Gated rather than
    // duplicated, because asserting the string here and hoping the console
    // agreed is exactly the drift this test exists to catch.
    #[cfg(feature = "web")]
    #[test]
    fn a_runner_observed_run_is_not_counted_as_box_claimed() {
        // The console badge would otherwise say "the box told us this" about a
        // run the box could not have forged.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let mut m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "signals",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");
        run_remote(&repo, &h5i_root, &mut m, &["true".to_string()], &fake).expect("run");

        let receipts = crate::receipt::list(&env_dir(&h5i_root, &m.agent, &m.slug)).unwrap();
        let signals = crate::server::signals_for_test(&m, &receipts);
        assert_eq!(signals.runner_observed, 1);
        assert_eq!(signals.box_claimed, 0, "it is not the box's own account");
        assert_eq!(signals.host_observed, 0, "and this machine did not watch it");
        assert!(
            !signals.box_claimed_only,
            "a run seen from outside the box must not read as box-claimed only"
        );
    }

    #[test]
    fn a_manifest_with_a_runner_id_that_is_not_an_id_is_refused_on_import() {
        // It decides which machine every later operation talks to, so it is
        // guarded beside base_commit rather than left to a renderer.
        let (dir, repo) = placement_repo();
        let h5i_root = dir.path().join(".h5i");
        let fake = crate::placement::fake::FakeRunner::new("pi5");
        let mut m = create_with_remote(
            &repo,
            &h5i_root,
            dir.path(),
            "human",
            "checked",
            CreateOpts {
                runner: Some("pi5".into()),
                ..Default::default()
            },
            Some(&fake),
        )
        .expect("create");

        assert!(validate_imported_manifest(&m).is_ok());
        m.runner_id = Some("not-an-object-id".into());
        let err = validate_imported_manifest(&m).expect_err("refused");
        assert!(format!("{err}").contains("runner_id"), "{err}");

        // Absent is fine: every local box has none.
        m.runner_id = None;
        assert!(validate_imported_manifest(&m).is_ok());
    }
    use super::*;

    /// A pid is only meaningful inside the namespace that issued it, so the
    /// record has to say which one — and has to keep parsing records written
    /// before it did.
    #[test]
    fn a_service_record_says_whose_pid_it_holds() {
        // A record from before the microvm tier had services: no `runtime` key
        // at all. It must read as a host process, not fail to parse and not
        // silently become a guest pid somebody might signal.
        let legacy = r#"{"name":"web","pid":4242,"command":"npm run dev",
            "started_at":"2026-08-14T00:00:00Z","port":3000,"dynamic_port":51234,
            "log":"/tmp/web.log"}"#;
        let rec: ServiceRecord = serde_json::from_str(legacy).expect("legacy record parses");
        assert_eq!(rec.runtime, ServiceRuntime::Host);

        // A guest record round-trips with the guest's identity *and* its boot,
        // which is what keeps the pid meaningful across a restart.
        let guest = ServiceRecord {
            runtime: ServiceRuntime::Guest {
                sandbox: "h5i-human-web-abc123".into(),
                boot: "7488c2c3-0000-0000-0000-000000000000".into(),
            },
            ..rec.clone()
        };
        let text = serde_json::to_string(&guest).unwrap();
        let back: ServiceRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back.runtime, guest.runtime);

        // A guest record written before the boot id existed still parses, and
        // its empty boot can never equal a real one — so it reads as dead
        // rather than as a pid somebody may signal.
        let no_boot = r#"{"name":"web","pid":42,"command":"x","started_at":"t",
            "port":null,"dynamic_port":null,"log":"/tmp/x",
            "runtime":{"kind":"guest","sandbox":"g"}}"#;
        let rec: ServiceRecord = serde_json::from_str(no_boot).expect("parses");
        match rec.runtime {
            ServiceRuntime::Guest { boot, .. } => assert!(boot.is_empty()),
            ServiceRuntime::Host => panic!("must stay a guest record"),
        }
    }

    /// Every supported platform must be able to prove whose pid a record is.
    ///
    /// This is not a property of the identity check, it is a property of the
    /// *platform support*, and it is asserted here because answering `None` is
    /// silently catastrophic rather than merely unhelpful: `h5i box share` asks
    /// `session_pid_verified` for the strict answer, that call skips any record
    /// whose `started_ticks` is absent, and a platform where this function
    /// cannot answer therefore has no shareable box at all. macOS shipped in
    /// exactly that state — every live record written with `started_ticks:
    /// None`, and `h5i box share` reporting "has no session running" for boxes
    /// whose session was running the whole time.
    ///
    /// A platform added later with no implementation fails here rather than in
    /// somebody's terminal.
    #[test]
    fn this_platform_can_tell_a_pid_from_its_reuse() {
        let me = std::process::id();
        let mine = proc_start_ticks(me);
        assert!(
            mine.is_some(),
            "this platform cannot prove a live record's identity, so `h5i box share` will \
             refuse every box on it"
        );
        // Stable for the life of the process: an identity that changed between
        // two reads would fail `live_identity_holds` against its own record.
        assert_eq!(mine, proc_start_ticks(me), "an identity must not drift");

        // And it distinguishes. A different process started later must not
        // share our value, or the check cannot catch pid reuse at all.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a child to compare against");
        let theirs = proc_start_ticks(child.id());
        assert!(theirs.is_some(), "a live child has a start time");
        assert_ne!(
            mine, theirs,
            "two processes shared an identity, so pid reuse would go unnoticed"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The record a running session writes must satisfy the strict reader.
    ///
    /// The two halves are written and checked in different modules, and the
    /// regression that motivated this passed every test in both: `register`
    /// wrote `None` happily, `live_identity_holds` tolerated `None` happily,
    /// and only the caller that demanded `Some` — in another crate — broke.
    #[test]
    fn a_registered_session_satisfies_the_check_that_sharing_makes() {
        let rec = LiveSession {
            pid: std::process::id(),
            kind: "run".into(),
            started_at: now_ts(),
            command: None,
            started_ticks: proc_start_ticks(std::process::id()),
        };
        assert!(
            rec.started_ticks.is_some(),
            "a session registering itself must record an identity, or sharing refuses it"
        );
        assert!(
            live_identity_holds(&rec),
            "a record written for this very process must match it"
        );
    }

    #[test]
    fn egress_rule_validation_accepts_proxy_forms_only() {
        // The three forms the proxy's AllowList understands, normalized.
        assert_eq!(
            validate_egress_rule(" API.Example.com ").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            validate_egress_rule(".example.com").unwrap(),
            ".example.com"
        );
        assert_eq!(
            validate_egress_rule("*.example.com").unwrap(),
            "*.example.com"
        );
        assert_eq!(
            validate_egress_rule("github.com:443").unwrap(),
            "github.com:443"
        );
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
            started_ticks: None,
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
        assert_eq!(
            user_allow_list_at(Some(&path)),
            vec!["pypi.org".to_string()]
        );
        // Missing file → empty, not an error.
        assert!(user_allow_list_at(Some(&dir.path().join("absent"))).is_empty());
    }

    /// The rcfile path gets the same pair of checks the persona sources get —
    /// not absolute, no `..` — and had the same blind spot: neither resolves.
    /// A repo shipping this path as a symlink puts a file from outside the
    /// worktree in front of `bash --rcfile`, which *sources* it. `is_file()`
    /// follows links and said yes.
    #[test]
    #[cfg(unix)]
    fn the_rcfile_will_not_follow_a_symlink_out_of_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join(".h5i")).unwrap();
        std::fs::write(dir.path().join("outside.sh"), "echo pwned").unwrap();

        // In-tree still resolves.
        std::fs::write(work.join(".h5i").join("box.bashrc"), "PS1=x").unwrap();
        assert!(resolve_work_rcfile(&work, ".h5i/box.bashrc").is_ok());

        // A symlinked leaf is refused rather than sourced.
        std::os::unix::fs::symlink(dir.path().join("outside.sh"), work.join(".h5i").join("evil.bashrc"))
            .unwrap();
        assert!(resolve_work_rcfile(&work, ".h5i/evil.bashrc").is_err());

        // And a symlinked ancestor.
        std::os::unix::fs::symlink(dir.path(), work.join("up")).unwrap();
        assert!(resolve_work_rcfile(&work, "up/outside.sh").is_err());
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

    /// A box shell must not answer `zsh: nice(5) failed: operation not
    /// permitted` on every `cmd &`: zsh renices background jobs by default and
    /// no box can call `setpriority(2)`, so the generated rc turns the renice
    /// off. Put to zsh itself where there is one — an option name is only right
    /// if zsh accepts it, and a typo here would be silently inert.
    #[test]
    fn the_generated_zshrc_turns_off_the_background_renice() {
        let h5i_root = tempfile::tempdir().unwrap();
        let m = canonical_manifest("claude", "demo");
        let z = write_plain_zshrc(h5i_root.path(), &m, None).unwrap();
        let rc = Path::new(&z.zdotdir).join(".zshrc");
        let body = std::fs::read_to_string(&rc).unwrap();
        assert!(
            body.contains("\nunsetopt bgnice\n"),
            "the rc must disable bgnice:\n{body}"
        );

        // `-f` so the host's own zsh startup cannot decide the answer; the rc
        // under test is sourced explicitly.
        let out = std::process::Command::new("zsh")
            .arg("-f")
            .arg("-c")
            .arg(format!(
                "source {}; [[ -o bgnice ]] && print ON || print OFF",
                sq(&rc.display().to_string())
            ))
            .output();
        match out {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("SKIP the_generated_zshrc_turns_off_the_background_renice: no zsh here");
            }
            Err(e) => panic!("run zsh: {e}"),
            Ok(out) => assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "OFF",
                "zsh still renices background jobs after sourcing the rc\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
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
            // Hex, like the git tree id `create` actually writes. It was `t`
            // repeated, which no object id can be.
            base_tree: "e".repeat(40),
            parent_branch: "main".into(),
            branch: format!("refs/heads/h5i/env/{agent}/{slug}"),
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "d".repeat(64),
            effective_digest: None,
            fs_authority: None,
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
            runner_id: None,
            runner: None,
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
        assert!(
            got.is_none(),
            "contended snapshot falls back to the branch tip"
        );
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
        std::fs::write(
            dir.path().join("quick_sort.py"),
            "def quick_sort():\n    pass\n",
        )
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
        assert!(
            inbox_pending_context_path_from(None, Some(dig.clone()), Some(spool.clone())).is_none()
        );
        assert!(
            inbox_pending_context_path_from(Some(id.clone()), None, Some(spool.clone())).is_none()
        );
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

        // The object-id fields, because every surface that shows a manifest
        // abbreviates them and an abbreviation is a slice.
        for tamper in [
            |m: &mut EnvManifest| m.base_commit = String::new(),
            |m: &mut EnvManifest| m.base_commit = "abc".into(),
            |m: &mut EnvManifest| m.base_commit = "a日日日日".into(),
            |m: &mut EnvManifest| m.base_tree = "not-hex-at-all".into(),
            |m: &mut EnvManifest| m.policy_digest = "\u{1b}[2Jzz".into(),
        ] {
            let mut m = canonical_manifest("claude", "fix");
            tamper(&mut m);
            assert!(
                validate_imported_manifest(&m).is_err(),
                "a field that is not an object id is rejected"
            );
        }
    }

    /// `git clone` runs on the host, unconfined, on a string somebody typed or
    /// pasted. `ext::` is not a URL — it is a command — and an operator whose
    /// `~/.gitconfig` says `protocol.ext.allow = always` turned a pasted
    /// "repository URL" into host command execution. A leading `-` was read as
    /// an option for the same reason `source::resolve_pr_base` passes
    /// `--end-of-options`.
    #[test]
    fn the_clone_argv_refuses_to_be_an_argument_or_a_command() {
        let argv = clone_argv(
            Path::new("/envs/a/clone-hooks-disabled"),
            "--upload-pack=touch /tmp/pwned",
            Path::new("/envs/a/work"),
        );
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        // The URL cannot be read as an option.
        let eoo = argv.iter().position(|a| a == "--end-of-options").expect("--end-of-options");
        let url = argv
            .iter()
            .position(|a| a == "--upload-pack=touch /tmp/pwned")
            .expect("the url is still passed");
        assert!(eoo < url, "the separator must come first: {argv:?}");

        // ...and cannot be a command, whatever the host's gitconfig says.
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-c" && w[1] == "protocol.ext.allow=never"),
            "{argv:?}"
        );
        // The hook lockdown that was already here stays.
        assert!(
            argv.windows(2).any(|w| {
                w[0] == "-c" && w[1].starts_with("core.hooksPath=")
            }),
            "{argv:?}"
        );
        // Both `-c` settings precede the subcommand, or git does not read them.
        let sub = argv.iter().position(|a| a == "clone").expect("clone");
        for (i, a) in argv.iter().enumerate() {
            if a == "-c" {
                assert!(i < sub, "a `-c` after the subcommand is ignored: {argv:?}");
            }
        }
        // Still a shallow clone into the box's own workspace.
        assert_eq!(argv.last().map(String::as_str), Some("/envs/a/work"));
        assert!(argv.windows(2).any(|w| w[0] == "--depth" && w[1] == "1"));
    }

    /// The exact-value scrub is the guaranteed half of the secret defence — the
    /// pattern scan is best-effort by construction. It went through
    /// `String::from_utf8_lossy` → `str::replace` → `into_bytes`, so a binary
    /// payload came back rewritten: every invalid byte became U+FFFD, and
    /// `receipt::append` digested *that*, whenever any secret was brokered.
    #[test]
    fn the_exact_scrub_removes_the_secret_and_leaves_the_bytes_alone() {
        let secrets = vec!["sk-live-abcdef".to_string()];

        // Binary in, byte-identical out.
        let binary: Vec<u8> = vec![0x00, 0xff, 0xfe, 0x80, b'o', b'k', 0xc3];
        assert_eq!(scrub_exact(&binary, &secrets), binary);

        // ...and the secret goes even when it sits next to bytes that are not
        // UTF-8 at all, which is where the lossy round trip did its damage.
        let mut payload = vec![0xffu8, 0xfe];
        payload.extend_from_slice(b"token=sk-live-abcdef\n");
        payload.push(0x80);
        let out = scrub_exact(&payload, &secrets);
        assert!(!out.windows(14).any(|w| w == b"sk-live-abcdef"), "{out:?}");
        assert_eq!(&out[..2], &[0xff, 0xfe], "the surrounding bytes are untouched");
        assert_eq!(out.last(), Some(&0x80));
        assert!(String::from_utf8_lossy(&out).contains("[redacted secret]"));

        // Every occurrence, and an empty entry is a no-op rather than an
        // infinite marker.
        let out = scrub_exact(b"a sk-live-abcdef b sk-live-abcdef", &secrets);
        assert_eq!(out, b"a [redacted secret] b [redacted secret]");
        assert_eq!(scrub_exact(b"abc", &["".to_string()]), b"abc");
        assert_eq!(scrub_exact(b"abc", &[]), b"abc");
    }

    /// `validate_profile` pins a persona source inside `$WORK` — relative, no
    /// `..` — and cannot resolve it. Both the entry and the worktree are
    /// repo-supplied, so a branch shipping `notes.md` as a symlink to a host
    /// file turned a valid-looking entry into a read of it, concatenated into
    /// `PERSONA.md` *inside the box*, which the agent is told to open.
    ///
    /// `private_paths` has the same shape and got `create_dirs_within` for
    /// exactly this reason. This is the read side of that argument.
    #[test]
    #[cfg(unix)]
    fn a_persona_source_will_not_follow_a_symlink_out_of_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("docs")).unwrap();

        let secret = dir.path().join("id_rsa");
        std::fs::write(&secret, "PRIVATE KEY MATERIAL").unwrap();

        // The ordinary case still reads.
        std::fs::write(work.join("docs").join("style.md"), "be brief").unwrap();
        assert_eq!(
            read_within_work(&work, "docs/style.md").unwrap(),
            "be brief"
        );

        // A symlinked leaf is refused.
        std::os::unix::fs::symlink(&secret, work.join("docs").join("notes.md")).unwrap();
        let err = read_within_work(&work, "docs/notes.md").unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");

        // And a symlinked *ancestor*, which the leaf check alone would miss.
        std::os::unix::fs::symlink(dir.path(), work.join("out")).unwrap();
        assert!(read_within_work(&work, "out/id_rsa").is_err());

        // The whole bake fails closed rather than baking part of a persona.
        let err = materialize_persona(&work, &["docs/notes.md".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("fail-closed"), "{err}");
        assert!(
            !work.join(PERSONA_FILE).exists(),
            "a refused source must not leave a partial PERSONA.md"
        );
    }

    /// Two readers of `services.json`, one digest check between them.
    ///
    /// `pinned_service_defs` is the one documented as being for "callers that
    /// must not read box-writable input", and it was the one *without* the
    /// check that establishes the file is the one that was pinned — a weaker
    /// guarantee than its sibling's, under a stronger claim.
    #[test]
    fn both_readers_of_the_pinned_service_manifest_check_its_digest() {
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let mut m = canonical_manifest("claude", "fix");
        let env = m.dir(h5i_root);
        std::fs::create_dir_all(&env).unwrap();

        let mut defs = std::collections::BTreeMap::new();
        defs.insert(
            "web".to_string(),
            ServiceDef {
                command: "npm run dev".into(),
                port: Some(3000),
                restart: None,
                logs: true,
            },
        );
        std::fs::write(
            env.join("services.json"),
            serde_json::to_vec_pretty(&defs).unwrap(),
        )
        .unwrap();

        // Pinned to what is there: both readers agree.
        m.service_digest = Some(service_defs_digest(&defs));
        assert_eq!(pinned_service_defs(h5i_root, &m).unwrap().len(), 1);
        assert_eq!(load_service_defs(h5i_root, &m).unwrap().len(), 1);

        // Pinned to something else: the sibling already refused, and this one
        // now answers `None` rather than handing back a manifest that does not
        // match the digest.
        m.service_digest = Some("0".repeat(64));
        assert!(
            pinned_service_defs(h5i_root, &m).is_none(),
            "a manifest that does not match its pin is not the pinned manifest"
        );
        assert!(load_service_defs(h5i_root, &m).is_err());
    }

    /// A manifest is read far more often than it is imported, and only the
    /// import validated it.
    ///
    /// `validate_imported_manifest`'s own doc says the identity fields are
    /// checked "BEFORE its `agent`/`slug` are used to compute on-disk paths" —
    /// true of `materialize_from_ref` and not of `load_manifest_at`. Everything
    /// downstream calls `m.dir(h5i_root)`, which joins those two fields
    /// unchecked, and one of the things downstream is `rm`'s `remove_dir_all`.
    #[test]
    fn a_manifest_is_validated_where_it_is_read_not_only_where_it_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let home = h5i_root.join(ENV_DIR).join("claude").join("fix");
        std::fs::create_dir_all(&home).unwrap();

        // The canonical case still reads.
        let good = canonical_manifest("claude", "fix");
        std::fs::write(
            home.join(MANIFEST_FILE),
            serde_json::to_vec(&good).unwrap(),
        )
        .unwrap();
        assert_eq!(load_manifest_at(&home).unwrap().id, "env/claude/fix");
        assert_eq!(list(h5i_root).len(), 1);

        // A traversing identity is refused rather than turned into a path.
        let mut escape = good.clone();
        escape.agent = "../../../../tmp/evil".into();
        escape.id = format!("env/{}/fix", escape.agent);
        escape.branch = format!("refs/heads/h5i/env/{}/fix", escape.agent);
        std::fs::write(
            home.join(MANIFEST_FILE),
            serde_json::to_vec(&escape).unwrap(),
        )
        .unwrap();
        assert!(load_manifest_at(&home).is_err(), "traversal must not load");
        assert!(list(h5i_root).is_empty(), "and `list` skips it");

        // So is a manifest that is canonical but describes a different env —
        // copied here, restored from a backup, or hand-edited. A manifest is
        // identified by where it lives.
        let elsewhere = canonical_manifest("codex", "other");
        std::fs::write(
            home.join(MANIFEST_FILE),
            serde_json::to_vec(&elsewhere).unwrap(),
        )
        .unwrap();
        let err = load_manifest_at(&home).unwrap_err().to_string();
        assert!(err.contains("describes a different environment"), "{err}");
    }

    /// One property over every renderer at once: nothing a box, a repo or a
    /// peer supplies reaches a terminal carrying a control character.
    ///
    /// Written as a sweep rather than one test per function because that is how
    /// this kept going wrong — `receipt::render`, then `status_report`, then
    /// `render_compare`, then the service lane, each found separately after the
    /// last was fixed. A renderer added later fails this without anybody having
    /// to remember the rule.
    #[test]
    fn no_renderer_puts_a_control_character_on_the_terminal() {
        // In every string field, including the ones that "cannot" hold it.
        const HOSTILE: &str = "x\u{1b}[2J\u{1b}[1;1Hforged\u{202e}\u{7}";
        let clean = |rendered: &str, what: &str| {
            assert!(
                !rendered.chars().any(|c| c.is_control() && c != '\n'),
                "{what} put a control character on the terminal: {rendered:?}"
            );
            assert!(!rendered.contains('\u{202e}'), "{what} kept a bidi override");
        };

        clean(
            &render_secrets(
                HOSTILE,
                &[SecretStatus {
                    name: HOSTILE.into(),
                    source: HOSTILE.into(),
                    inject: HOSTILE.into(),
                    ttl: Some(HOSTILE.into()),
                    status: HOSTILE.into(),
                    fingerprint: Some(HOSTILE.into()),
                }],
            ),
            "render_secrets",
        );

        let svc = ServiceStatus {
            record: ServiceRecord {
                name: HOSTILE.into(),
                pid: 1,
                command: HOSTILE.into(),
                started_at: HOSTILE.into(),
                port: Some(3000),
                dynamic_port: Some(3001),
                log: HOSTILE.into(),
                runtime: ServiceRuntime::Host,
            },
            alive: true,
        };
        clean(&render_services(HOSTILE, std::slice::from_ref(&svc)), "render_services");
        clean(&render_ports(HOSTILE, std::slice::from_ref(&svc)), "render_ports");

        clean(
            &render_doctor(&DoctorReport {
                env_id: HOSTILE.into(),
                isolation_claim: HOSTILE.into(),
                checks: vec![DoctorCheck {
                    name: HOSTILE.into(),
                    ok: false,
                    warn: false,
                    detail: HOSTILE.into(),
                }],
                healthy: false,
            }),
            "render_doctor",
        );

        clean(
            &render_compare(&[CompareRow {
                id: HOSTILE.into(),
                status: HOSTILE.into(),
                base_commit: HOSTILE.into(),
                files_changed: 1,
                insertions: 2,
                deletions: 3,
                last_exit: Some(0),
                last_cmd: Some(HOSTILE.into()),
                last_source: Some(HOSTILE.into()),
                last_egress_denied: Some(1),
            }]),
            "render_compare",
        );
    }

    /// A service is `sh -c '<command>'` running inside the box and appending to
    /// this file, so both its size and its bytes are the box's. `service logs`
    /// read the whole thing to show the last few lines, and printed them to the
    /// operator's terminal with the escapes still in.
    #[test]
    fn a_service_log_is_read_by_the_tail_and_cleaned_before_it_is_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web.log");

        // Far past the cap, so the read has to start part way in.
        let line = "the dev server said something\n";
        let repeats = (SERVICE_LOG_TAIL_BYTES as usize / line.len()) + 5_000;
        let mut body = line.repeat(repeats);
        body.push_str("first-visible\n");
        body.push_str("boot\u{1b}[2Jok\n");
        body.push_str("last-line\n");
        std::fs::write(&path, &body).unwrap();

        let tail = read_tail(&path, SERVICE_LOG_TAIL_BYTES);
        assert!(
            tail.len() as u64 <= SERVICE_LOG_TAIL_BYTES,
            "read {} bytes",
            tail.len()
        );
        assert!(tail.ends_with("last-line\n"), "the tail is the end of the file");
        // Whole lines only: the seek lands mid-line and the fragment is dropped.
        assert!(
            tail.lines().next().unwrap() == line.trim_end(),
            "first line was a fragment: {:?}",
            tail.lines().next()
        );

        // And what a reader is shown has no escapes but keeps its lines.
        let shown = crate::redact::sanitize_block(&tail);
        assert!(!shown.contains('\u{1b}'), "an escape reached the terminal");
        assert!(shown.contains("boot[2Jok"), "{:?}", shown.lines().rev().take(3).collect::<Vec<_>>());
        assert!(shown.lines().count() > 3);
    }

    /// `<env>/spool` is one of the two paths a box can write, and what it stages
    /// there is read back by `h5i box status` and printed to the operator's
    /// terminal. Flattening the two line breaks was not the same thing as
    /// dropping the escape that rewrites the lines above.
    #[test]
    fn a_box_cannot_stage_an_escape_sequence_into_box_status() {
        let dir = tempfile::tempdir().unwrap();
        let h5i_root = dir.path();
        let m = canonical_manifest("claude", "fix");
        let spool = env_capture_spool_dir(h5i_root, &m);
        std::fs::create_dir_all(&spool).unwrap();

        write_inbox_capture_spool(
            &spool,
            &InboxCaptureMeta {
                cmd: "ls\u{1b}[2J\u{1b}[H  status   : idle".into(),
                cwd: None,
                exit_code: Some(0),
                files: Vec::new(),
                cmd_argv: Vec::new(),
            },
            b"",
        )
        .unwrap();

        let pending = scan_spool_pending(h5i_root, &m);
        assert_eq!(pending.captures.len(), 1);
        let staged = &pending.captures[0];
        assert!(!staged.contains('\u{1b}'), "{staged:?}");
        assert!(staged.starts_with("ls"), "{staged:?}");
    }

    /// `&id[..12]` panics two ways on a manifest this machine did not write:
    /// when the field is shorter than the slice, and when byte 12 lands inside a
    /// multi-byte character. `h5i box list` abbreviates every manifest it can
    /// see, so one crafted line in a peer's `refs/h5i/env` aborted the listing —
    /// the "one poisoned line suppresses every legitimate env" outcome
    /// `materialize_from_ref` skips bad manifests specifically to avoid.
    #[test]
    fn abbreviating_an_id_never_panics_however_odd_it_is() {
        assert_eq!(short("0123456789abcdef", 12), "0123456789ab");
        assert_eq!(short("abc", 12), "abc");
        assert_eq!(short("", 12), "");
        // Byte 12 is mid-character here: 1 + 3 + 3 + 3 + 3.
        assert_eq!(short("a日日日日", 12), "a日日日日");
        assert_eq!(short("a日日日日", 3), "a日日");
        // And the renderers that call it survive a manifest carrying them.
        let mut m = canonical_manifest("claude", "fix");
        m.base_commit = "a日日日日".into();
        m.policy_digest = String::new();
        let _ = short(&m.base_commit, 12);
        let _ = short(&m.policy_digest, 12);
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
        // The env's own branch namespace is the rw entry nested under `refs`.
        // (`refs/h5i/context` used to be a second one; that grant is gone —
        // nothing reads or writes the namespace any more.)
        let nested_pos = paths
            .iter()
            .position(|p| p.rw && p.host.to_string_lossy().contains("refs/heads/h5i/env/"))
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
        ] {
            assert!(has(&rw, want), "rw grant {want} missing: {rw:?}");
        }
        // And the pre-pivot reasoning namespace is granted no longer: nothing
        // reads or writes it, and rw there let a box create arbitrary refs in
        // the host repository.
        assert!(
            !rw.iter().any(|p| p.contains("refs/h5i/context")),
            "the dead refs/h5i/context grant must be gone: {rw:?}"
        );
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
        assert!(
            !fresh.join("stale").exists(),
            "leftovers survived the reset"
        );
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
        assert!(
            victim.join("keep").exists(),
            "the symlink target was cleared"
        );
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

    /// Acquires a lock that *must* eventually become free, retrying briefly.
    ///
    /// A `flock` lives on the open file description, not the fd, and a `fork`
    /// duplicates every OFD. Tests in this binary run in parallel and several
    /// spawn `git`/`sh`/`ps`, so a child forked in the window where this test
    /// holds a lock file inherits that OFD; the lock therefore outlives the
    /// holder's `drop` until the child's `exec` closes the `O_CLOEXEC` fd. The
    /// window is microseconds, but on a loaded CI box it is wide enough to lose
    /// a race with the very next acquire. Assert the lock *becomes* free, not
    /// that it is free instantly.
    #[cfg(unix)]
    fn acquire_eventually(acquire: impl Fn() -> Result<RunLock, H5iError>) -> RunLock {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match acquire() {
                Ok(lock) => return lock,
                Err(e) if std::time::Instant::now() >= deadline => {
                    panic!("lock never became available within 10s: {e}")
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }

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
        let td = acquire_eventually(|| RunLock::acquire_teardown(env_dir));
        // While a teardown holds observers.lock exclusively, a new observer is
        // refused (the worktree is being removed).
        assert!(
            RunLock::acquire_observer(env_dir).is_err(),
            "an observer must be refused while a teardown is removing the worktree"
        );
        drop(td);
        // Once the teardown exits, an observer can attach again.
        acquire_eventually(|| RunLock::acquire_observer(env_dir));
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
        assert!(home
            .join(".claude/projects/some-proj/session.jsonl")
            .exists());
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
        assert!(
            !allowed.contains("8443"),
            "ports are not domains: {allowed}"
        );
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
        assert_eq!(
            env.get("AGENT_BROWSER_HEADED").map(String::as_str),
            Some("0")
        );
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
        if policy
            .home_binds
            .iter()
            .any(|b| b.target == Path::new("/tmp"))
        {
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
            base_tree: "e".repeat(40),
            parent_branch: "main".into(),
            branch: "refs/heads/h5i/env/claude/fix".into(),
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "d".repeat(64),
            effective_digest: None,
            fs_authority: None,
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
            runner_id: None,
            runner: None,
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
                base_tree: "e".repeat(40),
                parent_branch: "main".into(),
                branch: format!("refs/heads/h5i/env/{agent}/{slug}"),
                source: "repo".into(),
                profile: "default".into(),
                policy_digest: "d".repeat(64),
                effective_digest: None,
                fs_authority: None,
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
                runner_id: None,
                runner: None,
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

    #[test]
    fn a_lifecycle_verb_will_not_change_a_box_somebody_is_connected_to() {
        // `rm` learned to check this first and the other verbs did not: they
        // all take `run.lock` and none took any notice of a share, so with the
        // writer session gone they ran straight through. `abort` printed
        // success and `box ls` said `aborted` while a public tunnel URL and a
        // valid ticket kept pointing at the box; `rebase` force-checks-out the
        // worktree, which changes the files under the dev server a visitor is
        // looking at.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let m = canonical_manifest("human", "shared");
        let dir = m.dir(root);
        std::fs::create_dir_all(&dir).expect("mkdir");

        // No share record: every verb proceeds.
        for verb in ["abort", "apply", "rebase"] {
            assert!(
                refuse_if_shared(root, &m, verb).is_ok(),
                "{verb} refused an unshared box"
            );
        }

        let record = |winding: bool| {
            serde_json::json!({
                "v": 1,
                "box_id": m.id,
                "port": 3000,
                "transport": "tunnel",
                "endpoint": "https://x",
                "started_at": "2026-01-01T00:00:00Z",
                "pid": std::process::id(),
                "winding_up": winding,
                "grants": [{
                    "id": "a1b2c3d4",
                    "secret_sha256": "ff",
                    "revoked": false,
                    "expires_at": 4_000_000_000i64,
                }],
            })
            .to_string()
        };

        std::fs::write(dir.join("share.json"), record(false)).expect("write");
        for verb in ["abort", "apply", "rebase"] {
            let err = refuse_if_shared(root, &m, verb)
                .expect_err("a live share must stop a lifecycle verb");
            let said = format!("{err}");
            assert!(said.contains("being shared right now"), "{verb}: {said}");
            // With a command that works, and it is the one that tells the other
            // person their access has ended rather than pulling it out from
            // under them.
            assert!(said.contains("h5i box share stop shared"), "{verb}: {said}");
        }

        // A share already on its way out says so, and says to try again — it
        // will be gone in seconds, so refusing outright would be advice to
        // wait without saying so.
        std::fs::write(dir.join("share.json"), record(true)).expect("write");
        let err = refuse_if_shared(root, &m, "abort").expect_err("winding up");
        let said = format!("{err}");
        assert!(said.contains("already shutting down"), "{said}");
        assert!(said.contains("h5i box abort shared"), "{said}");

        // And a record whose process is gone is not a share.
        std::fs::write(
            dir.join("share.json"),
            r#"{"v":1,"box_id":"x","port":1,
            "transport":"p2p","endpoint":"e","started_at":"t","pid":0,"grants":[]}"#,
        )
        .expect("write");
        assert!(
            refuse_if_shared(root, &m, "abort").is_ok(),
            "a dead record blocked abort"
        );
    }

    /// A share is exclusionary for as long as its process is, not only while
    /// it could admit somebody new.
    ///
    /// `is_admitting` was the test, and it answers "could a fresh connection
    /// get in" — which is the wrong question by exactly the width of a drain.
    /// A connection already authorized stays up until its revocation poll runs,
    /// and teardown follows that; the serving process does not even notice its
    /// writer has gone until three seconds later. So with the writer just
    /// exited and the last grant just expired, a lifecycle verb walked through
    /// this guard and changed the box while a visitor was still connected.
    ///
    /// The two states this covers — every grant revoked, and every grant
    /// expired — are exactly the ones that used to read as "nothing is
    /// holding this box".
    #[test]
    fn a_share_still_draining_is_not_a_box_free_to_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let m = canonical_manifest("human", "draining");
        let dir = m.dir(root);
        std::fs::create_dir_all(&dir).expect("mkdir");

        let record = |grant: serde_json::Value, starting: bool| {
            serde_json::json!({
                "v": 1,
                "box_id": m.id,
                "port": 3000,
                "transport": "tunnel",
                "endpoint": "https://x",
                "started_at": "2026-01-01T00:00:00Z",
                "pid": std::process::id(),
                "starting": starting,
                "grants": if grant.is_null() { serde_json::json!([]) } else { serde_json::json!([grant]) },
            })
            .to_string()
        };
        let revoked = serde_json::json!({
            "id": "a1b2c3d4", "secret_sha256": "ff",
            "revoked": true, "expires_at": 4_000_000_000i64,
        });
        let expired = serde_json::json!({
            "id": "a1b2c3d4", "secret_sha256": "ff",
            "revoked": false, "expires_at": 1i64,
        });

        for (what, g) in [("revoked", revoked), ("expired", expired)] {
            std::fs::write(dir.join("share.json"), record(g, false)).expect("write");
            let err = refuse_if_shared(root, &m, "rebase").expect_err(what);
            assert!(
                format!("{err}").contains("being shared right now"),
                "{what}: {err}"
            );
        }

        // And the window before a transport exists at all: the record is
        // written before `Setup::start`, which for `--tunnel` waits up to
        // forty-five seconds. Nothing was on disk during it, so every verb
        // here saw an unshared box and proceeded — and the start then
        // announced a public endpoint on top of what they had done.
        std::fs::write(
            dir.join("share.json"),
            record(serde_json::Value::Null, true),
        )
        .expect("write");
        let err = refuse_if_shared(root, &m, "abort").expect_err("a starting share");
        let said = format!("{err}");
        assert!(said.contains("about to be shared"), "{said}");
        assert!(said.contains("h5i box share stop draining"), "{said}");
    }

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
        let raw = crate::refstore::read_blob_from_tree(repo, Some(&tree), MANIFESTS_FILE)
            .unwrap_or_default();
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
        std::fs::write(
            work.join("plugin/persona/architect.md"),
            "# Architect\nThink first.\n",
        )
        .unwrap();
        std::fs::write(work.join("plugin/persona/careful.md"), "Be careful.\n").unwrap();

        // Empty list → no file, no digest.
        assert_eq!(materialize_persona(work, &[]).unwrap(), None);
        assert!(!work.join(PERSONA_FILE).exists());

        // Two sources → concatenated in order with per-source headers.
        let sources = vec![
            "plugin/persona/architect.md".to_string(),
            "plugin/persona/careful.md".to_string(),
        ];
        let digest = materialize_persona(work, &sources)
            .unwrap()
            .expect("a digest");
        let body = std::fs::read_to_string(work.join(PERSONA_FILE)).unwrap();
        assert!(body.contains("<!-- persona: plugin/persona/architect.md -->"));
        assert!(body.contains("# Architect"));
        // Order is preserved: architect appears before careful.
        assert!(body.find("# Architect").unwrap() < body.find("Be careful.").unwrap());
        assert_eq!(digest, crate::refstore::sha256_hex(body.as_bytes()));

        // PERSONA.md is git-excluded so it never shows as a worktree change.
        let exclude = std::fs::read_to_string(work.join(".git/info/exclude")).unwrap_or_default();
        assert!(exclude.lines().any(|l| l.trim() == "/PERSONA.md"));
        let wt = Repository::open(work).unwrap();
        assert!(wt.status_should_ignore(Path::new(PERSONA_FILE)).unwrap());

        // A missing source fails closed.
        assert!(materialize_persona(work, &["plugin/persona/nope.md".to_string()]).is_err());
    }

    /// The per-env HOME copy must not be more permissive than the original. A
    /// 0700 `~/.codex` copied to 0755 exposes a 0644 config that was relying on
    /// its parent for protection.
    #[test]
    #[cfg(unix)]
    fn the_home_seed_copy_keeps_the_source_directory_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src/.codex");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(src.join("config.toml"), "key = 1").unwrap();

        let dst = td.path().join("dst/.codex");
        copy_tree(&src, &dst).unwrap();
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "copied dir must not widen, got {mode:o}");
    }

    /// State files are read by unsynchronised readers on every env command, so
    /// a truncate-then-write window turned a live box into "does not exist" —
    /// and in `materialize_from_ref` into "local is not newer", overwriting the
    /// on-disk manifest from the ref copy.
    #[test]
    fn state_writes_are_atomic_and_leave_no_temp_behind() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("state.json");
        atomic_write(&p, b"first").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first");
        atomic_write(&p, b"second").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second");
        let leftovers: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// `live_sessions` unlinks what it cannot parse. A record whose pid is still
    /// alive must survive that, or a healthy session loses its registration and
    /// the box reads as stale for the rest of its life.
    #[test]
    fn an_unparseable_live_record_for_a_running_pid_is_kept() {
        let td = tempfile::tempdir().unwrap();
        let live = td.path().join(LIVE_DIR);
        std::fs::create_dir_all(&live).unwrap();
        let mine = live.join(format!("{}.json", std::process::id()));
        std::fs::write(&mine, b"{\"pid\": ").unwrap(); // torn
        let dead = live.join("4294967294.json");
        std::fs::write(&dead, b"{\"pid\": ").unwrap();

        let _ = live_sessions(td.path());
        assert!(mine.exists(), "a live pid's record must not be reaped");
        assert!(!dead.exists(), "a dead pid's torn record is reapable");
    }

    /// The host's own `~/.claude` / `~/.codex` must not be snapshotted and
    /// restored: the box cannot write them (they are bind-redirected at the
    /// kernel tiers and unmounted at the container tiers), so any difference at
    /// exit is the operator's own edit — which the guard used to overwrite, or
    /// delete outright, and then blame on the sandbox.
    #[test]
    fn the_hook_guard_leaves_the_operators_own_config_alone() {
        let td = tempfile::tempdir().unwrap();
        let home = td.path().join("home");
        let work = td.path().join("work");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let host_cfg = home.join(".claude/settings.json");
        std::fs::write(&host_cfg, b"{\"before\":1}").unwrap();

        // Safety: single-threaded test.
        unsafe { std::env::set_var("HOME", &home) };
        let guard = ProtectedHookConfigGuard::prepare(&work, IsolationClaim::Process).unwrap();
        // The operator edits their own config during the session.
        std::fs::write(&host_cfg, b"{\"after\":2}").unwrap();
        guard
            .finish()
            .expect("a host-side edit is not a sandbox violation");
        assert_eq!(
            std::fs::read(&host_cfg).unwrap(),
            b"{\"after\":2}",
            "the operator's edit must survive"
        );
    }

    /// The spool is written by the box and is therefore untrusted. A reader
    /// that follows a symlink there can be pointed at any host file — or at
    /// /dev/zero, which with no size cap hangs the host process. `box status`
    /// (which the console polls every 8s) used a plain `fs::read`.
    #[test]
    #[cfg(unix)]
    fn spool_reads_refuse_symlinks_and_stay_capped() {
        let td = tempfile::tempdir().unwrap();
        let spool = td.path().join("spool");
        std::fs::create_dir_all(&spool).unwrap();

        // A symlink is refused outright, even to a perfectly ordinary file.
        let real = td.path().join("host-secret");
        std::fs::write(&real, "PRIVATE").unwrap();
        std::os::unix::fs::symlink(&real, spool.join("cap-1.json")).unwrap();
        assert_eq!(read_spool_capped(&spool.join("cap-1.json"), 1024), None);

        // A regular file is read, and a large one is capped rather than
        // pulled into memory whole.
        let big = spool.join("cmd-1-0.out");
        std::fs::write(&big, vec![b'x'; 4096]).unwrap();
        let got = read_spool_capped(&big, 512).unwrap();
        assert!(got.starts_with(&[b'x'; 512][..]));
        assert!(String::from_utf8_lossy(&got).contains("truncated"));
    }

    /// The remembered loopback ports decide what `policy.loopback_ports` grants,
    /// which macOS turns into `(allow network-outbound (remote ip
    /// "localhost:<port>"))`. They must therefore live outside the one browser
    /// directory the box can write, or a box picks the host service its own
    /// next session may reach.
    #[test]
    fn remembered_browser_ports_are_not_box_writable() {
        let td = tempfile::tempdir().unwrap();
        let m = wt_manifest("human", "b");
        let (dir, state) = browser_dirs(td.path(), &m);
        for f in ["cdp-port", "egress-port"] {
            assert!(
                !dir.join(f).starts_with(&state),
                "{f} must not sit under the box-writable state dir"
            );
        }
        // And the port that is read back is still validated.
        std::fs::create_dir_all(&dir).unwrap();
        for bad in ["0", "80", "-1", "not a port", ""] {
            std::fs::write(dir.join("cdp-port"), bad).unwrap();
            let got = remembered_port(&dir.join("cdp-port"), "test", &[]).unwrap();
            assert!(got >= 1024, "{bad:?} yielded {got}");
        }
    }

    /// Build a manifest for an attached box on `refs/heads/h5i/env/<agent>/<slug>`.
    fn wt_manifest(agent: &str, slug: &str) -> EnvManifest {
        EnvManifest {
            id: format!("env/{agent}/{slug}"),
            agent: agent.into(),
            slug: slug.into(),
            // Object ids, because `load_manifest_at` checks that they are —
            // this fixture is written to disk and read back.
            base_commit: "a".repeat(40),
            base_tree: "b".repeat(40),
            parent_branch: "main".into(),
            branch: format!("refs/heads/{BRANCH_PREFIX}{agent}/{slug}"),
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "c".repeat(64),
            effective_digest: None,
            fs_authority: None,
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
            runner_id: None,
            runner: None,
        }
    }

    /// A host repo with one commit, plus a real worktree for the env branch.
    /// Returns (tempdir, h5i_root, manifest).
    fn worktree_fixture() -> (tempfile::TempDir, PathBuf, EnvManifest) {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host");
        std::fs::create_dir_all(&host).unwrap();
        let repo = Repository::init(&host).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();

        let m = wt_manifest("human", "t");
        let h5i_root = repo.commondir().join(".h5i");
        let work = m.work_dir(&h5i_root);
        std::fs::create_dir_all(work.parent().unwrap()).unwrap();

        let out = std::process::Command::new("git")
            .current_dir(&host)
            .args(["worktree", "add", "-b", m.branch_short()])
            .arg(&work)
            .arg("HEAD")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (dir, h5i_root, m)
    }

    #[test]
    fn open_env_worktree_accepts_the_box_it_belongs_to() {
        let (_d, h5i_root, m) = worktree_fixture();
        let wt = open_env_worktree(&h5i_root, &m).expect("the box's own worktree opens");
        assert_eq!(wt.head().unwrap().name(), Some(m.branch.as_str()));
    }

    /// The advisory case: the box rewrites `$WORK/.git` to point at the host
    /// repository, so a later `propose`/`rebase` would stage the box tree onto
    /// whatever the host has checked out (`main`) instead of the env branch.
    /// The object store still matches — only HEAD gives it away.
    #[test]
    fn open_env_worktree_refuses_a_pointer_redirected_at_the_host_repo() {
        let (_d, h5i_root, m) = worktree_fixture();
        let work = m.work_dir(&h5i_root);
        let host_git = h5i_root.parent().unwrap().to_path_buf();
        std::fs::write(
            work.join(".git"),
            format!("gitdir: {}\n", host_git.display()),
        )
        .unwrap();

        let err = open_env_worktree(&h5i_root, &m)
            .map(|_| ())
            .expect_err("must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains(&m.branch),
            "should name the branch it owns: {msg}"
        );
        assert!(msg.contains("fail-closed"), "{msg}");
    }

    /// The same pointer rewritten at an unrelated repository: caught by the
    /// object-store check rather than the branch check.
    #[test]
    fn open_env_worktree_refuses_a_pointer_redirected_at_a_foreign_repo() {
        let (d, h5i_root, m) = worktree_fixture();
        let other = d.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let orepo = Repository::init(&other).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree = orepo
            .find_tree(orepo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        orepo
            .commit(Some("HEAD"), &sig, &sig, "other", &tree, &[])
            .unwrap();
        // Give the foreign repo the same branch name, so only the object-store
        // check can distinguish it.
        let head = orepo.head().unwrap().peel_to_commit().unwrap();
        orepo.branch(m.branch_short(), &head, false).unwrap();
        orepo.set_head(&m.branch).unwrap();

        let work = m.work_dir(&h5i_root);
        std::fs::write(
            work.join(".git"),
            format!("gitdir: {}\n", orepo.path().display()),
        )
        .unwrap();

        let err = open_env_worktree(&h5i_root, &m)
            .map(|_| ())
            .expect_err("must fail closed");
        assert!(err.to_string().contains("not this box's"), "{err}");
    }
}
