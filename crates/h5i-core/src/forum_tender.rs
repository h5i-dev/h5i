//! The tender: the only path between a box and the forum.
//!
//! A box has exactly two forum-shaped holes in it, and both were already cut
//! for other reasons:
//!
//! - **`/.h5i/inbox`** — bind-mounted read-only on the image tiers, granted
//!   read-only through Landlock on the kernel tiers. The box reads its threads
//!   here and can never write them.
//! - **`$H5I_ENV_CAPTURE_SPOOL`** — the box's one writable window onto the
//!   host, already drained after every run for command and capture records.
//!
//! There is no socket, no port, no token, and no HTTP. That is the point: a
//! box holds no forum credential because there is no credential to hold, and a
//! compromised agent that wants to reach the forum has nothing to steal and
//! nowhere to connect. The strongest access control here is the absence of an
//! API.
//!
//! ## Who runs this
//!
//! [`tend`] is called by the host, in the process that is already supervising a
//! box's run. There is no daemon: h5i has none by decision (ROADMAP R11), and
//! a forum does not earn one. A box that is running has a supervisor, and that
//! supervisor tends its box; a box that is not running has nothing to deliver
//! to. Host-side `h5i forum` commands also tend on the way past, so a forum
//! stays current under a human's hands even when nothing is executing.
//!
//! The honest limit of that choice: an idle box's inbox goes stale until either
//! something runs in it or a human touches the forum. When that stops being
//! good enough, the fix is a foreground `h5i forum serve` that loops over
//! [`tend_all`] — the same function, called more often — and not a background
//! daemon.
//!
//! ## Ingest is where authority is decided
//!
//! Draining the spool is not a transport step, it is the authority step. For
//! each staged record the host asks *which env directory did this come out
//! of*, resolves that to a roster entry, and stamps the resulting [`Author`]
//! onto the post. A record that names a different sender in its JSON does not
//! have that field read. A record from a revoked box is not dropped in silence:
//! it is posted carrying its refusal, so the forum shows that someone kept
//! talking after they were taken off it.
//!
//! The filename discipline is copied verbatim from the `cap-*` drain in
//! `env::ingest_shell_spool`, because the names are box-controlled: a fixed
//! prefix, a length cap, an `[A-Za-z0-9-]` charset, and a refusal to follow
//! symlinks.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use git2::Repository;

use crate::forum::{self, Author, InboxThread, Role, ThreadSummary};
use crate::env::{self, EnvManifest};
use crate::error::H5iError;

/// Prefix of a forum record staged by a box. Distinct from `cmd-*` and `cap-*`
/// so the existing drains and this one never see each other's files.
pub const SPOOL_PREFIX: &str = "forum-";
/// Longest accepted spool filename. The name is box-written, so it is bounded.
const SPOOL_NAME_MAX: usize = 96;
/// Most records drained from one box in one pass, so a box that spins writing
/// records cannot hold the tender in a loop.
const SPOOL_MAX_ENTRIES: usize = 256;
/// Largest accepted staged record. Bodies and attachments have their own caps;
/// this one bounds the read itself.
const SPOOL_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Filename of a thread as delivered into a box's inbox.
fn inbox_thread_file(thread_id: &str) -> String {
    format!("thread-{thread_id}.json")
}

/// What one pass of the tender did, for logging and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TendReport {
    /// Records drained from the box's spool and posted.
    pub ingested: usize,
    /// Records refused, and posted carrying the refusal.
    pub refused: usize,
    /// Records that could not be parsed at all, and so could not be posted.
    pub unreadable: usize,
    /// Threads written into the box's inbox.
    pub delivered: usize,
}

impl TendReport {
    /// Did this pass move anything?
    pub fn is_empty(&self) -> bool {
        *self == TendReport::default()
    }

    fn add(&mut self, other: TendReport) {
        self.ingested += other.ingested;
        self.refused += other.refused;
        self.unreadable += other.unreadable;
        self.delivered += other.delivered;
    }
}

/// Run one pass for one box: drain what it wrote, then deliver what it should
/// see.
///
/// Drain first, deliver second, deliberately: an agent that posts and
/// immediately re-reads its inbox then sees its own post, which is the
/// behaviour that makes the forum feel like a conversation rather than a queue.
pub fn tend(repo: &Repository, h5i_root: &Path, m: &EnvManifest) -> Result<TendReport, H5iError> {
    let roster = forum::read_roster(repo);
    // `by_box` finds the *active* participant for this box. A retired entry
    // still exists so its posts stay attributable, and must not be picked up
    // here as though the box were still carrying that identity.
    let Some(entry) = roster.by_box(&m.id).or_else(|| {
        // Revoked-but-still-bound: drain what it staged so the refusal is
        // recorded rather than lost, which `drain_spool` handles by posting it
        // with its denial. Delivery is skipped below.
        roster
            .agents
            .values()
            .find(|e| e.box_id.as_deref() == Some(&m.id))
    }) else {
        // Not on the forum. Nothing to drain and nothing to deliver — and in
        // particular, no inbox contents, so a box that was revoked and had its
        // entry removed does not keep reading yesterday's threads.
        return Ok(TendReport::default());
    };

    // The roster entry is not enough on its own. A box id is a path —
    // `env/<agent>/<slug>` — and paths get reused: remove a box and create
    // another with the same name and it inherits the id, and with it a roster
    // entry a human wrote about a different box. Measured before this check
    // existed: a recreated box that had never been attached was handed the
    // forum's threads on the first pass.
    //
    // So membership has to be confirmed from both ends. The roster says which
    // identity that box carries; the binding file in the env directory says the
    // box was actually bound to it, and `box rm` takes that file with the
    // directory. Only a real attach writes both. Neither is reachable from
    // inside the box, so this is two host-side facts agreeing, not a secret.
    let bound = env::team_binding(h5i_root, m).map(|(_run, agent)| agent);
    if bound.as_deref() != Some(entry.agent.as_str()) {
        // Note this is *not* how revocation is expressed: a revoked box keeps
        // its binding, so it stays identifiable and anything it stages is
        // posted carrying the refusal. This branch is the box that was never
        // bound to this identity at all.
        // Take the conversation away too: a box holding a stale inbox is a box
        // still reading a forum it is not on.
        clear_inbox(h5i_root, m);
        return Ok(TendReport::default());
    }

    let mut report = drain_spool(repo, h5i_root, m, entry)?;
    if entry.is_active() {
        let (written, peers) = deliver_inbox(repo, h5i_root, m, &entry.agent)?;
        report.delivered = written;
        mark_peer_influenced(h5i_root, m, &peers);
    }
    Ok(report)
}

/// Run one pass for every box on the forum.
///
/// This is what a host-side `h5i forum` command calls on its way past, and what
/// a future `h5i forum serve` would loop over.
pub fn tend_all(repo: &Repository, h5i_root: &Path) -> TendReport {
    let mut total = TendReport::default();
    // Pull first, so a box is delivered what the rest of the forum said before
    // this pass drains what it wants to say.
    let _ = crate::forum_sync::sync(repo, h5i_root);
    for m in env::list(h5i_root) {
        if let Ok(r) = tend(repo, h5i_root, &m) {
            total.add(r);
        }
    }
    // Publish what the drain just took in. A sync failure is not fatal to the
    // pass: the posts are durable in the local mirror and the next round pushes
    // them, which is the right behaviour for a laptop that is briefly offline.
    let _ = crate::forum_sync::sync(repo, h5i_root);
    total
}

// ── the resident pass, for as long as a box is running ─────────────────────

/// How often a running box's forum is tended.
///
/// One second. The forum's job is to make two agents feel like they are talking
/// to each other, and a reply that takes five seconds to appear reads as a
/// broken tool rather than a slow one. A pass over an idle box is two directory
/// reads and a ref lookup.
const TEND_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// A tender running for the lifetime of one box's session.
///
/// This is the whole of h5i's answer to liveness, and it is deliberately not a
/// daemon. A box that is running already has a host process supervising it —
/// holding its run lock, owning its egress proxy — and that process is exactly
/// the one that should be moving its mail. So the tender is a thread inside it,
/// started when the session starts and stopped when the session ends. Nothing
/// is installed, nothing outlives the run, and there is no second lifecycle for
/// anyone to reason about.
///
/// The cost of that choice, stated plainly: a box with nothing running does not
/// get its inbox refreshed until either something runs in it or a human touches
/// the forum. For collaborating agents — which are, by definition, running —
/// that gap does not arise.
pub struct SessionTender {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SessionTender {
    /// Start tending `m` until the returned guard is dropped.
    ///
    /// Returns `None` when the box is not on a forum, so a session that has
    /// nothing to do with the forum spawns no thread at all.
    ///
    /// The thread opens its own [`Repository`] from `repo_path` because a
    /// `Repository` is not `Send`. That is not a workaround: two handles onto
    /// one repository is the ordinary shape here, and the appends are
    /// compare-and-swap, so a concurrent writer is already the case this store
    /// is built for.
    pub fn start(repo_path: &Path, h5i_root: &Path, m: &EnvManifest) -> Option<SessionTender> {
        let repo = Repository::open(repo_path).ok()?;
        if forum_tender_is_idle(&repo, &m.id) {
            return None;
        }
        let repo_path = repo_path.to_path_buf();
        let h5i_root = h5i_root.to_path_buf();
        let m = m.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("h5i-forum-tender".into())
            .spawn(move || {
                let Ok(repo) = Repository::open(&repo_path) else {
                    return;
                };
                while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                    // A failed pass is not fatal to the run it is attached to.
                    // The forum is a coordination surface; losing a pass costs
                    // a second of latency, and taking the session down with it
                    // would be wildly out of proportion.
                    pass(&repo, &h5i_root, &m);
                    std::thread::sleep(TEND_INTERVAL);
                }
                // One last pass, so a post made in the final moments of a
                // session is not stranded in the spool until the next run.
                pass(&repo, &h5i_root, &m);
            })
            .ok()?;
        Some(SessionTender {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for SessionTender {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Is this box outside the forum entirely?
fn forum_tender_is_idle(repo: &Repository, box_id: &str) -> bool {
    forum::read_roster(repo).by_box(box_id).is_none()
}

/// One full pass for one box: pull, tend, push.
///
/// The sync is not optional and it is the half that was missing. A tender that
/// only drained the spool and refilled the inbox moved mail correctly *within*
/// one machine and never sent any of it, so two agents on two clones each held
/// a conversation with themselves — found the first time two real agents were
/// pointed at one forum, and invisible to every test that drove the forum
/// through a host command, because a host command calls [`tend_all`], which
/// always synced.
fn pass(repo: &Repository, h5i_root: &Path, m: &EnvManifest) {
    // Pull first, so the box is delivered what its peers said before this pass
    // drains what it wants to say; push after, so what it just said goes out
    // without waiting for the next tick.
    let _ = crate::forum_sync::sync(repo, h5i_root);
    let _ = tend(repo, h5i_root, m);
    let _ = crate::forum_sync::sync(repo, h5i_root);
}

// ── box → host ─────────────────────────────────────────────────────────────

fn drain_spool(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    entry: &forum::RosterEntry,
) -> Result<TendReport, H5iError> {
    let spool = env::env_capture_spool_dir(h5i_root, m);
    let mut report = TendReport::default();
    for path in staged_records(&spool)? {
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<forum::ForumPostSpool>(&raw) {
                Ok(staged) => {
                    match post_staged(repo, h5i_root, m, entry, staged) {
                        Ok(refused) => {
                            if refused {
                                report.refused += 1;
                            } else {
                                report.ingested += 1;
                            }
                        }
                        // A record the forum itself refused (unknown thread,
                        // role that may not post) is counted and removed: it
                        // will not succeed on a later pass either, and leaving
                        // it would retry forever.
                        Err(_) => report.refused += 1,
                    }
                }
                Err(_) => report.unreadable += 1,
            },
            Err(_) => report.unreadable += 1,
        }
        // Remove either way. The spool is a staging area, not a queue with
        // redelivery: a record that has been considered is done.
        let _ = std::fs::remove_file(&path);
    }
    Ok(report)
}

/// Post one staged record, returning whether it was refused.
fn post_staged(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    entry: &forum::RosterEntry,
    staged: forum::ForumPostSpool,
) -> Result<bool, H5iError> {
    let thread = staged.thread.clone();
    let mut new = staged.into_new_post();

    // The identity is the host's, built from the env directory this record came
    // out of and the roster entry bound to it. Nothing in the record is read.
    let author = Author::agent(
        &entry.agent,
        &m.id,
        entry.role,
        Some(m.policy_digest.clone()),
    )?
    .from_host(&forum::host_origin(h5i_root)?);

    let refused = if entry.is_active() {
        false
    } else {
        // Revoked, and still posting. Record it rather than dropping it: a
        // forum that silently swallows what it refuses teaches its readers that
        // nothing was refused.
        let note = format!(
            "sender revoked at {}",
            entry.revoked_at.as_deref().unwrap_or("(unknown time)")
        );
        new.denied = Some(match new.denied.take() {
            Some(prior) => format!("{note}; {prior}"),
            None => note,
        });
        true
    };

    forum::append_post(repo, &author, &thread, new)?;
    Ok(refused)
}

/// Forum records staged in a spool, oldest first, with the same paranoid
/// filename discipline the capture drain uses.
fn staged_records(spool: &Path) -> Result<Vec<PathBuf>, H5iError> {
    let Ok(dir) = std::fs::read_dir(spool) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in dir.flatten() {
        if out.len() >= SPOOL_MAX_ENTRIES {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(SPOOL_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        if name.len() > SPOOL_NAME_MAX {
            continue;
        }
        let stem = &name[..name.len() - ".json".len()];
        if !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            continue;
        }
        // `symlink_metadata` and not `metadata`: a symlink here is the box
        // pointing the drain at a file outside its own spool.
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > SPOOL_MAX_BYTES {
            continue;
        }
        out.push(entry.path());
    }
    out.sort();
    Ok(out)
}

// ── host → box ─────────────────────────────────────────────────────────────

/// Write the forum's current threads into a box's read-only inbox.
///
/// Files are named by thread, not by post, and rewritten in place: a thread's
/// file is always the whole conversation as of the last pass. Per-post files
/// would make the box reassemble a conversation from fragments and would leave
/// a deleted thread's posts behind; a whole-thread file is idempotent, and
/// removing the file is how a thread stops being visible.
///
/// Returns how many files moved, and which other participants' text this box
/// has now been shown — the input to the peer-influence marker.
fn deliver_inbox(
    repo: &Repository,
    h5i_root: &Path,
    m: &EnvManifest,
    me: &str,
) -> Result<(usize, Vec<String>), H5iError> {
    let inbox = env::env_inbox_dir(h5i_root, m);
    std::fs::create_dir_all(&inbox).map_err(|e| H5iError::with_path(e, &inbox))?;

    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut peers: BTreeSet<String> = BTreeSet::new();
    let mut written = 0usize;
    for summary in forum::list_threads(repo) {
        let Ok(thread) = forum::read_thread(repo, &summary.header.id) else {
            continue;
        };
        for p in &thread.posts {
            if p.sender != me {
                peers.insert(p.sender.clone());
            }
        }
        let view = InboxThread::of(&thread);
        let bytes = serde_json::to_vec_pretty(&view)?;
        let name = inbox_thread_file(&summary.header.id);
        let path = inbox.join(&name);
        // Skip an unchanged file so a box watching for modifications is not
        // woken every pass by bytes that did not move.
        if std::fs::read(&path).map(|cur| cur == bytes).unwrap_or(false) {
            wanted.insert(name);
            continue;
        }
        atomic_write(&path, &bytes)?;
        wanted.insert(name);
        written += 1;
    }

    // Remove the inbox files of threads that are gone (closed, or this box was
    // never meant to see them). The box cannot delete these itself — the mount
    // is read-only — so the host has to.
    if let Ok(dir) = std::fs::read_dir(&inbox) {
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("thread-") && name.ends_with(".json") && !wanted.contains(name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok((written, peers.into_iter().collect()))
}

// ── the taint ──────────────────────────────────────────────────────────────

/// Filename of the peer-influence marker in a box's env directory.
///
/// A file rather than a manifest field: the tender runs on a background thread
/// while the session it belongs to is writing the manifest, and a marker that
/// needed to interleave with those writes would be a race for no gain. The env
/// directory is outside every grant the box has, so the box can neither set the
/// marker nor clear it.
const INFLUENCE_FILE: &str = "forum-peer-influenced";

/// A record that this box was shown something another agent wrote.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfluence {
    /// When the first peer post reached this box.
    pub since: String,
    /// Forum identities whose posts have been delivered here.
    pub senders: Vec<String>,
}

/// Note that peer-written text was delivered into this box.
///
/// h5i does not claim to tell a hostile message from an ordinary one, and this
/// is not an attempt to: it records only that peer text arrived, which is a
/// fact the host observes rather than a judgement it makes.
///
/// Why that fact is worth keeping. A box's own output is evidence about the box.
/// Once a peer's text has been in front of the agent, its output is evidence
/// about the box *and* about whatever that text asked for — the two are no
/// longer separable from the outside. A reviewer deciding whether to trust a
/// patch, and a verifier re-running its tests, both need to know which of those
/// they are looking at.
///
/// Marked on **delivery**, not on read. Delivery is what the host can observe;
/// whether the agent read the file is a claim only the box could make, and this
/// is the lane where the host does not take the box's word.
fn mark_peer_influenced(h5i_root: &Path, m: &EnvManifest, senders: &[String]) {
    if senders.is_empty() {
        return;
    }
    let path = m.dir(h5i_root).join(INFLUENCE_FILE);
    let mut record = peer_influence(h5i_root, m).unwrap_or(PeerInfluence {
        since: crate::forum::now_ts(),
        senders: Vec::new(),
    });
    let mut changed = false;
    for s in senders {
        if !record.senders.contains(s) {
            record.senders.push(s.clone());
            changed = true;
        }
    }
    if !changed && path.exists() {
        return;
    }
    record.senders.sort();
    if let Ok(bytes) = serde_json::to_vec_pretty(&record) {
        let _ = atomic_write(&path, &bytes);
    }
}

/// Has this box been shown another agent's text?
pub fn peer_influence(h5i_root: &Path, m: &EnvManifest) -> Option<PeerInfluence> {
    let path = m.dir(h5i_root).join(INFLUENCE_FILE);
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Clear a box's inbox. Used when a participant is revoked: the conversation
/// stops being readable from inside the box at the next tend.
pub fn clear_inbox(h5i_root: &Path, m: &EnvManifest) {
    let inbox = env::env_inbox_dir(h5i_root, m);
    if let Ok(dir) = std::fs::read_dir(&inbox) {
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("thread-") && name.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), H5iError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).map_err(|e| H5iError::with_path(e, &tmp))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(H5iError::with_path(e, path))
        }
    }
}

// ── the box's own reads ────────────────────────────────────────────────────

/// Read the threads delivered into this box's inbox.
///
/// Called from inside a box, where the shared store is sealed and the refs are
/// unreachable. Malformed files are skipped rather than fatal: the inbox is
/// written by a host that may be a different build.
pub fn read_inbox(inbox: &Path) -> Vec<InboxThread> {
    let Ok(dir) = std::fs::read_dir(inbox) else {
        return Vec::new();
    };
    let mut out: Vec<InboxThread> = dir
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("thread-") && n.ends_with(".json"))
        })
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<InboxThread>(&b).ok())
        .collect();
    out.sort_by(|a, b| {
        let ak = a.posts.last().map(|p| p.ts.as_str()).unwrap_or(&a.header.created_at);
        let bk = b.posts.last().map(|p| p.ts.as_str()).unwrap_or(&b.header.created_at);
        bk.cmp(ak).then_with(|| a.header.id.cmp(&b.header.id))
    });
    out
}

/// Summaries of the threads in an inbox, in the same shape the host renders.
pub fn inbox_summaries(inbox: &Path) -> Vec<ThreadSummary> {
    read_inbox(inbox)
        .into_iter()
        .map(|t| ThreadSummary {
            status: t.status,
            claimed_by: t.claimed_by.clone(),
            posts: t.posts.len(),
            last_activity: t
                .posts
                .last()
                .map(|p| p.ts.clone())
                .unwrap_or_else(|| t.header.created_at.clone()),
            denials: t.posts.iter().filter(|p| p.is_denied()).count(),
            header: t.header,
        })
        .collect()
}

/// Stage a post from inside a box.
///
/// Writes one record into the capture spool for the host to ingest. The record
/// carries no identity, because identity is not the box's to assert.
pub fn stage_post(spool: &Path, staged: &forum::ForumPostSpool) -> Result<PathBuf, H5iError> {
    std::fs::create_dir_all(spool).map_err(|e| H5iError::with_path(e, spool))?;
    let name = format!(
        "{SPOOL_PREFIX}{}-{}.json",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let path = spool.join(name);
    let bytes = serde_json::to_vec(staged)?;
    std::fs::write(&path, bytes).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(path)
}

/// Block until the inbox changes, or the timeout expires.
///
/// The wake primitive an agent calls between turns. It polls a directory it
/// already has mounted — no hook to install, no runtime-specific integration,
/// no notification channel to keep alive. An agent's whole liveness story is
/// this one command, and it works identically under every coding agent that can
/// run a shell command.
///
/// Returns the inbox as it stands when something moved, or `None` on timeout.
pub fn wait_for_inbox(
    inbox: &Path,
    timeout: std::time::Duration,
    poll: std::time::Duration,
) -> Option<Vec<InboxThread>> {
    let before = inbox_fingerprint(inbox);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(poll);
        if inbox_fingerprint(inbox) != before {
            return Some(read_inbox(inbox));
        }
    }
}

/// A cheap summary of the inbox's contents, used to notice a change.
///
/// Names and modification times rather than contents: the host writes whole
/// files atomically, so a changed thread is always a changed mtime, and hashing
/// every thread on every poll would burn a box's CPU for nothing.
fn inbox_fingerprint(inbox: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let Ok(dir) = std::fs::read_dir(inbox) else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64, std::time::SystemTime)> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !name.starts_with("thread-") || !name.ends_with(".json") {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((name, meta.len(), meta.modified().ok()?))
        })
        .collect();
    out.sort();
    out
}

/// The forum identity this box was given, from the host-written binding.
///
/// Read from the environment rather than from any file the box can write: the
/// host injects it at spawn, layered after the ambient value so a box's persona
/// wins over whatever `H5I_AGENT` the host shell happened to carry.
pub fn box_identity() -> Option<String> {
    let v = std::env::var("H5I_AGENT").ok()?;
    let v = v.trim();
    if v.is_empty() || forum::validate_name(v).is_err() {
        return None;
    }
    Some(v.to_string())
}

/// Is this box attached to a forum? True when the host mounted an inbox for it.
pub fn box_inbox_path() -> Option<PathBuf> {
    let v = std::env::var_os("H5I_ENV_INBOX")?;
    let p = PathBuf::from(v);
    p.is_dir().then_some(p)
}

/// This box's capture spool, where a post is staged.
pub fn box_spool_path() -> Option<PathBuf> {
    let v = std::env::var_os("H5I_ENV_CAPTURE_SPOOL")?;
    Some(PathBuf::from(v))
}

// ── host-side binding ──────────────────────────────────────────────────────

/// Bind a box to a forum identity.
///
/// Writes the two files `env::team_binding` already reads, so `H5I_AGENT` flows
/// into the box through the injection path that exists — no change to the
/// lifecycle engine, and no second mechanism for the same idea. The files are
/// in the env directory, which is outside every grant the box has: a box can be
/// told who it is and can never tell itself something else.
pub fn write_binding(h5i_root: &Path, m: &EnvManifest, agent: &str) -> Result<(), H5iError> {
    forum::validate_name(agent)?;
    let dir = m.dir(h5i_root);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    let identity = dir.join("team-identity");
    std::fs::write(&identity, agent).map_err(|e| H5iError::with_path(e, &identity))?;
    let run = dir.join("team-run");
    std::fs::write(&run, "forum").map_err(|e| H5iError::with_path(e, &run))?;
    Ok(())
}

/// The role a box currently has on the forum, if it is on it.
pub fn box_role(repo: &Repository, box_id: &str) -> Option<Role> {
    forum::read_roster(repo).by_box(box_id).map(|e| e.role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_forum_records_with_safe_names_are_drained() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path();
        let write = |name: &str| std::fs::write(spool.join(name), "{}").unwrap();

        write("forum-1-2.json");
        write("forum-ok.json");
        write("cap-other.json"); // another drain's record
        write("cmd-other.cmd");
        // Names outside `[A-Za-z0-9-]`. A literal `../` cannot be one directory
        // entry, so the charset is what has to hold: dots, spaces and the rest
        // are refused before the name is ever joined onto a path.
        write("forum-a..b.json");
        write("forum-has space.json");
        write("forum-semi;rm.json");
        write(&format!("forum-{}.json", "x".repeat(SPOOL_NAME_MAX)));

        let found: Vec<String> = staged_records(spool)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(found, vec!["forum-1-2.json", "forum-ok.json"]);
    }

    #[test]
    fn a_symlinked_record_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path();
        let outside = dir.path().join("secret.json");
        std::fs::write(&outside, "{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, spool.join("forum-link.json")).unwrap();
        #[cfg(unix)]
        assert!(staged_records(spool).unwrap().is_empty());
    }

    #[test]
    fn staging_a_post_writes_one_drainable_record() {
        let dir = tempfile::tempdir().unwrap();
        let staged = forum::ForumPostSpool {
            thread: "0123456789abcdef".into(),
            kind: "FINDING".into(),
            body: "found it".into(),
            ..Default::default()
        };
        let path = stage_post(dir.path(), &staged).unwrap();
        assert!(path.exists());
        let found = staged_records(dir.path()).unwrap();
        assert_eq!(found.len(), 1, "the staged record must survive the filter");
        let back: forum::ForumPostSpool =
            serde_json::from_slice(&std::fs::read(&found[0]).unwrap()).unwrap();
        assert_eq!(back.body, "found it");
    }

    #[test]
    fn a_staged_record_carries_no_identity_fields() {
        let staged = forum::ForumPostSpool {
            thread: "0123456789abcdef".into(),
            kind: "FINDING".into(),
            body: "b".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&staged).unwrap();
        for forgeable in ["sender", "role", "box_id", "policy_digest"] {
            assert!(
                !json.contains(forgeable),
                "the wire format must have no {forgeable} field to forge"
            );
        }
    }

    #[test]
    fn oversized_attachments_are_dropped_and_named_not_silently_lost() {
        let staged = forum::ForumPostSpool {
            thread: "0123456789abcdef".into(),
            kind: "FINDING".into(),
            body: "b".into(),
            attachments: vec![
                forum::SpoolAttachment {
                    kind: "patch".into(),
                    name: Some("huge.diff".into()),
                    text: "x".repeat(forum::MAX_ATTACHMENT_BYTES + 1),
                },
                forum::SpoolAttachment {
                    kind: "text".into(),
                    name: Some("fine.txt".into()),
                    text: "ok".into(),
                },
            ],
            ..Default::default()
        };
        let new = staged.into_new_post();
        assert_eq!(new.attachments.len(), 1);
        let denied = new.denied.expect("the refusal must be recorded");
        assert!(denied.contains("huge.diff"));
    }

    #[test]
    fn an_unsupported_attachment_kind_is_dropped_with_the_post_still_delivered() {
        let staged = forum::ForumPostSpool {
            thread: "0123456789abcdef".into(),
            kind: "FINDING".into(),
            body: "still says something".into(),
            attachments: vec![forum::SpoolAttachment {
                kind: "binary".into(),
                name: None,
                text: "\u{0}\u{1}".into(),
            }],
            ..Default::default()
        };
        let new = staged.into_new_post();
        assert!(new.attachments.is_empty());
        assert_eq!(new.body, "still says something");
        assert!(new.denied.unwrap().contains("binary"));
    }

    #[test]
    fn an_oversized_body_is_truncated_and_the_truncation_is_recorded() {
        let staged = forum::ForumPostSpool {
            thread: "0123456789abcdef".into(),
            kind: "FINDING".into(),
            body: "x".repeat(forum::MAX_BODY_BYTES + 10),
            ..Default::default()
        };
        let new = staged.into_new_post();
        assert_eq!(new.body.len(), forum::MAX_BODY_BYTES);
        assert!(new.denied.unwrap().contains("truncated"));
    }

    #[test]
    fn an_inbox_read_skips_files_it_does_not_understand() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("thread-abc.json"), "not json").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "{}").unwrap();
        assert!(read_inbox(dir.path()).is_empty());
    }

    #[test]
    fn waiting_returns_none_when_nothing_moves() {
        let dir = tempfile::tempdir().unwrap();
        let got = wait_for_inbox(
            dir.path(),
            std::time::Duration::from_millis(60),
            std::time::Duration::from_millis(10),
        );
        assert!(got.is_none());
    }

    #[test]
    fn waiting_wakes_when_a_thread_lands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let view = serde_json::json!({
                "header": {
                    "id": "0123456789abcdef",
                    "title": "hello",
                    "created_at": "2026-08-20T00:00:00.000000Z",
                    "created_by": "operator",
                    "version": 1
                },
                "status": "open",
                "posts": []
            });
            std::fs::write(
                path.join("thread-0123456789abcdef.json"),
                serde_json::to_vec(&view).unwrap(),
            )
            .unwrap();
        });
        let got = wait_for_inbox(
            dir.path(),
            std::time::Duration::from_secs(3),
            std::time::Duration::from_millis(10),
        );
        let got = got.expect("the wait must wake on a new thread");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].header.title, "hello");
    }
}
