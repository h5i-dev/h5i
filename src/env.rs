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

/// Git ref holding the append-only env event log (one JSON object per line).
pub const ENV_REF: &str = "refs/h5i/env";
/// Top-level file inside the ref's tree holding the event log.
pub const EVENTS_FILE: &str = "events.jsonl";
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

// ─── event log: CAS append + union merge (same pattern as objects/msg) ──────

/// Append one event to `refs/h5i/env` with compare-and-swap semantics.
pub fn append_event(repo: &Repository, ev: &EnvEvent) -> Result<(), H5iError> {
    const MAX_ATTEMPTS: usize = 64;
    let line = serde_json::to_string(ev)?;
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
        log.push_str(&line);
        log.push('\n');

        let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &[(EVENTS_FILE, &log)])?;
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

/// Reconcile two divergent `refs/h5i/env` tips: the log is append-only, so a
/// divergence is two disjoint sets of events — union them (dedup on the
/// `(env_id, ts, event)` key), re-sort by timestamp, commit with both parents.
/// Mirrors [`crate::objects::union_merge_commits`] so `h5i pull` never drops
/// an event.
pub fn union_merge_commits(
    repo: &Repository,
    local_oid: git2::Oid,
    incoming_oid: git2::Oid,
) -> Result<git2::Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<EnvEvent> = Vec::new();
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
                    merged.push(e);
                }
            }
        }
    }
    merged.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.env_id.cmp(&b.env_id)));

    let mut log = String::new();
    for e in &merged {
        log.push_str(&serde_json::to_string(e)?);
        log.push('\n');
    }

    let base_tree = local_commit.tree().ok();
    let tree_oid = objects::build_tree(repo, base_tree.as_ref(), &[(EVENTS_FILE, &log)])?;
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
    save_manifest(h5i_root, m)?;
    append_event(
        repo,
        &EnvEvent {
            ts: now_ts(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: event.to_string(),
            detail,
            capture,
        },
    )
}

// ─── create (§9) ────────────────────────────────────────────────────────────

pub struct CreateOpts {
    /// Base revision (default HEAD). Pinned immutably at creation.
    pub from: Option<String>,
    /// Policy profile name in `.h5i/env.toml` (default `default`).
    pub profile: String,
    /// CLI `--isolation` override — the *minimum* claim; fails closed if unmet.
    pub isolation: Option<IsolationClaim>,
    /// Workspace backend. `auto` and `worktree` are accepted today.
    pub backend: String,
}

impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            from: None,
            profile: "default".into(),
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
    if agent.is_empty() || agent.contains('/') || agent.contains('\\') {
        return Err(H5iError::Metadata(format!("invalid agent name '{agent}'")));
    }
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

    // Policy first (fail closed BEFORE any state is created on disk).
    let profile = sandbox::load_profile(workdir, &opts.profile, opts.isolation)?;
    let caps = sandbox::probe_host();
    let policy = sandbox::resolve(&profile, &caps)?;
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
        status: ST_CREATED.to_string(),
        captures: Vec::new(),
    };

    let policy_path = dir.join(POLICY_RESOLVED_FILE);
    std::fs::write(&policy_path, policy.to_toml()?)
        .map_err(|e| H5iError::with_path(e, &policy_path))?;
    save_manifest(h5i_root, &manifest)?;
    append_event(
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
    )?;
    Ok(manifest)
}

// ─── run (§9): capture-wrapped, policy-enforced ─────────────────────────────

pub struct RunOutcome {
    /// Object id of the evidence capture in `refs/h5i/objects`.
    pub capture_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The capture manifest (for rendering).
    pub manifest: objects::Manifest,
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
        return Err(H5iError::Metadata(format!(
            "workspace for {} is missing ({}) — was it gc'd?",
            m.id,
            work.display()
        )));
    }

    // Serialize concurrent runs of THIS env (status + captures are mutated
    // below and must not interleave). Held for the duration of the run.
    #[cfg(unix)]
    let _run_lock = RunLock::acquire(&m.dir(h5i_root))?;

    // The stored policy, digest-verified, then re-resolved against a fresh
    // host probe (fail closed if the host can no longer satisfy the claim).
    let policy = load_policy(h5i_root, m)?;

    set_status(repo, h5i_root, m, ST_RUNNING, "status", Some("running".into()), None)?;
    let result = sandbox::run(&policy, &work, argv);
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
        // Evidence is shared via `h5i objects push` — scrub secrets from the
        // stored blob and summary before it is written (design §7).
        redact: true,
    };
    let captured = objects::capture(&wt_repo, h5i_root, &raw, capture_opts)?;
    let capture_id = captured.manifest.id.clone();

    m.captures.push(capture_id.clone());
    set_status(
        repo,
        h5i_root,
        m,
        ST_IDLE,
        "exec",
        Some(format!(
            "cmd=`{}` exit={}{}",
            argv.join(" "),
            outcome.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            if outcome.timed_out { " timed-out" } else { "" }
        )),
        Some(capture_id.clone()),
    )?;

    Ok(RunOutcome {
        capture_id,
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        manifest: captured.manifest,
    })
}

// ─── diff ───────────────────────────────────────────────────────────────────

/// Unified diff of the env's current worktree state against its pinned base
/// tree (committed + uncommitted work, including untracked files).
pub fn diff(h5i_root: &Path, m: &EnvManifest, stat_only: bool) -> Result<String, H5iError> {
    let work = m.work_dir(h5i_root);
    if !work.is_dir() {
        return Err(H5iError::Metadata(format!(
            "workspace for {} is missing — `env diff` needs the worktree (status: {})",
            m.id, m.status
        )));
    }
    let wt_repo = Repository::open(&work)?;
    let base_tree = wt_repo.find_tree(git2::Oid::from_str(&m.base_tree)?)?;
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true);
    let diff = wt_repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;

    if stat_only {
        let stats = diff.stats()?;
        let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        return Ok(buf.as_str().unwrap_or("").to_string());
    }
    let mut out = String::new();
    diff.print(git2::DiffFormat::Patch, |_d, _h, line| {
        let prefix = match line.origin() {
            '+' | '-' | ' ' => Some(line.origin()),
            _ => None,
        };
        if let Some(p) = prefix {
            out.push(p);
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(out)
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

// ─── mediated commit (§4 — the critical security boundary) ─────────────────

/// Snapshot the env worktree onto the env branch **host-side**: h5i stages and
/// commits; the agent never drives `git` at `process`+ tiers. Every staged
/// path is validated against the canonicalized-`$WORK` allowlist invariant —
/// symlink escapes, nested `.git` repos / submodule gitlinks, and `..`
/// traversal are rejected and the whole commit **fails closed**.
///
/// Returns `Ok(None)` when the worktree is identical to the branch tip.
pub fn mediated_commit(h5i_root: &Path, m: &EnvManifest) -> Result<Option<git2::Oid>, H5iError> {
    let work = m.work_dir(h5i_root);
    let wt_repo = Repository::open(&work)?;
    let canon_work = work.canonicalize().map_err(|e| H5iError::with_path(e, &work))?;

    // Pre-walk for nested git repositories. libgit2 either errors opaquely or
    // records a submodule gitlink when `add_all` meets a directory containing
    // `.git` — both are wrong here. Detect them OURSELVES, first, and refuse
    // with a precise diagnostic (fail closed).
    let mut violations: Vec<String> = scan_nested_git(&canon_work);
    if !violations.is_empty() {
        return Err(H5iError::Metadata(format!(
            "mediated commit refused (fail-closed) — {} path violation(s):\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        )));
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
        return Err(H5iError::Metadata(format!(
            "mediated commit refused (fail-closed) — {} path violation(s):\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        )));
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
    let commit = mediated_commit(h5i_root, m)?;
    let stat = diff(h5i_root, m, true).unwrap_or_default();
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

/// Stop the env: mark it aborted and preserve the manifest + workspace for
/// forensics (`gc` reclaims the workspace later).
pub fn abort(repo: &Repository, h5i_root: &Path, m: &mut EnvManifest) -> Result<(), H5iError> {
    if m.status == ST_APPLIED {
        return Err(H5iError::Metadata(format!("{} is already applied — nothing to abort", m.id)));
    }
    set_status(repo, h5i_root, m, ST_ABORTED, "aborted", Some("manifest preserved for forensics".into()), None)
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
        let work = m.work_dir(h5i_root);
        if !work.exists() {
            continue;
        }
        if let Ok(wt) = repo.find_worktree(&m.worktree_name()) {
            // The env was locked at create; we are intentionally reclaiming an
            // applied/aborted env, so override the lock (locked(true)).
            let _ = wt.unlock();
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            if wt.prune(Some(&mut opts)).is_err() {
                continue;
            }
        }
        if work.exists() {
            std::fs::remove_dir_all(&work).map_err(|e| H5iError::with_path(e, &work))?;
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

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
