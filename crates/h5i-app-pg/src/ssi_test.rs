//! SERIALIZABLE's conflict pool overflowing (53200) is retryable: many
//! open serializable transactions that each read a whole table and then
//! write one row record a read/write conflict with every other one, more
//! than the pool (about 5 × max_connections) holds. Needs
//! H5I_APP_TEST_DATABASE_URL; skips otherwise.

use crate::DbError;
use tokio_postgres::error::SqlState;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflict_pool_overflow_is_retryable() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
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
    // Real memory exhaustion is not retried.
    assert!(!DbError::Pool("x".into()).is_retryable());
}
