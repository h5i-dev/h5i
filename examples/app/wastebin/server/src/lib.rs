//! Wastebin's shell: HTTP routes, the uid cookie, random slugs, password
//! fingerprints and the PostgreSQL store. The time comes from the engine's
//! clock. Every decision is the kernel's.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use h5i_app_core::{Kernel, TenantId, Timestamp};
use h5i_app_http::{error_body, reply, AuthError, Authenticator};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, Engine, EngineConfig, ReplyCodec, Store, Tx};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wastebin_kernel as k;

/// Marker type the framework's traits hang off.
pub struct Wastebin;

/// One Wastebin instance is one tenant.
pub const TENANT: TenantId = TenantId(1);

impl Kernel for Wastebin {
    type Principal = k::Principal;
    type Snapshot = k::Snapshot;
    type Command = k::Command;
    type WriteSet = Vec<k::Write>;
    type Reply = k::Reply;
    type Error = k::Error;

    fn tenant(_: &k::Principal) -> TenantId {
        TENANT
    }

    fn transition(actor: &k::Principal, snap: &k::Snapshot, cmd: &k::Command) -> Result<(Vec<k::Write>, k::Reply), k::Error> {
        k::transition(actor, snap, cmd)
    }

    fn apply(snap: &k::Snapshot, ws: &Vec<k::Write>) -> k::Snapshot {
        k::apply(snap, ws)
    }

    /// The engine's time, in the kernel's Unix seconds.
    fn stamp(actor: &mut k::Principal, now: Timestamp) {
        actor.now = now.secs();
    }
}

/// Engine settings: the database clock, monotonic so an expired paste never comes back.
pub fn config() -> EngineConfig {
    EngineConfig::default().database_time()
}

// The `wastebin_pastes` and `wastebin_counters` tables, from the kernel's `schema!`.
wastebin_kernel::wastebin_tables!(Wastebin);

pub struct WastebinStore;

impl Store<Wastebin> for WastebinStore {
    fn ddl() -> Vec<String> {
        schema_ddl()
    }

    fn tables() -> Vec<&'static str> {
        schema_tables()
    }

    // Load with the kernel's `decode`, store its `sql_writes`; `Storage.lean`
    // proves the store then holds what `apply` computes.
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        schema_load(tx, t).await
    }

    async fn write(tx: &Tx<'_>, t: TenantId, ws: &Vec<k::Write>) -> Result<(), DbError> {
        schema_store(tx, t, ws).await
    }
}

/// Stored replies for idempotent retries.
#[derive(Serialize, Deserialize)]
enum StoredReply {
    Created(u64, u64),
    Shown(Vec<u8>, Option<u64>, bool, bool),
    ConfirmBurn,
    Gone,
    Done,
}

impl ReplyCodec<Wastebin> for WastebinStore {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    /// Callers without a uid share one scope, so the routes below never pass
    /// idempotency keys on.
    fn scope(actor: &k::Principal) -> String {
        match actor.uids.first() {
            Some(u) => format!("u{u}"),
            None => "anon".into(),
        }
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        let stored = match r {
            k::Reply::Created { slug, uid } => StoredReply::Created(*slug, *uid),
            k::Reply::Shown(s) => StoredReply::Shown(s.text.clone(), s.expires, s.burned, s.owned),
            k::Reply::ConfirmBurn => StoredReply::ConfirmBurn,
            k::Reply::Gone => StoredReply::Gone,
            k::Reply::Done => StoredReply::Done,
        };
        serde_json::to_vec(&stored).expect("stored replies serialize")
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        Ok(match serde_json::from_slice(b).map_err(|e| e.to_string())? {
            StoredReply::Created(slug, uid) => k::Reply::Created { slug, uid },
            StoredReply::Shown(text, expires, burned, owned) => k::Reply::Shown(k::Shown { text, expires, burned, owned }),
            StoredReply::ConfirmBurn => k::Reply::ConfirmBurn,
            StoredReply::Gone => k::Reply::Gone,
            StoredReply::Done => k::Reply::Done,
        })
    }
}

/// Wastebin's id alphabet; a slug prints as 11 of these characters.
const CHARS: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-+";

pub fn slug_to_str(mut n: u64) -> String {
    let mut out = [0u8; 11];
    for c in out.iter_mut().rev() {
        *c = CHARS[(n & 63) as usize];
        n >>= 6;
    }
    String::from_utf8(out.to_vec()).expect("ascii")
}

pub fn slug_from_str(s: &str) -> Option<u64> {
    // An extension after a dot only picks the highlighting.
    let s = s.split('.').next()?;
    if s.len() != 11 {
        return None;
    }
    let mut n: u64 = 0;
    for b in s.bytes() {
        let d = CHARS.iter().position(|c| *c == b)? as u64;
        n = n.checked_mul(64)?.checked_add(d)?;
    }
    Some(n)
}

fn hmac(secret: &[u8], msg: &[u8]) -> Vec<u8> {
    libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, secret, msg, None)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Supplies what the kernel cannot compute: the caller's uids from the
/// signed cookie, the time, a random slug, and password fingerprints.
pub struct Shell {
    secret: Vec<u8>,
}

impl Shell {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Shell { secret: secret.into() }
    }

    fn uid_tag(&self, uid: u64) -> String {
        hex(&hmac(&self.secret, format!("uid:{uid}").as_bytes())[..16])
    }

    /// The `uid` cookie: `<uid>.<tag>` entries separated by `,`.
    pub fn uid_cookie(&self, uids: &[u64]) -> String {
        let v: Vec<String> = uids.iter().map(|u| format!("{u}.{}", self.uid_tag(*u))).collect();
        format!("uid={}; Path=/; HttpOnly; SameSite=Lax; Secure", v.join(","))
    }

    /// Uids whose signature checks out; tampered entries are dropped.
    pub fn uids(&self, headers: &HeaderMap) -> Vec<u64> {
        let mut out = Vec::new();
        for h in headers.get_all(header::COOKIE) {
            let Ok(h) = h.to_str() else { continue };
            for kv in h.split(';') {
                let Some(v) = kv.trim().strip_prefix("uid=") else { continue };
                for entry in v.split(',') {
                    let Some((u, tag)) = entry.split_once('.') else { continue };
                    let Ok(u) = u.parse::<u64>() else { continue };
                    if ct_eq(self.uid_tag(u).as_bytes(), tag.as_bytes()) && !out.contains(&u) {
                        out.push(u);
                    }
                }
            }
        }
        out
    }

    /// Keyed fingerprint of a password, compared by the kernel. An empty
    /// password means none, as in Wastebin.
    pub fn fingerprint(&self, password: Option<&str>) -> Option<u64> {
        let p = password.filter(|p| !p.is_empty())?;
        let t = hmac(&self.secret, format!("pw:{p}").as_bytes());
        Some(u64::from_le_bytes(t[..8].try_into().expect("8 bytes")))
    }

    pub fn random() -> u64 {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).expect("OS randomness");
        u64::from_le_bytes(b)
    }
}

impl Shell {
    /// Never fails: no cookie means no uids. The engine fills in `now`.
    pub fn principal(&self, headers: &HeaderMap) -> k::Principal {
        k::Principal { uids: self.uids(headers), now: 0, fresh: Shell::random() }
    }
}

impl Authenticator<Wastebin> for Shell {
    fn authenticate(&self, headers: &HeaderMap) -> Result<k::Principal, AuthError> {
        Ok(self.principal(headers))
    }
}

pub type WastebinEngine = Engine<Wastebin, WastebinStore>;

#[derive(Clone)]
pub struct App {
    pub engine: Arc<WastebinEngine>,
    pub shell: Arc<Shell>,
}

fn error_status(e: &k::Error) -> (StatusCode, &'static str) {
    match e {
        k::Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        k::Error::NeedPassword => (StatusCode::UNAUTHORIZED, "password_required"),
        k::Error::WrongPassword => (StatusCode::UNAUTHORIZED, "wrong_password"),
        k::Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        k::Error::SlugTaken => (StatusCode::CONFLICT, "slug_taken"),
        k::Error::BadExpiry => (StatusCode::UNPROCESSABLE_ENTITY, "bad_expiry"),
        k::Error::Overflow => (StatusCode::INTERNAL_SERVER_ERROR, "overflow"),
    }
}

/// Runs a command; `Ok` holds the kernel's reply, `Err` a finished response.
async fn run(app: &App, actor: &k::Principal, cmd: &k::Command) -> Result<k::Reply, Response> {
    finish(app.engine.execute(actor, cmd).await)
}

fn finish(res: Result<Result<k::Reply, k::Error>, DbError>) -> Result<k::Reply, Response> {
    match res {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => {
            let (status, code) = error_status(&e);
            Err(reply(status, error_body(code)))
        }
        Err(e) => {
            tracing::error!(error = %e, "request failed");
            Err(reply(StatusCode::SERVICE_UNAVAILABLE, error_body("unavailable")))
        }
    }
}

fn not_found() -> Response {
    reply(StatusCode::NOT_FOUND, error_body("not_found"))
}

/// The body of `POST /`, as in Wastebin's JSON API.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPaste {
    pub text: String,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub expires: Option<u32>,
    #[serde(default)]
    pub burn_after_reading: Option<bool>,
    #[serde(default)]
    pub password: Option<String>,
}

async fn create(State(app): State<App>, headers: HeaderMap, Json(body): Json<NewPaste>) -> Response {
    let mut actor = app.shell.principal(&headers);
    let cmd = k::Command::Create {
        text: body.text.into_bytes(),
        expires_in: body.expires,
        burn: body.burn_after_reading.unwrap_or(false),
        lock: app.shell.fingerprint(body.password.as_deref()),
    };
    // Wastebin retries a colliding random id up to ten times.
    let mut tries = 0;
    let res = loop {
        match app.engine.execute(&actor, &cmd).await {
            Ok(Err(k::Error::SlugTaken)) if tries < 10 => {
                tries += 1;
                actor.fresh = Shell::random();
            }
            res => break res,
        }
    };
    let (slug, uid) = match finish(res) {
        Ok(k::Reply::Created { slug, uid }) => (slug, uid),
        Ok(_) => return not_found(),
        Err(resp) => return resp,
    };
    let ext = body.extension.map(|e| format!(".{e}")).unwrap_or_default();
    let path = format!("/{}{ext}", slug_to_str(slug));
    let mut resp = reply(StatusCode::OK, Out::obj([("path", Out::str(&path)), ("uid", uid.into())]));
    if !actor.uids.contains(&uid) {
        let mut uids = actor.uids.clone();
        uids.push(uid);
        if let Ok(v) = HeaderValue::from_str(&app.shell.uid_cookie(&uids)) {
            resp.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    resp
}

fn password(headers: &HeaderMap) -> Option<&str> {
    headers.get("wastebin-password").and_then(|v| v.to_str().ok())
}

#[derive(Deserialize, Default)]
pub struct Confirm {
    #[serde(default)]
    pub confirm_burn: Option<String>,
}

fn shown_json(s: &k::Shown) -> Out {
    Out::obj([
        ("text", Out::Str(s.text.clone())),
        ("expires", s.expires.map(Into::into).unwrap_or(Out::Null)),
        ("burned", s.burned.into()),
        ("can_delete", s.owned.into()),
    ])
}

/// `GET /{id}`: a burn-after-reading paste needs `?confirm_burn=1`.
async fn view(State(app): State<App>, Path(id): Path<String>, Query(q): Query<Confirm>, headers: HeaderMap) -> Response {
    let Some(slug) = slug_from_str(&id) else { return not_found() };
    let actor = app.shell.principal(&headers);
    let cmd = k::Command::View {
        slug,
        confirm: q.confirm_burn.as_deref() == Some("1"),
        key: app.shell.fingerprint(password(&headers)),
    };
    match run(&app, &actor, &cmd).await {
        Ok(k::Reply::Shown(s)) => reply(StatusCode::OK, shown_json(&s)),
        Ok(k::Reply::ConfirmBurn) => reply(StatusCode::OK, Out::obj([("confirm_burn", true.into())])),
        Ok(_) => not_found(),
        Err(resp) => resp,
    }
}

async fn fetch(app: &App, id: &str, headers: &HeaderMap) -> Result<k::Shown, Response> {
    let slug = slug_from_str(id).ok_or_else(not_found)?;
    let actor = app.shell.principal(headers);
    let cmd = k::Command::Fetch { slug, key: app.shell.fingerprint(password(headers)) };
    match run(app, &actor, &cmd).await? {
        k::Reply::Shown(s) => Ok(s),
        _ => Err(not_found()),
    }
}

/// `GET /raw/{id}`: the text alone.
async fn raw(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    match fetch(&app, &id, &headers).await {
        Ok(s) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], s.text).into_response(),
        Err(resp) => resp,
    }
}

/// `GET /dl/{id}`: the text as an attachment.
async fn download(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let name: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')).collect();
    match fetch(&app, &id, &headers).await {
        Ok(s) => (
            [
                (header::CONTENT_TYPE, "text; charset=utf-8".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            ],
            s.text,
        )
            .into_response(),
        Err(resp) => resp,
    }
}

/// `DELETE /{id}`: only with a uid cookie that owns the paste.
async fn remove(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let Some(slug) = slug_from_str(&id) else { return reply(StatusCode::FORBIDDEN, error_body("forbidden")) };
    let actor = app.shell.principal(&headers);
    match run(&app, &actor, &k::Command::Delete { slug }).await {
        Ok(_) => reply(StatusCode::OK, Out::obj([("ok", true.into())])),
        Err(resp) => resp,
    }
}

/// Deletes every expired paste, like `wastebin-ctl purge`.
pub async fn purge(engine: &WastebinEngine) -> Result<(), DbError> {
    let actor = k::Principal { uids: Vec::new(), now: 0, fresh: 0 };
    let _ = engine.execute(&actor, &k::Command::Purge).await?;
    Ok(())
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // h5i-allow: no-actor (signed-cookie auth in Shell)
        .route("/", axum::routing::post(create))
        // h5i-allow: no-actor (signed-cookie auth in Shell)
        .route("/{id}", get(view).post(view).delete(remove))
        .route("/raw/{id}", get(raw))
        .route("/dl/{id}", get(download))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip() {
        for n in [0, 1, 63, 64, u64::MAX, 0x1234_5678_9abc_def0] {
            assert_eq!(slug_from_str(&slug_to_str(n)), Some(n));
        }
        assert_eq!(slug_from_str("short"), None);
        assert_eq!(slug_from_str(&format!("{}.rs", slug_to_str(5))), Some(5));
    }

    #[test]
    fn cookies_are_signed() {
        let shell = Shell::new(b"secret".to_vec());
        let cookie = shell.uid_cookie(&[4, 9]);
        let value = cookie.split(';').next().unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        assert_eq!(shell.uids(&h), vec![4, 9]);
        let forged = value.replacen("4.", "5.", 1);
        h.insert(header::COOKIE, HeaderValue::from_str(&forged).unwrap());
        assert_eq!(shell.uids(&h), vec![9]);
        assert_eq!(shell.fingerprint(Some("")), None);
        assert_eq!(shell.fingerprint(Some("pw")), shell.fingerprint(Some("pw")));
    }
}
