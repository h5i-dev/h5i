//! SERIALIZABLE running out of its shared structures (53200) is retryable.
//!
//! The classification is checked on the reports PostgreSQL 17 sends
//! (`predicate.c`), always. Overflowing a real conflict pool needs a server
//! that can hold enough open transactions: the pool has
//! `5 × 10 × MaxBackends` entries (MaxBackends = max_connections +
//! autovacuum_max_workers + 1 + max_worker_processes + max_wal_senders,
//! about 6,100 at the defaults), while `n` transactions record at most
//! `n × (n − 1)` conflicts, so with the default `max_connections = 100`
//! (h5i's CI) no number of connections overflows it. That test is
//! therefore `#[ignore]`d: run it with `--ignored` against a server with
//! `max_connections ≥ 200` (h5i-pay's `scripts/dev-db.sh` starts one).

use crate::{ssi_exhausted_report, DbError};
use tokio_postgres::error::SqlState;

#[test]
fn ssi_exhaustion_reports_are_retryable_and_real_oom_is_not() {
    // The RWConflictPool report (predicate.c, SetRWConflict).
    assert!(ssi_exhausted_report(
        &SqlState::OUT_OF_MEMORY,
        "not enough elements in RWConflictPool to record a read/write conflict",
        Some("You might need to run fewer transactions at a time or increase \"max_connections\"."),
    ));
    // The predicate lock table report (predicate.c, CreatePredicateLock).
    assert!(ssi_exhausted_report(
        &SqlState::OUT_OF_MEMORY,
        "out of shared memory",
        Some("You might need to increase \"max_pred_locks_per_transaction\"."),
    ));
    // Other 53200 reports are real memory exhaustion: not retried.
    assert!(!ssi_exhausted_report(&SqlState::OUT_OF_MEMORY, "out of memory", Some("Failed on request of size 8192.")));
    assert!(!ssi_exhausted_report(
        &SqlState::OUT_OF_MEMORY,
        "out of shared memory",
        Some("You might need to increase \"max_locks_per_transaction\"."),
    ));
    // The same text under another code is not this condition.
    assert!(!ssi_exhausted_report(&SqlState::T_R_SERIALIZATION_FAILURE, "not enough elements in RWConflictPool", None));
    assert!(!DbError::Pool("x".into()).is_retryable());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs max_connections >= 200 to overflow the conflict pool; see the module docs"]
async fn conflict_pool_overflow_is_retryable() {
    let Some(url) = crate::test_env("H5I_APP_TEST_DATABASE_URL") else {
        return;
    };
    let connect = || async {
        let (c, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(conn);
        c
    };
    let admin = connect().await;
    let max: i64 = admin.query_one("SELECT current_setting('max_connections')::bigint", &[]).await.unwrap().get(0);
    // Enough transactions that their conflicts exceed the pool, within the
    // connections the server allows (leaving some for other sessions).
    let n = (max as usize).saturating_sub(25).max(10);
    admin.batch_execute("DROP TABLE IF EXISTS h5i_ssi_test; CREATE TABLE h5i_ssi_test (id INT PRIMARY KEY, v INT NOT NULL)").await.unwrap();
    admin.execute("INSERT INTO h5i_ssi_test SELECT g, 0 FROM generate_series(0, $1::int - 1) g", &[&(n as i32)]).await.unwrap();
    let mut clients = Vec::new();
    for _ in 0..n {
        let c = connect().await;
        c.batch_execute("BEGIN ISOLATION LEVEL SERIALIZABLE").await.unwrap();
        c.query_one("SELECT sum(v) FROM h5i_ssi_test", &[]).await.unwrap();
        clients.push(c);
    }
    let (mut exhausted, mut serialization) = (0, 0);
    for (i, c) in clients.iter().enumerate() {
        match c.execute("UPDATE h5i_ssi_test SET v = v + 1 WHERE id = $1", &[&(i as i32)]).await {
            Ok(_) => {}
            Err(e) => {
                let code = e.code().cloned();
                let err = DbError::Postgres(e);
                if code == Some(SqlState::OUT_OF_MEMORY) {
                    assert!(err.is_retryable(), "{err:?}");
                    exhausted += 1;
                } else {
                    assert_eq!(code, Some(SqlState::T_R_SERIALIZATION_FAILURE), "{err:?}");
                    serialization += 1;
                }
            }
        }
    }
    for c in &clients {
        let _ = c.batch_execute("ROLLBACK").await;
    }
    drop(clients);
    admin.batch_execute("DROP TABLE h5i_ssi_test").await.unwrap();
    assert!(exhausted > 0, "{n} transactions did not overflow the conflict pool (max_connections {max}; {serialization} serialization failures)");
}
