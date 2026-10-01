//! Integration between h5i-app and axum.
//!
//! A handler takes an authenticated [`Actor`], builds a kernel command, and
//! calls [`H5iApp::respond`]. For plain JSON commands, merge [`rpc_router`].
//!
//! Request decoding is trusted, but the kernel's theorems hold for every
//! command, so a bad decoding can't do more than a client could ask for.
//! Token parsing (`h5i-app-token`) and reply rendering (`h5i-app-json`) are verified.
//!
//! # Example
//!
//! ```ignore
//! async fn approve(State(app): State<App>, actor: Actor<DocsApp>, Path(doc): Path<u64>, h: HeaderMap) -> Response {
//!     app.respond(&actor, Command::Approve { doc }, &h).await
//! }
//!
//! let router = Router::new()
//!     .route("/documents/{id}/approve", post(approve))
//!     .with_state(app.clone())
//!     .merge(rpc_router(app));
//! ```

use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use h5i_app_core::Kernel;
use h5i_app_pg::{DbError, Engine, ReplyCodec, Store};
use h5i_app_json::Value;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct AuthError(pub String);

pub trait Authenticator<K: Kernel>: Send + Sync + 'static {
    fn authenticate(&self, headers: &HeaderMap) -> Result<K::Principal, AuthError>;
}

pub trait Api<K: Kernel>: Send + Sync + 'static {
    fn decode_command(body: serde_json::Value) -> Result<K::Command, String>;
    fn encode_reply(reply: &K::Reply) -> Value;
    fn encode_error(err: &K::Error) -> (StatusCode, Value);
}

type MakePrincipal<K> = Box<dyn Fn(u64, u64) -> <K as Kernel>::Principal + Send + Sync>;

/// Tokens of the form `v1.<tenant>.<user>.<expiry-unix>.<hex hmac-sha256>`,
/// sent as `Authorization: Bearer <token>`.
pub struct HmacAuth<K: Kernel> {
    secret: Vec<u8>,
    principal: MakePrincipal<K>,
    scheme: &'static str,
    anonymous: Option<K::Principal>,
}

impl<K: Kernel> HmacAuth<K> {
    /// `principal` runs once per request, before any retry, so it can carry
    /// per-request inputs like a random slug (not the time: the engine reads
    /// that per attempt for `Kernel::stamp`). Expiry uses the system clock.
    pub fn new(secret: impl Into<Vec<u8>>, principal: impl Fn(u64, u64) -> K::Principal + Send + Sync + 'static) -> Self {
        HmacAuth { secret: secret.into(), principal: Box::new(principal), scheme: "Bearer", anonymous: None }
    }

    /// Accept `Authorization: <scheme> <token>` instead of `Bearer`.
    pub fn with_scheme(mut self, scheme: &'static str) -> Self {
        self.scheme = scheme;
        self
    }

    /// Requests without an `Authorization` header act as `p`.
    pub fn or_anonymous(mut self, p: K::Principal) -> Self {
        self.anonymous = Some(p);
        self
    }

    fn tag(&self, payload: &[u8]) -> Vec<u8> {
        libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, &self.secret, payload, None)
    }

    pub fn issue(&self, tenant: u64, user: u64, ttl_secs: u64) -> String {
        let payload = h5i_app_token::encode_payload(tenant, user, now() + ttl_secs);
        let token = h5i_app_token::join(&payload, &self.tag(&payload));
        String::from_utf8(token).expect("tokens are ascii")
    }

    /// The tenant and user of a valid token. Parsing is verified in `h5i-app-token/proofs`.
    pub fn verify(&self, token: &str) -> Result<(u64, u64), AuthError> {
        let err = |m: &str| AuthError(m.to_string());
        let t = h5i_app_token::parse(token.as_bytes()).ok_or_else(|| err("malformed token"))?;
        if !ct_eq(&self.tag(&t.payload), &t.sig) {
            return Err(err("bad signature"));
        }
        if t.exp < now() {
            return Err(err("token expired"));
        }
        Ok((t.tenant, t.user))
    }
}

/// Constant-time equality, so the comparison does not leak the tag.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl<K: Kernel> Authenticator<K> for HmacAuth<K> {
    fn authenticate(&self, headers: &HeaderMap) -> Result<K::Principal, AuthError> {
        let Some(h) = headers.get("authorization") else {
            return self.anonymous.clone().ok_or_else(|| AuthError("missing authorization".into()));
        };
        let h = h.to_str().map_err(|_| AuthError("bad authorization header".into()))?;
        let token = h
            .strip_prefix(self.scheme)
            .and_then(|t| t.strip_prefix(' '))
            .ok_or_else(|| AuthError(format!("expected {} token", self.scheme)))?;
        let (tenant, user) = self.verify(token)?;
        Ok((self.principal)(tenant, user))
    }
}

impl<K: Kernel, A: Authenticator<K>> Authenticator<K> for Arc<A> {
    fn authenticate(&self, headers: &HeaderMap) -> Result<K::Principal, AuthError> {
        (**self).authenticate(headers)
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Shared authenticator, extracted from your axum state via `FromRef`.
pub struct Auth<K: Kernel>(pub Arc<dyn Authenticator<K>>);

impl<K: Kernel> Clone for Auth<K> {
    fn clone(&self) -> Self {
        Auth(self.0.clone())
    }
}

/// The authenticated caller, an axum extractor. The principal is private, so
/// the only way to get one is the extractor, which runs the [`Authenticator`].
pub struct Actor<K: Kernel>(K::Principal);

impl<K: Kernel> Actor<K> {
    pub fn principal(&self) -> &K::Principal {
        &self.0
    }

    /// Wrap a principal without the extractor. The name is greppable; use it
    /// only when the server authenticates its own way.
    pub fn assume_authenticated(principal: K::Principal) -> Self {
        Actor(principal)
    }
}

impl<K: Kernel, St: Send + Sync> FromRequestParts<St> for Actor<K>
where
    Auth<K>: FromRef<St>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &St) -> Result<Self, Self::Rejection> {
        let auth = Auth::<K>::from_ref(state);
        auth.0.authenticate(&parts.headers).map(Actor).map_err(|AuthError(m)| {
            reply(StatusCode::UNAUTHORIZED, error_body(&m))
        })
    }
}

/// Engine handle for handlers; the only path to the database, so every route goes through the kernel.
pub struct H5iApp<K: Kernel, S: Store<K>> {
    engine: Arc<Engine<K, S>>,
    auth: Auth<K>,
}

impl<K: Kernel, S: Store<K>> Clone for H5iApp<K, S> {
    fn clone(&self) -> Self {
        H5iApp { engine: self.engine.clone(), auth: self.auth.clone() }
    }
}

impl<K: Kernel, S: Store<K>> FromRef<H5iApp<K, S>> for Auth<K> {
    fn from_ref(i: &H5iApp<K, S>) -> Self {
        i.auth.clone()
    }
}

impl<K: Kernel, S: Store<K>> H5iApp<K, S> {
    pub fn new(engine: Arc<Engine<K, S>>, auth: impl Authenticator<K>) -> Self {
        H5iApp { engine, auth: Auth(Arc::new(auth)) }
    }

    pub fn engine(&self) -> &Engine<K, S> {
        &self.engine
    }

    /// Run `cmd` and render the result. Honors the `Idempotency-Key` header.
    pub async fn respond(&self, actor: &Actor<K>, cmd: K::Command, headers: &HeaderMap) -> Response
    where
        S: ReplyCodec<K> + Api<K>,
    {
        match self.run(actor, cmd, headers).await {
            Ok(r) => reply(StatusCode::OK, S::encode_reply(&r)),
            Err(res) => res,
        }
    }

    /// Like [`respond`](Self::respond), but returns a successful reply
    /// unrendered (e.g. to set a cookie). Errors come back rendered.
    // The error is an axum `Response`, which handlers return as is; boxing it
    // would make every caller unbox it.
    #[allow(clippy::result_large_err)]
    pub async fn run(&self, actor: &Actor<K>, cmd: K::Command, headers: &HeaderMap) -> Result<K::Reply, Response>
    where
        S: ReplyCodec<K> + Api<K>,
    {
        let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
        let result = match key {
            Some(k) => self.engine.execute_idempotent(actor.principal(), k, &cmd).await,
            None => self.engine.execute(actor.principal(), &cmd).await,
        };
        match result {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(refusal)) => {
                let (status, body) = S::encode_error(&refusal);
                Err(reply(status, body))
            }
            Err(DbError::IdempotencyConflict) => Err(reply(StatusCode::UNPROCESSABLE_ENTITY, error_body("idempotency key reused"))),
            Err(e) => {
                tracing::error!(error = %e, "request failed");
                Err(reply(StatusCode::SERVICE_UNAVAILABLE, error_body("unavailable")))
            }
        }
    }
}

/// Ready-made `POST /rpc` taking a JSON command. Merge or nest it into your router.
pub fn rpc_router<K, S>(app: H5iApp<K, S>) -> Router
where
    K: Kernel,
    S: Store<K> + ReplyCodec<K> + Api<K>,
{
    Router::new().route("/rpc", post(rpc::<K, S>)).with_state(app)
}

async fn rpc<K, S>(
    State(app): State<H5iApp<K, S>>,
    actor: Actor<K>,
    headers: HeaderMap,
    body: Json<serde_json::Value>,
) -> Response
where
    K: Kernel,
    S: Store<K> + ReplyCodec<K> + Api<K>,
{
    match S::decode_command(body.0) {
        Ok(cmd) => app.respond(&actor, cmd, &headers).await,
        Err(m) => reply(StatusCode::BAD_REQUEST, error_body(&m)),
    }
}

/// Every response body goes through the verified writer (`h5i-app-json`).
pub fn reply(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_bytes()).into_response()
}

/// `{"error": <message>}`.
pub fn error_body(msg: &str) -> Value {
    Value::obj([("error", Value::str(msg))])
}
