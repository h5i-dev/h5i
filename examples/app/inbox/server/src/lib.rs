//! The inbox's shell: moves data between the kernel, PostgreSQL and JSON.
//! It decides nothing.

use axum::http::StatusCode;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use inbox_kernel as k;
use serde::{Deserialize, Serialize};

/// Marker type the framework's traits hang off.
pub struct Inbox;

impl Kernel for Inbox {
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

// The `messages` and `blocks` tables, from the kernel's `schema!`.
inbox_kernel::inbox_tables!(Inbox);

pub struct InboxStore;

impl Store<Inbox> for InboxStore {
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

/// The JSON a client sends, e.g. `{"cmd":"send","to":2,"text":"hello"}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Send { to: u64, text: String },
    Inbox,
    Sent,
    MarkRead { from: u64, seq: u64 },
    Delete { from: u64, to: u64, seq: u64 },
    Block { user: u64 },
    Unblock { user: u64 },
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Send { to, text } => k::Command::Send { to, text: text.into_bytes() },
            CommandJson::Inbox => k::Command::Inbox,
            CommandJson::Sent => k::Command::Sent,
            CommandJson::MarkRead { from, seq } => k::Command::MarkRead { from, seq },
            CommandJson::Delete { from, to, seq } => k::Command::Delete { from, to, seq },
            CommandJson::Block { user } => k::Command::Block { user },
            CommandJson::Unblock { user } => k::Command::Unblock { user },
        }
    }
}

fn message_json(m: &k::Message) -> Out {
    Out::obj([
        ("from", m.sender.into()),
        ("to", m.recipient.into()),
        ("seq", m.seq.into()),
        ("text", Out::Str(m.text.clone())),
        ("read", m.read.into()),
    ])
}

impl h5i_app_http::Api<Inbox> for InboxStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Sent(seq) => Out::obj([("seq", (*seq).into())]),
            k::Reply::Done => Out::obj([("ok", true.into())]),
            k::Reply::Messages(ms) => Out::Arr(ms.iter().map(message_json).collect()),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let (status, code) = match e {
            k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            k::Error::Blocked => (StatusCode::FORBIDDEN, "blocked"),
            k::Error::BadText => (StatusCode::UNPROCESSABLE_ENTITY, "bad_text"),
            k::Error::Overflow => (StatusCode::INTERNAL_SERVER_ERROR, "overflow"),
        };
        (status, h5i_app_http::error_body(code))
    }
}

/// A message as stored with a reply: key, text, then the three flags.
type StoredMessage = (u64, u64, u64, Vec<u8>, bool, bool, bool);

/// Stored replies for idempotent retries.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Sent(u64),
    Done,
    Messages(Vec<StoredMessage>),
}

impl ReplyCodec<Inbox> for InboxStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        let stored = match r {
            k::Reply::Sent(seq) => StoredReply::Sent(*seq),
            k::Reply::Done => StoredReply::Done,
            k::Reply::Messages(ms) => StoredReply::Messages(
                ms.iter()
                    .map(|m| (m.sender, m.recipient, m.seq, m.text.clone(), m.read, m.sender_deleted, m.recipient_deleted))
                    .collect(),
            ),
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Sent(seq) => k::Reply::Sent(seq),
            StoredReply::Done => k::Reply::Done,
            StoredReply::Messages(ms) => k::Reply::Messages(
                ms.into_iter()
                    .map(|(sender, recipient, seq, text, read, sender_deleted, recipient_deleted)| k::Message {
                        sender,
                        recipient,
                        seq,
                        text,
                        read,
                        sender_deleted,
                        recipient_deleted,
                    })
                    .collect(),
            ),
        })
    }
}
