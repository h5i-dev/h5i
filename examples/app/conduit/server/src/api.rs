//! The RealWorld API (`/api/...`): each handler turns a request into a
//! kernel command and renders the reply in RealWorld's JSON shapes.

use crate::{principal, Conduit, ConduitStore, TENANT};
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use conduit_kernel as k;
use h5i_app_http::{reply, Actor, Auth, AuthError, Authenticator, HmacAuth};
use h5i_app_json::Value as Out;
use h5i_app_pg::{DbError, Engine};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tokens last two weeks, as upstream's.
const SESSION_SECS: u64 = 14 * 24 * 3600;

/// Reads `Authorization: Token <t>` (RealWorld's scheme; `Bearer` also
/// works). A request without the header is anonymous (user 0), which the
/// kernel refuses wherever an account is needed.
pub struct ConduitAuth {
    hmac: HmacAuth<Conduit>,
}

impl ConduitAuth {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        ConduitAuth { hmac: HmacAuth::new(secret, principal) }
    }

    pub fn issue(&self, user: u64) -> String {
        self.hmac.issue(TENANT, user, SESSION_SECS)
    }
}

impl Authenticator<Conduit> for ConduitAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<k::Principal, AuthError> {
        let Some(h) = headers.get("authorization") else {
            return Ok(principal(TENANT, 0));
        };
        let h = h.to_str().map_err(|_| AuthError("bad authorization header".into()))?;
        let token = h.strip_prefix("Token ").or_else(|| h.strip_prefix("Bearer ")).ok_or_else(|| AuthError("expected a token".into()))?;
        let (org, user) = self.hmac.verify(token)?;
        if org != TENANT {
            return Err(AuthError("wrong site".into()));
        }
        Ok(principal(org, user))
    }
}

/// Password hashes, keyed by the server secret. A stand-in for upstream's
/// Argon2: no per-user salt and no work factor.
pub fn hash_password(secret: &[u8], password: &str) -> Vec<u8> {
    let mut msg = b"conduit-password:".to_vec();
    msg.extend_from_slice(password.as_bytes());
    libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, secret, &msg, None)
}

/// Upstream's `slugify`: words of letters, digits and quotes, quotes
/// removed, lowercased, joined by `-`.
pub fn slugify(s: &str) -> String {
    const QUOTES: &[char] = &['\'', '"'];
    s.split(|c: char| !(QUOTES.contains(&c) || c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(|w| w.replace(QUOTES, "").to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Seconds since 1970 as `2026-09-27T12:00:00.000Z`.
pub fn iso8601(secs: u64) -> String {
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
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[derive(Clone)]
pub struct App {
    pub engine: Arc<Engine<Conduit, ConduitStore>>,
    pub auth: Arc<ConduitAuth>,
    pub secret: Arc<Vec<u8>>,
}

impl FromRef<App> for Auth<Conduit> {
    fn from_ref(app: &App) -> Self {
        Auth(app.auth.clone())
    }
}

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
        .with_state(app)
}

/* Running commands */

fn errors(status: StatusCode, field: &str, msg: &str) -> Response {
    reply(status, Out::obj([("errors", Out::obj([(field, Out::Arr(vec![Out::str(msg)]))]))]))
}

fn refusal(e: k::Error) -> Response {
    use k::Error::*;
    match e {
        Unauthorized => errors(StatusCode::UNAUTHORIZED, "body", "authentication required"),
        WrongPassword => errors(StatusCode::UNAUTHORIZED, "password", "is invalid"),
        UnknownEmail => errors(StatusCode::UNPROCESSABLE_ENTITY, "email", "does not exist"),
        NotFound => errors(StatusCode::NOT_FOUND, "body", "not found"),
        Forbidden => errors(StatusCode::FORBIDDEN, "body", "user may not perform that action"),
        UsernameTaken => errors(StatusCode::UNPROCESSABLE_ENTITY, "username", "username taken"),
        EmailTaken => errors(StatusCode::UNPROCESSABLE_ENTITY, "email", "email taken"),
        SlugTaken => errors(StatusCode::UNPROCESSABLE_ENTITY, "slug", "duplicate article slug"),
        Overflow => errors(StatusCode::INTERNAL_SERVER_ERROR, "body", "overflow"),
    }
}

/// Runs `cmd` for `actor`, honoring the `Idempotency-Key` header.
async fn run(app: &App, actor: &Actor<Conduit>, cmd: k::Command, h: &HeaderMap) -> Result<k::Reply, Response> {
    let key = h.get("idempotency-key").and_then(|v| v.to_str().ok());
    let result = match key {
        Some(key) => app.engine.execute_idempotent(actor.principal(), key, &cmd).await,
        None => app.engine.execute(actor.principal(), &cmd).await,
    };
    match result {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(refusal(e)),
        Err(DbError::IdempotencyConflict) => Err(errors(StatusCode::UNPROCESSABLE_ENTITY, "body", "idempotency key reused")),
        Err(e) => {
            tracing::error!(error = %e, "request failed");
            Err(errors(StatusCode::SERVICE_UNAVAILABLE, "body", "unavailable"))
        }
    }
}

/* Rendering */

fn text(t: &[u8]) -> Out {
    Out::Str(t.to_vec())
}

/// An empty image is `null`, as in upstream.
fn image(t: &[u8]) -> Out {
    if t.is_empty() {
        Out::Null
    } else {
        text(t)
    }
}

fn profile_json(p: &k::Profile) -> Out {
    Out::obj([
        ("username", text(&p.username)),
        ("bio", text(&p.bio)),
        ("image", image(&p.image)),
        ("following", p.following.into()),
    ])
}

fn article_json(v: &k::ArticleView) -> Out {
    let a = &v.article;
    Out::obj([
        ("slug", text(&a.slug)),
        ("title", text(&a.title)),
        ("description", text(&a.description)),
        ("body", text(&a.body)),
        ("tagList", Out::Arr(v.tags.iter().map(|t| text(t)).collect())),
        ("createdAt", Out::str(iso8601(a.created))),
        ("updatedAt", Out::str(iso8601(a.updated))),
        ("favorited", v.favorited.into()),
        ("favoritesCount", v.favorites.into()),
        ("author", profile_json(&v.author)),
    ])
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

/// Renders a reply; accounts get a fresh token.
fn render(app: &App, r: &k::Reply) -> Response {
    let body = match r {
        k::Reply::Account(a) => Out::obj([(
            "user",
            Out::obj([
                ("email", text(&a.email)),
                ("token", Out::str(app.auth.issue(a.id))),
                ("username", text(&a.username)),
                ("bio", text(&a.bio)),
                ("image", image(&a.image)),
            ]),
        )]),
        k::Reply::Profile(p) => Out::obj([("profile", profile_json(p))]),
        k::Reply::Article(v) => Out::obj([("article", article_json(v))]),
        k::Reply::Articles(vs) => Out::obj([
            ("articles", Out::Arr(vs.iter().map(article_json).collect())),
            // As upstream: the number returned, not the total.
            ("articlesCount", (vs.len() as u64).into()),
        ]),
        k::Reply::Comment(c) => Out::obj([("comment", comment_json(c))]),
        k::Reply::Comments(cs) => Out::obj([("comments", Out::Arr(cs.iter().map(comment_json).collect()))]),
        k::Reply::Tags(ts) => {
            let mut ts = ts.clone();
            ts.sort();
            Out::obj([("tags", Out::Arr(ts.iter().map(|t| text(t)).collect()))])
        }
        k::Reply::Done => Out::obj::<&str>([]),
    };
    reply(StatusCode::OK, body)
}

async fn respond(app: &App, actor: &Actor<Conduit>, cmd: k::Command, h: &HeaderMap) -> Response {
    match run(app, actor, cmd, h).await {
        Ok(r) => render(app, &r),
        Err(resp) => resp,
    }
}

/* Request bodies */

#[derive(Deserialize)]
struct Wrap<T> {
    #[serde(alias = "article", alias = "comment")]
    user: T,
}

#[derive(Deserialize)]
struct NewUser {
    username: String,
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct LoginUser {
    email: String,
    password: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdateUser {
    email: Option<String>,
    username: Option<String>,
    password: Option<String>,
    bio: Option<String>,
    image: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewArticle {
    title: String,
    description: String,
    body: String,
    #[serde(default)]
    tag_list: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdateArticle {
    title: Option<String>,
    description: Option<String>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct NewComment {
    body: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    tag: Option<String>,
    author: Option<String>,
    favorited: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}

fn bytes(s: String) -> Vec<u8> {
    s.into_bytes()
}

/* Handlers */

async fn register(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Json(b): Json<Wrap<NewUser>>) -> Response {
    let u = b.user;
    let cmd = k::Command::Register {
        username: bytes(u.username),
        email: bytes(u.email),
        password: hash_password(&app.secret, &u.password),
    };
    respond(&app, &actor, cmd, &h).await
}

async fn login(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Json(b): Json<Wrap<LoginUser>>) -> Response {
    let u = b.user;
    let cmd = k::Command::Login { email: bytes(u.email), password: hash_password(&app.secret, &u.password) };
    respond(&app, &actor, cmd, &h).await
}

async fn current_user(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap) -> Response {
    respond(&app, &actor, k::Command::CurrentUser, &h).await
}

async fn update_user(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Json(b): Json<Wrap<UpdateUser>>) -> Response {
    let u = b.user;
    let cmd = k::Command::UpdateUser {
        email: u.email.map(bytes),
        username: u.username.map(bytes),
        password: u.password.map(|p| hash_password(&app.secret, &p)),
        bio: u.bio.map(bytes),
        image: u.image.map(bytes),
    };
    respond(&app, &actor, cmd, &h).await
}

async fn get_profile(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &actor, k::Command::GetProfile { username: bytes(name) }, &h).await
}

async fn follow(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &actor, k::Command::Follow { username: bytes(name) }, &h).await
}

async fn unfollow(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(name): Path<String>) -> Response {
    respond(&app, &actor, k::Command::Unfollow { username: bytes(name) }, &h).await
}

async fn list_articles(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    let cmd = k::Command::ListArticles {
        tag: q.tag.map(bytes),
        author: q.author.map(bytes),
        favorited: q.favorited.map(bytes),
        limit: q.limit.unwrap_or(20),
        offset: q.offset.unwrap_or(0),
    };
    respond(&app, &actor, cmd, &h).await
}

async fn feed(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    let cmd = k::Command::Feed { limit: q.limit.unwrap_or(20), offset: q.offset.unwrap_or(0) };
    respond(&app, &actor, cmd, &h).await
}

async fn get_article(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &actor, k::Command::GetArticle { slug: bytes(slug) }, &h).await
}

async fn create_article(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Json(b): Json<Wrap<NewArticle>>) -> Response {
    let a = b.user;
    let mut tags = a.tag_list;
    // As upstream: tags are stored sorted.
    tags.sort();
    let cmd = k::Command::CreateArticle {
        slug: bytes(slugify(&a.title)),
        title: bytes(a.title),
        description: bytes(a.description),
        body: bytes(a.body),
        tags: tags.into_iter().map(bytes).collect(),
        now: now(),
    };
    respond(&app, &actor, cmd, &h).await
}

async fn update_article(
    State(app): State<App>,
    actor: Actor<Conduit>,
    h: HeaderMap,
    Path(slug): Path<String>,
    Json(b): Json<Wrap<UpdateArticle>>,
) -> Response {
    let a = b.user;
    let cmd = k::Command::UpdateArticle {
        slug: bytes(slug),
        new_slug: a.title.as_deref().map(|t| bytes(slugify(t))),
        title: a.title.map(bytes),
        description: a.description.map(bytes),
        body: a.body.map(bytes),
        now: now(),
    };
    respond(&app, &actor, cmd, &h).await
}

async fn delete_article(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &actor, k::Command::DeleteArticle { slug: bytes(slug) }, &h).await
}

async fn favorite(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &actor, k::Command::Favorite { slug: bytes(slug) }, &h).await
}

async fn unfavorite(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &actor, k::Command::Unfavorite { slug: bytes(slug) }, &h).await
}

async fn get_comments(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path(slug): Path<String>) -> Response {
    respond(&app, &actor, k::Command::GetComments { slug: bytes(slug) }, &h).await
}

async fn add_comment(
    State(app): State<App>,
    actor: Actor<Conduit>,
    h: HeaderMap,
    Path(slug): Path<String>,
    Json(b): Json<Wrap<NewComment>>,
) -> Response {
    let cmd = k::Command::AddComment { slug: bytes(slug), body: bytes(b.user.body), now: now() };
    respond(&app, &actor, cmd, &h).await
}

async fn delete_comment(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap, Path((slug, id)): Path<(String, u64)>) -> Response {
    respond(&app, &actor, k::Command::DeleteComment { slug: bytes(slug), id }, &h).await
}

async fn get_tags(State(app): State<App>, actor: Actor<Conduit>, h: HeaderMap) -> Response {
    respond(&app, &actor, k::Command::GetTags, &h).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_upstream() {
        assert_eq!(slugify("Segfaults and You: When Raw Pointers Go Wrong"), "segfaults-and-you-when-raw-pointers-go-wrong");
        assert_eq!(slugify("Why are DB Admins Always Shouting?"), "why-are-db-admins-always-shouting");
        assert_eq!(slugify("Converting to Rust from C: It's as Easy as 1, 2, 3!"), "converting-to-rust-from-c-its-as-easy-as-1-2-3");
    }

    #[test]
    fn dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_790_510_400), "2026-09-27T12:00:00.000Z");
    }
}
