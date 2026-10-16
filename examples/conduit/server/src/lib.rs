//! Conduit's shell: the PostgreSQL store, stored replies, authentication and
//! the RealWorld JSON API. Every decision about the state is in
//! `conduit_kernel`; `api` lists what the shell does besides decoding.

pub mod api;

use conduit_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_pg::{DbError, EngineConfig, Lock, ReplyCodec, Store, Tx};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// Conduit is one site, so every caller is in this tenant.
pub const TENANT: u64 = 1;

/// How the server runs the engine; `examples/load.rs` uses the same.
pub fn engine_config() -> EngineConfig {
    EngineConfig::default()
}

tokio::task_local! {
    /// A listing's `articlesCount`. `api::listing` sets up the cell;
    /// `ConduitStore::load_for` fills it with the count of
    /// `k::article_count`'s read, in the listing's own transaction.
    pub static ARTICLE_COUNT: Cell<Option<u64>>;
}

/// Marker type the framework's traits hang off.
pub struct Conduit;

impl Kernel for Conduit {
    type Principal = k::Principal;
    type Snapshot = k::Snapshot;
    type Command = k::Command;
    type WriteSet = Vec<k::Write>;
    type Reply = k::Reply;
    type Error = k::Error;

    fn tenant(actor: &k::Principal) -> TenantId {
        TenantId(actor.org)
    }

    fn transition(actor: &k::Principal, snap: &k::Snapshot, cmd: &k::Command) -> Result<(Vec<k::Write>, k::Reply), k::Error> {
        // PG load_for already set the count. RAM must count on this same snapshot.
        if ARTICLE_COUNT.try_with(|c| c.get().is_none()).unwrap_or(false) {
            let resolve = |name: &Option<Vec<u8>>| match name {
                None => Some(None),
                Some(n) => k::find_user_by_name(&snap.users, n).map(|u| Some(u.id)),
            };
            let count = match cmd {
                k::Command::ListArticles { tag, author, favorited, .. } => Some(match (resolve(author), resolve(favorited)) {
                    (Some(a), Some(f)) => snap.articles.iter().filter(|row| k::matches(snap, row, tag, a, f, false)).count() as u64,
                    _ => 0,
                }),
                k::Command::Feed { .. } => Some(snap.articles.iter().filter(|row| k::is_following(&snap.follows, actor.user, row.author)).count() as u64),
                _ => None,
            };
            let _ = ARTICLE_COUNT.try_with(|c| c.set(count));
        }
        k::transition(actor, snap, cmd)
    }

    fn apply(snap: &k::Snapshot, ws: &Vec<k::Write>) -> k::Snapshot {
        k::apply(snap, ws)
    }

    // `ReadOnly.lean`: `H5iAppLib.ReadOnly.Sound` for `k::read_only`.
    fn read_only(cmd: &k::Command) -> bool {
        k::read_only(cmd)
    }
}

pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user }
}

// The tables, from the kernel's `schema!`.
conduit_kernel::conduit_tables!(Conduit);

pub struct ConduitStore;

impl Store<Conduit> for ConduitStore {
    /// The tables, and indexes for the reads of `k::reads`: articles by slug
    /// and author, tags by name, favorites by user, users by name and email,
    /// comments by article. They change no result, only speed.
    fn ddl() -> Vec<String> {
        use conduit_kernel::conduit_tables as c;
        let mut out = schema_ddl();
        out.push(schema_index(k::Article::TABLE, &[c::Article::slug]));
        out.push(schema_index(k::Article::TABLE, &[c::Article::author, c::Article::id]));
        out.push(schema_index(k::Tag::TABLE, &[c::Tag::tag]));
        out.push(schema_index(k::Favorite::TABLE, &[c::Favorite::user]));
        out.push(schema_index(k::User::TABLE, &[c::User::username]));
        out.push(schema_index(k::User::TABLE, &[c::User::email]));
        out.push(schema_index(k::Comment::TABLE, &[c::Comment::article]));
        out
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    /// Rows come back in key order, which is the id order `apply` keeps.
    // `Storage.lean`: stored `sql_writes` load back as what `apply` computes.
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        schema_load(tx, t).await
    }

    /// The rows `k::reads` names, each table by one declarative read, or the
    /// whole site for the commands it leaves out. `Frame.lean`: `transition`
    /// gives the same result on them as on `load`.
    ///
    /// For a listing or the feed it also counts the articles before paging
    /// (`k::article_count`, `Frame.count_list`) for `articlesCount`, sent on
    /// the same connection as the plan's reads.
    async fn load_for(tx: &Tx<'_>, t: TenantId, actor: &k::Principal, cmd: &k::Command) -> Result<k::Snapshot, DbError> {
        let count = k::article_count(actor, cmd).filter(|_| ARTICLE_COUNT.try_with(|_| ()).is_ok());
        match (k::reads(actor, cmd), count) {
            (None, _) => schema_load(tx, t).await,
            (Some(plan), None) => schema_load_plan(tx, t, &plan).await,
            (Some(plan), Some(count)) => {
                let (snap, n) = futures_util::future::try_join(schema_load_plan(tx, t, &plan), schema_count(tx, t, &count)).await?;
                let _ = ARTICLE_COUNT.try_with(|c| c.set(Some(n)));
                Ok(snap)
            }
        }
    }

    /// Writes queue on what they change, not on the whole site: an
    /// article's slug, a follow pair, a user, the id counters. Only fewer
    /// retries ride on this; SERIALIZABLE orders the rest.
    fn lock_for(actor: &k::Principal, cmd: &k::Command) -> Lock {
        let user = actor.user.to_be_bytes();
        match cmd {
            k::Command::Favorite { slug } | k::Command::Unfavorite { slug } | k::Command::DeleteArticle { slug } => {
                Lock::Keys(vec![Lock::key("article", slug)])
            }
            // A rename also takes the slug it moves to.
            k::Command::UpdateArticle { slug, new_slug, .. } => {
                let mut keys = vec![Lock::key("article", slug)];
                keys.extend(new_slug.iter().map(|n| Lock::key("article", n)));
                Lock::Keys(keys)
            }
            k::Command::Follow { username } | k::Command::Unfollow { username } => {
                Lock::Keys(vec![Lock::key("follow", &[&user[..], username].concat())])
            }
            k::Command::UpdateUser { .. } => Lock::Keys(vec![Lock::key("user", &user)]),
            // Each takes the next id from the counter row.
            k::Command::Register { .. } | k::Command::CreateArticle { .. } | k::Command::AddComment { .. } => {
                Lock::Keys(vec![Lock::key("counter", &[])])
            }
            k::Command::DeleteComment { id, .. } => Lock::Keys(vec![Lock::key("comment", &id.to_be_bytes())]),
            _ => Lock::Tenant,
        }
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_store(tx, t, ws).await
    }
}

/// Runs the server as `main` does, configured by the environment:
/// `DATABASE_URL`, `JWT_SECRET` (default `conduit-benchmark-secret`),
/// `DB_POOL` connections (default 32) and `LISTEN` (default
/// `127.0.0.1:$PORT`, `PORT` default 8080). `GET /healthz` answers `ok`.
pub async fn serve_from_env() -> Result<(), Box<dyn std::error::Error>> {
    use axum::routing::get;
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "conduit-benchmark-secret".into());
    let pool_size = std::env::var("DB_POOL").ok().and_then(|s| s.parse().ok()).unwrap_or(32);
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| format!("127.0.0.1:{port}"));

    let pool = h5i_app_pg::pool(&h5i_app_pg::with_schema(&url, "conduit")?, pool_size)?;
    let engine = std::sync::Arc::new(h5i_app_pg::Engine::<Conduit, ConduitStore>::new(pool, engine_config()));
    engine.install_schema().await?;
    let app = api::App { engine: std::sync::Arc::new(api::BenchEngine::Pg(engine)), auth: std::sync::Arc::new(api::ConduitAuth::new(secret.as_bytes())) };
    let router = axum::Router::new().route("/healthz", get(|| async { "ok" })).merge(api::router(app));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}

/// `main`: `serve_from_env` on `WEB_CONCURRENCY` worker threads (default:
/// one per CPU).
pub fn main_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).try_init();
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = std::env::var("WEB_CONCURRENCY").ok().and_then(|s| s.parse::<usize>().ok()).filter(|n| *n > 0) {
        rt.worker_threads(n);
    }
    rt.enable_all().build()?.block_on(serve_from_env())
}

/* Stored replies, for idempotent retries */

#[derive(Serialize, Deserialize)]
struct P(Vec<u8>, Vec<u8>, Vec<u8>, bool);

#[derive(Serialize, Deserialize)]
struct A(u64, u64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, u64, u64);

#[derive(Serialize, Deserialize)]
struct Av(A, Vec<Vec<u8>>, bool, u64, P);

#[derive(Serialize, Deserialize)]
struct C(u64, u64, u64, Vec<u8>, u64);

#[derive(Serialize, Deserialize)]
struct Cv(C, P);

#[derive(Serialize, Deserialize)]
enum Stored {
    Account(u64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>),
    Profile(P),
    Article(Av),
    Articles(Vec<Av>),
    Comment(Cv),
    Comments(Vec<Cv>),
    Tags(Vec<Vec<u8>>),
    Done,
}

fn p_to(p: &k::Profile) -> P {
    P(p.username.clone(), p.bio.clone(), p.image.clone(), p.following)
}

fn p_from(p: P) -> k::Profile {
    k::Profile { username: p.0, bio: p.1, image: p.2, following: p.3 }
}

fn av_to(v: &k::ArticleView) -> Av {
    let a = &v.article;
    Av(
        A(a.id, a.author, a.slug.clone(), a.title.clone(), a.description.clone(), a.body.clone(), a.created, a.updated),
        v.tags.clone(),
        v.favorited,
        v.favorites,
        p_to(&v.author),
    )
}

fn av_from(v: Av) -> k::ArticleView {
    let a = v.0;
    k::ArticleView {
        article: k::Article { id: a.0, author: a.1, slug: a.2, title: a.3, description: a.4, body: a.5, created: a.6, updated: a.7 },
        tags: v.1,
        favorited: v.2,
        favorites: v.3,
        author: p_from(v.4),
    }
}

fn cv_to(v: &k::CommentView) -> Cv {
    let c = &v.comment;
    Cv(C(c.id, c.article, c.author, c.body.clone(), c.created), p_to(&v.author))
}

fn cv_from(v: Cv) -> k::CommentView {
    let c = v.0;
    k::CommentView { comment: k::Comment { id: c.0, article: c.1, author: c.2, body: c.3, created: c.4 }, author: p_from(v.1) }
}

impl ReplyCodec<Conduit> for ConduitStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        let s = match r {
            k::Reply::Account(a) => Stored::Account(a.id, a.email.clone(), a.username.clone(), a.bio.clone(), a.image.clone()),
            k::Reply::Profile(p) => Stored::Profile(p_to(p)),
            k::Reply::Article(v) => Stored::Article(av_to(v)),
            k::Reply::Articles(vs) => Stored::Articles(vs.iter().map(av_to).collect()),
            k::Reply::Comment(c) => Stored::Comment(cv_to(c)),
            k::Reply::Comments(cs) => Stored::Comments(cs.iter().map(cv_to).collect()),
            k::Reply::Tags(ts) => Stored::Tags(ts.clone()),
            k::Reply::Done => Stored::Done,
        };
        serde_json::to_vec(&s).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            Stored::Account(id, email, username, bio, image) => k::Reply::Account(k::Account { id, email, username, bio, image }),
            Stored::Profile(p) => k::Reply::Profile(p_from(p)),
            Stored::Article(v) => k::Reply::Article(av_from(v)),
            Stored::Articles(vs) => k::Reply::Articles(vs.into_iter().map(av_from).collect()),
            Stored::Comment(c) => k::Reply::Comment(cv_from(c)),
            Stored::Comments(cs) => k::Reply::Comments(cs.into_iter().map(cv_from).collect()),
            Stored::Tags(ts) => k::Reply::Tags(ts),
            Stored::Done => k::Reply::Done,
        })
    }
}
