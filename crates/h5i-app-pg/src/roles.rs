//! Database roles that stop code outside the engine from writing h5i-app tables.
//!
//! Run [`lockdown`] as an admin after `install_schema`. Superusers bypass it,
//! so the engine must not log in as one.

use crate::{DbError, Store, FRAMEWORK_TABLES};
use h5i_app_core::Kernel;

pub(crate) fn ident(name: &str) -> Result<&str, DbError> {
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(DbError::Decode(format!("role or table name {name:?} must be [A-Za-z0-9_]+")))
    }
}

/// The statements [`lockdown`] runs. Idempotent.
pub fn lockdown_sql<K: Kernel, S: Store<K>>(owner: &str, engine_role: &str) -> Result<Vec<String>, DbError> {
    let (owner, engine) = (ident(owner)?, ident(engine_role)?);
    let mut sql = vec![format!(
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{owner}') \
         THEN CREATE ROLE \"{owner}\" NOLOGIN; END IF; END $$"
    )];
    // Tables resolve through the admin connection's search path.
    sql.push(format!("DO $$ BEGIN EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', current_schema(), '{engine}'); END $$"));
    let tables = FRAMEWORK_TABLES.iter().copied().chain(S::tables());
    for t in tables {
        let t = ident(t)?;
        sql.push(format!("ALTER TABLE \"{t}\" OWNER TO \"{owner}\""));
        sql.push(format!("REVOKE ALL ON \"{t}\" FROM PUBLIC"));
        // Drop grants left over for any other role.
        sql.push(format!(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT DISTINCT grantee FROM information_schema.role_table_grants \
             WHERE table_schema = current_schema() AND table_name = '{t}' \
             AND grantee NOT IN ('{owner}', '{engine}', 'PUBLIC') LOOP \
             EXECUTE format('REVOKE ALL ON %I FROM %I', '{t}', r.grantee); END LOOP; END $$"
        ));
        sql.push(format!("GRANT SELECT, INSERT, UPDATE, DELETE ON \"{t}\" TO \"{engine}\""));
    }
    Ok(sql)
}

/// `owner` (NOLOGIN) owns the h5i-app tables; only `engine_role` may touch rows.
/// `admin_url` needs rights to create roles and change owners.
pub async fn lockdown<K: Kernel, S: Store<K>>(admin_url: &str, owner: &str, engine_role: &str) -> Result<(), DbError> {
    let stmts = lockdown_sql::<K, S>(owner, engine_role)?;
    let (admin, conn) = tokio_postgres::connect(admin_url, tokio_postgres::NoTls).await?;
    let conn = tokio::spawn(conn);
    for stmt in stmts {
        admin.batch_execute(&stmt).await?;
    }
    drop(admin);
    let _ = conn.await;
    Ok(())
}
