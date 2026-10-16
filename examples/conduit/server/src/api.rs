//! The RealWorld API (`/api/...`): each handler turns a request into a
//! kernel command and renders the reply in RealWorld's JSON shapes.
//!
//! Besides decoding, the shell does what the kernel cannot: it checks that
//! required fields are present and not blank (RealWorld's `422`s), hashes and
//! verifies passwords with bcrypt, signs and verifies JWTs, turns titles into
//! slugs (trying `-1`, `-2`, ... while the kernel answers `SlugTaken`), reads
//! the clock, and counts a listing's articles with the read the kernel names
//! (`article_count`). Every decision about the state is the kernel's.

use crate::{principal, Conduit, ConduitStore, ARTICLE_COUNT, TENANT};
use axum::body::Bytes;
use axum::extract::{FromRef, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use conduit_kernel as k;
use h5i_app_http::{reply, Actor, Auth, AuthError, Authenticator};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, Engine};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tokens last 60 days.
pub const TOKEN_SECS: u64 = 60 * 24 * 3600;
/// bcrypt's work factor, as the benchmark's other servers.
pub const BCRYPT_COST: u32 = 12;
/// NIST 800-63B: at least 8 characters, no composition rules.
pub const MIN_PASSWORD: usize = 8;
/// Page size bounds of the listing and the feed.
const DEFAULT_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 100;
/// How many slug suffixes to try before giving up with `409`.
const SLUG_TRIES: u64 = 1000;

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
}

/// HS256 JWTs whose `sub` is the user id, in `Authorization: Token <jwt>`.
/// A request without the header is anonymous (user 0), which the kernel
/// refuses wherever an account is needed.
pub struct ConduitAuth {
    enc: EncodingKey,
    dec: DecodingKey,
    validation: Validation,
}

impl ConduitAuth {
    pub fn new(secret: &[u8]) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        validation.set_required_spec_claims(&["exp", "sub"]);
        ConduitAuth { enc: EncodingKey::from_secret(secret), dec: DecodingKey::from_secret(secret), validation }
    }

    pub fn issue(&self, user: u64) -> String {
        let claims = Claims { sub: user.to_string(), exp: now_ms() / 1000 + TOKEN_SECS };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.enc).expect("HS256 signing does not fail")
    }
}

impl Authenticator<Conduit> for ConduitAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<k::Principal, AuthError> {
        let invalid = || AuthError("is invalid".into());
        let Some(h) = headers.get("authorization") else {
            return Ok(principal(TENANT, 0));
        };
        let h = h.to_str().map_err(|_| invalid())?;
        if h.trim().is_empty() {
            return Ok(principal(TENANT, 0));
        }
        let token = h.strip_prefix("Token ").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(invalid)?;
        let data = jsonwebtoken::decode::<Claims>(token, &self.dec, &self.validation).map_err(|_| invalid())?;
        match data.claims.sub.parse::<u64>() {
            Ok(user) if user > 0 => Ok(principal(TENANT, user)),
            _ => Err(invalid()),
        }
    }
}

/// bcrypt with a fresh salt, off the async workers.
pub async fn hash_password(password: String) -> Vec<u8> {
    tokio::task::spawn_blocking(move || bcrypt::hash(password, BCRYPT_COST).expect("bcrypt hashes any password").into_bytes())
        .await
        .expect("hashing task")
}

/// Checks `password` against a stored hash; an unknown user is checked
/// against a dummy hash, so both take the same time.
async fn password_matches(password: String, stored: Option<Vec<u8>>) -> bool {
    const DUMMY: &str = "$2b$12$iqGgOt4pkKoY.6s31.69rOVX2v.VemPSDFrDUk/AzZdKwqoXHjy8S";
    tokio::task::spawn_blocking(move || {
        let (hash, known) = match &stored {
            Some(h) => (String::from_utf8_lossy(h).into_owned(), true),
            None => (DUMMY.to_string(), false),
        };
        bcrypt::verify(password, &hash).unwrap_or(false) && known
    })
    .await
    .unwrap_or(false)
}

/// The benchmark's slug rule: ASCII letters and digits, lowercased; spaces,
/// tabs, newlines, `-` and `_` become single hyphens; anything else is
/// dropped; `article` if nothing is left.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut hyphen = true;
    for c in title.bytes() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c as char);
            hyphen = false;
        } else if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'-' | b'_') && !hyphen {
            out.push('-');
            hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "article".into() } else { out }
}

/// The `n`-th slug to try for `base`: `base`, `base-1`, `base-2`, ...
fn with_suffix(base: &str, n: u64) -> String {
    if n == 0 { base.to_string() } else { format!("{base}-{n}") }
}

/// Milliseconds since 1970; articles and comments store them.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Milliseconds since 1970 as `2026-09-27T12:00:00.000Z`.
pub fn iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}

#[derive(Clone)]
pub struct App {
    pub engine: Arc<BenchEngine>,
    pub auth: Arc<ConduitAuth>,
}

impl FromRef<App> for Auth<Conduit> {
    fn from_ref(app: &App) -> Self {
        Auth(app.auth.clone())
    }
}

/// A bad token gets RealWorld's `401` here, before a handler's `Actor`
/// extractor (the same `ConduitAuth`) would answer with h5i-app-http's.
async fn token_errors(State(app): State<App>, req: Request, next: Next) -> Response {
    match app.auth.authenticate(req.headers()) {
        Ok(_) => next.run(req).await,
        Err(AuthError(m)) => errors(StatusCode::UNAUTHORIZED, "token", &m),
    }
}

type Caller = Actor<Conduit>;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/users", post(register))
        .route("/api/users/login", post(login))
        .route("/api/user", get(current_user).put(update_user))
        .route("/api/profiles/{username}", get(get_profile))
        .route("/api/profiles/{username}/follow", post(follow).delete(unfollow))
        .route("/api/articles", get(list_articles).post(create_article))
        .route("/api/articles/feed", get(feed))
        .route("/api/articles/{slug}", get(get_article).put(update_article).delete(delete_article))
        .route("/api/articles/{slug}/favorite", post(favorite).delete(unfavorite))
        .route("/api/articles/{slug}/comments", get(get_comments).post(add_comment))
        .route("/api/articles/{slug}/comments/{id}", delete(delete_comment))
        .route("/api/tags", get(get_tags))
        .layer(middleware::from_fn_with_state(app.clone(), token_errors))
        .with_state(app)
}

/* Errors */

/// `{"errors": {field: [msg]}}`.
fn errors(status: StatusCode, field: &str, msg: &str) -> Response {
    reply(status, Out::obj([("errors", Out::obj([(field, Out::Arr(vec![Out::str(msg)]))]))]))
}

fn blank(field: &str) -> Response {
    errors(StatusCode::UNPROCESSABLE_ENTITY, field, "can't be blank")
}

fn invalid(field: &str) -> Response {
    errors(StatusCode::UNPROCESSABLE_ENTITY, field, "is invalid")
}

/// What a `NotFound` or `Forbidden` of the kernel is about.
#[derive(Clone, Copy)]
enum Subject {
    Profile,
    Article,
}

fn refusal(e: k::Error, caller: &k::Principal, subject: Subject) -> Response {
    use k::Error::*;
    let what = match subject {
        Subject::Profile => "profile",
        Subject::Article => "article",
    };
    match e {
        // The kernel found no account for the caller: no token, or one for a
        // user who does not exist.
        Unauthorized if caller.user == 0 => errors(StatusCode::UNAUTHORIZED, "token", "is missing"),
        Unauthorized => errors(StatusCode::UNAUTHORIZED, "token", "is invalid"),
        WrongPassword | UnknownEmail => errors(StatusCode::UNAUTHORIZED, "credentials", "invalid"),
        NotFound => errors(StatusCode::NOT_FOUND, what, "not found"),
        CommentNotFound => errors(StatusCode::NOT_FOUND, "comment", "not found"),
        Forbidden => errors(StatusCode::FORBIDDEN, what, "forbidden"),
        UsernameTaken => errors(StatusCode::CONFLICT, "username", "has already been taken"),
        EmailTaken => errors(StatusCode::CONFLICT, "email", "has already been taken"),
        SlugTaken => errors(StatusCode::CONFLICT, "slug", "has already been taken"),
        Overflow => errors(StatusCode::INTERNAL_SERVER_ERROR, "body", "overflow"),
    }
}

/// Runs `cmd` for the caller, honoring the `Idempotency-Key` header. The
/// outer error is the engine's; the inner one the kernel's refusal.
async fn run(app: &App, caller: &Caller, cmd: &k::Command, h: &HeaderMap) -> Result<Result<k::Reply, k::Error>, Response> {
    let key = h.get("idempotency-key").and_then(|v| v.to_str().ok());
    let result = match key {
        Some(key) => app.engine.execute_idempotent(caller.principal(), key, cmd).await,
        None => app.engine.execute(caller.principal(), cmd).await,
    };
    match result {
        Ok(r) => Ok(r),
        Err(DbError::IdempotencyConflict) => Err(errors(StatusCode::UNPROCESSABLE_ENTITY, "body", "idempotency key reused")),
        Err(e) => {
            tracing::error!(error = %e, "request failed");
            Err(errors(StatusCode::SERVICE_UNAVAILABLE, "body", "unavailable"))
        }
    }
}

async fn respond(app: &App, caller: &Caller, cmd: k::Command, h: &HeaderMap, subject: Subject, ok: StatusCode) -> Response {
    match run(app, caller, &cmd, h).await {
        Ok(Ok(r)) => render(app, &r, ok, None),
        Ok(Err(e)) => refusal(e, caller.principal(), subject),
        Err(resp) => resp,
    }
}

/* Rendering */

fn text(t: &[u8]) -> Out {
    Out::Str(t.to_vec())
}

/// An empty bio or image is `null`.
fn nullable(t: &[u8]) -> Out {
    if t.is_empty() { Out::Null } else { text(t) }
}

fn profile_json(p: &k::Profile) -> Out {
    Out::obj([
        ("username", text(&p.username)),
        ("bio", nullable(&p.bio)),
        ("image", nullable(&p.image)),
        ("following", p.following.into()),
    ])
}

/// Listings leave out the body.
fn article_json(v: &k::ArticleView, body: bool) -> Out {
    let a = &v.article;
    let mut fields = vec![
        ("slug", text(&a.slug)),
        ("title", text(&a.title)),
        ("description", text(&a.description)),
    ];
    if body {
        fields.push(("body", text(&a.body)));
    }
    fields.extend([
        ("tagList", Out::Arr(v.tags.iter().map(|t| text(t)).collect())),
        ("createdAt", Out::str(iso8601(a.created))),
        ("updatedAt", Out::str(iso8601(a.updated))),
        ("favorited", v.favorited.into()),
        ("favoritesCount", v.favorites.into()),
        ("author", profile_json(&v.author)),
    ]);
    Out::obj(fields)
}

fn comment_json(c: &k::CommentView) -> Out {
    Out::obj([
        ("id", c.comment.id.into()),
        ("createdAt", Out::str(iso8601(c.comment.created))),
        ("updatedAt", Out::str(iso8601(c.comment.created))),
        ("body", text(&c.comment.body)),
        ("author", profile_json(&c.author)),
    ])
}

/// Renders a reply; accounts get a fresh token. `count` is a listing's
/// `articlesCount`, the number of articles before paging.
fn render(app: &App, r: &k::Reply, ok: StatusCode, count: Option<u64>) -> Response {
    let body = match r {
        k::Reply::Account(a) => Out::obj([(
            "user",
            Out::obj([
                ("email", text(&a.email)),
                ("token", Out::str(app.auth.issue(a.id))),
                ("username", text(&a.username)),
                ("bio", nullable(&a.bio)),
                ("image", nullable(&a.image)),
            ]),
        )]),
        k::Reply::Profile(p) => Out::obj([("profile", profile_json(p))]),
        k::Reply::Article(v) => Out::obj([("article", article_json(v, true))]),
        k::Reply::Articles(vs) => Out::obj([
            ("articles", Out::Arr(vs.iter().map(|v| article_json(v, false)).collect())),
            ("articlesCount", count.unwrap_or(vs.len() as u64).into()),
        ]),
        k::Reply::Comment(c) => Out::obj([("comment", comment_json(c))]),
        k::Reply::Comments(cs) => Out::obj([("comments", Out::Arr(cs.iter().map(comment_json).collect()))]),
        k::Reply::Tags(ts) => {
            let mut ts = ts.clone();
            ts.sort();
            Out::obj([("tags", Out::Arr(ts.iter().map(|t| text(t)).collect()))])
        }
        k::Reply::Done => return StatusCode::NO_CONTENT.into_response(),
    };
    reply(ok, body)
}

/* Request bodies */

/// A field of a request object: absent, `null`, `""`, or a string.
enum Field {
    Absent,
    Null,
    Blank,
    Value(String),
}

fn field(obj: &Map<String, Json>, key: &str) -> Result<Field, Response> {
    match obj.get(key) {
        None => Ok(Field::Absent),
        Some(Json::Null) => Ok(Field::Null),
        Some(Json::String(s)) if s.is_empty() => Ok(Field::Blank),
        Some(Json::String(s)) => Ok(Field::Value(s.clone())),
        Some(_) => Err(invalid(key)),
    }
}

/// A field that must be given and not blank.
fn required(obj: &Map<String, Json>, key: &str) -> Result<String, Response> {
    match field(obj, key)? {
        Field::Value(s) => Ok(s),
        _ => Err(blank(key)),
    }
}

/// A field that may be left out but not cleared.
fn optional_required(obj: &Map<String, Json>, key: &str) -> Result<Option<String>, Response> {
    match field(obj, key)? {
        Field::Absent => Ok(None),
        Field::Value(s) => Ok(Some(s)),
        Field::Null | Field::Blank => Err(blank(key)),
    }
}

/// A field that may be left out (`None`) or cleared (`Some("")`).
fn optional(obj: &Map<String, Json>, key: &str) -> Result<Option<String>, Response> {
    match field(obj, key)? {
        Field::Absent => Ok(None),
        Field::Null | Field::Blank => Ok(Some(String::new())),
        Field::Value(s) => Ok(Some(s)),
    }
}

fn check_password(p: &str) -> Result<(), Response> {
    if p.chars().count() < MIN_PASSWORD {
        return Err(errors(StatusCode::UNPROCESSABLE_ENTITY, "password", "is too short (minimum is 8 characters)"));
    }
    Ok(())
}

/// The object under `key` of a JSON body (`{"user": {...}}`).
fn wrapped(body: &Bytes, key: &str) -> Result<Map<String, Json>, Response> {
    let v: Json = serde_json::from_slice(body).map_err(|_| invalid("body"))?;
    match v {
        Json::Object(mut m) => match m.remove(key) {
            Some(Json::Object(o)) => Ok(o),
            _ => Err(invalid(key)),
        },
        _ => Err(invalid("body")),
    }
}

/// A `tagList`: absent, or a list of strings, blanks dropped, each once,
/// sorted (the order the tags table loads them in).
fn tag_list(obj: &Map<String, Json>) -> Result<Option<Vec<Vec<u8>>>, Response> {
    let Some(raw) = obj.get("tagList") else { return Ok(None) };
    let Json::Array(items) = raw else { return Err(invalid("tagList")) };
    let mut tags = Vec::new();
    for item in items {
        let Json::String(s) = item else { return Err(invalid("tagList")) };
        if !s.is_empty() {
            tags.push(s.clone().into_bytes());
        }
    }
    tags.sort();
    tags.dedup();
    Ok(Some(tags))
}

/// `limit` and `offset` of the listing and the feed.
fn page(q: &HashMap<String, String>) -> Result<(u64, u64), Response> {
    let limit = match q.get("limit") {
        None => DEFAULT_LIMIT,
        Some(s) => match s.parse::<i64>() {
            Ok(n) if n >= 1 => (n as u64).min(MAX_LIMIT),
            _ => return Err(invalid("limit")),
        },
    };
    let offset = match q.get("offset") {
        None => 0,
        Some(s) => match s.parse::<i64>() {
            Ok(n) if n >= 0 => n as u64,
            _ => return Err(invalid("offset")),
        },
    };
    Ok((limit, offset))
}

fn bytes(s: String) -> Vec<u8> {
    s.into_bytes()
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

/* Handlers */

async fn register(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, body: Bytes) -> Response {
    let u = tri!(wrapped(&body, "user"));
    let username = tri!(required(&u, "username"));
    let email = tri!(required(&u, "email"));
    let password = tri!(required(&u, "password"));
    tri!(check_password(&password));
    let cmd = k::Command::Register { username: bytes(username), email: bytes(email), password: hash_password(password).await };
    respond(&app, &caller, cmd, &h, Subject::Profile, StatusCode::CREATED).await
}

/// The kernel decides a login by comparing the hash it stores with the one
/// the shell presents. The shell reads the stored hash with the login's own
/// read plan, checks the password against it with bcrypt, and presents it
/// only if it matches.
async fn login(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, body: Bytes) -> Response {
    let u = tri!(wrapped(&body, "user"));
    let email = bytes(tri!(required(&u, "email")));
    let password = tri!(required(&u, "password"));
    let lookup = k::Command::Login { email: email.clone(), password: Vec::new() };
    let stored = match app.engine.snapshot_for(caller.principal(), &lookup).await {
        Ok(snap) => k::find_user_by_email(&snap.users, &email).map(|u| u.password),
        Err(e) => {
            tracing::error!(error = %e, "login lookup failed");
            return errors(StatusCode::SERVICE_UNAVAILABLE, "body", "unavailable");
        }
    };
    let presented = match password_matches(password, stored.clone()).await {
        true => stored.unwrap_or_default(),
        false => Vec::new(),
    };
    respond(&app, &caller, k::Command::Login { email, password: presented }, &h, Subject::Profile, StatusCode::OK).await
}

async fn current_user(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap) -> Response {
    respond(&app, &caller, k::Command::CurrentUser, &h, Subject::Profile, StatusCode::OK).await
}

async fn update_user(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, body: Bytes) -> Response {
    if caller.principal().user == 0 {
        return errors(StatusCode::UNAUTHORIZED, "token", "is missing");
    }
    let u = tri!(wrapped(&body, "user"));
    let username = tri!(optional_required(&u, "username"));
    let email = tri!(optional_required(&u, "email"));
    let password = tri!(optional_required(&u, "password"));
    if let Some(p) = &password {
        tri!(check_password(p));
    }
    let bio = tri!(optional(&u, "bio"));
    let image = tri!(optional(&u, "image"));
    if username.is_none() && email.is_none() && password.is_none() && bio.is_none() && image.is_none() {
        return invalid("user");
    }
    let password = match password {
        Some(p) => Some(hash_password(p).await),
        None => None,
    };
    let cmd = k::Command::UpdateUser {
        email: email.map(bytes),
        username: username.map(bytes),
        password,
        bio: bio.map(bytes),
        image: image.map(bytes),
    };
    respond(&app, &caller, cmd, &h, Subject::Profile, StatusCode::OK).await
}

async fn get_profile(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &caller, k::Command::GetProfile { username: bytes(name) }, &h, Subject::Profile, StatusCode::OK).await
}

async fn follow(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &caller, k::Command::Follow { username: bytes(name) }, &h, Subject::Profile, StatusCode::OK).await
}

async fn unfollow(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &caller, k::Command::Unfollow { username: bytes(name) }, &h, Subject::Profile, StatusCode::OK).await
}

/// A listing with its `articlesCount`, which `ConduitStore::load_for`
/// counts in the same read-only transaction (`ARTICLE_COUNT`).
async fn listing(app: &App, caller: &Caller, cmd: k::Command, h: &HeaderMap) -> Response {
    let (result, count) = ARTICLE_COUNT
        .scope(Cell::new(None), async {
            let r = run(app, caller, &cmd, h).await;
            (r, ARTICLE_COUNT.with(Cell::get))
        })
        .await;
    match result {
        Ok(Ok(r)) => render(app, &r, StatusCode::OK, count),
        Ok(Err(e)) => refusal(e, caller.principal(), Subject::Article),
        Err(resp) => resp,
    }
}

async fn list_articles(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let (limit, offset) = tri!(page(&q));
    let cmd = k::Command::ListArticles {
        tag: q.get("tag").cloned().map(bytes),
        author: q.get("author").cloned().map(bytes),
        favorited: q.get("favorited").cloned().map(bytes),
        limit,
        offset,
    };
    listing(&app, &caller, cmd, &h).await
}

async fn feed(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let (limit, offset) = tri!(page(&q));
    listing(&app, &caller, k::Command::Feed { limit, offset }, &h).await
}

async fn get_article(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &caller, k::Command::GetArticle { slug: bytes(slug) }, &h, Subject::Article, StatusCode::OK).await
}

/// Runs `make(slug)` with the slug of `title`, then `-1`, `-2`, ... while
/// the kernel answers that the slug is taken.
async fn with_free_slug(
    app: &App,
    caller: &Caller,
    h: &HeaderMap,
    title: &str,
    ok: StatusCode,
    make: impl Fn(Vec<u8>) -> k::Command,
) -> Response {
    let base = slugify(title);
    for n in 0..SLUG_TRIES {
        match run(app, caller, &make(bytes(with_suffix(&base, n))), h).await {
            Ok(Err(k::Error::SlugTaken)) => continue,
            Ok(Ok(r)) => return render(app, &r, ok, None),
            Ok(Err(e)) => return refusal(e, caller.principal(), Subject::Article),
            Err(resp) => return resp,
        }
    }
    refusal(k::Error::SlugTaken, caller.principal(), Subject::Article)
}

async fn create_article(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, body: Bytes) -> Response {
    if caller.principal().user == 0 {
        return errors(StatusCode::UNAUTHORIZED, "token", "is missing");
    }
    let a = tri!(wrapped(&body, "article"));
    let title = tri!(required(&a, "title"));
    let description = bytes(tri!(required(&a, "description")));
    let body = bytes(tri!(required(&a, "body")));
    let tags = tri!(tag_list(&a)).unwrap_or_default();
    let now = now_ms();
    with_free_slug(&app, &caller, &h, &title, StatusCode::CREATED, |slug| k::Command::CreateArticle {
        slug,
        title: bytes(title.clone()),
        description: description.clone(),
        body: body.clone(),
        tags: tags.clone(),
        now,
    })
    .await
}

async fn update_article(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>, body: Bytes) -> Response {
    if caller.principal().user == 0 {
        return errors(StatusCode::UNAUTHORIZED, "token", "is missing");
    }
    let a = tri!(wrapped(&body, "article"));
    let title = tri!(optional_required(&a, "title"));
    let description = tri!(optional_required(&a, "description")).map(bytes);
    let body = tri!(optional_required(&a, "body")).map(bytes);
    let tags = tri!(tag_list(&a));
    if title.is_none() && description.is_none() && body.is_none() && tags.is_none() {
        return invalid("article");
    }
    let now = now_ms();
    let make = |new_slug: Option<Vec<u8>>| k::Command::UpdateArticle {
        slug: bytes(slug.clone()),
        new_slug,
        title: title.clone().map(bytes),
        description: description.clone(),
        body: body.clone(),
        tags: tags.clone(),
        now,
    };
    match &title {
        None => respond(&app, &caller, make(None), &h, Subject::Article, StatusCode::OK).await,
        Some(t) => with_free_slug(&app, &caller, &h, t, StatusCode::OK, |s| make(Some(s))).await,
    }
}

async fn delete_article(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &caller, k::Command::DeleteArticle { slug: bytes(slug) }, &h, Subject::Article, StatusCode::OK).await
}

async fn favorite(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &caller, k::Command::Favorite { slug: bytes(slug) }, &h, Subject::Article, StatusCode::OK).await
}

async fn unfavorite(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &caller, k::Command::Unfavorite { slug: bytes(slug) }, &h, Subject::Article, StatusCode::OK).await
}

async fn get_comments(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &caller, k::Command::GetComments { slug: bytes(slug) }, &h, Subject::Article, StatusCode::OK).await
}

async fn add_comment(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>, body: Bytes) -> Response {
    if caller.principal().user == 0 {
        return errors(StatusCode::UNAUTHORIZED, "token", "is missing");
    }
    let c = tri!(wrapped(&body, "comment"));
    let text = bytes(tri!(required(&c, "body")));
    let cmd = k::Command::AddComment { slug: bytes(slug), body: text, now: now_ms() };
    respond(&app, &caller, cmd, &h, Subject::Article, StatusCode::CREATED).await
}

async fn delete_comment(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap, Path((slug, id)): Path<(String, String)>) -> Response {
    // An id that is not a number names no comment; the kernel still decides
    // between "no such article" and "no such comment".
    let id = id.parse::<u64>().unwrap_or(0);
    let cmd = k::Command::DeleteComment { slug: bytes(slug), id };
    match run(&app, &caller, &cmd, &h).await {
        Ok(Ok(r)) => render(&app, &r, StatusCode::OK, None),
        Ok(Err(k::Error::Forbidden)) => errors(StatusCode::FORBIDDEN, "comment", "forbidden"),
        Ok(Err(e)) => refusal(e, caller.principal(), Subject::Article),
        Err(resp) => resp,
    }
}

async fn get_tags(State(app): State<App>, caller: Actor<Conduit>, h: HeaderMap) -> Response {
    respond(&app, &caller, k::Command::GetTags, &h, Subject::Article, StatusCode::OK).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Segfaults and You: When Raw Pointers Go Wrong"), "segfaults-and-you-when-raw-pointers-go-wrong");
        assert_eq!(slugify("  Hello -- World__x "), "hello-world-x");
        assert_eq!(slugify("¿Qué?"), "qu");
        assert_eq!(slugify("!!!"), "article");
        assert_eq!(with_suffix("a", 0), "a");
        assert_eq!(with_suffix("a", 2), "a-2");
    }

    #[test]
    fn dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_790_510_400_123), "2026-09-27T12:00:00.123Z");
    }

    #[test]
    fn tokens() {
        let auth = ConduitAuth::new(b"secret");
        let mut h = HeaderMap::new();
        assert_eq!(auth.authenticate(&h).unwrap().user, 0);
        h.insert("authorization", format!("Token {}", auth.issue(7)).parse().unwrap());
        assert_eq!(auth.authenticate(&h).unwrap().user, 7);
        h.insert("authorization", "Token nonsense".parse().unwrap());
        assert!(auth.authenticate(&h).is_err());
        let other = ConduitAuth::new(b"other");
        h.insert("authorization", format!("Token {}", other.issue(7)).parse().unwrap());
        assert!(auth.authenticate(&h).is_err());
    }
}

/// Benchmark switch. Both variants use the same API and kernel.
pub enum BenchEngine {
    Pg(Arc<Engine<Conduit, ConduitStore>>),
    Mem(h5i_app_mem::MemoryPgEngine<Conduit, ConduitStore>),
}
impl BenchEngine {
    pub async fn execute(&self, actor: &k::Principal, cmd: &k::Command) -> Result<Result<k::Reply, k::Error>, DbError> {
        match self {
            Self::Pg(e) => e.execute(actor, cmd).await,
            Self::Mem(e) => h5i_app_core::Executor::execute(e, actor, cmd).await,
        }
    }
    pub async fn execute_idempotent(&self, actor: &k::Principal, key: &str, cmd: &k::Command) -> Result<Result<k::Reply, k::Error>, DbError> {
        match self {
            Self::Pg(e) => e.execute_idempotent(actor, key, cmd).await,
            Self::Mem(e) => h5i_app_core::Executor::execute_idempotent::<ConduitStore>(e, actor, key, cmd).await,
        }
    }
    pub async fn snapshot_for(&self, actor: &k::Principal, cmd: &k::Command) -> Result<k::Snapshot, DbError> {
        match self {
            Self::Pg(e) => e.snapshot_for(actor, cmd).await,
            Self::Mem(e) => h5i_app_core::Executor::snapshot_for(e, actor, cmd).await,
        }
    }
}
