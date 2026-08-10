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
    pub grants: Vec<Grant>,
}

/// Why a presented secret was not honoured. Each is separate because they are
/// separate problems: an expired ticket needs a new one, a revoked ticket means
/// the sharer meant to cut you off, and an unknown secret means you have the
/// wrong ticket entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    Unknown,
    Expired,
    Revoked,
}

impl Denied {
    /// What the peer is told. Deliberately the same shape for all three at the
    /// wire level (see [`crate::gate`]); the distinction is for the sharer's
    /// terminal and the receipt, not for whoever is probing.
    pub fn explain(self) -> &'static str {
        match self {
            Denied::Unknown => "that ticket is not one this share knows",
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

/// Constant-time comparison of two hex digests.
///
/// They are digests of a high-entropy secret, so a timing oracle here is a
/// stretch even in theory. It is three lines, and the alternative is explaining
/// why an early-exit `==` on a credential comparison is fine.
fn digest_matches(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
            v: 1,
            box_id: box_id.to_string(),
            port,
            transport,
            endpoint: endpoint.to_string(),
            started_at: started_at.to_rfc3339(),
            pid: std::process::id(),
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

    /// True once no grant could admit anyone. The bridge polls this so a
    /// revoked or wholly expired share drops its live connections instead of
    /// serving them until the peer gets bored.
    pub fn is_spent(&self, now: i64) -> bool {
        !self
            .grants
            .iter()
            .any(|g| !g.revoked && g.expires_at > now)
    }

    /// The soonest moment this share stops admitting anyone, if it is bounded.
    pub fn expires_at(&self) -> Option<i64> {
        self.grants
            .iter()
            .filter(|g| !g.revoked)
            .map(|g| g.expires_at)
            .max()
    }
}

/// Mint a grant, returning it and the secret that must be printed **now**.
pub fn mint_grant(
    label: Option<String>,
    expires_at: i64,
) -> Result<(Grant, String), H5iError> {
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
pub struct Lock(PathBuf);

const LOCK_STALE_SECS: u64 = 30;

impl Lock {
    pub fn acquire(env_dir: &Path) -> Result<Lock, H5iError> {
        let path = env_dir.join(LOCK_FILE);
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
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

fn lock_is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.elapsed()
                .map(|d| d.as_secs() > LOCK_STALE_SECS)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Read the session, if this box has one. A malformed file reads as absent
/// rather than as an error: the caller's next move is to write a fresh one.
pub fn read(env_dir: &Path) -> Option<ShareSession> {
    let raw = std::fs::read(session_path(env_dir)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Write the session atomically, owner-readable only.
pub fn write(env_dir: &Path, s: &ShareSession) -> Result<(), H5iError> {
    let path = session_path(env_dir);
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let body = serde_json::to_vec_pretty(s)?;
    std::fs::write(&tmp, &body).map_err(|e| H5iError::with_path(e, &tmp))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // Rename last, so a reader sees either the old table or the new one and
    // never a half-written grant list.
    std::fs::rename(&tmp, &path).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

/// Read, change, write, under the lock. The only way grants should be edited.
pub fn update<T>(
    env_dir: &Path,
    f: impl FnOnce(&mut ShareSession) -> Result<T, H5iError>,
) -> Result<T, H5iError> {
    let _lock = Lock::acquire(env_dir)?;
    let mut s = read(env_dir).ok_or_else(|| {
        H5iError::Metadata(
            "this box is not being shared — run `h5i box share <name>` first".into(),
        )
    })?;
    let out = f(&mut s)?;
    write(env_dir, &s)?;
    Ok(out)
}

/// Forget the session. Called when the sharer exits, so `share ls` describes
/// what is running rather than what once ran.
pub fn clear(env_dir: &Path) {
    let _ = std::fs::remove_file(session_path(env_dir));
}

/// Is the process that wrote this session still alive?
///
/// A share file outliving its process is the ordinary result of a crash or a
/// `kill -9`, and the honest answer to "is this box shared" is no.
pub fn is_live(s: &ShareSession) -> bool {
    #[cfg(unix)]
    {
        s.pid != 0 && unsafe { libc::kill(s.pid as libc::pid_t, 0) } == 0
    }
    #[cfg(not(unix))]
    {
        let _ = s;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_secret_itself_is_never_written_down() {
        // The property the whole file rests on: someone who reads share.json
        // learns that a grant exists, not how to use it.
        let (g, secret) = mint_grant(Some("alex".into()), 4_000_000_000).expect("mint");
        let s = session_with(vec![g]);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains(&secret), "share.json must not carry the secret");
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
        assert_eq!(s.authorize(&expired_secret, 2_000).unwrap_err(), Denied::Expired);
        assert_eq!(s.authorize(&revoked_secret, 2_000).unwrap_err(), Denied::Revoked);
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
            s.grants.iter_mut().filter(|g| g.id == id).for_each(|g| g.revoked = true);
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
