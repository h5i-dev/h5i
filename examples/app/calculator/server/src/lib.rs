//! The calculator's shell: moves data between the kernel, PostgreSQL and
//! JSON. It decides nothing.

use axum::http::StatusCode;
use calculator_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use serde::Deserialize;

/// Marker type the framework's traits hang off.
pub struct Calc;

impl Kernel for Calc {
    type Principal = k::Principal;
    type Snapshot = k::Snapshot;
    type Command = k::Command;
    type WriteSet = Option<k::Memory>;
    type Reply = k::Reply;
    type Error = k::Error;

    fn tenant(actor: &k::Principal) -> TenantId {
        TenantId(actor.org)
    }

    fn transition(actor: &k::Principal, snap: &k::Snapshot, cmd: &k::Command) -> Result<(Option<k::Memory>, k::Reply), k::Error> {
        k::transition(actor, snap, cmd)
    }

    fn apply(snap: &k::Snapshot, w: &Option<k::Memory>) -> k::Snapshot {
        k::apply(snap, w)
    }
}

pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user }
}

// The `memories` table, from the kernel's `schema!` declaration.
calculator_kernel::calc_tables!(Calc);

pub struct CalcStore;

impl Store<Calc> for CalcStore {
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

    async fn write(tx: &Tx<'_>, t: TenantId, w: &Option<k::Memory>) -> Result<(), DbError> {
        schema_write(tx, t, &k::sql_writes(w)).await
    }
}

/// The JSON a client sends, e.g. `{"cmd":"apply","op":"mul","arg":7}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Set { value: u64 },
    Apply { op: OpJson, arg: u64 },
    Get,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpJson {
    Add,
    Sub,
    Mul,
    Div,
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Set { value } => k::Command::Set { value },
            CommandJson::Apply { op, arg } => {
                let op = match op {
                    OpJson::Add => k::Op::Add,
                    OpJson::Sub => k::Op::Sub,
                    OpJson::Mul => k::Op::Mul,
                    OpJson::Div => k::Op::Div,
                };
                k::Command::Apply { op, arg }
            }
            CommandJson::Get => k::Command::Get,
        }
    }
}

impl h5i_app_http::Api<Calc> for CalcStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Value(v) => Out::obj([("value", (*v).into())]),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let code = match e {
            k::Error::Overflow => "overflow",
            k::Error::Underflow => "underflow",
            k::Error::DivByZero => "division_by_zero",
        };
        (StatusCode::UNPROCESSABLE_ENTITY, h5i_app_http::error_body(code))
    }
}

/// Lets clients retry with an `Idempotency-Key` header.
impl ReplyCodec<Calc> for CalcStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        match r {
            k::Reply::Value(v) => v.to_be_bytes().to_vec(),
        }
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        let b: [u8; 8] = b.try_into().map_err(|_| "stored reply is not 8 bytes".to_string())?;
        Ok(k::Reply::Value(u64::from_be_bytes(b)))
    }
}
