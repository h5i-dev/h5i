//! Schema and data migrations, checked before they commit.
//!
//! Pending SQL runs in one transaction, then every tenant's snapshot goes
//! through the app's invariant checker; any failure rolls it all back. Sound
//! only if the checker is proven exact (e.g. the docs example's `check_inv`).

use crate::{DbError, Engine, Store, Tx, SCHEMA_LOCK};
use h5i_app_core::{Kernel, TenantId};
use tokio_postgres::IsolationLevel;

/// One step. `id` must be unique and never reused; steps run in list order.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub id: &'static str,
    pub sql: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// Steps applied by this call, in order.
    pub applied: Vec<&'static str>,
    /// Tenants checked after applying them.
    pub tenants_checked: usize,
}

const MIGRATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS h5i_migrations (
  id TEXT PRIMARY KEY,
  applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
)";

impl<K: Kernel, S: Store<K>> Engine<K, S> {
    /// Apply pending migrations, then check every tenant. Rolls back on failure.
    pub async fn migrate(&self, steps: &[Migration], check: fn(&K::Snapshot) -> bool) -> Result<Migrated, DbError> {
        let mut client = self.pool.0.get().await.map_err(|e| DbError::Pool(e.to_string()))?;
        let tx = client.build_transaction().isolation_level(IsolationLevel::Serializable).start().await?;
        tx.execute("SELECT pg_advisory_xact_lock($1, $2)", &[&SCHEMA_LOCK.0, &SCHEMA_LOCK.1]).await?;
        tx.batch_execute(MIGRATIONS_DDL).await?;
        let done: Vec<String> = tx.query("SELECT id FROM h5i_migrations", &[]).await?.iter().map(|r| r.get(0)).collect();
        let mut report = Migrated::default();
        for step in steps.iter().filter(|m| !done.iter().any(|d| d == m.id)) {
            tx.batch_execute(step.sql).await?;
            tx.execute("INSERT INTO h5i_migrations (id) VALUES ($1)", &[&step.id]).await?;
            report.applied.push(step.id);
        }
        if report.applied.is_empty() {
            tx.commit().await?;
            return Ok(report);
        }
        let union: Vec<String> = S::tables().iter().map(|t| format!("SELECT tenant_id FROM \"{t}\"")).collect();
        let tenants: Vec<i64> = if union.is_empty() {
            Vec::new()
        } else {
            let sql = format!("SELECT DISTINCT tenant_id FROM ({}) t ORDER BY tenant_id", union.join(" UNION "));
            tx.query(&sql, &[]).await?.iter().map(|r| r.get(0)).collect()
        };
        for t in &tenants {
            let tenant = TenantId(*t as u64);
            let snap = S::load(&Tx(&tx), tenant).await?;
            if !check(&snap) {
                tx.rollback().await?;
                return Err(DbError::InvariantViolated { tenant: tenant.0, migrations: report.applied.join(", ") });
            }
        }
        report.tenants_checked = tenants.len();
        tx.commit().await?;
        Ok(report)
    }
}
