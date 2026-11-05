//! What the engine fault tests share: a small kernel and its store, hooks the
//! store runs at fixed points of an attempt, an admin connection that kills
//! backends and inspects locks, and a TCP proxy that loses a COMMIT.
//!
//! The kernel keeps counters ("cells") per tenant. Its store calls the hook
//! registered for the tenant at each [`Point`], with the attempt's backend
//! pid, and records every attempt's [`TxInfo`]; `transition` records the
//! snapshot and time it was called with. Tests use their own PostgreSQL
//! schema, so they can run in parallel.
#![allow(dead_code)]

use h5i_app_core::{Kernel, ReplyCodec, TenantId};
use h5i_app_pg::outbox;
use h5i_app_pg::pgsql::{Column, Kind, Table as Spec};
use h5i_app_pg::sql::{Val, Write as SqlWrite};
use h5i_app_pg::{DbError, Engine, EngineConfig, Lock, Store, Timestamp, Tx, TxInfo};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::NoTls;

pub struct Bank;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub tenant: u64,
    pub user: u64,
    /// Set by `stamp` from the attempt's clock.
    pub now: u64,
}

pub fn actor(tenant: u64, user: u64) -> Actor {
    Actor { tenant, user, now: 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub id: u64,
    pub v: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snap {
    /// In key order.
    pub cells: Vec<Cell>,
}

impl Snap {
    pub fn get(&self, id: u64) -> u64 {
        self.cells.iter().find(|c| c.id == id).map_or(0, |c| c.v)
    }

    pub fn total(&self) -> u128 {
        self.cells.iter().map(|c| u128::from(c.v)).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Refused if the cell would go below zero.
    Add { cell: u64, delta: i64 },
    /// Refused if `from` holds less than `amount`.
    Move { from: u64, to: u64, amount: u64 },
    /// Read-only.
    Get { cell: u64 },
    /// Declared read-only, but writes: the engine must fail the request.
    BadRead { cell: u64 },
    /// Adds 1 and queues an outbox effect to `dest`.
    Emit { cell: u64, dest: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum W {
    Put(Cell),
    Effect { dest: u64, payload: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The cell's value after the command.
    pub value: u64,
    /// The time `transition` ran at.
    pub now: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Negative,
    SameCell,
}

/// One call of `transition`.
#[derive(Clone, Debug)]
pub struct Seen {
    pub cmd: Cmd,
    pub snap: Snap,
    pub now: u64,
}

static SEEN: Mutex<Option<HashMap<u64, Vec<Seen>>>> = Mutex::new(None);
static ATTEMPTS: Mutex<Option<HashMap<u64, Vec<TxInfo>>>> = Mutex::new(None);

/// Every `transition` call for `tenant`, in order.
pub fn seen(tenant: u64) -> Vec<Seen> {
    SEEN.lock().unwrap().as_ref().and_then(|m| m.get(&tenant).cloned()).unwrap_or_default()
}

/// The transaction of every attempt (write or read) that loaded `tenant`.
pub fn attempts(tenant: u64) -> Vec<TxInfo> {
    ATTEMPTS.lock().unwrap().as_ref().and_then(|m| m.get(&tenant).cloned()).unwrap_or_default()
}

fn put(c: Cell) -> W {
    W::Put(c)
}

impl Kernel for Bank {
    type Principal = Actor;
    type Snapshot = Snap;
    type Command = Cmd;
    type WriteSet = Vec<W>;
    type Reply = Reply;
    type Error = Refused;

    fn tenant(a: &Actor) -> TenantId {
        TenantId(a.tenant)
    }

    fn stamp(a: &mut Actor, now: Timestamp) {
        a.now = now.0;
    }

    fn read_only(cmd: &Cmd) -> bool {
        matches!(cmd, Cmd::Get { .. } | Cmd::BadRead { .. })
    }

    fn transition(a: &Actor, s: &Snap, cmd: &Cmd) -> Result<(Vec<W>, Reply), Refused> {
        SEEN.lock().unwrap().get_or_insert_with(HashMap::new).entry(a.tenant).or_default().push(Seen {
            cmd: cmd.clone(),
            snap: s.clone(),
            now: a.now,
        });
        let now = a.now;
        match *cmd {
            Cmd::Add { cell, delta } => {
                let v = i128::from(s.get(cell)) + i128::from(delta);
                let v = u64::try_from(v).map_err(|_| Refused::Negative)?;
                Ok((vec![put(Cell { id: cell, v })], Reply { value: v, now }))
            }
            Cmd::Move { from, to, amount } => {
                if from == to {
                    return Err(Refused::SameCell);
                }
                let (f, t) = (s.get(from), s.get(to));
                if f < amount {
                    return Err(Refused::Negative);
                }
                let ws = vec![put(Cell { id: from, v: f - amount }), put(Cell { id: to, v: t + amount })];
                Ok((ws, Reply { value: f - amount, now }))
            }
            Cmd::Get { cell } => Ok((Vec::new(), Reply { value: s.get(cell), now })),
            Cmd::BadRead { cell } => Ok((vec![put(Cell { id: cell, v: s.get(cell) + 1 })], Reply { value: s.get(cell), now })),
            Cmd::Emit { cell, dest } => {
                let v = s.get(cell) + 1;
                let payload = format!("{cell}:{v}").into_bytes();
                Ok((vec![put(Cell { id: cell, v }), W::Effect { dest, payload }], Reply { value: v, now }))
            }
        }
    }

    fn apply(s: &Snap, ws: &Vec<W>) -> Snap {
        let mut s = s.clone();
        for w in ws {
            if let W::Put(c) = w {
                match s.cells.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => x.v = c.v,
                    None => {
                        s.cells.push(*c);
                        s.cells.sort_by_key(|x| x.id);
                    }
                }
            }
        }
        s
    }
}

/// Where in an attempt the store calls the tenant's hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// In `load`, inside the transaction, before reading.
    Load,
    /// In `write`, before any write.
    Write,
    /// In `write`, after the cells are written, before the outbox.
    Written,
    /// After the outbox rows: the last statement before the idempotency
    /// reply, `h5i_clock` and COMMIT.
    Enqueued,
}

pub type HookFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
/// Called with the point and the attempt's backend pid. An `Err` fails the
/// store call with `DbError::Decode` (not retryable).
pub type Hook = Arc<dyn Fn(Point, i32) -> HookFuture + Send + Sync>;

static HOOKS: Mutex<Option<HashMap<u64, Hook>>> = Mutex::new(None);

pub fn set_hook(tenant: u64, hook: impl Fn(Point, i32) -> HookFuture + Send + Sync + 'static) {
    HOOKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(tenant, Arc::new(hook));
}

pub fn clear_hook(tenant: u64) {
    if let Some(m) = HOOKS.lock().unwrap().as_mut() {
        m.remove(&tenant);
    }
}

async fn fire(tx: &Tx<'_>, tenant: TenantId, point: Point, pid: Option<i32>) -> Result<(), DbError> {
    let hook = HOOKS.lock().unwrap().as_ref().and_then(|m| m.get(&tenant.0).cloned());
    let Some(hook) = hook else { return Ok(()) };
    let pid = match pid {
        Some(p) => p,
        None => tx.backend_pid().await?,
    };
    hook(point, pid).await.map_err(DbError::Decode)
}

pub fn schema() -> Vec<Spec> {
    let col = |name: &str| Column { name: name.as_bytes().to_vec(), kind: Kind::Int, nullable: false };
    vec![Spec { name: b"cells".to_vec(), columns: vec![col("id"), col("v")], key_len: 1 }]
}

fn int(v: &Val) -> Result<u64, DbError> {
    match v {
        Val::Int(i) => Ok(*i as u64),
        other => Err(DbError::Decode(format!("not an integer: {other:?}"))),
    }
}

pub struct BankStore;

impl Store<Bank> for BankStore {
    fn ddl() -> Vec<String> {
        h5i_app_pg::create_tables(&schema()).expect("valid schema")
    }

    fn tables() -> Vec<&'static str> {
        vec!["cells"]
    }

    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<Snap, DbError> {
        let info = tx.info().await?;
        let pid = info.pid;
        ATTEMPTS.lock().unwrap().get_or_insert_with(HashMap::new).entry(t.0).or_default().push(info);
        fire(tx, t, Point::Load, Some(pid)).await?;
        let rows = h5i_app_pg::load_table(tx, t, &schema(), 0, None).await?;
        let cells = rows.iter().map(|r| Ok(Cell { id: int(&r[0])?, v: int(&r[1])? })).collect::<Result<_, DbError>>()?;
        Ok(Snap { cells })
    }

    fn lock_for(_: &Actor, cmd: &Cmd) -> Lock {
        match cmd {
            Cmd::Move { from, to, .. } => Lock::Keys(vec![Lock::key("cells", &from.to_be_bytes()), Lock::key("cells", &to.to_be_bytes())]),
            _ => Lock::Tenant,
        }
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<W>) -> Result<(), DbError> {
        fire(tx, t, Point::Write, None).await?;
        let puts: Vec<SqlWrite> = ws
            .iter()
            .filter_map(|w| match w {
                W::Put(c) => Some(SqlWrite::Put { table: 0, key_len: 1, row: vec![Val::Int(c.id as i64), Val::Int(c.v as i64)] }),
                W::Effect { .. } => None,
            })
            .collect();
        h5i_app_pg::store_writes(tx, t, &schema(), &puts).await?;
        fire(tx, t, Point::Written, None).await?;
        for w in ws {
            if let W::Effect { dest, payload } = w {
                outbox::enqueue(tx, t, *dest, payload).await?;
            }
        }
        fire(tx, t, Point::Enqueued, None).await
    }
}

impl ReplyCodec<Bank> for BankStore {
    fn fingerprint(cmd: &Cmd) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(a: &Actor) -> String {
        format!("u{}", a.user)
    }

    fn encode(r: &Reply) -> Vec<u8> {
        [r.value.to_be_bytes(), r.now.to_be_bytes()].concat()
    }

    fn decode(b: &[u8]) -> Result<Reply, String> {
        if b.len() != 16 {
            return Err(format!("{} bytes", b.len()));
        }
        Ok(Reply { value: u64::from_be_bytes(b[..8].try_into().unwrap()), now: u64::from_be_bytes(b[8..].try_into().unwrap()) })
    }
}

pub type Pg = Engine<Bank, BankStore>;

/// A fresh tenant id, unique in this test binary (hooks and logs are per tenant).
pub fn tenant() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One test's database: its own schema, the engine URL (tagged with an
/// `application_name`, so its backends can be found) and an admin client.
pub struct Db {
    /// The engine URL: schema `name`, `application_name` `name`.
    pub url: String,
    pub name: String,
    pub admin: Arc<tokio_postgres::Client>,
    pub admin_url: String,
    pub base_url: String,
}

/// The test database, or `None` after printing "skipping" (`test_env`; with
/// `H5I_APP_TEST_REQUIRE_DB` set, as in CI, a missing URL panics instead).
pub async fn setup(name: &str) -> Option<Db> {
    let base_url = h5i_app_pg::test_env("H5I_APP_TEST_DATABASE_URL")?;
    // Terminating backends and reading every backend's locks needs a
    // superuser (or pg_signal_backend and pg_read_all_stats).
    let admin_url = h5i_app_pg::test_env("H5I_APP_TEST_ADMIN_URL")?;
    let admin = Arc::new(connect(&admin_url).await);
    admin.batch_execute(&format!("DROP SCHEMA IF EXISTS {name} CASCADE")).await.unwrap();
    let url = format!("{}&application_name={name}", h5i_app_pg::with_schema(&base_url, name).unwrap());
    Some(Db { url, name: name.to_string(), admin, admin_url, base_url })
}

pub async fn connect(url: &str) -> tokio_postgres::Client {
    let (c, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    c
}

impl Db {
    /// An engine on its own pool of `size` connections, schema installed.
    pub async fn engine(&self, size: usize, config: EngineConfig) -> Pg {
        self.engine_at(&self.url, size, config).await
    }

    pub async fn engine_at(&self, url: &str, size: usize, config: EngineConfig) -> Pg {
        let e = Pg::new(h5i_app_pg::pool(url, size).unwrap(), config);
        e.install_schema().await.unwrap();
        e
    }

    /// Committed cells of `tenant`, read directly.
    pub async fn cells(&self, tenant: u64) -> Snap {
        let rows = self
            .admin
            .query(&format!("SELECT id, v FROM {}.cells WHERE tenant_id = $1 ORDER BY id", self.name), &[&(tenant as i64)])
            .await
            .unwrap();
        Snap { cells: rows.iter().map(|r| Cell { id: r.get::<_, i64>(0) as u64, v: r.get::<_, i64>(1) as u64 }).collect() }
    }

    /// Rows of `table` (one of ours or a framework table) for `tenant`.
    pub async fn count(&self, table: &str, tenant: u64) -> i64 {
        self.admin
            .query_one(&format!("SELECT count(*) FROM {}.{table} WHERE tenant_id = $1", self.name), &[&(tenant as i64)])
            .await
            .unwrap()
            .get(0)
    }

    /// Advisory locks held (granted) by this test's engine backends.
    pub async fn held_advisory_locks(&self) -> i64 {
        self.admin
            .query_one(
                "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                 WHERE l.locktype = 'advisory' AND l.granted AND a.application_name = $1",
                &[&self.name],
            )
            .await
            .unwrap()
            .get(0)
    }

    /// Advisory locks held by one backend.
    pub async fn advisory_locks_of(&self, pid: i32) -> Vec<(String, bool)> {
        self.admin
            .query("SELECT mode, granted FROM pg_locks WHERE locktype = 'advisory' AND pid = $1 ORDER BY mode", &[&pid])
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect()
    }

    /// Waits until no engine backend of this test holds an advisory lock
    /// (closing a connection releases its locks asynchronously).
    pub async fn wait_no_advisory_locks(&self, within: Duration) -> i64 {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let n = self.held_advisory_locks().await;
            if n == 0 || tokio::time::Instant::now() >= deadline {
                return n;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// `pg_terminate_backend`, then wait until the backend is gone.
pub async fn kill(admin: &tokio_postgres::Client, pid: i32) {
    let ok: bool = admin.query_one("SELECT pg_terminate_backend($1)", &[&pid]).await.unwrap().get(0);
    assert!(ok, "could not terminate backend {pid}");
    for _ in 0..500 {
        let n: i64 = admin.query_one("SELECT count(*) FROM pg_stat_activity WHERE pid = $1", &[&pid]).await.unwrap().get(0);
        if n == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("backend {pid} still running");
}

/// What the proxy does with the next COMMIT it sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Close both sides without forwarding COMMIT: the server rolls back.
    DropBefore,
    /// Forward COMMIT, wait for the server's answer, then close both sides
    /// without forwarding it: the client cannot know it committed.
    DropAfter,
}

/// What the server answered a COMMIT the proxy kept from the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not forwarded.
    NotSent,
    /// CommandComplete: committed.
    Committed,
    /// ErrorResponse: not committed.
    Failed,
}

#[derive(Default)]
struct Plan {
    faults: VecDeque<Fault>,
    outcomes: Vec<Outcome>,
}

/// A TCP proxy in front of PostgreSQL that can lose the next COMMIT. It
/// reads the frontend protocol (startup message, then typed messages) and
/// acts on a simple-query `COMMIT`, which is how tokio-postgres commits.
pub struct Proxy {
    pub port: u16,
    plan: Arc<Mutex<Plan>>,
}

impl Proxy {
    pub async fn start(database_url: &str) -> Proxy {
        let cfg: tokio_postgres::Config = database_url.parse().unwrap();
        let host = match &cfg.get_hosts()[0] {
            tokio_postgres::config::Host::Tcp(h) => h.clone(),
            #[allow(unreachable_patterns)]
            other => panic!("the proxy needs a TCP host, not {other:?}"),
        };
        let port = cfg.get_ports().first().copied().unwrap_or(5432);
        let upstream = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let me = Proxy { port: listener.local_addr().unwrap().port(), plan: Arc::default() };
        let plan = me.plan.clone();
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else { return };
                let (upstream, plan) = (upstream.clone(), plan.clone());
                tokio::spawn(async move {
                    if let Ok(server) = tokio::net::TcpStream::connect(&upstream).await {
                        relay(client, server, plan).await;
                    }
                });
            }
        });
        me
    }

    /// Lose the next COMMIT that passes the proxy.
    pub fn arm(&self, f: Fault) {
        self.plan.lock().unwrap().faults.push_back(f);
    }

    pub fn armed(&self) -> usize {
        self.plan.lock().unwrap().faults.len()
    }

    pub fn outcomes(&self) -> Vec<Outcome> {
        self.plan.lock().unwrap().outcomes.clone()
    }

    /// `url` with its host and port replaced by the proxy's.
    pub fn url(&self, url: &str) -> String {
        let start = url.find("://").map_or(0, |i| i + 3);
        let end = url[start..].find(['/', '?']).map_or(url.len(), |i| start + i);
        let at = url[start..end].rfind('@').map_or(start, |i| start + i + 1);
        format!("{}127.0.0.1:{}{}", &url[..at], self.port, &url[end..])
    }
}

/// Next whole message in `buf`: the untyped startup message first, then
/// `type, length, body` messages.
fn next_message(buf: &mut Vec<u8>, typed: bool) -> Option<Vec<u8>> {
    let (head, len_at) = if typed { (5, 1) } else { (4, 0) };
    if buf.len() < head {
        return None;
    }
    let len = u32::from_be_bytes(buf[len_at..len_at + 4].try_into().unwrap()) as usize;
    let total = len + len_at;
    if buf.len() < total {
        return None;
    }
    Some(buf.drain(..total).collect())
}

async fn relay(client: tokio::net::TcpStream, server: tokio::net::TcpStream, plan: Arc<Mutex<Plan>>) {
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (mut cr, mut cw) = client.into_split();
    let (mut sr, mut sw) = server.into_split();
    let (mut from_client, mut from_server) = (Vec::new(), Vec::new());
    let (mut started, mut swallowing, mut committed) = (false, false, false);
    let (mut a, mut b) = (vec![0u8; 16384], vec![0u8; 16384]);
    loop {
        tokio::select! {
            n = cr.read(&mut a) => {
                let Ok(n) = n else { return };
                if n == 0 { return }
                from_client.extend_from_slice(&a[..n]);
                while let Some(m) = next_message(&mut from_client, started) {
                    started = true;
                    if m[0] == b'Q' && &m[5..] == b"COMMIT\0" {
                        let fault = plan.lock().unwrap().faults.pop_front();
                        match fault {
                            Some(Fault::DropBefore) => {
                                plan.lock().unwrap().outcomes.push(Outcome::NotSent);
                                return;
                            }
                            Some(Fault::DropAfter) => swallowing = true,
                            None => {}
                        }
                    }
                    if sw.write_all(&m).await.is_err() { return }
                }
            }
            n = sr.read(&mut b) => {
                let Ok(n) = n else { return };
                if n == 0 { return }
                if !swallowing {
                    if cw.write_all(&b[..n]).await.is_err() { return }
                    continue;
                }
                from_server.extend_from_slice(&b[..n]);
                while let Some(m) = next_message(&mut from_server, true) {
                    // Replies to messages sent before COMMIT may come first
                    // (tokio-postgres closes statements without waiting).
                    // COMMIT ends with ReadyForQuery `I`: outside a transaction.
                    if m[0] == b'C' && m[5..].starts_with(b"COMMIT") {
                        committed = true;
                    }
                    if m[0] == b'Z' && m.get(5) == Some(&b'I') {
                        plan.lock().unwrap().outcomes.push(if committed { Outcome::Committed } else { Outcome::Failed });
                        return;
                    }
                }
            }
        }
    }
}

/// `EngineConfig` with the given attempts and locking.
pub fn config(max_attempts: u32, tenant_lock: bool) -> EngineConfig {
    EngineConfig { max_attempts, tenant_lock, ..EngineConfig::default() }
}

/// A hook that does `f` the first `times` times it reaches `at`.
pub fn at_first<F, Fut>(at: Point, times: u32, f: F) -> impl Fn(Point, i32) -> HookFuture + Send + Sync + 'static
where
    F: Fn(i32) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let left = Arc::new(std::sync::atomic::AtomicU32::new(times));
    let f = Arc::new(f);
    move |p, pid| {
        let (left, f) = (left.clone(), f.clone());
        Box::pin(async move {
            if p == at && take_one(&left) {
                f(pid).await
            } else {
                Ok(())
            }
        })
    }
}

/// Decrements `n` unless it is zero; whether it did.
pub fn take_one(n: &std::sync::atomic::AtomicU32) -> bool {
    let mut cur = n.load(Ordering::SeqCst);
    while cur > 0 {
        match n.compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}
