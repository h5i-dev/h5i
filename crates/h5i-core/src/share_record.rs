//! Reading `<env>/share.json`, for the parts of h5i that are not the share.
//!
//! `h5i-share` owns this file: it writes it, locks it, and decides what a grant
//! means. But three things below that crate need to know whether a box is being
//! shared right now — `box rm` must not pull a box out from under somebody,
//! `export` must not produce a bundle that is silent about it, and the console
//! must say so while it is open — and `h5i-share` sits *above* `h5i-core`, so
//! none of them can call it.
//!
//! They each grew their own `serde_json::Value` probe instead, and by the time
//! anyone counted there were four definitions of "a live share" in the
//! codebase. The three down here needed exactly one field, a numeric `pid`, so
//! they accepted files the real reader rejects — and a `share.json` containing
//! only `{"pid": 1234}` made `box rm` refuse forever while `box share stop`
//! answered "not being shared", which is a dead end reachable by nothing worse
//! than adding a required field to the record in a later version. None of the
//! three knew about `winding_up` either, a field `h5i-share` added precisely
//! because a live pid is not the same as a share that is serving.
//!
//! So: one reader, here, that fails closed on any file it does not fully
//! understand — and a test in `h5i-share` that writes a real record and asserts
//! this reads it, so the two cannot drift apart silently.

use std::path::Path;

use serde::Deserialize;

/// The file, as everything below `h5i-share` needs to see it.
///
/// Deliberately a *subset* with every field required. Adding a field to the
/// record in `h5i-share` will not break this; making one required there without
/// adding it here would, and the round-trip test is what catches that.
#[derive(Debug, Clone, Deserialize)]
struct OnDisk {
    #[allow(dead_code)]
    v: u8,
    #[allow(dead_code)]
    box_id: String,
    port: u16,
    transport: String,
    pid: u64,
    #[serde(default)]
    winding_up: bool,
    grants: Vec<Grant>,
}

#[derive(Debug, Clone, Deserialize)]
struct Grant {
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
        !self.winding_up && self.live_grants > 0
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
    // Bounded by `i32`, not by `u32`. `pid_t` is signed, so `4294967295` fits
    // a `u32` and still arrives at `kill` as `-1` — "every process" — which
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
        transport: d.transport,
        port: d.port,
        winding_up: d.winding_up,
        live_grants: d
            .grants
            .iter()
            .filter(|g| !g.revoked && g.expires_at > now)
            .count(),
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `EPERM` — somebody else's process with this pid — is counted as not ours
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
        assert!(read_live(dir.path()).is_none(), "a pid alone is not a record");

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
        assert!(read_live(dir.path()).is_none(), "pid that truncates to zero");
    }

    fn full_raw(pid: &str) -> String {
        full(1, "").replace("\"pid\":1,", &format!("\"pid\":{pid},"))
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
        write(dir.path(), &full(std::process::id(), r#","winding_up":true"#));
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
