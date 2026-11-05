//! The trusted engine contract (A6 in docs/design/design-app-trust.md),
//! bullet by bullet, against real PostgreSQL with injected faults: killed
//! backends, COMMITs lost before or after the server ran them, forced
//! serialization failures and deadlocks, cancelled requests, and many
//! concurrent clients.
//!
//! Needs H5I_APP_TEST_DATABASE_URL (the engine's database) and
//! H5I_APP_TEST_ADMIN_URL (a superuser on the same server, to terminate
//! backends and read every backend's locks); skips otherwise, and fails
//! instead with `H5I_APP_TEST_REQUIRE_DB` set (as in CI; `h5i_app_pg::test_env`). Each test uses its own
//! PostgreSQL schema and tenants. Build with `--features testing`.

mod common;

use common::*;
use h5i_app_pg::outbox::{Deliver, Delivery, DispatchConfig, Pass};
use h5i_app_pg::{Clock, DbError, EngineConfig, ManualClock, Timestamp};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_postgres::error::SqlState;

const T: Duration = Duration::from_secs(20);

/// The outside transaction of `deadlocks_are_retried`.
type Outside = tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>;
/// (mode, granted) of a backend's advisory locks.
type Locks = Vec<(String, bool)>;

fn add(cell: u64, delta: i64) -> Cmd {
    Cmd::Add { cell, delta }
}

/// Rows of every table a command can write, for `tenant`.
async fn rows(db: &Db, tenant: u64) -> Vec<i64> {
    let mut n = Vec::new();
    for table in ["cells", "h5i_idempotency", "h5i_clock", "h5i_outbox"] {
        n.push(db.count(table, tenant).await);
    }
    n
}

fn stat(e: &Pg) -> (u64, u64, u64, u64, u64) {
    let s = &e.stats;
    let l = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    (l(&s.attempts), l(&s.retries), l(&s.commits), l(&s.replays), l(&s.reads))
}

// Bullets 1 and 2: one attempt is one SERIALIZABLE transaction; each retry
// opens a new transaction, reloads, reads a new time and reruns `transition`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serialization_failures_retry_in_a_new_transaction() {
    let Some(db) = setup("h5i_fault_ssi").await else { return };
    let pg = db.engine(2, EngineConfig::default()).await;
    let t = tenant();
    let a = actor(t, 1);
    pg.execute(&a, &add(0, 5)).await.unwrap().unwrap();
    // The first two attempts lose a write-write race to an outside commit
    // made after their snapshot: PostgreSQL fails their write with 40001.
    let admin = Arc::new(connect(&db.admin_url).await);
    let (name, conflicts) = (db.name.clone(), Arc::new(AtomicU32::new(0)));
    let c = conflicts.clone();
    set_hook(t, at_first(Point::Write, 2, move |_| {
        let (admin, name, c) = (admin.clone(), name.clone(), c.clone());
        async move {
            admin.execute(&format!("UPDATE {name}.cells SET v = v + 100 WHERE tenant_id = $1 AND id = 0"), &[&(t as i64)]).await.unwrap();
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    let before = (attempts(t).len(), seen(t).len(), stat(&pg));
    let r = tokio::time::timeout(T, pg.execute(&a, &add(0, 1))).await.unwrap().unwrap().unwrap();
    clear_hook(t);
    assert_eq!(conflicts.load(Ordering::SeqCst), 2);
    assert_eq!(r.value, 5 + 200 + 1);
    let after = stat(&pg);
    assert_eq!(after.0 - before.2.0, 3, "attempts");
    assert_eq!(after.1 - before.2.1, 2, "retries");
    // Three transactions, all SERIALIZABLE read-write, each a new one.
    let txs = &attempts(t)[before.0..];
    assert_eq!(txs.len(), 3);
    for tx in txs {
        assert_eq!(tx.isolation, "serializable");
        assert!(!tx.read_only);
    }
    assert!(txs[0].vxid != txs[1].vxid && txs[1].vxid != txs[2].vxid && txs[0].vxid != txs[2].vxid, "{txs:?}");
    // `transition` ran three times, each on a reload that saw the outside
    // commit (impossible in the first snapshot), at a later time.
    let runs = &seen(t)[before.1..];
    assert_eq!(runs.iter().map(|s| s.snap.get(0)).collect::<Vec<_>>(), vec![5, 105, 205]);
    assert!(runs[0].now < runs[1].now && runs[1].now < runs[2].now, "{runs:?}");
    assert_eq!(r.now, runs[2].now);
    assert_eq!(db.cells(t).await.get(0), 206);
}

// Bullet 2: retries stop at `max_attempts` with `RetriesExhausted`, and none
// of the attempts left a write or a stored reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serialization_failures_exhaust_after_max_attempts() {
    let Some(db) = setup("h5i_fault_exhaust").await else { return };
    let pg = db.engine(2, config(3, true)).await;
    let t = tenant();
    let a = actor(t, 1);
    pg.execute(&a, &add(0, 5)).await.unwrap().unwrap();
    let admin = Arc::new(connect(&db.admin_url).await);
    let name = db.name.clone();
    set_hook(t, at_first(Point::Write, u32::MAX, move |_| {
        let (admin, name) = (admin.clone(), name.clone());
        async move {
            admin.execute(&format!("UPDATE {name}.cells SET v = v + 100 WHERE tenant_id = $1 AND id = 0"), &[&(t as i64)]).await.unwrap();
            Ok(())
        }
    }));
    let before = stat(&pg);
    let r = tokio::time::timeout(T, pg.execute_idempotent(&a, "k", &add(0, 1))).await.unwrap();
    clear_hook(t);
    assert!(matches!(r, Err(DbError::RetriesExhausted)), "{r:?}");
    assert_eq!(stat(&pg).0 - before.0, 3);
    assert_eq!(db.cells(t).await.get(0), 305, "only the outside writes");
    assert_eq!(db.count("h5i_idempotency", t).await, 0);
}

// Bullet 2 (deadlock): the attempt holds the tenant's advisory lock and
// waits for a row an outside transaction holds, which then waits for the
// advisory lock. PostgreSQL aborts the attempt (40P01, after the 1 s
// `deadlock_timeout`; the outside session waits 30 s before checking), the
// engine releases its lock, and the retry succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadlocks_are_retried() {
    let Some(db) = setup("h5i_fault_deadlock").await else { return };
    let pg = db.engine(2, EngineConfig::default()).await;
    let t = tenant();
    let a = actor(t, 1);
    pg.execute(&a, &add(0, 5)).await.unwrap().unwrap();
    let other = Arc::new(connect(&db.admin_url).await);
    let name = db.name.clone();
    let outside: Arc<Mutex<Option<Outside>>> = Arc::default();
    let o = outside.clone();
    set_hook(t, at_first(Point::Write, 1, move |pid| {
        let (other, name, o) = (other.clone(), name.clone(), o.clone());
        async move {
            other
                .batch_execute(&format!("BEGIN; SET LOCAL deadlock_timeout = '30s'; UPDATE {name}.cells SET v = v WHERE tenant_id = {t} AND id = 0"))
                .await
                .unwrap();
            let task = tokio::spawn(async move {
                // Once the attempt waits for the row, wait for its advisory lock.
                loop {
                    let n: i64 = other.query_one("SELECT count(*) FROM pg_locks WHERE pid = $1 AND NOT granted", &[&pid]).await?.get(0);
                    if n > 0 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                other.execute("SELECT pg_advisory_xact_lock($1)", &[&(t as i64)]).await?;
                other.batch_execute("ROLLBACK").await
            });
            *o.lock().unwrap() = Some(task);
            Ok(())
        }
    }));
    let before = stat(&pg);
    let r = tokio::time::timeout(T, pg.execute(&a, &add(0, 1))).await.unwrap().unwrap().unwrap();
    clear_hook(t);
    let task = outside.lock().unwrap().take().unwrap();
    // The outside transaction was not the victim: it got the advisory lock.
    task.await.unwrap().unwrap();
    assert_eq!(stat(&pg).1 - before.1, 1, "one retry, after the deadlock");
    assert_eq!(r.value, 6);
    assert_eq!(db.cells(t).await.get(0), 6);
    assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(2)).await, 0);
}

// Bullet 6, first half: a connection lost before COMMIT (here: its backend
// terminated at each point of an attempt) retries from BEGIN, and the
// command takes effect once; with or without a key. One exception, without
// a key: a connection lost right before COMMIT, noticed only by COMMIT, is
// `CommitUnknown` (it did not commit; the engine cannot know that).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_before_commit_retries_once() {
    let Some(db) = setup("h5i_fault_kill").await else { return };
    let pg = db.engine(2, EngineConfig::default()).await;
    for (i, point) in [Point::Load, Point::Write, Point::Written, Point::Enqueued].into_iter().enumerate() {
        for keyed in [false, true] {
            let t = tenant();
            let a = actor(t, 1);
            let admin = db.admin.clone();
            set_hook(t, at_first(point, 1, move |pid| {
                let admin = admin.clone();
                async move {
                    kill(&admin, pid).await;
                    Ok(())
                }
            }));
            let before = (stat(&pg), attempts(t).len());
            let cmd = add(0, 7);
            let key = format!("k{i}");
            let r = tokio::time::timeout(T, async {
                if keyed { pg.execute_idempotent(&a, &key, &cmd).await } else { pg.execute(&a, &cmd).await }
            })
            .await
            .unwrap();
            clear_hook(t);
            // Killed after the last statement before COMMIT: unless the client
            // saw the connection close first, COMMIT is what fails, and
            // without a key the engine cannot tell that it was never sent.
            if point == Point::Enqueued && !keyed && matches!(r, Err(DbError::CommitUnknown(_))) {
                assert_eq!(db.cells(t).await.get(0), 0, "not committed");
                eprintln!("killed before an unkeyed COMMIT: reported CommitUnknown");
                continue;
            }
            let r = r.unwrap_or_else(|e| panic!("{point:?} keyed={keyed}: {e}")).unwrap();
            assert_eq!(r.value, 7, "{point:?} keyed={keyed}");
            let s = stat(&pg);
            assert_eq!(s.1 - before.0.1, 1, "{point:?} keyed={keyed}: one retry");
            assert_eq!(s.3 - before.0.3, 0, "{point:?} keyed={keyed}: not a replay");
            let txs = &attempts(t)[before.1..];
            assert_eq!(txs.len(), 2);
            assert_ne!(txs[0].pid, txs[1].pid, "the retry runs on a new connection");
            assert_eq!(db.cells(t).await.get(0), 7, "{point:?} keyed={keyed}: applied once");
            assert_eq!(db.count("h5i_idempotency", t).await, i64::from(keyed));
        }
    }
    assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(2)).await, 0);
}

// Bullet 6, second half, with a key: whether the lost COMMIT ran or not, the
// retry runs the command once, and a repeat gets the same reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_commit_with_key_runs_once() {
    let Some(db) = setup("h5i_fault_lost_keyed").await else { return };
    let proxy = Proxy::start(&db.base_url).await;
    let pg = db.engine_at(&proxy.url(&db.url), 2, EngineConfig::default()).await;
    for fault in [Fault::DropBefore, Fault::DropAfter] {
        let t = tenant();
        let a = actor(t, 1);
        pg.execute(&a, &add(0, 10)).await.unwrap().unwrap();
        let before = stat(&pg);
        proxy.arm(fault);
        let r = tokio::time::timeout(T, pg.execute_idempotent(&a, "pay", &add(0, 1))).await.unwrap().unwrap().unwrap();
        assert_eq!(proxy.armed(), 0);
        let s = stat(&pg);
        assert_eq!(r.value, 11, "{fault:?}");
        assert_eq!(db.cells(t).await.get(0), 11, "{fault:?}: once");
        assert_eq!(s.1 - before.1, 1, "{fault:?}: one retry");
        match fault {
            // Not committed: the retry ran the command.
            Fault::DropBefore => {
                assert_eq!(proxy.outcomes().last(), Some(&Outcome::NotSent));
                assert_eq!(s.3 - before.3, 0);
            }
            // Committed: the retry found the stored reply.
            Fault::DropAfter => {
                assert_eq!(proxy.outcomes().last(), Some(&Outcome::Committed), "{:?}", proxy.outcomes());
                assert_eq!(s.3 - before.3, 1);
            }
        }
        let again = pg.execute_idempotent(&a, "pay", &add(0, 1)).await.unwrap().unwrap();
        assert_eq!(again, r, "{fault:?}: the stored reply, time included");
        assert_eq!(db.cells(t).await.get(0), 11);
    }
}

// Bullet 6, without a key: a lost COMMIT is `CommitUnknown`, not retried, and
// either outcome is possible. The caller must assume neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_commit_without_key_is_unknown() {
    let Some(db) = setup("h5i_fault_lost_unkeyed").await else { return };
    let proxy = Proxy::start(&db.base_url).await;
    let pg = db.engine_at(&proxy.url(&db.url), 2, EngineConfig::default()).await;
    for (fault, landed) in [(Fault::DropBefore, false), (Fault::DropAfter, true)] {
        let t = tenant();
        let a = actor(t, 1);
        pg.execute(&a, &add(0, 10)).await.unwrap().unwrap();
        let before = stat(&pg);
        proxy.arm(fault);
        let r = tokio::time::timeout(T, pg.execute(&a, &add(0, 1))).await.unwrap();
        assert!(matches!(r, Err(DbError::CommitUnknown(_))), "{fault:?}: {r:?}");
        assert_eq!(stat(&pg).0 - before.0, 1, "{fault:?}: not retried");
        assert_eq!(db.cells(t).await.get(0), if landed { 11 } else { 10 }, "{fault:?}");
    }
}

// Bullets 3 and 5, and "refusals are not stored": a repeated key replays the
// stored reply without writing, another command under the key is refused,
// keys are per (tenant, scope), and a refusal stores nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idempotency_keys_replay_and_conflict() {
    let Some(db) = setup("h5i_fault_idem").await else { return };
    let pg = db.engine(2, EngineConfig { monotonic: true, ..EngineConfig::default() }).await;
    let t = tenant();
    let (a, b) = (actor(t, 1), actor(t, 2));
    // Committed, response lost: the client retries with the same key.
    let r = pg.execute_idempotent(&a, "k1", &add(0, 3)).await.unwrap().unwrap();
    let before = stat(&pg);
    let again = pg.execute_idempotent(&a, "k1", &add(0, 3)).await.unwrap().unwrap();
    assert_eq!(again, r);
    assert_eq!(stat(&pg).3 - before.3, 1, "a replay");
    assert_eq!(db.cells(t).await.get(0), 3, "no second write");
    // Same key, another command.
    let e = pg.execute_idempotent(&a, "k1", &add(0, 4)).await;
    assert!(matches!(e, Err(DbError::IdempotencyConflict)), "{e:?}");
    // Same key, another scope (user) or tenant: a different key.
    pg.execute_idempotent(&b, "k1", &add(0, 3)).await.unwrap().unwrap();
    let t2 = tenant();
    pg.execute_idempotent(&actor(t2, 1), "k1", &add(0, 3)).await.unwrap().unwrap();
    assert_eq!(db.cells(t).await.get(0), 6);
    assert_eq!(db.cells(t2).await.get(0), 3);
    // A refusal writes no app or framework row and is not stored: the same
    // key later runs on the new state.
    let before_rows = rows(&db, t).await;
    let snap = db.cells(t).await;
    assert_eq!(pg.execute_idempotent(&a, "k2", &add(0, -100)).await.unwrap(), Err(Refused::Negative));
    assert_eq!(pg.execute(&a, &add(5, -1)).await.unwrap(), Err(Refused::Negative));
    assert_eq!(rows(&db, t).await, before_rows);
    assert_eq!(db.cells(t).await, snap);
    pg.execute(&a, &add(0, 100)).await.unwrap().unwrap();
    let before = stat(&pg);
    assert_eq!(pg.execute_idempotent(&a, "k2", &add(0, -100)).await.unwrap().unwrap().value, 6);
    assert_eq!(stat(&pg).3 - before.3, 0, "ran, not replayed");
}

// Bullet 5 under concurrency: the same key sent by many clients at once runs
// once; every client gets the same reply. Without advisory locks the losers
// fail on the key's primary key or to serialize, and retry into a replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_requests_with_one_key_run_once() {
    let Some(db) = setup("h5i_fault_idem_race").await else { return };
    for lock in [true, false] {
        let pg = Arc::new(db.engine(16, config(50, lock)).await);
        let t = tenant();
        let a = actor(t, 1);
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let (pg, a) = (pg.clone(), a.clone());
            tasks.push(tokio::spawn(async move { pg.execute_idempotent(&a, "once", &add(0, 1)).await }));
        }
        let mut replies = Vec::new();
        for task in tasks {
            replies.push(tokio::time::timeout(T, task).await.unwrap().unwrap().unwrap().unwrap());
        }
        assert!(replies.iter().all(|r| *r == replies[0]), "lock={lock}: {replies:?}");
        assert_eq!(replies[0].value, 1);
        assert_eq!(db.cells(t).await.get(0), 1, "lock={lock}");
        assert_eq!(pg.stats.replays.load(Ordering::Relaxed), 31, "lock={lock}");
    }
}

// Bullet 4: app writes, the stored reply, `h5i_clock` and the outbox row
// commit together. A COMMIT lost before the server ran it leaves none of
// them; one lost after leaves all of them (the engine, out of attempts,
// reports `RetriesExhausted` although it committed; a retry with the key
// gets the reply).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn framework_rows_commit_with_the_app_writes() {
    let Some(db) = setup("h5i_fault_atomic").await else { return };
    let proxy = Proxy::start(&db.base_url).await;
    let config = EngineConfig { monotonic: true, max_attempts: 1, ..EngineConfig::default() };
    let pg = db.engine_at(&proxy.url(&db.url), 2, config).await;
    for (fault, n) in [(Fault::DropBefore, 0), (Fault::DropAfter, 1)] {
        let t = tenant();
        let a = actor(t, 1);
        proxy.arm(fault);
        let r = tokio::time::timeout(T, pg.execute_idempotent(&a, "e", &Cmd::Emit { cell: 0, dest: 1 })).await.unwrap();
        assert!(matches!(r, Err(DbError::RetriesExhausted)), "{fault:?}: {r:?}");
        assert_eq!(rows(&db, t).await, vec![n; 4], "{fault:?}: cells, reply, clock, outbox");
        if fault == Fault::DropAfter {
            let r = pg.execute_idempotent(&a, "e", &Cmd::Emit { cell: 0, dest: 1 }).await.unwrap().unwrap();
            assert_eq!(r.value, 1);
            assert_eq!(db.count("h5i_outbox", t).await, 1);
        }
    }
}

// Bullet 7: advisory locks are released, and never carried by a pooled
// connection into the next request: after a failed attempt, after a killed
// connection, for tenant and key locks. And they are taken before BEGIN.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn advisory_locks_are_released() {
    let Some(db) = setup("h5i_fault_locks").await else { return };
    // One connection, so the next request reuses the failed one's.
    let pg = Arc::new(db.engine(1, EngineConfig::default()).await);
    let other = db.engine(1, EngineConfig::default()).await;
    let t = tenant();
    let a = actor(t, 1);
    pg.execute(&a, &add(0, 10)).await.unwrap().unwrap();
    pg.execute(&a, &add(1, 10)).await.unwrap().unwrap();

    // A failed attempt (not retryable).
    set_hook(t, at_first(Point::Written, 1, |_| async { Err("injected".to_string()) }));
    let r = pg.execute(&a, &add(0, 1)).await;
    assert!(matches!(r, Err(DbError::Decode(ref m)) if m == "injected"), "{r:?}");
    assert_eq!(db.held_advisory_locks().await, 0);
    // Another pool (another server) gets the tenant at once.
    tokio::time::timeout(Duration::from_secs(5), other.execute(&a, &add(0, 1))).await.expect("tenant lock free").unwrap().unwrap();

    // While held: the tenant lock exclusively, or the tenant shared and one
    // exclusive lock per key.
    let held: Arc<Mutex<Vec<Locks>>> = Arc::default();
    let (h, admin) = (held.clone(), db.admin.clone());
    set_hook(t, move |p, pid| {
        let (h, admin) = (h.clone(), admin.clone());
        Box::pin(async move {
            if p == Point::Write {
                let rows = admin
                    .query("SELECT mode, granted FROM pg_locks WHERE locktype = 'advisory' AND pid = $1 ORDER BY mode", &[&pid])
                    .await
                    .unwrap();
                h.lock().unwrap().push(rows.iter().map(|r| (r.get(0), r.get(1))).collect());
            }
            Ok(())
        })
    });
    pg.execute(&a, &add(0, 1)).await.unwrap().unwrap();
    pg.execute(&a, &Cmd::Move { from: 0, to: 1, amount: 2 }).await.unwrap().unwrap();
    clear_hook(t);
    let x = |m: &str| (m.to_string(), true);
    assert_eq!(*held.lock().unwrap(), vec![vec![x("ExclusiveLock")], vec![x("ExclusiveLock"), x("ExclusiveLock"), x("ShareLock")]]);
    assert_eq!(db.held_advisory_locks().await, 0);

    // A killed connection: the server drops its locks with the session.
    let admin = db.admin.clone();
    set_hook(t, at_first(Point::Write, 1, move |pid| {
        let admin = admin.clone();
        async move {
            kill(&admin, pid).await;
            Ok(())
        }
    }));
    pg.execute(&a, &Cmd::Move { from: 0, to: 1, amount: 1 }).await.unwrap().unwrap();
    clear_hook(t);
    assert_eq!(db.held_advisory_locks().await, 0);

    // Taken before BEGIN: a request queued behind a held tenant lock waits in
    // `pg_advisory_lock` before its transaction has a snapshot, so once the
    // holder commits it loads the new state and commits without a retry
    // (a snapshot from before the wait would fail to serialize).
    let (go, wait) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
    let (g, w) = (go.clone(), wait.clone());
    set_hook(t, at_first(Point::Write, 1, move |_| {
        let (g, w) = (g.clone(), w.clone());
        async move {
            w.notify_one();
            g.notified().await;
            Ok(())
        }
    }));
    let p = pg.clone();
    let (a2, a3) = (a.clone(), a.clone());
    let n = seen(t).len();
    let holder = tokio::spawn(async move { p.execute(&a2, &add(0, 1)).await });
    wait.notified().await;
    let queued = tokio::spawn(async move {
        let r = other.execute(&a3, &add(0, 1)).await;
        (other, r)
    });
    let waiting = loop {
        let rows = db
            .admin
            .query(
                "SELECT a.query, a.backend_xid IS NULL
                 FROM pg_stat_activity a JOIN pg_locks l ON l.pid = a.pid
                 WHERE a.application_name = $1 AND l.locktype = 'advisory' AND NOT l.granted",
                &[&db.name],
            )
            .await
            .unwrap();
        if let Some(r) = rows.first() {
            break (r.get::<_, String>(0), r.get::<_, bool>(1));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(waiting, ("SELECT pg_advisory_lock($1)".to_string(), true));
    go.notify_one();
    holder.await.unwrap().unwrap().unwrap();
    let (other, r) = tokio::time::timeout(T, queued).await.unwrap().unwrap();
    // Its snapshot was taken after the wait: it saw the holder's commit and
    // did not have to retry.
    let r = r.unwrap().unwrap();
    assert_eq!(other.stats.retries.load(Ordering::Relaxed), 0);
    let runs = &seen(t)[n..];
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1].snap.get(0), runs[0].snap.get(0) + 1, "the queued request loaded the holder's write");
    assert_eq!(r.value, runs[0].snap.get(0) + 2);
    clear_hook(t);
    assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(2)).await, 0);
}

// Bullet 8: a read-only command runs in a SERIALIZABLE READ ONLY DEFERRABLE
// transaction without advisory locks, is retried when its connection drops,
// is not retried otherwise, and fails if it writes. A keyed read takes the
// write path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_path() {
    let Some(db) = setup("h5i_fault_read").await else { return };
    let pg = db.engine(2, EngineConfig::default()).await;
    let t = tenant();
    let a = actor(t, 1);
    pg.execute(&a, &add(0, 4)).await.unwrap().unwrap();

    let locks: Arc<Mutex<Vec<i64>>> = Arc::default();
    let (l, admin) = (locks.clone(), db.admin.clone());
    let killed = Arc::new(AtomicU32::new(0));
    let k = killed.clone();
    set_hook(t, move |p, pid| {
        let (l, admin, k) = (l.clone(), admin.clone(), k.clone());
        Box::pin(async move {
            if p == Point::Load {
                let n: i64 = admin.query_one("SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND pid = $1", &[&pid]).await.unwrap().get(0);
                l.lock().unwrap().push(n);
                if k.fetch_add(1, Ordering::SeqCst) == 0 {
                    kill(&admin, pid).await;
                }
            }
            Ok(())
        })
    });
    let (before, n) = (stat(&pg), attempts(t).len());
    let r = tokio::time::timeout(T, pg.execute(&a, &Cmd::Get { cell: 0 })).await.unwrap().unwrap().unwrap();
    clear_hook(t);
    assert_eq!(r.value, 4);
    let s = stat(&pg);
    assert_eq!((s.1 - before.1, s.4 - before.4), (1, 2), "retried once after the kill, both on the read path");
    assert_eq!(*locks.lock().unwrap(), vec![0, 0], "no advisory locks");
    for tx in &attempts(t)[n..] {
        assert_eq!((tx.isolation.as_str(), tx.read_only, tx.deferrable), ("serializable", true, true));
    }

    // Not retried for other errors.
    set_hook(t, at_first(Point::Load, 1, |_| async { Err("injected".to_string()) }));
    let before = stat(&pg);
    let r = pg.execute(&a, &Cmd::Get { cell: 0 }).await;
    clear_hook(t);
    assert!(matches!(r, Err(DbError::Decode(_))), "{r:?}");
    assert_eq!(stat(&pg).0 - before.0, 1);

    // A read-only command with a non-empty write set fails the request
    // (PostgreSQL refuses the write in the READ ONLY transaction).
    let before = stat(&pg);
    let r = pg.execute(&a, &Cmd::BadRead { cell: 0 }).await;
    match &r {
        Err(DbError::Postgres(e)) => assert_eq!(e.code(), Some(&SqlState::READ_ONLY_SQL_TRANSACTION), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(stat(&pg).0 - before.0, 1, "not retried");
    assert_eq!(db.cells(t).await.get(0), 4);

    // With a key, the write path: a read-write transaction.
    let n = attempts(t).len();
    assert_eq!(pg.execute_idempotent(&a, "g", &Cmd::Get { cell: 0 }).await.unwrap().unwrap().value, 4);
    assert!(!attempts(t)[n].read_only);
    assert_eq!(db.count("h5i_idempotency", t).await, 1);
}

#[derive(Clone, Default)]
struct Inbox {
    seen: Arc<Mutex<Vec<Delivery>>>,
    /// Fail this many deliveries first.
    fail: Arc<AtomicU32>,
    /// Never answer (a dispatcher that dies mid-delivery).
    hang: bool,
}

impl Deliver<String> for Inbox {
    async fn deliver(&self, _endpoint: &String, d: &Delivery) -> Result<(), String> {
        self.seen.lock().unwrap().push(d.clone());
        if self.hang {
            std::future::pending::<()>().await;
        }
        if take_one(&self.fail) {
            return Err("unavailable".into());
        }
        Ok(())
    }
}

// Bullets 4 and 9: an outbox row commits with the request's writes (a
// killed connection leaves neither); deliveries repeat with the same key,
// after a failure and after a dispatcher dies holding a lease; an unknown
// destination is never delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_is_atomic_and_redelivers_with_one_key() {
    let Some(db) = setup("h5i_fault_outbox").await else { return };
    let once = db.engine(2, config(1, true)).await;
    let pg = db.engine(2, EngineConfig::default()).await;
    let t = tenant();
    let a = actor(t, 1);
    let kill_at = |admin: Arc<tokio_postgres::Client>| {
        at_first(Point::Enqueued, 1, move |pid| {
            let admin = admin.clone();
            async move {
                kill(&admin, pid).await;
                Ok(())
            }
        })
    };
    // Killed after the outbox insert, out of attempts: neither row. (Keyed,
    // so that a COMMIT that fails on the dead connection is retried too.)
    set_hook(t, kill_at(db.admin.clone()));
    let r = once.execute_idempotent(&a, "e1", &Cmd::Emit { cell: 0, dest: 1 }).await;
    assert!(matches!(r, Err(DbError::RetriesExhausted)), "{r:?}");
    assert_eq!((db.count("cells", t).await, db.count("h5i_outbox", t).await), (0, 0));
    // Killed, then retried: one of each.
    set_hook(t, kill_at(db.admin.clone()));
    pg.execute_idempotent(&a, "e2", &Cmd::Emit { cell: 0, dest: 1 }).await.unwrap().unwrap();
    clear_hook(t);
    assert_eq!((db.cells(t).await.get(0), db.count("h5i_outbox", t).await), (1, 1));

    let registry = || HashMap::from([(1u64, "endpoint".to_string())]);
    // A failed delivery is retried (after 1 s) with the same key.
    let inbox = Inbox { fail: Arc::new(AtomicU32::new(1)), ..Inbox::default() };
    let d = pg.dispatcher(registry(), inbox.clone(), DispatchConfig::default());
    assert_eq!(d.run_once().await.unwrap(), Pass { delivered: 0, failed: 1, dead: 0 });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(d.run_once().await.unwrap(), Pass { delivered: 1, failed: 0, dead: 0 });
    assert_eq!(d.run_once().await.unwrap(), Pass::default());
    let seen = inbox.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].key, seen[1].key);
    assert_eq!((seen[0].attempt, seen[1].attempt), (1, 2));
    assert_eq!(seen[0].payload, b"0:1");
    assert!(seen[0].key.starts_with(&format!("{t}-")));

    // A dispatcher that dies mid-delivery: after its lease another delivers
    // the same effect with the same key.
    pg.execute(&a, &Cmd::Emit { cell: 0, dest: 1 }).await.unwrap().unwrap();
    let lease = DispatchConfig { lease_secs: 0.5, ..DispatchConfig::default() };
    let dying = Inbox { hang: true, ..Inbox::default() };
    let d1 = pg.dispatcher(registry(), dying.clone(), lease.clone());
    assert!(tokio::time::timeout(Duration::from_millis(200), d1.run_once()).await.is_err());
    drop(d1);
    let inbox = Inbox::default();
    let d2 = pg.dispatcher(registry(), inbox.clone(), lease.clone());
    assert_eq!(d2.run_once().await.unwrap(), Pass::default(), "leased");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(d2.run_once().await.unwrap(), Pass { delivered: 1, failed: 0, dead: 0 });
    let (first, second) = (dying.seen.lock().unwrap()[0].clone(), inbox.seen.lock().unwrap()[0].clone());
    assert_eq!(first.key, second.key);
    assert_eq!((first.attempt, second.attempt), (1, 2));
    assert_eq!(second.payload, b"0:2");

    // A destination outside the registry is marked dead, never delivered.
    pg.execute(&a, &Cmd::Emit { cell: 0, dest: 99 }).await.unwrap().unwrap();
    let inbox = Inbox::default();
    let d3 = pg.dispatcher(registry(), inbox.clone(), DispatchConfig::default());
    assert_eq!(d3.run_once().await.unwrap(), Pass { delivered: 0, failed: 0, dead: 1 });
    assert!(inbox.seen.lock().unwrap().is_empty());
}

// Cancellation: dropping a request's future mid-transaction (here: after its
// writes, before COMMIT), or while it waits for an advisory lock, leaves no
// write and no advisory lock, and other servers get the tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_requests_leave_no_write_and_no_lock() {
    let Some(db) = setup("h5i_fault_cancel").await else { return };
    let pg = Arc::new(db.engine(2, EngineConfig::default()).await);
    let other = db.engine(2, EngineConfig::default()).await;
    for keyed in [false, true] {
        let t = tenant();
        let a = actor(t, 1);
        pg.execute(&a, &add(0, 1)).await.unwrap().unwrap();
        let reached = Arc::new(tokio::sync::Notify::new());
        let r = reached.clone();
        set_hook(t, at_first(Point::Written, 1, move |_| {
            let r = r.clone();
            async move {
                r.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            }
        }));
        let (p, a2) = (pg.clone(), a.clone());
        let task = tokio::spawn(async move {
            if keyed { p.execute_idempotent(&a2, "c", &add(0, 5)).await } else { p.execute(&a2, &add(0, 5)).await }
        });
        reached.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        clear_hook(t);
        assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(5)).await, 0, "keyed={keyed}: lock left held");
        assert_eq!(db.cells(t).await.get(0), 1, "keyed={keyed}: no partial write");
        assert_eq!(db.count("h5i_idempotency", t).await, 0);
        tokio::time::timeout(Duration::from_secs(5), other.execute(&a, &add(0, 1))).await.expect("tenant lock free").unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), pg.execute(&a, &add(0, 1))).await.expect("pool usable").unwrap().unwrap();
        assert_eq!(db.cells(t).await.get(0), 3);
    }

    // Cancelled while waiting for the tenant lock another request holds.
    let t = tenant();
    let a = actor(t, 1);
    let (go, wait) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
    let (g, w) = (go.clone(), wait.clone());
    set_hook(t, at_first(Point::Write, 1, move |_| {
        let (g, w) = (g.clone(), w.clone());
        async move {
            w.notify_one();
            g.notified().await;
            Ok(())
        }
    }));
    let (p, a1) = (pg.clone(), a.clone());
    let holder = tokio::spawn(async move { p.execute(&a1, &add(0, 1)).await });
    wait.notified().await;
    let (p, a2) = (pg.clone(), a.clone());
    let queued = tokio::spawn(async move { p.execute(&a2, &add(0, 100)).await });
    loop {
        let n: i64 = db
            .admin
            .query_one(
                "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                 WHERE a.application_name = $1 AND l.locktype = 'advisory' AND NOT l.granted",
                &[&db.name],
            )
            .await
            .unwrap()
            .get(0);
        if n > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    go.notify_one();
    holder.await.unwrap().unwrap().unwrap();
    clear_hook(t);
    assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(5)).await, 0, "the cancelled wait left a lock");
    assert_eq!(db.cells(t).await.get(0), 1, "the cancelled request did not run");
    tokio::time::timeout(Duration::from_secs(5), other.execute(&a, &add(0, 1))).await.expect("tenant lock free").unwrap().unwrap();
}

/// Server backends of this test's engines.
async fn backends(db: &Db) -> i64 {
    db.admin.query_one("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1", &[&db.name]).await.unwrap().get(0)
}

/// Locks this test's engine backends wait for (advisory, row, transaction).
async fn waiting(db: &Db) -> i64 {
    db.admin
        .query_one(
            "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
             WHERE a.application_name = $1 AND NOT l.granted",
            &[&db.name],
        )
        .await
        .unwrap()
        .get(0)
}

// Cancellation under a flood (from an independent review): one request holds
// the tenant (its advisory lock, or without advisory locks its row lock),
// then 40 requests for the same tenant are each cancelled while waiting for
// it. The engine's connections must stay within its pools (2 + 1): a
// cancelled waiter's connection may not leave the pool's count while its
// backend still waits on the server, or each cancellation costs a backend
// until `max_connections` runs out for every client of the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiters_stay_within_the_pool() {
    let Some(db) = setup("h5i_fault_zombie").await else { return };
    for tenant_lock in [true, false] {
        let pool = h5i_app_pg::pool(&db.url, 2).unwrap();
        let pg = Arc::new(Pg::new(pool.clone(), config(20, tenant_lock)));
        pg.install_schema().await.unwrap();
        let holder_engine = Arc::new(db.engine(1, config(20, tenant_lock)).await);
        let t = tenant();
        let a = actor(t, 1);
        pg.execute(&a, &add(0, 1)).await.unwrap().unwrap();
        // With advisory locks the waiters queue on the tenant lock; without,
        // on the holder's row lock (taken by its write).
        let at = if tenant_lock { Point::Write } else { Point::Written };
        let (go, wait) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        let (g, w) = (go.clone(), wait.clone());
        set_hook(t, at_first(at, 1, move |_| {
            let (g, w) = (g.clone(), w.clone());
            async move {
                w.notify_one();
                g.notified().await;
                Ok(())
            }
        }));
        let (p, a1) = (holder_engine.clone(), a.clone());
        let holder = tokio::spawn(async move { p.execute(&a1, &add(0, 1)).await });
        wait.notified().await;
        let before = waiting(&db).await;
        let mut max_backends = 0;
        let mut queued = 0u64;
        for i in 0..40 {
            // Give the previous cancellation's cleanup a moment, then count
            // what waits now, so this request's own wait is seen.
            let settle = tokio::time::Instant::now() + Duration::from_secs(2);
            while waiting(&db).await > before && tokio::time::Instant::now() < settle {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let w0 = waiting(&db).await;
            let (p, a2) = (pg.clone(), a.clone());
            let q = tokio::spawn(async move { p.execute(&a2, &add(0, 100)).await });
            // Until it waits on the server (or a timeout, if the pool has no
            // connection for it).
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while waiting(&db).await <= w0 && !q.is_finished() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if waiting(&db).await > w0 {
                queued += 1;
            }
            assert!(!q.is_finished(), "tenant_lock={tenant_lock}: request {i} finished while the tenant was held: {:?}", q.await);
            q.abort();
            let _ = q.await;
            max_backends = max_backends.max(backends(&db).await);
        }
        // Another client of the server still connects.
        let fresh = tokio_postgres::connect(&db.base_url, tokio_postgres::NoTls).await;
        assert!(fresh.is_ok(), "tenant_lock={tenant_lock}: {:?}", fresh.err());
        drop(fresh);
        eprintln!("tenant_lock={tenant_lock}: {queued} of 40 cancelled while waiting; at most {max_backends} engine backends");
        assert!(queued >= 30, "tenant_lock={tenant_lock}: only {queued} of 40 requests were seen waiting before their cancellation");
        assert!(max_backends <= 3, "tenant_lock={tenant_lock}: {max_backends} engine backends for pools of 2 + 1");
        go.notify_one();
        tokio::time::timeout(T, holder).await.unwrap().unwrap().unwrap().unwrap();
        clear_hook(t);
        // Nothing waits, nothing is held, and the cancelled requests never ran.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while waiting(&db).await > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(waiting(&db).await, 0, "tenant_lock={tenant_lock}");
        assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(5)).await, 0, "tenant_lock={tenant_lock}");
        assert_eq!(db.cells(t).await.get(0), 2, "tenant_lock={tenant_lock}");
        assert!(backends(&db).await <= 3, "tenant_lock={tenant_lock}");
        // Every cancelled connection was cleaned and returned, none detached.
        let stats = pool.cleanup_stats();
        assert!(stats.cleaned.load(Ordering::Relaxed) >= queued, "tenant_lock={tenant_lock}: {stats:?}");
        assert_eq!(stats.detached.load(Ordering::Relaxed), 0, "tenant_lock={tenant_lock}");
        // The pool still serves.
        tokio::time::timeout(Duration::from_secs(5), pg.execute(&a, &add(0, 1))).await.expect("pool usable").unwrap().unwrap();
        drop((pg, holder_engine));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// `EngineConfig::lock_timeout`: a write that waits longer than it for the
// tenant's advisory lock, or (without advisory locks) for a row lock, fails
// with 55P03 instead of queueing; it is not retried and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lock_timeout_bounds_lock_waits() {
    let Some(db) = setup("h5i_fault_lock_timeout").await else { return };
    for tenant_lock in [true, false] {
        let cfg = EngineConfig { lock_timeout: Some(Duration::from_millis(300)), tenant_lock, ..EngineConfig::default() };
        let pg = db.engine(2, cfg).await;
        let holder_engine = Arc::new(db.engine(1, config(20, tenant_lock)).await);
        let t = tenant();
        let a = actor(t, 1);
        pg.execute(&a, &add(0, 1)).await.unwrap().unwrap();
        let at = if tenant_lock { Point::Write } else { Point::Written };
        let (go, wait) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        let (g, w) = (go.clone(), wait.clone());
        set_hook(t, at_first(at, 1, move |_| {
            let (g, w) = (g.clone(), w.clone());
            async move {
                w.notify_one();
                g.notified().await;
                Ok(())
            }
        }));
        let (p, a1) = (holder_engine.clone(), a.clone());
        let holder = tokio::spawn(async move { p.execute(&a1, &add(0, 1)).await });
        wait.notified().await;
        let before = stat(&pg);
        let start = std::time::Instant::now();
        let r = tokio::time::timeout(T, pg.execute(&a, &add(0, 100))).await.unwrap();
        let took = start.elapsed();
        match &r {
            Err(DbError::Postgres(e)) => assert_eq!(e.code(), Some(&SqlState::LOCK_NOT_AVAILABLE), "tenant_lock={tenant_lock}: {e}"),
            other => panic!("tenant_lock={tenant_lock}: {other:?}"),
        }
        assert!(took < Duration::from_secs(5), "tenant_lock={tenant_lock}: {took:?}");
        assert_eq!(stat(&pg).0 - before.0, 1, "tenant_lock={tenant_lock}: not retried");
        go.notify_one();
        holder.await.unwrap().unwrap().unwrap();
        clear_hook(t);
        assert_eq!(db.cells(t).await.get(0), 2, "tenant_lock={tenant_lock}: only the holder wrote");
        // Uncontended, the same engine writes.
        pg.execute(&a, &add(0, 1)).await.unwrap().unwrap();
        assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(2)).await, 0);
    }
}

/// A serial order of `done` (actor, command, reply, writes) through
/// `transition` from `start` that reproduces every reply and write set, by
/// depth-first search; and the state it ends in. Each command writes every
/// row it reads, with absolute values, so in every order `transition`
/// accepts the set of commands run so far leaves the same state; a set
/// from which no order completes is remembered and never searched again: the search is
/// complete, and `Err` says whether it proved there is no order or ran out
/// of budget.
fn serial_order(start: Snap, done: &[(Actor, Cmd, Reply, Vec<W>)]) -> Result<Snap, String> {
    use h5i_app_core::Kernel;
    use std::collections::HashSet;
    struct Search<'a> {
        done: &'a [(Actor, Cmd, Reply, Vec<W>)],
        used: Vec<bool>,
        dead: HashSet<Vec<bool>>,
        budget: u64,
    }
    impl Search<'_> {
        fn go(&mut self, s: &Snap, left: usize) -> Result<Option<Snap>, ()> {
            if left == 0 {
                return Ok(Some(s.clone()));
            }
            if self.dead.contains(&self.used) {
                return Ok(None);
            }
            for i in 0..self.done.len() {
                if self.used[i] {
                    continue;
                }
                self.budget = self.budget.checked_sub(1).ok_or(())?;
                let (a, c, r, ws) = &self.done[i];
                let mut a = a.clone();
                a.now = r.now;
                if let Ok((ws2, r2)) = Bank::transition(&a, s, c)
                    && ws2 == *ws
                    && r2 == *r
                {
                    self.used[i] = true;
                    let end = self.go(&Bank::apply(s, ws), left - 1);
                    self.used[i] = false;
                    if let Some(end) = end? {
                        return Ok(Some(end));
                    }
                }
            }
            self.dead.insert(self.used.clone());
            Ok(None)
        }
    }
    let mut search = Search { done, used: vec![false; done.len()], dead: HashSet::new(), budget: 200_000_000 };
    match search.go(&start, done.len()) {
        Ok(Some(end)) => Ok(end),
        Ok(None) => Err(format!("no serial order of the {} commits exists", done.len())),
        Err(()) => Err(format!("search budget exhausted ({} dead sets); undecided", search.dead.len())),
    }
}

// Bullets 1 and 7 under load, with and without advisory locks: 64 clients
// move amounts between 4 cells of one tenant. The total is conserved, no
// cell goes negative (refused), the committed state is a serial run of the
// successful commands, another tenant is untouched, and no lock is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_moves_are_serial() {
    let Some(db) = setup("h5i_fault_concurrent").await else { return };
    for lock in [true, false] {
        let pg = Arc::new(db.engine(16, config(500, lock)).await);
        let (t, bystander) = (tenant(), tenant());
        let start_cells = [40u64, 30, 20, 10];
        for (i, v) in start_cells.iter().enumerate() {
            pg.execute(&actor(t, 1), &add(i as u64, *v as i64)).await.unwrap().unwrap();
            pg.execute(&actor(bystander, 1), &add(i as u64, 7)).await.unwrap().unwrap();
        }
        let start = db.cells(t).await;
        let mut tasks = Vec::new();
        for client in 0..64u64 {
            let pg = pg.clone();
            tasks.push(tokio::spawn(async move {
                let mut s = client.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
                let mut rng = |n: u64| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    s % n
                };
                let mut out = Vec::new();
                for _ in 0..8 {
                    let (from, to, amount) = (rng(4), rng(4), 1 + rng(25));
                    let a = actor(t, client);
                    let cmd = Cmd::Move { from, to, amount };
                    let c = pg.execute_committed(&a, &cmd).await;
                    out.push((a, cmd, c));
                }
                out
            }));
        }
        let mut done = Vec::new();
        let (mut refused, mut failed) = (0, 0);
        for task in tasks {
            for (a, cmd, c) in tokio::time::timeout(Duration::from_secs(120), task).await.unwrap().unwrap() {
                match c {
                    Ok(c) => match c.result {
                        Ok(r) => done.push((a, cmd, r, c.writes.expect("a commit has its writes"))),
                        Err(_) => refused += 1,
                    },
                    Err(DbError::RetriesExhausted) => failed += 1,
                    Err(e) => panic!("lock={lock}: {e}"),
                }
            }
        }
        let end = db.cells(t).await;
        assert_eq!(end.total(), start.total(), "lock={lock}: conserved");
        assert_eq!(done.len() + refused + failed, 64 * 8);
        let commits = pg.stats.commits.load(Ordering::Relaxed) as usize - 8;
        assert_eq!(commits, done.len(), "lock={lock}: one commit per successful reply");
        let serial = serial_order(start.clone(), &done).unwrap_or_else(|e| panic!("lock={lock}: {e}"));
        assert_eq!(serial, end, "lock={lock}");
        assert_eq!(db.cells(bystander).await, Snap { cells: (0..4).map(|id| Cell { id, v: 7 }).collect() });
        assert_eq!(db.wait_no_advisory_locks(Duration::from_secs(2)).await, 0);
        let retries = pg.stats.retries.load(Ordering::Relaxed);
        eprintln!("lock={lock}: {} commits, {refused} refused, {failed} out of attempts, {retries} retries", done.len());
        assert!(done.len() > 64, "lock={lock}: too few commits to mean anything");
    }
}

// `monotonic`: time never goes back in commit order within a tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monotonic_time_never_goes_back() {
    let Some(db) = setup("h5i_fault_clock").await else { return };
    let clock = ManualClock::new(Timestamp(1_000));
    let pg = db.engine(2, EngineConfig { clock: Clock::Manual(clock.clone()), monotonic: true, ..EngineConfig::default() }).await;
    let t = tenant();
    let a = actor(t, 1);
    assert_eq!(pg.execute(&a, &add(0, 1)).await.unwrap().unwrap().now, 1_000);
    clock.set(Timestamp(500));
    assert_eq!(pg.execute(&a, &add(0, 1)).await.unwrap().unwrap().now, 1_000);
    clock.set(Timestamp(2_000));
    assert_eq!(pg.execute(&a, &add(0, 1)).await.unwrap().unwrap().now, 2_000);
    let last: i64 = db.admin.query_one(&format!("SELECT last FROM {}.h5i_clock WHERE tenant_id = $1", db.name), &[&(t as i64)]).await.unwrap().get(0);
    assert_eq!(last, 2_000);
}
