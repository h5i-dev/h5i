//! A database server restart under load (A6: connection loss at any point,
//! including COMMIT, on every connection at once).
//!
//! `#[ignore]`d because it restarts the server: run it alone, with
//! `--ignored`, against a server no other test uses. Needs
//! H5I_APP_TEST_DATABASE_URL, H5I_APP_TEST_ADMIN_URL and
//! H5I_APP_TEST_RESTART_CMD, a shell command that restarts that server
//! (e.g. `docker restart h5i-fault-pg`); skips otherwise. Build with
//! `--features testing`.

mod common;

use common::*;
use h5i_app_pg::{DbError, EngineConfig};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WORKERS: u64 = 32;

#[derive(Default)]
struct Tally {
    /// Keys whose request failed: they are sent again after the restart.
    retry: Mutex<Vec<(String, u64)>>,
    keys: AtomicU64,
    unkeyed_ok: AtomicU64,
    unkeyed_unknown: AtomicU64,
    errors: AtomicU64,
    ok_after_restart: AtomicU64,
}

fn allowed(e: &DbError) -> bool {
    // Connection loss, a server that is down or starting, an unknown COMMIT,
    // or retries used up while it was down. Never a decode error or a key
    // conflict, which would mean a request was mixed up with another.
    matches!(e, DbError::Postgres(_) | DbError::Pool(_) | DbError::CommitUnknown(_) | DbError::RetriesExhausted)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "restarts the database server; run alone with --ignored and H5I_APP_TEST_RESTART_CMD"]
async fn database_restart_under_load() {
    let Some(db) = setup("h5i_fault_restart").await else { return };
    let Some(restart) = h5i_app_pg::test_env("H5I_APP_TEST_RESTART_CMD") else {
        return;
    };
    let pg = Arc::new(db.engine(16, EngineConfig::default()).await);
    let t = tenant();
    let (stop, restarted) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let tally = Arc::new(Tally::default());
    let mut workers = Vec::new();
    for w in 0..WORKERS {
        let (pg, stop, restarted, tally) = (pg.clone(), stop.clone(), restarted.clone(), tally.clone());
        workers.push(tokio::spawn(async move {
            let a = actor(t, w + 1);
            let mut i = 0u64;
            while !stop.load(Ordering::SeqCst) {
                i += 1;
                // Even workers use keys on cells 0-3, odd ones none on cells 4-7.
                let keyed = w % 2 == 0;
                let cell = if keyed { w % 4 } else { 4 + w % 4 };
                let cmd = Cmd::Add { cell, delta: 1 };
                let key = format!("w{w}-{i}");
                let r = if keyed {
                    tally.keys.fetch_add(1, Ordering::SeqCst);
                    pg.execute_idempotent(&a, &key, &cmd).await
                } else {
                    pg.execute(&a, &cmd).await
                };
                match r {
                    Ok(r) => {
                        r.expect("adding 1 is never refused");
                        if !keyed {
                            tally.unkeyed_ok.fetch_add(1, Ordering::SeqCst);
                        }
                        if restarted.load(Ordering::SeqCst) {
                            tally.ok_after_restart.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Err(e) => {
                        assert!(allowed(&e), "{e:?}");
                        tally.errors.fetch_add(1, Ordering::SeqCst);
                        if keyed {
                            tally.retry.lock().unwrap().push((key, cell));
                        } else if matches!(e, DbError::CommitUnknown(_)) {
                            tally.unkeyed_unknown.fetch_add(1, Ordering::SeqCst);
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        }));
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = tally.keys.load(Ordering::SeqCst);
    let status = tokio::process::Command::new("sh").arg("-c").arg(&restart).status().await.unwrap();
    assert!(status.success(), "{restart}: {status}");
    restarted.store(true, Ordering::SeqCst);
    // The same engine (same process, same pool) serves again.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tally.ok_after_restart.load(Ordering::SeqCst) < 4 * WORKERS {
        assert!(tokio::time::Instant::now() < deadline, "the engine did not recover");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop.store(true, Ordering::SeqCst);
    for w in workers {
        w.await.unwrap();
    }
    // A key whose request failed is sent again until it gets a reply.
    let retry = tally.retry.lock().unwrap().clone();
    for (key, cell) in &retry {
        let user = key[1..key.find('-').unwrap()].parse::<u64>().unwrap() + 1;
        let mut tries = 0;
        loop {
            match pg.execute_idempotent(&actor(t, user), key, &Cmd::Add { cell: *cell, delta: 1 }).await {
                Ok(r) => {
                    r.unwrap();
                    break;
                }
                Err(e) if tries < 100 => {
                    assert!(allowed(&e), "{e:?}");
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(e) => panic!("{key}: {e}"),
            }
        }
    }
    let admin = connect(&db.admin_url).await;
    let count = |sql: String| {
        let admin = &admin;
        async move { admin.query_one(&sql, &[&(t as i64)]).await.unwrap().get::<_, i64>(0) as u64 }
    };
    let keys = tally.keys.load(Ordering::SeqCst);
    let keyed_sum = count(format!("SELECT coalesce(sum(v), 0)::bigint FROM {}.cells WHERE tenant_id = $1 AND id < 4", db.name)).await;
    let stored = count(format!("SELECT count(*) FROM {}.h5i_idempotency WHERE tenant_id = $1", db.name)).await;
    let unkeyed_sum = count(format!("SELECT coalesce(sum(v), 0)::bigint FROM {}.cells WHERE tenant_id = $1 AND id >= 4", db.name)).await;
    let (ok, unknown) = (tally.unkeyed_ok.load(Ordering::SeqCst), tally.unkeyed_unknown.load(Ordering::SeqCst));
    let errors = tally.errors.load(Ordering::SeqCst);
    eprintln!(
        "{keys} keyed requests ({before} before the restart, {} failed and were sent again), \
         {ok} unkeyed successes, {unknown} unknown commits, {errors} errors",
        retry.len()
    );
    assert!(errors > 0, "the restart did not interrupt any request");
    // Keyed: every key took effect exactly once.
    assert_eq!(keyed_sum, keys);
    assert_eq!(stored, keys);
    // Unkeyed: every success once, an unknown commit at most once, any other
    // error not at all.
    assert!(ok <= unkeyed_sum && unkeyed_sum <= ok + unknown, "{ok} <= {unkeyed_sum} <= {ok} + {unknown}");
}
