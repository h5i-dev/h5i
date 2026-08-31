//! What a box is, on the runner's disk.
//!
//! ```text
//! <state_dir>/
//!   creating/<operation_id>/   being built; never a box yet
//!   live/<box_id>/
//!     box.json                 what it is
//!     policy.json              the resolved policy, verbatim as it arrived
//!     lease.json               when it stops being ours to keep
//!     work/                    the clone
//! ```
//!
//! Two ideas carry the whole module.
//!
//! A box exists at exactly one instant (ROADMAP.md R7). Everything is built
//! under `creating/<operation_id>` and an atomic rename into `live/<box_id>` is
//! the moment it becomes real. There is no half-built state for a crash to
//! invent, and what a crash leaves behind names the *attempt*, not the box.
//!
//! The lease is a fact on disk, not a timer (R11). There is no daemon on the
//! runner to watch a clock, so expiry is something any later invocation can
//! evaluate from what it finds, which is why every worker invocation sweeps
//! before it does its own work: the reaper is whoever turns up next.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::proto::{BoxState, BoxSummary, LeaseSpec, ResourceLimits, SourceKind};

/// How long a `creating/` directory may sit before a sweep takes it.
///
/// It is not a lease: nothing ever refreshed it, because a create that got far
/// enough to refresh anything would have renamed itself into `live/`. This is
/// only "long enough that a slow create in progress is not reaped underneath
/// itself", which is a bound on a transfer, not on a box's life.
const CREATING_TTL: Duration = Duration::hours(3);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("runner state at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("box `{0}` does not exist on this runner")]
    Unknown(String),

    /// A create arrived for a name that is taken by a *different* box. Not the
    /// idempotent case: that one is decided by comparing request digests, and
    /// this is what is left when they differ.
    #[error(
        "box `{box_id}` already exists on this runner and was built from a different request \
         — destroy it first, or use another name"
    )]
    Conflict { box_id: String },

    /// Someone else holds the box. Named rather than waited on: an RPC that
    /// silently hangs for minutes is worse than one that refuses and says why.
    #[error(
        "box `{box_id}` is busy — something else is using it and this needed a {wanted} hold. \
         Wait for it to finish, or check `h5i runner boxes` for what is running."
    )]
    Busy {
        box_id: String,
        wanted: &'static str,
    },

    /// Something a runner needs that this platform does not have.
    #[error("{0}")]
    Unsupported(&'static str),

    #[error("runner state is not readable as a box record at {path}: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl StoreError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// What was built, and from what.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxRecord {
    pub box_id: String,
    /// The attempt that built it. Kept so a `creating/` directory left by a
    /// crash can be told apart from the box that eventually succeeded.
    pub operation_id: String,
    /// The idempotency key (R7). A re-send matching this returns the existing
    /// box; a different one under the same name is [`StoreError::Conflict`].
    pub request_digest: String,
    pub isolation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub limits: ResourceLimits,
    pub policy_digest: String,
    /// The container backing this box, when the tier has one. Stored rather
    /// than derived so that destroy can clean up a runtime this module knows
    /// nothing else about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub source_kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// When a box stops being ours to keep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub created_at: DateTime<Utc>,
    /// Last time an RPC touched this box.
    pub touched_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub hard_ttl_secs: u64,
}

impl Lease {
    fn new(spec: LeaseSpec, now: DateTime<Utc>) -> Self {
        Self {
            created_at: now,
            touched_at: now,
            ttl_secs: spec.ttl_secs,
            hard_ttl_secs: spec.hard_ttl_secs,
        }
    }

    /// The earlier of the two deadlines: idle since last touch, and absolute
    /// since creation. The hard one is not refreshed by use, which is what
    /// stops a box that is busy forever from living forever.
    pub fn expires_at(&self) -> DateTime<Utc> {
        let idle = self.touched_at + Duration::seconds(self.ttl_secs as i64);
        let hard = self.created_at + Duration::seconds(self.hard_ttl_secs as i64);
        idle.min(hard)
    }

    pub fn expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at()
    }
}

/// A box under construction. Becomes a box, or is swept.
pub struct Creating {
    dir: PathBuf,
    operation_id: String,
}

impl Creating {
    /// Where the source is unpacked. Inside the attempt's directory, so a crash
    /// takes the partial clone with it.
    pub fn work_dir(&self) -> PathBuf {
        self.dir.join("work")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

/// Every box on this runner.
pub struct BoxStore {
    root: PathBuf,
}

impl BoxStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn live_root(&self) -> PathBuf {
        self.root.join("live")
    }

    fn creating_root(&self) -> PathBuf {
        self.root.join("creating")
    }

    /// One box's directory. The id is path-checked by
    /// [`crate::proto::check_id`] before it ever reaches here, and this joins
    /// a single segment, so it cannot escape.
    pub fn box_dir(&self, box_id: &str) -> PathBuf {
        self.live_root().join(box_id)
    }

    /// Start building. The directory is the attempt's, not the box's.
    pub fn begin(&self, operation_id: &str) -> Result<Creating, StoreError> {
        let dir = self.creating_root().join(operation_id);
        // A repeated operation id means a retry of the same attempt; its
        // leftovers are this attempt's to replace, not to merge with.
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        }
        std::fs::create_dir_all(dir.join("work")).map_err(|e| StoreError::io(&dir, e))?;
        Ok(Creating {
            dir,
            operation_id: operation_id.to_string(),
        })
    }

    /// The instant a box exists.
    ///
    /// The record and the lease are written *inside* the attempt's directory
    /// first, so the rename publishes a directory that is already complete.
    /// Writing them after the rename would open a window where a sweep could
    /// find a live box with no lease and have to guess what to do with it.
    pub fn commit(
        &self,
        creating: Creating,
        record: &BoxRecord,
        lease: LeaseSpec,
        policy: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<BoxRecord, StoreError> {
        write_json(&creating.dir.join("box.json"), record)?;
        write_json(&creating.dir.join("policy.json"), policy)?;
        write_json(&creating.dir.join("lease.json"), &Lease::new(lease, now))?;

        let target = self.box_dir(&record.box_id);
        std::fs::create_dir_all(self.live_root())
            .map_err(|e| StoreError::io(self.live_root(), e))?;

        // `rename` onto an existing non-empty directory fails, which is the
        // behaviour wanted: two creates racing for one name must not merge.
        // The caller has already decided the idempotent case by digest, so
        // anything arriving here is a genuine collision.
        std::fs::rename(&creating.dir, &target).map_err(|e| {
            if target.exists() {
                StoreError::Conflict {
                    box_id: record.box_id.clone(),
                }
            } else {
                StoreError::io(&target, e)
            }
        })?;
        Ok(record.clone())
    }

    pub fn find(&self, box_id: &str) -> Result<Option<BoxRecord>, StoreError> {
        let path = self.box_dir(box_id).join("box.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| StoreError::Corrupt { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    pub fn lease(&self, box_id: &str) -> Result<Option<Lease>, StoreError> {
        let path = self.box_dir(box_id).join("lease.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| StoreError::Corrupt { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    /// Refresh the idle half of a lease. The hard deadline is untouched.
    pub fn touch(&self, box_id: &str, now: DateTime<Utc>) -> Result<(), StoreError> {
        let Some(mut lease) = self.lease(box_id)? else {
            return Ok(());
        };
        lease.touched_at = now;
        write_json(&self.box_dir(box_id).join("lease.json"), &lease)
    }

    /// Every box, expired ones included: a caller sweeping needs to see them.
    pub fn list(&self, now: DateTime<Utc>) -> Result<Vec<BoxSummary>, StoreError> {
        let mut out = Vec::new();
        for name in self.live_names()? {
            let Some(record) = self.find(&name)? else {
                continue;
            };
            let lease = self.lease(&name)?;
            let (expires, expired) = match &lease {
                Some(l) => (l.expires_at(), l.expired_at(now)),
                // A box with no lease is not immortal: a missing lease is
                // treated as expired so that a partially-removed box is swept
                // rather than kept forever.
                None => (record.created_at, true),
            };
            out.push(BoxSummary {
                box_id: record.box_id.clone(),
                state: if expired {
                    BoxState::Expired
                } else {
                    BoxState::Live
                },
                isolation: record.isolation.clone(),
                created_at: record.created_at,
                lease_expires_at: expires,
                container: record.container.clone(),
                policy_digest: record.policy_digest.clone(),
            });
        }

        // Attempts that never became boxes are visible too, so an operator can
        // see a create that died rather than wonder where the disk went.
        for name in self.creating_names()? {
            let dir = self.creating_root().join(&name);
            let started = dir_mtime(&dir).unwrap_or(now);
            out.push(BoxSummary {
                box_id: name,
                state: BoxState::Creating,
                isolation: String::new(),
                created_at: started,
                lease_expires_at: started + CREATING_TTL,
                container: None,
                policy_digest: String::new(),
            });
        }

        out.sort_by(|a, b| a.box_id.cmp(&b.box_id));
        Ok(out)
    }

    /// Delete a box's directory. The caller stops its container first: this
    /// module does not know what a container is.
    pub fn remove(&self, box_id: &str) -> Result<bool, StoreError> {
        let dir = self.box_dir(box_id);
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        Ok(true)
    }

    /// What a sweep would take, without taking it.
    ///
    /// Separated from the removal so the caller can stop containers for exactly
    /// the boxes about to go, and so a dry run is possible at all.
    pub fn reapable(&self, now: DateTime<Utc>, all: bool) -> Result<Vec<Reapable>, StoreError> {
        let mut out = Vec::new();
        for name in self.live_names()? {
            let record = self.find(&name)?;
            let expired = match self.lease(&name)? {
                Some(l) => l.expired_at(now),
                None => true,
            };
            if all || expired {
                out.push(Reapable {
                    box_id: name,
                    container: record.and_then(|r| r.container),
                    creating: false,
                });
            }
        }
        for name in self.creating_names()? {
            let dir = self.creating_root().join(&name);
            let started = dir_mtime(&dir).unwrap_or(now);
            if all || now >= started + CREATING_TTL {
                out.push(Reapable {
                    box_id: name,
                    container: None,
                    creating: true,
                });
            }
        }
        Ok(out)
    }

    /// Remove one thing a sweep chose.
    pub fn reap(&self, item: &Reapable) -> Result<(), StoreError> {
        let dir = if item.creating {
            self.creating_root().join(&item.box_id)
        } else {
            self.box_dir(&item.box_id)
        };
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        }
        Ok(())
    }

    /// Take the box's lock, or say who has it.
    ///
    /// Never waits. A caller blocked behind another request has no way to say
    /// so over a protocol with one reply, and an RPC that silently hangs for
    /// minutes is worse than one that refuses and names the reason.
    pub fn lock(&self, box_id: &str, kind: LockKind) -> Result<BoxLock, StoreError> {
        let path = self.box_dir(box_id).join("lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let op = match kind {
                LockKind::Exclusive => libc::LOCK_EX,
                LockKind::Shared => libc::LOCK_SH,
            } | libc::LOCK_NB;
            // SAFETY: `file` is open for the length of this call and the fd is
            // valid; `flock` only takes an advisory lock on it.
            let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
            if rc != 0 {
                return Err(StoreError::Busy {
                    box_id: box_id.to_string(),
                    wanted: match kind {
                        LockKind::Exclusive => "exclusive",
                        LockKind::Shared => "shared",
                    },
                });
            }
        }

        // A lock that silently does not lock is worse than no lock: every
        // caller above believes exec and export are mutually exclusive. A
        // runner must be Linux anyway (ROADMAP.md R1), so the honest answer off
        // Unix is to refuse rather than to hand back a handle that guards
        // nothing.
        #[cfg(not(unix))]
        {
            let _ = (kind, file);
            return Err(StoreError::Unsupported(
                "box locking needs flock, which this platform does not have — a runner must \
                 be a Linux machine",
            ));
        }

        #[cfg(unix)]
        Ok(BoxLock { _file: file })
    }

    fn live_names(&self) -> Result<Vec<String>, StoreError> {
        dir_names(&self.live_root())
    }

    fn creating_names(&self) -> Result<Vec<String>, StoreError> {
        dir_names(&self.creating_root())
    }
}

/// A held lock on one box.
///
/// Worker invocations are separate processes, so the lock has to be a fact the
/// filesystem holds rather than something in memory: `flock` on a file beside
/// the box, released when the handle drops or when the process dies, which is
/// the property that matters.
///
/// The rule (ROADMAP.md R8): create, destroy and export take it *exclusive*;
/// exec takes it *shared*. An export while execs are running would read a torn
/// tree, and a torn tree that passes validation is worse than a refused request.
#[derive(Debug)]
pub struct BoxLock {
    _file: std::fs::File,
}

/// What a caller wants from [`BoxStore::lock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// One at a time, and nothing else alongside.
    Exclusive,
    /// Many at once, but never beside an exclusive holder.
    Shared,
}

/// Something a sweep would remove.
#[derive(Debug, Clone)]
pub struct Reapable {
    pub box_id: String,
    pub container: Option<String>,
    /// An attempt that never became a box, rather than a box whose lease ran
    /// out. Different directory, and nothing to stop.
    pub creating: bool,
}

fn dir_names(root: &Path) -> Result<Vec<String>, StoreError> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::io(root, e)),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

fn dir_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let meta = std::fs::metadata(path).ok()?;
    Some(DateTime::<Utc>::from(meta.modified().ok()?))
}

/// Write JSON through a temporary file and a rename, so a reader never sees
/// half a record and a crash never leaves one.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| StoreError::Corrupt {
        path: path.to_path_buf(),
        source,
    })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| StoreError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| StoreError::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BoxStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BoxStore::new(dir.path());
        (dir, store)
    }

    fn record(box_id: &str, op: &str, digest: &str) -> BoxRecord {
        BoxRecord {
            box_id: box_id.into(),
            operation_id: op.into(),
            request_digest: digest.into(),
            isolation: "container".into(),
            image: Some("debian".into()),
            limits: ResourceLimits::default(),
            policy_digest: "b".repeat(64),
            container: Some(format!("h5i-{box_id}")),
            source_kind: SourceKind::GitBundle,
            base_commit: Some("deadbeef".into()),
            created_at: Utc::now(),
        }
    }

    fn commit(store: &BoxStore, box_id: &str, op: &str, digest: &str) -> BoxRecord {
        let creating = store.begin(op).expect("begin");
        std::fs::write(creating.work_dir().join("file.txt"), b"source").unwrap();
        store
            .commit(
                creating,
                &record(box_id, op, digest),
                LeaseSpec::default(),
                &serde_json::json!({"profile": "agent"}),
                Utc::now(),
            )
            .expect("commit")
    }

    #[test]
    fn a_box_exists_only_after_the_rename() {
        let (_d, store) = store();
        let creating = store.begin("op1").expect("begin");

        // Everything is under the attempt's directory, and the box's name does
        // not exist yet. There is no half-built box for a crash to leave.
        assert!(creating.work_dir().exists());
        assert!(!store.box_dir("demo").exists());
        assert!(store.find("demo").unwrap().is_none());

        store
            .commit(
                creating,
                &record("demo", "op1", &"a".repeat(64)),
                LeaseSpec::default(),
                &serde_json::json!({}),
                Utc::now(),
            )
            .expect("commit");

        assert!(store.box_dir("demo").exists());
        assert_eq!(store.find("demo").unwrap().unwrap().box_id, "demo");
    }

    #[test]
    fn the_published_directory_is_complete_the_instant_it_appears() {
        // Writing the record after the rename would leave a window where a
        // sweep finds a live box with no lease and has to guess.
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        let dir = store.box_dir("demo");
        for f in ["box.json", "policy.json", "lease.json"] {
            assert!(dir.join(f).exists(), "{f} should be there already");
        }
        assert!(dir.join("work/file.txt").exists(), "and so should the source");
    }

    #[test]
    fn a_crash_leaves_an_attempt_that_names_the_attempt_not_the_box() {
        let (_d, store) = store();
        let creating = store.begin("op-crashed").expect("begin");
        let dir = creating.dir().to_path_buf();
        drop(creating); // the process died here

        assert!(dir.exists());
        assert!(store.find("demo").unwrap().is_none(), "no box was made");

        let listed = store.list(Utc::now()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, BoxState::Creating);
        assert_eq!(listed[0].box_id, "op-crashed");
    }

    #[test]
    fn a_stale_attempt_is_swept_and_a_fresh_one_is_left_alone() {
        // A create in progress must not be reaped underneath itself.
        let (_d, store) = store();
        store.begin("op-fresh").expect("begin");
        let now = Utc::now();

        assert!(
            store.reapable(now, false).unwrap().is_empty(),
            "a fresh attempt is not stale"
        );

        let later = now + CREATING_TTL + Duration::minutes(1);
        let reapable = store.reapable(later, false).unwrap();
        assert_eq!(reapable.len(), 1);
        assert!(reapable[0].creating);
        store.reap(&reapable[0]).unwrap();
        assert!(store.list(later).unwrap().is_empty());
    }

    #[test]
    fn a_second_create_for_a_taken_name_conflicts() {
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));

        let creating = store.begin("op2").expect("begin");
        let err = store.commit(
            creating,
            &record("demo", "op2", &"different".repeat(8)),
            LeaseSpec::default(),
            &serde_json::json!({}),
            Utc::now(),
        );
        assert!(matches!(err, Err(StoreError::Conflict { .. })));
        // And the first box is untouched.
        assert_eq!(store.find("demo").unwrap().unwrap().operation_id, "op1");
    }

    #[test]
    fn a_lease_expires_on_whichever_deadline_comes_first() {
        let now = Utc::now();
        let lease = Lease {
            created_at: now,
            touched_at: now,
            ttl_secs: 3600,
            hard_ttl_secs: 7200,
        };
        assert_eq!(lease.expires_at(), now + Duration::seconds(3600));
        assert!(!lease.expired_at(now + Duration::seconds(3599)));
        assert!(lease.expired_at(now + Duration::seconds(3600)));

        // Touched near the hard deadline: the hard one wins, which is what
        // stops a box that is busy forever from living forever.
        let lease = Lease {
            created_at: now,
            touched_at: now + Duration::seconds(7000),
            ttl_secs: 3600,
            hard_ttl_secs: 7200,
        };
        assert_eq!(lease.expires_at(), now + Duration::seconds(7200));
    }

    #[test]
    fn touching_a_box_moves_the_idle_deadline_and_not_the_hard_one() {
        let (_d, store) = store();
        let start = Utc::now();
        commit(&store, "demo", "op1", &"a".repeat(64));
        let before = store.lease("demo").unwrap().unwrap();

        let later = start + Duration::seconds(600);
        store.touch("demo", later).unwrap();
        let after = store.lease("demo").unwrap().unwrap();

        assert!(after.touched_at > before.touched_at);
        assert_eq!(
            after.created_at, before.created_at,
            "the hard deadline's origin never moves"
        );
        assert!(after.expires_at() > before.expires_at());
    }

    #[test]
    fn an_expired_box_is_reapable_and_a_live_one_is_not() {
        let (_d, store) = store();
        let now = Utc::now();
        commit(&store, "demo", "op1", &"a".repeat(64));

        assert!(store.reapable(now, false).unwrap().is_empty());
        assert_eq!(store.list(now).unwrap()[0].state, BoxState::Live);

        let expired = now + Duration::seconds(LeaseSpec::default().ttl_secs as i64 + 1);
        assert_eq!(store.list(expired).unwrap()[0].state, BoxState::Expired);

        let reapable = store.reapable(expired, false).unwrap();
        assert_eq!(reapable.len(), 1);
        assert_eq!(reapable[0].box_id, "demo");
        assert_eq!(
            reapable[0].container.as_deref(),
            Some("h5i-demo"),
            "the sweep must know what to stop"
        );
    }

    #[test]
    fn a_box_with_no_lease_is_swept_rather_than_kept_forever() {
        // A partially-removed box must not become immortal.
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        std::fs::remove_file(store.box_dir("demo").join("lease.json")).unwrap();

        let now = Utc::now();
        assert_eq!(store.list(now).unwrap()[0].state, BoxState::Expired);
        assert_eq!(store.reapable(now, false).unwrap().len(), 1);
    }

    #[test]
    fn reaping_everything_takes_live_boxes_too() {
        let (_d, store) = store();
        commit(&store, "one", "op1", &"a".repeat(64));
        commit(&store, "two", "op2", &"b".repeat(64));
        let now = Utc::now();

        assert!(store.reapable(now, false).unwrap().is_empty());
        assert_eq!(store.reapable(now, true).unwrap().len(), 2);
    }

    #[test]
    fn removing_something_that_is_already_gone_is_not_an_error() {
        // A retry after a lost answer must not fail: destroying something
        // already destroyed is the state the caller asked for.
        let (_d, store) = store();
        assert!(!store.remove("never-existed").unwrap());
        commit(&store, "demo", "op1", &"a".repeat(64));
        assert!(store.remove("demo").unwrap());
        assert!(!store.remove("demo").unwrap());
    }

    #[test]
    fn a_corrupt_record_names_its_own_file() {
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        std::fs::write(store.box_dir("demo").join("box.json"), b"{not json").unwrap();
        match store.find("demo") {
            Err(StoreError::Corrupt { path, .. }) => {
                assert!(path.ends_with("box.json"));
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn listing_an_empty_runner_is_empty_rather_than_an_error() {
        let (_d, store) = store();
        assert!(store.list(Utc::now()).unwrap().is_empty());
        assert!(store.reapable(Utc::now(), true).unwrap().is_empty());
    }

    #[test]
    fn a_repeated_attempt_id_replaces_its_own_leftovers() {
        // A retry of the same attempt owns whatever the last try left; merging
        // with it would build a box out of two different transfers.
        let (_d, store) = store();
        let first = store.begin("op1").expect("begin");
        std::fs::write(first.work_dir().join("stale.txt"), b"old").unwrap();
        drop(first);

        let second = store.begin("op1").expect("begin again");
        assert!(
            !second.work_dir().join("stale.txt").exists(),
            "the previous attempt's bytes must not survive into this one"
        );
    }

    #[test]
    fn an_export_cannot_run_while_an_exec_holds_the_box() {
        // The rule R8 states, and the reason: an export beside a running build
        // reads a torn tree, and a torn tree that passes validation is worse
        // than a refused request.
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));

        let exec = store.lock("demo", LockKind::Shared).expect("exec may run");
        // A second exec is fine: many at once.
        let exec2 = store.lock("demo", LockKind::Shared).expect("and another");
        // An export is not.
        match store.lock("demo", LockKind::Exclusive) {
            Err(StoreError::Busy { box_id, wanted }) => {
                assert_eq!(box_id, "demo");
                assert_eq!(wanted, "exclusive");
            }
            other => panic!("expected Busy, got {other:?}"),
        }

        drop(exec);
        drop(exec2);
        // With the execs gone it is available again.
        let _export = store.lock("demo", LockKind::Exclusive).expect("now free");
    }

    #[test]
    fn an_exec_cannot_start_while_an_export_holds_the_box() {
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        let _export = store.lock("demo", LockKind::Exclusive).expect("export");
        assert!(matches!(
            store.lock("demo", LockKind::Shared),
            Err(StoreError::Busy { .. })
        ));
    }

    #[test]
    fn a_lock_is_released_when_its_holder_drops() {
        // The property that matters is really the one below it: a worker killed
        // mid-exec must not leave a box locked forever, and `flock` gives that
        // for free because the kernel releases it with the process.
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        {
            let _held = store.lock("demo", LockKind::Exclusive).expect("held");
            assert!(store.lock("demo", LockKind::Exclusive).is_err());
        }
        assert!(store.lock("demo", LockKind::Exclusive).is_ok());
    }

    #[test]
    fn no_temporary_file_survives_a_write() {
        let (_d, store) = store();
        commit(&store, "demo", "op1", &"a".repeat(64));
        let leftovers: Vec<_> = std::fs::read_dir(store.box_dir("demo"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
