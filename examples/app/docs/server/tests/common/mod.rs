#![allow(dead_code)]
use docs_kernel as k;
use docs_server::{DocsApp, DocsStore};
use h5i_app_pg::{pool, Engine, EngineConfig};
use std::sync::atomic::{AtomicU64, Ordering};

pub type PgEngine = Engine<DocsApp, DocsStore>;

/// None when no test database is configured; tests then skip.
pub async fn engine(config: EngineConfig) -> Option<PgEngine> {
    let url = std::env::var("H5I_APP_TEST_DATABASE_URL").ok()?;
    let e = Engine::new(pool(&url, 32).expect("pool"), config);
    e.install_schema().await.expect("schema");
    Some(e)
}

/// A tenant id no other test run has used.
pub fn fresh_tenant() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
    (t >> 8) % (1 << 50) * 64 + N.fetch_add(1, Ordering::Relaxed) % 64
}

/// Row order is not meaningful; compare snapshots in key order.
pub fn normalize(mut s: k::Snapshot) -> k::Snapshot {
    s.projects.sort_by_key(|p| p.id);
    s.members.sort_by_key(|m| (m.project, m.user));
    s.documents.sort_by_key(|d| d.id);
    s.webhooks.sort_by_key(|w| w.project);
    s
}

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

pub fn random_command(r: &mut Rng) -> k::Command {
    let id = |r: &mut Rng| r.next(8);
    let text = |r: &mut Rng| format!("t{}", r.next(100)).into_bytes();
    let role = |r: &mut Rng| [k::Role::Viewer, k::Role::Editor, k::Role::Owner][r.next(3) as usize];
    match r.next(12) {
        0 => k::Command::CreateProject { name: text(r) },
        1 => k::Command::SetMember { project: id(r), user: 1 + r.next(4), role: role(r) },
        2 => k::Command::RemoveMember { project: id(r), user: 1 + r.next(4) },
        3 => k::Command::CreateDocument { project: id(r), title: text(r), body: text(r) },
        4 => k::Command::EditDocument { doc: id(r), body: text(r), expected_version: 1 + r.next(4) },
        5 => k::Command::Submit { doc: id(r) },
        6 => k::Command::Approve { doc: id(r) },
        7 => k::Command::Publish { doc: id(r) },
        8 => k::Command::DeleteDocument { doc: id(r) },
        9 => k::Command::GetDocument { doc: id(r) },
        10 => k::Command::ListDocuments { project: id(r) },
        _ => k::Command::SetWebhook { project: id(r), dest: if r.next(3) == 0 { None } else { Some(r.next(3)) } },
    }
}

/// `Frame.slice` from the proofs, for checking `DocsStore::load_for`.
pub fn slice(s: &k::Snapshot, scope: k::Scope) -> k::Snapshot {
    let keep = |p: u64| k::Snapshot {
        counter: s.counter.clone(),
        projects: s.projects.iter().filter(|q| q.id == p).cloned().collect(),
        members: s.members.iter().filter(|m| m.project == p).cloned().collect(),
        documents: s.documents.iter().filter(|d| d.project == p).cloned().collect(),
        webhooks: s.webhooks.iter().filter(|w| w.project == p).cloned().collect(),
    };
    let counter_only = k::Snapshot { counter: s.counter.clone(), ..Default::default() };
    match scope {
        k::Scope::Counter => counter_only,
        k::Scope::Project(p) => keep(p),
        k::Scope::Document(d) => match s.documents.iter().find(|x| x.id == d) {
            Some(doc) => keep(doc.project),
            None => counter_only,
        },
    }
}
