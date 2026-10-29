//! On random commands from random users, the PostgreSQL engine and
//! `MemoryEngine` (which runs `apply`) give the same replies, denials
//! included, and the same final state. Both run the guarded transition.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use forum_kernel as k;
use forum_server::{Forum, ForumStore};
use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{Engine, EngineConfig, pool};
use h5i_app_std::authz::Guarded;

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.posts.sort_by_key(|p| p.id);
    s.users.sort_by_key(|u| u.user);
    s
}

fn command(s: &mut u64) -> k::Command {
    let text = |s: &mut u64| if rng(s, 8) == 0 { Vec::new() } else { format!("t{}", rng(s, 100)).into_bytes() };
    match rng(s, 9) {
        0 => k::Command::Register { admin: rng(s, 3) == 0 },
        1 => k::Command::CreatePost { title: text(s), body: text(s), published: rng(s, 2) == 0 },
        2 => k::Command::EditPost { id: rng(s, 8), title: text(s), body: text(s) },
        3 => k::Command::SetPublished { id: rng(s, 8), published: rng(s, 2) == 0 },
        4 => k::Command::DeletePost { id: rng(s, 8) },
        5 => k::Command::SetAdmin { user: 1 + rng(s, 4), admin: rng(s, 2) == 0 },
        6 => k::Command::ReadPost { id: rng(s, 8) },
        7 => k::Command::ListPosts,
        _ => k::Command::ReadUser { user: 1 + rng(s, 4) },
    }
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Forum, ForumStore>::new(
        pool(&h5i_app_pg::with_schema(&url, "forum").unwrap(), 4).unwrap(),
        EngineConfig::default(),
    );
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Forum>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let (mut seed, mut denied, mut committed) = (17, 0, 0);
    for _ in 0..600 {
        let actor = k::Principal { org, user: 1 + rng(&mut seed, 4) };
        let cmd = command(&mut seed);
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
        match want {
            Err(Guarded::Denied) => denied += 1,
            Ok(_) => committed += 1,
            Err(Guarded::App(_)) => {}
        }
    }
    assert!(denied > 0 && committed > 0, "the run exercises both: {denied} denied, {committed} committed");
    let a = sorted(pg.snapshot(TenantId(org)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(org)));
    assert_eq!(a, b);
}
