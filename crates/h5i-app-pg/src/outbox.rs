//! Effects that leave the database: webhooks, emails, payment calls.
//!
//! [`enqueue`] writes an effect in the request's transaction, so it exists iff
//! the change commits. A [`Dispatcher`] delivers it at least once, with a
//! stable key for dedup. Destinations are ids resolved only through the
//! operator's registry, so user input never picks the host.

use crate::{DbError, Pool, Tx};
use h5i_app_core::TenantId;
use std::collections::HashMap;
use std::future::Future;

pub(crate) const OUTBOX_DDL: &str = "CREATE TABLE IF NOT EXISTS h5i_outbox (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant_id BIGINT NOT NULL,
  dest BIGINT NOT NULL,
  payload BYTEA NOT NULL,
  attempts INT NOT NULL DEFAULT 0,
  lease_until TIMESTAMPTZ,
  delivered_at TIMESTAMPTZ,
  dead BOOLEAN NOT NULL DEFAULT false,
  last_error TEXT
)";

/// Queue an effect in the current transaction.
pub async fn enqueue(tx: &Tx<'_>, tenant: TenantId, dest: u64, payload: &[u8]) -> Result<(), DbError> {
    let tid = crate::table::tenant_param(tenant)?;
    let dest = i64::try_from(dest).map_err(|_| DbError::Decode(format!("destination {dest} out of range")))?;
    tx.0.execute("INSERT INTO h5i_outbox (tenant_id, dest, payload) VALUES ($1, $2, $3)", &[&tid, &dest, &payload])
        .await?;
    Ok(())
}

/// One effect to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// Stable across retries; receivers use it to drop duplicates.
    pub key: String,
    pub tenant: TenantId,
    pub dest: u64,
    pub payload: Vec<u8>,
    /// 1 on the first try.
    pub attempt: u32,
}

/// Sends a delivery to an endpoint from the registry.
pub trait Deliver<E>: Send + Sync {
    fn deliver(&self, endpoint: &E, d: &Delivery) -> impl Future<Output = Result<(), String>> + Send;
}

#[derive(Clone, Debug)]
pub struct DispatchConfig {
    pub batch: i64,
    /// How long a claimed effect is hidden from other dispatchers.
    pub lease_secs: f64,
    /// After this many failed attempts an effect is marked dead.
    pub max_attempts: i32,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig { batch: 32, lease_secs: 30.0, max_attempts: 10 }
    }
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub delivered: usize,
    pub failed: usize,
    pub dead: usize,
}

pub struct Dispatcher<E, D: Deliver<E>> {
    pool: Pool,
    registry: HashMap<u64, E>,
    deliver: D,
    config: DispatchConfig,
}

impl<E: Send + Sync, D: Deliver<E>> Dispatcher<E, D> {
    /// `registry` is the only way a destination id becomes an endpoint.
    pub fn new(pool: Pool, registry: HashMap<u64, E>, deliver: D, config: DispatchConfig) -> Self {
        Dispatcher { pool, registry, deliver, config }
    }

    /// Claim a batch of pending effects and try to deliver each once.
    pub async fn run_once(&self) -> Result<Pass, DbError> {
        let client = self.pool.0.get().await.map_err(|e| DbError::Pool(e.to_string()))?;
        let rows = client
            .query(
                "UPDATE h5i_outbox SET lease_until = now() + make_interval(secs => $1), attempts = attempts + 1
                 WHERE id IN (
                   SELECT id FROM h5i_outbox
                   WHERE delivered_at IS NULL AND NOT dead AND (lease_until IS NULL OR lease_until < now())
                   ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED)
                 RETURNING id, tenant_id, dest, payload, attempts",
                &[&self.config.lease_secs, &self.config.batch],
            )
            .await?;
        // RETURNING has no order; send a claimed batch in row-id order.
        let mut rows = rows;
        rows.sort_by_key(|r| r.get::<_, i64>(0));
        let mut pass = Pass::default();
        for row in rows {
            let (id, tenant, dest, payload, attempts): (i64, i64, i64, Vec<u8>, i32) =
                (row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?, row.try_get(4)?);
            let Some(endpoint) = u64::try_from(dest).ok().and_then(|d| self.registry.get(&d)) else {
                client
                    .execute("UPDATE h5i_outbox SET dead = true, last_error = 'unknown destination' WHERE id = $1", &[&id])
                    .await?;
                pass.dead += 1;
                continue;
            };
            let d = Delivery {
                key: format!("{tenant}-{id}"),
                tenant: TenantId(tenant as u64),
                dest: dest as u64,
                payload,
                attempt: attempts as u32,
            };
            match self.deliver.deliver(endpoint, &d).await {
                Ok(()) => {
                    client.execute("UPDATE h5i_outbox SET delivered_at = now() WHERE id = $1", &[&id]).await?;
                    pass.delivered += 1;
                }
                Err(e) if attempts >= self.config.max_attempts => {
                    client.execute("UPDATE h5i_outbox SET dead = true, last_error = $2 WHERE id = $1", &[&id, &e]).await?;
                    pass.dead += 1;
                }
                Err(e) => {
                    // Retry after a short delay, not after the full lease.
                    client
                        .execute(
                            "UPDATE h5i_outbox SET last_error = $2, lease_until = now() + interval '1 second' WHERE id = $1",
                            &[&id, &e],
                        )
                        .await?;
                    pass.failed += 1;
                }
            }
        }
        Ok(pass)
    }
}
