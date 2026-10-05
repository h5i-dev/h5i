//! Notifications reach the room's endpoint over real HTTP. The dispatcher
//! claims every tenant's rows, so these tests use their own schema and run
//! one at a time.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use booking_kernel as k;
use booking_server::{effect_payload, parse_registry, BookingApp, BookingStore, Endpoint, Notifier};
use h5i_app_pg::outbox::DispatchConfig;
use h5i_app_pg::{pool, Clock, Engine, EngineConfig, Timestamp};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_postgres::NoTls;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type Pg = Engine<BookingApp, BookingStore>;

/// The test database, in its own schema so no other dispatcher sees its outbox.
fn database() -> Option<String> {
    let url = std::env::var("H5I_APP_TEST_DATABASE_URL").ok()?;
    Some(h5i_app_pg::with_schema(&url, "booking_outbox").unwrap())
}

/// Idempotency-Key and body of one POST.
type Post = (String, Vec<u8>);

/// Records one tenant's POSTs as (Idempotency-Key, body); can fail the first.
#[derive(Clone, Default)]
struct Receiver {
    org: u64,
    fail_first: bool,
    seen: Arc<Mutex<Vec<Post>>>,
}

impl Receiver {
    /// Serves `POST /hooks` on a free port and returns the port.
    async fn start(&self) -> u16 {
        let me = self.clone();
        let app = Router::new().route(
            "/hooks",
            post(move |h: HeaderMap, body: axum::body::Bytes| async move {
                let key = h.get("idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                // Keys are `<tenant>-<row>`; rows of earlier runs are acknowledged and ignored.
                if !key.starts_with(&format!("{}-", me.org)) {
                    return StatusCode::OK;
                }
                let mut seen = me.seen.lock().unwrap();
                seen.push((key, body.to_vec()));
                if me.fail_first && seen.len() == 1 {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    fn seen(&self) -> Vec<Post> {
        self.seen.lock().unwrap().clone()
    }
}

fn fresh_org() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50)
}

async fn run(pg: &Pg, org: u64, user: u64, cmd: k::Command) -> Result<k::Reply, k::Error> {
    pg.execute(&k::Principal { org, user, now: 0 }, &cmd).await.unwrap()
}

/// A room with destination `dest`, created by admin 1.
async fn room(pg: &Pg, org: u64, dest: u64) -> u64 {
    run(pg, org, 1, k::Command::AddAdmin { user: 1 }).await.unwrap();
    let Ok(k::Reply::Created(id)) = run(pg, org, 1, k::Command::CreateRoom { dest }).await else { panic!() };
    id
}

/// (delivered, dead, attempts) of each outbox row of `org`.
async fn rows(url: &str, org: u64) -> Vec<(bool, bool, i32)> {
    let (c, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(conn);
    c.query("SELECT delivered_at IS NOT NULL, dead, attempts FROM h5i_outbox WHERE tenant_id = $1 ORDER BY id", &[&(org as i64)])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

async fn setup() -> Option<(String, Pg)> {
    let url = database()?;
    // Every request runs at time 1000.
    let pg = Pg::new(pool(&url, 4).unwrap(), EngineConfig { clock: Clock::Fixed(Timestamp::from_secs(1000)), ..Default::default() });
    pg.install_schema().await.unwrap();
    Some((url, pg))
}

macro_rules! setup_or_skip {
    () => {
        match setup().await {
            Some(v) => v,
            None => {
                eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
                return;
            }
        }
    };
}

fn booked(b: k::Booking, dest: u64) -> Vec<u8> {
    effect_payload(&k::Effect { dest, event: k::Event::Booked, booking: b })
}

fn registry(port: u16) -> HashMap<u64, Endpoint> {
    parse_registry(&format!("7=http://127.0.0.1:{port}/hooks,8=log")).unwrap()
}

#[tokio::test]
async fn bookings_and_cancellations_are_posted_to_the_room_endpoint() {
    let _serial = SERIAL.lock().await;
    let (url, pg) = setup_or_skip!();
    let org = fresh_org();
    let r = room(&pg, org, 7).await;

    let Ok(k::Reply::Created(id)) = run(&pg, org, 2, k::Command::Book { room: r, start_at: 2000, end_at: 3000 }).await else {
        panic!()
    };
    // Refused commands commit nothing, so they notify nobody.
    assert_eq!(run(&pg, org, 3, k::Command::Book { room: r, start_at: 2500, end_at: 3500 }).await, Err(k::Error::Taken));
    assert_eq!(run(&pg, org, 3, k::Command::Cancel { id }).await, Err(k::Error::Forbidden));
    assert_eq!(run(&pg, org, 2, k::Command::Cancel { id }).await, Ok(k::Reply::Done));
    assert_eq!(rows(&url, org).await, vec![(false, false, 0), (false, false, 0)]);

    let rx = Receiver { org, ..Default::default() };
    let disp = pg.dispatcher(registry(rx.start().await), Notifier::default(), DispatchConfig::default());
    disp.run_once().await.unwrap();
    disp.run_once().await.unwrap();

    let b = k::Booking { id, room: r, user: 2, start_at: 2000, end_at: 3000 };
    let cancelled = effect_payload(&k::Effect { dest: 7, event: k::Event::Cancelled, booking: b });
    // A pass sends in commit order.
    let bodies: Vec<Vec<u8>> = rx.seen().into_iter().map(|(_, body)| body).collect();
    assert_eq!(bodies, vec![booked(b, 7), cancelled]);
    assert_eq!(rows(&url, org).await, vec![(true, false, 1), (true, false, 1)]);
}

#[tokio::test]
async fn a_failed_post_is_retried_with_the_same_key() {
    let _serial = SERIAL.lock().await;
    let (url, pg) = setup_or_skip!();
    let org = fresh_org();
    let r = room(&pg, org, 7).await;
    run(&pg, org, 2, k::Command::Book { room: r, start_at: 2000, end_at: 3000 }).await.unwrap();

    let rx = Receiver { org, fail_first: true, ..Default::default() };
    let disp = pg.dispatcher(registry(rx.start().await), Notifier::default(), DispatchConfig::default());
    disp.run_once().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    disp.run_once().await.unwrap();

    let seen = rx.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1], "same key and body, so the receiver can drop the repeat");
    assert_eq!(rows(&url, org).await, vec![(true, false, 2)]);
}

#[tokio::test]
async fn an_unregistered_destination_is_never_contacted() {
    let _serial = SERIAL.lock().await;
    let (url, pg) = setup_or_skip!();
    let org = fresh_org();
    let r = room(&pg, org, 99).await;
    run(&pg, org, 2, k::Command::Book { room: r, start_at: 2000, end_at: 3000 }).await.unwrap();

    let rx = Receiver { org, ..Default::default() };
    pg.dispatcher(registry(rx.start().await), Notifier::default(), DispatchConfig::default()).run_once().await.unwrap();
    assert!(rx.seen().is_empty());
    assert_eq!(rows(&url, org).await, vec![(false, true, 1)]);
}

#[test]
fn registry_parses() {
    let reg = parse_registry("7=log, 8=http://hooks.internal:9000/rooms/a").unwrap();
    assert_eq!(reg[&7], Endpoint::Log);
    assert_eq!(reg[&8], Endpoint::Http { host: "hooks.internal".into(), port: 9000, path: "/rooms/a".into() });
    assert!(parse_registry("7=https://example.com").is_err());
}
