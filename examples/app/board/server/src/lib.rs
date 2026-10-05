//! The board's shell: moves data between the kernel, PostgreSQL and JSON.
//! It decides nothing.

use axum::http::StatusCode;
use board_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use serde::{Deserialize, Serialize};

/// Marker type the framework's traits hang off.
pub struct Board;

impl Kernel for Board {
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

// The `posts`, `moderators` and `counters` tables, from the kernel's `schema!`.
board_kernel::board_tables!(Board);

pub struct BoardStore;

impl Store<Board> for BoardStore {
    fn ddl() -> Vec<String> {
        schema_ddl()
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    // Rows decode with the kernel's `decode`; writes go through `sql_writes`.
    // `Storage.lean` proves the store holds what `apply` computes.
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        schema_load(tx, t).await
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_store(tx, t, ws).await
    }
}

/// The JSON a client sends, e.g. `{"cmd":"publish","text":"hello"}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Publish { text: String },
    Edit { id: u64, text: String },
    Delete { id: u64 },
    List,
    Promote { user: u64 },
    Demote { user: u64 },
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Publish { text } => k::Command::Publish { text: text.into_bytes() },
            CommandJson::Edit { id, text } => k::Command::Edit { id, text: text.into_bytes() },
            CommandJson::Delete { id } => k::Command::Delete { id },
            CommandJson::List => k::Command::List,
            CommandJson::Promote { user } => k::Command::Promote { user },
            CommandJson::Demote { user } => k::Command::Demote { user },
        }
    }
}

fn post_json(p: &k::Post) -> Out {
    Out::obj([("id", p.id.into()), ("author", p.author.into()), ("text", Out::Str(p.text.clone()))])
}

impl h5i_app_http::Api<Board> for BoardStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Created(id) => Out::obj([("id", (*id).into())]),
            k::Reply::Done => Out::obj([("ok", true.into())]),
            k::Reply::Posts(ps) => Out::Arr(ps.iter().map(post_json).collect()),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let (status, code) = match e {
            k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            k::Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            k::Error::BadText => (StatusCode::UNPROCESSABLE_ENTITY, "bad_text"),
            k::Error::LastModerator => (StatusCode::CONFLICT, "last_moderator"),
            k::Error::Overflow => (StatusCode::INTERNAL_SERVER_ERROR, "overflow"),
        };
        (status, h5i_app_http::error_body(code))
    }
}

/// Stored replies for idempotent retries.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Created(u64),
    Done,
    Posts(Vec<(u64, u64, Vec<u8>)>),
}

impl ReplyCodec<Board> for BoardStore {
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
            k::Reply::Posts(ps) => StoredReply::Posts(ps.iter().map(|p| (p.id, p.author, p.text.clone())).collect()),
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Created(id) => k::Reply::Created(id),
            StoredReply::Done => k::Reply::Done,
            StoredReply::Posts(ps) => {
                k::Reply::Posts(ps.into_iter().map(|(id, author, text)| k::Post { id, author, text }).collect())
            }
        })
    }
}
