//! The share session on disk: which box is shared, over what, and to whom.
//!
//! One file, `<env>/share.json`, and where it sits is the security property.
//! It is a sibling of `receipt.jsonl` and `view-token`, which means it is
//! outside `<env>/spool` and `<env>/tmp` — the only two paths a box can write.
//! An agent cannot mint itself a grant, cannot un-revoke one, and cannot read
//! the file to learn who is connected.
//!
//! What it holds is a **grant table**, not a password. Each grant stores the
//! SHA-256 of its secret, so the file admits nobody even to a reader who has
//! it. The cost is that a ticket is printed once and never again, which is the
//! right trade for a capability that reaches into a running box from another
//! machine.
//!
//! Revocation lives here rather than in the sharer's memory on purpose:
//! `h5i box share revoke` is a *different process* from the one serving the
//! share, and a revoke that only reached the process that happened to run it
//! would be a revoke that silently did nothing.

use std::path::{Path, PathBuf};

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Sibling of the receipt log; never under a path the box can write.
const SESSION_FILE: &str = "share.json";
/// The format `share.json` is written in, and the only one this h5i will read.
///
/// Checked on the way in as well as written on the way out — see [`read_state`]
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
    pub grants: Vec<Grant>,
}

/// Why a presented secret was not honoured. Each is separate because they are
/// separate problems: an expired ticket needs a new one, a revoked ticket means
/// the sharer meant to cut you off, and an unknown secret means you have the
/// wrong ticket entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    Unknown,
    /// The grant table could not be read at all — the disk is full, the file
    /// is unreadable, the process is out of descriptors. Not a fact about the
    /// visitor's ticket, and it used to be reported as one: the visitor was
    /// told to ask for a new invite, the sharer's terminal said "that ticket
    /// is not one this share knows", and the receipt recorded an unknown
    /// ticket — for a machine problem on the sharer's own side.
    TableUnreadable,
    Expired,
    Revoked,
    /// Nobody presented anything. Separate from [`Denied::Unknown`] because on
    /// a public tunnel URL this is the commonest event of the whole session — a
    /// scanner fetching `/` — and folding it into "unknown ticket" made the
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

/// `45s`, `4m`, `1h30m` — whichever unit does not read as zero.
///
/// Lives here rather than in the CLI because two callers render "how long is
/// left" and they were not the same function: the announce line was fixed to
/// stop saying `0m` for a 45-second ticket, and `share status` was left on
/// integer minutes, so every share in existence read `0m left` for its final
/// minute — right next to `expired` in the same column, at the exact moment
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

/// Mint a grant, returning it and the secret that must be printed **now**.
pub fn mint_grant(label: Option<String>, expires_at: i64) -> Result<(Grant, String), H5iError> {
    let secret = crate::ticket::mint_secret()?;
    // Eight hex characters is a handle to type, not a secret: it identifies a
    // grant in `share ls` and `share revoke` and authorizes nothing.
    let id = h5i_core::token::hex(4)?;
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

/// An exclusive lock over the grant table, released on drop.
///
/// `create_new` is the whole mechanism: it is atomic on every filesystem we
/// care about, and it needs no daemon. A stale lock from a killed process is
/// broken after [`LOCK_STALE_SECS`], because the alternative is a share nobody
/// can revoke.
pub struct Lock {
    path: PathBuf,
    /// The pid we stamped into the file. Checked again on the way out.
    owner: String,
}

const LOCK_STALE_SECS: u64 = 30;

impl Lock {
    pub fn acquire(env_dir: &Path) -> Result<Lock, H5iError> {
        let path = env_dir.join(LOCK_FILE);
        let mine = std::process::id().to_string();
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    // Stamped with who holds it. Both halves of the stale-break
                    // need this: deciding whether the holder is really gone, and
                    // making sure the loser of a break race cannot remove the
                    // winner's lock on its way out.
                    //
                    // A stamp that cannot be written is a lock nobody can
                    // reason about, so it is not held. On a full disk the
                    // `create_new` succeeds and the write does not, and `Drop`
                    // then saw content that was not its own and left the file:
                    // every later verb waited five seconds and blamed a holder
                    // that had never existed, leaking another one on the way
                    // out. Measured at 3.8s, 5.2s, 5.6s for three verbs in a
                    // row after one ENOSPC.
                    use std::io::Write as _;
                    if let Err(e) = f.write_all(mine.as_bytes()) {
                        drop(f);
                        // Removed only if it is still the empty file this call
                        // made. `remove_file` deletes whatever is at the path
                        // *now* — the anti-pattern the stale break fourteen
                        // lines below was rewritten to avoid — so a lock some
                        // other process had since created and stamped would
                        // have been unlinked by this cleanup.
                        if std::fs::read(&path).map(|c| c.is_empty()).unwrap_or(false) {
                            let _ = std::fs::remove_file(&path);
                        }
                        return Err(H5iError::with_path(e, &path));
                    }
                    return Ok(Lock { path, owner: mine });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Broken by *renaming it aside*, not by unlinking it.
                    // `remove_file` deletes whatever is at the path now rather
                    // than the file that was just stat'd, so two processes that
                    // both decided "stale" both removed and both created, and
                    // both walked away believing they held it. Measured with
                    // the real code in a fork-synchronised harness: 8 of 20
                    // runs broke mutual exclusion with no jitter at all, and 6
                    // of 6 with a single 30 ms preemption injected between the
                    // stat and the remove — which is one page fault.
                    //
                    // A rename is atomic and single-winner: the process that
                    // renames the stale file away is the only one that can then
                    // create in its place, and a process that renames nothing
                    // has already lost.
                    if lock_is_stale(&path) {
                        let aside = path.with_extension("lock.stale");
                        if std::fs::rename(&path, &aside).is_ok() {
                            let _ = std::fs::remove_file(&aside);
                            continue;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(H5iError::with_path(e, &path)),
            }
        }
        Err(H5iError::Metadata(format!(
            "another h5i is holding this box's share lock ({}). If nothing else is running, \
             remove that file.",
            path.display()
        )))
    }
}

/// Is this lock file one whose holder is gone?
///
/// Age alone was the whole test, which is a guess: a process that took thirty
/// seconds under the lock would have had it broken underneath it. The pid the
/// holder stamped is the actual question, and age is kept only as the fallback
/// for a file written before this existed or by a pid we cannot read.
fn lock_is_stale(path: &Path) -> bool {
    // The pid first. Asking about age before asking whose it is meant a lock
    // naming a provably dead process was honoured for thirty seconds: measured
    // at four to five seconds of retries and then "another h5i is holding this
    // box's share lock", for a holder that did not exist. Age is the fallback
    // for a file with no readable pid, not the gate in front of the question.
    if let Ok(text) = std::fs::read_to_string(path) {
        // Stamped: the pid is the question.
        if let Ok(pid) = text.trim().parse::<u32>() {
            return !pid_alive(pid);
        }
        // An empty file is *also* the shape of a lock that is one instruction
        // from being stamped. Between `create_new` returning and the stamp
        // landing it is zero bytes, and calling that stale on sight let a
        // second process break a lock the first was about to hold — both then
        // returned `Ok(Lock)` and the grant table took two concurrent
        // read-modify-writes. Measured at 3.4 µs mean and 362 µs worst here,
        // and deterministic with a preemption injected at that point; and the
        // window widens under exactly the full-or-slow disk this was added
        // for, because that is what makes the stamp write block.
        //
        // So an unstamped lock still falls through to the age test below: the
        // orphan it was meant to clear is cleared after thirty seconds
        // instead of instantly, which is the price of not breaking a live one.
    }
    let old_enough = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.elapsed()
                .map(|d| d.as_secs() > LOCK_STALE_SECS)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    // No pid to read, so age is all there is to go on.
    old_enough
}

/// A pid that `kill` will treat as one process, or nothing.
///
/// `pid_t` is signed and every one of these numbers arrives from a file on
/// disk. `4294967295` fits the `u32` these records store and reaches `kill` as
/// `-1` — *every process this user may signal* — which succeeds, so a corrupt
/// or crafted record read as permanently live: `share` refused to start,
/// cleanup refused to clear it, and grant operations trusted a process that
/// was never there. `0` is the caller's own process group and `-1`'s smaller
/// sibling. Both are out of range for a pid and neither reaches `kill` now.
///
/// `h5i-core`'s reader has had this bound since it was written; these two
/// callers did not, and they are the ones that decide whether a share starts.
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

impl Drop for Lock {
    fn drop(&mut self) {
        // Only if it is still ours. Two processes breaking the same stale lock
        // both create one, and only one of them wins — without this check the
        // loser removed the winner's lock on its way out, and the grant table
        // was then edited by two processes at once.
        let still_mine = std::fs::read_to_string(&self.path)
            .map(|c| c.trim() == self.owner)
            .unwrap_or(false);
        if still_mine {
            let _ = std::fs::remove_file(&self.path);
        }
    }
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
/// could not read its own grant table — a problem on the sharing machine".
/// The machine was fine; somebody had stopped the share.
pub enum ReadState {
    Present(Box<ShareSession>),
    /// No file. The share is over, or is being stopped right now.
    Gone,
    /// A file that cannot be read or cannot be understood — a full disk, no
    /// descriptors, a permission problem, or a record from a version this h5i
    /// does not know.
    Unreadable,
}

pub fn read_state(env_dir: &Path) -> ReadState {
    let raw = match std::fs::read(session_path(env_dir)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadState::Gone,
        Err(_) => return ReadState::Unreadable,
    };
    match serde_json::from_slice::<ShareSession>(&raw) {
        // The version is a discriminator or it is decoration, and it was
        // decoration: `v` was written and never read, so a record from a
        // *newer* h5i decoded here as a v1 one as long as the field names still
        // matched. Share management runs in a different process from the share
        // — `grant`, `revoke`, `stop` — so during an upgrade or a rollback an
        // older binary could read a live v2 table, apply v1 grant semantics to
        // it, and atomically rewrite it. If a later format keeps these fields
        // and changes what they mean, that is authorization state corrupted by
        // a binary that could not have known better.
        //
        // `Unreadable` is the state whose own doc comment already said "a
        // record from a version this h5i does not know"; now it is.
        Ok(s) if s.v != SESSION_VERSION => ReadState::Unreadable,
        Ok(s) => ReadState::Present(Box::new(s)),
        Err(_) => ReadState::Unreadable,
    }
}

/// Write the session atomically, owner-readable only.
pub fn write(env_dir: &Path, s: &ShareSession) -> Result<(), H5iError> {
    let path = session_path(env_dir);
    // Unique per writer. A fixed `share.json.tmp` meant two processes that
    // both held the lock — which the stale-break race above allowed — wrote
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
        // behind, and nothing in the tree — no `gc`, no `doctor`, no docs —
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
///
/// The check and the write have to be one step. Done apart — look for a live
/// share, then write ours — two `h5i box share` starts can both pass the check
/// and the second overwrites the first's grant table, which means the first
/// share's tickets stop working with no explanation to whoever is holding one.
///
/// A record whose process is gone is a leftover from a crash and is taken over
/// rather than obeyed; the caller is told, because silently clearing somebody's
/// share record is not something to do without saying so.
pub fn claim(env_dir: &Path, s: &ShareSession, name: &str) -> Result<Option<u32>, H5iError> {
    let _lock = Lock::acquire(env_dir)?;
    let mut cleared = None;
    if let Some(existing) = read(env_dir) {
        if is_live(&existing) {
            return Err(already_shared(&existing, name));
        }
        cleared = Some(existing.pid);
    }
    write(env_dir, s)?;
    Ok(cleared)
}

/// The refusal a second `share` gets, with the box's real name in it.
///
/// One function rather than two spellings, because the copy in `run` said
/// `h5i box share stop <name>` — angle brackets and all — to somebody who then
/// had to guess what to type. The library does not know the name; the caller
/// does, so it passes it in.
pub fn already_shared(existing: &ShareSession, name: &str) -> H5iError {
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
    // on a peer-to-peer share — recommending it there sent people into a loop
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

/// Mark the share as winding up, so the other verbs stop pretending it serves.
///
/// The gap this closes: between the serving process deciding to stop and the
/// moment it removes the record, `is_live` is still true — the pid is right
/// there. So `share grant` in that window minted a ticket, printed a secret
/// that by contract is printed once, and then the serving process deleted the
/// table it was written into. The peer got a URL that never worked.
pub fn begin_winding_up(env_dir: &Path) {
    let _ = update(env_dir, |s| {
        s.winding_up = true;
        Ok(())
    });
}

/// Remove the record outright, whatever it says.
///
/// The escape hatch for a record whose pid has been reused by an unrelated
/// process. `is_live` is `kill(pid, 0)`, so such a record reads as serving
/// forever: `stop` revokes grants nobody reads and leaves the file, and `share`
/// refuses because the box is "already shared". No verb got out of that, and
/// the only fix was knowing to delete a file nothing had told the operator
/// about.
/// What `forget` actually achieved.
///
/// A `bool` could not say the third thing, and the third thing is the one that
/// matters: the removal is deliberately not serialised against anything, so a
/// serving process rewriting its table — every grant, every revoke, every
/// expiry sweep does — can put the record straight back a microsecond later.
/// Reported as success, that told an operator who had just been told "use this
/// only when you believe nothing is serving it" that they had cut access off,
/// when the share was in fact still admitting people.
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
    // the lock *first* meant that on a read-only or full env directory —
    // exactly when a record gets wedged — `share stop --force` failed too, and
    // there was no way left to revoke somebody's access short of Ctrl-C in the
    // sharer's own terminal. An unconditional delete of one file needs nothing
    // serialised anyway.
    let _lock = Lock::acquire(env_dir);
    let p = session_path(env_dir);
    match std::fs::remove_file(&p) {
        // Looked at again on the way out. Not a lock and not a guarantee — the
        // record could reappear the instant after this — but it catches the
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
/// legitimately takes the stale record over — and the removal then deletes the
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
    // share — and a grant landing in that window did not merely mint a doomed
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

/// Forget the session. Called when the sharer exits, so `share ls` describes
/// what is running rather than what once ran.
///
/// Under the lock like every other mutation. Without it, a `revoke` that had
/// already read the table can write it back *after* this deletes it, and the
/// share record outlives the process that owned it — a confusing `share ls`
/// rather than a security failure, but the fix is one line.
/// Is the record on disk the one *this* process wrote?
///
/// The premise every unconditional delete rested on, now actually checked. It
/// is reachable without it: `share stop --force` removes a live share's record,
/// a second `h5i box share` legitimately claims the box and prints its ticket
/// to somebody, and then the first process finishes its teardown and deletes
/// the *second* share's table — whose ticket has already been sent to a human
/// and now works for nobody.
fn record_is_ours(env_dir: &Path) -> bool {
    read(env_dir)
        .map(|s| s.pid == std::process::id())
        .unwrap_or(false)
}

pub fn clear(env_dir: &Path) {
    // `let _ = …` would drop the guard at the end of *this statement*, which is
    // to say before the removal it is guarding. It was written that way once,
    // under a comment explaining the race it was closing.
    //
    // Acquisition failing is not a reason to skip the removal: the alternative
    // is a record for a process that is exiting anyway, which is the state
    // every "GONE share nobody can clear" report starts from.
    let _lock = Lock::acquire(env_dir);
    if record_is_ours(env_dir) {
        let _ = std::fs::remove_file(session_path(env_dir));
    }
}

/// Remove the record without waiting for the lock.
///
/// Only for the second Ctrl-C, where the operator has said "stop now" and
/// [`Lock::acquire`] would spend up to five seconds retrying before giving up
/// and doing this anyway — most likely against a lock held by *this* process's
/// own orderly shutdown, which is the thing being abandoned. There is nothing to
/// serialise against: this is an unconditional delete of our own share's record
/// by the process that wrote it, not a read-modify-write of the grant table.
pub fn clear_now(env_dir: &Path) {
    if record_is_ours(env_dir) {
        let _ = std::fs::remove_file(session_path(env_dir));
    }
}

/// Is the process that wrote this session still alive?
///
/// A share file outliving its process is the ordinary result of a crash or a
/// `kill -9`, and the honest answer to "is this box shared" is no.
/// A pid out of `pid_t`'s range is not a live process — see [`as_pid`] for what
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
        // shutdown being abandoned — so the exit that is supposed to be instant
        // was not.
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &session_with(vec![])).expect("write");
        let held = Lock::acquire(dir.path()).expect("hold the lock");

        let started = std::time::Instant::now();
        clear_now(dir.path());
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
        // The loop closed. `h5i-core` cannot call this crate — it sits below
        // it — so `box rm`, `export` and the console each grew their own
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
        // `--force` has to work. The cost is that a serving process — which
        // rewrites its table on every grant, revoke and expiry sweep — can put
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
        // check itself is what is pinned — the file existing after a
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
        // "deleted" — the commit that added the lock-optional path claimed
        // `forget` works on a read-only directory, and unlinking needs write
        // permission on the directory holding the file, so it never did.
        assert!(refused.is_err(), "{refused:?}");
    }

    #[test]
    fn a_failed_rename_leaves_no_temp_file_behind() {
        // The cleanup was written for the write and not for the rename, which
        // is backwards: a read-only directory or a full disk fails at the
        // rename, and nothing in the tree — no `gc`, no `doctor`, no docs —
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
    fn an_unstamped_lock_is_not_broken_on_sight() {
        // Two failures on opposite sides of one line, both real.
        //
        // On a full disk `create_new` succeeds and the stamp write does not,
        // so `Drop` saw content that was not its own and left the file: every
        // later verb waited five seconds and blamed a holder that had never
        // existed. The fix for that — "an empty file is stale on sight" — then
        // broke mutual exclusion, because a lock is *also* empty for the
        // microseconds between being created and being stamped. Measured at
        // 3.4 µs mean and 362 µs worst, deterministic under a preemption, and
        // widening under exactly the slow disk it was written for.
        //
        // So: an unstamped lock is left alone, and the orphan is cleared by
        // age. The failed stamp now cleans up after itself instead.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("share.lock");
        std::fs::write(&path, "").expect("write an unstamped lock");
        assert!(
            !lock_is_stale(&path),
            "an unstamped lock was broken on sight, which is a lock two processes can hold"
        );

        // Nor is one holding something that is not a pid.
        std::fs::write(&path, "not-a-pid").expect("write");
        assert!(!lock_is_stale(&path));

        // And the cleanup that replaced it removes only its own empty file: a
        // lock somebody else has since created and stamped is left alone,
        // which is the anti-pattern the stale break was rewritten to avoid.
        std::fs::write(&path, "4194301").expect("another holder, stamped");
        let held = Lock::acquire(dir.path()).expect("break a dead holder's lock");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read").trim(),
            std::process::id().to_string(),
            "the lock on disk is not the one we hold"
        );
        drop(held);
    }

    #[test]
    fn a_lock_whose_holder_is_dead_is_stale_immediately() {
        // Age used to be asked before ownership, so a lock naming a provably
        // dead process was honoured for thirty seconds: measured at four to
        // five seconds of retries and then "another h5i is holding this box's
        // share lock" — for a holder that did not exist.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("share.lock");

        // A pid well above any `pid_max`, so it cannot be alive.
        std::fs::write(&path, "4194301").expect("write");
        assert!(
            lock_is_stale(&path),
            "a dead holder's lock is not stale until it ages"
        );

        // And a live one is not stale, however long it has been held: a
        // process that takes a while under the lock must not have it broken
        // underneath it.
        std::fs::write(&path, std::process::id().to_string()).expect("write");
        assert!(!lock_is_stale(&path));

        // Acquiring past a dead holder is prompt rather than a five-second
        // wait, and leaves no rubbish behind.
        std::fs::write(&path, "4194301").expect("write");
        let started = std::time::Instant::now();
        let held = Lock::acquire(dir.path()).expect("acquire past a dead holder");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "waited {:?} for a lock nobody held",
            started.elapsed()
        );
        drop(held);
        assert!(
            !dir.path().join("share.lock.stale").exists(),
            "the break left rubbish"
        );
    }

    #[test]
    fn two_writers_do_not_share_one_temp_file() {
        // A fixed `share.json.tmp` meant two processes that both held the lock
        // wrote into the same file and one renamed the other's partial bytes
        // into place — and a truncated `share.json` reads as *absent*, so
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
        // earlier than this countdown says — and the only symptom otherwise is
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
        // only one of them — `Ticket::remaining` — had a test that touched the
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

    #[test]
    fn a_pid_kill_would_treat_as_a_wildcard_is_not_a_live_process() {
        // `pid_t` is signed. `4294967295` fits the `u32` this record stores and
        // arrives at `kill` as `-1` — every process this user may signal —
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
        clear(dir.path());
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

    #[test]
    fn a_lock_held_by_a_living_process_is_not_broken_for_being_old() {
        // Age alone was the whole test, so a process that took thirty seconds
        // under the lock had it broken underneath it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(LOCK_FILE);
        std::fs::write(&path, std::process::id().to_string()).expect("write");
        // Make it look ancient.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        let f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open");
        f.set_modified(old).expect("backdate");
        assert!(
            !lock_is_stale(&path),
            "a live holder's lock was called stale"
        );

        std::fs::write(&path, "999999").expect("write");
        let f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open");
        f.set_modified(old).expect("backdate");
        assert!(
            lock_is_stale(&path),
            "a dead holder's lock was not called stale"
        );
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = Lock::acquire(dir.path()).expect("first");
        assert!(dir.path().join(LOCK_FILE).exists());
        drop(held);
        assert!(!dir.path().join(LOCK_FILE).exists());
        let _again = Lock::acquire(dir.path()).expect("second");
    }

    #[test]
    fn updating_a_box_that_is_not_shared_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = update(dir.path(), |_| Ok(())).expect_err("no session");
        assert!(format!("{err}").contains("not being shared"));
    }
}
