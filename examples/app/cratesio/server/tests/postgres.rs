//! The PostgreSQL store agrees with the kernel's `apply`: random commands run
//! through the real engine and through `MemoryEngine` give the same replies
//! and the same final state, on a test clock that moves forward.
//! Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use cratesio_kernel as k;
use cratesio_server::{CratesStore, Cratesio};
use h5i_app_core::{MemoryEngine, TenantId, Timestamp};
use h5i_app_pg::{pool, Clock, Engine, EngineConfig, ManualClock};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.users.sort_by_key(|x| x.id);
    s.sessions.sort_by_key(|x| x.id);
    s.tokens.sort_by_key(|x| x.id);
    s.crates.sort_by_key(|x| x.id);
    s.versions.sort_by_key(|x| (x.krate, x.num));
    s.owners.sort_by_key(|x| (x.krate, x.owner, x.team));
    s.invites.sort_by_key(|x| (x.krate, x.user));
    s.deps.sort_by_key(|x| (x.krate, x.num, x.on));
    s
}

fn teams_of(user: u64) -> Vec<u64> {
    match user {
        2 => vec![7],
        3 => vec![7, 8],
        _ => vec![],
    }
}

fn command(s: &mut u64) -> k::Command {
    use k::Command as C;
    let krate = 1 + rng(s, 4);
    match rng(s, 16) {
        0 | 1 => C::Authorize,
        2 => C::VerifyEmail,
        3 => C::CreateToken {
            scopes: k::NewToken {
                legacy: rng(s, 3) == 0,
                publish_new: rng(s, 2) == 0,
                publish_update: rng(s, 2) == 0,
                yank: rng(s, 2) == 0,
                change_owners: rng(s, 2) == 0,
                krate: if rng(s, 2) == 0 { None } else { Some(1 + rng(s, 4)) },
                expires: if rng(s, 2) == 0 { 0 } else { rng(s, 3_000_000) },
            },
        },
        4 => C::RevokeToken { id: rng(s, 6) },
        5 | 6 => {
            let deps = (0..rng(s, 3)).map(|_| 1 + rng(s, 4)).collect();
            C::Publish { krate, num: 1 + rng(s, 3), deps }
        }
        7 => C::Yank { krate, num: 1 + rng(s, 3), yanked: rng(s, 2) == 0 },
        8 => C::InviteOwner { krate, user: 1 + rng(s, 4) },
        9 => C::AddTeam { krate, team: 7 + rng(s, 2) },
        10 => C::RemoveOwner { krate, owner: if rng(s, 2) == 0 { 1 + rng(s, 4) } else { 7 + rng(s, 2) }, team: rng(s, 2) == 0 },
        11 => C::HandleInvite { krate, accept: rng(s, 3) != 0 },
        12 => C::DeleteCrate { krate, downloads: rng(s, 3000) },
        13 => C::Lock { user: 1 + rng(s, 4), until: if rng(s, 2) == 0 { 0 } else { rng(s, 3_000_000) } },
        14 => C::Unlock { user: 1 + rng(s, 4) },
        _ => C::SetAdmin { user: 1 + rng(s, 4), admin: rng(s, 2) == 0 },
    }
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let clock = ManualClock::default();
    let config = EngineConfig { clock: Clock::Manual(clock.clone()), monotonic: true, ..Default::default() };
    let pg = Engine::<Cratesio, CratesStore>::new(pool(&h5i_app_pg::with_schema(&url, "cratesio").unwrap(), 4).unwrap(), config);
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Cratesio>::default();
    let registry = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 7;
    let mut now = 1_000_000u64;
    let mut ok = 0;
    let mut errs = std::collections::BTreeMap::new();
    // Sessions and tokens handed out so far, with their users, so most
    // requests carry a credential that exists.
    let mut sessions: Vec<(u64, u64)> = Vec::new();
    let mut tokens: Vec<(u64, u64)> = Vec::new();
    for _ in 0..1200 {
        // Time moves on, sometimes by days, so invitations expire and the
        // 72-hour deletion window closes.
        now += if rng(&mut s, 10) == 0 { 86_400 * (1 + rng(&mut s, 40)) } else { rng(&mut s, 3600) };
        let cmd = command(&mut s);
        let mut user = 1 + rng(&mut s, 4);
        let pick = |s: &mut u64, l: &Vec<(u64, u64)>| l[rng(s, l.len() as u64) as usize];
        // Mostly the credential the command expects; sometimes any other.
        let kind = match (&cmd, rng(&mut s, 10)) {
            (_, 0) => rng(&mut s, 4),
            (k::Command::Authorize, _) => 0,
            (k::Command::Lock { .. } | k::Command::Unlock { .. } | k::Command::SetAdmin { .. }, _) => 3,
            (_, n) if n < 7 => 1,
            _ => 2,
        };
        let via = match kind {
            0 => k::Via::GitHub,
            1 if !sessions.is_empty() && rng(&mut s, 8) != 0 => {
                let (sid, u) = pick(&mut s, &sessions);
                user = u;
                k::Via::Cookie(sid)
            }
            1 => k::Via::Cookie(rng(&mut s, 8)),
            2 if !tokens.is_empty() && rng(&mut s, 8) != 0 => {
                let (tid, u) = pick(&mut s, &tokens);
                user = u;
                k::Via::Token(tid)
            }
            2 => k::Via::Token(rng(&mut s, 6)),
            _ => k::Via::Operator,
        };
        let actor = k::Principal { registry, user, via, now: 0, teams: teams_of(user) };
        clock.set(Timestamp::from_secs(now));
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute_at(&actor, Timestamp::from_secs(now), &cmd);
        assert_eq!(got, want, "{cmd:?} by {actor:?}");
        ok += got.is_ok() as u32;
        if let Err(e) = &got {
            *errs.entry(format!("{e:?}")).or_insert(0u32) += 1;
        }
        match got {
            Ok(k::Reply::SignedIn { user, session }) => sessions.push((session, user)),
            Ok(k::Reply::TokenCreated(id)) => tokens.push((id, user)),
            _ => {}
        }
    }
    let a = sorted(pg.snapshot(TenantId(registry)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(registry)));
    assert_eq!(a, b);
    // The run reached interesting states, not only refusals.
    eprintln!("refusals: {errs:?}");
    eprintln!("{ok} commands succeeded; {} crates, {} versions, {} owners, {} deps at the end",
        a.crates.len(), a.versions.len(), a.owners.len(), a.deps.len());
    assert!(ok > 300, "only {ok} commands succeeded");
    assert!(!a.versions.is_empty(), "{a:?}");
}
