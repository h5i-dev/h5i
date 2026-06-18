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
//! on every `inbox`. Instead each agent keeps a **set of seen message ids** in
//! a per-agent local file (`.git/.h5i/msg/cursors/<agent>.json`). `inbox`
//! returns every message addressed to the agent whose id is not yet in that set
//! and (when advancing) adds the delivered ids to it. Because membership is by
//! id rather than a timestamp watermark, a message that arrives via `pull` with
//! an *earlier* timestamp than something already read (clock skew / late
//! delivery) is still delivered exactly once. The legacy single-file
//! `cursor.json` watermark is read once and migrated into the seen-id model.

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
/// identity) when resolving "who am I" for `send` / `inbox`. The canonical
/// definition lives in [`crate::idents`] (dependency-free); re-exported here so
/// existing `msg::AGENT_ENV` callers keep working.
pub use crate::idents::AGENT_ENV;

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

    // Auto-tag the sender's current git branch unless overridden. This is what
    // makes the PR-body coordination section (which selects threads by branch)
    // capture a conversation without every message having to pass `--branch`.
    //   - explicit non-empty branch  → kept as-is
    //   - explicit empty (`--branch ""`) → opt out, stays untagged
    //   - absent → the current branch (or None on a detached/unborn HEAD)
    let branch = match opts.branch {
        Some(b) if b.trim().is_empty() => None,
        Some(b) => Some(b),
        None => current_branch(repo),
    };

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
        branch,
        context_branch: opts.context_branch.filter(|s| !s.is_empty()),
        focus: opts.focus.into_iter().filter(|s| !s.is_empty()).collect(),
        risk: opts.risk.filter(|s| !s.is_empty()),
        deadline: opts.deadline.filter(|s| !s.is_empty()),
        links: opts.links,
        meta: None,
    };

    append_message_cas(repo, &msg)?;
    // Persist the sender as the local default only in a solo clone. In a shared
    // clone this single slot is ambiguous — `resolve_identity` refuses to trust
    // it — so writing it just churns shared state and risks misleading a tool
    // that reads the file directly. `from` is already counted by `known_agents`,
    // so a brand-new solo clone still sees `len() <= 1` here.
    if known_agents(h5i_root).len() <= 1 {
        write_identity(h5i_root, from)?;
    }
    Ok(msg)
}

/// Send a threaded reply to `original`, as `me`.
///
/// Resolves the recipient (the *other* party of `original`), threads via
/// `reply_to` + the thread root, and — crucially — inherits the **thread's**
/// branch rather than letting [`send_msg`] auto-tag the responder's current
/// checkout. A reply's relevance is the thread it belongs to; tagging it with
/// wherever the replier happens to be standing would let, say, an `ACK` typed
/// from a `docs` checkout drag an `auth` thread into the `docs` PR body. The
/// thread's branch is the root message's branch (falling back to the immediate
/// parent's); when the thread is untagged, an empty string opts the reply out
/// of auto-tagging too.
pub fn reply(
    repo: &Repository,
    h5i_root: &Path,
    me: &str,
    original: &Message,
    kind: Option<&str>,
    body: &str,
) -> Result<Message, H5iError> {
    let to = if original.from == me {
        original.to.clone()
    } else {
        original.from.clone()
    };
    let thread_branch = get_message(repo, &original.thread_root())
        .and_then(|root| root.branch)
        .or_else(|| original.branch.clone());
    let opts = SendOpts {
        kind: kind.map(str::to_string),
        reply_to: Some(original.id.clone()),
        thread_id: Some(original.thread_root()),
        // Some("") when untagged → opt out of send_msg's current-branch auto-tag.
        branch: Some(thread_branch.unwrap_or_default()),
        ..Default::default()
    };
    send_msg(repo, h5i_root, me, &to, body, opts)
}

/// The sender's current git branch, or `None` on a detached or unborn HEAD.
///
/// Deliberately uses `repo.head()` (not a HEAD-file fallback): if we aren't on a
/// real branch, a message is left untagged rather than guessing — auto-tagging
/// is a convenience, not something to fabricate.
fn current_branch(repo: &Repository) -> Option<String> {
    repo.head()
        .ok()?
        .shorthand()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Append `msg` to `refs/h5i/msg` with compare-and-swap semantics: build the
/// commit off the current tip, then move the ref only if it still points where
/// we read it. If a concurrent writer moved the tip first, re-read and retry so
/// no append is silently lost (the i5h send contract).
fn append_message_cas(repo: &Repository, msg: &Message) -> Result<(), H5iError> {
    // Each retry is a cheap object write + ref CAS; cap high so bursty
    // concurrency (many agents sending at once into one clone) never spuriously
    // fails. A writer would have to lose this many consecutive races to error.
    const MAX_ATTEMPTS: usize = 64;
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
    let mut addressed: Vec<Message> = read_messages(repo)
        .into_iter()
        .filter(|m| m.addressed_to(me))
        .collect();
    addressed.sort_by(|a, b| a.key().cmp(&b.key()));

    // Read-state lives in a PER-AGENT file (cursors/<agent>.json) so two agents
    // sharing one clone never write the same file. If the per-agent file is
    // absent, seed it from the legacy shared cursor.json (one-time migration).
    let (mut seen, mut dirty) = match read_agent_seen(h5i_root, me) {
        Some(set) => (set, false),
        None => (migrate_legacy_seen(h5i_root, me, &addressed), true),
    };

    let unread: Vec<Message> = addressed
        .into_iter()
        .filter(|m| !seen.contains(&m.id))
        .collect();

    if advance && !unread.is_empty() {
        for m in &unread {
            seen.insert(m.id.clone());
        }
        dirty = true;
    }
    if dirty {
        write_agent_seen(h5i_root, me, &seen)?;
    }
    Ok(unread)
}

/// Commit read-state for `me` by adding `ids` to its seen-set, without reading
/// the log. This is the **acknowledge** half of a deliver-then-ack handoff:
/// callers `inbox(.., advance=false)` to peek, render the messages, and only
/// then call `mark_seen` — so a dropped or failed render never silently
/// consumes mail. Idempotent; a no-op when `ids` are already seen.
pub fn mark_seen(h5i_root: &Path, me: &str, ids: &[String]) -> Result<(), H5iError> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut seen = read_agent_seen(h5i_root, me).unwrap_or_default();
    let mut dirty = false;
    for id in ids {
        dirty |= seen.insert(id.clone());
    }
    if dirty {
        write_agent_seen(h5i_root, me, &seen)?;
    }
    Ok(())
}

/// Seed a fresh per-agent seen-set from the legacy shared `cursor.json`
/// (the pre-per-agent format): copy `me`'s seen ids, and convert a legacy
/// watermark to ids by marking everything at or below it as seen.
fn migrate_legacy_seen(h5i_root: &Path, me: &str, addressed: &[Message]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let legacy = read_cursors(h5i_root).unwrap_or_default();
    if let Some(s) = legacy.seen.get(me) {
        set.extend(s.iter().cloned());
    }
    if let Some(wm) = legacy.cursors.get(me) {
        for m in addressed {
            if m.key() <= wm.key() {
                set.insert(m.id.clone());
            }
        }
    }
    set
}

/// Return up to `limit` most-recent messages (oldest-first within the window).
/// When `with` is set, restrict to messages where that agent is the sender or
/// recipient (a conversation view). When `branch` is set, restrict to the
/// conversation tied to that git branch using thread-closure semantics: a
/// thread qualifies iff at least one of its messages is tagged
/// `branch == Some(branch)` (exact match), and the *entire* thread is then kept
/// — including replies that omitted the branch field — so an `ACK`/`DONE` that
/// only set `reply_to` still travels with its `REVIEW_REQUEST`. This mirrors the
/// rule used by [`threads_for_branch`] for PR bodies. `with` and `branch`
/// compose (both filters apply).
pub fn history(
    repo: &Repository,
    with: Option<&str>,
    branch: Option<&str>,
    limit: usize,
) -> Result<Vec<Message>, H5iError> {
    let all_msgs = read_messages(repo);

    // Thread roots that have at least one message explicitly tagged with the
    // requested branch. Computed once over the full set so the closure picks up
    // untagged replies in the same thread.
    let branch_roots: Option<std::collections::HashSet<String>> = branch.map(|b| {
        all_msgs
            .iter()
            .filter(|m| m.branch.as_deref() == Some(b))
            .map(|m| m.thread_root())
            .collect()
    });

    let mut all: Vec<Message> = all_msgs
        .into_iter()
        .filter(|m| match with {
            Some(w) => m.from == w || m.to == w,
            None => true,
        })
        .filter(|m| match &branch_roots {
            Some(roots) => roots.contains(&m.thread_root()),
            None => true,
        })
        .collect();
    all.sort_by(|a, b| a.key().cmp(&b.key()));
    if all.len() > limit {
        all = all.split_off(all.len() - limit);
    }
    Ok(all)
}

/// One coordination thread selected for a PR body: every message sharing a
/// `thread_root`, sorted in canonical `(ts, id)` order.
#[derive(Debug, Clone)]
pub struct PrThread {
    /// Stable thread-root id.
    pub thread_id: String,
    /// Messages in the thread, oldest-first.
    pub messages: Vec<Message>,
    /// Timestamp of the most recent message — the thread's sort key.
    pub latest_ts: String,
}

/// Select the message threads relevant to a PR for `branch`, newest thread
/// first, capped at `max_threads`.
///
/// A thread qualifies iff at least one of its messages carries
/// `branch == Some(branch)` (exact match). The *entire* thread is then returned
/// — including replies that omitted the branch field — so an `ACK`/`DONE` that
/// only set `reply_to` still travels with its `REVIEW_REQUEST`. This is the
/// "exact branch match + thread closure" rule agreed for the PR body.
///
/// Returns `(threads, total_qualifying)` so the caller can render an elision
/// note when `total_qualifying > threads.len()`.
pub fn threads_for_branch(
    repo: &Repository,
    branch: &str,
    max_threads: usize,
) -> (Vec<PrThread>, usize) {
    let all = read_messages(repo);

    // Roots that have at least one message explicitly tagged with this branch.
    let mut matched_roots: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &all {
        if m.branch.as_deref() == Some(branch) {
            matched_roots.insert(m.thread_root());
        }
    }

    // Bucket every message under its thread_root, keeping only matched roots.
    let mut by_root: HashMap<String, Vec<Message>> = HashMap::new();
    for m in all {
        let root = m.thread_root();
        if matched_roots.contains(&root) {
            by_root.entry(root).or_default().push(m);
        }
    }

    let mut threads: Vec<PrThread> = by_root
        .into_iter()
        .map(|(thread_id, mut messages)| {
            messages.sort_by(|a, b| a.key().cmp(&b.key()));
            let latest_ts = messages
                .last()
                .map(|m| m.ts.clone())
                .unwrap_or_default();
            PrThread { thread_id, messages, latest_ts }
        })
        .collect();

    // Newest thread first; tie-break on thread_id for a stable order.
    threads.sort_by(|a, b| {
        b.latest_ts
            .cmp(&a.latest_ts)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
    });

    let total = threads.len();
    threads.truncate(max_threads);
    (threads, total)
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
    let dir = views_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let json = serde_json::to_string_pretty(&serde_json::json!({ "ids": ids }))?;
    let path = agent_view_path(h5i_root, agent);
    // Atomic replace so `reply <n>` never reads a half-written view file.
    atomic_write(&path, json.as_bytes()).map_err(|e| H5iError::with_path(e, path))
}

/// Resolve a 1-based number from `agent`'s last numbered view into a message
/// id. The view is a per-agent file, so it can't be clobbered by another agent
/// in the same clone. Returns `None` when there is no view or `n` is out of range.
pub fn resolve_view_number(h5i_root: &Path, agent: &str, n: usize) -> Option<String> {
    let raw = std::fs::read_to_string(agent_view_path(h5i_root, agent)).ok()?;
    let view: ViewFile = serde_json::from_str(&raw).ok()?;
    if n == 0 || n > view.ids.len() {
        return None;
    }
    Some(view.ids[n - 1].clone())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ViewFile {
    #[serde(default)]
    ids: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SeenFile {
    #[serde(default)]
    seen: BTreeSet<String>,
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
        // Safe-by-default: in a clone shared by more than one agent the single
        // stored-identity file is an ambiguous slot — the last `msg as`/sender
        // wins it (see `send_msg`), so silently trusting it would attribute
        // messages to whoever wrote it last. Refuse instead and make the caller
        // be explicit. Solo clones keep the convenient fallback.
        let agents = known_agents(h5i_root);
        if agents.len() > 1 {
            return Err(H5iError::Metadata(format!(
                "ambiguous agent identity in a shared clone ({}): the stored \
                 default '{stored}' is not trustworthy here — set $H5I_AGENT or \
                 pass --from <name>/--as <name>",
                agents.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
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

/// Agents known to share this clone, discovered from per-agent read-state files
/// (`msg/cursors/<agent>.json`, `msg/views/<agent>.json`) plus the stored
/// default identity. Used to detect a multi-agent clone, where the single
/// stored-identity slot is ambiguous and must not be trusted as a silent
/// identity fallback (see [`resolve_identity`]).
fn known_agents(h5i_root: &Path) -> BTreeSet<String> {
    let mut agents = BTreeSet::new();
    for dir in [cursors_dir(h5i_root), views_dir(h5i_root)] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if validate_name(stem).is_ok() {
                    agents.insert(stem.to_string());
                }
            }
        }
    }
    if let Some(stored) = read_identity(h5i_root) {
        agents.insert(stored);
    }
    agents
}

/// Idempotently merge the messaging wiring into a Claude Code `settings.json`
/// document: set `env.H5I_AGENT = agent` and ensure exactly one turn-delivery
/// Stop hook (`h5i msg hook`, or `--block`). Existing env keys and other hooks
/// are preserved. `existing` may be empty (treated as `{}`). Pure (no I/O) so
/// it is unit-testable; the caller does the file read/write.
///
/// The hook intentionally has no `--as` — identity comes from `env.H5I_AGENT`,
/// so it stays correct even when several agents share one clone.
pub fn merge_settings_json(existing: &str, agent: &str, block: bool) -> Result<String, H5iError> {
    use serde_json::{Map, Value};
    validate_name(agent)?;

    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing)?
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings.json is not a JSON object".into()))?;

    // env.H5I_AGENT = agent
    let env = obj
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    env.as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings 'env' is not an object".into()))?
        .insert("H5I_AGENT".to_string(), Value::String(agent.to_string()));

    // hooks.Stop: drop any prior h5i-msg entry, then add ours (idempotent).
    let cmd = if block { "h5i msg hook --block" } else { "h5i msg hook" };
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| H5iError::Metadata("settings 'hooks' is not an object".into()))?;
    let stop = hooks_obj
        .entry("Stop")
        .or_insert_with(|| Value::Array(Vec::new()));
    let stop_arr = stop
        .as_array_mut()
        .ok_or_else(|| H5iError::Metadata("settings hooks.Stop is not an array".into()))?;
    stop_arr.retain(|entry| !is_msg_hook_entry(entry));
    stop_arr.push(serde_json::json!({
        "hooks": [ { "type": "command", "command": cmd } ]
    }));

    Ok(serde_json::to_string_pretty(&root)?)
}

/// True if a hooks-array entry contains an inner `h5i msg hook` command.
fn is_msg_hook_entry(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|hk| {
                hk.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.trim_start().starts_with("h5i msg hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
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

/// Legacy single-file cursor (pre-per-agent). Read only, for migration.
fn cursor_path(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("cursor.json")
}

fn cursors_dir(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("cursors")
}

fn agent_cursor_path(h5i_root: &Path, agent: &str) -> PathBuf {
    cursors_dir(h5i_root).join(format!("{agent}.json"))
}

fn views_dir(h5i_root: &Path) -> PathBuf {
    msg_dir(h5i_root).join("views")
}

fn agent_view_path(h5i_root: &Path, agent: &str) -> PathBuf {
    views_dir(h5i_root).join(format!("{agent}.json"))
}

/// Read `agent`'s per-agent seen-set, or `None` if it has none yet.
fn read_agent_seen(h5i_root: &Path, agent: &str) -> Option<BTreeSet<String>> {
    let raw = std::fs::read_to_string(agent_cursor_path(h5i_root, agent)).ok()?;
    let f: SeenFile = serde_json::from_str(&raw).ok()?;
    Some(f.seen)
}

fn write_agent_seen(
    h5i_root: &Path,
    agent: &str,
    seen: &BTreeSet<String>,
) -> Result<(), H5iError> {
    let dir = cursors_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = agent_cursor_path(h5i_root, agent);
    // The seen-set is grow-only, so re-read the current on-disk set and UNION
    // before writing. Multiple processes can act as the same identity in one
    // clone (an agent's Stop hook + its own `inbox` calls), and a plain
    // last-writer-wins overwrite would drop a concurrent writer's additions —
    // re-delivering already-read mail. Union makes the merge commutative.
    let mut merged = read_agent_seen(h5i_root, agent).unwrap_or_default();
    merged.extend(seen.iter().cloned());
    let json = serde_json::to_string_pretty(&serde_json::json!({ "seen": merged }))?;
    atomic_write(&path, json.as_bytes()).map_err(|e| H5iError::with_path(e, path))
}

/// Write `bytes` to `path` atomically: write to a unique temp file in the same
/// directory, then rename over the target. On POSIX the rename is atomic, so a
/// concurrent reader never observes a half-written file (a truncated cursor file
/// would fail to parse and silently reset read-state, re-delivering everything).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp); // don't leak the temp on failure
            Err(e)
        }
    }
}

/// Read the legacy shared cursor.json (for one-time migration only).
fn read_cursors(h5i_root: &Path) -> Result<CursorStore, H5iError> {
    let path = cursor_path(h5i_root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CursorStore::default()),
        Err(e) => Err(H5iError::with_path(e, path)),
    }
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
    fn peek_then_mark_seen_equals_advance() {
        // Deliver-then-ack: peeking (advance=false) then mark_seen consumes the
        // message exactly once, just like inbox(advance=true) — but only after
        // the caller has surfaced it. A peek that is never acked is NOT consumed,
        // which is what prevents `watch` and dropped renders from losing mail.
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "hi", None).unwrap();

        // Peek without ack: still unread (this is the watch / dropped-render case).
        assert_eq!(inbox(&repo, &root, "bob", false).unwrap().len(), 1);
        assert_eq!(inbox(&repo, &root, "bob", false).unwrap().len(), 1);

        // Surface, then ack.
        let peeked = inbox(&repo, &root, "bob", false).unwrap();
        let ids: Vec<String> = peeked.iter().map(|m| m.id.clone()).collect();
        mark_seen(&root, "bob", &ids).unwrap();

        // Now consumed — nothing left, and mark_seen is idempotent.
        assert_eq!(unread_count(&repo, &root, "bob").unwrap(), 0);
        mark_seen(&root, "bob", &ids).unwrap();
        assert_eq!(unread_count(&repo, &root, "bob").unwrap(), 0);
    }

    #[test]
    fn write_agent_seen_unions_with_disk() {
        // Two processes acting as the same identity: A persists {a}; B started
        // from an empty view and persists {b}. A naive overwrite would drop {a};
        // the grow-only union-on-write must keep both (no re-delivery of read mail).
        let (_d, _repo, root) = fixture();
        let mut a = BTreeSet::new();
        a.insert("a".to_string());
        write_agent_seen(&root, "bob", &a).unwrap();

        let mut b = BTreeSet::new();
        b.insert("b".to_string());
        write_agent_seen(&root, "bob", &b).unwrap();

        let seen = read_agent_seen(&root, "bob").unwrap();
        assert!(seen.contains("a"), "concurrent writer's id must survive");
        assert!(seen.contains("b"));
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

        let all = history(&repo, None, None, 10).unwrap();
        assert_eq!(all.len(), 3);

        let with_alice = history(&repo, Some("alice"), None, 10).unwrap();
        assert_eq!(with_alice.len(), 2);

        let limited = history(&repo, None, None, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].body, "3"); // most recent
    }

    #[test]
    fn history_filters_by_branch_with_thread_closure() {
        let (_d, repo, root) = fixture();

        // Thread on `feature-x`, with a reply that omits the branch tag.
        let a_root = send_on(&repo, &root, "codex", "claude", "review feature-x", Some("feature-x"), None);
        send_on(&repo, &root, "claude", "codex", "done", None, Some(&a_root.id));
        // A different branch and an untagged message — both excluded.
        send_on(&repo, &root, "codex", "claude", "review other", Some("other-branch"), None);
        send_on(&repo, &root, "claude", "codex", "fyi", None, None);

        // Branch filter keeps the whole thread (the untagged reply rides along).
        let on_branch = history(&repo, None, Some("feature-x"), 10).unwrap();
        assert_eq!(on_branch.len(), 2);
        assert_eq!(on_branch[0].body, "review feature-x");
        assert_eq!(on_branch[1].body, "done");

        // No match → empty.
        assert!(history(&repo, None, Some("nonexistent"), 10).unwrap().is_empty());

        // `with` and `branch` compose: restrict the branch thread to one agent.
        let scoped = history(&repo, Some("codex"), Some("feature-x"), 10).unwrap();
        assert_eq!(scoped.len(), 2, "both messages involve codex");
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
    fn merge_settings_adds_env_and_hook_to_empty() {
        let out = merge_settings_json("", "claude", false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["env"]["H5I_AGENT"], "claude");
        let stop = &v["hooks"]["Stop"];
        assert_eq!(stop[0]["hooks"][0]["command"], "h5i msg hook");
    }

    #[test]
    fn merge_settings_preserves_existing_and_is_idempotent() {
        // Existing settings with an unrelated env key and pre-existing,
        // non-h5i user hooks that the merge must preserve untouched.
        let existing = r#"{
            "env": { "EDITOR": "vim" },
            "hooks": {
                "PostToolUse": [ { "hooks": [ { "type": "command", "command": "my-linter --fix" } ] } ],
                "Stop": [ { "hooks": [ { "type": "command", "command": "notify-send done" } ] } ]
            }
        }"#;
        let once = merge_settings_json(existing, "claude", false).unwrap();
        let twice = merge_settings_json(&once, "claude", false).unwrap();
        // Idempotent: second run yields the same document.
        assert_eq!(once, twice);

        let v: serde_json::Value = serde_json::from_str(&twice).unwrap();
        // Preserved unrelated state.
        assert_eq!(v["env"]["EDITOR"], "vim");
        assert_eq!(v["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "my-linter --fix");
        // Stop keeps the user's hook AND has exactly one msg hook.
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        let msg_hooks = stop
            .iter()
            .filter(|e| is_msg_hook_entry(e))
            .count();
        assert_eq!(msg_hooks, 1, "exactly one msg hook entry");
        assert!(stop.iter().any(|e| e["hooks"][0]["command"] == "notify-send done"));
        assert_eq!(v["env"]["H5I_AGENT"], "claude");
    }

    #[test]
    fn merge_settings_block_flag_and_validation() {
        let out = merge_settings_json("{}", "codex", true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "h5i msg hook --block");
        assert_eq!(v["env"]["H5I_AGENT"], "codex");
        // Bad identity rejected; non-object document rejected.
        assert!(merge_settings_json("", "bad name", false).is_err());
        assert!(merge_settings_json("[1,2]", "claude", false).is_err());
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

        // A per-agent seen file was written (the legacy file is left untouched).
        let raw = std::fs::read_to_string(agent_cursor_path(&root, "bob")).unwrap();
        assert!(raw.contains("seen"));
        assert!(raw.contains(&m1.id), "migrated watermark should mark m1 seen");
        // bob's seen state is isolated — carol has no per-agent file at all.
        assert!(read_agent_seen(&root, "carol").is_none());
    }

    #[test]
    fn read_state_is_isolated_per_agent_in_one_clone() {
        // Two agents sharing one clone must not clobber each other's read-state.
        let (_d, repo, root) = fixture();
        send(&repo, &root, "x", "claude", "to claude", None).unwrap();
        send(&repo, &root, "x", "codex", "to codex", None).unwrap();

        // claude consumes its inbox; codex's stays untouched.
        assert_eq!(inbox(&repo, &root, "claude", true).unwrap().len(), 1);
        assert_eq!(inbox(&repo, &root, "codex", false).unwrap().len(), 1);

        // Per-agent view files don't collide: each reply targets its own view.

        write_last_view(&root, "claude", &["a".into()]).unwrap();
        write_last_view(&root, "codex", &["b".into()]).unwrap();
        assert_eq!(resolve_view_number(&root, "claude", 1).as_deref(), Some("a"));
        assert_eq!(resolve_view_number(&root, "codex", 1).as_deref(), Some("b"));
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

    /// Build a message with a distinct (ts, id) so `key()` is a total order.
    /// Same id ⇒ same content, mirroring the append-only-log invariant the
    /// union-merge relies on for convergence.
    fn pm(id: &str, secs: u32, body: &str) -> Message {
        Message {
            id: id.into(),
            ts: format!("2026-05-28T10:00:{secs:02}.000000Z"),
            from: "alice".into(),
            to: "bob".into(),
            body: body.into(),
            tag: None,
            ..Default::default()
        }
    }

    /// `merge_message_sets` is a CRDT join: the merged jsonl is a canonical
    /// (deduped, totally-ordered) function of the *set* of message ids, so
    /// `h5i pull` must converge no matter which side a peer saw first.
    ///
    /// Commutative — order of the two sides cannot change the byte output.
    #[test]
    fn union_merge_is_commutative() {
        let s = pm("shared00", 0, "shared");
        let a = pm("aaaa0001", 1, "from-a");
        let b = pm("bbbb0001", 2, "from-b");
        let ab = merge_message_sets(
            vec![s.clone(), a.clone()],
            vec![s.clone(), b.clone()],
        );
        let ba = merge_message_sets(vec![s.clone(), b], vec![s, a]);
        assert_eq!(ab, ba, "merge(a,b) must equal merge(b,a) byte-for-byte");
    }

    /// Idempotent — re-merging an already-merged set (or a set with itself)
    /// adds nothing. A pull that re-delivers seen messages is a no-op.
    #[test]
    fn union_merge_is_idempotent() {
        let a = pm("aaaa0001", 1, "from-a");
        let b = pm("bbbb0001", 2, "from-b");
        let once = merge_message_sets(vec![a.clone(), b.clone()], vec![]);
        let twice = merge_message_sets(parse_messages(&once), parse_messages(&once));
        assert_eq!(once, twice, "merging a canonical set with itself is a no-op");
        // …and merging with a subset of itself changes nothing either.
        let with_subset = merge_message_sets(parse_messages(&once), vec![a]);
        assert_eq!(once, with_subset);
    }

    /// Associative — pairwise merge order is irrelevant, so three peers
    /// reconciling in any pairing reach the same state. Canonical output means
    /// we can assert the strings are equal, not just the sets.
    #[test]
    fn union_merge_is_associative() {
        let a = pm("aaaa0001", 1, "from-a");
        let b = pm("bbbb0001", 2, "from-b");
        let c = pm("cccc0001", 3, "from-c");
        // (a ∪ b) ∪ c
        let ab = merge_message_sets(vec![a.clone()], vec![b.clone()]);
        let left = merge_message_sets(parse_messages(&ab), vec![c.clone()]);
        // a ∪ (b ∪ c)
        let bc = merge_message_sets(vec![b], vec![c]);
        let right = merge_message_sets(vec![a], parse_messages(&bc));
        assert_eq!(left, right, "(a∪b)∪c must equal a∪(b∪c)");
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

    #[test]
    fn identity_resolution_refuses_shared_stored_in_multi_agent_clone() {
        let (_d, _repo, root) = fixture();
        std::env::remove_var(AGENT_ENV);

        // Solo clone: the stored default is the only known agent, so the
        // convenient fallback is allowed.
        write_identity(&root, "codex").unwrap();
        assert_eq!(resolve_identity(&root, None).unwrap(), "codex");

        // A second agent's per-agent read-state file makes this a shared clone.
        let cursors = cursors_dir(&root);
        std::fs::create_dir_all(&cursors).unwrap();
        std::fs::write(cursors.join("claude.json"), "{}").unwrap();

        // The ambiguous shared slot must no longer be silently trusted...
        assert!(resolve_identity(&root, None).is_err());
        // ...but an explicit identity (and $H5I_AGENT, checked elsewhere) still
        // resolves cleanly.
        assert_eq!(resolve_identity(&root, Some("claude")).unwrap(), "claude");
    }

    fn send_on(
        repo: &Repository,
        root: &Path,
        from: &str,
        to: &str,
        body: &str,
        branch: Option<&str>,
        reply_to: Option<&str>,
    ) -> Message {
        send_msg(
            repo,
            root,
            from,
            to,
            body,
            SendOpts {
                kind: Some("ASK".into()),
                branch: branch.map(str::to_string),
                reply_to: reply_to.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn threads_for_branch_filters_and_includes_full_thread() {
        let (_d, repo, root) = fixture();

        // Thread A: rooted on `feature-x`, with a reply that omits the branch.
        let a_root = send_on(&repo, &root, "codex", "claude", "review feature-x", Some("feature-x"), None);
        let _a_reply = send_on(&repo, &root, "claude", "codex", "done", None, Some(&a_root.id));

        // Thread B: a different branch — must be excluded entirely.
        let _b = send_on(&repo, &root, "codex", "claude", "review other", Some("other-branch"), None);

        // A branch-less broadcast — excluded (no matching branch tag).
        let _c = send_on(&repo, &root, "claude", "codex", "fyi", None, None);

        let (threads, total) = threads_for_branch(&repo, "feature-x", 12);
        assert_eq!(total, 1, "only thread A qualifies");
        assert_eq!(threads.len(), 1);
        // Thread closure: the branch-less reply rides along with its root.
        assert_eq!(threads[0].messages.len(), 2);
        assert_eq!(threads[0].messages[0].body, "review feature-x");
        assert_eq!(threads[0].messages[1].body, "done");
    }

    #[test]
    fn threads_for_branch_caps_and_reports_total() {
        let (_d, repo, root) = fixture();
        for i in 0..5 {
            send_on(&repo, &root, "codex", "claude", &format!("msg {i}"), Some("b"), None);
        }
        let (threads, total) = threads_for_branch(&repo, "b", 3);
        assert_eq!(total, 5, "total reflects all qualifying threads");
        assert_eq!(threads.len(), 3, "capped at max_threads");
    }

    #[test]
    fn threads_for_branch_empty_when_no_match() {
        let (_d, repo, root) = fixture();
        send_on(&repo, &root, "codex", "claude", "hi", Some("main"), None);
        let (threads, total) = threads_for_branch(&repo, "nonexistent", 12);
        assert!(threads.is_empty());
        assert_eq!(total, 0);
    }

    /// Put the fixture repo on a real (born) branch with one commit, so
    /// `repo.head().shorthand()` resolves — needed to exercise auto-tagging.
    fn commit_on_branch(repo: &Repository, branch: &str) {
        repo.set_head(&format!("refs/heads/{branch}")).unwrap();
        let sig = repo.signature().unwrap();
        let tree = {
            let mut idx = repo.index().unwrap();
            repo.find_tree(idx.write_tree().unwrap()).unwrap()
        };
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }

    #[test]
    fn send_auto_tags_current_branch() {
        let (_d, repo, root) = fixture();
        commit_on_branch(&repo, "feature-x");
        // Plain send (no branch opt) must inherit the sender's current branch.
        let m = send(&repo, &root, "alice", "bob", "hi", None).unwrap();
        assert_eq!(m.branch.as_deref(), Some("feature-x"));
        // And it's selectable for that branch's PR body.
        let (threads, total) = threads_for_branch(&repo, "feature-x", 12);
        assert_eq!(total, 1);
        assert_eq!(threads.len(), 1);
    }

    #[test]
    fn explicit_branch_overrides_auto_tag() {
        let (_d, repo, root) = fixture();
        commit_on_branch(&repo, "feature-x");
        let m = send_msg(
            &repo,
            &root,
            "alice",
            "bob",
            "hi",
            SendOpts { branch: Some("other".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(m.branch.as_deref(), Some("other"));
    }

    #[test]
    fn empty_branch_opts_out_of_auto_tag() {
        let (_d, repo, root) = fixture();
        commit_on_branch(&repo, "feature-x");
        let m = send_msg(
            &repo,
            &root,
            "alice",
            "bob",
            "hi",
            SendOpts { branch: Some("".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(m.branch, None, "explicit empty branch must opt out");
    }

    #[test]
    fn reply_inherits_thread_branch_not_responder_checkout() {
        // Regression (Codex blocker): a reply must take its thread's branch, not
        // wherever the responder is checked out — else replying to an `auth`
        // thread from a `docs` checkout would drag the whole `auth` thread into
        // the `docs` PR body.
        let (_d, repo, root) = fixture();
        commit_on_branch(&repo, "docs"); // responder's current checkout
        let auth_root = send_msg(
            &repo,
            &root,
            "alice",
            "bob",
            "auth work",
            SendOpts { branch: Some("auth".into()), ..Default::default() },
        )
        .unwrap();

        let r = reply(&repo, &root, "bob", &auth_root, Some("ACK"), "looking").unwrap();
        assert_eq!(r.to, "alice", "reply goes back to the other party");
        assert_eq!(
            r.branch.as_deref(),
            Some("auth"),
            "reply inherits the thread branch, not the docs checkout"
        );

        // The docs PR must NOT pick up the auth thread...
        let (docs_threads, _) = threads_for_branch(&repo, "docs", 12);
        assert!(docs_threads.is_empty(), "auth thread leaked into docs: {docs_threads:?}");
        // ...while the auth PR shows the full thread (root + reply).
        let (auth_threads, _) = threads_for_branch(&repo, "auth", 12);
        assert_eq!(auth_threads.len(), 1);
        assert_eq!(auth_threads[0].messages.len(), 2);
    }

    #[test]
    fn reply_to_untagged_thread_stays_untagged() {
        // Replying (from a real branch) to an untagged thread must not suddenly
        // tag the reply with the checkout — the thread has no branch relevance.
        let (_d, repo, root) = fixture();
        commit_on_branch(&repo, "docs");
        let untagged = send_msg(
            &repo,
            &root,
            "alice",
            "bob",
            "general note",
            SendOpts { branch: Some("".into()), ..Default::default() }, // opt out
        )
        .unwrap();
        assert_eq!(untagged.branch, None);
        let r = reply(&repo, &root, "bob", &untagged, None, "ok").unwrap();
        assert_eq!(r.branch, None, "reply to an untagged thread stays untagged");
    }

    #[test]
    fn unborn_head_leaves_branch_untagged() {
        // The default fixture has no commit (unborn HEAD): auto-tag must yield
        // None rather than fabricating a branch name from the HEAD file.
        let (_d, repo, root) = fixture();
        let m = send(&repo, &root, "alice", "bob", "hi", None).unwrap();
        assert_eq!(m.branch, None);
    }
}
