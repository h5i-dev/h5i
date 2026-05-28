//! Cross-agent messaging over a Git ref — the associative dimension's
//! coordination layer.
//!
//! Unlike machine-local message stores (e.g. a shared SQLite file), h5i keeps
//! the message log in a Git ref (`refs/h5i/msg`) so it travels with the repo
//! and is shared through `h5i share push` / `h5i share pull`, exactly like
//! `refs/h5i/notes` and `refs/h5i/memory`.
//!
//! ## Layout in `refs/h5i/msg` (an orphan branch)
//!
//! ```text
//! messages.jsonl   append-only, one JSON [`Message`] per line
//! agents.json      roster of known agents (name → last-seen timestamp)
//! ```
//!
//! `messages.jsonl` is **strictly append-only**: a `send` only ever adds a
//! line. This is what makes the ref safe to union-merge across machines — two
//! agents that each appended different messages produce non-overlapping line
//! sets, and [`union_merge_commits`] reconciles them by id.
//!
//! ## Read-state lives locally, not in the ref
//!
//! "Which messages have I already seen" is a per-machine concern, and storing
//! it in the shared ref would both bloat the log and create write contention
//! on every `inbox`. Instead each agent keeps a **watermark cursor** in the
//! local sidecar (`.git/.h5i/msg/cursor.json`): the `(ts, id)` of the last
//! message it read. `inbox` returns everything addressed to the agent after
//! that watermark and advances it. Because the watermark is a single point in
//! the total `(ts, id)` order, a message that arrives via `pull` with an
//! *earlier* key than the current watermark (clock skew / late delivery) is
//! treated as already-read by `inbox` — it still shows up in `history`. This
//! is a deliberate v1 tradeoff: it keeps the cursor O(1) and the shared log
//! append-only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::H5iError;

/// The single ref namespace that holds the shared message log.
pub const MSG_REF: &str = "refs/h5i/msg";

/// Recipient name that fans a message out to every agent except the sender.
pub const BROADCAST: &str = "all";

/// Environment variable consulted (after an explicit flag, before the stored
/// identity) when resolving "who am I" for `send` / `inbox`.
pub const AGENT_ENV: &str = "H5I_AGENT";

const MESSAGES_FILE: &str = "messages.jsonl";
const AGENTS_FILE: &str = "agents.json";

/// Current i5h wire-format version written by this build.
pub const PROTOCOL_VERSION: u32 = 1;

/// One message in the shared log. Lines in `messages.jsonl` are exactly the
/// JSON serialization of this struct.
///
/// The total order over messages is `(ts, id)`: `ts` is a fixed-width RFC3339
/// UTC timestamp (microsecond precision) so it sorts lexicographically, and
/// `id` breaks ties deterministically.
///
/// ## i5h compatibility
///
/// Every added field is `#[serde(default)]` and skipped when empty, so a v0
/// PoC line (`{id,ts,from,to,body[,tag]}`) still deserializes — `version`
/// reads back as `0`. Use [`Message::effective_kind`] rather than reading
/// `kind` directly, so legacy messages map onto an i5h kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    /// Stable unique id (16 hex chars). Used for dedup on merge and as the
    /// tie-breaker in the total order.
    pub id: String,
    /// RFC3339 UTC timestamp, `YYYY-MM-DDThh:mm:ss.ffffffZ` (lexically sortable).
    pub ts: String,
    /// Sending agent's identity.
    pub from: String,
    /// Recipient agent identity, or [`BROADCAST`] for a fan-out message.
    pub to: String,
    /// Message body (free text). Stored verbatim; sanitized only at render.
    pub body: String,
    /// Legacy v0 classification (`review` / `risk` / …). Retained for back-compat
    /// and surfaced as a UI badge; new messages also carry [`Message::kind`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,

    // ── i5h v1 fields ──────────────────────────────────────────────────────
    /// Protocol version. `0` for a legacy v0 line; [`PROTOCOL_VERSION`] for new.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub version: u32,
    /// i5h message kind (e.g. `ASK`, `REVIEW_REQUEST`, `RISK`, `DONE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Id of the message this one replies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Stable thread-root id (defaults to the reply_to root, or self).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// `low` / `normal` / `high` / `urgent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// `open` / `acked` / `done` / `declined` / `stale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Git branch relevant to the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// h5i context branch relevant to the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_branch: Option<String>,
    /// Files, symbols, tests, or scopes to inspect first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<String>,
    /// Concise risk statement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    /// Optional UTC RFC3339 deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// Related PRs, commits, context nodes, claims, or URLs (open object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<serde_json::Value>,
    /// Forward-compatible extension area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl Message {
    /// Total-order key: `(ts, id)`.
    fn key(&self) -> (&str, &str) {
        (self.ts.as_str(), self.id.as_str())
    }

    /// True if this message should land in `who`'s inbox: either addressed
    /// directly to them, or a broadcast from someone else.
    fn addressed_to(&self, who: &str) -> bool {
        self.to == who || (self.to == BROADCAST && self.from != who)
    }

    /// The effective i5h kind: the explicit `kind` when present, otherwise
    /// inferred from a legacy `tag` / the recipient (so v0 messages render as
    /// a kind too).
    pub fn effective_kind(&self) -> String {
        match &self.kind {
            Some(k) if !k.is_empty() => k.clone(),
            _ => infer_kind(self.tag.as_deref(), &self.to),
        }
    }

    /// Stable thread-root id: the explicit `thread_id`, else this message's own
    /// id (a message with no thread is the root of its own thread).
    pub fn thread_root(&self) -> String {
        self.thread_id.clone().unwrap_or_else(|| self.id.clone())
    }
}

/// Map a legacy tag (or absence of one) onto an i5h kind.
///
/// - known tag → its kind (`review`/`review-request` → `REVIEW_REQUEST`,
///   `risk` → `RISK`); a tag that is already a kind name is upper-cased through;
/// - otherwise the recipient decides the default: `FYI` for a broadcast,
///   `ASK` for a directed message.
pub fn infer_kind(tag: Option<&str>, to: &str) -> String {
    if let Some(t) = tag {
        let t = t.trim();
        match t.to_ascii_lowercase().as_str() {
            "" => {}
            "review" | "review-request" | "review_request" => return "REVIEW_REQUEST".into(),
            "risk" => return "RISK".into(),
            other => {
                const KINDS: &[&str] = &[
                    "fyi", "ask", "review_request", "risk", "blocked", "handoff", "ack",
                    "done", "decline", "broadcast",
                ];
                if KINDS.contains(&other) {
                    return other.to_ascii_uppercase();
                }
                // Unknown tag: fall through to the recipient-based default.
            }
        }
    }
    if to == BROADCAST {
        "FYI".into()
    } else {
        "ASK".into()
    }
}

/// Roster of known agents, persisted as `agents.json`. Maps an agent name to
/// the timestamp it was last seen sending or receiving.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Roster {
    #[serde(default)]
    agents: BTreeMap<String, String>,
}

/// A legacy `(ts, id)` watermark. Kept only so an older `cursor.json` can be
/// read and lazily migrated into the seen-id model.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Watermark {
    ts: String,
    id: String,
}

impl Watermark {
    fn key(&self) -> (&str, &str) {
        (self.ts.as_str(), self.id.as_str())
    }
}

/// Local, per-machine read state. Persisted to `.git/.h5i/msg/cursor.json`.
///
/// Read-state is a **set of seen message ids per agent**, not a `(ts, id)`
/// watermark. This is what the i5h protocol requires: a message pulled from
/// another clone can have an *older* timestamp than the newest message already
/// seen, so a single watermark would silently hide it. A seen-id set never
/// does. The legacy `cursors` watermark is read once and migrated.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorStore {
    #[serde(default)]
    seen: BTreeMap<String, BTreeSet<String>>,
    /// Legacy watermark map (pre-seen-id). Migrated lazily, then cleared.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    cursors: BTreeMap<String, Watermark>,
}

// ─────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────

/// Optional i5h fields for a structured send. All default to empty/None, so
/// `SendOpts::default()` plus a `tag` reproduces the simple v0 send.
#[derive(Debug, Clone, Default)]
pub struct SendOpts {
    /// Explicit i5h kind. When `None`, it is inferred from `tag` / recipient.
    pub kind: Option<String>,
    /// Legacy classification badge (kept for back-compat / display).
    pub tag: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub branch: Option<String>,
    pub context_branch: Option<String>,
    pub focus: Vec<String>,
    pub risk: Option<String>,
    pub deadline: Option<String>,
    pub links: Option<serde_json::Value>,
    /// Id of the message being replied to (sets `reply_to` + threads).
    pub reply_to: Option<String>,
    /// Explicit thread root; defaults to `reply_to` when replying.
    pub thread_id: Option<String>,
}

/// Append a simple message from `from` to `to` (optionally tagged) and update
/// the roster. Thin wrapper over [`send_msg`]; persists `from` as the local
/// default identity.
pub fn send(
    repo: &Repository,
    h5i_root: &Path,
    from: &str,
    to: &str,
    body: &str,
    tag: Option<&str>,
) -> Result<Message, H5iError> {
    send_msg(
        repo,
        h5i_root,
        from,
        to,
        body,
        SendOpts {
            tag: tag.map(str::to_string),
            ..Default::default()
        },
    )
}

/// Append a structured i5h message. Resolves the kind (explicit, else inferred
/// from tag/recipient), threads replies (`thread_id` defaults to `reply_to`),
/// stamps the protocol version, appends via CAS, and stores the sender as the
/// local identity.
pub fn send_msg(
    repo: &Repository,
    h5i_root: &Path,
    from: &str,
    to: &str,
    body: &str,
    opts: SendOpts,
) -> Result<Message, H5iError> {
    validate_name(from)?;
    validate_name(to)?;

    let ts = now_ts();
    let id = gen_id(&ts, from, to, body);
    let tag = opts.tag.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let kind = opts
        .kind
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| infer_kind(tag.as_deref(), to));
    let thread_id = opts.thread_id.or_else(|| opts.reply_to.clone());

    let msg = Message {
        id,
        ts,
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
        tag,
        version: PROTOCOL_VERSION,
        kind: Some(kind),
        reply_to: opts.reply_to,
        thread_id,
        priority: opts.priority.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()),
        status: opts.status.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()),
        branch: opts.branch.filter(|s| !s.is_empty()),
        context_branch: opts.context_branch.filter(|s| !s.is_empty()),
        focus: opts.focus.into_iter().filter(|s| !s.is_empty()).collect(),
        risk: opts.risk.filter(|s| !s.is_empty()),
        deadline: opts.deadline.filter(|s| !s.is_empty()),
        links: opts.links,
        meta: None,
    };

    append_message_cas(repo, &msg)?;
    write_identity(h5i_root, from)?;
    Ok(msg)
}

/// Append `msg` to `refs/h5i/msg` with compare-and-swap semantics: build the
/// commit off the current tip, then move the ref only if it still points where
/// we read it. If a concurrent writer moved the tip first, re-read and retry so
/// no append is silently lost (the i5h send contract).
fn append_message_cas(repo: &Repository, msg: &Message) -> Result<(), H5iError> {
    const MAX_ATTEMPTS: usize = 8;
    let line = serde_json::to_string(msg)?;
    let message = format!("h5i msg: {} → {}", msg.from, msg.to);

    for _ in 0..MAX_ATTEMPTS {
        let tip = repo.refname_to_id(MSG_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        // Append our line to the current log.
        let mut log = read_blob_from_tree(repo, base_tree.as_ref(), MESSAGES_FILE)
            .unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&line);
        log.push('\n');

        // Update the roster off the same base.
        let mut roster: Roster = read_blob_from_tree(repo, base_tree.as_ref(), AGENTS_FILE)
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        roster.agents.insert(msg.from.clone(), msg.ts.clone());
        if msg.to != BROADCAST {
            roster.agents.entry(msg.to.clone()).or_insert_with(|| msg.ts.clone());
        }
        let roster_json = serde_json::to_string_pretty(&roster)?;

        let tree_oid = build_tree(
            repo,
            base_tree.as_ref(),
            &[(MESSAGES_FILE, &log), (AGENTS_FILE, &roster_json)],
        )?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        // Create the commit object WITHOUT moving the ref.
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        // Compare-and-swap the ref.
        let cas_ok = match tip {
            // No tip yet: create only if still absent (force=false fails if a
            // racing writer created it first).
            None => repo.reference(MSG_REF, new_oid, false, &message).is_ok(),
            // Tip existed: overwrite only if it still equals what we read.
            Some(old) => repo
                .reference_matching(MSG_REF, new_oid, true, old, &message)
                .is_ok(),
        };
        if cas_ok {
            return Ok(());
        }
        // Lost the race — loop, re-read the new tip, and re-append.
    }
    Err(H5iError::Internal(format!(
        "h5i msg: {} → {} could not be appended after {MAX_ATTEMPTS} attempts (ref kept moving)",
        msg.from, msg.to
    )))
}

/// Return the messages addressed to `me` that it has not yet seen, sorted
/// oldest-first. When `advance` is true the returned ids are added to `me`'s
/// seen-set (so the next call won't repeat them); pass `false` to peek.
///
/// Unread is decided by **id membership**, not a timestamp watermark, so a
/// late-arriving message (older `ts` than something already read, e.g. pulled
/// from another clone) is still delivered exactly once.
pub fn inbox(
    repo: &Repository,
    h5i_root: &Path,
    me: &str,
    advance: bool,
) -> Result<Vec<Message>, H5iError> {
    let mut store = read_cursors(h5i_root)?;

    let mut addressed: Vec<Message> = read_messages(repo)
        .into_iter()
        .filter(|m| m.addressed_to(me))
        .collect();
    addressed.sort_by(|a, b| a.key().cmp(&b.key()));

    // Lazily migrate a legacy watermark: everything at or below it was already
    // read, so seed the seen-set with those ids and drop the watermark.
    let mut dirty = false;
    if let Some(wm) = store.cursors.remove(me) {
        let seen = store.seen.entry(me.to_string()).or_default();
        for m in &addressed {
            if m.key() <= wm.key() {
                seen.insert(m.id.clone());
            }
        }
        dirty = true;
    }

    let seen = store.seen.get(me).cloned().unwrap_or_default();
    let unread: Vec<Message> = addressed
        .into_iter()
        .filter(|m| !seen.contains(&m.id))
        .collect();

    if advance && !unread.is_empty() {
        let set = store.seen.entry(me.to_string()).or_default();
        for m in &unread {
            set.insert(m.id.clone());
        }
        dirty = true;
    }
    if dirty {
        write_cursors(h5i_root, &store)?;
    }
    Ok(unread)
}

/// Return up to `limit` most-recent messages (oldest-first within the window).
/// When `with` is set, restrict to messages where that agent is the sender or
/// recipient (a conversation view).
pub fn history(
    repo: &Repository,
    with: Option<&str>,
    limit: usize,
) -> Result<Vec<Message>, H5iError> {
    let mut all: Vec<Message> = read_messages(repo)
        .into_iter()
        .filter(|m| match with {
            Some(w) => m.from == w || m.to == w,
            None => true,
        })
        .collect();
    all.sort_by(|a, b| a.key().cmp(&b.key()));
    if all.len() > limit {
        all = all.split_off(all.len() - limit);
    }
    Ok(all)
}

/// List known agents as `(name, last_seen_ts)`, sorted by name.
pub fn team(repo: &Repository) -> Vec<(String, String)> {
    read_roster(repo).agents.into_iter().collect()
}

/// Count messages currently unread by `me` (does not advance the cursor).
pub fn unread_count(repo: &Repository, h5i_root: &Path, me: &str) -> Result<usize, H5iError> {
    Ok(inbox(repo, h5i_root, me, false)?.len())
}

/// Look up a single message by id.
pub fn get_message(repo: &Repository, id: &str) -> Option<Message> {
    read_messages(repo).into_iter().find(|m| m.id == id)
}

/// Snapshot of the message ref for the dashboard's "GIT PROOF" band.
#[derive(Debug, Clone)]
pub struct Stats {
    /// Total messages in the log.
    pub total: usize,
    /// Short OID of the ref tip, if the ref exists.
    pub tip: Option<String>,
    /// Unix seconds of the tip commit time, if the ref exists.
    pub tip_time: Option<i64>,
}

/// Read the message-ref tip stats without loading message bodies twice.
pub fn stats(repo: &Repository) -> Stats {
    let total = read_messages(repo).len();
    let (tip, tip_time) = match repo
        .find_reference(MSG_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
    {
        Some(commit) => {
            let oid = commit.id().to_string();
            (Some(oid[..7.min(oid.len())].to_string()), Some(commit.time().seconds()))
        }
        None => (None, None),
    };
    Stats { total, tip, tip_time }
}

/// Persist the ordered ids shown in the most recent numbered view, so
/// `h5i msg reply <n>` can resolve a number back to a message. Scoped to the
/// viewing agent so a reply can't accidentally target another agent's view.
pub fn write_last_view(h5i_root: &Path, agent: &str, ids: &[String]) -> Result<(), H5iError> {
    let dir = msg_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let view = LastView {
        agent: agent.to_string(),
        ids: ids.to_vec(),
    };
    let json = serde_json::to_string_pretty(&view)?;
    let path = last_view_path(h5i_root);
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, path))
}

/// Resolve a 1-based number from the last numbered view into a message id,
/// verifying it belongs to `agent`'s view. Returns `None` when there is no
/// view, the agent differs, or `n` is out of range.
pub fn resolve_view_number(h5i_root: &Path, agent: &str, n: usize) -> Option<String> {
    let view = read_last_view(h5i_root)?;
    if view.agent != agent || n == 0 || n > view.ids.len() {
        return None;
    }
    Some(view.ids[n - 1].clone())
}

#[derive(Debug, Serialize, Deserialize)]
struct LastView {
    agent: String,
    ids: Vec<String>,
}

fn read_last_view(h5i_root: &Path) -> Option<LastView> {
    let raw = std::fs::read_to_string(last_view_path(h5i_root)).ok()?;
    serde_json::from_str(&raw).ok()
}

// ─────────────────────────────────────────────────────────────────────────
// Identity
// ─────────────────────────────────────────────────────────────────────────

/// Resolve the active agent identity: explicit value first, then the
/// `H5I_AGENT` environment variable, then the stored local identity. Errors
/// with actionable guidance when none is available.
pub fn resolve_identity(h5i_root: &Path, explicit: Option<&str>) -> Result<String, H5iError> {
    if let Some(name) = explicit {
        let name = name.trim();
        validate_name(name)?;
        write_identity(h5i_root, name)?;
        return Ok(name.to_string());
    }
    if let Ok(env) = std::env::var(AGENT_ENV) {
        let env = env.trim();
        if !env.is_empty() {
            validate_name(env)?;
            return Ok(env.to_string());
        }
    }
    if let Some(stored) = read_identity(h5i_root) {
        validate_name(&stored)?;
        return Ok(stored);
    }
    Err(H5iError::Metadata(format!(
        "no agent identity set — pass --as <name>, set ${AGENT_ENV}, or send a message with --from <name> first"
    )))
}

/// Read the stored local identity, if any.
pub fn read_identity(h5i_root: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(identity_path(h5i_root)).ok()?;
    let name = raw.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Persist the local default identity. Validates the name first, so a bad
/// identity can never be stored (and later trusted by `send` / dashboard /
/// hook) — `as` and `whoami <name>` reach this directly.
pub fn write_identity(h5i_root: &Path, name: &str) -> Result<(), H5iError> {
    validate_name(name)?;
    let dir = msg_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = identity_path(h5i_root);
    std::fs::write(&path, format!("{name}\n")).map_err(|e| H5iError::with_path(e, path))
}

/// Make an untrusted string safe to print to a terminal.
///
/// Message fields (`from` / `to` / `tag` / `body`) arrive from other clones
/// via `h5i share pull`, so they are untrusted input. A hostile sender could
/// embed ANSI/OSC escape sequences (cursor moves, colour resets, clickable
/// hyperlinks) or newlines to spoof the dashboard, the turn-delivery hook, or
/// the line-per-message `--plain` contract. We drop control characters
/// entirely (neutralising the `ESC` that begins every escape sequence) and
/// fold tab/newline/CR into single spaces. Storage keeps the exact bytes; only
/// rendering is sanitised, per the i5h protocol.
pub fn sanitize_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {} // drop ESC and other C0/C1/DEL controls
            c => out.push(c),
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────
// Pull divergence: line-union merge
// ─────────────────────────────────────────────────────────────────────────

/// Reconcile two divergent `refs/h5i/msg` tips into a single merge commit.
///
/// The two `messages.jsonl` blobs are unioned by message `id` (so a message
/// present on both sides appears once) and re-sorted into the canonical
/// `(ts, id)` order; the two rosters are unioned keeping the latest last-seen
/// per agent. The result is committed with both tips as parents (local first,
/// so it stays a descendant of the local ref and `update-ref` accepts it).
pub fn union_merge_commits(
    repo: &Repository,
    local_oid: Oid,
    incoming_oid: Oid,
) -> Result<Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    let local_msgs = parse_messages(
        &read_file_from_commit(repo, local_oid, MESSAGES_FILE).unwrap_or_default(),
    );
    let incoming_msgs = parse_messages(
        &read_file_from_commit(repo, incoming_oid, MESSAGES_FILE).unwrap_or_default(),
    );
    let merged_log = merge_message_sets(local_msgs, incoming_msgs);

    let mut roster = read_roster_from(repo, local_oid);
    for (name, seen) in read_roster_from(repo, incoming_oid).agents {
        roster
            .agents
            .entry(name)
            .and_modify(|cur| {
                if seen > *cur {
                    *cur = seen.clone();
                }
            })
            .or_insert(seen);
    }
    let roster_json = serde_json::to_string_pretty(&roster)?;

    let base_tree = local_commit.tree().ok();
    let tree_oid = build_tree(
        repo,
        base_tree.as_ref(),
        &[(MESSAGES_FILE, &merged_log), (AGENTS_FILE, &roster_json)],
    )?;
    let tree = repo.find_tree(tree_oid)?;

    let sig = signature(repo)?;
    let parents = [&local_commit, &incoming_commit];
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i pull: union-merge of refs/h5i/msg",
        &tree,
        &parents,
    )?;
    Ok(oid)
}

// ─────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────

fn msg_dir(h5i_root: &Path) -> PathBuf {
    h5i_root.join("msg")
}

fn identity_path(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("identity")
}

fn cursor_path(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("cursor.json")
}

fn last_view_path(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("last_view.json")
}

fn read_cursors(h5i_root: &Path) -> Result<CursorStore, H5iError> {
    let path = cursor_path(h5i_root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CursorStore::default()),
        Err(e) => Err(H5iError::with_path(e, path)),
    }
}

fn write_cursors(h5i_root: &Path, store: &CursorStore) -> Result<(), H5iError> {
    let dir = msg_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = cursor_path(h5i_root);
    let json = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, json).map_err(|e| H5iError::with_path(e, path))
}

/// Read every message currently on the `refs/h5i/msg` tip.
fn read_messages(repo: &Repository) -> Vec<Message> {
    parse_messages(&read_blob(repo, MESSAGES_FILE).unwrap_or_default())
}

fn read_roster(repo: &Repository) -> Roster {
    read_blob(repo, AGENTS_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn read_roster_from(repo: &Repository, oid: Oid) -> Roster {
    read_file_from_commit(repo, oid, AGENTS_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Parse a `messages.jsonl` blob into messages, skipping blank or malformed
/// lines (forward-compatible with unknown future formats).
fn parse_messages(content: &str) -> Vec<Message> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Message>(l).ok())
        .collect()
}

/// Union two message sets by id, sorted into canonical `(ts, id)` order, and
/// render back to a `messages.jsonl` blob.
fn merge_message_sets(a: Vec<Message>, b: Vec<Message>) -> String {
    let mut by_id: HashMap<String, Message> = HashMap::new();
    for m in a.into_iter().chain(b) {
        by_id.entry(m.id.clone()).or_insert(m);
    }
    let mut msgs: Vec<Message> = by_id.into_values().collect();
    msgs.sort_by(|x, y| x.key().cmp(&y.key()));
    let mut out = String::new();
    for m in &msgs {
        if let Ok(line) = serde_json::to_string(m) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Read a top-level file from the `refs/h5i/msg` tip.
fn read_blob(repo: &Repository, path: &str) -> Option<String> {
    let reference = repo.find_reference(MSG_REF).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Read a top-level file from a specific commit's tree.
fn read_file_from_commit(repo: &Repository, oid: Oid, path: &str) -> Option<String> {
    let commit = repo.find_commit(oid).ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Build a flat tree (top-level files only) by overlaying `files` onto `base`.
fn build_tree(
    repo: &Repository,
    base: Option<&git2::Tree>,
    files: &[(&str, &str)],
) -> Result<Oid, H5iError> {
    let mut builder = repo.treebuilder(base)?;
    for (name, content) in files {
        let blob = repo.blob(content.as_bytes())?;
        builder.insert(name, blob, 0o100644)?;
    }
    Ok(builder.write()?)
}

/// Read a top-level file from an arbitrary tree (the CAS append path reads the
/// candidate parent's tree directly rather than the live ref).
fn read_blob_from_tree(repo: &Repository, tree: Option<&git2::Tree>, path: &str) -> Option<String> {
    let entry = tree?.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

fn signature(repo: &Repository) -> Result<Signature<'static>, H5iError> {
    repo.signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)
}

/// Current UTC time as a fixed-width, lexically sortable RFC3339 string.
fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// Derive a stable, collision-resistant message id from its fields plus a
/// random nonce (so two identical bodies sent in the same microsecond differ).
fn gen_id(ts: &str, from: &str, to: &str, body: &str) -> String {
    let nonce = fastrand::u64(..);
    let mut hasher = Sha256::new();
    hasher.update(ts.as_bytes());
    hasher.update([0]);
    hasher.update(from.as_bytes());
    hasher.update([0]);
    hasher.update(to.as_bytes());
    hasher.update([0]);
    hasher.update(body.as_bytes());
    hasher.update([0]);
    hasher.update(nonce.to_le_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Validate an agent identity against the conservative i5h charset
/// `[A-Za-z0-9._-]+`. This rejects whitespace, path separators (`/`, `\`),
/// and terminal control characters — names flow into roster keys, the
/// `from → to` model, file-free local state, and (untrusted on pull) terminal
/// output, so the same rule applies on every resolution path.
fn validate_name(name: &str) -> Result<(), H5iError> {
    if name.is_empty() {
        return Err(H5iError::Metadata("agent name must not be empty".into()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(H5iError::Metadata(format!(
            "invalid agent name {name:?}: use letters, digits, '.', '_', '-' only \
             (no spaces, path separators, or control characters)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A throwaway git repo plus an h5i sidecar root inside it.
    fn fixture() -> (TempDir, Repository, PathBuf) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Deterministic committer identity for the message commits.
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        let h5i_root = dir.path().join(".git").join(".h5i");
        std::fs::create_dir_all(&h5i_root).unwrap();
        (dir, repo, h5i_root)
    }

    #[test]
    fn send_then_inbox_delivers_and_advances_cursor() {
        let (_d, repo, root) = fixture();

        send(&repo, &root, "alice", "bob", "hello bob", None).unwrap();
        send(&repo, &root, "alice", "bob", "second", None).unwrap();

        let first = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].body, "hello bob");
        assert_eq!(first[1].body, "second");

        // Cursor advanced — nothing new on a second read.
        let second = inbox(&repo, &root, "bob", true).unwrap();
        assert!(second.is_empty());

        // A new message after the watermark shows up.
        send(&repo, &root, "alice", "bob", "third", None).unwrap();
        let third = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].body, "third");
    }

    #[test]
    fn peek_does_not_advance_cursor() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "ping", None).unwrap();

        let peek = inbox(&repo, &root, "bob", false).unwrap();
        assert_eq!(peek.len(), 1);
        // Peeking again still shows it.
        let peek2 = inbox(&repo, &root, "bob", false).unwrap();
        assert_eq!(peek2.len(), 1);
        assert_eq!(unread_count(&repo, &root, "bob").unwrap(), 1);
    }

    #[test]
    fn inbox_only_shows_messages_addressed_to_me() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "for bob", None).unwrap();
        send(&repo, &root, "alice", "carol", "for carol", None).unwrap();

        let bob = inbox(&repo, &root, "bob", false).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].to, "bob");
    }

    #[test]
    fn broadcast_reaches_everyone_but_sender() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", BROADCAST, "standup in 5", None).unwrap();

        assert_eq!(inbox(&repo, &root, "bob", false).unwrap().len(), 1);
        assert_eq!(inbox(&repo, &root, "carol", false).unwrap().len(), 1);
        // Sender does not receive their own broadcast.
        assert_eq!(inbox(&repo, &root, "alice", false).unwrap().len(), 0);
    }

    #[test]
    fn history_filters_by_participant_and_limit() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "1", None).unwrap();
        send(&repo, &root, "bob", "alice", "2", None).unwrap();
        send(&repo, &root, "carol", "dave", "3", None).unwrap();

        let all = history(&repo, None, 10).unwrap();
        assert_eq!(all.len(), 3);

        let with_alice = history(&repo, Some("alice"), 10).unwrap();
        assert_eq!(with_alice.len(), 2);

        let limited = history(&repo, None, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].body, "3"); // most recent
    }

    #[test]
    fn roster_records_participants() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "hi", None).unwrap();
        let names: Vec<String> = team(&repo).into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"alice".to_string()));
        assert!(names.contains(&"bob".to_string()));
    }

    #[test]
    fn identity_resolution_prefers_explicit_then_stored() {
        let (_d, _repo, root) = fixture();
        // Don't let an ambient $H5I_AGENT (set by a repo's settings) win over
        // the stored value this test is exercising.
        std::env::remove_var(AGENT_ENV);
        // Explicit persists it.
        assert_eq!(resolve_identity(&root, Some("alice")).unwrap(), "alice");
        // Now stored is used when no explicit value is given.
        assert_eq!(resolve_identity(&root, None).unwrap(), "alice");
    }

    #[test]
    fn identity_resolution_errors_when_unset() {
        let (_d, _repo, root) = fixture();
        // Ensure the env var doesn't leak in from the host.
        std::env::remove_var(AGENT_ENV);
        assert!(resolve_identity(&root, None).is_err());
    }

    #[test]
    fn validate_name_enforces_i5h_charset() {
        assert!(validate_name("alice").is_ok());
        assert!(validate_name("claude-code").is_ok());
        assert!(validate_name("agent.1_x").is_ok());
        assert!(validate_name(BROADCAST).is_ok()); // "all"
        assert!(validate_name("").is_err());
        assert!(validate_name("a b").is_err()); // whitespace
        assert!(validate_name("../etc").is_err()); // path traversal
        assert!(validate_name("a/b").is_err()); // path separator
        assert!(validate_name("a\x1b[31m").is_err()); // ANSI escape
        assert!(validate_name("a\nb").is_err()); // newline
    }

    #[test]
    fn write_identity_rejects_invalid_names() {
        let (_d, _repo, root) = fixture();
        assert!(write_identity(&root, "ok-name").is_ok());
        assert!(write_identity(&root, "bad name").is_err());
        assert!(write_identity(&root, "evil\x1b[2J").is_err());
    }

    #[test]
    fn resolve_identity_rejects_poisoned_stored_value() {
        let (_d, _repo, root) = fixture();
        std::env::remove_var(AGENT_ENV);
        // Simulate a tampered identity file (bypassing write_identity).
        let dir = msg_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(identity_path(&root), "evil\x1b[31mname\n").unwrap();
        assert!(resolve_identity(&root, None).is_err());
    }

    #[test]
    fn sanitize_display_neutralises_escapes_and_newlines() {
        // ESC dropped → the sequence can no longer move the cursor or set colour.
        let s = sanitize_display("\x1b[31mred\x1b[0m");
        assert!(!s.contains('\x1b'));
        assert_eq!(s, "[31mred[0m");
        // Newlines fold to spaces → cannot forge extra dashboard/plain lines.
        assert_eq!(sanitize_display("line1\nline2"), "line1 line2");
        // OSC-8 hyperlink escape is neutralised.
        let osc = sanitize_display("\x1b]8;;http://evil\x07click\x1b]8;;\x07");
        assert!(!osc.contains('\x1b') && !osc.contains('\x07'));
        // Ordinary text is untouched.
        assert_eq!(sanitize_display("hello world"), "hello world");
    }

    #[test]
    fn late_arriving_older_message_is_still_delivered() {
        let (_d, repo, root) = fixture();
        // Read a current message → its id is now in bob's seen-set.
        send(&repo, &root, "alice", "bob", "newest", None).unwrap();
        assert_eq!(inbox(&repo, &root, "bob", true).unwrap().len(), 1);

        // A message pulled from another clone with an OLDER timestamp than what
        // bob already read. A watermark would hide it; the seen-id set must not.
        let late = Message {
            id: "late0001".into(),
            ts: "2020-01-01T00:00:00.000000Z".into(),
            from: "carol".into(),
            to: "bob".into(),
            body: "late but unseen".into(),
            ..Default::default()
        };
        append_message_cas(&repo, &late).unwrap();

        let got = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "late but unseen");
        // And it is not re-delivered.
        assert!(inbox(&repo, &root, "bob", true).unwrap().is_empty());
    }

    #[test]
    fn legacy_watermark_cursor_migrates_to_seen_ids() {
        let (_d, repo, root) = fixture();
        let m1 = send(&repo, &root, "alice", "bob", "one", None).unwrap();
        let _m2 = send(&repo, &root, "alice", "bob", "two", None).unwrap();

        // Hand-write a pre-seen-id cursor.json with a watermark at m1.
        let dir = msg_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = format!(
            r#"{{"cursors":{{"bob":{{"ts":"{}","id":"{}"}}}}}}"#,
            m1.ts, m1.id
        );
        std::fs::write(cursor_path(&root), legacy).unwrap();

        // Migration: m1 (≤ watermark) counts as read; only m2 is unread.
        let unread = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].body, "two");

        // The legacy watermark is gone and a seen-set persisted.
        let raw = std::fs::read_to_string(cursor_path(&root)).unwrap();
        assert!(raw.contains("seen"));
        assert!(!raw.contains("cursors"));
    }

    #[test]
    fn new_sends_carry_version_and_inferred_kind() {
        let (_d, repo, root) = fixture();
        let ask = send(&repo, &root, "alice", "bob", "hi", None).unwrap();
        assert_eq!(ask.version, PROTOCOL_VERSION);
        assert_eq!(ask.kind.as_deref(), Some("ASK"));

        let review = send(&repo, &root, "alice", "bob", "look", Some("review")).unwrap();
        assert_eq!(review.kind.as_deref(), Some("REVIEW_REQUEST"));

        let bcast = send(&repo, &root, "alice", BROADCAST, "fyi", None).unwrap();
        assert_eq!(bcast.kind.as_deref(), Some("FYI"));
    }

    #[test]
    fn legacy_v0_line_deserializes_and_maps_to_a_kind() {
        // A v0 PoC line: no version, no kind, a legacy tag.
        let v0 = r#"{"id":"abc123","ts":"2026-05-28T10:00:00.000000Z","from":"alice","to":"bob","body":"hello","tag":"risk"}"#;
        let msgs = parse_messages(v0);
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.version, 0); // legacy
        assert_eq!(m.kind, None); // not stored
        assert_eq!(m.effective_kind(), "RISK"); // inferred from tag
        assert_eq!(m.body, "hello");

        // v0 with no tag, directed → ASK; broadcast → FYI.
        let bare = r#"{"id":"d1","ts":"2026-05-28T10:00:00.000000Z","from":"a","to":"b","body":"x"}"#;
        assert_eq!(parse_messages(bare)[0].effective_kind(), "ASK");
        let bc = r#"{"id":"d2","ts":"2026-05-28T10:00:00.000000Z","from":"a","to":"all","body":"x"}"#;
        assert_eq!(parse_messages(bc)[0].effective_kind(), "FYI");
    }

    #[test]
    fn v1_message_round_trips_through_jsonl() {
        let m = Message {
            id: "id1".into(),
            ts: "2026-05-28T10:00:00.000000Z".into(),
            from: "claude".into(),
            to: "codex".into(),
            body: "review please".into(),
            kind: Some("REVIEW_REQUEST".into()),
            version: PROTOCOL_VERSION,
            branch: Some("auth-refactor".into()),
            focus: vec!["src/auth.rs".into()],
            priority: Some("high".into()),
            ..Default::default()
        };
        let line = serde_json::to_string(&m).unwrap();
        // Empty optionals are omitted from the wire form.
        assert!(!line.contains("reply_to"));
        assert!(!line.contains("\"tag\""));
        let back: Message = serde_json::from_str(&line).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn infer_kind_passes_through_explicit_kind_names() {
        assert_eq!(infer_kind(Some("done"), "bob"), "DONE");
        assert_eq!(infer_kind(Some("HANDOFF"), "bob"), "HANDOFF");
        // Unknown tag falls back to the recipient default.
        assert_eq!(infer_kind(Some("deploy"), "bob"), "ASK");
        assert_eq!(infer_kind(Some("deploy"), BROADCAST), "FYI");
    }

    #[test]
    fn tag_is_persisted_and_round_trips() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "look here", Some("review")).unwrap();
        let m = &inbox(&repo, &root, "bob", false).unwrap()[0];
        assert_eq!(m.tag.as_deref(), Some("review"));
    }

    #[test]
    fn empty_tag_is_normalised_to_none() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "x", Some("  ")).unwrap();
        let m = &inbox(&repo, &root, "bob", false).unwrap()[0];
        assert_eq!(m.tag, None);
    }

    #[test]
    fn last_view_resolves_numbers_for_the_right_agent() {
        let (_d, repo, root) = fixture();
        let m1 = send(&repo, &root, "alice", "bob", "first", None).unwrap();
        let m2 = send(&repo, &root, "alice", "bob", "second", None).unwrap();

        write_last_view(&root, "bob", &[m1.id.clone(), m2.id.clone()]).unwrap();
        assert_eq!(resolve_view_number(&root, "bob", 1).as_deref(), Some(m1.id.as_str()));
        assert_eq!(resolve_view_number(&root, "bob", 2).as_deref(), Some(m2.id.as_str()));
        // Out of range and wrong-agent both yield None.
        assert_eq!(resolve_view_number(&root, "bob", 3), None);
        assert_eq!(resolve_view_number(&root, "carol", 1), None);
        assert_eq!(resolve_view_number(&root, "bob", 0), None);
    }

    #[test]
    fn get_message_finds_by_id() {
        let (_d, repo, root) = fixture();
        let m = send(&repo, &root, "alice", "bob", "find me", None).unwrap();
        assert_eq!(get_message(&repo, &m.id).unwrap().body, "find me");
        assert!(get_message(&repo, "nope").is_none());
    }

    #[test]
    fn stats_report_total_and_tip() {
        let (_d, repo, root) = fixture();
        assert_eq!(stats(&repo).total, 0);
        assert!(stats(&repo).tip.is_none());
        send(&repo, &root, "alice", "bob", "one", None).unwrap();
        send(&repo, &root, "alice", "bob", "two", None).unwrap();
        let st = stats(&repo);
        assert_eq!(st.total, 2);
        assert!(st.tip.is_some());
        assert!(st.tip_time.is_some());
    }

    #[test]
    fn union_merge_deduplicates_and_orders() {
        let shared = Message {
            id: "shared00".into(),
            ts: "2026-05-28T10:00:00.000000Z".into(),
            from: "alice".into(),
            to: "bob".into(),
            body: "shared".into(),
            tag: None,
            ..Default::default()
        };
        let only_a = Message {
            id: "aaaa0001".into(),
            ts: "2026-05-28T10:00:01.000000Z".into(),
            from: "alice".into(),
            to: "bob".into(),
            body: "from-a".into(),
            tag: None,
            ..Default::default()
        };
        let only_b = Message {
            id: "bbbb0001".into(),
            ts: "2026-05-28T09:59:59.000000Z".into(),
            from: "carol".into(),
            to: "bob".into(),
            body: "from-b".into(),
            tag: None,
            ..Default::default()
        };

        let merged = merge_message_sets(
            vec![shared.clone(), only_a.clone()],
            vec![shared.clone(), only_b.clone()],
        );
        let msgs = parse_messages(&merged);
        // The shared message appears once → 3 total.
        assert_eq!(msgs.len(), 3);
        // Sorted by (ts, id): only_b (09:59:59) < shared (10:00:00) < only_a.
        assert_eq!(msgs[0].body, "from-b");
        assert_eq!(msgs[1].body, "shared");
        assert_eq!(msgs[2].body, "from-a");
    }

    #[test]
    fn union_merge_commits_reconciles_divergent_tips() {
        let (_d, repo, root) = fixture();

        // Common base.
        send(&repo, &root, "alice", "bob", "base", None).unwrap();
        let base = repo.refname_to_id(MSG_REF).unwrap();

        // Local tip: append one message.
        send(&repo, &root, "alice", "bob", "local-only", None).unwrap();
        let local = repo.refname_to_id(MSG_REF).unwrap();

        // Build a divergent "incoming" tip from the base by committing a
        // different extra message directly onto a side ref.
        let base_commit = repo.find_commit(base).unwrap();
        let base_log = read_file_from_commit(&repo, base, MESSAGES_FILE).unwrap();
        let incoming_msg = Message {
            id: "incoming1".into(),
            ts: now_ts(),
            from: "carol".into(),
            to: "bob".into(),
            body: "incoming-only".into(),
            tag: None,
            ..Default::default()
        };
        let incoming_log =
            format!("{}{}\n", base_log, serde_json::to_string(&incoming_msg).unwrap());
        let sig = signature(&repo).unwrap();
        let tree_oid = build_tree(
            &repo,
            base_commit.tree().ok().as_ref(),
            &[(MESSAGES_FILE, &incoming_log)],
        )
        .unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let incoming = repo
            .commit(None, &sig, &sig, "incoming", &tree, &[&base_commit])
            .unwrap();

        // Merge and point the ref at the result.
        let merged = union_merge_commits(&repo, local, incoming).unwrap();
        repo.reference(MSG_REF, merged, true, "test merge").unwrap();

        let bodies: Vec<String> = read_messages(&repo).into_iter().map(|m| m.body).collect();
        assert!(bodies.contains(&"base".to_string()));
        assert!(bodies.contains(&"local-only".to_string()));
        assert!(bodies.contains(&"incoming-only".to_string()));
        assert_eq!(bodies.len(), 3);
    }
}
