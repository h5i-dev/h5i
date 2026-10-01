//! Random commands give the same replies and state on PostgreSQL as on
//! `transition` + `apply` in memory, under a clock that jumps back and forth.
//! Then a burn-after-reading paste goes through the HTTP routes.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use h5i_app_core::{Kernel, Timestamp};
use h5i_app_pg::{pool, Clock, EngineConfig, ManualClock};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wastebin_kernel as k;
use wastebin_server::{router, App, Shell, Wastebin, WastebinEngine, TENANT};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.pastes.sort_by_key(|p| p.id);
    s
}

// Both tests use the single Wastebin tenant, so they must not interleave.
static DB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn engine(url: &str, config: EngineConfig) -> WastebinEngine {
    let pg = WastebinEngine::new(pool(&h5i_app_pg::with_schema(url, "wastebin").unwrap(), 4).unwrap(), config);
    pg.install_schema().await.unwrap();
    pg
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let _guard = DB.lock().await;
    // Not monotonic: the time jumps around, to reach expiry from both sides.
    let clock = ManualClock::default();
    let pg = engine(&url, EngineConfig { clock: Clock::Manual(clock.clone()), ..Default::default() }).await;
    // Earlier runs leave pastes behind; the model starts where the database is.
    let mut mem = pg.snapshot(TENANT).await.unwrap();

    let mut s = 7;
    let (mut shown, mut burned, mut gone, mut refused) = (0, 0, 0, 0);
    for _ in 0..500 {
        let uids = match rng(&mut s, 4) {
            0 => vec![],
            1 => vec![1],
            2 => vec![2],
            _ => vec![2, 1],
        };
        let now = Timestamp::from_secs(100 + rng(&mut s, 20));
        clock.set(now);
        let actor = k::Principal { uids, now: 0, fresh: 1_000 + rng(&mut s, 8) };
        let mut stamped = actor.clone();
        Wastebin::stamp(&mut stamped, now);
        let key = |s: &mut u64| match rng(s, 3) {
            0 => None,
            n => Some(n),
        };
        let slug = 1_000 + rng(&mut s, 8);
        let cmd = match rng(&mut s, 6) {
            0 | 1 => k::Command::Create {
                text: format!("paste {}", rng(&mut s, 100)).into_bytes(),
                expires_in: match rng(&mut s, 3) {
                    0 => None,
                    1 => Some(rng(&mut s, 2) as u32),
                    _ => Some(1 + rng(&mut s, 15) as u32),
                },
                burn: rng(&mut s, 3) == 0,
                lock: key(&mut s),
            },
            2 => k::Command::View { slug, confirm: rng(&mut s, 2) == 0, key: key(&mut s) },
            3 => k::Command::Fetch { slug, key: key(&mut s) },
            4 => k::Command::Delete { slug },
            _ => k::Command::Purge,
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = match k::transition(&stamped, &mem, &cmd) {
            Ok((ws, r)) => {
                mem = k::apply(&mem, &ws);
                Ok(r)
            }
            Err(e) => Err(e),
        };
        assert_eq!(got, want, "{cmd:?}");
        match &got {
            Ok(k::Reply::Shown(v)) if v.burned => burned += 1,
            Ok(k::Reply::Shown(_)) => shown += 1,
            Ok(k::Reply::Gone) => gone += 1,
            Err(_) => refused += 1,
            _ => {}
        }
    }
    assert!(shown > 0 && burned > 0 && gone > 0 && refused > 0, "{shown} {burned} {gone} {refused}");
    let a = sorted(pg.snapshot(TENANT).await.unwrap());
    assert_eq!(a, sorted(mem));
}

/// One HTTP/1.1 request over a fresh connection; returns status and body.
async fn http(addr: &str, req: &str) -> (u16, String, String) {
    let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
    c.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    c.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text[9..12].parse().unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (status, head.to_string(), body.to_string())
}

fn get(path: &str, extra: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}\r\n")
}

#[tokio::test]
async fn link_preview_does_not_burn() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let _guard = DB.lock().await;
    let engine = Arc::new(engine(&url, wastebin_server::config()).await);
    let app = App { engine, shell: Arc::new(Shell::new(b"test key".to_vec())) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });

    let body = r#"{"text":"secret-body-xyz","burn_after_reading":true}"#;
    let req = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (status, head, created) = http(&addr, &req).await;
    assert_eq!(status, 200, "{created}");
    let v: serde_json::Value = serde_json::from_str(&created).unwrap();
    let path = v["path"].as_str().unwrap().to_string();
    let cookie = head.lines().find_map(|l| l.strip_prefix("set-cookie: ")).unwrap();
    let cookie = cookie.split(';').next().unwrap().to_string();

    // A preview bot fetches the page twice; it only gets the confirmation page.
    for _ in 0..2 {
        let (status, _, body) = http(&addr, &get(&path, "")).await;
        assert_eq!(status, 200);
        assert!(body.contains("confirm_burn") && !body.contains("secret-body-xyz"), "{body}");
    }
    // The reader confirms and sees it once.
    let (status, _, body) = http(&addr, &get(&format!("{path}?confirm_burn=1"), "")).await;
    assert_eq!(status, 200);
    assert!(body.contains("secret-body-xyz") && body.contains("\"burned\":true"), "{body}");
    let (status, _, _) = http(&addr, &get(&format!("{path}?confirm_burn=1"), "")).await;
    assert_eq!(status, 404);

    // An ordinary paste: the owner's cookie may delete it, a stranger may not.
    let body = r#"{"text":"plain"}"#;
    let req = |extra: &str| {
        format!(
            "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let (_, _, created) = http(&addr, &req(&format!("Cookie: {cookie}\r\n"))).await;
    let v: serde_json::Value = serde_json::from_str(&created).unwrap();
    let path = v["path"].as_str().unwrap().to_string();
    let del = |extra: &str| format!("DELETE {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}\r\n");
    assert_eq!(http(&addr, &del("")).await.0, 403);
    assert_eq!(http(&addr, &del("Cookie: uid=1.forged\r\n")).await.0, 403);
    assert_eq!(http(&addr, &get(&format!("/raw{path}"), "")).await.0, 200);
    assert_eq!(http(&addr, &del(&format!("Cookie: {cookie}\r\n"))).await.0, 200);
    assert_eq!(http(&addr, &get(&path, "")).await.0, 404);
}
