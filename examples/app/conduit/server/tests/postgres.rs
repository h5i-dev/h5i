//! The PostgreSQL store agrees with the kernel's `apply`: random commands run
//! through the real engine and through `MemoryEngine` give the same replies
//! and the same final state. Needs H5I_APP_TEST_DATABASE_URL; skips otherwise.

use conduit_kernel as k;
use conduit_server::{Conduit, ConduitStore};
use h5i_app_core::{MemoryEngine, TenantId};
use h5i_app_pg::{pool, Engine, EngineConfig};

fn rng(state: &mut u64, n: u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 33) % n
}

fn pick(s: &mut u64, words: &[&str]) -> Vec<u8> {
    words[rng(s, words.len() as u64) as usize].as_bytes().to_vec()
}

fn maybe(s: &mut u64, words: &[&str]) -> Option<Vec<u8>> {
    if rng(s, 2) == 0 {
        None
    } else {
        Some(pick(s, words))
    }
}

const NAMES: &[&str] = &["ann", "bob", "cy", "dee", "eve"];
const SLUGS: &[&str] = &["a", "b", "c", "d"];
const TAGS: &[&str] = &["rust", "sql", "lean", "web"];

fn sorted(mut s: k::Snapshot) -> k::Snapshot {
    s.users.sort_by_key(|x| x.id);
    s.follows.sort_by_key(|f| (f.follower, f.followed));
    s.articles.sort_by_key(|a| a.id);
    s.tags.sort_by(|a, b| (a.article, &a.tag).cmp(&(b.article, &b.tag)));
    s.favorites.sort_by_key(|f| (f.article, f.user));
    s.comments.sort_by_key(|c| c.id);
    s
}

fn command(s: &mut u64) -> k::Command {
    match rng(s, 18) {
        0 => k::Command::Register { username: pick(s, NAMES), email: pick(s, NAMES), password: pick(s, SLUGS) },
        1 => k::Command::Login { email: pick(s, NAMES), password: pick(s, SLUGS) },
        2 => k::Command::CurrentUser,
        3 => k::Command::UpdateUser {
            email: maybe(s, NAMES),
            username: maybe(s, NAMES),
            password: None,
            bio: maybe(s, TAGS),
            image: None,
        },
        4 => k::Command::GetProfile { username: pick(s, NAMES) },
        5 => k::Command::Follow { username: pick(s, NAMES) },
        6 => k::Command::Unfollow { username: pick(s, NAMES) },
        7 => k::Command::ListArticles {
            tag: maybe(s, TAGS),
            author: maybe(s, NAMES),
            favorited: maybe(s, NAMES),
            limit: rng(s, 4),
            offset: rng(s, 3),
        },
        8 => k::Command::Feed { limit: 1 + rng(s, 4), offset: rng(s, 2) },
        9 => k::Command::GetArticle { slug: pick(s, SLUGS) },
        10 | 11 => {
            // Sorted, as the shell sends them.
            let mut tags: Vec<Vec<u8>> = (0..rng(s, 4)).map(|_| pick(s, TAGS)).collect();
            tags.sort();
            let slug = pick(s, SLUGS);
            k::Command::CreateArticle { slug: slug.clone(), title: slug, description: b"d".to_vec(), body: b"b".to_vec(), tags, now: rng(s, 100) }
        }
        12 => k::Command::UpdateArticle {
            slug: pick(s, SLUGS),
            new_slug: maybe(s, SLUGS),
            title: None,
            description: None,
            body: maybe(s, TAGS),
            now: rng(s, 100),
        },
        13 => k::Command::DeleteArticle { slug: pick(s, SLUGS) },
        14 => k::Command::Favorite { slug: pick(s, SLUGS) },
        15 => k::Command::Unfavorite { slug: pick(s, SLUGS) },
        16 => match rng(s, 3) {
            0 => k::Command::GetComments { slug: pick(s, SLUGS) },
            1 => k::Command::DeleteComment { slug: pick(s, SLUGS), id: 1 + rng(s, 6) },
            _ => k::Command::GetTags,
        },
        _ => k::Command::AddComment { slug: pick(s, SLUGS), body: pick(s, TAGS), now: rng(s, 100) },
    }
}

#[tokio::test]
async fn store_agrees_with_apply() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let pg = Engine::<Conduit, ConduitStore>::new(pool(&h5i_app_pg::with_schema(&url, "conduit").unwrap(), 4).unwrap(), EngineConfig::default());
    pg.install_schema().await.unwrap();
    let mem = MemoryEngine::<Conduit>::default();
    let org = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1 << 50);

    let mut s = 7;
    let (mut oks, mut refusals) = (0, 0);
    for i in 0..1500 {
        // Users 1 to 5 exist after the first commands; 0 is anonymous.
        let user = if i < 10 { 0 } else { rng(&mut s, 6) };
        let actor = k::Principal { org, user };
        let cmd = if i < 10 {
            let n = NAMES[i % NAMES.len()].as_bytes().to_vec();
            k::Command::Register { username: n.clone(), email: n, password: b"pw".to_vec() }
        } else {
            command(&mut s)
        };
        let got = pg.execute(&actor, &cmd).await.unwrap();
        let want = mem.execute(&actor, &cmd);
        match &want {
            Ok(_) => oks += 1,
            Err(_) => refusals += 1,
        }
        assert_eq!(got, want, "{cmd:?} by {user}");
    }
    let a = sorted(pg.snapshot(TenantId(org)).await.unwrap());
    let b = sorted(mem.snapshot(TenantId(org)));
    assert_eq!(a, b);
    // The run exercises both outcomes and leaves rows in every table.
    assert!(oks > 300 && refusals > 300, "{oks} ok, {refusals} refused");
    assert!(!a.articles.is_empty() && !a.favorites.is_empty() && !a.follows.is_empty());
}
