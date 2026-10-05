//! On random commands, the PostgreSQL engine and `MemoryEngine` (which runs
//! `apply`) give the same replies and final state.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{pool, Engine, EngineConfig};
use ledger_kernel as k;
use ledger_server::{Ledger, LedgerStore};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.accounts.sort_by_key(|a| a.id);
    s
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Ledger, LedgerStore>::new(pool(&h5i_app_pg::with_schema(&url, "ledger").unwrap(), 4).unwrap(), EngineConfig::default());
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Ledger>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 7;
    for _ in 0..400 {
        let actor = k::Principal { org, user: 1 + rng(&mut s, 3) };
        // Mostly small amounts, sometimes one large enough to overflow.
        let amount = |s: &mut u64| if rng(s, 20) == 0 { u64::MAX - rng(s, 10) } else { rng(s, 120) };
        let cmd = match rng(&mut s, 6) {
            0 => k::Command::Open,
            1 => k::Command::Deposit { account: rng(&mut s, 6), amount: amount(&mut s) },
            2 => k::Command::Withdraw { account: rng(&mut s, 6), amount: amount(&mut s) },
            3 | 4 => k::Command::Transfer { src: rng(&mut s, 6), dst: rng(&mut s, 6), amount: amount(&mut s) },
            _ => k::Command::List,
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute(&actor, &cmd);
        match (&got, &want) {
            // Row order in a listing is not meaningful.
            (Ok(k::Reply::Accounts(a)), Ok(k::Reply::Accounts(b))) => {
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
    // Conservation, as `Theorems.conservation` states it.
    let total: u128 = a.accounts.iter().map(|x| x.balance as u128).sum();
    assert_eq!(total + a.ledger.withdrawn as u128, a.ledger.deposited as u128);
    assert!(a.ledger.withdrawn > 0, "the run should include withdrawals");
}
