//! Load test: seeds a RealWorld-sized site (50 users, 10k articles, 20k tag
//! rows, 5k favorites, 500 follows) with plain SQL, serves the API in this
//! process, and drives it over HTTP at several concurrencies.
//!
//! ```text
//! DATABASE_URL=postgres://h5i:h5i@127.0.0.1:5460/h5i \
//!   cargo run --release -p conduit-server --example load -- [secs] [scenarios] [concurrencies]
//! ```
//!
//! Prints one line per (scenario, concurrency): requests per second, latency
//! percentiles, non-2xx replies, and the engine's attempts, retries and
//! commits during the run (`EngineStats`).

use conduit_server::api::{router, App, ConduitAuth};
use conduit_server::{Conduit, ConduitStore, TENANT};
use h5i_app_pg::{pool, Engine};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const USERS: u64 = 50;
const ARTICLES: u64 = 10_000;
const TAGS_PER_ARTICLE: u64 = 2;
const TAG_NAMES: u64 = 100;
const FAVORITES: u64 = 5_000;
const FOLLOWS_PER_USER: u64 = 10;
const SECRET: &str = "load-test-secret";

fn seed_sql() -> String {
    let t = TENANT;
    format!(
        "TRUNCATE conduit.conduit_users, conduit.conduit_follows, conduit.conduit_articles, conduit.conduit_tags,
                  conduit.conduit_favorites, conduit.conduit_comments, conduit.conduit_counters;
         INSERT INTO conduit.conduit_users
           SELECT {t}, i, convert_to('user' || i, 'UTF8'), convert_to('user' || i || '@example.com', 'UTF8'),
                  convert_to('pw', 'UTF8'), convert_to('bio of user ' || i, 'UTF8'), ''::bytea
           FROM generate_series(1, {USERS}) i;
         INSERT INTO conduit.conduit_follows
           SELECT {t}, u, ((u - 1 + d) % {USERS}) + 1 FROM generate_series(1, {USERS}) u, generate_series(1, {FOLLOWS_PER_USER}) d;
         INSERT INTO conduit.conduit_articles
           SELECT {t}, i, ((i - 1) % {USERS}) + 1, convert_to('article-' || i, 'UTF8'), convert_to('Article ' || i, 'UTF8'),
                  convert_to('Description of article ' || i, 'UTF8'),
                  convert_to(repeat('Lorem ipsum dolor sit amet, consectetur adipiscing elit. ', 8) || i, 'UTF8'),
                  1700000000 + i, 1700000000 + i
           FROM generate_series(1, {ARTICLES}) i;
         INSERT INTO conduit.conduit_tags
           SELECT {t}, i, convert_to('tag' || (((i * 7) + d * 31) % {TAG_NAMES}), 'UTF8')
           FROM generate_series(1, {ARTICLES}) i, generate_series(0, {TAGS_PER_ARTICLE} - 1) d;
         INSERT INTO conduit.conduit_counters VALUES ({t}, {USERS}, {ARTICLES}, 0);"
    )
}

fn reset_favorites_sql() -> String {
    let t = TENANT;
    format!(
        "DELETE FROM conduit.conduit_favorites;
         INSERT INTO conduit.conduit_favorites
           SELECT {t}, ((k * 2) % {ARTICLES}) + 1, (k % {USERS}) + 1 FROM generate_series(0, {FAVORITES} - 1) k;
         ANALYZE;"
    )
}

/// Tiny deterministic generator, one per client.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    GetArticle,
    List,
    ListTag,
    Feed,
    Favorite,
}

impl Scenario {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "get-article" => Scenario::GetArticle,
            "list" => Scenario::List,
            "list-tag" => Scenario::ListTag,
            "feed" => Scenario::Feed,
            "favorite" => Scenario::Favorite,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Scenario::GetArticle => "get-article",
            Scenario::List => "list",
            Scenario::ListTag => "list-tag",
            Scenario::Feed => "feed",
            Scenario::Favorite => "favorite",
        }
    }

    fn request(self, rng: &mut Rng, tokens: &[String]) -> String {
        let user = rng.below(USERS) as usize;
        let auth = format!("Authorization: Token {}\r\n", tokens[user]);
        let (method, path) = match self {
            Scenario::GetArticle => ("GET", format!("/api/articles/article-{}", 1 + rng.below(ARTICLES))),
            Scenario::List => ("GET", "/api/articles?limit=20&offset=0".to_string()),
            Scenario::ListTag => ("GET", format!("/api/articles?tag=tag{}&limit=20", rng.below(TAG_NAMES))),
            Scenario::Feed => ("GET", "/api/articles/feed?limit=20&offset=0".to_string()),
            Scenario::Favorite => ("POST", format!("/api/articles/article-{}/favorite", 1 + rng.below(ARTICLES))),
        };
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: 0\r\n\r\n")
    }
}

/// Sends one request on a keep-alive connection; returns the status code.
async fn roundtrip(conn: &mut TcpStream, req: &str, buf: &mut Vec<u8>) -> std::io::Result<u16> {
    conn.write_all(req.as_bytes()).await?;
    buf.clear();
    let mut chunk = [0u8; 65536];
    let header_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("connection closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let status: u16 = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let len: usize = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .ok_or_else(|| std::io::Error::other("no content-length"))?;
    while buf.len() < header_end + len {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("connection closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(status)
}

struct Outcome {
    latencies: Vec<Duration>,
    errors: u64,
}

async fn client(addr: String, scenario: Scenario, seed: u64, tokens: Arc<Vec<String>>, stop: Arc<AtomicBool>) -> Outcome {
    let mut rng = Rng(seed);
    let mut out = Outcome { latencies: Vec::new(), errors: 0 };
    let mut conn = TcpStream::connect(&addr).await.expect("connect");
    conn.set_nodelay(true).ok();
    let mut buf = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let req = scenario.request(&mut rng, &tokens);
        let t = Instant::now();
        match roundtrip(&mut conn, &req, &mut buf).await {
            Ok(s) if (200..300).contains(&s) => out.latencies.push(t.elapsed()),
            Ok(_) => {
                out.errors += 1;
                out.latencies.push(t.elapsed());
            }
            Err(_) => {
                out.errors += 1;
                conn = TcpStream::connect(&addr).await.expect("reconnect");
                conn.set_nodelay(true).ok();
            }
        }
    }
    out
}

fn pct(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i].as_secs_f64() * 1000.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let scenarios: Vec<Scenario> = args
        .get(2)
        .map(|s| s.split(',').filter_map(Scenario::parse).collect())
        .unwrap_or_else(|| vec![Scenario::GetArticle, Scenario::List, Scenario::ListTag, Scenario::Feed, Scenario::Favorite]);
    let concs: Vec<usize> =
        args.get(3).map(|s| s.split(',').filter_map(|c| c.parse().ok()).collect()).unwrap_or_else(|| vec![1, 16, 64]);
    let url = std::env::var("DATABASE_URL")?;
    let pool_size: usize = std::env::var("POOL_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(32);

    // The server gets its own runtime, so clients do not steal its threads.
    let server_rt = tokio::runtime::Builder::new_multi_thread().worker_threads(16).enable_all().build()?;
    let client_rt = tokio::runtime::Builder::new_multi_thread().worker_threads(8).enable_all().build()?;

    let engine = Arc::new(Engine::<Conduit, ConduitStore>::new(
        pool(&h5i_app_pg::with_schema(&url, "conduit")?, pool_size)?,
        conduit_server::engine_config(),
    ));
    let (addr, auth) = server_rt.block_on(async {
        engine.install_schema().await?;
        let (db, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
        tokio::spawn(conn);
        db.batch_execute(&seed_sql()).await?;
        db.batch_execute(&reset_favorites_sql()).await?;
        let auth = Arc::new(ConduitAuth::new(SECRET));
        let app = App { engine: engine.clone(), auth: auth.clone(), secret: Arc::new(SECRET.as_bytes().to_vec()) };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?.to_string();
        tokio::spawn(async move { axum::serve(listener, router(app)).await });
        Ok::<_, Box<dyn std::error::Error>>((addr, auth))
    })?;
    let tokens: Arc<Vec<String>> = Arc::new((1..=USERS).map(|u| auth.issue(u)).collect());

    println!("| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for &sc in &scenarios {
        for &c in &concs {
            if sc == Scenario::Favorite {
                server_rt.block_on(async {
                    let (db, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
                    tokio::spawn(conn);
                    db.batch_execute(&reset_favorites_sql()).await
                })?;
            }
            let before = (
                engine.stats.attempts.load(Ordering::Relaxed),
                engine.stats.retries.load(Ordering::Relaxed),
                engine.stats.commits.load(Ordering::Relaxed),
            );
            let stop = Arc::new(AtomicBool::new(false));
            let start = Instant::now();
            let outcomes = client_rt.block_on(async {
                let handles: Vec<_> = (0..c)
                    .map(|i| tokio::spawn(client(addr.clone(), sc, 1 + i as u64 * 7919, tokens.clone(), stop.clone())))
                    .collect();
                tokio::time::sleep(Duration::from_secs(secs)).await;
                stop.store(true, Ordering::Relaxed);
                let mut outs = Vec::new();
                for h in handles {
                    outs.push(h.await.expect("client"));
                }
                outs
            });
            let elapsed = start.elapsed().as_secs_f64();
            let mut lat: Vec<Duration> = outcomes.iter().flat_map(|o| o.latencies.iter().copied()).collect();
            lat.sort();
            let errors: u64 = outcomes.iter().map(|o| o.errors).sum();
            let after = (
                engine.stats.attempts.load(Ordering::Relaxed),
                engine.stats.retries.load(Ordering::Relaxed),
                engine.stats.commits.load(Ordering::Relaxed),
            );
            println!(
                "| {} | {c} | {:.1} | {:.2} | {:.2} | {errors} | {} | {} | {} |",
                sc.name(),
                lat.len() as f64 / elapsed,
                pct(&lat, 0.5),
                pct(&lat, 0.99),
                after.0 - before.0,
                after.1 - before.1,
                after.2 - before.2,
            );
        }
    }
    Ok(())
}
