//! 64 concurrent clients on one organization's 6 accounts, through the
//! PostgreSQL engine, with and without advisory locks (A5, A6). Afterwards:
//! the kernel's invariant holds (`Spec.Inv`: account ids below `next_id`,
//! balances plus withdrawals equal deposits), and the committed state is a
//! serial run of exactly the successful commands through `transition` and
//! `apply`: an order is found that reproduces each one's reply and write set
//! and ends in the final snapshot.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use h5i_app_core::{Kernel, TenantId};
use h5i_app_pg::{pool, DbError, Engine, EngineConfig};
use ledger_kernel as k;
use ledger_server::{Ledger, LedgerStore};
use std::collections::HashSet;
use std::sync::Arc;

const CLIENTS: u64 = 64;
const PER_CLIENT: usize = 10;
const USERS: u64 = 6;

type Done = (k::Principal, k::Command, k::Reply, Vec<k::Write>);

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.accounts.sort_by_key(|a| a.id);
    s
}

/// Depth-first search for an order of `done` that `transition` accepts with
/// the same writes and reply at every step; the state it ends in. Each
/// command writes every row it reads, with absolute values, so in every
/// order `transition` accepts the set run so far leaves the same state; a
/// set from which no order completes is never searched again: the search is
/// complete, and `Err` says whether it proved there is no order or ran out
/// of budget.
fn serial_order(start: &k::Snapshot, done: &[Done]) -> Result<k::Snapshot, String> {
    struct Search<'a> {
        done: &'a [Done],
        used: Vec<bool>,
        dead: HashSet<Vec<bool>>,
        budget: u64,
    }
    impl Search<'_> {
        fn go(&mut self, s: &k::Snapshot, left: usize) -> Result<Option<k::Snapshot>, ()> {
            if left == 0 {
                return Ok(Some(s.clone()));
            }
            if self.dead.contains(&self.used) {
                return Ok(None);
            }
            for i in 0..self.done.len() {
                if self.used[i] {
                    continue;
                }
                self.budget = self.budget.checked_sub(1).ok_or(())?;
                let (a, c, r, ws) = &self.done[i];
                if let Ok((ws2, r2)) = Ledger::transition(a, s, c)
                    && ws2 == *ws
                    && r2 == *r
                {
                    self.used[i] = true;
                    let end = self.go(&Ledger::apply(s, ws), left - 1);
                    self.used[i] = false;
                    if let Some(end) = end? {
                        return Ok(Some(end));
                    }
                }
            }
            self.dead.insert(self.used.clone());
            Ok(None)
        }
    }
    let mut search = Search { done, used: vec![false; done.len()], dead: HashSet::new(), budget: 200_000_000 };
    match search.go(start, done.len()) {
        Ok(Some(end)) => Ok(end),
        Ok(None) => Err(format!("no serial order of the {} commits exists", done.len())),
        Err(()) => Err(format!("search budget exhausted ({} dead sets); undecided", search.dead.len())),
    }
}

fn check_inv(s: &k::Snapshot) {
    let mut ids: Vec<u64> = s.accounts.iter().map(|a| a.id).collect();
    ids.dedup();
    assert_eq!(ids.len(), s.accounts.len(), "account ids are unique");
    assert!(s.accounts.iter().all(|a| a.id < s.ledger.next_id), "fresh");
    let total: u128 = s.accounts.iter().map(|a| u128::from(a.balance)).sum();
    assert_eq!(total + u128::from(s.ledger.withdrawn), u128::from(s.ledger.deposited), "conserved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_clients_are_serial() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let url = h5i_app_pg::with_schema(&url, "ledger").unwrap();
    for tenant_lock in [true, false] {
        let config = EngineConfig { tenant_lock, max_attempts: 1000, ..EngineConfig::default() };
        let pg = Arc::new(Engine::<Ledger, LedgerStore>::new(pool(&url, 16).unwrap(), config));
        pg.install_schema().await.unwrap();
        let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);
        // Users 1-6 each open account `user - 1` and deposit 500.
        for user in 1..=USERS {
            let a = k::Principal { org, user };
            assert_eq!(pg.execute(&a, &k::Command::Open).await.unwrap(), Ok(k::Reply::Created(user - 1)));
            pg.execute(&a, &k::Command::Deposit { account: user - 1, amount: 500 }).await.unwrap().unwrap();
        }
        let start = sorted(pg.snapshot(TenantId(org)).await.unwrap());
        let mut tasks = Vec::new();
        for client in 0..CLIENTS {
            let pg = pg.clone();
            tasks.push(tokio::spawn(async move {
                let mut s = client.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ u64::from(tenant_lock);
                let mut rng = move |n: u64| {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (s >> 33) % n
                };
                let user = 1 + client % USERS;
                let a = k::Principal { org, user };
                let mut out = Vec::new();
                for _ in 0..PER_CLIENT {
                    // Mostly transfers, from a random account (refused unless
                    // the client's own) of up to 300 (refused beyond the balance).
                    let cmd = match rng(10) {
                        0 => k::Command::Deposit { account: user - 1, amount: 1 + rng(50) },
                        1 => k::Command::Withdraw { account: user - 1, amount: 1 + rng(100) },
                        _ => k::Command::Transfer { src: if rng(4) == 0 { rng(USERS) } else { user - 1 }, dst: rng(USERS), amount: 1 + rng(300) },
                    };
                    let r = pg.execute_committed(&a, &cmd).await;
                    out.push((a, cmd, r));
                }
                out
            }));
        }
        let (mut done, mut refused, mut exhausted) = (Vec::new(), 0, 0);
        for t in tasks {
            for (a, cmd, r) in t.await.unwrap() {
                match r {
                    Ok(c) => match c.result {
                        Ok(reply) => done.push((a, cmd, reply, c.writes.expect("a commit returns its writes"))),
                        Err(_) => refused += 1,
                    },
                    // Certainly not committed: every attempt failed to serialize.
                    Err(DbError::RetriesExhausted) => exhausted += 1,
                    Err(e) => panic!("tenant_lock={tenant_lock}: {e}"),
                }
            }
        }
        let end = sorted(pg.snapshot(TenantId(org)).await.unwrap());
        check_inv(&end);
        assert_eq!(end.ledger.next_id, USERS);
        assert_eq!(done.len() + refused + exhausted, (CLIENTS as usize) * PER_CLIENT);
        // One commit per successful reply (the setup made 2 per user).
        let commits = pg.stats.commits.load(std::sync::atomic::Ordering::Relaxed) as usize;
        assert_eq!(commits - 2 * USERS as usize, done.len(), "tenant_lock={tenant_lock}");
        let serial = serial_order(&start, &done).map(sorted).unwrap_or_else(|e| panic!("tenant_lock={tenant_lock}: {e}"));
        assert_eq!(serial, end, "tenant_lock={tenant_lock}: the serial order ends elsewhere");
        let retries = pg.stats.retries.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("tenant_lock={tenant_lock}: {} commits, {refused} refused, {exhausted} out of attempts, {retries} retries", done.len());
        assert!(done.len() >= CLIENTS as usize, "tenant_lock={tenant_lock}: too few commits to mean anything");
    }
}
