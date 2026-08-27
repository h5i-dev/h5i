//! The host-owned registry of browser sessions.
//!
//! A browser session is the unit an agent talks to: one page state, one cookie
//! jar, one request log, one policy. Nothing else about it is agent-facing —
//! not the process that renders the page, not the port it listens on, not
//! whether it happens to be inside a box.
//!
//! # The id is internal
//!
//! Every session has an opaque id (`br_7k2xqa`) and it appears in the record,
//! in `--json`, and in receipts, because a durable reference has to be
//! something no rename can break. **It is not what an agent types.** A CLI that
//! demands an opaque string on every verb is a CLI copying a remote-browser
//! HTTP API, where the id exists because the client and the browser share
//! nothing else; here they share a filesystem.
//!
//! So resolution has three layers, from most to least explicit
//! ([`resolve`]): a `--session <name>` a person chose, `$H5I_BROWSER_SESSION`,
//! and the **default session** — the one `open` made when nobody said which.
//! Names are for running several at once; the default is for the ordinary case
//! of running one.
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
//! not. `h5i browser open https://example.com` in an empty directory is the
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

/// Holds the id of the session a verb acts on when nobody says which.
///
/// A pointer rather than a convention like "the newest live one", because the
/// convention silently moves under an agent the moment a second session is
/// opened, and an agent that has been quietly redirected to a different page is
/// the failure this whole module is arranged to prevent.
const DEFAULT_POINTER: &str = "default";

/// Where the engine advertises its control port, inside a session directory.
pub const CONTROL_FILE: &str = "control";

/// Where the engine advertises its viewer stream port.
pub const STREAM_FILE: &str = "stream";

/// The engine's request log: one JSON object per line, written before the wire.
pub const RECEIPTS_FILE: &str = "requests.jsonl";

/// The verbs an agent asked for, as the session recorded them.
pub const ACTIONS_FILE: &str = "actions.jsonl";

/// The handover journal: one line per `take` or `release`.
///
/// Separate from `control.json`, which holds only *who holds it now*. A current
/// holder cannot answer "was a human driving when that form was submitted",
/// and that is the question an audit is for.
pub const CONTROL_JOURNAL: &str = "control.jsonl";

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

/// How a verb reaches the engine.
///
/// Two, because neither works everywhere. A loopback port is the simple case
/// and needs no path short enough to be a socket address. A Unix socket is the
/// only thing that works **anywhere a network namespace is in play**: a box's
/// netns may have no usable loopback at all (`net.mode = deny` leaves nothing
/// to dial), and every `h5i box run` gets a fresh one, so a port bound in one
/// is unreachable from the next. A path survives both, because the box's
/// filesystem is one filesystem across every run in it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    /// A file holding a loopback port number.
    #[default]
    Port,
    /// A Unix domain socket.
    Socket,
}

impl Channel {
    /// The flag the engine's CLI takes for this channel.
    pub fn flag(self) -> &'static str {
        match self {
            Channel::Port => "--control-file",
            Channel::Socket => "--control-socket",
        }
    }
}

/// How to reach the engine, when it is reachable at all.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Control {
    /// Which of the two channels [`Control::file`] names.
    #[serde(default)]
    pub channel: Channel,
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
    /// The opaque, durable reference. In the record, in `--json`, in receipts.
    /// Not what an agent types: see the module docs.
    pub id: String,
    /// The name a person gave this session with `--session`, if any. Unnamed
    /// sessions are the ordinary case and are reached through the default
    /// pointer instead.
    ///
    /// A name is not an identity: it can be reused once the session it named
    /// has ended, which is exactly what makes it comfortable to type. The id
    /// cannot, which is why the id is what gets written down.
    #[serde(default)]
    pub name: Option<String>,
    pub engine: Engine,
    pub placement: Placement,
    pub lane: Lane,
    /// The URL this session was last **told to open**: the one `open` was given,
    /// whether it made the session or navigated it.
    ///
    /// Deliberately not "the current URL". That is page state: a redirect, a
    /// script or a human at the viewer moves it, and asking the session is the
    /// only way to know it. Recording *that* here would be a second answer that
    /// goes stale. What is recorded is an instruction, which does not.
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
    /// What is holding the engine process.
    ///
    /// A third axis, and genuinely a third question. `placement` says *where*
    /// the session runs, `enclosing_box` says what h5i was standing in when it
    /// opened one, and this says what confines the engine there. A host session
    /// can be confined; a boxed one is confined by its box.
    #[serde(default)]
    pub confinement: crate::browser_sandbox::Confinement,
    /// The box **h5i itself was running inside** when this session was opened,
    /// if any.
    ///
    /// Different from `placement`, and the difference is who is describing
    /// what. `Placement::Box` means h5i, from outside, put the session in a box
    /// it can see the policy of and carries every verb into. This means h5i was
    /// *already* in a box and opened a session beside itself.
    ///
    /// Mechanically the second is a host session: same namespace, same
    /// loopback, same registry, and the verbs go straight to the engine. What
    /// it is not is uncontained, and a record that said "no containment beyond
    /// the engine" there would be understating what is true. So the box is
    /// named — and nothing more is claimed about it, because from inside, the
    /// policy is sealed and h5i cannot read what its own boundary enforces.
    #[serde(default)]
    pub enclosing_box: Option<String>,
    pub control: Control,
    /// Where this machine can read the session's own logs, when it can.
    #[serde(default)]
    pub logs: Logs,
}

/// The engine's two logs, **as this machine sees them**.
///
/// Recorded at start rather than derived at read time, for the reason
/// [`Control::witness`] exists: a boxed session's logs live in the box's
/// `/tmp`, and re-deriving that path later means re-deriving a mapping that has
/// since been rewritten. `None` means this machine cannot read that log at all,
/// which an audit reports as **unavailable** rather than as an empty list. An
/// empty list looks like a quiet session; unavailable looks like what it is.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Logs {
    pub actions: Option<PathBuf>,
    pub requests: Option<PathBuf>,
}

impl Session {
    /// Where this session ran, in one clause, with nothing claimed that cannot
    /// be checked from wherever h5i was standing.
    pub fn where_it_ran(&self) -> String {
        use crate::browser_sandbox::Confinement;
        match (&self.placement, &self.enclosing_box) {
            (Placement::Box { name }, _) => format!("in box `{name}`"),
            // h5i was in the box too. Name it; claim nothing about it.
            (Placement::Host, Some(id)) => {
                format!("on this machine, which is box `{id}` — its policy is not readable here")
            }
            (Placement::Host, None) => match &self.confinement {
                Confinement::Process => {
                    // Named precisely, because the two things it does not do are
                    // the two a reader would otherwise assume it does.
                    "on this machine, in a process-tier sandbox (its files and its \
                     environment; not its network)"
                        .to_string()
                }
                Confinement::None { why } => {
                    format!("on this machine, unconfined — {why}")
                }
            },
        }
    }

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
    // Inside a box, the state directory is not a choice. `$HOME` there is the
    // host's path over a sealed overlay: `~/.local/state` is not writable, and a
    // session that cannot write its registry cannot start at all. What *is*
    // writable is the box's own `/tmp`, which is private to the box and lives
    // exactly as long as it does — which is also how long its sessions can.
    //
    // `temp_dir` rather than a literal `/tmp`, because it follows the redirect
    // the box was given rather than assuming what it was.
    if crate::env::in_env_box() {
        return Ok(std::env::temp_dir().join("h5i").join("browser"));
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
             Start one with `h5i browser open <url>`."
        ))
    } else {
        H5iError::Metadata(format!(
            "`{id}` is not a browser session on this machine. Live sessions: {}.",
            known.join(", ")
        ))
    }
}

/// The id of the default session, if one has been set.
pub fn read_default(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(root.join(DEFAULT_POINTER)).ok()?;
    let id = raw.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Point the default at this session.
pub fn set_default(root: &Path, id: &str) -> Result<(), H5iError> {
    fs::create_dir_all(root).map_err(H5iError::Io)?;
    fs::write(root.join(DEFAULT_POINTER), format!("{id}\n")).map_err(H5iError::Io)
}

/// Clear the default if it points at this session.
///
/// Used for one case only: the pointer names a record that is *gone*. A pointer
/// to a session that merely **ended** is deliberately kept, because following it
/// is what lets the next bare verb say "the session you were on was closed"
/// instead of "no session is open". The first tells an agent what happened; the
/// second reads like it never had one.
///
/// Conditional on the id, because closing a named session must not disturb a
/// default someone else is using.
pub fn clear_default_if(root: &Path, id: &str) {
    if read_default(root).as_deref() == Some(id) {
        let _ = fs::remove_file(root.join(DEFAULT_POINTER));
    }
}

/// The live session carrying this name, if any.
pub fn find_by_name(root: &Path, name: &str) -> Option<Session> {
    list(root)
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.state.is_live() && s.name.as_deref() == Some(name))
}

/// Turn what the caller said (or did not say) into a session a verb may act on.
///
/// `selector` is a `--session` value or `$H5I_BROWSER_SESSION`: a name, or an
/// opaque id for the case where something already recorded one. With neither,
/// the default pointer answers.
///
/// There is deliberately **no** "if only one session is live, use it" rule. It
/// reads as helpful and is the same hazard as a moving default: an agent that
/// opened one session, had it end, and opened another under a different name
/// would find its next verb quietly landing somewhere it never asked for.
pub fn resolve(root: &Path, selector: Option<&str>) -> Result<Session, SessionGone> {
    let selector = selector
        .map(str::to_string)
        .or_else(|| std::env::var("H5I_BROWSER_SESSION").ok())
        .filter(|s| !s.trim().is_empty());

    match selector {
        Some(wanted) => {
            // A name first: that is what a person typed. An id is accepted too,
            // because `--json` and receipts hand one back and it should work
            // where it is pasted.
            if let Some(session) = find_by_name(root, &wanted) {
                return Ok(session);
            }
            match read(root, &wanted) {
                Ok(session) => open_it(root, session),
                Err(_) => Err(SessionGone::Unknown(unknown_selector(root, &wanted))),
            }
        }
        None => match read_default(root) {
            Some(id) => match read(root, &id) {
                Ok(session) => open_it(root, session),
                // The pointer outlived what it pointed at. Say so plainly
                // rather than reporting an id nobody typed.
                Err(_) => {
                    clear_default_if(root, &id);
                    Err(SessionGone::Unknown(no_default(root)))
                }
            },
            None => Err(SessionGone::Unknown(no_default(root))),
        },
    }
}

/// The error for a `--session` that names nothing.
fn unknown_selector(root: &Path, wanted: &str) -> H5iError {
    let live: Vec<String> = list(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.state.is_live())
        .map(|s| match &s.name {
            Some(name) => format!("{name} ({})", s.id),
            None => format!("{} (unnamed)", s.id),
        })
        .collect();
    if live.is_empty() {
        H5iError::Metadata(format!(
            "no browser session called `{wanted}`, and none is running. \
             Open one with `h5i browser open <url>`."
        ))
    } else {
        H5iError::Metadata(format!(
            "no browser session called `{wanted}`. Running: {}.",
            live.join(", ")
        ))
    }
}

/// The error for a verb sent with nothing to send it to.
fn no_default(root: &Path) -> H5iError {
    let named: Vec<String> = list(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.state.is_live())
        .filter_map(|s| s.name)
        .collect();
    if named.is_empty() {
        H5iError::Metadata(
            "no browser session is open. Open one with `h5i browser open <url>`.".into(),
        )
    } else {
        H5iError::Metadata(format!(
            "no default browser session, and the ones running are named. \
             Say which: {}.",
            named
                .iter()
                .map(|n| format!("`--session {n}`"))
                .collect::<Vec<_>>()
                .join(", ")
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
    open_it(root, session)
}

/// The liveness half of [`open_live`], over a record already in hand.
fn open_it(root: &Path, session: Session) -> Result<Session, SessionGone> {
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
    /// The selector names a session that has ended.
    Ended {
        state: State,
        reason: Option<String>,
        /// The opaque id, which is what `--restore` takes: a name can be reused
        /// and so cannot carry storage forward unambiguously.
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
                     Open a new one with `h5i browser open <url>`, or carry this one's \
                     storage forward with `h5i browser open <url> --restore {id}`."
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

/// One handover, as the host recorded it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ControlEvent {
    pub at: String,
    /// `agent` or `human`.
    pub holder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Append a handover. Best effort by design: a `take` must not fail because the
/// journal could not be written, since the point of `take` is that a person
/// wants the pointer *now*.
///
/// That makes this the one log here that is **not** fail-closed, and the audit
/// says so rather than presenting it beside two logs that are.
pub fn journal_control(dir: &Path, holder: &str, note: Option<&str>) {
    let event = ControlEvent {
        at: now(),
        holder: holder.to_string(),
        note: note.map(str::to_string),
    };
    let Ok(line) = serde_json::to_string(&event) else {
        return;
    };
    let _ = fs::create_dir_all(dir);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(CONTROL_JOURNAL))
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Every handover this session recorded, oldest first.
pub fn control_journal(dir: &Path) -> Vec<ControlEvent> {
    let Ok(text) = fs::read_to_string(dir.join(CONTROL_JOURNAL)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Which of an audit's sources this machine could actually read.
///
/// Reported beside the events, because "no rows" and "no log" are different
/// findings and an audit that renders them the same way reports coverage it
/// does not have.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Sources {
    pub actions: Availability,
    pub requests: Availability,
    pub control: Availability,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    /// Read, and it had content.
    Read,
    /// Read, and it was empty. The session really did nothing of this kind.
    #[default]
    Empty,
    /// Not readable from here. Nothing can be concluded from its silence.
    Unavailable,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Read => "read",
            Availability::Empty => "empty",
            Availability::Unavailable => "unavailable",
        }
    }

    fn of(text: &Option<String>) -> Availability {
        match text {
            None => Availability::Unavailable,
            Some(t) if t.trim().is_empty() => Availability::Empty,
            Some(_) => Availability::Read,
        }
    }
}

/// Everything recorded about one session, in one ordered timeline.
#[derive(Serialize, Debug, Clone)]
pub struct Audit {
    pub session: Session,
    pub sources: Sources,
    /// Oldest first. Each row carries the lane it came from and, where the
    /// source said so, the event that caused it.
    pub events: Vec<crate::browser_events::ViewerEvent>,
    /// How many rows the cap discarded. Rendered, never hidden.
    pub dropped: u64,
}

/// Most rows an audit holds before it starts dropping the oldest.
///
/// A page that loads a thousand subresources must not make one session's audit
/// unreadable, and a cap that dropped silently would report a quiet session
/// where there was a loud one — so the count comes back in [`Audit::dropped`].
const AUDIT_CAPACITY: usize = 5000;

/// Assemble the whole record of a session: what the agent asked for, what the
/// engine decided, who was driving, and how it ended.
///
/// The merge reuses [`crate::browser_events`], which is the same machinery the
/// console renders from. That is deliberate and it is the point: two surfaces
/// that each assemble their own view of one session are two surfaces that can
/// disagree, and a disagreement between them is unfalsifiable for whoever is
/// trying to review the session.
///
/// **The lanes are not merged.** The action and request logs are the engine's
/// own account; the handovers and the lifecycle are h5i's, written from
/// outside. Every row carries which, because a claim rendered as an observation
/// is the one error this product cannot afford.
pub fn audit(root: &Path, session: &Session) -> Audit {
    use crate::browser_events as ev;

    let dir = dir(root, &session.id);
    let read = |path: &Option<PathBuf>| -> Option<String> {
        path.as_ref().and_then(|p| fs::read_to_string(p).ok())
    };
    let actions = read(&session.logs.actions);
    let requests = read(&session.logs.requests);
    let handovers = control_journal(&dir);
    let read_at = now();

    // Gathered with their times first and ordered afterwards, because the
    // sources are three files and a timeline grouped by file is not a timeline.
    // A stable sort keeps each source's own order inside a tie, which is what
    // keeps a request ahead of its response and a verb ahead of the fetches it
    // caused.
    let mut rows: Vec<Row> = Vec::new();

    rows.push(Row::host(
        &session.started_at,
        ev::Draft::host(ev::EventKind::Lifecycle {
            state: "opened".into(),
            reason: Some(format!("{} — {}", session.url, session.where_it_ran())),
        }),
    ));

    // The action log before the request log: the causal map is filled by the
    // first and read by the second. The one ordering dependency here, and the
    // same one `BoxStream::poll` states in its own comment.
    let mut caused = std::collections::BTreeMap::new();
    if let Some(text) = &actions {
        for draft in ev::ingest_light_actions_with(text, &mut caused) {
            rows.push(Row::engine(&session.started_at, &read_at, draft));
        }
    }
    if let Some(text) = &requests {
        for draft in ev::ingest_request_log_with(text, &caused) {
            rows.push(Row::engine(&session.started_at, &read_at, draft));
        }
    }

    for handover in &handovers {
        rows.push(Row::host(
            &handover.at,
            ev::Draft::host(ev::EventKind::Control {
                holder: handover.holder.clone(),
                note: handover.note.clone(),
            }),
        ));
    }

    if let (Some(ended_at), state) = (&session.ended_at, session.state)
        && !state.is_live()
    {
        rows.push(Row::host(
            ended_at,
            ev::Draft::host(ev::EventKind::Lifecycle {
                state: state.as_str().into(),
                reason: session.end_reason.clone(),
            }),
        ));
    }

    rows.sort_by_key(|row| row.order);

    let mut log = ev::EventLog::new(AUDIT_CAPACITY);
    for row in rows {
        log.extend([row.draft], &row.observed_at);
    }

    Audit {
        session: session.clone(),
        sources: Sources {
            actions: Availability::of(&actions),
            requests: Availability::of(&requests),
            control: if handovers.is_empty() {
                Availability::Empty
            } else {
                Availability::Read
            },
        },
        events: log.since(0).into_iter().cloned().collect(),
        dropped: log.dropped(),
    }
}

/// One audit row, with the instant it sorts on.
///
/// `order` is a parsed instant rather than the string, because the two clocks
/// print at different precisions: `2026-01-01T00:00:00Z` sorts *after*
/// `2026-01-01T00:00:00.500000Z` as text, which would put a handover after
/// engine rows it actually preceded.
struct Row {
    order: i64,
    observed_at: String,
    draft: crate::browser_events::Draft,
}

impl Row {
    /// A row h5i wrote itself: its time is an observation, and it is the same
    /// value the timeline sorts on.
    fn host(at: &str, draft: crate::browser_events::Draft) -> Row {
        Row {
            order: micros(at),
            observed_at: at.to_string(),
            draft,
        }
    }

    /// A row from one of the engine's logs.
    ///
    /// It sorts on the engine's own claim, because that is the only clock that
    /// can order the two engine logs against each other. What it is *stamped*
    /// with is `read_at`, the moment h5i read the file, because `observed_at`
    /// means "when h5i saw this" everywhere else in this module and an audit
    /// must not be the one place it quietly means something else. A row with no
    /// claim at all falls back to the session's start, which puts it at the top
    /// rather than pretending to a position it cannot support.
    fn engine(started_at: &str, read_at: &str, draft: crate::browser_events::Draft) -> Row {
        let order = draft
            .claimed_at
            .as_deref()
            .map(micros)
            .unwrap_or_else(|| micros(started_at));
        Row {
            order,
            observed_at: read_at.to_string(),
            draft,
        }
    }
}

/// An RFC3339 stamp as microseconds since the epoch, or `0` when it will not
/// parse. Zero rather than a failure: a row with an unreadable time still
/// belongs in the audit, at the top, where its lack of a position is visible.
fn micros(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|t| t.timestamp_micros())
        .unwrap_or(0)
}

/// The host's clock, RFC3339 with microseconds.
///
/// Microseconds because these stamps have to interleave with the engine's, and
/// the engine writes a whole agent loop inside one second. At second precision
/// every host row lands on the `.000000` boundary and sorts ahead of engine
/// rows it actually followed — a timeline that is arithmetically correct and
/// tells the wrong story, which is worse than one that is obviously broken.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
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
            name: None,
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
            confinement: crate::browser_sandbox::Confinement::Process,
            enclosing_box: None,
            control: Control::default(),
            logs: Logs::default(),
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

    fn named(root: &Path, name: &str) -> Session {
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        s.name = Some(name.to_string());
        write(root, &s).unwrap();
        s
    }

    #[test]
    fn the_ordinary_case_names_nothing_and_lands_on_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        write(root, &session(&id, Placement::Host)).unwrap();
        set_default(root, &id).unwrap();

        assert_eq!(resolve(root, None).unwrap().id, id);
    }

    #[test]
    fn a_name_addresses_a_session_and_so_does_its_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let auth = named(root, "auth");
        let public = named(root, "public");

        assert_eq!(resolve(root, Some("auth")).unwrap().id, auth.id);
        assert_eq!(resolve(root, Some("public")).unwrap().id, public.id);
        // The id from `--json` works where it is pasted.
        assert_eq!(resolve(root, Some(&auth.id)).unwrap().id, auth.id);
    }

    #[test]
    fn there_is_no_lone_session_shortcut() {
        // One live session and no default: still an error, not a guess. A rule
        // that silently picks "the only one" moves under an agent the moment a
        // second session exists.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        named(root, "auth");
        let why = resolve(root, None).unwrap_err().to_string();
        assert!(why.contains("--session auth"), "{why}");
    }

    #[test]
    fn the_default_outlives_the_session_so_the_next_verb_can_say_how_it_ended() {
        // Clearing the pointer here would turn "the session you were on was
        // closed" into "no session is open", which reads as though there never
        // was one. The first tells an agent what happened to its page.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        write(root, &s).unwrap();
        set_default(root, &id).unwrap();

        end(root, &mut s, State::Closed, "closed by the user");
        match resolve(root, None) {
            Err(SessionGone::Ended { state, id: gone, .. }) => {
                assert_eq!(state, State::Closed);
                assert_eq!(gone, id);
            }
            other => panic!("expected the ending, got {other:?}"),
        }
    }

    #[test]
    fn a_default_naming_a_record_that_is_gone_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        set_default(root, "br_never").unwrap();

        let why = resolve(root, None).unwrap_err().to_string();
        assert!(why.contains("h5i browser open"), "{why}");
        assert_eq!(read_default(root), None, "a pointer to nothing is not kept");
    }

    #[test]
    fn a_name_can_be_reused_once_its_session_has_ended() {
        // This is what makes a name comfortable to type, and exactly why the
        // id is what gets written into the record.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut first = named(root, "auth");
        end(root, &mut first, State::Closed, "closed by the user");

        let second = named(root, "auth");
        assert_ne!(second.id, first.id);
        assert_eq!(resolve(root, Some("auth")).unwrap().id, second.id);
    }

    /// A session with both engine logs, a handover between two verbs, and an
    /// ending. The point of the test is the **order**: grouped by source is not
    /// a timeline, and "a human was driving between these two verbs" is the
    /// question an audit exists to answer.
    #[test]
    fn the_audit_interleaves_the_engine_and_the_host_by_time() {
        use crate::browser_events::EventKind as K;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let dir = dir(root, &id);

        std::fs::write(
            dir.join(ACTIONS_FILE),
            "{\"seq\":0,\"at\":\"2026-01-01T00:00:01.000000Z\",\"phase\":\"result\",\"verb\":\"snapshot\",\"ok\":true}\n\
             {\"seq\":1,\"at\":\"2026-01-01T00:00:03.000000Z\",\"phase\":\"result\",\"verb\":\"click\",\"target\":\"@e1\",\"ok\":true,\"requests\":[7]}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(RECEIPTS_FILE),
            "{\"seq\":7,\"at\":\"2026-01-01T00:00:03.500000Z\",\"phase\":\"request\",\"initiator\":\"navigation\",\"method\":\"GET\",\"url\":\"https://example.com/\",\"allowed\":true}\n",
        )
        .unwrap();
        journal_control(&dir, "human", Some("taken"));

        let mut session = session(&id, Placement::Host);
        session.started_at = "2026-01-01T00:00:00.000000Z".into();
        session.logs = Logs {
            actions: Some(dir.join(ACTIONS_FILE)),
            requests: Some(dir.join(RECEIPTS_FILE)),
        };
        write(root, &session).unwrap();
        end(root, &mut session, State::Closed, "closed by the user");

        let audit = audit(root, &read(root, &id).unwrap());
        let shape: Vec<&str> = audit
            .events
            .iter()
            .map(|e| match &e.kind {
                K::Lifecycle { state, .. } if state == "opened" => "open",
                K::Lifecycle { .. } => "end",
                K::Control { .. } => "control",
                K::AgentAction { action, .. } if action.starts_with("snapshot") => "snapshot",
                K::AgentAction { .. } => "click",
                K::Request { .. } => "request",
                _ => "other",
            })
            .collect();

        // The handover was journalled with today's clock, so it lands after the
        // 2026-01-01 rows; what matters here is that the engine rows themselves
        // came out in time order rather than file order.
        let engine: Vec<&&str> = shape
            .iter()
            .filter(|k| matches!(**k, "snapshot" | "click" | "request"))
            .collect();
        assert_eq!(
            engine,
            vec![&"snapshot", &"click", &"request"],
            "grouped by source rather than ordered by time: {shape:?}"
        );
        assert_eq!(shape.first(), Some(&"open"));
        assert_eq!(shape.last(), Some(&"end"));

        // The causal link the action log carried is resolved, not inferred.
        let request = audit
            .events
            .iter()
            .find(|e| matches!(e.kind, K::Request { .. }))
            .expect("the request row");
        assert!(
            request.caused_by.is_some(),
            "the click that caused this fetch is not linked to it"
        );
    }

    /// A log this machine cannot read is `unavailable`, never an empty list.
    /// An empty list looks like a session that did nothing.
    #[test]
    fn an_unreadable_log_is_reported_as_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut session = session(&id, Placement::Host);
        session.logs = Logs::default();
        write(root, &session).unwrap();

        let audit = audit(root, &session);
        assert_eq!(audit.sources.actions, Availability::Unavailable);
        assert_eq!(audit.sources.requests, Availability::Unavailable);
        assert_eq!(audit.sources.control, Availability::Empty);
    }

    /// The two lanes stay apart. A row h5i wrote from outside must never be
    /// presented as something the engine reported about itself.
    #[test]
    fn host_rows_and_engine_rows_keep_their_lanes() {
        use crate::browser_events::{EventKind as K, Lane};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let dir = dir(root, &id);
        std::fs::write(
            dir.join(ACTIONS_FILE),
            "{\"seq\":0,\"at\":\"2026-01-01T00:00:01.000000Z\",\"phase\":\"result\",\"verb\":\"snapshot\",\"ok\":true}\n",
        )
        .unwrap();
        let mut session = session(&id, Placement::Host);
        session.logs.actions = Some(dir.join(ACTIONS_FILE));
        write(root, &session).unwrap();
        journal_control(&dir, "human", None);

        let audit = audit(root, &session);
        for event in &audit.events {
            let expected = match event.kind {
                K::Lifecycle { .. } | K::Control { .. } => Lane::HostObserved,
                _ => Lane::BoxClaimed,
            };
            assert_eq!(event.lane, expected, "{:?} is in the wrong lane", event.kind);
        }
    }

    /// A session opened from inside a box is mechanically a host session and is
    /// **not** uncontained. Saying "no containment beyond the engine" there
    /// would understate what is true, which is the same class of error as
    /// overstating it — just in the direction that happens to be safe.
    #[test]
    fn a_session_opened_from_inside_a_box_names_the_box_it_is_in() {
        let mut inside = session("br_inside", Placement::Host);
        inside.enclosing_box = Some("env/human/web".into());
        let said = inside.where_it_ran();
        assert!(said.contains("env/human/web"), "{said}");
        assert!(
            !said.contains("no containment"),
            "understating a box is still describing it wrong: {said}"
        );
        // And nothing is claimed about what that box enforces, because from in
        // there the policy is sealed.
        assert!(said.contains("not readable here"), "{said}");
        assert_eq!(
            Session::lane_for(&inside.placement, false),
            Lane::EngineClaimed
        );
    }

    /// The channel is recorded rather than re-derived. A verb that guessed the
    /// address would be a second place that has to agree with the first, and
    /// the two failures it produces — a port nothing is listening on, a socket
    /// path that is not there — both look like a session that is not running.
    #[test]
    fn the_channel_travels_with_the_record() {
        assert_eq!(Channel::Port.flag(), "--control-file");
        assert_eq!(Channel::Socket.flag(), "--control-socket");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let id = new_id(root).unwrap();
        let mut s = session(&id, Placement::Host);
        s.control.channel = Channel::Socket;
        s.control.file = Some(dir(root, &id).join("control.sock"));
        write(root, &s).unwrap();

        let back = read(root, &id).unwrap();
        assert_eq!(back.control.channel, Channel::Socket);
        assert_eq!(back.control.file, s.control.file);
    }

    /// A record written before the channel existed still loads, and loads as
    /// the channel those sessions actually used.
    #[test]
    fn a_record_without_a_channel_reads_as_a_port() {
        let raw = r#"{"id":"br_old","engine":"h5i-light","placement":{"kind":"host"},
            "lane":"engine-claimed","url":"https://example.com/","started_at":"2026-01-01T00:00:00Z",
            "expires_at":null,"storage":"ephemeral","policy_digest":"sha256:x","restored_from":null,
            "state":"closed","ended_at":null,"end_reason":null,
            "control":{"file":null,"witness":null,"pid":null}}"#;
        let session: Session = serde_json::from_str(raw).expect("an older record still loads");
        assert_eq!(session.control.channel, Channel::Port);
        assert_eq!(session.enclosing_box, None);
    }

    /// A host that cannot confine runs the session anyway and says so, with the
    /// reason. A sandbox nobody can see is indistinguishable from one that was
    /// never applied.
    #[test]
    fn an_unconfined_session_says_so_and_why() {
        let mut outside = session("br_outside", Placement::Host);
        outside.confinement = crate::browser_sandbox::Confinement::None {
            why: "this host has no Landlock".into(),
        };
        let said = outside.where_it_ran();
        assert!(said.contains("unconfined"), "{said}");
        assert!(said.contains("no Landlock"), "{said}");
    }

    /// And a confined one names the two things it does *not* contain, because
    /// those are the two a reader would otherwise assume it does.
    #[test]
    fn a_confined_session_names_what_it_does_not_contain() {
        let inside = session("br_inside", Placement::Host);
        assert_eq!(
            inside.confinement,
            crate::browser_sandbox::Confinement::Process
        );
        let said = inside.where_it_ran();
        assert!(said.contains("sandbox"), "{said}");
        assert!(said.contains("not its network"), "{said}");
        // The sandbox is not evidence: it corroborates no part of the log.
        assert_eq!(
            Session::lane_for(&inside.placement, false),
            Lane::EngineClaimed
        );
    }

    /// `--in` and "I am already in a box" are different facts, and the record
    /// keeps them apart: one is h5i placing a session somewhere it can see the
    /// policy of, the other is h5i already being there.
    #[test]
    fn placed_in_a_box_and_opened_inside_one_are_not_the_same_row() {
        let placed = session(
            "br_placed",
            Placement::Box {
                name: "web".into(),
            },
        );
        assert!(placed.where_it_ran().contains("in box `web`"));
        assert!(placed.enclosing_box.is_none());
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
