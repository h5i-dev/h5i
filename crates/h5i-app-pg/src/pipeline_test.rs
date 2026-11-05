//! Pipelined statements report the error that failed the transaction, not
//! the "current transaction is aborted" (25P02) of the statements after it,
//! whatever order their replies are seen in. Needs PostgreSQL at
//! H5I_APP_TEST_DATABASE_URL; skips otherwise.

use crate::table::{first_cause, pipelined};
use crate::{pool, DbError};
use tokio_postgres::error::SqlState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipelined_statements_report_the_cause() {
    let Some(url) = crate::test_env("H5I_APP_TEST_DATABASE_URL") else {
        return;
    };
    let p = pool(&url, 1).unwrap();
    let mut client = p.0.get().await.unwrap();
    for round in 0..50 {
        let tx = client.transaction().await.unwrap();
        // Cached first, so most rounds send the later statements before the
        // division is even parsed; some rounds the other way round.
        let ok = tx.prepare_cached("SELECT 1::bigint").await.unwrap();
        let fail = if round % 2 == 0 { tx.prepare_cached("SELECT 1::bigint / 0").await.unwrap() } else { tx.prepare("SELECT 1::bigint / 0").await.unwrap() };
        let mut order = [&fail, &ok, &ok, &ok];
        order.rotate_left(round % 3);
        let futs = order.iter().map(|st| {
            let tx = &tx;
            async move { Ok::<_, DbError>(tx.query_one(*st, &[]).await?.get::<_, i64>(0)) }
        });
        let e = pipelined(futs).await.unwrap_err();
        // Statements before the division succeed; it fails the transaction
        // (22012) and the ones after it get 25P02, but the cause is returned.
        assert_eq!(e.code(), Some(&SqlState::DIVISION_BY_ZERO), "round {round}: {e}");
        drop(tx);
    }
    // The selection itself: the cause wins wherever it is.
    let tx = client.transaction().await.unwrap();
    let cause: DbError = tx.query_one("SELECT 1::bigint / 0", &[]).await.unwrap_err().into();
    let refused: DbError = tx.query_one("SELECT 1::bigint", &[]).await.unwrap_err().into();
    assert_eq!(refused.code(), Some(&SqlState::IN_FAILED_SQL_TRANSACTION));
    drop(tx);
    let r: Result<Vec<u8>, DbError> = first_cause(vec![Ok(1), Err(refused), Err(cause)]);
    assert_eq!(r.unwrap_err().code(), Some(&SqlState::DIVISION_BY_ZERO));
    let r: Result<Vec<u8>, DbError> = first_cause(vec![Ok(1), Ok(2)]);
    assert_eq!(r.unwrap(), vec![1, 2]);
}
