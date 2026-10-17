//! Write-through warm-snapshot executor over the PostgreSQL engine.
//!
//! Reads are served from a per-tenant snapshot kept warm in memory; writes go
//! through to PostgreSQL, which owns transactions, retries and idempotency.
//!
//! A write never holds the snapshot lock across its commit, so reads of the
//! same tenant proceed against the current snapshot and are not blocked by a
//! commit in flight. Writes to one tenant are serialized by a per-tenant gate
//! (reads do not take it), so the committed write set is applied to the warm
//! snapshot in commit order — reads after a write stay warm, with no reload.
//!
//! If a commit is cancelled or fails its outcome is unknown, so a drop guard
//! poisons the tenant: reads then fall through to PostgreSQL and a later commit
//! does not revive the cache, until the executor is recreated (see
//! docs/app/MEMORY.md). Poisoning is the only state a cancelled future can
//! leave, and it is always safe.
use h5i_app_core::{Executor, Kernel, ReplyCodec, TenantId, Timestamp};
use h5i_app_pg::{Committed, DbError, Engine, Store};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Per-tenant warm snapshot. `poisoned` is separate from the locks so the write
/// guard can set it synchronously from `Drop`, where it cannot await them.
struct Slot<K: Kernel> {
    poisoned: AtomicBool,
    /// Serializes this tenant's writes so their write sets apply in commit
    /// order. Reads never take it, so a commit in flight does not block them.
    write_gate: AsyncMutex<()>,
    snapshot: RwLock<Option<K::Snapshot>>,
}
type SlotRef<K> = Arc<Slot<K>>;

/// Poisons a tenant's cache if dropped before `disarm`. A write arms one around
/// its commit; a cancelled or failed commit leaves an unknown database state,
/// and poisoning forces reads back to PostgreSQL.
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
                    write_gate: AsyncMutex::new(()),
                    snapshot: RwLock::new(None),
                })
            })
            .clone()
    }

    /// The tenant's warm snapshot, loaded from PostgreSQL once if absent. Takes
    /// the write lock only to populate; never held across a write's commit.
    async fn warm_snapshot(&self, slot: &Slot<K>, tenant: TenantId) -> Result<K::Snapshot, DbError> {
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
    ) -> Result<Result<K::Reply, K::Error>, DbError> {
        if slot.poisoned.load(Ordering::SeqCst) {
            return self.pg.execute(actor, cmd).await;
        }
        let snapshot = self.warm_snapshot(slot, tenant).await?;
        let mut actor = actor.clone();
        K::stamp(&mut actor, Timestamp::now());
        Ok(K::transition(&actor, &snapshot, cmd).map(|(_, reply)| reply))
    }

    /// Fold a committed write into the warm snapshot, keeping reads warm. Runs
    /// under a brief snapshot write lock after the commit, not across it. A
    /// replay drops the snapshot (it must not apply twice); a write with no
    /// warm snapshot yet leaves it empty for the next read to load fresh.
    async fn apply(slot: &Slot<K>, committed: &Committed<K>) {
        if slot.poisoned.load(Ordering::SeqCst) {
            return;
        }
        let mut cell = slot.snapshot.write().await;
        if committed.replayed {
            *cell = None;
        } else if let (Some(snapshot), Some(writes)) = (cell.as_ref(), committed.writes.as_ref()) {
            *cell = Some(K::apply(snapshot, writes));
        }
    }
}

impl<K: Kernel, S: Store<K>> Executor<K> for MemoryPgEngine<K, S> {
    type Error = DbError;

    fn is_idempotency_conflict(e: &DbError) -> bool {
        matches!(e, DbError::IdempotencyConflict)
    }

    async fn execute(&self, actor: &K::Principal, cmd: &K::Command) -> Result<Result<K::Reply, K::Error>, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        if K::read_only(cmd) {
            return self.read(&slot, tenant, actor, cmd).await;
        }
        let _gate = slot.write_gate.lock().await;
        let guard = CommitGuard::arm(slot.clone());
        let committed = self.pg.execute_committed(actor, cmd).await?;
        let slot = guard.disarm();
        Self::apply(&slot, &committed).await;
        Ok(committed.result)
    }

    async fn execute_idempotent<C: ReplyCodec<K>>(&self, actor: &K::Principal, key: &str, cmd: &K::Command) -> Result<Result<K::Reply, K::Error>, DbError> {
        let slot = self.slot(K::tenant(actor));
        let _gate = slot.write_gate.lock().await;
        let guard = CommitGuard::arm(slot.clone());
        let committed = self.pg.execute_idempotent_committed::<C>(actor, key, cmd).await?;
        let slot = guard.disarm();
        Self::apply(&slot, &committed).await;
        Ok(committed.result)
    }

    async fn snapshot_for(&self, actor: &K::Principal, _: &K::Command) -> Result<K::Snapshot, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        if slot.poisoned.load(Ordering::SeqCst) {
            return self.pg.snapshot(tenant).await;
        }
        self.warm_snapshot(&slot, tenant).await
    }
}
