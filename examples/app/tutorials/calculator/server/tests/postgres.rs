//! On random commands, the PostgreSQL engine and `MemoryEngine` (which runs
//! `apply`) give the same replies and final state.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use calculator_kernel as k;
use calculator_server::{Calc, CalcStore};
use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{pool, Engine, EngineConfig};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Calc, CalcStore>::new(pool(&h5i_app_pg::with_schema(&url, "calculator").unwrap(), 4).unwrap(), EngineConfig::default());
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Calc>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 7;
    for _ in 0..300 {
        let actor = k::Principal { org, user: 1 + rng(&mut s, 3) };
        // Values near 0 and near the top of u64 hit every refusal.
        let big = |s: &mut u64| if rng(s, 2) == 0 { rng(s, 10) } else { u64::MAX - rng(s, 10) };
        let cmd = match rng(&mut s, 3) {
            0 => k::Command::Set { value: big(&mut s) },
            1 => {
                let op = [k::Op::Add, k::Op::Sub, k::Op::Mul, k::Op::Div][rng(&mut s, 4) as usize];
                k::Command::Apply { op, arg: big(&mut s) }
            }
            _ => k::Command::Get,
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute(&actor, &cmd);
        assert_eq!(got, want, "{cmd:?}");
    }
    let mut a = pg.snapshot(TenantId(org)).await.unwrap();
    let mut b = mem.snapshot(TenantId(org));
    a.memories.sort_by_key(|m| m.user);
    b.memories.sort_by_key(|m| m.user);
    assert_eq!(a, b);
}
