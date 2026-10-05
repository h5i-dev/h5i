//! On random commands, the PostgreSQL engine and `MemoryEngine` (which runs
//! `apply`) give the same replies and final state.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{pool, Engine, EngineConfig};
use inbox_kernel as k;
use inbox_server::{Inbox, InboxStore};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn key(m: &k::Message) -> (u64, u64, u64) {
    (m.sender, m.recipient, m.seq)
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.messages.sort_by_key(key);
    s.blocks.sort_by_key(|b| (b.owner, b.sender));
    s
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Inbox, InboxStore>::new(pool(&h5i_app_pg::with_schema(&url, "inbox").unwrap(), 4).unwrap(), EngineConfig::default());
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Inbox>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 17;
    for _ in 0..500 {
        let actor = k::Principal { org, user: 1 + rng(&mut s, 4) };
        let user = |s: &mut u64| 1 + rng(s, 4);
        let text = |s: &mut u64| match rng(s, 6) {
            0 => Vec::new(),
            1 => vec![b'x'; 1001],
            _ => format!("msg {}", rng(s, 100)).into_bytes(),
        };
        let cmd = match rng(&mut s, 9) {
            0..=2 => k::Command::Send { to: user(&mut s), text: text(&mut s) },
            3 => k::Command::Inbox,
            4 => k::Command::Sent,
            5 => k::Command::MarkRead { from: user(&mut s), seq: rng(&mut s, 4) },
            6 => k::Command::Delete { from: user(&mut s), to: user(&mut s), seq: rng(&mut s, 4) },
            7 => k::Command::Block { user: user(&mut s) },
            _ => k::Command::Unblock { user: user(&mut s) },
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute(&actor, &cmd);
        match (&got, &want) {
            // Row order in a listing is not meaningful.
            (Ok(k::Reply::Messages(a)), Ok(k::Reply::Messages(b))) => {
                let (mut a, mut b) = (a.clone(), b.clone());
                a.sort_by_key(key);
                b.sort_by_key(key);
                assert_eq!(a, b, "{cmd:?}");
            }
            _ => assert_eq!(got, want, "{cmd:?}"),
        }
    }
    let a = sorted(pg.snapshot(TenantId(org)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(org)));
    assert!(!b.messages.is_empty() && !b.blocks.is_empty());
    assert_eq!(a, b);
}
