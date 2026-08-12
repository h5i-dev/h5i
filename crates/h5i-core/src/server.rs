//! The box console: `h5i ui`.
//!
//! One screen for the question the CLI answers a box at a time — *what is the
//! fleet doing, and what has pressed on a boundary?* It is the sandbox half of
//! the old workbench dashboard, rebuilt on what the contained-environment
//! product actually keeps: manifests, the resolved policy, the env event log,
//! and [`crate::receipt`] — the append-only record of everything that ran.
//!
//! # Read-only, and structurally so
//!
//! Every route is a `GET`. There is no mutating handler to guard, so the
//! console needs no CSRF story and cannot be turned into a remote control for
//! someone's boxes: `shell`, `run`, `export`, `apply` and `rm` stay in the CLI,
//! where a human types them. That was the original dashboard's rule and it is
//! worth keeping — a monitoring surface that cannot act is a much smaller thing
//! to get wrong.
//!
//! # What guards it
//!
//! The old `h5i serve` bound loopback and trusted everyone who could reach it,
//! which on a developer machine means every process and every page. This one
//! follows [`crate::view`]'s discipline instead:
//!
//! * **Loopback only**, on a port h5i chose.
//! * **A per-session token**, minted at `bind` and printed once in the URL.
//!   It lives in this process's memory — nothing on disk, so nothing a box can
//!   read. The page trades it for a `SameSite=Strict` cookie, which a
//!   cross-site request never carries.
//! * **Foreign origins refused.** A page on another origin cannot read these
//!   responses anyway, but saying no at the door is cheaper than reasoning
//!   about it.
//!
//! # Honesty
//!
//! The dashboard this replaces had a risk classifier behind its red/amber/grey
//! badges. That module was cut with the provenance surface, and nothing here
//! invents a score to replace it. [`Signals`] is arithmetic over receipts and
//! nothing else: red means **enforcement fired** (the egress proxy refused a
//! destination), amber means a run failed or timed out or the page reported
//! errors, and grey means the evidence is weak — either nothing was confined
//! (`workspace` tier) or every record came from inside the box. None of those
//! is an accusation, and the UI never renders a number the receipts don't hold.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::Serialize;

use crate::env::{self, EnvEvent, EnvManifest, LiveSession, ServiceStatus};
use crate::error::H5iError;
use crate::receipt::ExecRecord;
use crate::sandbox::Profile;

/// The cookie the page uses after it has spent the token in the URL.
const COOKIE: &str = "h5i_console";

/// 128 bits, hex — the same budget [`crate::view`] mints for a box viewer.
const TOKEN_BYTES: usize = 16;

/// Newest-first cap on the receipts carried in one box's detail payload. A
/// long-lived box accumulates thousands; the console shows the working set and
/// says how many it folded rather than paging silently.
const RECEIPT_CAP: usize = 400;

/// Distinct refused destinations carried in a fleet row.
const DENIED_HOSTS_CAP: usize = 8;

#[derive(Clone)]
pub struct AppState {
    pub repo_path: PathBuf,
    /// This session's token. Never written to disk.
    pub token: String,
    /// One [`crate::browser_events::BoxStream`] per box the console has looked
    /// at, kept for the life of the process.
    ///
    /// State in a server whose whole pitch is that it holds none takes a
    /// justification, and it is not caching: it is what makes the event ids
    /// stable. Rebuilding the stream per request renumbers from 1 over whatever
    /// the sources currently hold, and a run clears the box's `/tmp`, so a
    /// viewer's cursor would silently swallow the next session (see `BoxStream`).
    /// Bounded twice over — one entry per box *viewed*, each capped at
    /// [`STREAM_CAP`] events — and it is derived from files on disk, so losing
    /// it costs nothing but a re-read.
    browser: Arc<std::sync::Mutex<std::collections::HashMap<String, crate::browser_events::BoxStream>>>,
    /// One live-view reader per box currently being watched. Separate from
    /// `browser` because their lifetimes differ: the event stream is cheap and
    /// kept, while a relay holds a socket into a box and is dropped as soon as
    /// the box stops serving a view.
    frames:
        Arc<std::sync::Mutex<std::collections::HashMap<String, crate::browser_frames::FrameRelay>>>,
}

impl AppState {
    pub fn new(repo_path: PathBuf, token: String) -> Self {
        Self {
            repo_path,
            token,
            browser: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            frames: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

// ── the gate ─────────────────────────────────────────────────────────────────

/// Why a request was turned away. Distinct variants because they mean very
/// different things to whoever is reading the logs: one is a missing token, the
/// other is a page that should not have been asking at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No token, or the wrong one.
    Unauthorized,
    /// An `Origin` header naming somewhere other than this console.
    ForeignOrigin,
}

impl Refusal {
    pub fn status(self) -> (StatusCode, &'static str) {
        match self {
            Refusal::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "h5i console: missing or wrong token — open the URL `h5i ui` printed",
            ),
            Refusal::ForeignOrigin => (
                StatusCode::FORBIDDEN,
                "h5i console: refused a cross-origin request",
            ),
        }
    }
}

/// The whole authorization decision, as a pure function of the four things it
/// depends on — so it can be tested without a socket.
///
/// `Origin` is checked first: a request from another origin is refused even
/// when it somehow carries the right token, because at that point the token is
/// the thing that has leaked and honouring it would be the bug.
pub fn authorize(
    query: Option<&str>,
    cookie_header: Option<&str>,
    origin: Option<&str>,
    expected: &str,
    self_origin: &str,
) -> Result<(), Refusal> {
    if let Some(o) = origin {
        if o != self_origin {
            return Err(Refusal::ForeignOrigin);
        }
    }
    let presented = query
        .and_then(|q| param(q, "token"))
        .or_else(|| cookie_header.and_then(|c| cookie(c, COOKIE)));
    match presented {
        Some(t) if token_matches(expected, &t) => Ok(()),
        _ => Err(Refusal::Unauthorized),
    }
}

/// Compare in constant time w.r.t. content. The lengths are public (both are
/// fixed-width hex), so only the bytes are hidden.
fn token_matches(expected: &str, given: &str) -> bool {
    if expected.len() != given.len() {
        return false;
    }
    expected
        .bytes()
        .zip(given.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// First value of `key` in a `a=b&c=d` query string.
fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Value of `name` in a `Cookie:` header.
fn cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

async fn gate(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1");
    // The Origin check below compares against our own Host header, which a
    // DNS-rebinding page satisfies trivially: it arrives as
    // `Host: evil.test:<port>` / `Origin: http://evil.test:<port>`, which match
    // each other. Requiring the Host to actually name loopback is what makes
    // "foreign origins refused" true rather than self-referential. (The token
    // cookie already held this shut; the documented property did not.)
    if !is_loopback_host(host) {
        return Refusal::ForeignOrigin.status().into_response();
    }
    let self_origin = format!("http://{host}");
    let verdict = authorize(
        req.uri().query(),
        req.headers()
            .get(header::COOKIE)
            .and_then(|h| h.to_str().ok()),
        req.headers()
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok()),
        &state.token,
        &self_origin,
    );
    match verdict {
        Ok(()) => next.run(req).await,
        Err(r) => r.status().into_response(),
    }
}

/// Does this `Host` header name the loopback interface? Port-insensitive: the
/// console binds an ephemeral port, so the name is the part worth pinning.
fn is_loopback_host(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        // Only strip a trailing all-digit port; `[::1]` has colons of its own.
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    let name = name.trim_matches(|c| c == '[' || c == ']');
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
}

// ── the bundle ───────────────────────────────────────────────────────────────

#[derive(rust_embed::Embed)]
// The frontend project and its build output live at the workspace root; this
// crate is one level down. `build.rs` guarantees the directory exists.
#[folder = "../../web/dist/"]
struct Asset;

fn embedded(path: &str) -> Response {
    match Asset::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, format!("no such asset: {path}")).into_response(),
    }
}

/// `/` — the console page, plus the cookie that lets its `fetch` calls back in.
///
/// Reaching this handler means [`gate`] already accepted the request, so the
/// cookie is only ever handed to someone who proved they had the token.
async fn index(State(state): State<Arc<AppState>>) -> Response {
    let mut resp = embedded("index.html");
    let cookie = format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",
        state.token
    );
    if let Ok(v) = header::HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

async fn asset(Path(path): Path<String>) -> Response {
    // The `/assets/*path` route strips the prefix; rust-embed indexes by the
    // full path under web/dist/, so it goes back on.
    embedded(&format!("assets/{path}"))
}

// ── API types ────────────────────────────────────────────────────────────────

/// Boundary activity for one box, as arithmetic over its receipts.
///
/// Every field is a count of something recorded. Nothing here is a model of
/// intent, and the two `..._observed` counters exist so a reader can tell
/// evidence h5i collected from evidence the box handed it.
/// The lanes h5i itself writes, from the host, outside the box's reach.
/// Anything else — `tee-shim`, `inbox-capture`, or a lane added later — is the
/// box's own account and is counted as such.
// The browser mediator is host-observed like the viewer: the records are
// written by an h5i process sitting on the socket, not claimed by the box.
// `share` is on this list because h5i owns both ends of the share bridge, the
// box supplies none of it, and the box cannot suppress it. Leaving it off
// inverted the badge: a box whose only receipt was a share read as having no
// host-observed evidence at all — the grey "the box told us this" badge — for
// the one lane the box cannot touch.
const HOST_OBSERVED_LANES: [&str; 5] = [
    "host-env-run",
    "shell-egress",
    "viewer",
    "browser-proxy",
    "share",
];

#[derive(Serialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct Signals {
    /// Receipts on this box's log.
    pub runs: usize,
    /// Runs that exited non-zero.
    pub failed: usize,
    /// Runs the wall-clock limit killed.
    pub timed_out: usize,
    /// Egress the allowlist proxy permitted. Host-observed (container tier).
    pub egress_allowed: u64,
    /// Egress the proxy refused — the single highest-fidelity signal here,
    /// and the only thing that turns a row red.
    pub egress_denied: u64,
    /// Distinct refused `host:port` destinations, capped.
    pub denied_hosts: Vec<String>,
    /// Console errors, page errors and failed requests the in-box browser
    /// reported, summed over runs that drove it.
    pub browser_issues: usize,
    /// Runs h5i observed from the host — see [`HOST_OBSERVED_LANES`].
    pub host_observed: usize,
    /// Runs recorded by the in-box tee shim — the box's own account.
    pub box_claimed: usize,
    /// Timestamp of the newest receipt.
    pub last_run_ts: Option<String>,
    /// `denial` | `attention` | `clean`.
    pub verdict: &'static str,
    /// The box runs at `workspace` tier: nothing was confined. Reported beside
    /// the verdict rather than folded into it — it is a standing property of
    /// the box, not something a run did.
    pub weak_isolation: bool,
    /// Runs exist and none of them was host-observed.
    pub box_claimed_only: bool,
}

fn signals(m: &EnvManifest, receipts: &[ExecRecord]) -> Signals {
    let mut s = Signals {
        runs: receipts.len(),
        weak_isolation: m.isolation_claim == "workspace",
        ..Default::default()
    };
    let mut denied_hosts: Vec<String> = Vec::new();
    for r in receipts {
        if r.exit_code.is_some_and(|c| c != 0) {
            s.failed += 1;
        }
        if r.timed_out {
            s.timed_out += 1;
        }
        // Allowlist, not a catch-all. `_ => host_observed` counted any unknown
        // source as evidence h5i collected — including `inbox-capture`, which
        // the boxed process writes into its own spool. A box could therefore
        // clear the grey "box-claimed" badge by writing its own records, which
        // is precisely the distinction this screen exists to keep.
        if HOST_OBSERVED_LANES.contains(&r.source.as_str()) {
            s.host_observed += 1;
        } else {
            s.box_claimed += 1;
        }
        if let Some(e) = &r.egress {
            s.egress_allowed += e.allowed;
            s.egress_denied += e.denied;
            for h in e.hosts.iter().filter(|h| h.denied > 0) {
                let label = format!("{}:{}", h.host, h.port);
                if !denied_hosts.contains(&label) {
                    denied_hosts.push(label);
                }
            }
        }
        if let Some(b) = &r.browser {
            s.browser_issues += b.console.len() + b.errors.len() + b.failed_requests.len();
        }
    }
    denied_hosts.truncate(DENIED_HOSTS_CAP);
    s.denied_hosts = denied_hosts;
    // Receipts are appended in order and their timestamps sort lexically.
    s.last_run_ts = receipts.last().map(|r| r.timestamp.clone());
    s.box_claimed_only = s.runs > 0 && s.host_observed == 0;
    s.verdict = if s.egress_denied > 0 {
        "denial"
    } else if s.failed > 0 || s.timed_out > 0 || s.browser_issues > 0 {
        "attention"
    } else {
        "clean"
    };
    s
}

/// Sort key: most pressing first. `denial` outranks `attention` outranks
/// everything else, and within a rank the most recently touched box wins.
fn rank(s: &Signals) -> u8 {
    match s.verdict {
        "denial" => 2,
        "attention" => 1,
        _ => 0,
    }
}

/// One row in the fleet.
///
/// The manifest is flattened in, so this payload is a superset of
/// `h5i box list --json` — the console and the scriptable CLI describe a box
/// with the same field names, and there is no second shape to keep in step.
#[derive(Serialize)]
pub struct BoxRow {
    #[serde(flatten)]
    pub manifest: EnvManifest,
    /// Stable kind (`up-to-date`, `parent-ahead`, …) plus its one-line summary.
    pub drift: String,
    pub drift_summary: String,
    /// PID-verified sessions from the live registry.
    pub live: Vec<LiveSession>,
    /// The durable status says `running` but no live writer holds the box — a
    /// crash leftover to flag, never to trust.
    pub stale_running: bool,
    /// Is the workspace materialized here (false = pulled from another clone)?
    pub has_workspace: bool,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub last_event: Option<EnvEvent>,
    pub signals: Signals,
    /// A share serving this box **right now**, if one is.
    ///
    /// The receipt lands when the share ends, so until this the console showed
    /// nothing at all while a box was open to somebody on another machine —
    /// and the console's whole job is saying what is pressing on a boundary.
    /// The one lane that lets somebody *in* was the one it could not see while
    /// it was open.
    pub shared_now: Option<SharedNow>,
}

/// What the console needs to say about a share that is open.
///
/// Read off `<env>/share.json`, which is where the share keeps it, rather than
/// through `h5i-share`: that crate is above this one. No secret is in the file
/// and none is read here — the grant table stores digests, and this takes the
/// transport, the port and the number of grants that can still admit anybody.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SharedNow {
    pub transport: String,
    pub port: u64,
    pub grants: usize,
}

fn shared_now(env_dir: &std::path::Path) -> Option<SharedNow> {
    // Only when somebody can actually reach in. A share that is winding up, or
    // whose grants have all been revoked, has a live pid and admits nobody —
    // and the badge said "somebody outside can reach port 3000 right now" of
    // it, for as long as that process took to exit. On a screen whose job is
    // saying what is pressing on a boundary, overclaiming toward alarm is the
    // failure that costs it its credibility.
    let r = crate::share_record::read_live(env_dir)?;
    if !r.is_admitting() {
        return None;
    }
    Some(SharedNow {
        transport: r.transport,
        port: r.port as u64,
        grants: r.live_grants,
    })
}

fn build_row(
    git: &git2::Repository,
    h5i_root: &std::path::Path,
    m: &EnvManifest,
    events: &[EnvEvent],
    receipts: &[ExecRecord],
) -> BoxRow {
    let drift = env::drift(git, m);
    let live = env::live_sessions(&m.dir(h5i_root));
    let stale_running =
        m.status == "running" && !live.iter().any(|s| env::live_is_writer(&s.kind));
    let (files_changed, insertions, deletions) =
        env::diffstat_numbers(git, h5i_root, m).unwrap_or((0, 0, 0));
    BoxRow {
        drift: drift.kind_str().to_string(),
        drift_summary: drift.summary(),
        live,
        stale_running,
        has_workspace: env::has_workspace(m, h5i_root),
        files_changed,
        insertions,
        deletions,
        last_event: events.last().cloned(),
        signals: signals(m, receipts),
        shared_now: shared_now(&m.dir(h5i_root)),
        manifest: m.clone(),
    }
}

/// The resolved policy, flattened for the console's allowance panel: what was
/// actually enforced, not what the profile asked for.
#[derive(Serialize)]
pub struct EnforcedPolicy {
    pub isolation: String,
    pub net_mode: String,
    pub net_egress: Vec<String>,
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub fs_deny: Vec<String>,
    pub tools: Vec<String>,
    pub env_pass: Vec<String>,
    pub image: Option<String>,
    pub mem_bytes: Option<u64>,
    pub max_procs: Option<u64>,
    pub wall_secs: u64,
    pub cpu_secs: Option<u64>,
    pub fsize_bytes: Option<u64>,
    /// Whether this tier actually applies `mem_bytes` on the host serving this
    /// console. The panel is headed "what was actually allowed", so a cap the
    /// host cannot impose has to be drawn as declared-only rather than silently
    /// rendered next to the ones that are real. See
    /// [`crate::sandbox::limit_support`].
    pub mem_enforced: bool,
    /// As `mem_enforced`, for `max_procs`.
    pub procs_enforced: bool,
}

impl From<&Profile> for EnforcedPolicy {
    fn from(p: &Profile) -> Self {
        let limits = crate::sandbox::limit_support(p.isolation);
        EnforcedPolicy {
            isolation: p.isolation.as_str().to_string(),
            net_mode: match p.net_mode {
                crate::sandbox::NetMode::Deny => "deny".into(),
                crate::sandbox::NetMode::Host => "host".into(),
            },
            net_egress: p.net_egress.clone(),
            fs_read: p.fs_read.clone(),
            fs_write: p.fs_write.clone(),
            fs_deny: p.fs_deny.clone(),
            tools: p.tools.clone(),
            env_pass: p.env_pass.clone(),
            image: p.image.clone(),
            mem_bytes: p.mem_bytes,
            max_procs: p.max_procs,
            wall_secs: p.wall_secs,
            cpu_secs: p.cpu_secs,
            fsize_bytes: p.fsize_bytes,
            mem_enforced: limits.mem,
            procs_enforced: limits.procs,
        }
    }
}

/// Everything the detail pane draws for one box.
#[derive(Serialize)]
pub struct BoxDetail {
    pub item: BoxRow,
    /// `None` when `policy.resolved.toml` is unreadable — a pulled or gc'd box.
    pub policy: Option<EnforcedPolicy>,
    pub events: Vec<EnvEvent>,
    /// Newest last, capped at [`RECEIPT_CAP`].
    pub receipts: Vec<ExecRecord>,
    /// How many older receipts the cap folded, so a clamped list never reads
    /// as the whole history.
    pub receipts_folded: usize,
    /// Declared long-lived services and their liveness.
    pub services: Vec<ServiceStatus>,
    /// `base..worktree` diffstat, as `h5i box diff --stat` prints it.
    pub diffstat: Option<String>,
}

// ── handlers ─────────────────────────────────────────────────────────────────

/// Run a blocking domain call off the async runtime, folding "the task died"
/// and "the work found nothing" into one `None`.
///
/// Everything in `env` is synchronous git and filesystem work, so every handler
/// goes through here. `Option` rather than a `Result`: these are read-only
/// lookups whose only two outcomes on the wire are a payload and a 404, so
/// carrying an error type would be carrying something no caller can use.
async fn blocking<T, F>(f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.ok().flatten()
}

/// Open the repository the console was pointed at, and its h5i root.
fn open(repo_path: &std::path::Path) -> Option<(git2::Repository, PathBuf)> {
    let git = git2::Repository::discover(repo_path).ok()?;
    let root = crate::storage::h5i_root_for_repo(&git).ok()?;
    Some((git, root))
}

fn receipts_of(h5i_root: &std::path::Path, m: &EnvManifest) -> Vec<ExecRecord> {
    crate::receipt::list(&env::env_dir(h5i_root, &m.agent, &m.slug)).unwrap_or_default()
}

/// `GET /api/boxes` — the fleet, most pressing first.
async fn api_boxes(State(state): State<Arc<AppState>>) -> Json<Vec<BoxRow>> {
    let path = state.repo_path.clone();
    let rows = blocking(move || {
        let (git, h5i_root) = open(&path)?;
        let mut rows: Vec<BoxRow> = env::list(&h5i_root)
            .iter()
            .map(|m| {
                let events = env::read_events(&git, Some(&m.id));
                let receipts = receipts_of(&h5i_root, m);
                build_row(&git, &h5i_root, m, &events, &receipts)
            })
            .collect();
        rows.sort_by(|a, b| {
            rank(&b.signals)
                .cmp(&rank(&a.signals))
                .then(b.manifest.updated_at.cmp(&a.manifest.updated_at))
        });
        Some(rows)
    })
    .await;
    Json(rows.unwrap_or_default())
}

/// `GET /api/box/:agent/:slug` — one box in full.
async fn api_box(
    State(state): State<Arc<AppState>>,
    Path((agent, slug)): Path<(String, String)>,
) -> Response {
    let path = state.repo_path.clone();
    let detail = blocking(move || {
        let (git, h5i_root) = open(&path)?;
        let id = format!("env/{agent}/{slug}");
        let m = env::list(&h5i_root).into_iter().find(|m| m.id == id)?;
        let events = env::read_events(&git, Some(&m.id));
        let mut receipts = receipts_of(&h5i_root, &m);
        let policy = env::load_policy(&h5i_root, &m).ok().map(|rp| rp.profile);
        // Signals over the FULL history, then fold for display. Draining first
        // made the detail pane disagree with the fleet row the user just
        // clicked, and in the dangerous direction: a denial older than the cap
        // vanished on inspection while the row still showed red.
        let item = build_row(&git, &h5i_root, &m, &events, &receipts);
        let receipts_folded = receipts.len().saturating_sub(RECEIPT_CAP);
        if receipts_folded > 0 {
            receipts.drain(..receipts_folded);
        }
        Some(BoxDetail {
            item,
            policy: policy.as_ref().map(EnforcedPolicy::from),
            events,
            receipts,
            receipts_folded,
            services: env::service_status(&h5i_root, &m),
            diffstat: env::diff(&git, &h5i_root, &m, true).ok(),
        })
    })
    .await;
    match detail {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "no such box").into_response(),
    }
}

/// `GET /api/box/:agent/:slug/receipts/:id` — one receipt, rendered exactly as
/// `h5i box inspect` renders it (which refuses ids belonging to another box).
async fn api_receipt(
    State(state): State<Arc<AppState>>,
    Path((agent, slug, id)): Path<(String, String, String)>,
) -> Response {
    let path = state.repo_path.clone();
    let render = blocking(move || {
        let (_git, h5i_root) = open(&path)?;
        let want = format!("env/{agent}/{slug}");
        let m = env::list(&h5i_root).into_iter().find(|m| m.id == want)?;
        env::inspect(&h5i_root, &m, &id).ok()
    })
    .await;
    match render {
        Some(text) => Json(serde_json::json!({ "render": text })).into_response(),
        None => (StatusCode::NOT_FOUND, "no such receipt for this box").into_response(),
    }
}

/// What the browser terminal reads: the box's event stream plus the two things
/// a pane must state rather than imply.
#[derive(Serialize)]
pub struct BrowserStream {
    /// Events after the caller's cursor, oldest first.
    pub events: Vec<crate::browser_events::ViewerEvent>,
    /// The newest id held. The caller sends this back as `since` and gets only
    /// what it has not seen.
    pub cursor: u64,
    /// Events the cap discarded. Rendered by the console, because a bound that
    /// drops silently reports a quiet session where there was a loud one.
    pub dropped: u64,
    /// The engine this box is pinned to, or `None` when it has no browser
    /// profile. The network pane's evidence grade follows from it, so the pane
    /// names the engine rather than leaving a reader to assume Chromium.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Whether a live view is being served inside the box right now — a
    /// `.stream` file next to the daemon socket, the same discovery
    /// `h5i box view` uses.
    pub live_view: bool,
    /// Sequence number of the newest frame the console holds, or `None` when it
    /// holds none.
    ///
    /// The page re-fetches the frame only when this changes, which is what keeps
    /// a still page at zero requests instead of on a timer — the same
    /// change-driven rule the engine itself follows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_seq: Option<u64>,
    /// Why there is no picture, when there is a live view but no frame. Shown
    /// rather than swallowed: a viewer that cannot say why it is blank is
    /// indistinguishable from one that is broken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_error: Option<String>,
}

/// How many events one box's stream holds. A page pulling in subresources makes
/// two rows each, so this is a few hundred navigations' worth — enough to scroll
/// back through a session, bounded enough that a long-lived console does not
/// grow without limit.
const STREAM_CAP: usize = 4000;

/// `GET /api/box/:agent/:slug/browser?since=N` — the browser terminal's stream
/// (ROADMAP M11a).
///
/// A `GET` like every other route here, and for the same reason: the console
/// watches and never drives. Taking the control lock and typing into a page go
/// through [`crate::view`]'s forward, which is a different surface with a
/// different token, and deliberately not this one.
async fn api_browser(
    State(state): State<Arc<AppState>>,
    Path((agent, slug)): Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<BrowserQuery>,
) -> Response {
    let path = state.repo_path.clone();
    let streams = state.browser.clone();
    let frames = state.frames.clone();
    let stream = blocking(move || {
        let (_git, h5i_root) = open(&path)?;
        let want = format!("env/{agent}/{slug}");
        let m = env::list(&h5i_root).into_iter().find(|m| m.id == want)?;
        let policy = env::load_policy(&h5i_root, &m).ok();
        let engine = policy
            .as_ref()
            .and_then(|p| p.profile.engine)
            .map(|e| e.as_str().to_string());

        // A poisoned lock means another request panicked mid-poll. The state is
        // a derived read cursor, not a record of anything, so the honest
        // recovery is to keep serving from it rather than to fail every
        // subsequent request for the life of the process.
        let mut held = streams.lock().unwrap_or_else(|e| e.into_inner());
        let entry = held
            .entry(want)
            .or_insert_with(|| crate::browser_events::BoxStream::new(STREAM_CAP));
        entry.poll(&h5i_root, &m);
        let log = entry.log();

        let events: Vec<_> = log.since(query.since).into_iter().cloned().collect();
        let (cursor, dropped) = (log.cursor(), log.dropped());
        drop(held);

        // Looking at the browser tab is what starts a reader; closing the
        // console is what ends it. Kept off the `browser` lock because a
        // namespace entry is slow and the event stream should not wait on it.
        let env_dir = m.dir(&h5i_root);
        let located = crate::browser_frames::locate(&env_dir);
        let (frame_seq, frame_error) = tend_relay(&frames, &m.id, located);

        Some(BrowserStream {
            events,
            cursor,
            dropped,
            engine,
            live_view: located.is_some(),
            frame_seq,
            frame_error,
        })
    })
    .await;
    match stream {
        Some(s) => Json(s).into_response(),
        None => (StatusCode::NOT_FOUND, "no such box").into_response(),
    }
}

/// The cursor, defaulted so a first poll needs no parameter.
#[derive(serde::Deserialize)]
pub struct BrowserQuery {
    #[serde(default)]
    pub since: u64,
}

type Relays = std::sync::Mutex<std::collections::HashMap<String, crate::browser_frames::FrameRelay>>;

/// Keep the live-view reader for one box in step with reality, and report what
/// it has.
///
/// Three transitions, all driven by the box rather than by a user action: a
/// view appears and a reader starts; a view goes away and the reader is dropped
/// (which closes the socket into the box — a console tab left open must not pin
/// a connection to a box that stopped serving); a reader dies on its own and is
/// replaced, but not faster than its retry delay.
fn tend_relay(
    relays: &Arc<Relays>,
    box_id: &str,
    located: Option<(u32, u16)>,
) -> (Option<u64>, Option<String>) {
    let mut held = relays.lock().unwrap_or_else(|e| e.into_inner());

    let Some((pid, port)) = located else {
        held.remove(box_id);
        return (None, None);
    };

    if held.get(box_id).is_some_and(|r| r.spent()) {
        held.remove(box_id);
    }
    let relay = held
        .entry(box_id.to_string())
        .or_insert_with(|| crate::browser_frames::FrameRelay::start(pid, port));

    let seq = relay.latest().map(|f| f.seq);
    // Only worth reporting while there is nothing to show. A relay that died
    // after delivering frames still leaves the last one on screen, and an error
    // beside a picture that is visibly there reads as a bug in the console.
    let error = (seq.is_none() && !relay.connected())
        .then(|| relay.error())
        .flatten();
    (seq, error)
}

/// `GET /api/box/:agent/:slug/browser/frame` — the newest frame the console
/// holds for this box, as a JPEG.
///
/// A `GET` that returns an image, so the console's "every route is a GET" rule
/// is untouched and the page can point an `<img>` at it. The bytes are the
/// box's, which is why two headers are not optional: `nosniff` so a crafted
/// payload cannot be re-interpreted as anything but an image, and `no-store` so
/// a frame of somebody's page does not settle into a disk cache.
async fn api_browser_frame(
    State(state): State<Arc<AppState>>,
    Path((agent, slug)): Path<(String, String)>,
) -> Response {
    let id = format!("env/{agent}/{slug}");
    let relays = state.frames.clone();
    let frame = tokio::task::spawn_blocking(move || {
        let held = relays.lock().unwrap_or_else(|e| e.into_inner());
        held.get(&id).and_then(|r| r.latest())
    })
    .await
    .ok()
    .flatten();

    match frame {
        Some(f) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            f.jpeg,
        )
            .into_response(),
        // Not an error: a box with no live view, or one whose page has not
        // changed since the reader attached, simply has no frame.
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// `GET /api/probe` — what this host can actually enforce. The same report
/// `h5i box capabilities --json` prints, so the console's top strip and the
/// CLI can never disagree.
async fn api_probe() -> Json<crate::sandbox::CapabilitiesReport> {
    // A capability report is a diagnostic, so it is probed fresh rather than
    // served from the per-boot podman cache — exactly as `box probe` does. The
    // freshness is a property of the call, not of the process environment: this
    // is a long-lived multithreaded server, and a `set_var` here would race
    // every other thread's `getenv` and outlive the request that wanted it.
    let report = tokio::task::spawn_blocking(crate::sandbox::capabilities_report_fresh).await;
    Json(report.unwrap_or_else(|_| crate::sandbox::capabilities_report_fresh()))
}

/// Every route, wired to `state`. Extracted so tests can drive the surface
/// without a socket — and so the "all of them are GETs" claim is checkable.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/*path", get(asset))
        .route("/api/probe", get(api_probe))
        .route("/api/boxes", get(api_boxes))
        .route("/api/box/:agent/:slug", get(api_box))
        .route("/api/box/:agent/:slug/receipts/:id", get(api_receipt))
        .route("/api/box/:agent/:slug/browser", get(api_browser))
        .route("/api/box/:agent/:slug/browser/frame", get(api_browser_frame))
        .layer(axum::middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

// ── entry point ──────────────────────────────────────────────────────────────

/// A bound console, before it starts serving.
///
/// Split from [`Console::serve`] the way [`crate::view`] splits its forward:
/// binding can fail (a port in use is the common case) and the caller wants to
/// print the URL — token and all — before blocking forever.
pub struct Console {
    listener: std::net::TcpListener,
    state: Arc<AppState>,
}

impl Console {
    /// Bind loopback and mint this session's token. `port` 0 asks the OS for a
    /// free one, which [`Console::url`] then reports.
    pub fn bind(repo_path: PathBuf, port: u16) -> Result<Console, H5iError> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                H5iError::Metadata(format!(
                    "port {port} is already in use — pick another with `h5i ui --port <N>`"
                ))
            } else {
                H5iError::Io(e)
            }
        })?;
        let token = crate::token::hex(TOKEN_BYTES)?;
        Ok(Console {
            listener,
            state: Arc::new(AppState::new(repo_path, token)),
        })
    }

    /// The one URL that works. The token is in the query string because that
    /// is the only channel a terminal has to a browser; the page trades it for
    /// a cookie on first load.
    pub fn url(&self) -> Result<String, H5iError> {
        let addr = self.listener.local_addr().map_err(H5iError::Io)?;
        Ok(format!("http://{}/?token={}", addr, self.state.token))
    }

    /// Serve until the process is interrupted. Builds its own runtime, so
    /// nothing above this module needs to know the console is async.
    pub fn serve(self) -> Result<(), H5iError> {
        self.listener
            .set_nonblocking(true)
            .map_err(H5iError::Io)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(H5iError::Io)?;
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(self.listener).map_err(H5iError::Io)?;
            axum::serve(listener, router(self.state))
                .await
                .map_err(H5iError::Io)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::BrowserEvidence;
    use crate::sandbox_policy::{EgressHost, EgressSummary};

    fn manifest(isolation: &str) -> EnvManifest {
        EnvManifest {
            id: "env/claude/fix-auth".into(),
            agent: "claude".into(),
            slug: "fix-auth".into(),
            base_commit: "a".repeat(40),
            base_tree: "b".repeat(40),
            parent_branch: "main".into(),
            branch: "refs/heads/h5i/env/claude/fix-auth".into(),
            source: "repo".into(),
            profile: "default".into(),
            policy_digest: "c".repeat(64),
            isolation_claim: isolation.into(),
            backend: "worktree".into(),
            created_at: "2026-08-05T00:00:00Z".into(),
            updated_at: "2026-08-05T00:00:00Z".into(),
            status: "idle".into(),
            captures: vec![],
            service_digest: None,
            persona_digest: None,
            pr: None,
            pr_head_ref: None,
        }
    }

    fn receipt(source: &str, exit: i32) -> ExecRecord {
        ExecRecord {
            id: "r0".into(),
            timestamp: "2026-08-05T01:00:00Z".into(),
            env_id: "env/claude/fix-auth".into(),
            policy_digest: None,
            source: source.into(),
            cmd: Some("cargo test".into()),
            cwd: None,
            exit_code: Some(exit),
            timed_out: false,
            wall_ms: None,
            cpu_ms: None,
            max_rss_kb: None,
            git_tree: None,
            files: vec![],
            egress: None,
            browser: None,
            share: None,
            redactions: vec![],
            raw_oid: "d".repeat(64),
            raw_size: 0,
            raw_lines: 0,
            raw_truncated: false,
        }
    }

    /// The box writes `inbox-capture` records into its own spool. Counting any
    /// unknown source as host-observed let it clear the grey "box-claimed"
    /// badge — the one distinction this screen is built on.
    #[test]
    fn a_box_being_shared_right_now_says_so() {
        // The receipt lands when the share *ends*, so until this the console
        // showed nothing at all while a box was open to somebody on another
        // machine — for the one lane that lets somebody in, on a screen whose
        // job is saying what is pressing on a boundary.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(shared_now(dir.path()).is_none(), "no file, nothing to say");

        let now = chrono::Utc::now().timestamp();
        let record = |extra: &str, revoked: bool| {
            serde_json::json!({
                "v": 1,
                "box_id": "env/a/demo",
                "port": 3000,
                "transport": "tunnel",
                "endpoint": "https://x",
                "started_at": "2026-01-01T00:00:00Z",
                "pid": std::process::id(),
                "winding_up": extra == "winding",
                "grants": [
                    {"id": "a", "secret_sha256": "ff", "revoked": revoked, "expires_at": now + 600},
                    {"id": "b", "secret_sha256": "ff", "revoked": true,    "expires_at": now + 600},
                    {"id": "c", "secret_sha256": "ff", "revoked": false,   "expires_at": now - 1},
                ],
            })
            .to_string()
        };

        std::fs::write(dir.path().join("share.json"), record("", false)).expect("write");
        let got = shared_now(dir.path()).expect("a live share");
        assert_eq!(got.transport, "tunnel");
        assert_eq!(got.port, 3000);
        // Only the grants that can still admit somebody: one revoked and one
        // expired are two tickets that let nobody in.
        assert_eq!(got.grants, 1);

        // A share that is winding up has a live pid and admits nobody. The
        // badge used to say "somebody outside can reach port 3000 right now"
        // of it, for as long as that process took to write its receipt.
        std::fs::write(dir.path().join("share.json"), record("winding", false)).expect("write");
        assert!(shared_now(dir.path()).is_none(), "a winding-up share is not admitting anyone");

        // And so does one whose every grant has been revoked.
        std::fs::write(dir.path().join("share.json"), record("", true)).expect("write");
        assert!(shared_now(dir.path()).is_none(), "no live grant is nobody admitted");

        // A record left by a process that is gone is not a share.
        std::fs::write(dir.path().join("share.json"), "{\"pid\":0}").expect("write");
        assert!(shared_now(dir.path()).is_none());
    }

    #[test]
    fn a_share_is_host_observed_evidence() {
        // h5i owns both ends of the share bridge and the box supplies none of
        // it, so leaving the lane off the list inverted the badge: a box whose
        // only receipt was a share read as having no host-observed evidence at
        // all, which is the grey "the box told us this" badge — for the one
        // lane the box cannot touch.
        assert!(HOST_OBSERVED_LANES.contains(&"share"));
    }

    #[test]
    fn box_written_lanes_are_never_counted_as_host_observed() {
        let m = manifest("container");
        for lane in ["tee-shim", "inbox-capture", "something-added-later"] {
            let s = signals(&m, &[receipt(lane, 0)]);
            assert_eq!(s.host_observed, 0, "{lane} must not read as host-observed");
            assert_eq!(s.box_claimed, 1, "{lane}");
            assert!(s.box_claimed_only, "{lane} alone means box-claimed only");
        }
        for lane in HOST_OBSERVED_LANES {
            let s = signals(&m, &[receipt(lane, 0)]);
            assert_eq!(s.host_observed, 1, "{lane} is a host lane");
            assert!(!s.box_claimed_only, "{lane}");
        }
    }

    /// A DNS-rebinding page arrives with Host and Origin that match each other,
    /// so comparing them proves nothing. The Host has to name loopback.
    #[test]
    fn the_gate_requires_a_loopback_host() {
        for good in ["127.0.0.1:8080", "localhost:1", "[::1]:9", "LOCALHOST", "127.0.0.1"] {
            assert!(is_loopback_host(good), "{good} should be accepted");
        }
        for bad in ["evil.test:8080", "example.com", "127.0.0.1.evil.test", "10.0.0.1:80"] {
            assert!(!is_loopback_host(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn a_refused_destination_is_the_only_thing_that_turns_a_box_red() {
        let m = manifest("container");
        // A failing test run is worth attention, but it is not the boundary
        // saying no — the distinction the old dashboard's red/amber split
        // existed to preserve.
        let noisy = signals(&m, &[receipt("host-env-run", 1)]);
        assert_eq!(noisy.verdict, "attention");
        assert_eq!(noisy.failed, 1);

        let mut denied = receipt("host-env-run", 0);
        denied.egress = Some(EgressSummary {
            allowed: 3,
            denied: 2,
            hosts: vec![EgressHost {
                host: "evil.test".into(),
                port: 443,
                allowed: 0,
                denied: 2,
            }],
            hosts_truncated: false,
            log: None,
        });
        let red = signals(&m, &[denied]);
        assert_eq!(red.verdict, "denial");
        assert_eq!(red.egress_denied, 2);
        assert_eq!(red.denied_hosts, vec!["evil.test:443".to_string()]);
    }

    #[test]
    fn weak_evidence_is_reported_beside_the_verdict_not_folded_into_it() {
        // Workspace tier confines nothing, and a box that only ever saw its
        // own tee shim has no host-observed record. Both are properties of the
        // evidence, so neither is allowed to masquerade as a clean run or as a
        // boundary trip.
        let clean = signals(&manifest("workspace"), &[receipt("tee-shim", 0)]);
        assert_eq!(clean.verdict, "clean");
        assert!(clean.weak_isolation);
        assert!(clean.box_claimed_only);
        assert_eq!(clean.box_claimed, 1);
        assert_eq!(clean.host_observed, 0);

        let observed = signals(&manifest("container"), &[receipt("host-env-run", 0)]);
        assert!(!observed.weak_isolation);
        assert!(!observed.box_claimed_only);
    }

    #[test]
    fn what_the_page_reported_counts_as_something_to_look_at() {
        let mut r = receipt("host-env-run", 0);
        r.browser = Some(BrowserEvidence {
            verb: Some("click".into()),
            console: vec!["TypeError: x is not a function".into()],
            errors: vec![],
            failed_requests: vec!["GET /api/me → (blocked)".into()],
            truncated: false,
            unavailable: false,
        });
        let s = signals(&manifest("container"), &[r]);
        assert_eq!(s.browser_issues, 2);
        assert_eq!(s.verdict, "attention");
    }

    #[test]
    fn the_most_pressing_box_sorts_first() {
        let m = manifest("container");
        let mut denied = receipt("host-env-run", 0);
        denied.egress = Some(EgressSummary {
            allowed: 0,
            denied: 1,
            hosts: vec![],
            hosts_truncated: false,
            log: None,
        });
        assert!(rank(&signals(&m, &[denied])) > rank(&signals(&m, &[receipt("host-env-run", 1)])));
        assert!(
            rank(&signals(&m, &[receipt("host-env-run", 1)]))
                > rank(&signals(&m, &[receipt("host-env-run", 0)]))
        );
    }

    #[test]
    fn nothing_gets_in_without_the_token() {
        let tok = "0123456789abcdef";
        let ok = |q: Option<&str>, c: Option<&str>| {
            authorize(q, c, None, tok, "http://127.0.0.1:8765").is_ok()
        };
        assert!(ok(Some("token=0123456789abcdef"), None));
        assert!(ok(None, Some("h5i_console=0123456789abcdef")));
        assert!(ok(Some("fps=10&token=0123456789abcdef"), None));
        assert!(!ok(None, None));
        assert!(!ok(Some("token=0123456789abcdee"), None));
        assert!(!ok(None, Some("h5i_console=nope")));
        // A cookie for something else on loopback is not this console's token.
        assert!(!ok(None, Some("jupyter=0123456789abcdef")));
    }

    #[test]
    fn a_page_on_another_origin_is_refused_even_holding_the_token() {
        let tok = "0123456789abcdef";
        let me = "http://127.0.0.1:8765";
        assert_eq!(
            authorize(Some("token=0123456789abcdef"), None, Some("http://evil.test"), tok, me),
            Err(Refusal::ForeignOrigin)
        );
        // Same-origin XHR sends Origin too, and must still work.
        assert!(authorize(None, Some("h5i_console=0123456789abcdef"), Some(me), tok, me).is_ok());
    }

    #[test]
    fn the_console_binds_loopback_and_puts_the_token_in_the_url() {
        let c = Console::bind(PathBuf::from("."), 0).expect("bind an ephemeral port");
        let url = c.url().expect("a bound listener has an address");
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        assert!(url.contains("?token="), "{url}");
        assert_eq!(c.state.token.len(), TOKEN_BYTES * 2);
    }
}
