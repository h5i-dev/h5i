//! The ledger's shell: moves data between the kernel, PostgreSQL and JSON.
//! It decides nothing.

use axum::http::StatusCode;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use ledger_kernel as k;
use serde::{Deserialize, Serialize};

/// Marker type the framework's traits hang off.
pub struct Ledger;

impl Kernel for Ledger {
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

// The `accounts` and `ledger` tables, from the kernel's `schema!`.
ledger_kernel::ledger_tables!(Ledger);

pub struct LedgerStore;

impl Store<Ledger> for LedgerStore {
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

/// The JSON a client sends, e.g. `{"cmd":"deposit","account":0,"amount":100}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Open,
    Deposit { account: u64, amount: u64 },
    Withdraw { account: u64, amount: u64 },
    Transfer { from: u64, to: u64, amount: u64 },
    List,
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Open => k::Command::Open,
            CommandJson::Deposit { account, amount } => k::Command::Deposit { account, amount },
            CommandJson::Withdraw { account, amount } => k::Command::Withdraw { account, amount },
            CommandJson::Transfer { from, to, amount } => k::Command::Transfer { src: from, dst: to, amount },
            CommandJson::List => k::Command::List,
        }
    }
}

fn account_json(a: &k::Account) -> Out {
    Out::obj([("id", a.id.into()), ("owner", a.owner.into()), ("balance", a.balance.into())])
}

impl h5i_app_http::Api<Ledger> for LedgerStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Created(id) => Out::obj([("id", (*id).into())]),
            k::Reply::Balance(b) => Out::obj([("balance", (*b).into())]),
            k::Reply::Accounts(accs) => Out::Arr(accs.iter().map(account_json).collect()),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let (status, code) = match e {
            k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            k::Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            k::Error::Insufficient => (StatusCode::CONFLICT, "insufficient_funds"),
            k::Error::SameAccount => (StatusCode::UNPROCESSABLE_ENTITY, "same_account"),
            k::Error::Overflow => (StatusCode::UNPROCESSABLE_ENTITY, "overflow"),
        };
        (status, h5i_app_http::error_body(code))
    }
}

/// Stored replies for idempotent retries.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Created(u64),
    Balance(u64),
    Accounts(Vec<(u64, u64, u64)>),
}

impl ReplyCodec<Ledger> for LedgerStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        let stored = match r {
            k::Reply::Created(id) => StoredReply::Created(*id),
            k::Reply::Balance(b) => StoredReply::Balance(*b),
            k::Reply::Accounts(accs) => StoredReply::Accounts(accs.iter().map(|a| (a.id, a.owner, a.balance)).collect()),
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Created(id) => k::Reply::Created(id),
            StoredReply::Balance(b) => k::Reply::Balance(b),
            StoredReply::Accounts(accs) => {
                k::Reply::Accounts(accs.into_iter().map(|(id, owner, balance)| k::Account { id, owner, balance }).collect())
            }
        })
    }
}
