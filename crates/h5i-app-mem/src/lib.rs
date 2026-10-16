//! Single-owner, tenant-serialized write-through executor.
//!
//! All writers must use this instance. Reads use a full tenant snapshot;
//! writes retain PostgreSQL's transaction/retry/idempotency implementation.
//! Acknowledgement follows commit; the cache reloads lazily. Invalidation precedes
//! any write await, so cancellation and ambiguous commits cannot leave stale
//! state readable. See docs/app/MEMORY.md for deployment restrictions.
use h5i_app_core::{Executor, Kernel, ReplyCodec, TenantId, Timestamp};
use h5i_app_pg::{DbError, Engine, Store};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

struct Cached<S> {
    snapshot: Option<S>,
    // A cancelled/failed commit may finish after a subsequent snapshot starts.
    // Keep using PG until this executor is recreated after draining the DB.
    cacheable: bool,
}
type Slot<K> = Arc<RwLock<Cached<<K as Kernel>::Snapshot>>>;

pub struct MemoryPgEngine<K: Kernel, S: Store<K>> {
    pg: Arc<Engine<K, S>>,
    tenants: Mutex<HashMap<TenantId, Slot<K>>>,
}
impl<K: Kernel, S: Store<K>> MemoryPgEngine<K, S> {
    pub fn new(pg: Arc<Engine<K, S>>) -> Self {
        Self {
            pg,
            tenants: Mutex::new(HashMap::new()),
        }
    }
    fn slot(&self, tenant: TenantId) -> Slot<K> {
        self.tenants
            .lock()
            .unwrap()
            .entry(tenant)
            .or_insert_with(|| {
                Arc::new(RwLock::new(Cached {
                    snapshot: None,
                    cacheable: true,
                }))
            })
            .clone()
    }
}
impl<K: Kernel, S: Store<K>> Executor<K> for MemoryPgEngine<K, S> {
    type Error = DbError;
    fn is_idempotency_conflict(e: &DbError) -> bool {
        matches!(e, DbError::IdempotencyConflict)
    }
    async fn execute(
        &self,
        actor: &K::Principal,
        cmd: &K::Command,
    ) -> Result<Result<K::Reply, K::Error>, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        if K::read_only(cmd) {
            loop {
                let snap = slot.read().await;
                if !snap.cacheable {
                    return self.pg.execute(actor, cmd).await;
                }
                if let Some(snap) = snap.snapshot.as_ref() {
                    let mut actor = actor.clone();
                    K::stamp(&mut actor, Timestamp::now());
                    return Ok(K::transition(&actor, snap, cmd).map(|(_, reply)| reply));
                }
                drop(snap);
                let mut snap = slot.write().await;
                if snap.cacheable && snap.snapshot.is_none() {
                    snap.snapshot = Some(self.pg.snapshot(tenant).await?);
                }
            }
        }
        let mut snap = slot.write().await;
        snap.snapshot = None;
        let was_cacheable = snap.cacheable;
        snap.cacheable = false;
        let result = self.pg.execute(actor, cmd).await?;
        snap.cacheable = was_cacheable;
        // Reload lazily on the next read. The lock orders reads after commits;
        // keeping None also handles cancellation immediately after COMMIT.
        Ok(result)
    }
    async fn execute_idempotent<C: ReplyCodec<K>>(
        &self,
        actor: &K::Principal,
        key: &str,
        cmd: &K::Command,
    ) -> Result<Result<K::Reply, K::Error>, DbError> {
        let slot = self.slot(K::tenant(actor));
        let mut snap = slot.write().await;
        snap.snapshot = None;
        let was_cacheable = snap.cacheable;
        snap.cacheable = false;
        let result = Executor::execute_idempotent::<C>(&*self.pg, actor, key, cmd).await?;
        snap.cacheable = was_cacheable;
        Ok(result)
    }
    async fn snapshot_for(
        &self,
        actor: &K::Principal,
        _: &K::Command,
    ) -> Result<K::Snapshot, DbError> {
        let tenant = K::tenant(actor);
        let slot = self.slot(tenant);
        let mut snap = slot.write().await;
        if snap.cacheable && snap.snapshot.is_none() {
            snap.snapshot = Some(self.pg.snapshot(tenant).await?);
        }
        if !snap.cacheable {
            return self.pg.snapshot(tenant).await;
        }
        Ok(snap.snapshot.as_ref().unwrap().clone())
    }
}
