//! On random commands, the PostgreSQL engine and `MemoryEngine` (which runs
//! `apply`) give the same replies and final state.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use board_kernel as k;
use board_server::{Board, BoardStore};
use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{pool, Engine, EngineConfig};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.posts.sort_by_key(|p| p.id);
    s.moderators.sort_by_key(|m| m.user);
    s
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Board, BoardStore>::new(pool(&h5i_app_pg::with_schema(&url, "board").unwrap(), 4).unwrap(), EngineConfig::default());
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Board>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 11;
    for _ in 0..400 {
        let actor = k::Principal { org, user: 1 + rng(&mut s, 4) };
        let text = |s: &mut u64| match rng(s, 5) {
            0 => Vec::new(),
            1 => vec![b'x'; 281],
            _ => format!("post {}", rng(s, 100)).into_bytes(),
        };
        let cmd = match rng(&mut s, 6) {
            0 => k::Command::Publish { text: text(&mut s) },
            1 => k::Command::Edit { id: rng(&mut s, 8), text: text(&mut s) },
            2 => k::Command::Delete { id: rng(&mut s, 8) },
            3 => k::Command::List,
            4 => k::Command::Promote { user: 1 + rng(&mut s, 4) },
            _ => k::Command::Demote { user: 1 + rng(&mut s, 4) },
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute(&actor, &cmd);
        match (&got, &want) {
            // Row order in a listing is not meaningful.
            (Ok(k::Reply::Posts(a)), Ok(k::Reply::Posts(b))) => {
                let (mut a, mut b) = (a.clone(), b.clone());
                a.sort_by_key(|p| p.id);
                b.sort_by_key(|p| p.id);
                assert_eq!(a, b, "{cmd:?}");
            }
            _ => assert_eq!(got, want, "{cmd:?}"),
        }
    }
    let a = sorted(pg.snapshot(TenantId(org)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(org)));
    assert_eq!(a, b);
}
