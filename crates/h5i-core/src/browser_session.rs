//! The host-owned registry of browser sessions.
//!
//! A browser session is the unit an agent talks to: one page state, one cookie
//! jar, one request log, one policy. `h5i browser start` makes one and prints
//! its id; every later verb names that id. Nothing else about the session is
//! agent-facing — not the process that renders the page, not the port it
//! listens on, not whether it happens to be inside a box.
//!
//! # Why the registry is host-owned
//!
//! The engine already writes a control file next to its stream file, and a CLI
//! that reads that file can drive the resident page (`h5i-browser-light`'s
//! `stream::serve`). That is enough to *reach* a session and not enough to
//! *name* one, and the difference matters as soon as a session can live
//! somewhere other than this process's own filesystem view.
//!
//! So the id and the record live here, on the host, always — including for a
//! session whose engine runs inside a box. One table, one resolution path,
//! whatever the placement. The alternative, letting the box own the record for
//! boxed sessions, buys nothing and splits every lookup in two.
//!
//! # Not under a git repository
//!
//! Every other noun in this product stores its state under the enclosing
//! repository, because every other noun is *about* a repository. A browser is
//! not. `h5i browser start https://example.com` in an empty directory is the
//! ordinary case and must work, so the registry lives in the user's state
//! directory instead ([`root`]).
//!
//! # Ids are never reused
//!
//! A session directory outlives the session: closing one writes its ending into
//! the record rather than deleting it. That is what makes [`Session::state`]
//! answerable after the fact, and it is also what makes reuse impossible —
//! [`new_id`] rejects any candidate whose directory already exists. An agent
//! that keeps a stale id gets a definite "this session ended, here is how",
//! never a different session wearing the same name.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::H5iError;

/// Exit status for a verb addressed to a session that is not live.
///
/// `sysexits.h`'s `EX_UNAVAILABLE`. It is a distinct code rather than the
/// generic failure because the whole point is that an agent can tell "the
/// session is gone" from "the click did not work", and retry logic that cannot
/// tell them apart is retry logic that silently starts a second browser.
pub const EXIT_SESSION_GONE: i32 = 69;

/// Directory name under the state root that holds one directory per session.
const SESSIONS: &str = "sessions";

/// The record file inside a session directory.
const RECORD: &str = "session.json";

/// Where the engine advertises its control port, inside a session directory.
pub const CONTROL_FILE: &str = "control";

/// Where the engine advertises its viewer stream port.
pub const STREAM_FILE: &str = "stream";

/// The engine's request log: one JSON object per line, written before the wire.
pub const RECEIPTS_FILE: &str = "requests.jsonl";

/// The verbs an agent asked for, as the session recorded them.
pub const ACTIONS_FILE: &str = "actions.jsonl";

/// Where files this session produced are collected. Host-named, always: see
/// [`crate::browser_session::artifact_path`].
pub const ARTIFACTS_DIR: &str = "artifacts";

/// Which engine renders the page.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// h5i's own engine. Every request is policy-checked and recorded before it
    /// reaches the wire, and the record is fail-closed.
    H5iLight,
    /// A Chromium driven through `agent-browser`. Its request lane is
    /// best-effort: attach races and buffer limits leave gaps.
    Chromium,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::H5iLight => "h5i-light",
            Engine::Chromium => "chromium",
        }
    }
}

/// Where the engine process runs.
///
/// This is the only thing `--in` changes, and it changes nothing an agent
/// types: the id resolves the same way and every verb has the same name and
/// the same answer. What it changes is what the record can honestly claim
/// about the network lane ([`Session::lane`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Placement {
    /// On this machine, in this user's ordinary process space. No containment
    /// beyond the engine itself.
    Host,
    /// Inside a box. The engine's loopback is the box's loopback, so verbs are
    /// carried in rather than dialled directly (`h5i box run`).
    Box {
        /// The box's name, as `h5i box list` prints it.
        name: String,
    },
}

impl Placement {
    pub fn box_name(&self) -> Option<&str> {
        match self {
            Placement::Host => None,
            Placement::Box { name } => Some(name),
        }
    }

    /// What to print for `isolation` in a one-line status.
    pub fn as_str(&self) -> &str {
        match self {
            Placement::Host => "none",
            Placement::Box { .. } => "box",
        }
    }
}

/// Who observed the session's network activity.
///
/// The same split [`crate::browser_events::Lane`] carries per row, recorded
/// once for the session so a reader does not have to infer it from placement.
/// A host session's requests are the engine's own account of what it fetched:
/// fail-closed and complete, and still the engine's account. A boxed session's
/// requests are additionally seen at the box's boundary, which is outside the
/// thing being described.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// The engine said so, fail-closed.
    EngineClaimed,
    /// h5i saw it from outside the box as well.
    HostObserved,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::EngineClaimed => "engine-claimed",
            Lane::HostObserved => "host-observed",
        }
    }
}

/// What survives the session.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Storage {
    /// Cookies and page state die with the session.
    Ephemeral,
    /// Cookies are written into the session directory and can seed a later
    /// session through `--restore`.
    Persistent,
}

/// Where a session is in its life.
///
/// `Live` is the only state a verb may act on. The other four are all endings,
/// kept apart because they are different facts about the run and a receipt that
/// merged them would be a receipt that cannot say whether the record is
/// complete.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Started, and the engine answered the last time anyone looked.
    Live,
    /// Ended by `h5i browser close`. The record is complete.
    Closed,
    /// The engine stopped without being asked. Whatever it was doing when it
    /// stopped is not in the record, and the record says so.
    Died,
    /// Outlived `expires_at`. An ending like any other, written as an event
    /// rather than by the directory quietly disappearing.
    Expired,
    /// The box holding the engine was removed. Distinct from `Died` because the
    /// cause is on this side of the boundary and is therefore known.
    Evicted,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Live => "live",
            State::Closed => "closed",
            State::Died => "died",
            State::Expired => "expired",
            State::Evicted => "evicted",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, State::Live)
    }

    /// The ending as a clause, so a message reads as English rather than as a
    /// field name pasted into a sentence.
    pub fn describe(self) -> &'static str {
        match self {
            State::Live => "is live",
            State::Closed => "was closed",
            State::Died => "died",
            State::Expired => "expired",
            State::Evicted => "was evicted",
        }
    }
}

/// How to reach the engine, when it is reachable at all.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Control {
    /// Where the engine listens, **as the engine sees it**.
    ///
    /// Two different things behind one field, because a caller only ever hands
    /// it straight back to the engine. For a host session it is the file the
    /// engine wrote its control port into, inside the session directory. For a
    /// boxed one it is a Unix socket in the box's own `/tmp` — a path rather
    /// than a port, because each `h5i box run` gets its own network namespace
    /// and a port bound in one is unreachable from the next.
    pub file: Option<PathBuf>,
    /// The same file **as this machine sees it**, when this machine can see it
    /// at all. `None` on an image-backed tier, whose `/tmp` lives in the image
    /// and is not on the host's filesystem — there, liveness is not knowable
    /// from outside and [`Session::probe`] says so by not guessing.
    pub witness: Option<PathBuf>,
    /// The process h5i spawned. On the host that is the engine; for a boxed
    /// session it is the `box run` that carries it, which is a host process and
    /// lives exactly as long as the engine inside does.
    pub pid: Option<u32>,
}

/// One browser session, as the host records it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Session {
    pub id: String,
    pub engine: Engine,
    pub placement: Placement,
    pub lane: Lane,
    /// The URL `start` was given. Not the current URL: that is page state, it
    /// changes under the agent's feet, and asking the session is the only way
    /// to know it. Recording it here would be a second answer that goes stale.
    pub url: String,
    pub started_at: String,
    pub expires_at: Option<String>,
    pub storage: Storage,
    /// The policy this session runs under, digested. Two sessions with the same
    /// digest were allowed the same things.
    pub policy_digest: String,
    /// The session whose storage seeded this one, if any. A restore is a new
    /// session with a new id and an inheritance recorded, never a resurrection.
    pub restored_from: Option<String>,
    pub state: State,
    pub ended_at: Option<String>,
    /// One line on how it ended, for the states that have something to say.
    pub end_reason: Option<String>,
    pub control: Control,
}

impl Session {
    /// The lane a placement can honestly claim.
    ///
    /// **Being in a box is not enough.** A box whose policy lets the engine
    /// reach the whole host network has nothing at its boundary to corroborate
    /// what the engine says it fetched, so its lane is still the engine's own
    /// account. What earns `HostObserved` is enforcement outside the engine:
    /// an egress allowlist the box applies at its boundary, or a net mode that
    /// lets nothing out at all.
    ///
    /// Getting this wrong in the generous direction is the one error this
    /// product cannot afford — it is exactly the box-claimed row rendered as
    /// host-observed that the lane split exists to prevent.
    pub fn lane_for(placement: &Placement, boundary_enforced: bool) -> Lane {
        match placement {
            Placement::Host => Lane::EngineClaimed,
            Placement::Box { .. } if boundary_enforced => Lane::HostObserved,
            Placement::Box { .. } => Lane::EngineClaimed,
        }
    }

    /// Is the engine still there?
    ///
    /// Three answers, not two, and the third is the point: **unknown**.
    ///
    /// The pid answers whenever h5i spawned something — for a boxed session
    /// that is the `box run` carrying the engine, which dies with it. Failing
    /// that, the control file as this machine sees it answers. Failing *that*,
    /// on a tier whose `/tmp` is inside an image, nothing here can tell, and
    /// this returns `true` rather than inventing a death: the verb about to be
    /// sent will find out for certain, and reporting a live session dead is the
    /// worse of the two errors.
    ///
    /// Deliberately not a network probe. A status listing that opened a socket
    /// per row would make `h5i browser list` a thing that touches every session
    /// it prints.
    pub fn probe(&self) -> bool {
        if !self.state.is_live() {
            return false;
        }
        if let Some(pid) = self.control.pid {
            return process_alive(pid);
        }
        match &self.control.witness {
            Some(path) => path.exists(),
            None => true,
        }
    }
}

/// The state directory the registry lives in.
///
/// `$H5I_BROWSER_HOME` wins so a test, or a user who wants two independent
/// fleets, can say where. Otherwise the XDG state directory, which is the
/// correct place for "state that should persist between restarts but is not
/// important enough for the data directory".
pub fn root() -> Result<PathBuf, H5iError> {
    if let Some(explicit) = std::env::var_os("H5I_BROWSER_HOME") {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        let state = PathBuf::from(state);
        if state.is_absolute() {
            return Ok(state.join("h5i").join("browser"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        H5iError::Metadata(
            "cannot resolve where to keep browser sessions — set $HOME, \
             $XDG_STATE_HOME, or $H5I_BROWSER_HOME"
                .into(),
        )
    })?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("h5i")
        .join("browser"))
}

/// `<root>/sessions`, created if it is not there.
pub fn sessions_dir(root: &Path) -> Result<PathBuf, H5iError> {
    let dir = root.join(SESSIONS);
    fs::create_dir_all(&dir).map_err(H5iError::Io)?;
    Ok(dir)
}

/// The directory holding one session's record, control file, log and artifacts.
pub fn dir(root: &Path, id: &str) -> PathBuf {
    root.join(SESSIONS).join(id)
}

/// Mint an id no session has ever had, and create its directory.
///
/// The directory is the claim: creating it with `create_new` is what makes two
/// concurrent `start`s unable to agree on the same id, and its continued
/// existence after the session ends is what stops the id coming back.
pub fn new_id(root: &Path) -> Result<String, H5iError> {
    let sessions = sessions_dir(root)?;
    for _ in 0..64 {
        let id = format!("br_{}", suffix());
        let path = sessions.join(&id);
        match fs::create_dir(&path) {
            Ok(()) => {
                fs::create_dir_all(path.join(ARTIFACTS_DIR)).map_err(H5iError::Io)?;
                return Ok(id);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(H5iError::Io(e)),
        }
    }
    Err(H5iError::Metadata(
        "could not mint a free browser session id after 64 tries".into(),
    ))
}

/// Six characters of `[0-9a-z]`, avoiding the letters that read as digits.
///
/// Short because an agent types it on every verb, and an id is not a secret:
/// reaching a session needs the control file, not the name.
fn suffix() -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstuvwxyz";
    (0..6)
        .map(|_| ALPHABET[fastrand::usize(..ALPHABET.len())] as char)
        .collect()
}

/// Write a record, replacing whatever was there.
pub fn write(root: &Path, session: &Session) -> Result<(), H5iError> {
    let dir = dir(root, &session.id);
    fs::create_dir_all(&dir).map_err(H5iError::Io)?;
    let body = serde_json::to_string_pretty(session)
        .map_err(|e| H5iError::Metadata(format!("could not serialize the session record: {e}")))?;
    // Rename over, so a reader never sees half a record.
    let tmp = dir.join(".session.json.tmp");
    fs::write(&tmp, format!("{body}\n")).map_err(H5iError::Io)?;
    fs::rename(&tmp, dir.join(RECORD)).map_err(H5iError::Io)
}

/// Read one record by id.
pub fn read(root: &Path, id: &str) -> Result<Session, H5iError> {
    let path = dir(root, id).join(RECORD);
    let body = fs::read_to_string(&path).map_err(|_| unknown(root, id))?;
    serde_json::from_str(&body)
        .map_err(|e| H5iError::Metadata(format!("`{id}`'s record is unreadable: {e}")))
}

/// Every session this host knows about, newest first.
pub fn list(root: &Path) -> Result<Vec<Session>, H5iError> {
    let sessions = root.join(SESSIONS);
    if !sessions.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&sessions).map_err(H5iError::Io)? {
        let entry = entry.map_err(H5iError::Io)?;
        let record = entry.path().join(RECORD);
        if !record.exists() {
            continue;
        }
        if let Ok(body) = fs::read_to_string(&record)
            && let Ok(session) = serde_json::from_str::<Session>(&body)
        {
            out.push(session);
        }
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(out)
}

/// The error for an id that names nothing, with the ids that do.
fn unknown(root: &Path, id: &str) -> H5iError {
    let known: Vec<String> = list(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.state.is_live())
        .map(|s| s.id)
        .collect();
    if known.is_empty() {
        H5iError::Metadata(format!(
            "`{id}` is not a browser session on this machine, and none are running. \
             Start one with `h5i browser start <url>`."
        ))
    } else {
        H5iError::Metadata(format!(
            "`{id}` is not a browser session on this machine. Live sessions: {}.",
            known.join(", ")
        ))
    }
}

/// Resolve an id to a session that a verb may act on.
///
/// This is the one place the "died" rule is enforced, and it is enforced by
/// refusing rather than by restarting. The error names the ending and points at
/// `--restore`, because the agent's next move is a decision (continue from that
/// storage, or start clean) and not something this function may make for it.
pub fn open_live(root: &Path, id: &str) -> Result<Session, SessionGone> {
    let session = match read(root, id) {
        Ok(s) => s,
        Err(e) => return Err(SessionGone::Unknown(e)),
    };
    if !session.state.is_live() {
        return Err(SessionGone::Ended {
            state: session.state,
            reason: session.end_reason.clone(),
            id: session.id.clone(),
        });
    }
    if !session.probe() {
        // Seen dead now, so record it now: the next reader should not have to
        // re-derive it, and a receipt that says "died" needs a time.
        let mut dead = session.clone();
        end(root, &mut dead, State::Died, "the engine stopped answering");
        return Err(SessionGone::Ended {
            state: State::Died,
            reason: dead.end_reason.clone(),
            id: dead.id,
        });
    }
    Ok(session)
}

/// Why a verb could not be delivered.
#[derive(Debug)]
pub enum SessionGone {
    /// No such id here.
    Unknown(H5iError),
    /// The id names a session that has ended.
    Ended {
        state: State,
        reason: Option<String>,
        id: String,
    },
}

impl std::fmt::Display for SessionGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionGone::Unknown(e) => write!(f, "{e}"),
            SessionGone::Ended { state, reason, id } => {
                write!(f, "browser session `{id}` {}", state.describe())?;
                if let Some(reason) = reason {
                    write!(f, ": {reason}")?;
                }
                write!(
                    f,
                    ". It will not be restarted automatically. \
                     Start a new one with `h5i browser start <url>`, or carry this one's \
                     storage forward with `h5i browser start <url> --restore {id}`."
                )
            }
        }
    }
}

impl std::error::Error for SessionGone {}

/// Write an ending into a record. Idempotent: a session that already ended
/// keeps the first ending, because the first one is the true one.
pub fn end(root: &Path, session: &mut Session, state: State, reason: &str) {
    if !session.state.is_live() {
        return;
    }
    session.state = state;
    session.ended_at = Some(now());
    session.end_reason = Some(reason.to_string());
    session.control = Control::default();
    let _ = write(root, session);
}

/// Mark every live session placed in `box_name` as evicted.
///
/// Called when a box is removed. Without it the sessions would be found dead
/// later by probe and recorded as `Died`, which is true but less informative
/// than the cause this side of the boundary actually knows.
pub fn evict_box(root: &Path, box_name: &str) -> Result<usize, H5iError> {
    let mut n = 0;
    for mut session in list(root)? {
        if session.state.is_live() && session.placement.box_name() == Some(box_name) {
            end(
                root,
                &mut session,
                State::Evicted,
                &format!("box `{box_name}` was removed while the session was live"),
            );
            n += 1;
        }
    }
    Ok(n)
}

/// Close every live session that has outlived its `expires_at`.
///
/// Expiry is a sweep rather than a timer because there is no daemon to hold
/// one, and it is recorded rather than enacted by deletion for the reason the
/// whole module exists: an ending nobody wrote down is indistinguishable from a
/// session that never ran.
pub fn expire_due(root: &Path) -> Result<usize, H5iError> {
    let now_ts = now();
    let mut n = 0;
    for mut session in list(root)? {
        if !session.state.is_live() {
            continue;
        }
        if let Some(expires) = session.expires_at.clone()
            && expires <= now_ts
        {
            end(
                root,
                &mut session,
                State::Expired,
                &format!("the session's time limit ({expires}) passed"),
            );
            n += 1;
        }
    }
    Ok(n)
}

/// Where a named artifact goes.
///
/// **The host names the file.** The engine, and anything the page persuaded it
/// to do, chooses only the bytes. `name` is reduced to a single path component
/// of a known-safe alphabet before it is joined, so a session cannot write
/// through `..`, through a symlink it planted, or onto a dotfile — the same
/// rule the runner import applies to a tree that came home from a machine we
/// assume is broken ([`crate::quarantine`]).
pub fn artifact_path(root: &Path, id: &str, name: &str) -> PathBuf {
    dir(root, id).join(ARTIFACTS_DIR).join(safe_name(name))
}

/// One path component, `[A-Za-z0-9._-]`, never empty, never leading-dot,
/// bounded in length.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    out.truncate(96);
    if out.is_empty() {
        out.push_str("artifact");
    }
    out
}

/// Largest string h5i will relay from a session, in bytes.
///
/// A snapshot of a real page is tens of kilobytes and a markdown rendering can
/// be more, so this is generous. It is not unbounded, because the thing on the
/// other end composed the bytes and an agent's context window is a resource a
/// page should not be able to spend.
const MAX_STRING: usize = 256 * 1024;

/// Longest array relayed. A request log or a snapshot ref list is long; a page
/// that can make it arbitrarily long can make one verb cost everything.
const MAX_ARRAY: usize = 10_000;

/// Deepest nesting relayed, past which the value is replaced.
const MAX_DEPTH: usize = 64;

/// Make a session's answer safe to print and safe to hand to a model.
///
/// **Everything a session returns is attacker-influenced.** The page chose the
/// title, the link text, the error message and the URL; the engine only carried
/// them. That is true whether or not there is a box, so this runs on every
/// answer rather than only on the boxed ones.
///
/// Three things are removed, each for a different reason:
///
/// * **Escape sequences.** `ESC` in a relayed string is a page rewriting the
///   terminal it is printed to — moving the cursor over the line above, hiding
///   what it just did, or repainting a prompt. Nothing a browser has to say
///   needs `ESC`, so it never survives.
/// * **Other control characters.** They corrupt the transcript a human reads
///   back, and `\r` alone is enough to overwrite a line. Tab and newline stay,
///   because a page's text legitimately contains them.
/// * **Size.** Capped per string, per array and by depth, with the truncation
///   *stated in the value* rather than performed quietly — a silently shortened
///   answer is one an agent will reason about as if it were complete.
pub fn scrub(value: &mut serde_json::Value) {
    scrub_at(value, 0);
}

fn scrub_at(value: &mut serde_json::Value, depth: usize) {
    use serde_json::Value;
    if depth > MAX_DEPTH {
        *value = Value::String("[nesting too deep to relay]".into());
        return;
    }
    match value {
        Value::String(s) => {
            let cleaned = scrub_text(s);
            *s = cleaned;
        }
        Value::Array(items) => {
            let dropped = items.len().saturating_sub(MAX_ARRAY);
            items.truncate(MAX_ARRAY);
            for item in items.iter_mut() {
                scrub_at(item, depth + 1);
            }
            if dropped > 0 {
                items.push(Value::String(format!("[{dropped} more items not relayed]")));
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                scrub_at(v, depth + 1);
            }
        }
        _ => {}
    }
}

/// One string, with escapes and control characters gone and a stated cap.
pub fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_STRING));
    let mut budget = MAX_STRING;
    let mut dropped = 0usize;
    for ch in text.chars() {
        let keep = match ch {
            '\t' | '\n' => Some(ch),
            // Carriage return is line-overwrite. Newline carries the meaning.
            '\r' => None,
            // C0, DEL, and the C1 block, which some terminals decode as escapes
            // in their own right.
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => None,
            c if ((c as u32) >= 0x80 && (c as u32) <= 0x9f) => None,
            c => Some(c),
        };
        let Some(ch) = keep else {
            dropped += 1;
            continue;
        };
        let len = ch.len_utf8();
        if len > budget {
            let rest = text.len().saturating_sub(out.len());
            out.push_str(&format!("…[{rest} more bytes not relayed]"));
            return out;
        }
        budget -= len;
        out.push(ch);
    }
    if dropped > 0 {
        out.push_str(&format!(" [{dropped} control characters removed]"));
    }
    out
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `kill(pid, 0)`: does a process with this id exist and are we allowed to
/// signal it? A pid we cannot signal is not ours and so is not our session's.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    // No portable equivalent, and a wrong `false` would report a live session
    // dead. The control file is the fallback everywhere else.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, placement: Placement) -> Session {
        Session {
            id: id.to_string(),
            engine: Engine::H5iLight,
            lane: Session::lane_for(&placement, true),
            placement,
            url: "https://example.com/".into(),
            started_at: now(),
            expires_at: None,
            storage: Storage::Ephemeral,
            policy_digest: "sha256:test".into(),
            restored_from: None,
            state: State::Live,
            ended_at: None,
            end_reason: None,
            control: Control::default(),
        }
    }

    #[test]
    fn ids_are_never_reused_even_after_the_session_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        write(root, &s).unwrap();
        end(root, &mut s, State::Closed, "closed by the user");

        // The directory survives the ending, which is what forbids the reuse.
        assert!(dir(root, &id).exists());
        for _ in 0..32 {
            assert_ne!(new_id(root).unwrap(), id);
        }
    }

    #[test]
    fn a_verb_on_an_ended_session_is_refused_rather_than_restarted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        write(root, &s).unwrap();
        end(root, &mut s, State::Closed, "closed by the user");

        match open_live(root, &id) {
            Err(SessionGone::Ended { state, .. }) => assert_eq!(state, State::Closed),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_control_file_is_recorded_as_died_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        // Boxed, and the `box run` that carried it is gone: liveness falls to
        // the control file as the host sees it, and there is none.
        let mut s = session(
            &id,
            Placement::Box {
                name: "web".into(),
            },
        );
        s.control.witness = Some(dir(root, &id).join(CONTROL_FILE));
        write(root, &s).unwrap();

        match open_live(root, &id) {
            Err(SessionGone::Ended { state, .. }) => assert_eq!(state, State::Died),
            other => panic!("expected died, got {other:?}"),
        }
        // And it is now written down, not re-derived by the next reader.
        assert_eq!(read(root, &id).unwrap().state, State::Died);
    }

    #[test]
    fn a_session_we_cannot_see_into_is_not_declared_dead() {
        // Image-backed tier: no host pid left, no host-visible control file.
        // Guessing "died" here would close a session that is still serving.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let s = session(
            &id,
            Placement::Box {
                name: "web".into(),
            },
        );
        assert!(s.control.witness.is_none() && s.control.pid.is_none());
        write(root, &s).unwrap();
        assert!(open_live(root, &id).is_ok());
        assert_eq!(read(root, &id).unwrap().state, State::Live);
    }

    #[test]
    fn the_first_ending_is_the_one_that_sticks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        write(root, &s).unwrap();
        end(root, &mut s, State::Closed, "closed by the user");
        end(root, &mut s, State::Died, "engine stopped");
        assert_eq!(read(root, &id).unwrap().state, State::Closed);
    }

    #[test]
    fn removing_a_box_evicts_its_sessions_and_leaves_host_ones_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let boxed = new_id(root).unwrap();
        let hosted = new_id(root).unwrap();
        write(
            root,
            &session(
                &boxed,
                Placement::Box {
                    name: "web".into(),
                },
            ),
        )
        .unwrap();
        write(root, &session(&hosted, Placement::Host)).unwrap();

        assert_eq!(evict_box(root, "web").unwrap(), 1);
        assert_eq!(read(root, &boxed).unwrap().state, State::Evicted);
        assert_eq!(read(root, &hosted).unwrap().state, State::Live);
    }

    #[test]
    fn expiry_is_an_event_on_the_record_not_a_disappearance() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        s.expires_at = Some("2000-01-01T00:00:00Z".into());
        write(root, &s).unwrap();

        assert_eq!(expire_due(root).unwrap(), 1);
        let after = read(root, &id).unwrap();
        assert_eq!(after.state, State::Expired);
        assert!(after.ended_at.is_some());
        assert!(dir(root, &id).exists());
    }

    #[test]
    fn a_box_earns_the_host_observed_lane_only_by_enforcing_at_its_boundary() {
        let boxed = Placement::Box { name: "w".into() };
        assert_eq!(Session::lane_for(&Placement::Host, true), Lane::EngineClaimed);
        assert_eq!(Session::lane_for(&boxed, true), Lane::HostObserved);
        // A box that lets the engine reach the whole network corroborates
        // nothing, so it does not upgrade the lane just by being a box.
        assert_eq!(Session::lane_for(&boxed, false), Lane::EngineClaimed);
    }

    #[test]
    fn an_escape_sequence_never_survives_the_relay() {
        // A page that can print ESC into an agent's terminal can repaint the
        // line above it, which is the whole attack.
        let hostile = "ok\u{1b}[2K\u{1b}[1A malicious\r overwrite\u{0}";
        let clean = scrub_text(hostile);
        assert!(!clean.contains('\u{1b}'), "{clean}");
        assert!(!clean.contains('\r'), "{clean}");
        assert!(!clean.contains('\u{0}'), "{clean}");
        assert!(clean.starts_with("ok"), "{clean}");
        assert!(clean.contains("control characters removed"), "{clean}");
    }

    #[test]
    fn newlines_and_tabs_are_page_text_and_stay() {
        assert_eq!(scrub_text("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn truncation_is_stated_in_the_value_not_performed_quietly() {
        let huge = "x".repeat(MAX_STRING * 2);
        let clean = scrub_text(&huge);
        assert!(clean.len() < huge.len());
        assert!(clean.contains("not relayed"), "silent truncation");

        let mut value = serde_json::json!({
            "refs": (0..MAX_ARRAY + 5).map(|i| i.to_string()).collect::<Vec<_>>(),
        });
        scrub(&mut value);
        let refs = value["refs"].as_array().unwrap();
        assert_eq!(refs.len(), MAX_ARRAY + 1);
        assert!(refs.last().unwrap().as_str().unwrap().contains("not relayed"));
    }

    #[test]
    fn scrub_reaches_nested_strings() {
        let mut value = serde_json::json!({"page": {"title": "a\u{1b}[31mb"}});
        scrub(&mut value);
        assert!(!value["page"]["title"].as_str().unwrap().contains('\u{1b}'));
    }

    #[test]
    fn the_host_names_every_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "..",
            ".bashrc",
            "a/b/c.png",
            "",
        ] {
            let path = artifact_path(root, "br_test", hostile);
            let parent = dir(root, "br_test").join(ARTIFACTS_DIR);
            assert_eq!(path.parent().unwrap(), parent, "escaped with {hostile:?}");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            assert!(!name.starts_with('.'), "dotfile from {hostile:?}");
            assert!(!name.is_empty());
        }
    }
}
