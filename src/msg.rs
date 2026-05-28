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

use std::collections::{BTreeMap, HashMap};
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

/// One message in the shared log. Lines in `messages.jsonl` are exactly the
/// JSON serialization of this struct.
///
/// The total order over messages is `(ts, id)`: `ts` is a fixed-width RFC3339
/// UTC timestamp (microsecond precision) so it sorts lexicographically, and
/// `id` breaks ties deterministically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Message body (free text).
    pub body: String,
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
}

/// Roster of known agents, persisted as `agents.json`. Maps an agent name to
/// the timestamp it was last seen sending or receiving.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Roster {
    #[serde(default)]
    agents: BTreeMap<String, String>,
}

/// A read watermark: the `(ts, id)` of the last message an agent consumed.
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
#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorStore {
    #[serde(default)]
    cursors: BTreeMap<String, Watermark>,
}

// ─────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────

/// Append a message from `from` to `to` and update the roster. Persists
/// `from` as the local default identity so later `inbox` calls can omit it.
pub fn send(
    repo: &Repository,
    h5i_root: &Path,
    from: &str,
    to: &str,
    body: &str,
) -> Result<Message, H5iError> {
    validate_name(from)?;
    validate_name(to)?;

    let ts = now_ts();
    let id = gen_id(&ts, from, to, body);
    let msg = Message {
        id,
        ts,
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
    };

    // Append one line to the log. Read the current blob fresh so we extend the
    // latest tip rather than clobbering it.
    let mut log = read_blob(repo, MESSAGES_FILE).unwrap_or_default();
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    log.push_str(&serde_json::to_string(&msg)?);
    log.push('\n');

    // Update the roster: sender is definitely active now; a non-broadcast
    // recipient is recorded too (but never overwrites a later last-seen).
    let mut roster = read_roster(repo);
    roster.agents.insert(from.to_string(), msg.ts.clone());
    if to != BROADCAST {
        roster
            .agents
            .entry(to.to_string())
            .or_insert_with(|| msg.ts.clone());
    }
    let roster_json = serde_json::to_string_pretty(&roster)?;

    write_ref_files(
        repo,
        &[(MESSAGES_FILE, &log), (AGENTS_FILE, &roster_json)],
        &format!("h5i msg: {from} → {to}"),
    )?;

    write_identity(h5i_root, from)?;
    Ok(msg)
}

/// Return the messages addressed to `me` that are newer than `me`'s watermark,
/// sorted oldest-first. When `advance` is true the watermark is moved past the
/// returned set (so the next call won't repeat them); pass `false` to peek
/// without consuming.
pub fn inbox(
    repo: &Repository,
    h5i_root: &Path,
    me: &str,
    advance: bool,
) -> Result<Vec<Message>, H5iError> {
    let mut store = read_cursors(h5i_root)?;
    let watermark = store.cursors.get(me).map(Watermark::key).map(|(t, i)| (t.to_string(), i.to_string()));

    let mut unread: Vec<Message> = read_messages(repo)
        .into_iter()
        .filter(|m| m.addressed_to(me))
        .filter(|m| match &watermark {
            Some((wt, wi)) => m.key() > (wt.as_str(), wi.as_str()),
            None => true,
        })
        .collect();
    unread.sort_by(|a, b| a.key().cmp(&b.key()));

    if advance {
        if let Some(last) = unread.last() {
            store.cursors.insert(
                me.to_string(),
                Watermark {
                    ts: last.ts.clone(),
                    id: last.id.clone(),
                },
            );
            write_cursors(h5i_root, &store)?;
        }
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
            return Ok(env.to_string());
        }
    }
    if let Some(stored) = read_identity(h5i_root) {
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

/// Persist the local default identity.
pub fn write_identity(h5i_root: &Path, name: &str) -> Result<(), H5iError> {
    let dir = msg_dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let path = identity_path(h5i_root);
    std::fs::write(&path, format!("{name}\n")).map_err(|e| H5iError::with_path(e, path))
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

/// Commit `files` onto `refs/h5i/msg`, creating the orphan branch on first use
/// and otherwise appending a commit whose parent is the current tip.
fn write_ref_files(
    repo: &Repository,
    files: &[(&str, &str)],
    message: &str,
) -> Result<(), H5iError> {
    let sig = signature(repo)?;
    let parent = repo
        .find_reference(MSG_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

    let tree_oid = build_tree(repo, base_tree.as_ref(), files)?;
    let tree = repo.find_tree(tree_oid)?;

    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some(MSG_REF), &sig, &sig, message, &tree, &parents)?;
    Ok(())
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

/// Reject names that would break the `from → to` model or the roster keys.
fn validate_name(name: &str) -> Result<(), H5iError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(H5iError::Metadata("agent name must not be empty".into()));
    }
    if n.contains(char::is_whitespace) {
        return Err(H5iError::Metadata(format!(
            "agent name must not contain whitespace: {name:?}"
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

        send(&repo, &root, "alice", "bob", "hello bob").unwrap();
        send(&repo, &root, "alice", "bob", "second").unwrap();

        let first = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].body, "hello bob");
        assert_eq!(first[1].body, "second");

        // Cursor advanced — nothing new on a second read.
        let second = inbox(&repo, &root, "bob", true).unwrap();
        assert!(second.is_empty());

        // A new message after the watermark shows up.
        send(&repo, &root, "alice", "bob", "third").unwrap();
        let third = inbox(&repo, &root, "bob", true).unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].body, "third");
    }

    #[test]
    fn peek_does_not_advance_cursor() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "ping").unwrap();

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
        send(&repo, &root, "alice", "bob", "for bob").unwrap();
        send(&repo, &root, "alice", "carol", "for carol").unwrap();

        let bob = inbox(&repo, &root, "bob", false).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].to, "bob");
    }

    #[test]
    fn broadcast_reaches_everyone_but_sender() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", BROADCAST, "standup in 5").unwrap();

        assert_eq!(inbox(&repo, &root, "bob", false).unwrap().len(), 1);
        assert_eq!(inbox(&repo, &root, "carol", false).unwrap().len(), 1);
        // Sender does not receive their own broadcast.
        assert_eq!(inbox(&repo, &root, "alice", false).unwrap().len(), 0);
    }

    #[test]
    fn history_filters_by_participant_and_limit() {
        let (_d, repo, root) = fixture();
        send(&repo, &root, "alice", "bob", "1").unwrap();
        send(&repo, &root, "bob", "alice", "2").unwrap();
        send(&repo, &root, "carol", "dave", "3").unwrap();

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
        send(&repo, &root, "alice", "bob", "hi").unwrap();
        let names: Vec<String> = team(&repo).into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"alice".to_string()));
        assert!(names.contains(&"bob".to_string()));
    }

    #[test]
    fn identity_resolution_prefers_explicit_then_stored() {
        let (_d, _repo, root) = fixture();
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
    fn validate_name_rejects_empty_and_whitespace() {
        assert!(validate_name("").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("alice").is_ok());
    }

    #[test]
    fn union_merge_deduplicates_and_orders() {
        let shared = Message {
            id: "shared00".into(),
            ts: "2026-05-28T10:00:00.000000Z".into(),
            from: "alice".into(),
            to: "bob".into(),
            body: "shared".into(),
        };
        let only_a = Message {
            id: "aaaa0001".into(),
            ts: "2026-05-28T10:00:01.000000Z".into(),
            from: "alice".into(),
            to: "bob".into(),
            body: "from-a".into(),
        };
        let only_b = Message {
            id: "bbbb0001".into(),
            ts: "2026-05-28T09:59:59.000000Z".into(),
            from: "carol".into(),
            to: "bob".into(),
            body: "from-b".into(),
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
        send(&repo, &root, "alice", "bob", "base").unwrap();
        let base = repo.refname_to_id(MSG_REF).unwrap();

        // Local tip: append one message.
        send(&repo, &root, "alice", "bob", "local-only").unwrap();
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
