//! Shell of the example app. Nothing here decides permissions.

use axum::http::StatusCode;
use docs_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_pg::{DbError, PgField, ReplyCodec, Store, Tx, Value};
use serde::{Deserialize, Serialize};
use h5i_app_json::Value as Out;
use h5i_app_pg::sql::Column;

pub struct DocsApp;

impl Kernel for DocsApp {
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

macro_rules! enum_field {
    ($t:ty { $($v:ident = $n:literal),* }) => {
        impl PgField<DocsApp> for $t {
            const KIND: h5i_app_pg::Kind = h5i_app_pg::Kind::Int;
            fn to_value(&self) -> Result<Value, DbError> {
                Ok(Value::Int(match self { $(<$t>::$v => $n),* }))
            }
            fn from_value(v: &Value) -> Result<Self, DbError> {
                match v {
                    $(Value::Int($n) => Ok(<$t>::$v),)*
                    _ => Err(DbError::Decode(format!("bad {}: {v:?}", stringify!($t)))),
                }
            }
        }
    };
}
enum_field!(k::Role { Viewer = 0, Editor = 1, Owner = 2 });
enum_field!(k::Status { Draft = 0, InReview = 1, Approved = 2, Published = 3 });

// Table mappings for every row type, from the kernel's `schema!` block.
docs_kernel::docs_tables!(DocsApp);

/// The tables as the SQL compiler sees them, for tests.
pub fn table_spec() -> Vec<h5i_app_pg::pgsql::Table> {
    schema_spec()
}

/// Outbox payload for a publish notification: ids only, no document content.
pub fn effect_payload(e: &k::Effect) -> Vec<u8> {
    Out::Obj(vec![
        (b"event".to_vec(), Out::Str(b"published".to_vec())),
        (b"project".to_vec(), Out::Num(e.project)),
        (b"doc".to_vec(), Out::Num(e.doc)),
        (b"version".to_vec(), Out::Num(e.version)),
    ])
    .to_bytes()
}

pub struct DocsStore;

fn decode(rows: &k::Rows) -> Result<k::Snapshot, DbError> {
    k::decode(rows).ok_or_else(|| DbError::Decode("stored rows do not decode".into()))
}

impl Store<DocsApp> for DocsStore {
    fn ddl() -> Vec<String> {
        let mut ddl = schema_ddl();
        // Scoped reads filter documents by project.
        ddl.push("CREATE INDEX IF NOT EXISTS documents_by_project ON documents (tenant_id, project)".into());
        ddl
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    // Rows are decoded by the kernel (`decode`, proven in `Load.lean`).
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        let rows = k::Rows {
            counter: schema_rows(tx, t, k::Counter::TABLE).await?,
            projects: schema_rows(tx, t, k::Project::TABLE).await?,
            members: schema_rows(tx, t, k::Member::TABLE).await?,
            documents: schema_rows(tx, t, k::Document::TABLE).await?,
            webhooks: schema_rows(tx, t, k::Webhook::TABLE).await?,
        };
        decode(&rows)
    }

    /// The counter plus every row of the command's project, picked by the
    /// kernel's `scoped_project`. `Scoped.scoped_sound` proves the decoded
    /// result is `Frame.slice` of a snapshot the rows hold, and
    /// `transition_frame` that the kernel's result is the same as on it.
    async fn load_for(tx: &Tx<'_>, t: TenantId, cmd: &k::Command) -> Result<k::Snapshot, DbError> {
        let counter = schema_rows(tx, t, k::Counter::TABLE).await?;
        let scope = k::read_scope(cmd);
        let doc_rows = match scope {
            k::Scope::Document(d) => {
                schema_rows_where(tx, t, k::Document::TABLE, k::docs_tables::Document::id, &Column::to_val(&d)).await?
            }
            _ => Vec::new(),
        };
        let Some(p) = k::scoped_project(&scope, &doc_rows) else {
            return decode(&k::Rows { counter, ..Default::default() });
        };
        // `Scoped.lean` models each query by the column index it filters on.
        let p = Column::to_val(&p);
        decode(&k::Rows {
            counter,
            projects: schema_rows_where(tx, t, k::Project::TABLE, k::docs_tables::Project::id, &p).await?,
            members: schema_rows_where(tx, t, k::Member::TABLE, k::docs_tables::Member::project, &p).await?,
            documents: schema_rows_where(tx, t, k::Document::TABLE, k::docs_tables::Document::project, &p).await?,
            webhooks: schema_rows_where(tx, t, k::Webhook::TABLE, k::docs_tables::Webhook::project, &p).await?,
        })
    }

    // Rows are encoded by the kernel (`sql_writes`, proven to store what
    // `apply` computes); effects go to the outbox.
    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_write(tx, t, &k::sql_writes(ws)).await?;
        for w in ws {
            if let k::Write::Emit(e) = w {
                h5i_app_pg::outbox::enqueue(tx, t, e.dest, &effect_payload(e)).await?;
            }
        }
        Ok(())
    }
}

// ---- JSON wire format (trusted boundary) ----

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoleJson {
    Viewer,
    Editor,
    Owner,
}

impl From<RoleJson> for k::Role {
    fn from(r: RoleJson) -> Self {
        match r {
            RoleJson::Viewer => k::Role::Viewer,
            RoleJson::Editor => k::Role::Editor,
            RoleJson::Owner => k::Role::Owner,
        }
    }
}

impl From<k::Role> for RoleJson {
    fn from(r: k::Role) -> Self {
        match r {
            k::Role::Viewer => RoleJson::Viewer,
            k::Role::Editor => RoleJson::Editor,
            k::Role::Owner => RoleJson::Owner,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandJson {
    CreateProject { name: String },
    SetMember { project: u64, user: u64, role: RoleJson },
    RemoveMember { project: u64, user: u64 },
    CreateDocument { project: u64, title: String, body: String },
    EditDocument { doc: u64, body: String, expected_version: u64 },
    Submit { doc: u64 },
    Approve { doc: u64 },
    Publish { doc: u64 },
    DeleteDocument { doc: u64 },
    GetDocument { doc: u64 },
    ListDocuments { project: u64 },
    SetWebhook { project: u64, dest: Option<u64> },
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        use CommandJson as J;
        let t = |s: String| s.into_bytes();
        match c {
            J::CreateProject { name } => k::Command::CreateProject { name: t(name) },
            J::SetMember { project, user, role } => k::Command::SetMember { project, user, role: role.into() },
            J::RemoveMember { project, user } => k::Command::RemoveMember { project, user },
            J::CreateDocument { project, title, body } => k::Command::CreateDocument { project, title: t(title), body: t(body) },
            J::EditDocument { doc, body, expected_version } => k::Command::EditDocument { doc, body: t(body), expected_version },
            J::Submit { doc } => k::Command::Submit { doc },
            J::Approve { doc } => k::Command::Approve { doc },
            J::Publish { doc } => k::Command::Publish { doc },
            J::DeleteDocument { doc } => k::Command::DeleteDocument { doc },
            J::GetDocument { doc } => k::Command::GetDocument { doc },
            J::ListDocuments { project } => k::Command::ListDocuments { project },
            J::SetWebhook { project, dest } => k::Command::SetWebhook { project, dest },
        }
    }
}

fn status_str(s: k::Status) -> &'static str {
    match s {
        k::Status::Draft => "draft",
        k::Status::InReview => "in_review",
        k::Status::Approved => "approved",
        k::Status::Published => "published",
    }
}

// Titles and bodies come from JSON strings, so they are valid UTF-8 and the
// writer passes them through unchanged.
fn doc_json(d: &k::Document) -> Out {
    Out::obj([
        ("id", d.id.into()),
        ("project", d.project.into()),
        ("author", d.author.into()),
        ("title", Out::Str(d.title.clone())),
        ("body", Out::Str(d.body.clone())),
        ("status", status_str(d.status).into()),
        ("approver", d.approver.into()),
        ("version", d.version.into()),
    ])
}

impl h5i_app_http::Api<DocsApp> for DocsStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Created(id) => Out::obj([("created", (*id).into())]),
            k::Reply::Done => Out::obj([("ok", true.into())]),
            k::Reply::Version(v) => Out::obj([("version", (*v).into())]),
            k::Reply::Doc(d) => doc_json(d),
            k::Reply::Docs(ds) => Out::Arr(ds.iter().map(doc_json).collect()),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let (status, code) = match e {
            k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            k::Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            k::Error::Conflict => (StatusCode::CONFLICT, "version_conflict"),
            k::Error::BadState => (StatusCode::CONFLICT, "bad_state"),
            k::Error::LastOwner => (StatusCode::CONFLICT, "last_owner"),
            k::Error::SelfApproval => (StatusCode::FORBIDDEN, "self_approval"),
            k::Error::Overflow => (StatusCode::INTERNAL_SERVER_ERROR, "overflow"),
        };
        (status, h5i_app_http::error_body(code))
    }
}

/// Lossless reply form for the idempotency table, so any reply can be replayed.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Created(u64),
    Done,
    Version(u64),
    Doc(StoredDoc),
    Docs(Vec<StoredDoc>),
}

#[derive(Serialize, Deserialize)]
struct StoredDoc {
    id: u64,
    project: u64,
    author: u64,
    title: Vec<u8>,
    body: Vec<u8>,
    status: u8,
    approver: Option<u64>,
    version: u64,
}

impl From<&k::Document> for StoredDoc {
    fn from(d: &k::Document) -> Self {
        let status = match d.status {
            k::Status::Draft => 0,
            k::Status::InReview => 1,
            k::Status::Approved => 2,
            k::Status::Published => 3,
        };
        StoredDoc {
            id: d.id,
            project: d.project,
            author: d.author,
            title: d.title.clone(),
            body: d.body.clone(),
            status,
            approver: d.approver,
            version: d.version,
        }
    }
}

impl TryFrom<StoredDoc> for k::Document {
    type Error = String;
    fn try_from(d: StoredDoc) -> Result<Self, String> {
        let status = match d.status {
            0 => k::Status::Draft,
            1 => k::Status::InReview,
            2 => k::Status::Approved,
            3 => k::Status::Published,
            n => return Err(format!("bad status {n}")),
        };
        Ok(k::Document {
            id: d.id,
            project: d.project,
            author: d.author,
            title: d.title,
            body: d.body,
            status,
            approver: d.approver,
            version: d.version,
        })
    }
}

impl ReplyCodec<DocsApp> for DocsStore {
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
            k::Reply::Version(v) => StoredReply::Version(*v),
            k::Reply::Doc(d) => StoredReply::Doc(d.into()),
            k::Reply::Docs(ds) => StoredReply::Docs(ds.iter().map(Into::into).collect()),
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        let stored: StoredReply = serde_json::from_slice(b).map_err(|e| e.to_string())?;
        Ok(match stored {
            StoredReply::Created(id) => k::Reply::Created(id),
            StoredReply::Done => k::Reply::Done,
            StoredReply::Version(v) => k::Reply::Version(v),
            StoredReply::Doc(d) => k::Reply::Doc(d.try_into()?),
            StoredReply::Docs(ds) => k::Reply::Docs(ds.into_iter().map(TryInto::try_into).collect::<Result<_, _>>()?),
        })
    }
}
