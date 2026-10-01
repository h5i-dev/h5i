mod common;

use common::*;
use docs_kernel as k;
use docs_server::{principal, DocsApp, DocsStore};
use h5i_app_core::TenantId;
use tokio_postgres::NoTls;
use h5i_app_pg::{pool, DbError, Engine, EngineConfig, ReplyCodec, Store, Tx};
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering::SeqCst};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

// Where the next FaultStore call pauses: 0 nowhere, 1 in load, 2 after write.
static PAUSE_AT: AtomicU8 = AtomicU8::new(0);
static PID: AtomicI32 = AtomicI32::new(0);
static PAUSED: Notify = Notify::const_new();
static RESUME: Notify = Notify::const_new();
// Fault tests share the statics above, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

/// DocsStore that can stop at a chosen point so a test can kill its connection.
struct FaultStore;

async fn maybe_pause(tx: &Tx<'_>, at: u8) -> Result<(), DbError> {
    if PAUSE_AT.compare_exchange(at, 0, SeqCst, SeqCst).is_ok() {
        let pid = tx.backend_pid().await?;
        PID.store(pid, SeqCst);
        PAUSED.notify_one();
        RESUME.notified().await;
    }
    Ok(())
}

impl Store<DocsApp> for FaultStore {
    fn ddl() -> Vec<String> {
        DocsStore::ddl()
    }

    fn tables() -> Vec<&'static str> {
        DocsStore::tables()
    }

    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        maybe_pause(tx, 1).await?;
        DocsStore::load(tx, t).await
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        DocsStore::write(tx, t, ws).await?;
        maybe_pause(tx, 2).await
    }
}

impl ReplyCodec<DocsApp> for FaultStore {
    fn fingerprint(c: &k::Command) -> Vec<u8> {
        <DocsStore as ReplyCodec<DocsApp>>::fingerprint(c)
    }
    fn scope(a: &k::Principal) -> String {
        <DocsStore as ReplyCodec<DocsApp>>::scope(a)
    }
    fn encode(r: &k::Reply) -> Vec<u8> {
        <DocsStore as ReplyCodec<DocsApp>>::encode(r)
    }
    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        <DocsStore as ReplyCodec<DocsApp>>::decode(b)
    }
}

async fn setup() -> Option<(Arc<Engine<DocsApp, FaultStore>>, tokio_postgres::Client)> {
    let url = std::env::var("H5I_APP_TEST_DATABASE_URL").ok()?;
    let e = Engine::new(pool(&url, 8).unwrap(), EngineConfig { tenant_lock: true, max_attempts: 20, monotonic: true, ..Default::default() });
    e.install_schema().await.unwrap();
    let (admin, conn) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(conn);
    Some((Arc::new(e), admin))
}

/// Start `cmd` with a pause armed at `at`, kill the engine's backend there, then let it go on.
async fn run_and_kill(
    e: &Arc<Engine<DocsApp, FaultStore>>,
    admin: &tokio_postgres::Client,
    at: u8,
    actor: k::Principal,
    cmd: k::Command,
    key: Option<&'static str>,
) -> Result<Result<k::Reply, k::Error>, DbError> {
    PAUSE_AT.store(at, SeqCst);
    let e2 = e.clone();
    let task = tokio::spawn(async move {
        match key {
            Some(key) => e2.execute_idempotent(&actor, key, &cmd).await,
            None => e2.execute(&actor, &cmd).await,
        }
    });
    PAUSED.notified().await;
    let pid = PID.load(SeqCst);
    admin.execute("SELECT pg_terminate_backend($1)", &[&pid]).await.unwrap();
    RESUME.notify_one();
    task.await.unwrap()
}

fn project(name: &str) -> k::Command {
    k::Command::CreateProject { name: name.as_bytes().to_vec() }
}

macro_rules! setup_or_skip {
    () => {
        match setup().await {
            Some(x) => x,
            None => {
                eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
                return;
            }
        }
    };
}

/// Connection killed before anything was written: the engine retries on a new connection.
#[tokio::test]
async fn killed_during_load_is_retried() {
    let _g = SERIAL.lock().await;
    let (e, admin) = setup_or_skip!();
    let t = fresh_tenant();
    let r = run_and_kill(&e, &admin, 1, principal(t, 1), project("a"), None).await;
    assert!(matches!(r, Ok(Ok(k::Reply::Created(_)))), "{r:?}");
    assert_eq!(e.snapshot(TenantId(t)).await.unwrap().projects.len(), 1);
    assert!(e.stats.retries.load(SeqCst) >= 1);
}

/// Killed after the rows were written but before COMMIT: either retried (server said
/// it aborted) or surfaced as CommitUnknown. Never partial, never twice.
#[tokio::test]
async fn killed_before_commit_leaves_no_partial_writes() {
    let _g = SERIAL.lock().await;
    let (e, admin) = setup_or_skip!();
    let t = fresh_tenant();
    let r = run_and_kill(&e, &admin, 2, principal(t, 1), project("b"), None).await;
    let snap = e.snapshot(TenantId(t)).await.unwrap();
    match r {
        Ok(Ok(k::Reply::Created(_))) => {
            eprintln!("outcome: retried");
            assert_eq!(snap.projects.len(), 1)
        }
        Err(DbError::CommitUnknown(_)) => {
            eprintln!("outcome: commit unknown");
            assert_eq!(snap, k::Snapshot::default(), "a killed transaction must leave nothing")
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Same fault under an idempotency key: the engine may always retry, and applies once.
#[tokio::test]
async fn killed_before_commit_with_key_applies_once() {
    let _g = SERIAL.lock().await;
    let (e, admin) = setup_or_skip!();
    let t = fresh_tenant();
    let r = run_and_kill(&e, &admin, 2, principal(t, 1), project("c"), Some("k-kill")).await;
    assert!(matches!(r, Ok(Ok(k::Reply::Created(_)))), "{r:?}");
    let again = e.execute_idempotent(&principal(t, 1), "k-kill", &project("c")).await.unwrap();
    assert_eq!(again, r.unwrap());
    assert_eq!(e.snapshot(TenantId(t)).await.unwrap().projects.len(), 1);
}

/// The reply is lost after COMMIT (client crash). Retrying with the key returns the
/// stored reply and does not apply the command again.
#[tokio::test]
async fn lost_reply_is_replayed() {
    let _g = SERIAL.lock().await;
    let (e, _admin) = setup_or_skip!();
    let t = fresh_tenant();
    let u = principal(t, 1);
    let first = e.execute_idempotent(&u, "k-lost", &project("d")).await.unwrap();
    drop(first); // the client never saw it
    let replays = e.stats.replays.load(SeqCst);
    let second = e.execute_idempotent(&u, "k-lost", &project("d")).await.unwrap();
    assert!(matches!(second, Ok(k::Reply::Created(_))));
    assert_eq!(e.stats.replays.load(SeqCst), replays + 1);
    assert_eq!(e.snapshot(TenantId(t)).await.unwrap().projects.len(), 1);
}

/// Invariants from Spec.lean, checked on the committed state.
fn check_invariants(s: &k::Snapshot) {
    for p in &s.projects {
        assert!(k::count_owners(&s.members, p.id) >= 1, "project {} has no owner", p.id);
        assert!(p.id < s.counter.next_id);
    }
    for m in &s.members {
        assert!(s.projects.iter().any(|p| p.id == m.project), "member of missing project");
    }
    for d in &s.documents {
        assert!(s.projects.iter().any(|p| p.id == d.project), "document in missing project");
        assert!(d.id < s.counter.next_id);
        if matches!(d.status, k::Status::Approved | k::Status::Published) {
            assert!(matches!(d.approver, Some(a) if a != d.author), "four-eyes rule broken");
        }
    }
}

/// Many writers on one tenant without the advisory lock: SERIALIZABLE alone must keep
/// every invariant.
#[tokio::test]
async fn concurrent_writers_keep_invariants() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<DocsApp, DocsStore>::new(
        pool(&url, 32).unwrap(),
        EngineConfig { tenant_lock: false, max_attempts: 100, ..Default::default() }.database_time(),
    );
    pg.install_schema().await.unwrap();
    let pg = Arc::new(pg);
    let t = fresh_tenant();
    // Seed a few projects so commands have targets.
    for i in 0..3 {
        pg.execute(&principal(t, 1 + i), &project("seed")).await.unwrap().unwrap();
    }
    let mut tasks = vec![];
    for w in 0..16u64 {
        let pg = pg.clone();
        tasks.push(tokio::spawn(async move {
            let mut rng = Rng(w * 7919 + 1);
            let mut exhausted = 0;
            for _ in 0..25 {
                let cmd = random_command(&mut rng);
                match pg.execute(&principal(t, 1 + rng.next(4)), &cmd).await {
                    Ok(_) => {}
                    Err(DbError::RetriesExhausted) => exhausted += 1,
                    Err(e) => panic!("{e}"),
                }
            }
            exhausted
        }));
    }
    let mut exhausted = 0;
    for task in tasks {
        exhausted += task.await.unwrap();
    }
    check_invariants(&pg.snapshot(TenantId(t)).await.unwrap());
    eprintln!("stats: {:?}, retries exhausted: {exhausted}", pg.stats);
}
