//! The forum's shell: moves data between the kernel, PostgreSQL and JSON.
//! It decides nothing and writes no `Kernel` or `Store`: it runs
//! `forum_kernel::App` (`h5i_app_core::Authz<forum_kernel::Forum>`, whose
//! `transition` is the authorization guard) on the `Store` that `schema!`
//! generates, so the framework's theorems are about what this server commits.

use axum::http::StatusCode;
use forum_kernel as k;
use h5i_app_json::Value as Out;
use h5i_app_pg::ReplyCodec;
use h5i_app_std::authz::Guarded;
use serde::{Deserialize, Serialize};

/// The kernel behind the guard (`h5i_app_std::authz_kernel!` in the kernel).
pub type Forum = k::App;

pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user }
}

// The `users`, `posts` and `counters` tables, from the kernel's `schema!`,
// and `ForumStore`, their generated `Store`.
forum_kernel::forum_tables!(Forum, store ForumStore);

/// The JSON a client sends, e.g. `{"cmd":"create_post","title":"hi","body":"…"}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Register {
        #[serde(default)]
        admin: bool,
    },
    CreatePost {
        title: String,
        body: String,
        #[serde(default)]
        published: bool,
    },
    EditPost { id: u64, title: String, body: String },
    SetPublished { id: u64, published: bool },
    DeletePost { id: u64 },
    SetAdmin { user: u64, admin: bool },
    ReadPost { id: u64 },
    ListPosts,
    ReadUser { user: u64 },
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Register { admin } => k::Command::Register { admin },
            CommandJson::CreatePost { title, body, published } => {
                k::Command::CreatePost { title: title.into_bytes(), body: body.into_bytes(), published }
            }
            CommandJson::EditPost { id, title, body } => {
                k::Command::EditPost { id, title: title.into_bytes(), body: body.into_bytes() }
            }
            CommandJson::SetPublished { id, published } => k::Command::SetPublished { id, published },
            CommandJson::DeletePost { id } => k::Command::DeletePost { id },
            CommandJson::SetAdmin { user, admin } => k::Command::SetAdmin { user, admin },
            CommandJson::ReadPost { id } => k::Command::ReadPost { id },
            CommandJson::ListPosts => k::Command::ListPosts,
            CommandJson::ReadUser { user } => k::Command::ReadUser { user },
        }
    }
}

fn post_json(p: &k::Post) -> Out {
    Out::obj([
        ("id", p.id.into()),
        ("author", p.author.into()),
        ("title", Out::Str(p.title.clone())),
        ("body", Out::Str(p.body.clone())),
        ("published", p.published.into()),
    ])
}

impl h5i_app_http::Api<Forum> for ForumStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Created(id) => Out::obj([("id", (*id).into())]),
            k::Reply::Done => Out::obj([("ok", true.into())]),
            k::Reply::Found(p) => post_json(p),
            k::Reply::Posts(ps) => Out::Arr(ps.iter().map(post_json).collect()),
            k::Reply::Account(u) => Out::obj([("user", u.user.into()), ("admin", u.admin.into())]),
        }
    }

    fn encode_error(e: &Guarded<k::Error>) -> (StatusCode, Out) {
        let (status, code) = match e {
            Guarded::Denied => (StatusCode::FORBIDDEN, "forbidden"),
            Guarded::App(k::Error::NotFound) => (StatusCode::NOT_FOUND, "not_found"),
            Guarded::App(k::Error::BadText) => (StatusCode::UNPROCESSABLE_ENTITY, "bad_text"),
            Guarded::App(k::Error::Exists) => (StatusCode::CONFLICT, "exists"),
            Guarded::App(k::Error::Overflow) => (StatusCode::INTERNAL_SERVER_ERROR, "overflow"),
        };
        (status, h5i_app_http::error_body(code))
    }
}

type PostTuple = (u64, u64, Vec<u8>, Vec<u8>, bool);

/// Stored replies for idempotent retries.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Created(u64),
    Done,
    Found(PostTuple),
    Posts(Vec<PostTuple>),
    Account(u64, bool),
}

fn tuple(p: &k::Post) -> PostTuple {
    (p.id, p.author, p.title.clone(), p.body.clone(), p.published)
}

fn untuple((id, author, title, body, published): PostTuple) -> k::Post {
    k::Post { id, author, title, body, published }
}

impl ReplyCodec<Forum> for ForumStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        let stored = match r {
            k::Reply::Created(id) => StoredReply::Created(*id),
            k::Reply::Done => StoredReply::Done,
            k::Reply::Found(p) => StoredReply::Found(tuple(p)),
            k::Reply::Posts(ps) => StoredReply::Posts(ps.iter().map(tuple).collect()),
            k::Reply::Account(u) => StoredReply::Account(u.user, u.admin),
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Created(id) => k::Reply::Created(id),
            StoredReply::Done => k::Reply::Done,
            StoredReply::Found(p) => k::Reply::Found(untuple(p)),
            StoredReply::Posts(ps) => k::Reply::Posts(ps.into_iter().map(untuple).collect()),
            StoredReply::Account(user, admin) => k::Reply::Account(k::User { user, admin }),
        })
    }
}
