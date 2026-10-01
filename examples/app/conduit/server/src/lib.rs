//! Conduit's shell: the PostgreSQL store, stored replies, authentication and
//! the RealWorld JSON API. It makes no decisions of its own; every rule is in
//! `conduit_kernel`.

pub mod api;

use conduit_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use serde::{Deserialize, Serialize};

/// Conduit is one site, so every caller is in this tenant.
pub const TENANT: u64 = 1;

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
        k::transition(actor, snap, cmd)
    }

    fn apply(snap: &k::Snapshot, ws: &Vec<k::Write>) -> k::Snapshot {
        k::apply(snap, ws)
    }
}

pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user }
}

// The tables, from the kernel's `schema!`.
conduit_kernel::conduit_tables!(Conduit);

pub struct ConduitStore;

impl Store<Conduit> for ConduitStore {
    fn ddl() -> Vec<String> {
        schema_ddl()
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    /// Rows come back in key order, which is the id order `apply` keeps.
    // `Storage.lean`: stored `sql_writes` load back as what `apply` computes.
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        schema_load(tx, t).await
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_store(tx, t, ws).await
    }
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
