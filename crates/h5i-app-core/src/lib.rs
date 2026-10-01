//! The core types of h5i-app: the [`Kernel`] trait and an in-memory reference engine.
//!
//! A [`Kernel`] is a pure `transition` (what a command does) and `apply` (what
//! its writes mean), written in the Rust subset Aeneas translates to Lean.
//! [`MemoryEngine`] is the reference any real engine must match on commits.
//! The kernel has no clock; engines pass the time through [`Kernel::stamp`].
//!
//! # Example
//!
//! ```ignore
//! impl Kernel for Calc {
//!     type Principal = Principal;
//!     type Snapshot = Snapshot;
//!     type Command = Command;
//!     type WriteSet = Option<Memory>;
//!     type Reply = Reply;
//!     type Error = Error;
//!
//!     fn tenant(actor: &Principal) -> TenantId {
//!         TenantId(actor.org)
//!     }
//!
//!     fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Option<Memory>, Reply), Error> {
//!         kernel::transition(actor, snap, cmd)
//!     }
//!
//!     fn apply(snap: &Snapshot, w: &Option<Memory>) -> Snapshot {
//!         kernel::apply(snap, w)
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// An organization. Every row and every SQL statement is scoped by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(pub u64);

/// Microseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const EPOCH: Timestamp = Timestamp(0);

    /// The system clock of this process.
    pub fn now() -> Self {
        Timestamp(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0))
    }

    /// Saturates at `u64::MAX` microseconds.
    pub fn from_secs(secs: u64) -> Self {
        Timestamp(secs.saturating_mul(1_000_000))
    }

    pub fn micros(self) -> u64 {
        self.0
    }

    /// Whole seconds, rounded down.
    pub fn secs(self) -> u64 {
        self.0 / 1_000_000
    }
}

/// Implement on a marker type in the server crate, delegating to the kernel crate.
pub trait Kernel: Send + Sync + 'static {
    type Principal: Send + Sync + Clone;
    /// One tenant's state.
    type Snapshot: Send + Sync + Clone + Default;
    type Command: Send + Sync;
    type WriteSet: Send + Sync;
    type Reply: Send + Sync;
    /// A refusal. Nothing is committed.
    type Error: Send + Sync;

    /// The only tenant this principal may read or write.
    fn tenant(actor: &Self::Principal) -> TenantId;

    /// Pure and deterministic. This is what the Lean proofs are about.
    fn transition(
        actor: &Self::Principal,
        snap: &Self::Snapshot,
        cmd: &Self::Command,
    ) -> Result<(Self::WriteSet, Self::Reply), Self::Error>;

    /// Meaning of a write set. After a commit, loading must return `apply(snap, ws)`.
    fn apply(snap: &Self::Snapshot, ws: &Self::WriteSet) -> Self::Snapshot;

    /// Put the attempt's time into the principal, right before `transition`.
    /// The default ignores it.
    ///
    /// ```ignore
    /// fn stamp(actor: &mut Principal, now: Timestamp) {
    ///     actor.now = now.secs();
    /// }
    /// ```
    fn stamp(actor: &mut Self::Principal, now: Timestamp) {
        let _ = (actor, now);
    }
}

/// Runs commands one at a time in memory. A real engine must behave like this
/// on its committed requests, in some serial order.
pub struct MemoryEngine<K: Kernel> {
    tenants: Mutex<HashMap<TenantId, Tenant<K>>>,
}

struct Tenant<K: Kernel> {
    snap: K::Snapshot,
    /// Time of the latest commit through `execute_at`.
    last: Timestamp,
}

impl<K: Kernel> Default for Tenant<K> {
    fn default() -> Self {
        Tenant { snap: K::Snapshot::default(), last: Timestamp::EPOCH }
    }
}

impl<K: Kernel> Default for MemoryEngine<K> {
    fn default() -> Self {
        MemoryEngine { tenants: Mutex::new(HashMap::new()) }
    }
}

impl<K: Kernel> MemoryEngine<K> {
    /// Run `cmd` with `actor` as given: no clock, no stamp.
    pub fn execute(&self, actor: &K::Principal, cmd: &K::Command) -> Result<K::Reply, K::Error> {
        let tenant = K::tenant(actor);
        let mut tenants = self.tenants.lock().unwrap();
        let t = tenants.entry(tenant).or_default();
        let (ws, reply) = K::transition(actor, &t.snap, cmd)?;
        t.snap = K::apply(&t.snap, &ws);
        Ok(reply)
    }

    /// Run `cmd` at `now` via [`Kernel::stamp`]. Like `h5i-app-pg`'s `monotonic`, a
    /// `now` before the tenant's last commit is raised to it.
    pub fn execute_at(&self, actor: &K::Principal, now: Timestamp, cmd: &K::Command) -> Result<K::Reply, K::Error> {
        let tenant = K::tenant(actor);
        let mut tenants = self.tenants.lock().unwrap();
        let t = tenants.entry(tenant).or_default();
        let now = now.max(t.last);
        let mut actor = actor.clone();
        K::stamp(&mut actor, now);
        let (ws, reply) = K::transition(&actor, &t.snap, cmd)?;
        t.snap = K::apply(&t.snap, &ws);
        t.last = now;
        Ok(reply)
    }

    /// Start `tenant` from `snap`, e.g. what a database already holds.
    pub fn with_snapshot(self, tenant: TenantId, snap: K::Snapshot) -> Self {
        self.tenants.lock().unwrap().insert(tenant, Tenant { snap, last: Timestamp::EPOCH });
        self
    }

    pub fn snapshot(&self, tenant: TenantId) -> K::Snapshot {
        self.tenants.lock().unwrap().get(&tenant).map(|t| t.snap.clone()).unwrap_or_default()
    }

    /// Time of the tenant's latest commit through `execute_at`.
    pub fn last_time(&self, tenant: TenantId) -> Timestamp {
        self.tenants.lock().unwrap().get(&tenant).map(|t| t.last).unwrap_or_default()
    }
}

/// Sort rows by key, for comparing snapshots.
pub fn sorted_by_key<T: Clone, O: Ord, F: Fn(&T) -> O>(rows: &[T], key: F) -> Vec<T> {
    let mut v = rows.to_vec();
    v.sort_by_key(|r| key(r));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the time of every command.
    struct Log;

    impl Kernel for Log {
        type Principal = u64;
        type Snapshot = Vec<u64>;
        type Command = ();
        type WriteSet = u64;
        type Reply = u64;
        type Error = ();

        fn tenant(_: &u64) -> TenantId {
            TenantId(1)
        }

        fn transition(now: &u64, _: &Vec<u64>, _: &()) -> Result<(u64, u64), ()> {
            Ok((*now, *now))
        }

        fn apply(snap: &Vec<u64>, t: &u64) -> Vec<u64> {
            let mut s = snap.clone();
            s.push(*t);
            s
        }

        fn stamp(actor: &mut u64, now: Timestamp) {
            *actor = now.secs();
        }
    }

    #[test]
    fn execute_at_never_goes_back() {
        let m = MemoryEngine::<Log>::default();
        assert_eq!(m.execute_at(&0, Timestamp::from_secs(5), &()), Ok(5));
        assert_eq!(m.execute_at(&0, Timestamp::from_secs(3), &()), Ok(5));
        assert_eq!(m.execute_at(&0, Timestamp::from_secs(9), &()), Ok(9));
        // `execute` does not stamp.
        assert_eq!(m.execute(&2, &()), Ok(2));
        assert_eq!(m.snapshot(TenantId(1)), vec![5, 5, 9, 2]);
        assert_eq!(m.last_time(TenantId(1)), Timestamp::from_secs(9));
    }
}
