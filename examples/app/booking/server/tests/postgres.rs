//! On random commands, the PostgreSQL engine and `MemoryEngine` give the same
//! replies and final state, and the outbox holds exactly the committed
//! notifications. Both use a test clock that moves forward.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use booking_kernel as k;
use booking_server::{effect_payload, BookingApp, BookingStore};
use h5i_app_core::{Kernel, MemoryEngine, TenantId, Timestamp};
use h5i_app_pg::{pool, Clock, Engine, EngineConfig, ManualClock};
use tokio_postgres::NoTls;

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.admins.sort_by_key(|a| a.user);
    s.rooms.sort_by_key(|r| r.id);
    s.bookings.sort_by_key(|b| b.id);
    s
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let clock = ManualClock::default();
    let config = EngineConfig { clock: Clock::Manual(clock.clone()), monotonic: true, ..Default::default() };
    let pg = Engine::<BookingApp, BookingStore>::new(pool(&h5i_app_pg::with_schema(&url, "booking").unwrap(), 4).unwrap(), config);
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<BookingApp>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 5;
    let mut expected = Vec::new();
    for i in 0..500u64 {
        // The clock moves forward as the test runs.
        let now = i / 4;
        clock.set(Timestamp::from_secs(now));
        let actor = k::Principal { org, user: 1 + rng(&mut s, 4), now: 0 };
        let cmd = match rng(&mut s, 8) {
            0 => k::Command::AddAdmin { user: 1 + rng(&mut s, 4) },
            1 => k::Command::CreateRoom { dest: 7 + rng(&mut s, 2) },
            2..=4 => {
                let start_at = (now + rng(&mut s, 40)).saturating_sub(5);
                k::Command::Book { room: rng(&mut s, 4), start_at, end_at: start_at + rng(&mut s, 6) }
            }
            5 | 6 => k::Command::Cancel { id: rng(&mut s, 40) },
            _ => k::Command::List,
        };
        // The notifications this command commits, if it succeeds.
        let mut stamped = actor;
        BookingApp::stamp(&mut stamped, Timestamp::from_secs(now));
        if let Ok((ws, _)) = k::transition(&stamped, &mem.snapshot(TenantId(org)), &cmd) {
            for w in &ws {
                if let k::Write::Emit(e) = w {
                    expected.push((e.dest as i64, effect_payload(e)));
                }
            }
        }
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute_at(&actor, Timestamp::from_secs(now), &cmd);
        match (&got, &want) {
            // Row order in a listing is not meaningful.
            (Ok(k::Reply::Bookings(a)), Ok(k::Reply::Bookings(b))) => {
                let (mut a, mut b) = (a.clone(), b.clone());
                a.sort_by_key(|x| x.id);
                b.sort_by_key(|x| x.id);
                assert_eq!(a, b, "{cmd:?}");
            }
            _ => assert_eq!(got, want, "{cmd:?}"),
        }
    }
    let a = sorted(pg.snapshot(TenantId(org)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(org)));
    assert_eq!(a, b);
    assert!(expected.len() > 20, "the run books and cancels");

    let (c, conn) = tokio_postgres::connect(&h5i_app_pg::with_schema(&url, "booking").unwrap(), NoTls).await.unwrap();
    tokio::spawn(conn);
    let rows: Vec<(i64, Vec<u8>)> = c
        .query("SELECT dest, payload FROM h5i_outbox WHERE tenant_id = $1 ORDER BY id", &[&(org as i64)])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(rows, expected);
}

/// The run of `Clock.clock_back_cancels`, on the real engine: a booking that
/// started before the last commit, and an owner whose clock reads earlier.
async fn cancel_after_clock_went_back(url: &str, monotonic: bool) -> Result<k::Reply, k::Error> {
    let clock = ManualClock::new(Timestamp::from_secs(100));
    let config = EngineConfig { clock: Clock::Manual(clock.clone()), monotonic, ..Default::default() };
    let pg = Engine::<BookingApp, BookingStore>::new(pool(&h5i_app_pg::with_schema(url, "booking").unwrap(), 2).unwrap(), config);
    pg.install_schema().await.unwrap();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);
    let who = |user| k::Principal { org, user, now: 0 };
    pg.execute(&who(1), &k::Command::AddAdmin { user: 1 }).await.unwrap().unwrap();
    pg.execute(&who(1), &k::Command::CreateRoom { dest: 7 }).await.unwrap().unwrap();
    let Ok(k::Reply::Created(id)) = pg.execute(&who(2), &k::Command::Book { room: 0, start_at: 150, end_at: 200 }).await.unwrap()
    else {
        panic!("booking refused")
    };
    // A commit at 160, after the booking started.
    clock.set(Timestamp::from_secs(160));
    pg.execute(&who(3), &k::Command::List).await.unwrap().unwrap();
    // The clock goes back to 120.
    clock.set(Timestamp::from_secs(120));
    pg.execute(&who(2), &k::Command::Cancel { id }).await.unwrap()
}

#[tokio::test]
async fn monotonic_time_keeps_started_bookings() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    assert_eq!(cancel_after_clock_went_back(&url, false).await, Ok(k::Reply::Done));
    assert_eq!(cancel_after_clock_went_back(&url, true).await, Err(k::Error::Started));
}
