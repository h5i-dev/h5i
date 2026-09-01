//! The share session on disk: which box is shared, over what, and to whom.

use std::path::{Path, PathBuf};

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Sibling of the receipt log; never under a path the box can write.
const SESSION_FILE: &str = "share.json";
/// The format `share.json` is written in, and the only one this h5i will read.
///
/// Checked on the way in as well as written on the way out. See [`read_state`]
/// for why a discriminator nobody reads is not a discriminator.
pub const SESSION_VERSION: u8 = 1;
/// Held for the read-modify-write of the grant table. Two `revoke` calls racing
/// must not end with one of them silently lost.
const LOCK_FILE: &str = "share.lock";

pub fn session_path(env_dir: &Path) -> PathBuf {
    env_dir.join(SESSION_FILE)
}

/// Which transport is carrying a share, recorded so the receipt can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// iroh: QUIC between the two h5i processes, end-to-end encrypted.
    P2p,
    /// A Cloudflare quick tunnel. Reaches a plain browser; terminates TLS at
    /// Cloudflare, so it is not end to end and the docs say so.
    Tunnel,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::P2p => "p2p",
            Transport::Tunnel => "tunnel",
        }
    }
}

/// One peer's capability. Revoking one does not touch the others.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    /// Short hex, the handle a human types to revoke this one.
    pub id: String,
    /// What the sharer called this peer, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// SHA-256 of the secret, hex. The secret itself was printed once.
    pub secret_sha256: String,
    /// Unix seconds.
    pub expires_at: i64,
    #[serde(default)]
    pub revoked: bool,
}

/// A share that is running, or was.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShareSession {
    pub v: u8,
    pub box_id: String,
    /// The port inside the box. The bridge is pinned to it for its whole life.
    pub port: u16,
    pub transport: Transport,
    /// The endpoint id (P2P) or the public URL (tunnel). Carries no secret:
    /// for the tunnel this is the bare origin, not the URL with the token.
    pub endpoint: String,
    pub started_at: String,
    /// The host-side `h5i box share` process. Used to tell a live share from a
    /// stale file left by a crash.
    pub pid: u32,
    /// Set once the serving process has decided to stop but has not yet removed
    /// this file. `pid` is still alive throughout, so nothing else could tell.
    #[serde(default)]
    pub winding_up: bool,
    /// The box is claimed and the transport is not up yet.
    #[serde(default)]
    pub starting: bool,
    pub grants: Vec<Grant>,
}

/// Why a presented secret was not honoured. Each is separate because they are
/// separate problems: an expired ticket needs a new one, a revoked ticket means
/// the sharer meant to cut you off, and an unknown secret means you have the
/// wrong ticket entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    Unknown,
    /// The grant table could not be read at all. The disk is full, the file
    /// is unreadable, the process is out of descriptors. Not a fact about the
    /// visitor's ticket, and it used to be reported as one: the visitor was
    /// told to ask for a new invite, the sharer's terminal said "that ticket
    /// is not one this share knows", and the receipt recorded an unknown
    /// ticket. For a machine problem on the sharer's own side.
    TableUnreadable,
    Expired,
    Revoked,
    /// Nobody presented anything. Separate from [`Denied::Unknown`] because on
    /// a public tunnel URL this is the commonest event of the whole session, a
    /// scanner fetching `/`, and folding it into "unknown ticket" made the
    /// dominant row of the receipt a sentence that was usually false: no ticket
    /// was presented, so none was unknown.
    NoCredential,
    /// The grant table is *gone*, not broken. Separate from
    /// [`Denied::TableUnreadable`] because a missing file is what
    /// `share stop --force` leaves behind, and what the last moment of every
    /// ordinary stop looks like: calling that a machine problem accused the
    /// machine of a deliberate operator action.
    ShareOver,
}

impl Denied {
    /// What the peer is told. Deliberately the same shape for all of them at the
    /// wire level (see [`crate::gate`]); the distinction is for the sharer's
    /// terminal and the receipt, not for whoever is probing.
    pub fn explain(self) -> &'static str {
        match self {
            Denied::Unknown => "that ticket is not one this share knows",
            Denied::TableUnreadable => {
                "this share cannot read its own grant table — that is a problem on the \
                 sharing machine, not with your invite"
            }
            Denied::ShareOver => "this share has ended",
            Denied::NoCredential => "this share needs an invite link",
            Denied::Expired => "that ticket has expired — ask for a new one",
            Denied::Revoked => "that ticket was revoked",
        }
    }
}

pub fn hash_secret(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    format!("{:x}", h.finalize())
}

/// Does a presented secret match an expected one?
///
/// Exposed so the joiner's proxy compares its local token the same way the
/// grant table compares a ticket. Two credential comparisons written two
/// different ways is how one of them ends up being the sloppy one.
pub fn secret_matches(presented: &str, expected: &str) -> bool {
    digest_matches(&hash_secret(presented), &hash_secret(expected))
}

/// Constant-time comparison of two hex digests.
///
/// They are digests of a high-entropy secret, so a timing oracle here is a
/// stretch even in theory. It is three lines, and the alternative is explaining
/// why an early-exit `==` on a credential comparison is fine.
fn digest_matches(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

impl ShareSession {
    pub fn new(
        box_id: &str,
        port: u16,
        transport: Transport,
        endpoint: &str,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> ShareSession {
        ShareSession {
            v: SESSION_VERSION,
            box_id: box_id.to_string(),
            port,
            transport,
            endpoint: endpoint.to_string(),
            started_at: started_at.to_rfc3339(),
            pid: std::process::id(),
            winding_up: false,
            starting: false,
            grants: Vec::new(),
        }
    }

    /// Resolve a presented secret to the grant it belongs to.
    ///
    /// Every grant is examined even after a match, so the work done does not
    /// depend on *which* grant matched. Expiry and revocation are decided here
    /// too: a caller that got a grant back has an answer it can act on without
    /// a second lookup that could disagree.
    pub fn authorize(&self, secret: &str, now: i64) -> Result<&Grant, Denied> {
        let presented = hash_secret(secret);
        let mut found: Option<&Grant> = None;
        for g in &self.grants {
            if digest_matches(&g.secret_sha256, &presented) {
                found = Some(g);
            }
        }
        let g = found.ok_or(Denied::Unknown)?;
        if g.revoked {
            return Err(Denied::Revoked);
        }
        if g.expires_at <= now {
            return Err(Denied::Expired);
        }
        Ok(g)
    }

    /// How many grants can still admit somebody.
    pub fn live_grants(&self, now: i64) -> usize {
        self.grants
            .iter()
            .filter(|g| !g.revoked && g.expires_at > now)
            .count()
    }

    /// True once no grant could admit anyone. The bridge polls this so a
    /// revoked or wholly expired share drops its live connections instead of
    /// serving them until the peer gets bored.
    pub fn is_spent(&self, now: i64) -> bool {
        !self.grants.iter().any(|g| !g.revoked && g.expires_at > now)
    }
}

/// `45s`, `4m`, `1h30m`: whichever unit does not read as zero.
///
/// Lives here rather than in the CLI because two callers render "how long is
/// left" and they were not the same function: the announce line was fixed to
/// stop saying `0m` for a 45-second ticket, and `share status` was left on
/// integer minutes, so every share in existence read `0m left` for its final
/// minute. Right next to `expired` in the same column, at the exact moment
/// somebody runs `status` to decide whether to re-mint.
pub fn humanise(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

/// Mint a grant, returning it and the secret that must be printed *now*.
pub fn mint_grant(label: Option<String>, expires_at: i64) -> Result<(Grant, String), H5iError> {
    mint_grant_unlike(&[], label, expires_at)
}

/// Mint a grant whose id no grant in `existing` already uses.
pub fn mint_grant_unlike(
    existing: &[Grant],
    label: Option<String>,
    expires_at: i64,
) -> Result<(Grant, String), H5iError> {
    let secret = crate::ticket::mint_secret()?;
    let mut id = h5i_core::token::hex(4)?;
    // Bounded, because an unbounded retry against a table that somehow holds
    // every id would spin forever. Sixty-four attempts against even a hundred
    // thousand grants is a certainty many times over.
    for _ in 0..64 {
        if !existing.iter().any(|g| g.id == id) {
            break;
        }
        id = h5i_core::token::hex(4)?;
    }
    if existing.iter().any(|g| g.id == id) {
        return Err(H5iError::Metadata(
            "could not mint a grant id this share is not already using. Stop the share and \
             start a fresh one."
                .into(),
        ));
    }
    Ok((
        Grant {
            id,
            label,
            secret_sha256: hash_secret(&secret),
            expires_at,
            revoked: false,
        },
        secret,
    ))
}

// ─── the file ───────────────────────────────────────────────────────────────

/// An exclusive lock over the grant table, held for as long as this value is.
pub struct Lock {
    /// Held only to keep the descriptor open: closing it is what unlocks, so
    /// the field is the lock.
    #[allow(dead_code)]
    file: std::fs::File,
}

/// How long a caller waits for the grant table before giving up.
///
/// The same five seconds the retry loop it replaces added up to, because several
/// call sites' comments quote that number to the operator.
///
/// Unix only, like the `flock` it bounds and like `share_gate`'s `GATE_WAIT` one
/// layer down: the other branch takes no lock, so there is nothing to wait for.
/// Without the gate it is dead code on Windows, which `cargo check --target
/// x86_64-pc-windows-msvc` under `-D warnings` refuses.
#[cfg(unix)]
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl Lock {
    pub fn acquire(env_dir: &Path) -> Result<Lock, H5iError> {
        let path = env_dir.join(LOCK_FILE);
        // `create(true).truncate(false)`: the file is a handle to lock, never a
        // place to put anything, and truncating it would be one process
        // rewriting another's open file for no reason.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // Non-blocking in a deadline loop rather than a blocking `flock`,
            // for the reason `share_gate` gives: a blocking one cannot be given
            // a deadline, and a verb that waits forever on a wedged share is
            // the thing operators file bugs about.
            let deadline = std::time::Instant::now() + LOCK_WAIT;
            let mut attempt: u32 = 0;
            loop {
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    return Ok(Lock { file });
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                    return Err(H5iError::with_path(err, &path));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(H5iError::Metadata(format!(
                        "another h5i is holding this box's share lock and has held it for {}s. \
                         Try again in a moment. (The lock is released by the kernel when its \
                         holder exits, so there is never a file to delete by hand.)",
                        LOCK_WAIT.as_secs()
                    )));
                }
                // Backed off from a millisecond, not a flat fifty.
                attempt = attempt.wrapping_add(1);
                let step = 1u64 << attempt.min(4); // 2, 4, 8, 16, 16 ms
                let spread = (u64::from(std::process::id()).wrapping_mul(2_654_435_761)
                    ^ u64::from(attempt).wrapping_mul(40_503))
                    % step.max(1);
                std::thread::sleep(std::time::Duration::from_micros(
                    (step * 1_000 / 2) + spread * 500,
                ));
            }
        }
        #[cfg(not(unix))]
        {
            // No advisory lock here, exactly as `share_gate` does on this
            // branch. `h5i box share` refuses to start on a platform with
            // neither a namespace to enter nor a process tree to attribute a
            // socket to, so nothing reaches this with a share to protect.
            Ok(Lock { file })
        }
    }
}

/// A pid that `kill` will treat as one process, or nothing.
fn as_pid(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok().filter(|p| *p > 0)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    match as_pid(pid) {
        Some(p) => (unsafe { libc::kill(p as libc::pid_t, 0) }) == 0,
        None => false,
    }
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    as_pid(pid).is_some()
}

/// Read the session, if this box has one. A malformed file reads as absent
/// rather than as an error: the caller's next move is to write a fresh one.
pub fn read(env_dir: &Path) -> Option<ShareSession> {
    match read_state(env_dir) {
        ReadState::Present(s) => Some(*s),
        _ => None,
    }
}

/// Why there is no grant table, when there is none.
///
/// The three are different facts and were collapsed into one: a *missing*
/// file is what `share stop --force` leaves, and for the second before the
/// serving process notices, every visitor request was recorded as "this share
/// could not read its own grant table. A problem on the sharing machine".
/// The machine was fine; somebody had stopped the share.
pub enum ReadState {
    Present(Box<ShareSession>),
    /// No file. The share is over, or is being stopped right now.
    Gone,
    /// A file that cannot be read or cannot be understood. A full disk, no
    /// descriptors, a permission problem, or a record from a version this h5i
    /// does not know.
    Unreadable,
}

impl ReadState {
    /// No file at all: the share is over, or is being stopped right now.
    pub fn is_gone(&self) -> bool {
        matches!(self, ReadState::Gone)
    }
}

pub fn read_state(env_dir: &Path) -> ReadState {
    let raw = match std::fs::read(session_path(env_dir)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadState::Gone,
        Err(_) => return ReadState::Unreadable,
    };
    match serde_json::from_slice::<ShareSession>(&raw) {
        // The version is a discriminator or it is decoration, and it was
        // decoration: `v` was written and never read, so a record from a *newer*
        // h5i decoded here as a v1 one as long as the field names still matched.
        // Share management runs in a different process from the share, so during an
        // upgrade or a rollback an older binary could read a live v2 table, apply
        // v1 grant semantics to it, and atomically rewrite it. If a later format
        // keeps these fields and changes what they mean, that is authorization
        // state corrupted by a binary that could not have known better.
        Ok(s) if s.v != SESSION_VERSION => ReadState::Unreadable,
        Ok(s) => ReadState::Present(Box::new(s)),
        Err(_) => ReadState::Unreadable,
    }
}

/// Write the session atomically, owner-readable only.
pub fn write(env_dir: &Path, s: &ShareSession) -> Result<(), H5iError> {
    let path = session_path(env_dir);
    // Unique per writer. A fixed `share.json.tmp` meant two processes that
    // both held the lock, which the stale-break race above allowed, wrote
    // into the same temp file and one renamed the other's partial bytes into
    // place. A truncated `share.json` reads as *absent*, so every verb then
    // said "this box is not being shared", every peer was denied, and the
    // running share tore itself down.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let body = serde_json::to_vec_pretty(s)?;
    if let Err(e) = std::fs::write(&tmp, &body) {
        // Cleared on the way out. A failed write leaves a zero-byte temp
        // behind, and nothing in the tree (no `gc`, no `doctor`, no docs)
        // knows those exist or would ever remove one.
        let _ = std::fs::remove_file(&tmp);
        return Err(H5iError::with_path(e, &tmp));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // Rename last, so a reader sees either the old table or the new one and
    // never a half-written grant list.
    if let Err(e) = std::fs::rename(&tmp, &path) {
        // The write's cleanup, applied to the rename. Same reasoning: a
        // read-only directory fails here, not at the write, so the failure
        // most likely to leave a stray `share.json.tmp.NNN` was the one the
        // cleanup did not cover.
        let _ = std::fs::remove_file(&tmp);
        return Err(H5iError::with_path(e, &path));
    }
    Ok(())
}

/// Claim this box for a new share, under the lock.
pub fn claim(env_dir: &Path, s: &ShareSession, name: &str) -> Result<Option<u32>, H5iError> {
    // The share gate first, then the session lock, and never the other way
    // round: `h5i-core`'s lifecycle verbs take the gate and then `run.lock`, and
    // this crate never takes `run.lock` at all, so the order is total.
    //
    // What the gate buys: `apply`, `rebase`, `abort`, `rm` and `export` all
    // decide what to do by reading this record, and they hold the gate across
    // that decision and the work that follows. Without it the two were a check
    // and a gap, and a claim landing in the gap meant a visitor admitted while
    // `rebase` force-checked out the worktree.
    let _gate = h5i_core::share_record::share_gate(env_dir)?;
    // And the box must still exist. `rm` may have removed it while this start
    // was waiting for a tunnel URL; writing here would recreate the directory
    // it erased.
    if !env_dir.exists() {
        return Err(H5iError::Metadata(format!(
            "`{name}` is gone — it was removed while this share was starting. Nothing was \
             written."
        )));
    }
    let _lock = Lock::acquire(env_dir)?;
    let mut cleared = None;
    // On `read_state`, not on `read`.
    match read_state(env_dir) {
        ReadState::Present(existing) => {
            if is_live(&existing) {
                return Err(already_shared(&existing, name));
            }
            cleared = Some(existing.pid);
        }
        ReadState::Gone => {}
        ReadState::Unreadable => {
            return Err(H5iError::Metadata(format!(
                "{} exists and this h5i cannot read it. It may belong to a share that is still \
                 serving — a bridge rereads this table on every connection, so replacing it \
                 would hand that bridge the new share's tickets while it kept dialling the old \
                 share's port. Nothing is written. Check whether anything is serving this box, \
                 then `h5i box share stop {name} --force` to remove the record.",
                session_path(env_dir).display()
            )));
        }
    }
    write(env_dir, s)?;
    Ok(cleared)
}

/// The refusal a second `share` gets, with the box's real name in it.
///
/// One function rather than two spellings, because the copy in `run` said
/// `h5i box share stop <name>`, angle brackets and all, to somebody who then
/// had to guess what to type. The library does not know the name; the caller
/// does, so it passes it in.
pub fn already_shared(existing: &ShareSession, name: &str) -> H5iError {
    if existing.starting {
        return H5iError::Metadata(format!(
            "this box has just been claimed by another `h5i box share` (pid {}), which is \
             setting up its transport now. Wait for it to print its invite, or stop it with \
             `h5i box share stop {name}`.",
            existing.pid
        ));
    }
    if existing.winding_up {
        return H5iError::Metadata(format!(
            "this box's share is shutting down (pid {}) — it is writing its receipt and will be \
             gone in a moment. Run `h5i box share {name}` again then. (If it is still saying \
             this in a minute, that pid belongs to something else now: \
             `h5i box share stop {name} --force`.)",
            existing.pid
        ));
    }
    // The second sentence depends on the transport, because `grant` is refused
    // on a peer-to-peer share. Recommending it there sent people into a loop
    // between two refusals, each pointing at the other.
    let and_then = match existing.transport {
        Transport::Tunnel => format!(
            " Stop it first (`h5i box share stop {name}`), or add a peer to the share you \
             have (`h5i box share grant {name}`)."
        ),
        Transport::P2p => format!(
            " A peer-to-peer share carries one ticket, so adding somebody means stopping this \
             one and starting again: `h5i box share stop {name}`."
        ),
    };
    H5iError::Metadata(format!(
        "this box is already being shared by pid {} over {}.{and_then}",
        existing.pid,
        existing.transport.as_str()
    ))
}

/// How far a share's recorded start may be in this shell's future before the
/// two clocks are treated as disagreeing.
///
/// Not zero, because `started_at` is written to whole seconds and read back by
/// a different process; five seconds is well below any real clock step and
/// well above that rounding.
const CLOCK_SKEW_TOLERANCE: i64 = 5;

/// How far this shell's clock is *behind* the one the share started on, if it is behind at all.
pub fn started_in_the_future(s: &ShareSession, now: i64) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(&s.started_at).ok()?;
    let ahead = started.timestamp() - now;
    (ahead > CLOCK_SKEW_TOLERANCE).then_some(ahead)
}

/// How many times [`begin_winding_up`] will try before giving up.
///
/// This write is the only thing standing between a teardown and a `grant` in
/// another process minting a capability into a table that is about to be
/// deleted, so it gets more patience than an ordinary mutation: a transiently
/// held `share.lock` is exactly the case it must survive.
const WINDING_UP_ATTEMPTS: usize = 6;

/// Mark the share as winding up, so the other verbs stop pretending it serves.
pub fn begin_winding_up(env_dir: &Path, started_at: &str) -> Result<(), H5iError> {
    let mut last = None;
    for attempt in 0..WINDING_UP_ATTEMPTS {
        match update(env_dir, |s| {
            if s.pid != std::process::id() || s.started_at != started_at {
                // Not ours any more. Someone force-stopped this share and
                // another has claimed the box; marking it winding-up would
                // shut a door that is not this process's to shut.
                return Ok(());
            }
            s.winding_up = true;
            Ok(())
        }) {
            Ok(()) => return Ok(()),
            // Nothing to mark. The record is already gone (`share stop
            // --force`, or a teardown that got there first) which is the
            // state this was trying to reach.
            Err(e) if read_state(env_dir).is_gone() => {
                let _ = e;
                return Ok(());
            }
            Err(e) => {
                last = Some(e);
                if attempt + 1 < WINDING_UP_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
    Err(last
        .unwrap_or_else(|| H5iError::Metadata("could not mark this share as winding up".into())))
}

/// Remove the record outright, whatever it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forgotten {
    Deleted,
    NothingThere,
    /// Deleted, and a record was on disk again immediately afterwards.
    Reappeared,
}

pub fn forget(env_dir: &Path) -> Result<Forgotten, H5iError> {
    // The lock is taken if it can be, and its absence does not stop the
    // removal. This is the documented escape from a wedged record, and taking
    // the lock *first* meant that on a read-only or full env directory,
    // exactly when a record gets wedged, `share stop --force` failed too, and
    // there was no way left to revoke somebody's access short of Ctrl-C in the
    // sharer's own terminal. An unconditional delete of one file needs nothing
    // serialised anyway.
    let _lock = Lock::acquire(env_dir);
    let p = session_path(env_dir);
    match std::fs::remove_file(&p) {
        // Looked at again on the way out. Not a lock and not a guarantee, the
        // record could reappear the instant after this, but it catches the
        // case that actually happens, which is a live serving process that
        // rewrites its table on its next poll.
        Ok(()) if p.exists() => Ok(Forgotten::Reappeared),
        Ok(()) => Ok(Forgotten::Deleted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Forgotten::NothingThere),
        Err(e) => Err(H5iError::with_path(e, &p)),
    }
}

/// End a share, under a single lock hold.
///
/// Returns whether anything was actually serving. One lock for the whole
/// decision on purpose: revoking under the lock, releasing it, and *then*
/// removing the file leaves a window in which a concurrently starting share
/// legitimately takes the stale record over, and the removal then deletes the
/// new share's grant table, after its ticket has been printed to somebody.
pub fn stop(env_dir: &Path) -> Result<bool, H5iError> {
    let _lock = Lock::acquire(env_dir)?;
    let Some(mut s) = read(env_dir) else {
        return Err(H5iError::Metadata(
            "this box is not being shared — there is nothing to stop".into(),
        ));
    };
    if !is_live(&s) {
        let p = session_path(env_dir);
        std::fs::remove_file(&p).map_err(|e| H5iError::with_path(e, &p))?;
        return Ok(false);
    }
    for g in &mut s.grants {
        g.revoked = true;
    }
    // In the same locked step as the revoke, not left to the serving process to
    // set when it notices. It notices by polling, so between `stop` returning
    // and that poll there was a window where `share grant` still saw a live
    // share, and a grant landing in that window did not merely mint a doomed
    // ticket, it added a live grant, which is exactly the condition the serving
    // process polls for. A share could be resurrected by the race with the
    // command that was stopping it.
    s.winding_up = true;
    write(env_dir, &s)?;
    Ok(true)
}

/// Read, change, write, under the lock. The only way grants should be edited.
pub fn update<T>(
    env_dir: &Path,
    f: impl FnOnce(&mut ShareSession) -> Result<T, H5iError>,
) -> Result<T, H5iError> {
    let _lock = Lock::acquire(env_dir)?;
    let mut s = read(env_dir).ok_or_else(|| {
        H5iError::Metadata("this box is not being shared — run `h5i box share <name>` first".into())
    })?;
    let out = f(&mut s)?;
    write(env_dir, &s)?;
    Ok(out)
}

/// Forget the session.
fn record_is_ours(env_dir: &Path, started_at: &str) -> bool {
    read(env_dir)
        .map(|s| s.pid == std::process::id() && s.started_at == started_at)
        .unwrap_or(false)
}

pub fn clear(env_dir: &Path, started_at: &str) {
    // `let _ = …` would drop the guard at the end of *this statement*, which is
    // to say before the removal it is guarding. It was written that way once,
    // under a comment explaining the race it was closing.
    //
    // Acquisition failing is not a reason to skip the removal: the alternative
    // is a record for a process that is exiting anyway, which is the state
    // every "GONE share nobody can clear" report starts from.
    let _lock = Lock::acquire(env_dir);
    if record_is_ours(env_dir, started_at) {
        let _ = std::fs::remove_file(session_path(env_dir));
    }
}

/// Remove the record without waiting for the lock.
///
/// Only for the second Ctrl-C, where the operator has said "stop now" and
/// [`Lock::acquire`] would spend up to five seconds retrying before giving up
/// and doing this anyway. Most likely against a lock held by *this* process's
/// own orderly shutdown, which is the thing being abandoned. There is nothing to
/// serialise against: this is an unconditional delete of our own share's record
/// by the process that wrote it, not a read-modify-write of the grant table.
pub fn clear_now(env_dir: &Path, started_at: &str) {
    if record_is_ours(env_dir, started_at) {
        let _ = std::fs::remove_file(session_path(env_dir));
    }
}

/// Is the process that wrote this session still alive?
///
/// A share file outliving its process is the ordinary result of a crash or a
/// `kill -9`, and the honest answer to "is this box shared" is no.
/// A pid out of `pid_t`'s range is not a live process. See [`as_pid`] for what
/// went wrong when it reached `kill` unchecked.
pub fn is_live(s: &ShareSession) -> bool {
    pid_alive(s.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hard_exit_clears_the_record_without_waiting_for_the_lock() {
        // A second Ctrl-C means "stop now". `clear` waits up to five seconds
        // for a lock that, on this path, is most likely held by the orderly
        // shutdown being abandoned, so the exit that is supposed to be instant
        // was not.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = session_with(vec![]);
        write(dir.path(), &s).expect("write");
        let held = Lock::acquire(dir.path()).expect("hold the lock");

        let started = std::time::Instant::now();
        clear_now(dir.path(), &s.started_at);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "clearing waited for a lock it should not have: {:?}",
            started.elapsed()
        );
        assert!(read(dir.path()).is_none(), "the record survived the exit");
        drop(held);
    }

    #[test]
    fn what_this_crate_writes_is_what_h5i_core_reads() {
        // The loop closed. `h5i-core` cannot call this crate, it sits below
        // it, so `box rm`, `export` and the console each grew their own
        // hand-rolled probe of `share.json`, and by the time anyone counted
        // there were four definitions of "a live share" that did not agree.
        // There is one reader down there now, and this is the test that stops
        // the two drifting: a record written here, read there, field for
        // field.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = session_with(vec![]);
        s.port = 4321;
        s.transport = Transport::Tunnel;
        let (live, _) = mint_grant(Some("alex".into()), 4_000_000_000).expect("mint");
        let (dead, _) = mint_grant(None, 1).expect("mint");
        let (mut revoked, _) = mint_grant(None, 4_000_000_000).expect("mint");
        revoked.revoked = true;
        s.grants = vec![live, dead, revoked];
        write(dir.path(), &s).expect("write");

        let seen = h5i_core::share_record::read_live(dir.path())
            .expect("h5i-core could not read a record this crate just wrote");
        assert_eq!(seen.pid, std::process::id());
        assert_eq!(seen.port, 4321);
        assert_eq!(seen.transport, "tunnel");
        assert!(!seen.winding_up);
        assert_eq!(
            seen.live_grants, 1,
            "expired and revoked are not live grants"
        );
        assert!(seen.is_admitting());

        // And the state the console used to call "somebody can reach this box
        // right now" while the share was refusing to admit anybody.
        s.winding_up = true;
        write(dir.path(), &s).expect("write");
        let seen = h5i_core::share_record::read_live(dir.path()).expect("still a live process");
        assert!(seen.winding_up);
        assert!(!seen.is_admitting());
    }

    #[test]
    fn forget_says_so_when_the_record_comes_straight_back() {
        use std::os::unix::fs::PermissionsExt;
        // `forget` takes the lock if it can and proceeds without it, which is
        // deliberate: a wedged record on a read-only directory is exactly when
        // `--force` has to work. The cost is that a serving process (which
        // rewrites its table on every grant, revoke and expiry sweep) can put
        // the record back immediately, and the operator was told "deleted the
        // share record", one line under advice that reads as "access is now
        // cut off". It is not, and this is the one case that reliably happens.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        write(dir.path(), &s).expect("write");

        // Stand in for the serving process's next poll: the record is on disk
        // again by the time `forget` looks.
        let p = session_path(dir.path());
        let body = std::fs::read(&p).expect("read");
        assert_eq!(forget(dir.path()).expect("forget"), Forgotten::Deleted);
        std::fs::write(&p, &body).expect("rewrite");
        // Nothing rewrote it this time, so this is the honest answer.
        assert_eq!(forget(dir.path()).expect("forget"), Forgotten::Deleted);

        // And now the real shape: a hook that recreates the file inside the
        // removal window is not something a test can arrange portably, so the
        // check itself is what is pinned. The file existing after a
        // successful delete is reported as `Reappeared` and not as success.
        std::fs::write(&p, &body).expect("rewrite");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
            .expect("read-only dir");
        let refused = forget(dir.path());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restore");
        // Root ignores the mode, so the assertion below would be a statement
        // about the CI user rather than about `forget`.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        // A directory nothing can unlink from is an error, not a false
        // "deleted". The commit that added the lock-optional path claimed
        // `forget` works on a read-only directory, and unlinking needs write
        // permission on the directory holding the file, so it never did.
        assert!(refused.is_err(), "{refused:?}");
    }

    #[test]
    fn a_failed_rename_leaves_no_temp_file_behind() {
        // The cleanup was written for the write and not for the rename, which
        // is backwards: a read-only directory or a full disk fails at the
        // rename, and nothing in the tree (no `gc`, no `doctor`, no docs)
        // knows a `share.json.tmp.NNN` can exist or would ever remove one.
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory where the record goes makes the rename fail and nothing
        // else, so this tests the rename arm rather than the write arm.
        std::fs::create_dir_all(session_path(dir.path())).expect("blocker");

        let s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        let err = write(dir.path(), &s).expect_err("renaming onto a directory");
        assert!(format!("{err}").contains("share.json"), "{err}");

        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    #[test]
    fn two_writers_do_not_share_one_temp_file() {
        // A fixed `share.json.tmp` meant two processes that both held the lock
        // wrote into the same file and one renamed the other's partial bytes
        // into place, and a truncated `share.json` reads as *absent*, so
        // every verb then said the box was not being shared.
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &session_with(vec![])).expect("write");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a temp file survived the write: {leftovers:?}"
        );
        assert!(
            crate::session::read(dir.path()).is_some(),
            "the record did not land"
        );
    }

    #[test]
    fn a_share_record_can_always_be_forgotten() {
        // The dead end this exits: `is_live` is `kill(pid, 0)` and nothing
        // else, so a record whose pid has been reused by an unrelated process
        // reads as serving forever. `stop` revoked grants nobody was reading
        // and left the file; `share` refused because the box was already
        // shared; `status` said the pid was live. No verb got out of it.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            forget(dir.path()).expect("no record is not an error"),
            Forgotten::NothingThere
        );

        let s = session_with(vec![]);
        write(dir.path(), &s).expect("write");
        assert_eq!(forget(dir.path()).expect("forget"), Forgotten::Deleted);
        assert!(read(dir.path()).is_none());
    }

    #[test]
    fn status_says_so_when_this_shell_and_the_share_disagree_about_now() {
        // `share status` reads a file with whatever clock the shell it runs in
        // has. The serving process floors its expiry against elapsed time and
        // this command cannot, so after a backward clock step the door closes
        // earlier than this countdown says, and the only symptom otherwise is
        // a ticket refused while `status` still shows time left.
        let started = chrono::Utc::now();
        let mut s = ShareSession::new("env/a/demo", 3000, Transport::P2p, "abc", started);
        let (g, _secret) = mint_grant(None, started.timestamp() + 600).expect("mint");
        s.grants.push(g);

        // A clock that agrees says nothing.
        let quiet = crate::bridge::render_status(&s, started.timestamp() + 1);
        assert!(!quiet.contains("NOTE:"), "{quiet}");

        // A shell an hour behind the share sees the record start in its
        // future.
        let behind = crate::bridge::render_status(&s, started.timestamp() - 3600);
        assert!(behind.contains("1h0m in the future"), "{behind}");
        assert!(behind.contains("close sooner"), "{behind}");
    }

    #[test]
    fn every_definition_of_still_valid_flips_in_the_same_second() {
        // Six places in three crates decide whether a grant is still good, and
        // only one of them, `Ticket::remaining`, had a test that touched the
        // boundary. Flipping `>` to `>=` in any of the other five passed the
        // entire suite. This file's own module docs record that there were once
        // four definitions of "a live share" in the codebase; this is the guard
        // that keeps the surviving ones from drifting a second apart.
        const T: i64 = 1_800_000_000;
        let (g, secret) = mint_grant(None, T).expect("mint");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        s.grants.push(g);

        // One second before: alive, everywhere.
        assert!(s.authorize(&secret, T - 1).is_ok());
        assert_eq!(s.live_grants(T - 1), 1);
        assert!(!s.is_spent(T - 1));

        // At `expires_at` itself: expired, everywhere. This is the second the
        // holder was promised up to, not through.
        assert_eq!(s.authorize(&secret, T).unwrap_err(), Denied::Expired);
        assert_eq!(s.live_grants(T), 0);
        assert!(s.is_spent(T));
        assert_eq!(s.authorize(&secret, T + 1).unwrap_err(), Denied::Expired);

        // And what the sharer is shown agrees with what the door does.
        let live = crate::bridge::render_status(&s, T - 1);
        assert!(live.contains("1s left"), "{live}");
        assert!(!live.contains("expired"), "{live}");
        let dead = crate::bridge::render_status(&s, T);
        assert!(dead.contains("expired"), "{dead}");
    }

    #[test]
    fn how_long_is_left_never_reads_as_zero_while_it_is_not() {
        // Integer minutes made every share in existence say "0m left" for its
        // final minute, in the same column that says "expired", at the exact
        // moment somebody runs `status` to decide whether to re-mint.
        assert_eq!(humanise(45), "45s");
        assert_eq!(humanise(59), "59s");
        assert_eq!(humanise(60), "1m");
        assert_eq!(humanise(3600), "1h0m");
        assert_eq!(humanise(-5), "0s");
    }

    #[test]
    fn winding_up_survives_a_file_written_before_the_field_existed() {
        // `#[serde(default)]`, checked rather than assumed: a share.json from
        // an older h5i must still read, and must read as *not* winding up.
        let old = r#"{"v":1,"box_id":"env/a/demo","port":3000,"transport":"tunnel",
            "endpoint":"https://x","started_at":"2026-01-01T00:00:00Z","pid":1,"grants":[]}"#;
        let s: ShareSession = serde_json::from_str(old).expect("an older record still reads");
        assert!(!s.winding_up);
    }

    /// A table this h5i cannot read is not a table it may replace.
    ///
    /// `read` maps both `Gone` and `Unreadable` to `None`, and `claim` read
    /// `None` as permission to write a fresh record. So a share that was still
    /// serving, whose table had become malformed, was overwritten while its
    /// endpoint stayed alive, and since every bridge rereads that table on
    /// every connection, the old bridge would then authorize the *new* share's
    /// tickets while still dialling the port the old share pinned.
    #[test]
    fn an_unreadable_table_is_not_claimed_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        let junk = b"{ not a record";
        std::fs::write(session_path(dir.path()), junk).expect("write junk");

        let fresh = session_with(vec![]);
        let err = claim(dir.path(), &fresh, "demo").expect_err("claimed over an unreadable table");
        assert!(format!("{err}").contains("cannot read"), "{err}");
        assert!(format!("{err}").contains("--force"), "{err}");
        assert_eq!(
            std::fs::read(session_path(dir.path())).expect("read"),
            junk,
            "the unreadable record was replaced anyway"
        );

        // And with it out of the way, claiming works.
        std::fs::remove_file(session_path(dir.path())).expect("remove");
        claim(dir.path(), &fresh, "demo").expect("a clear path claims");
    }

    /// Two grants in one table never share a handle.
    ///
    /// Eight hex characters is thirty-two bits and the id was chosen without
    /// looking at the table. `revoke` finds the first matching row, so on a
    /// collision the second grant can never be revoked individually, and a
    /// connection admitted by the first survives revoking it, because
    /// `grant_is_live` still finds the colliding row alive.
    #[test]
    fn a_minted_grant_never_reuses_a_handle_the_table_has() {
        let (taken, _) = mint_grant(None, 4_000_000_000).expect("mint");
        let existing = vec![taken.clone()];
        for _ in 0..64 {
            let (g, _) = mint_grant_unlike(&existing, None, 4_000_000_000).expect("mint");
            assert_ne!(
                g.id, taken.id,
                "a grant reused a handle already in the table"
            );
        }

        // And an id that is free is used as it is.
        let (g, _) = mint_grant_unlike(&[], None, 4_000_000_000).expect("mint");
        assert_eq!(g.id.len(), 8);
    }

    #[test]
    fn a_pid_kill_would_treat_as_a_wildcard_is_not_a_live_process() {
        // `pid_t` is signed. `4294967295` fits the `u32` this record stores and
        // arrives at `kill` as `-1`, every process this user may signal,
        // which succeeds, so a corrupt or crafted record read as live forever:
        // `share` refused to start, cleanup refused to clear it, and grant
        // operations trusted a process that never existed. `h5i-core`'s reader
        // had this bound; these two did not.
        let mut s = session_with(vec![]);
        s.pid = u32::MAX;
        assert!(!is_live(&s), "kill(-1, 0) was read as a live share");
        s.pid = 0;
        assert!(!is_live(&s), "pid 0 is this process group, not a process");
        s.pid = std::process::id();
        assert!(is_live(&s), "a real pid is still live");
    }

    #[test]
    fn a_record_from_a_version_this_h5i_does_not_know_is_unreadable() {
        // Management verbs run in their own processes, so an upgrade or a
        // rollback puts an older binary in front of a newer live table. `v` was
        // written and never read, so that binary decoded a v2 record as v1,
        // applied v1 grant semantics, and rewrote it. `Unreadable` is the state
        // whose own doc comment already promised to cover this.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = session_with(vec![]);
        s.pid = std::process::id();
        write(dir.path(), &s).expect("write");
        assert!(matches!(read_state(dir.path()), ReadState::Present(_)));

        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(session_path(dir.path())).expect("read"))
                .expect("json");
        v["v"] = serde_json::json!(2);
        std::fs::write(session_path(dir.path()), v.to_string()).expect("write v2");
        assert!(
            matches!(read_state(dir.path()), ReadState::Unreadable),
            "a v2 record was decoded as a v1 one"
        );
        assert!(read(dir.path()).is_none());
    }

    fn session_with(grants: Vec<Grant>) -> ShareSession {
        let mut s = ShareSession::new(
            "env/agent/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        s.grants = grants;
        s
    }

    #[test]
    fn a_grant_admits_its_own_secret_and_nothing_else() {
        let (g, secret) = mint_grant(None, 4_000_000_000).expect("mint");
        let (other, other_secret) = mint_grant(None, 4_000_000_000).expect("mint");
        let s = session_with(vec![g.clone(), other]);
        assert_eq!(s.authorize(&secret, 0).expect("admitted").id, g.id);
        assert_ne!(s.authorize(&other_secret, 0).expect("admitted").id, g.id);
        assert_eq!(s.authorize("not a secret", 0).unwrap_err(), Denied::Unknown);
    }

    #[test]
    fn a_secret_matches_only_itself() {
        assert!(secret_matches("abc123", "abc123"));
        assert!(!secret_matches("abc123", "abc124"));
        assert!(!secret_matches("", "abc123"));
        assert!(!secret_matches("abc123", ""));
    }

    #[test]
    fn the_secret_itself_is_never_written_down() {
        // The property the whole file rests on: someone who reads share.json
        // learns that a grant exists, not how to use it.
        let (g, secret) = mint_grant(Some("alex".into()), 4_000_000_000).expect("mint");
        let s = session_with(vec![g]);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(
            !json.contains(&secret),
            "share.json must not carry the secret"
        );
        assert!(json.contains(&hash_secret(&secret)));
    }

    #[test]
    fn expiry_and_revocation_are_told_apart() {
        let (mut expired, expired_secret) = mint_grant(None, 1_000).expect("mint");
        expired.id = "expired0".into();
        let (mut revoked, revoked_secret) = mint_grant(None, 4_000_000_000).expect("mint");
        revoked.id = "revoked0".into();
        revoked.revoked = true;
        let s = session_with(vec![expired, revoked]);
        assert_eq!(
            s.authorize(&expired_secret, 2_000).unwrap_err(),
            Denied::Expired
        );
        assert_eq!(
            s.authorize(&revoked_secret, 2_000).unwrap_err(),
            Denied::Revoked
        );
    }

    #[test]
    fn revocation_beats_a_still_valid_expiry() {
        // The ordering that matters: a revoked grant whose clock has not run
        // out must not be admitted because the expiry check passed first.
        let (mut g, secret) = mint_grant(None, 4_000_000_000).expect("mint");
        g.revoked = true;
        let s = session_with(vec![g]);
        assert_eq!(s.authorize(&secret, 0).unwrap_err(), Denied::Revoked);
    }

    #[test]
    fn a_share_is_spent_when_no_grant_can_admit_anyone() {
        let (live, _) = mint_grant(None, 4_000_000_000).expect("mint");
        let (short, _) = mint_grant(None, 1_000).expect("mint");
        assert!(!session_with(vec![live.clone()]).is_spent(0));
        assert!(session_with(vec![short.clone()]).is_spent(2_000));
        // One live grant among spent ones keeps the share alive.
        assert!(!session_with(vec![short, live]).is_spent(2_000));
        assert!(session_with(vec![]).is_spent(0));
    }

    #[test]
    fn the_table_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (g, secret) = mint_grant(None, 4_000_000_000).expect("mint");
        let s = session_with(vec![g]);
        write(dir.path(), &s).expect("write");
        assert_eq!(read(dir.path()).expect("read"), s);
        assert!(read(dir.path()).unwrap().authorize(&secret, 0).is_ok());
        clear(dir.path(), &s.started_at);
        assert!(read(dir.path()).is_none());
    }

    #[test]
    fn a_revoke_written_by_another_process_is_what_the_next_read_sees() {
        // The reason revocation lives on disk: `share revoke` is not the
        // process serving the share.
        let dir = tempfile::tempdir().expect("tempdir");
        let (g, secret) = mint_grant(None, 4_000_000_000).expect("mint");
        let id = g.id.clone();
        write(dir.path(), &session_with(vec![g])).expect("write");
        update(dir.path(), |s| {
            s.grants
                .iter_mut()
                .filter(|g| g.id == id)
                .for_each(|g| g.revoked = true);
            Ok(())
        })
        .expect("update");
        assert_eq!(
            read(dir.path()).unwrap().authorize(&secret, 0).unwrap_err(),
            Denied::Revoked
        );
    }

    #[test]
    fn the_session_file_is_not_somewhere_the_box_can_write() {
        // `<env>/spool` and `<env>/tmp` are the box's write window. A share
        // table inside either would be a grant table the agent could edit.
        let p = session_path(Path::new("/envs/demo"));
        assert_eq!(p, Path::new("/envs/demo/share.json"));
        assert!(!p.starts_with("/envs/demo/spool"));
        assert!(!p.starts_with("/envs/demo/tmp"));
    }

    #[test]
    fn a_lock_is_only_removed_by_the_process_that_holds_it() {
        // Two processes breaking the same stale lock both create one and only
        // one wins. Without an owner check the loser removed the winner's lock
        // on its way out, and two processes then edited the grant table at
        // once.
        let dir = tempfile::tempdir().expect("tempdir");
        let held = Lock::acquire(dir.path()).expect("acquire");
        // Somebody else's lock, in the same place.
        std::fs::write(dir.path().join(LOCK_FILE), "999999").expect("overwrite");
        drop(held);
        assert!(
            dir.path().join(LOCK_FILE).exists(),
            "a lock that was no longer ours was removed anyway"
        );
        std::fs::remove_file(dir.path().join(LOCK_FILE)).expect("tidy");
    }

    /// Dropping releases the lock, and deliberately leaves the file.
    ///
    /// The file used to be unlinked on the way out, and under `flock` that
    /// would restore the whole defect this lock was rewritten for: a waiter
    /// holds a descriptor to an inode with no name left, a newcomer creates a
    /// fresh file at the path and locks *that*, and there are two holders
    /// again. A zero-byte `share.lock` in the box's directory is the correct
    /// end state: `share-gate.lock` beside it has always worked this way.
    #[test]
    fn the_lock_is_released_on_drop_and_its_file_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = Lock::acquire(dir.path()).expect("first");
        assert!(dir.path().join(LOCK_FILE).exists());
        drop(held);
        assert!(
            dir.path().join(LOCK_FILE).exists(),
            "the lock file was unlinked, which is how two processes come to hold it"
        );
        let _again = Lock::acquire(dir.path()).expect("second");
    }

    #[test]
    fn updating_a_box_that_is_not_shared_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = update(dir.path(), |_| Ok(())).expect_err("no session");
        assert!(format!("{err}").contains("not being shared"));
    }
}

/// Concurrency, tested with concurrency: real processes, racing on purpose.
#[cfg(all(test, unix))]
mod concurrency {
    use super::*;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// What a re-executed child should do. Its absence is what makes [`worker`]
    /// inert in an ordinary run.
    const JOB: &str = "H5I_SHARE_CONCURRENCY_JOB";
    const DIR: &str = "H5I_SHARE_CONCURRENCY_DIR";
    const START: &str = "H5I_SHARE_CONCURRENCY_START";

    /// Exit codes a child uses to report what happened, since a child cannot
    /// assert. Anything else (101 above all, which is a panic) is a failure
    /// the parent prints the child's stderr for.
    const EXIT_OK: i32 = 0;
    const EXIT_FAILED: i32 = 2;
    /// The claim was refused *because somebody else holds the box*, which is the
    /// right answer for every loser of that race. Distinct from `EXIT_FAILED` so
    /// a refusal for some other reason cannot be counted as the expected one.
    const EXIT_ALREADY_SHARED: i32 = 3;
    /// Two holders were inside the lock at once.
    const EXIT_OVERLAPPED: i32 = 4;

    fn workers() -> usize {
        std::env::var("H5I_SHARE_CONCURRENCY_WORKERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)
    }

    fn rounds_each() -> usize {
        std::env::var("H5I_SHARE_CONCURRENCY_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(12)
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_millis()
    }

    /// Spin until the instant the parent chose.
    ///
    /// A spin rather than a sleep, and only for the last few hundred
    /// milliseconds: `sleep` returns when the scheduler feels like it, and the
    /// window these tests are trying to hit is microseconds wide.
    fn wait_for_start() {
        let Some(at) = std::env::var(START)
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
        else {
            return;
        };
        while now_ms() < at {
            std::hint::spin_loop();
        }
    }

    /// The child half. Inert unless [`JOB`] is set, so a normal `cargo test`
    /// runs it, does nothing, and passes.
    #[test]
    fn worker() {
        let Ok(job) = std::env::var(JOB) else {
            return;
        };
        let dir = PathBuf::from(std::env::var(DIR).expect("a directory to work in"));
        wait_for_start();
        let code = match job.as_str() {
            "append" => append(&dir),
            "hold" => hold(&dir),
            "claim" => claim_one(&dir),
            "grab" => grab(&dir),
            other => panic!("no such job: {other}"),
        };
        // Before libtest gets a chance to report success on its own terms: the
        // parent reads the exit code, and it has to be ours.
        std::process::exit(code);
    }

    /// Append one grant per round through the real read-modify-write.
    fn append(dir: &Path) -> i32 {
        for i in 0..rounds_each() {
            let label = format!("{}-{i}", std::process::id());
            let r = update(dir, |s| {
                let (g, _secret) =
                    mint_grant_unlike(&s.grants, Some(label.clone()), 4_000_000_000)?;
                s.grants.push(g);
                Ok(())
            });
            if let Err(e) = r {
                eprintln!("append {i} failed: {e}");
                return EXIT_FAILED;
            }
        }
        EXIT_OK
    }

    /// Take the lock, prove nobody else is inside it, let it go. Repeat.
    ///
    /// The proof is a file created with `create_new` while the lock is held and
    /// removed before it is released, so a second holder's create fails, and
    /// that failure is the violation. It cannot report a false one: the sentinel
    /// is always removed before the guard drops, so a process that acquires
    /// after us finds nothing there.
    fn hold(dir: &Path) -> i32 {
        let sentinel = dir.join("held-by");
        let mut overlaps = 0usize;
        for _ in 0..rounds_each() {
            let Ok(_lock) = Lock::acquire(dir) else {
                eprintln!("could not acquire the lock at all");
                return EXIT_FAILED;
            };
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&sentinel)
            {
                Ok(mut f) => {
                    use std::io::Write as _;
                    // Written, not synced. Another process reading this file
                    // sees it through the page cache, so `sync_all` bought no
                    // visibility here and cost a real fsync per acquisition.
                    // Milliseconds on APFS, which at high worker counts was the
                    // whole reason a run ran out of deadline. The harness was
                    // measuring its own durability call.
                    let _ = write!(f, "{}", std::process::id());
                    // Wide enough that an overlapping holder has time to see
                    // it. Without this the two could pass through in sequence
                    // and the harness would report nothing.
                    std::thread::sleep(Duration::from_micros(300));
                    let _ = std::fs::remove_file(&sentinel);
                }
                // Somebody else is inside the lock. Deliberately *not* removed:
                // it is theirs, and taking it would hide the next overlap.
                //
                // Whose it is decides whether this is a real overlap or a
                // harness artifact: a *live sibling* means two holders, and
                // anything else means a sentinel somebody failed to clean up.
                Err(_) => {
                    let held_by = std::fs::read_to_string(&sentinel).unwrap_or_default();
                    let other: i32 = held_by.trim().parse().unwrap_or(-1);
                    let alive = other > 0 && unsafe { libc::kill(other, 0) } == 0;
                    eprintln!(
                        "overlap: pid {} is inside the lock and pid {} claims it too \
                         (that one is {})",
                        std::process::id(),
                        other,
                        if alive { "alive" } else { "gone" }
                    );
                    overlaps += 1;
                }
            }
        }
        if overlaps > 0 {
            eprintln!("{overlaps} overlapping holders");
            return EXIT_OVERLAPPED;
        }
        EXIT_OK
    }

    /// Take the lock, say so, and never let go. To be killed.
    fn grab(dir: &Path) -> i32 {
        let Ok(lock) = Lock::acquire(dir) else {
            eprintln!("could not take the lock");
            return EXIT_FAILED;
        };
        if std::fs::write(dir.join("grabbed"), "held").is_err() {
            return EXIT_FAILED;
        }
        std::thread::sleep(Duration::from_secs(60));
        drop(lock);
        EXIT_OK
    }

    /// Claim the box, then stay alive.
    ///
    /// Staying alive is load bearing: `claim` takes over a record whose process
    /// is gone, and rightly. That is a crash. A claimer that exited the moment
    /// it won would be taken over by the next one, and the race would report
    /// every process as a winner.
    fn claim_one(dir: &Path) -> i32 {
        let s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "endpoint",
            chrono::Utc::now(),
        );
        let outcome = claim(dir, &s, "demo");
        std::thread::sleep(Duration::from_millis(900));
        match outcome {
            Ok(_) => EXIT_OK,
            Err(e) if format!("{e}").contains("already being shared") => EXIT_ALREADY_SHARED,
            Err(e) => {
                eprintln!("refused for the wrong reason: {e}");
                EXIT_FAILED
            }
        }
    }

    // ─── the parent half ────────────────────────────────────────────────────

    fn spawn(job: &str, dir: &Path, start_ms: u128) -> Child {
        let exe = std::env::current_exe().expect("the test binary's own path");
        Command::new(exe)
            // libtest takes the filter positionally; `--exact` stops it from
            // matching every test whose name contains this one.
            .arg("session::concurrency::worker")
            .arg("--exact")
            .arg("--test-threads=1")
            // Without this a worker's `eprintln!` goes into libtest's capture
            // buffer, and the worker calls `exit` before libtest ever prints
            // it, so every complaint a child made was discarded, and the
            // parent's panic said only that something had gone wrong.
            .arg("--nocapture")
            .env(JOB, job)
            .env(DIR, dir)
            .env(START, start_ms.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("re-execute the test binary as a worker")
    }

    /// Wait for every child and return its exit code, printing the stderr of
    /// any that ended unexpectedly. A child cannot assert, so this is the only
    /// place its complaint can be read.
    fn collect(kids: Vec<Child>, expected: &[i32]) -> Vec<(i32, String)> {
        let mut codes = Vec::new();
        for kid in kids {
            let out = kid.wait_with_output().expect("wait for a worker");
            let code = out.status.code().unwrap_or(-1);
            // A worker's own account of what it saw, kept for every exit and
            // not only the unexpected ones: the interesting failures here are
            // the *expected* codes, and the reason is on stderr.
            let said = String::from_utf8_lossy(&out.stderr)
                .lines()
                .filter(|l| !l.starts_with("running ") && !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !expected.contains(&code) {
                panic!("a worker exited {code}, which is not one of {expected:?}:\n{said}");
            }
            codes.push((code, said));
        }
        codes
    }

    /// Far enough out that every child has been forked, executed and reached
    /// its spin before the first one starts.
    fn in_a_moment() -> u128 {
        now_ms() + 400
    }

    /// Every append lands, and nothing reading alongside them sees a table that is neither the
    /// old one nor the new one.
    #[test]
    fn concurrent_grants_all_land_and_a_reader_never_sees_a_half_written_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seed = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "endpoint",
            chrono::Utc::now(),
        );
        write(dir.path(), &seed).expect("seed the table");

        let (n, k) = (workers(), rounds_each());
        let start = in_a_moment();
        let mut kids: Vec<Child> = (0..n).map(|_| spawn("append", dir.path(), start)).collect();

        // Read the whole time they are writing. This is a different process
        // from all of them, which is the only way the question is the real one.
        let mut unreadable = 0usize;
        let mut gone = 0usize;
        let mut reads = 0usize;
        loop {
            match read_state(dir.path()) {
                ReadState::Present(_) => {}
                ReadState::Unreadable => unreadable += 1,
                ReadState::Gone => gone += 1,
            }
            reads += 1;
            if kids.iter_mut().all(|c| matches!(c.try_wait(), Ok(Some(_)))) {
                break;
            }
        }

        let codes = collect(kids, &[EXIT_OK]);
        assert_eq!(codes.len(), n);
        assert!(reads > 0);
        assert_eq!(
            unreadable, 0,
            "a reader saw a table that was neither the old one nor the new one, \
             {unreadable} times in {reads} reads"
        );
        assert_eq!(
            gone, 0,
            "a reader saw the record vanish while it was only being rewritten, \
             {gone} times in {reads} reads"
        );

        let table = read(dir.path()).expect("the table survived");
        assert_eq!(
            table.grants.len(),
            n * k,
            "{} of {} appends were lost to a race",
            n * k - table.grants.len(),
            n * k
        );
        // And every one is its own grant: `mint_grant_unlike` only sees the
        // table it was handed, so a collision here is two writers having been
        // handed the same one.
        let mut ids: Vec<&str> = table.grants.iter().map(|g| g.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two grants were minted with one id");
    }

    /// A lock left behind by a dead holder, and every process piling onto it in the same
    /// instant, and still only one holder.
    #[test]
    fn a_stampede_onto_a_dead_holders_lock_leaves_a_single_holder() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A lock file with no live holder, which is what a `kill -9` leaves.
        // Its *contents* are now beside the point, that they used to decide
        // who could break it is what this test exists about, so it is written
        // with the old pid stamp precisely to show it decides nothing.
        std::fs::write(dir.path().join(LOCK_FILE), "4194301").expect("plant a dead holder's lock");

        let start = in_a_moment();
        let kids: Vec<Child> = (0..workers())
            .map(|_| spawn("hold", dir.path(), start))
            .collect();
        let codes = collect(kids, &[EXIT_OK, EXIT_OVERLAPPED, EXIT_FAILED]);
        // The two failures are different facts and must never be reported as
        // each other. An overlap is the lock being wrong; a worker that could
        // not acquire within the deadline is the lock being *right* and this
        // machine being busier than the deadline allows for, and an assertion
        // that blamed the first for the second sent the last reader of this
        // file looking for a race that was not there.
        let said = |want: i32| -> Vec<&str> {
            codes
                .iter()
                .filter(|(c, _)| *c == want)
                .map(|(_, said)| said.as_str())
                .collect()
        };
        let overlaps = said(EXIT_OVERLAPPED);
        assert!(
            overlaps.is_empty(),
            "two processes were inside the lock at once:\n{}",
            overlaps.join("\n")
        );
        let timed_out = said(EXIT_FAILED);
        assert!(
            timed_out.is_empty(),
            "a worker never got the lock inside its {}s deadline — no overlap, so this is \
             contention past what the deadline allows rather than a lock that is wrong:\n{}",
            LOCK_WAIT.as_secs(),
            timed_out.join("\n")
        );

        // The lock file stays, on purpose: unlinking it under `flock` is how a
        // waiter and a newcomer end up on two different inodes, both locked.
        assert!(
            dir.path().join(LOCK_FILE).exists(),
            "the lock file was unlinked, which is how two processes come to hold it"
        );
        // And nothing from the apparatus that used to break it.
        assert!(
            !dir.path().join("share.lock.stale").exists(),
            "a break left rubbish behind"
        );
        assert!(
            !dir.path().join("held-by").exists(),
            "a holder left its sentinel behind"
        );
    }

    /// A killed holder wedges nothing, and no heuristic is consulted to decide
    /// it.
    ///
    /// The property the pid stamp, the age fallback and the break all existed to
    /// provide: "a stale lock from a killed process is broken, because the
    /// alternative is a share nobody can revoke". None of them provided it
    /// safely, and the kernel provides it for nothing: an `flock` is released
    /// when the holder's last descriptor closes, and `SIGKILL` closes them all.
    #[test]
    fn a_killed_holder_leaves_a_lock_anyone_can_take() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut holder = spawn("grab", dir.path(), now_ms());
        let ready = dir.path().join("grabbed");
        for _ in 0..400 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "the holder never took the lock");

        // The bluntest way a holder can go: no destructor runs, nothing is
        // cleaned up, and the lock file is left exactly where it was.
        holder.kill().expect("kill the holder");
        holder.wait().expect("reap the holder");
        assert!(dir.path().join(LOCK_FILE).exists(), "nothing to inherit");

        let started = std::time::Instant::now();
        let taken = Lock::acquire(dir.path()).expect("a killed holder's lock is free");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "waited {:?} for a lock whose holder had been killed",
            started.elapsed()
        );
        drop(taken);
    }

    /// Many `h5i box share` starts, one box, one winner.
    ///
    /// The check and the write have to be one step, and this is the only test
    /// that can say so: done apart, two starts both pass the check and the
    /// second overwrites the first's grant table, which means the first
    /// share's ticket, already sent to somebody, stops working with no
    /// explanation. It exercises the share gate and the session lock together,
    /// in the order `claim` takes them.
    #[test]
    fn only_one_of_many_simultaneous_starts_claims_the_box() {
        let dir = tempfile::tempdir().expect("tempdir");
        let start = in_a_moment();
        let kids: Vec<Child> = (0..workers())
            .map(|_| spawn("claim", dir.path(), start))
            .collect();
        let codes = collect(kids, &[EXIT_OK, EXIT_ALREADY_SHARED, EXIT_FAILED]);

        let won = codes.iter().filter(|(c, _)| *c == EXIT_OK).count();
        let refused = codes
            .iter()
            .filter(|(c, _)| *c == EXIT_ALREADY_SHARED)
            .count();
        assert_eq!(
            won,
            1,
            "{won} of {} starts claimed the same box (refused {refused})",
            codes.len()
        );
        assert_eq!(
            won + refused,
            codes.len(),
            "a start was refused for a reason that is not 'already shared': {codes:?}"
        );

        // And what is on disk is a claim, not a collision.
        let table = read(dir.path()).expect("the winner's record");
        assert_eq!(table.port, 3000);
        assert!(!table.starting);
    }
}
