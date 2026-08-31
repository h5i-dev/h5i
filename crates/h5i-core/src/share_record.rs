//! Reading `<env>/share.json`, for the parts of h5i that are not the share.
//!
//! `h5i-share` owns this file: it writes it, locks it, and decides what a grant
//! means. But three things below that crate need to know whether a box is being
//! shared right now (`box rm` must not pull a box out from under somebody,
//! `export` must not produce a bundle that is silent about it, and the console
//! must say so while it is open) and `h5i-share` sits *above* `h5i-core`, so
//! none of them can call it.
//!
//! They each grew their own `serde_json::Value` probe instead, and by the time
//! anyone counted there were four definitions of "a live share" in the
//! codebase. The three down here needed exactly one field, a numeric `pid`, so
//! they accepted files the real reader rejects, and a `share.json` containing
//! only `{"pid": 1234}` made `box rm` refuse forever while `box share stop`
//! answered "not being shared", which is a dead end reachable by nothing worse
//! than adding a required field to the record in a later version. None of the
//! three knew about `winding_up` either, a field `h5i-share` added precisely
//! because a live pid is not the same as a share that is serving.
//!
//! So: one reader, here, that fails closed on any file it does not fully
//! understand, and a test in `h5i-share` that writes a real record and asserts
//! this reads it, so the two cannot drift apart silently.

use std::path::Path;

use serde::Deserialize;

use crate::error::H5iError;

/// The file both sides take to make "is this box shared" a decision rather
/// than a guess.
const GATE_FILE: &str = "share-gate.lock";

/// How long a caller waits for the gate before giving up.
///
/// Generous, because the operations it guards are short and the alternative,
/// failing fast, turns an ordinary overlap into an error the user has to
/// understand. `rm --force` on a large worktree is the longest of them.
///
/// Unix only, like the `flock` it bounds: there is no waiting on the other
/// branch, because there is nothing there to wait for.
#[cfg(unix)]
const GATE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Exclusive access to the *decision* about whether this box is shared.
///
/// Everything that reads [`read_live`] to decide what to do next (`apply`,
/// `rebase`, `abort`, `rm`, `export`) and the one thing that changes the
/// answer, `h5i-share`'s `session::claim`, takes this first and holds it for
/// the whole operation.
///
/// Without it the check and the operation were two steps with a gap between
/// them, and `run.lock` did not close it: a share does not hold `run.lock`,
/// the box *session* it stands on does, and a share's own claim happens after
/// its transport setup, which for `--tunnel` waits up to forty-five seconds
/// for a URL. So the writer could exit during that wait, releasing `run.lock`;
/// `rebase` or `export` or `rm` would then see no `share.json` at all and
/// proceed; and the in-flight start would claim and announce a public URL
/// while that operation was running. A visitor admitted while `rebase`
/// force-checks out the worktree, or a box deleted out from under a share that
/// then recreates its directory to write a receipt into, are both reachable
/// that way.
///
/// Ordered *before* `run.lock` everywhere it is taken with it, and never taken
/// while holding `run.lock`, so the two cannot deadlock. `h5i-share` never
/// takes `run.lock` at all.
#[derive(Debug)]
pub struct ShareGate {
    #[allow(dead_code)]
    file: std::fs::File,
}

/// Take the gate, waiting up to `GATE_WAIT` (30s).
///
/// Non-blocking `flock` in a retry loop rather than a blocking one: a blocking
/// `flock` cannot be given a deadline, and a lifecycle op that waits forever on
/// a wedged share is the thing operators file bugs about.
pub fn share_gate(env_dir: &Path) -> Result<ShareGate, H5iError> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let path = env_dir.join(GATE_FILE);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        let deadline = std::time::Instant::now() + GATE_WAIT;
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(ShareGate { file });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(H5iError::with_path(err, &path));
            }
            if std::time::Instant::now() >= deadline {
                return Err(H5iError::Metadata(format!(
                    "another command is starting or ending a share of this box and has held it \
                     for {}s. Try again, or `h5i box share stop <name> --force` if a share is \
                     wedged.",
                    GATE_WAIT.as_secs()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    #[cfg(not(unix))]
    {
        let path = env_dir.join(GATE_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        Ok(ShareGate { file })
    }
}

/// The format this reader understands. Must match `h5i_share::session`'s
/// `SESSION_VERSION`; a record from any other version is not a record here.
const SESSION_VERSION: u8 = 1;

/// The file, as everything below `h5i-share` needs to see it.
///
/// Deliberately a *subset* with every field required. Adding a field to the
/// record in `h5i-share` will not break this; making one required there without
/// adding it here would, and the round-trip test is what catches that.
///
/// "Every field required" was the claim and not the code: `endpoint`,
/// `started_at`, and a grant's `id` and `secret_sha256` were all required by the
/// real deserializer up in `h5i-share` and absent from this one, and
/// `transport` took any string at all. That asymmetry is exactly the
/// split-brain this module exists to prevent, and it points the wrong way: a
/// record missing `endpoint` read as *live* down here (the console advertised
/// it, `apply`/`rebase`/`abort` refused as "being shared") while
/// `box share status` and `box share stop`, which use the real reader, said the
/// box was not being shared and could not perform the recovery the refusal
/// recommended. Reachable under version skew or a hand-edited file.
#[derive(Debug, Clone, Deserialize)]
struct OnDisk {
    v: u8,
    #[allow(dead_code)]
    box_id: String,
    port: u16,
    transport: Transport,
    #[allow(dead_code)]
    endpoint: String,
    #[allow(dead_code)]
    started_at: String,
    pid: u64,
    #[serde(default)]
    winding_up: bool,
    /// The box is claimed and the transport is not up yet. Admits nobody, and
    /// is still a share: it exists precisely so the window it covers is not
    /// invisible to the verbs that read this file.
    #[serde(default)]
    starting: bool,
    grants: Vec<Grant>,
}

/// The same two-variant enum `h5i-share` writes, spelled the same way. A free
/// `String` here accepted a transport this h5i has no idea how to describe and
/// then printed it to a reviewer as fact.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Transport {
    P2p,
    Tunnel,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::P2p => "p2p",
            Transport::Tunnel => "tunnel",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Grant {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    secret_sha256: String,
    #[serde(default)]
    revoked: bool,
    expires_at: i64,
}

/// A share whose process is still alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRecord {
    pub pid: u32,
    /// `p2p` or `tunnel`, as written.
    pub transport: String,
    /// The port inside the box.
    pub port: u16,
    /// The serving process has decided to stop and has not finished. It is
    /// alive, and it is not admitting anybody.
    pub winding_up: bool,
    /// The box is claimed and the transport is not up yet.
    pub starting: bool,
    /// Grants that could still admit somebody: not revoked, not expired.
    pub live_grants: usize,
}

impl ShareRecord {
    /// Is somebody able to reach into this box right now?
    ///
    /// Not the same as "the process is alive", which is what every caller used
    /// to test. A share that is winding up, or whose grants have all been
    /// revoked, has a live pid and admits nobody.
    pub fn is_admitting(&self) -> bool {
        !self.winding_up && !self.starting && self.live_grants > 0
    }
}

/// The share serving this box, if one is.
///
/// `None` for: no file, a file this cannot fully parse, and a record whose
/// process is gone. Every one of those is "nothing is serving", and answering
/// anything else would let a malformed file wedge `box rm`.
pub fn read_live(env_dir: &Path) -> Option<ShareRecord> {
    let raw = std::fs::read(env_dir.join("share.json")).ok()?;
    let d: OnDisk = serde_json::from_slice(&raw).ok()?;
    // A version this reader does not know is a file it does not understand,
    // which is the one case this whole module is built to fail closed on. `v`
    // was decoded and then ignored, so a v2 record written by a newer h5i read
    // as a v1 one here for as long as the field names still lined up.
    if d.v != SESSION_VERSION {
        return None;
    }
    // Bounded by `i32`, not by `u32`. `pid_t` is signed, so `4294967295` fits
    // a `u32` and still arrives at `kill` as `-1`, "every process", which
    // returns success and made a nonsense record read as live forever. `2^32`
    // truncates to `0`, "this process group", with the same result. Both are
    // out of range for a pid and both are refused here.
    let pid = i32::try_from(d.pid).ok().filter(|p| *p > 0)? as u32;
    if !pid_alive(pid) {
        return None;
    }
    let now = chrono::Utc::now().timestamp();
    Some(ShareRecord {
        pid,
        transport: d.transport.as_str().to_string(),
        port: d.port,
        winding_up: d.winding_up,
        starting: d.starting,
        live_grants: d
            .grants
            .iter()
            .filter(|g| !g.revoked && g.expires_at > now)
            .count(),
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `EPERM`, somebody else's process with this pid, is counted as not ours
    // and therefore not a share of this box, which is the safe answer for every
    // caller here.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) {
        std::fs::write(dir.join("share.json"), body).expect("write");
    }

    fn full(pid: u32, extra: &str) -> String {
        format!(
            r#"{{"v":1,"box_id":"env/a/demo","port":3000,"transport":"tunnel",
                "endpoint":"https://x","started_at":"2026-01-01T00:00:00Z","pid":{pid},
                "grants":[{{"id":"a","secret_sha256":"ff","expires_at":4000000000,
                "revoked":false}}]{extra}}}"#
        )
    }

    #[test]
    fn a_file_this_cannot_fully_understand_is_not_a_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_live(dir.path()).is_none(), "no file");

        // The shape that made `box rm` refuse forever while `share stop`
        // answered "not being shared": enough for a pid probe, nowhere near a
        // record. This is what an added-required-field version bump looks like
        // from down here, so failing closed on it is the whole point.
        write(dir.path(), &format!(r#"{{"pid":{}}}"#, std::process::id()));
        assert!(
            read_live(dir.path()).is_none(),
            "a pid alone is not a record"
        );

        write(dir.path(), "");
        assert!(read_live(dir.path()).is_none(), "empty");
        write(dir.path(), "{\"v\":1,\"box_id\":\"a\",\"port\":3000,");
        assert!(read_live(dir.path()).is_none(), "truncated mid-write");
        write(dir.path(), &full(0, ""));
        assert!(read_live(dir.path()).is_none(), "pid 0");

        // A pid that truncates to something `kill` treats as a wildcard.
        write(dir.path(), &full_raw("4294967295"));
        assert!(read_live(dir.path()).is_none(), "pid past u32");
        write(dir.path(), &full_raw("4294967296"));
        assert!(
            read_live(dir.path()).is_none(),
            "pid that truncates to zero"
        );
    }

    fn full_raw(pid: &str) -> String {
        full(1, "").replace("\"pid\":1,", &format!("\"pid\":{pid},"))
    }

    /// Every field the real reader requires is required here too.
    ///
    /// The asymmetry this pins down was not theoretical. A record with a live
    /// pid, a live-looking grant and no `endpoint` made this function return
    /// `Some`, so the console advertised the share and `apply`/`rebase`/
    /// `abort` refused with "this box is being shared", while `h5i box share
    /// status` and `stop`, which go through the real deserializer, answered
    /// "not being shared" and could not perform the recovery the refusal
    /// recommended. The mirror of that dead end is what `h5i-share`'s
    /// `what_this_crate_writes_is_what_h5i_core_reads` covers going the other
    /// way; this is the closing half.
    #[test]
    fn a_record_missing_any_required_field_is_not_a_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();
        let good = full(pid, "");
        write(dir.path(), &good);
        assert!(read_live(dir.path()).is_some(), "the fixture must be valid");

        // Top-level fields, removed one at a time from a record that is
        // otherwise complete and live.
        for field in [
            "v",
            "box_id",
            "port",
            "transport",
            "endpoint",
            "started_at",
            "pid",
            "grants",
        ] {
            let mut v: serde_json::Value =
                serde_json::from_str(&good).expect("the fixture parses as JSON");
            v.as_object_mut().expect("object").remove(field);
            write(dir.path(), &v.to_string());
            assert!(
                read_live(dir.path()).is_none(),
                "a record with no `{field}` was read as a live share"
            );
        }

        // And the two grant fields the real reader requires.
        for field in ["id", "secret_sha256", "expires_at"] {
            let mut v: serde_json::Value =
                serde_json::from_str(&good).expect("the fixture parses as JSON");
            v["grants"][0]
                .as_object_mut()
                .expect("grant object")
                .remove(field);
            write(dir.path(), &v.to_string());
            assert!(
                read_live(dir.path()).is_none(),
                "a grant with no `{field}` was read as a live grant"
            );
        }
    }

    #[test]
    fn an_unknown_transport_or_version_is_not_a_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();

        // `transport` was a free `String`, so anything at all round-tripped
        // into a `ShareRecord` and out to a reviewer as a statement about how
        // the traffic was carried.
        let odd = full(pid, "").replace(
            "\"transport\":\"tunnel\"",
            "\"transport\":\"carrier-pigeon\"",
        );
        write(dir.path(), &odd);
        assert!(read_live(dir.path()).is_none(), "an unknown transport");

        // A newer record read as a v1 one for as long as the field names lined
        // up, which is the situation an upgrade or a rollback creates.
        let v2 = full(pid, "").replace("\"v\":1", "\"v\":2");
        write(dir.path(), &v2);
        assert!(read_live(dir.path()).is_none(), "a v2 record");
    }

    #[test]
    fn a_grant_is_live_up_to_its_expiry_and_not_through_it() {
        // The sixth definition of "still valid", in a different crate from the
        // other five, and the only one below `h5i-share`. It had no boundary
        // test either: `>=` here and `>` there would have `box rm` refusing a
        // share whose door the share itself had already shut, for one second
        // per expiry, or the reverse, which is worse.
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();
        let at = |t: i64| {
            full(pid, "").replace("\"expires_at\":4000000000", &format!("\"expires_at\":{t}"))
        };

        let now = chrono::Utc::now().timestamp();
        write(dir.path(), &at(now + 1));
        assert_eq!(read_live(dir.path()).expect("live").live_grants, 1);

        write(dir.path(), &at(now));
        let r = read_live(dir.path()).expect("the process is still alive");
        assert_eq!(
            r.live_grants, 0,
            "a grant expiring this second still counted"
        );
        assert!(!r.is_admitting());
    }

    #[test]
    fn a_live_pid_is_not_the_same_as_admitting_anybody() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &full(std::process::id(), ""));
        let r = read_live(dir.path()).expect("a live share");
        assert_eq!(r.transport, "tunnel");
        assert_eq!(r.port, 3000);
        assert_eq!(r.live_grants, 1);
        assert!(r.is_admitting());

        // Winding up: the process is alive and writing its receipt, and it
        // refuses to mint a ticket. Saying "somebody can reach this box right
        // now" of it is an overclaim in the direction of alarm.
        write(
            dir.path(),
            &full(std::process::id(), r#","winding_up":true"#),
        );
        let r = read_live(dir.path()).expect("still a live process");
        assert!(r.winding_up);
        assert!(!r.is_admitting());

        // Every grant revoked is the same story by a different route.
        let revoked = full(std::process::id(), "").replace("\"revoked\":false", "\"revoked\":true");
        write(dir.path(), &revoked);
        let r = read_live(dir.path()).expect("still a live process");
        assert_eq!(r.live_grants, 0);
        assert!(!r.is_admitting());
    }
}
