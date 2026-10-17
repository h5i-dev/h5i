//! Write-through warm-snapshot executor over the PostgreSQL engine.
//!
//! Reads are served from a per-tenant snapshot kept warm in memory; writes go
//! through to PostgreSQL, which owns transactions, retries and idempotency.
//!
//! A write holds no lock across its commit, so reads of the same tenant proceed
//! concurrently against the current snapshot and are never blocked by a commit.
//! The cache is refreshed only after the commit returns: the stale snapshot is
//! dropped and the next read reloads the committed state. If a commit is
//! cancelled or fails its outcome is unknown, so a drop guard poisons the
//! tenant — reads then fall through to PostgreSQL until the executor is
//! recreated (see docs/app/MEMORY.md). Poisoning is the only state a cancelled
//! future can leave, and it is always safe.
use h5i_app_core::{Executed, Executor, Kernel, ReplyCodec, TenantId, Timestamp};
use h5i_app_pg::{DbError, Engine, Store};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;

/// Per-tenant warm snapshot. `poisoned` is separate from the snapshot lock so
/// the write guard can set it synchronously from `Drop`, where it cannot await
/// the async lock.
struct Slot<K: Kernel> {
    poisoned: AtomicBool,
    snapshot: RwLock<Option<K::Snapshot>>,
}
type SlotRef<K> = Arc<Slot<K>>;

/// Poisons a tenant's cache if dropped before `disarm`. A write arms one before
/// its commit await; a cancelled or failed commit leaves an unknown database
/// state, and poisoning forces reads back to PostgreSQL.
struct CommitGuard<K: Kernel>(Option<SlotRef<K>>);
impl<K: Kernel> CommitGuard<K> {
    fn arm(slot: SlotRef<K>) -> Self {
        Self(Some(slot))
    }
    fn disarm(mut self) -> SlotRef<K> {
        self.0.take().expect("armed")
    }
}
impl<K: Kernel> Drop for CommitGuard<K> {
    fn drop(&mut self) {
        if let Some(slot) = &self.0 {
            slot.poisoned.store(true, Ordering::SeqCst);
        }
    }
}

pub struct MemoryPgEngine<K: Kernel, S: Store<K>> {
    pg: Arc<Engine<K, S>>,
    tenants: Mutex<HashMap<TenantId, SlotRef<K>>>,
}

impl<K: Kernel, S: Store<K>> MemoryPgEngine<K, S> {
    pub fn new(pg: Arc<Engine<K, S>>) -> Self {
        Self {
            pg,
            tenants: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, tenant: TenantId) -> SlotRef<K> {
        self.tenants
            .lock()
            .unwrap()
            .entry(tenant)
            .or_insert_with(|| {
                Arc::new(Slot {
                    poisoned: AtomicBool::new(false),
                    snapshot: RwLock::new(None),
                })
            })
            .clone()
    }

    /// The tenant's warm snapshot, loaded from PostgreSQL once if absent. Takes
    /// the write lock only to populate; never held across a write's commit.
    async fn warm_snapshot(
        &self,
        slot: &Slot<K>,
        tenant: TenantId,
    ) -> Result<K::Snapshot, DbError> {
        if let Some(snapshot) = slot.snapshot.read().await.as_ref() {
            return Ok(snapshot.clone());
        }
        let mut cell = slot.snapshot.write().await;
        if cell.is_none() {
            *cell = Some(self.pg.snapshot(tenant).await?);
        }
        Ok(cell.as_ref().unwrap().clone())
    }

    /// Serve a read from the warm snapshot, or from PostgreSQL if poisoned.
    async fn read(
        &self,
        slot: &Slot<K>,
        tenant: TenantId,
        actor: &K::Principal,
        cmd: &K::Command,
    ) -> Executed<K, DbError> {
        if slot.poisoned.load(Ordering::SeqCst) {
            return self.pg.execute(actor, cmd).await;
        }
        let snapshot = self.warm_snapshot(slot, tenant).await?;
        let mut actor = actor.clone();
        K::stamp(&mut actor, Timestamp::now());
        Ok(K::transition(&actor, &snapshot, cmd).map(|(_, reply)| reply))
    }

    /// Drop the warm snapshot so the next read reloads the just-committed state.
    async fn invalidate(slot: &Slot<K>) {
        *slot.snapshot.write().await = None;
    }
}

impl<K: Kernel, S: Store<K>> Executor<K> for MemoryPgEngine<K, S> {
    type Error = DbError;

    fn is_idempotency_conflict(e: &DbError) -> bool {
        matches!(e, DbError::IdempotencyConflict)
    }

    async fn execute(&self, actor: &K::Principal, cmd: &K::Command) -> Executed<K, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        if K::read_only(cmd) {
            return self.read(&slot, tenant, actor, cmd).await;
        }
        let guard = CommitGuard::arm(slot);
        let committed = self.pg.execute_committed(actor, cmd).await?;
        Self::invalidate(&guard.disarm()).await;
        Ok(committed.result)
    }

    async fn execute_idempotent<C: ReplyCodec<K>>(
        &self,
        actor: &K::Principal,
        key: &str,
        cmd: &K::Command,
    ) -> Executed<K, DbError> {
        let slot = self.slot(K::tenant(actor));
        let guard = CommitGuard::arm(slot);
        let committed = self
            .pg
            .execute_idempotent_committed::<C>(actor, key, cmd)
            .await?;
        Self::invalidate(&guard.disarm()).await;
        Ok(committed.result)
    }

    async fn snapshot_for(
        &self,
        actor: &K::Principal,
        _: &K::Command,
    ) -> Result<K::Snapshot, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        if slot.poisoned.load(Ordering::SeqCst) {
            return self.pg.snapshot(tenant).await;
        }
        self.warm_snapshot(&slot, tenant).await
    }
}
