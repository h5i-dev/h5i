//! The booking service's shell: moves data between the kernel, PostgreSQL,
//! JSON and the outbox. It decides nothing; the time is the engine's clock.

use axum::http::StatusCode;
use booking_kernel as k;
use h5i_app_core::{Kernel, TenantId, Timestamp};
use h5i_app_json::Value as Out;
use h5i_app_pg::outbox::{self, Deliver, Delivery};
use h5i_app_pg::{DbError, EngineConfig, ReplyCodec, Store, Tx};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Marker type the framework's traits hang off.
pub struct BookingApp;

impl Kernel for BookingApp {
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

    /// The engine's time, in the kernel's Unix seconds. The kernel trusts it.
    fn stamp(actor: &mut k::Principal, now: Timestamp) {
        actor.now = now.secs();
    }
}

/// The caller of an authenticated request; the engine sets `now` on each attempt.
pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user, now: 0 }
}

/// The database's clock, never going back: every server agrees on the time,
/// and `Clock.started_stays` applies.
pub fn config() -> EngineConfig {
    EngineConfig::default().database_time()
}

// The `admins`, `rooms`, `bookings` and `counters` tables, from the kernel's `schema!`.
booking_kernel::booking_tables!(BookingApp);

/// The outbox payload of a notification. The destination is not in it: the
/// dispatcher looks it up in the registry.
pub fn effect_payload(e: &k::Effect) -> Vec<u8> {
    let event = match e.event {
        k::Event::Booked => "booked",
        k::Event::Cancelled => "cancelled",
    };
    let b = &e.booking;
    Out::obj([
        ("event", Out::str(event)),
        ("booking", b.id.into()),
        ("room", b.room.into()),
        ("user", b.user.into()),
        ("start", b.start_at.into()),
        ("end", b.end_at.into()),
    ])
    .to_bytes()
}

pub struct BookingStore;

impl Store<BookingApp> for BookingStore {
    fn ddl() -> Vec<String> {
        schema_ddl()
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    // Rows are decoded by the kernel's `decode`.
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        schema_load(tx, t).await
    }

    // Table writes go through `sql_writes` (`Storage.lean`). Effects go to the
    // outbox in the same transaction, so they exist iff the change commits.
    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_store(tx, t, ws).await?;
        for w in ws {
            if let k::Write::Emit(e) = w {
                outbox::enqueue(tx, t, e.dest, &effect_payload(e)).await?;
            }
        }
        Ok(())
    }
}

/// The JSON a client sends, e.g. `{"cmd":"book","room":0,"start":1700000000,"end":1700003600}`.
/// There is no field for the time: the engine supplies it.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    AddAdmin { user: u64 },
    CreateRoom { dest: u64 },
    Book { room: u64, start: u64, end: u64 },
    Cancel { id: u64 },
    List,
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::AddAdmin { user } => k::Command::AddAdmin { user },
            CommandJson::CreateRoom { dest } => k::Command::CreateRoom { dest },
            CommandJson::Book { room, start, end } => k::Command::Book { room, start_at: start, end_at: end },
            CommandJson::Cancel { id } => k::Command::Cancel { id },
            CommandJson::List => k::Command::List,
        }
    }
}

fn booking_json(b: &k::Booking) -> Out {
    Out::obj([
        ("id", b.id.into()),
        ("room", b.room.into()),
        ("user", b.user.into()),
        ("start", b.start_at.into()),
        ("end", b.end_at.into()),
    ])
}

impl h5i_app_http::Api<BookingApp> for BookingStore {
    fn decode_command(body: serde_json::Value) -> Result<k::Command, String> {
        serde_json::from_value::<CommandJson>(body).map(Into::into).map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Created(id) => Out::obj([("id", (*id).into())]),
            k::Reply::Done => Out::obj([("ok", true.into())]),
            k::Reply::Bookings(bs) => Out::Arr(bs.iter().map(booking_json).collect()),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let (status, code) = match e {
            k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            k::Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            k::Error::BadInterval => (StatusCode::UNPROCESSABLE_ENTITY, "bad_interval"),
            k::Error::InThePast => (StatusCode::UNPROCESSABLE_ENTITY, "in_the_past"),
            k::Error::Taken => (StatusCode::CONFLICT, "taken"),
            k::Error::Started => (StatusCode::CONFLICT, "started"),
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
    Bookings(Vec<[u64; 5]>),
}

impl ReplyCodec<BookingApp> for BookingStore {
    // Without the time, so a later retry still gets the stored reply.
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
            k::Reply::Bookings(bs) => {
                StoredReply::Bookings(bs.iter().map(|b| [b.id, b.room, b.user, b.start_at, b.end_at]).collect())
            }
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Created(id) => k::Reply::Created(id),
            StoredReply::Done => k::Reply::Done,
            StoredReply::Bookings(bs) => k::Reply::Bookings(
                bs.into_iter()
                    .map(|[id, room, user, start_at, end_at]| k::Booking { id, room, user, start_at, end_at })
                    .collect(),
            ),
        })
    }
}

/// Where a destination id leads. Only the operator's registry creates these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Write the notification to the log.
    Log,
    /// POST it as JSON to `http://host:port/path`.
    Http { host: String, port: u16, path: String },
}

/// Parses `7=log,8=http://127.0.0.1:9000/hooks`.
pub fn parse_registry(spec: &str) -> Result<HashMap<u64, Endpoint>, String> {
    let mut reg = HashMap::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (id, target) = entry.split_once('=').ok_or_else(|| format!("expected <id>=<target> in {entry:?}"))?;
        let id: u64 = id.parse().map_err(|_| format!("bad destination id in {entry:?}"))?;
        let endpoint = if target == "log" {
            Endpoint::Log
        } else {
            let rest = target.strip_prefix("http://").ok_or_else(|| format!("expected log or http:// in {entry:?}"))?;
            let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".into()));
            let (host, port) = authority.rsplit_once(':').ok_or_else(|| format!("expected host:port in {entry:?}"))?;
            let port = port.parse().map_err(|_| format!("bad port in {entry:?}"))?;
            Endpoint::Http { host: host.into(), port, path }
        };
        reg.insert(id, endpoint);
    }
    Ok(reg)
}

/// Delivers notifications. Receivers should drop repeated `Idempotency-Key`s,
/// since delivery is at least once.
pub struct Notifier {
    pub timeout: Duration,
}

impl Default for Notifier {
    fn default() -> Self {
        Notifier { timeout: Duration::from_secs(10) }
    }
}

impl Deliver<Endpoint> for Notifier {
    async fn deliver(&self, endpoint: &Endpoint, d: &Delivery) -> Result<(), String> {
        match endpoint {
            Endpoint::Log => {
                let body = String::from_utf8_lossy(&d.payload);
                tracing::info!(key = %d.key, dest = d.dest, attempt = d.attempt, %body, "notification");
                Ok(())
            }
            Endpoint::Http { host, port, path } => tokio::time::timeout(self.timeout, post(host, *port, path, d))
                .await
                .map_err(|_| "timed out".to_string())?,
        }
    }
}

/// A minimal HTTP/1.1 POST; any 2xx status counts as delivered.
async fn post(host: &str, port: u16, path: &str, d: &Delivery) -> Result<(), String> {
    let mut conn = tokio::net::TcpStream::connect((host, port)).await.map_err(|e| e.to_string())?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Idempotency-Key: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        d.key,
        d.payload.len()
    );
    conn.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
    conn.write_all(&d.payload).await.map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    conn.read_to_end(&mut resp).await.map_err(|e| e.to_string())?;
    let status = resp.split(|&c| c == b' ').nth(1).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.parse::<u16>().ok());
    match status {
        Some(s) if (200..300).contains(&s) => Ok(()),
        Some(s) => Err(format!("receiver answered {s}")),
        None => Err("malformed response".into()),
    }
}
