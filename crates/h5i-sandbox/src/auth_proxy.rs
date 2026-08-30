//! Host-side **credential-injecting** egress proxy ("option 2", see
//! `docs/credential-proxy-design.md`): what lets an agent box authenticate to
//! its provider API without the long-lived token ever entering the box.
//!
//! The [`crate::container`] egress proxy tunnels TLS (`CONNECT`) and so can
//! never inject an `Authorization`/`x-api-key` header, the bytes being
//! end-to-end encrypted. This proxy terminates the box-to-proxy hop in cleartext
//! on host loopback and re-originates a fresh TLS request upstream, injecting
//! the real credential host-side. The agent is pointed at it with a base-URL
//! override (`ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL`) and a **dummy** token; the
//! genuine one lives only in this process's memory.
//!
//! Security properties, all fail-closed:
//!
//! - **Token never in the box.** The box sees the base URL and a per-run dummy.
//!   The real credential is resolved from h5i's own host environment and handed
//!   to the upstream request, never an env var, mount or argv in the box.
//! - **No SSRF.** The upstream origin is pinned to the runtime's single API host
//!   ([`RuntimeProxy::upstream_host`]) and the box's `Host` header is discarded.
//!   Ignoring the authority is necessary but not sufficient: the request target
//!   is appended to that origin, and a target not beginning with `/` extends the
//!   *authority* rather than the path, so `@evil.example/v1` makes the pinned
//!   name mere userinfo. The target must be origin-form ([`is_origin_form`]),
//!   and the assembled URL is re-parsed and required to resolve to the pinned
//!   origin before a byte is sent.
//! - **DNS-rebinding resistant.** The upstream host is resolved and pinned once
//!   at spawn, mirroring the egress proxy's `pin_dns`.
//! - **Loopback plus shared secret.** The listener binds `127.0.0.1`, reachable
//!   from the box via slirp `allow_host_loopback` at `10.0.2.2` and also by
//!   other host processes, so the real credential is injected only for a request
//!   presenting the per-run dummy token. The box is allowed to use the proxy;
//!   other host users are not.
//! - **Never logs the token or bodies.** [`Credential`]'s `Debug` is redacted.
//!
//! The live forwarder uses `reqwest` (blocking + rustls) so TLS, chunked
//! decoding and streaming (SSE) come from a vetted client rather than
//! hand-rolled.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::H5iError;
use crate::sandbox_policy::AgentRuntime;

/// The wire form a credential is injected as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CredHeader {
    /// `x-api-key: <value>` — Anthropic Console API keys.
    XApiKey,
    /// `Authorization: Bearer <value>` — OAuth tokens (Anthropic + OpenAI).
    Bearer,
}

/// A resolved upstream credential. Held only host-side; the value is never
/// logged, serialized, or placed in the box. `Debug` is deliberately redacted so
/// it cannot leak through a `{:?}` in an error path or trace.
#[derive(Clone)]
pub struct Credential {
    header: CredHeader,
    value: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credential({:?}, <redacted>)", self.header)
    }
}

/// Static per-runtime wiring: which single upstream API host the proxy forwards
/// to, and which box env vars point the agent at the proxy.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeProxy {
    /// The one upstream host the proxy will ever connect to (SSRF pin).
    pub upstream_host: &'static str,
    /// The base-URL override the box is given (agent SDK's API origin).
    pub base_url_var: &'static str,
    /// The env var the box carries the *dummy* token in (also the SDK's outgoing
    /// auth header, which doubles as the proxy's shared-secret gate).
    pub dummy_var: &'static str,
}

/// Resolve the per-runtime proxy wiring. Mirrors [`AgentRuntime::egress`]'s
/// primary API host.
pub fn runtime_proxy(rt: AgentRuntime) -> RuntimeProxy {
    match rt {
        AgentRuntime::Claude => RuntimeProxy {
            upstream_host: "api.anthropic.com",
            base_url_var: "ANTHROPIC_BASE_URL",
            dummy_var: "ANTHROPIC_AUTH_TOKEN",
        },
        AgentRuntime::Codex => RuntimeProxy {
            upstream_host: "api.openai.com",
            base_url_var: "OPENAI_BASE_URL",
            dummy_var: "OPENAI_API_KEY",
        },
    }
}

/// A non-empty, non-whitespace host env var, or `None`.
/// Read a credential from the host environment, **trimmed**.
///
/// Trimming is not cosmetic: `export TOKEN="$(cat ~/key)"` carries a trailing
/// newline, `HeaderValue` rejects it, and `send()` then fails for every request
/// — surfacing as a permanent 502 whose cause is deliberately not printed (the
/// right call for an upstream error, wrong for a local one). The sibling
/// `engage_grant_at` already trimmed; this path did not.
fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Resolve the genuine upstream credential from *h5i's own* host environment,
/// in the same precedence the runtime's own CLI uses. `None` (the common case
/// today) means no host-side credential is available, so the caller leaves the
/// box on its existing (in-box login) path rather than engaging the proxy — we
/// never *downgrade* an active protection, but we also never break a working
/// interactive-login flow that has no host token to broker.
pub fn resolve_credential(rt: AgentRuntime) -> Option<Credential> {
    match rt {
        AgentRuntime::Claude => {
            // Bearer token (proxy/gateway or subscription) wins over an API key,
            // matching Claude Code's own precedence.
            if let Some(v) = nonempty_env("ANTHROPIC_AUTH_TOKEN") {
                return Some(Credential { header: CredHeader::Bearer, value: v });
            }
            if let Some(v) = nonempty_env("ANTHROPIC_API_KEY") {
                return Some(Credential { header: CredHeader::XApiKey, value: v });
            }
            nonempty_env("CLAUDE_CODE_OAUTH_TOKEN")
                .map(|v| Credential { header: CredHeader::Bearer, value: v })
        }
        AgentRuntime::Codex => {
            nonempty_env("OPENAI_API_KEY").map(|v| Credential { header: CredHeader::Bearer, value: v })
        }
    }
}

/// A running credential-injecting proxy. Dropping the handle shuts the accept
/// loop down (and with it the only in-memory copy of the credential).
pub struct AuthProxyHandle {
    /// Loopback port the box reaches at `http://10.0.2.2:<port>`.
    pub port: u16,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AuthProxyHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept poll promptly.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Largest request body the proxy will buffer, in bytes.
///
/// `Content-Length` is attacker-controlled (the box sends it), so the buffer
/// must be sized from a constant rather than from the header — allocating
/// `Content-Length` bytes on trust lets a prompt-injected box exhaust *host*
/// memory with a single request. 32 MiB is comfortably above the largest real
/// request these APIs accept (a message with inline images).
const MAX_REQUEST_BODY: usize = 32 * 1024 * 1024;

/// Concurrent forwarded requests. The box is one client; a bound here stops a
/// loop of connections from spawning unbounded host threads.
const MAX_IN_FLIGHT: usize = 64;

/// How many *consecutive* failed `accept()` calls end the accept loop. Sized so
/// transient per-connection and fd-pressure errors (which retry at 25 ms) never
/// reach it, while a permanently broken listener still stops the thread inside
/// a few seconds instead of spinning forever.
const MAX_CONSECUTIVE_ACCEPT_ERRORS: usize = 256;

/// Holds one of the [`MAX_IN_FLIGHT`] concurrent-forward slots and releases it
/// on drop — including when the worker unwinds.
///
/// The count is the only thing standing between the box and an unbounded number
/// of host threads, so releasing it has to be unconditional. A bare
/// `fetch_sub` at the end of the worker closure is not: a panic in
/// `handle_client` skips it, and the slot is then held forever by a thread that
/// no longer exists.
struct InFlightSlot(Arc<ProxyState>);

impl Drop for InFlightSlot {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Immutable per-proxy state shared with worker threads.
struct ProxyState {
    /// The origin the proxy forwards to, parsed once at spawn:
    /// `https://api.anthropic.com` in production, an `http://127.0.0.1:<port>`
    /// origin in tests. Kept as a `Url` rather than a string because it is the
    /// reference every outgoing request is checked against (see
    /// [`handle_client`]) — a comparison a string cannot make.
    upstream: reqwest::Url,
    /// The single upstream host, forced as the outgoing `Host` header.
    upstream_host: String,
    /// The genuine credential injected into every forwarded request.
    credential: Credential,
    /// The per-run dummy token the box must present (shared-secret gate).
    client_token: String,
    /// Blocking HTTP client (TLS via rustls, DNS pinned, no proxy, no redirects).
    client: reqwest::blocking::Client,
    /// Requests currently being forwarded, against [`MAX_IN_FLIGHT`].
    in_flight: std::sync::atomic::AtomicUsize,
}

/// Spawn the proxy for a runtime with a resolved credential. `client_token` is
/// the unguessable per-run dummy the box will present. Production entry point:
/// forwards to the runtime's HTTPS API host, DNS-pinned.
/// Resolve a profile-declared grant into a live proxy.
///
/// The credential is read from `credential_env` **on the host** and never
/// leaves it: the box is handed the proxy's origin and a per-run token, and the
/// proxy adds the real header on the way out. Fail-closed — a grant whose
/// environment variable is unset is an error, not a box that silently talks to
/// the upstream unauthenticated.
pub fn engage_grant(
    grant: &crate::sandbox_policy::AuthGrant,
    tier_ok: bool,
) -> Result<Option<GrantEngagement>, H5iError> {
    engage_grant_at(grant, tier_ok, SLIRP_GATEWAY_HOST)
}

/// [`engage_grant`] with an explicit host-proxy address (see [`box_env_at`]).
pub fn engage_grant_at(
    grant: &crate::sandbox_policy::AuthGrant,
    tier_ok: bool,
    host: &str,
) -> Result<Option<GrantEngagement>, H5iError> {
    if !tier_ok || opted_out() {
        return Ok(None);
    }
    let value = std::env::var(&grant.credential_env)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            H5iError::Metadata(format!(
                "profile grants authenticated egress to {} but ${} is unset on the host — \
                 refusing rather than sending the box out unauthenticated",
                grant.host, grant.credential_env
            ))
        })?;
    let credential = Credential {
        header: CredHeader::Bearer,
        value,
    };
    let token = new_client_token()?;
    let handle = spawn_to_upstream(
        format!("https://{}", grant.host),
        grant.host.clone(),
        credential,
        token.clone(),
        true,
    )?;
    let no_proxy = format!("localhost,127.0.0.1,{host}");
    let box_env = vec![
        (
            grant.base_url_var.clone(),
            format!("http://{host}:{}", handle.port),
        ),
        // The gate token. Without it the box presents no credential, every
        // request is refused with 403, and the grant does nothing at all.
        (grant.token_var.clone(), token.clone()),
        ("NO_PROXY".to_string(), no_proxy.clone()),
        ("no_proxy".to_string(), no_proxy),
    ];
    Ok(Some(GrantEngagement { handle, box_env }))
}

/// A live proxy for one profile-declared grant.
pub struct GrantEngagement {
    /// Hold for the box's lifetime; dropping it shuts the proxy — and the only
    /// in-memory copy of the credential — down.
    pub handle: AuthProxyHandle,
    /// Box env additions: the base-URL override and `NO_PROXY`.
    pub box_env: Vec<(String, String)>,
}

pub fn spawn(
    rt: AgentRuntime,
    credential: Credential,
    client_token: String,
) -> Result<AuthProxyHandle, H5iError> {
    let host = runtime_proxy(rt).upstream_host.to_string();
    spawn_to_upstream(format!("https://{host}"), host, credential, client_token, true)
}

/// Core spawn. `upstream_base` is the scheme+host(+port) origin; `upstream_host`
/// is the bare host for the forced `Host` header. `pin_dns` resolves+pins the
/// host once (production HTTPS); tests point at a loopback `http://` origin and
/// skip pinning.
fn spawn_to_upstream(
    upstream_base: String,
    upstream_host: String,
    credential: Credential,
    client_token: String,
    pin_dns: bool,
) -> Result<AuthProxyHandle, H5iError> {
    let mut builder = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        // No total timeout: streaming (SSE) responses are long-lived. A bounded
        // connect timeout still fails a dead upstream fast.
        .connect_timeout(Duration::from_secs(15));
    if pin_dns
        && let Ok(mut addrs) = (upstream_host.as_str(), 443u16).to_socket_addrs()
        && let Some(addr) = addrs.next()
    {
        builder = builder.resolve(&upstream_host, addr);
    }
    let client = builder
        .build()
        .map_err(|e| H5iError::Metadata(format!("auth proxy: build HTTP client: {e}")))?;

    // Parse the origin once, here, so every request can be checked against it
    // instead of against a string that concatenation might have moved.
    let upstream = reqwest::Url::parse(&upstream_base).map_err(|e| {
        H5iError::Metadata(format!("auth proxy: invalid upstream origin '{upstream_base}': {e}"))
    })?;

    let listener = TcpListener::bind("127.0.0.1:0").map_err(H5iError::Io)?;
    let port = listener.local_addr().map_err(H5iError::Io)?.port();
    listener.set_nonblocking(true).map_err(H5iError::Io)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let state = Arc::new(ProxyState {
        upstream,
        upstream_host,
        credential,
        client_token,
        client,
        in_flight: std::sync::atomic::AtomicUsize::new(0),
    });

    let join = std::thread::spawn(move || {
        // Consecutive non-`WouldBlock` accept failures. `accept` reports
        // per-connection and per-host conditions that say nothing about the
        // listener: `ECONNABORTED` when a client RSTs between SYN and accept,
        // `EMFILE`/`ENFILE` under fd pressure. Treating any of them as fatal
        // killed the accept loop for good — and with it the box's only
        // authenticated egress for the rest of its life, since all it holds is
        // the dummy. Both are triggerable by any local process (connect, then
        // reset), which is precisely the actor the token gate exists for, so
        // "the listener dies" cannot be their reward. Retry instead, and keep a
        // consecutive count so a genuinely dead listener (a closed fd spinning
        // `EBADF`) still terminates the thread rather than burning a core.
        let mut consecutive_errors = 0usize;
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((client, _)) => {
                    consecutive_errors = 0;
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    // The accepted socket may arrive non-blocking (see
                    // `handle_client`, which is where that is corrected — at the
                    // point the blocking reads are actually made).
                    let state = state.clone();
                    // Bound concurrent forwards: the box is a single client, and
                    // an accept loop that spawns a thread per connection is a
                    // host resource the box should not be able to grow without
                    // limit. Over the cap we answer and close rather than queue.
                    let live = state
                        .in_flight
                        .fetch_add(1, Ordering::SeqCst);
                    if live >= MAX_IN_FLIGHT {
                        state.in_flight.fetch_sub(1, Ordering::SeqCst);
                        let mut client = client;
                        // `Drain::Buffered`: this is the one refusal answered on
                        // the accept thread, so it must not wait on the peer.
                        write_status_draining(
                            &mut client,
                            503,
                            "Service Unavailable",
                            Drain::Buffered,
                        );
                        continue;
                    }
                    // The release is a guard, not a statement after the call.
                    // Decrementing at the end of the closure leaks the slot on
                    // any path that does not reach it — a panic in
                    // `handle_client`, or a `spawn` that never runs the closure
                    // at all — and a leaked slot is permanent: after
                    // MAX_IN_FLIGHT of them the proxy answers 503 forever and
                    // the box loses authenticated egress for good.
                    let slot = InFlightSlot(state.clone());
                    // `Builder::spawn`, not `thread::spawn`: the latter
                    // *panics* when the OS refuses a thread, and that panic is
                    // in the accept loop — so the one condition `MAX_IN_FLIGHT`
                    // exists to bound would end the loop anyway, and the box
                    // would lose authenticated egress for good. The slot goes
                    // with the closure that was never run.
                    let _ = std::thread::Builder::new().spawn(move || {
                        let slot = slot;
                        let _ = handle_client(client, &slot.0);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    consecutive_errors = 0;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => {
                    consecutive_errors += 1;
                    if consecutive_errors > MAX_CONSECUTIVE_ACCEPT_ERRORS {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    });

    Ok(AuthProxyHandle { port, stop, join: Some(join) })
}

/// The address the box reaches the host's loopback on. The Linux tiers put the
/// box in its own network namespace whose slirp uplink maps host loopback to the
/// gateway; macOS has no netns, so the box shares the host's loopback and dials
/// it directly. Everything else about the wiring is identical.
pub const SLIRP_GATEWAY_HOST: &str = "10.0.2.2";
/// Host loopback as seen from a macOS Seatbelt box (no network namespace).
pub const LOOPBACK_HOST: &str = "127.0.0.1";

/// Box env additions that point the agent at the proxy: the base-URL override,
/// the per-run dummy token, and a `NO_PROXY` that excludes the proxy's own
/// address so the base URL is dialed directly (not re-wrapped through the egress
/// CONNECT proxy). All non-secret — safe to pass by value.
pub fn box_env(rt: AgentRuntime, port: u16, client_token: &str) -> Vec<(String, String)> {
    box_env_at(rt, SLIRP_GATEWAY_HOST, port, client_token)
}

/// [`box_env`] with an explicit proxy host, for backends whose box reaches the
/// host at an address other than the slirp gateway (macOS Seatbelt boxes share
/// the host's loopback).
pub fn box_env_at(
    rt: AgentRuntime,
    host: &str,
    port: u16,
    client_token: &str,
) -> Vec<(String, String)> {
    let rp = runtime_proxy(rt);
    let no_proxy = format!("localhost,127.0.0.1,{host}");
    vec![
        (rp.base_url_var.to_string(), format!("http://{host}:{port}")),
        (rp.dummy_var.to_string(), client_token.to_string()),
        ("NO_PROXY".to_string(), no_proxy.clone()),
        ("no_proxy".to_string(), no_proxy),
    ]
}

// ─── request parsing (pure; unit-tested) ─────────────────────────────────────

/// A parsed request head. `headers` preserves order and original casing minus
/// the ones we strip; `content_length` drives body reading.
#[derive(Debug, PartialEq, Eq)]
struct ParsedReq {
    method: String,
    /// Origin-form path (`/v1/messages`) exactly as the box sent it.
    path: String,
    headers: Vec<(String, String)>,
    content_length: usize,
    /// The client framed its body with `Transfer-Encoding` rather than
    /// `Content-Length`. This proxy reads a `Content-Length`-framed body only,
    /// so such a request must be refused rather than forwarded — see
    /// [`handle_client`].
    chunked: bool,
}

/// Header names never forwarded upstream: hop-by-hop framing, the box's own
/// (dummy) auth, and `Host` (we force the pinned upstream's).
fn is_stripped_request_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "host"
            | "authorization"
            | "x-api-key"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

/// Is `target` a safe **origin-form** request target (`/v1/messages?beta=1`)?
///
/// This is the SSRF gate, and it is load-bearing rather than cosmetic. The
/// outgoing URL is the pinned origin with this string appended, and a URL's
/// authority runs until the first `/` — so a target that does not start with
/// `/` can extend the *authority* instead of the path. The sharp case is
/// userinfo: `@evil.example/v1/messages` turns
/// `https://api.anthropic.com` + target into
/// `https://api.anthropic.com@evil.example/v1/messages`, whose host is
/// `evil.example` and whose userinfo is the name we thought we had pinned. The
/// proxy would then open TLS to the attacker (SNI and certificate validation
/// following the *attacker's* name, so any ordinary certificate satisfies it)
/// and attach the real credential — the exact exfiltration the credential proxy
/// exists to prevent.
///
/// Origin-form is also all these SDKs ever send, so nothing legitimate is lost.
/// A leading `//` is refused too: it is the protocol-relative form, harmless
/// against today's origin but one trailing slash away from meaning `//host/`.
/// Control characters and whitespace are refused because they are request
/// smuggling, not paths.
fn is_origin_form(target: &str) -> bool {
    target.starts_with('/')
        && !target.starts_with("//")
        && !target
            .bytes()
            .any(|b| b <= b' ' || b == 0x7f || b == b'\\')
}

/// Parse the request line + headers. Returns `None` on a malformed head.
fn parse_request_head(head: &[u8]) -> Option<ParsedReq> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    // Require a plausible HTTP version token to reject junk.
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) {
        return None;
    }
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok()?;
        }
        // Any `Transfer-Encoding` other than the no-op `identity` means the body
        // is not `Content-Length`-framed. Recorded rather than ignored: see
        // [`ParsedReq::chunked`].
        if name.eq_ignore_ascii_case("transfer-encoding") && !value.eq_ignore_ascii_case("identity")
        {
            chunked = true;
        }
        if !is_stripped_request_header(name) {
            headers.push((name.to_string(), value.to_string()));
        }
    }
    Some(ParsedReq { method, path, headers, content_length, chunked })
}

/// True iff the request presents the expected per-run dummy token (as either
/// `Authorization: Bearer <t>` or `x-api-key: <t>`). Checked on the *raw* head
/// before the auth headers are stripped. Constant-ish comparison is unnecessary
/// (the secret is per-run and only gates other host users, not the box).
fn client_authorized(head: &[u8], client_token: &str) -> bool {
    if client_token.is_empty() {
        return false;
    }
    let text = String::from_utf8_lossy(head);
    for line in text.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let presented = match name.as_str() {
            "authorization" => value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")),
            "x-api-key" => Some(value),
            _ => None,
        };
        if presented == Some(client_token) {
            return true;
        }
    }
    false
}

// ─── live forwarding ─────────────────────────────────────────────────────────

/// Read the request head (request line + headers) up to the blank line, capped.
fn read_head(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    // Bounded overall, not just per read(). The socket's read timeout bounds a
    // single syscall, so a client dribbling one header byte just under that
    // interval held its in-flight slot indefinitely. Sixty-four such clients
    // — any local process, which is the actor the token gate exists for —
    // exhausted MAX_IN_FLIGHT and the agent's own API call got a 503 for the
    // life of the box, with no fallback because its only credential is the
    // dummy.
    let deadline = std::time::Instant::now() + HEAD_DEADLINE;
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request head exceeded its deadline",
            ));
        }
        let n = s.read(&mut byte)?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }
    Ok(buf)
}

/// Whole-head deadline. Generous for a real client on a slow link, short enough
/// that a stalled connection cannot hold an in-flight slot for a session.
const HEAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the post-refusal drain may wait on the peer.
#[derive(Clone, Copy)]
enum Drain {
    /// Wait up to [`DRAIN_TIMEOUT`] for the peer to finish sending. Safe only
    /// on a worker thread, where the sole thing held is that worker's own
    /// in-flight slot.
    Waiting,
    /// Take what the kernel has already buffered and return — never park on the
    /// peer. For the one refusal written *on the accept loop*.
    Buffered,
}

/// Answer with a bare status and close.
///
/// Every refusal path in this module lands here, and each one refuses *before*
/// the request body has been read — so the socket still holds bytes the peer
/// sent. Closing on unread data makes the kernel send an RST, which discards
/// the response we just wrote: the client sees `ECONNRESET`, not the `413`/`411`
/// that says what it did wrong. A short bounded drain lets the status actually
/// arrive. Bounded in both bytes and time because the data is attacker-supplied
/// — the point is to be polite, not to read whatever the peer feels like
/// sending.
fn write_status(client: &mut TcpStream, code: u16, reason: &str) {
    write_status_draining(client, code, reason, Drain::Waiting);
}

/// [`write_status`], with the drain policy named.
///
/// The over-capacity `503` is the one refusal answered on the accept thread,
/// and a [`Drain::Waiting`] drain there is a stall the box can ask for: a peer
/// that sends a request and then simply holds the connection open parks the
/// accept loop for the whole window, once per excess connection. The loop is
/// the proxy's only way to pick up the connections that would *free* the slots
/// the 503 is about, so waiting on the box's slowest peer to answer "too many
/// requests" is exactly backwards — and losing the accept loop is the outcome
/// [`MAX_IN_FLIGHT`] and the accept-error retry above both exist to prevent.
/// That path drains non-blocking instead: the request head is already in the
/// receive buffer in the ordinary case, so the refusal still lands rather than
/// being discarded by an RST, and a peer that is still mid-send costs the loop
/// nothing.
fn write_status_draining(client: &mut TcpStream, code: u16, reason: &str, drain: Drain) {
    let _ = client.write_all(
        format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .as_bytes(),
    );
    let _ = client.flush();
    let _ = client.shutdown(std::net::Shutdown::Write);
    match drain {
        Drain::Waiting => {
            let _ = client.set_read_timeout(Some(DRAIN_TIMEOUT));
        }
        // `WouldBlock` the moment the socket is empty, which the loop below
        // treats as end-of-drain — so no read can outlast what is already here.
        Drain::Buffered => {
            let _ = client.set_nonblocking(true);
        }
    }
    let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
    let mut scratch = [0u8; 8192];
    let mut drained = 0usize;
    while drained < MAX_DRAIN_BYTES && std::time::Instant::now() < deadline {
        match client.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

/// Wall-clock and byte bounds on the post-refusal drain in [`write_status`].
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_DRAIN_BYTES: usize = 256 * 1024;

/// Response headers dropped when relaying upstream→box: we re-frame the body as
/// connection-close, so any upstream length/encoding framing must not leak.
fn is_stripped_response_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "content-length"
            | "proxy-connection"
            | "trailer"
            | "upgrade"
    )
}

fn handle_client(mut client: TcpStream, state: &ProxyState) -> std::io::Result<()> {
    // `read_head` and the `read_exact` below are blocking reads, so say so
    // rather than assume it. The listener is non-blocking (the accept loop polls
    // `stop`) and on macOS — BSD `accept`, where Linux uses `accept4` — the
    // accepted socket *inherits* that flag. Left inherited, any read that has to
    // wait for the next packet fails with `WouldBlock`, and a well-formed
    // request is answered `400`: a request body only has to exceed one segment
    // to arrive split, which every real prompt does. That is the primary path on
    // the macOS supervised tier, not an edge case.
    //
    // `?`, not `let _ =`: continuing with a non-blocking socket reinstates that
    // exact bug, silently. Dropping the connection is the honest failure.
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(Duration::from_secs(60)))?;
    let head = read_head(&mut client)?;

    // Gate on the per-run shared secret BEFORE doing any upstream work: an
    // unauthenticated host process gets a 403 and never triggers a credentialed
    // call.
    if !client_authorized(&head, &state.client_token) {
        write_status(&mut client, 403, "Forbidden");
        return Ok(());
    }
    let Some(req) = parse_request_head(&head) else {
        write_status(&mut client, 400, "Bad Request");
        return Ok(());
    };

    // SSRF gate, before anything is read or forwarded: only an origin-form
    // target may be appended to the pinned origin. See [`is_origin_form`].
    if !is_origin_form(&req.path) {
        write_status(&mut client, 403, "Forbidden");
        return Ok(());
    }

    // Never allocate on an attacker-supplied length.
    if req.content_length > MAX_REQUEST_BODY {
        write_status(&mut client, 413, "Payload Too Large");
        return Ok(());
    }

    // Only `Content-Length` framing is read below, and `transfer-encoding` is
    // stripped on the way out. A chunked request therefore had its body silently
    // dropped and was re-originated upstream — with the real credential attached
    // — as a bodyless request. Refuse instead: a credential-injecting proxy must
    // never send a request that is not the one the client asked for, and the
    // caller gets a status it can act on rather than an upstream 400.
    if req.chunked {
        write_status(&mut client, 411, "Length Required");
        return Ok(());
    }

    // Read the request body (Content-Length framed; these SDKs always send one).
    let mut body = vec![0u8; req.content_length];
    if req.content_length > 0 && client.read_exact(&mut body).is_err() {
        write_status(&mut client, 400, "Bad Request");
        return Ok(());
    }

    // Build the upstream request against the PINNED origin, injecting the
    // genuine credential. The target was already checked to be origin-form; the
    // parse below is the second, independent check — whatever the concatenation
    // produced, it must still resolve to exactly the origin we pinned at spawn,
    // with no userinfo. Two cheap checks guard the one irreversible act this
    // process performs: handing the real credential to whoever answers.
    let base = state.upstream.as_str().trim_end_matches('/');
    let url = match reqwest::Url::parse(&format!("{base}{}", req.path)) {
        Ok(u)
            if u.origin() == state.upstream.origin()
                && u.username().is_empty()
                && u.password().is_none() =>
        {
            u
        }
        _ => {
            write_status(&mut client, 403, "Forbidden");
            return Ok(());
        }
    };
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            write_status(&mut client, 400, "Bad Request");
            return Ok(());
        }
    };
    let mut builder = state.client.request(method, url).header("host", &state.upstream_host);
    for (name, value) in &req.headers {
        builder = builder.header(name, value);
    }
    builder = match state.credential.header {
        CredHeader::XApiKey => builder.header("x-api-key", &state.credential.value),
        CredHeader::Bearer => builder.header("authorization", format!("Bearer {}", state.credential.value)),
    };
    if !body.is_empty() {
        builder = builder.body(body);
    }

    let mut resp = match builder.send() {
        Ok(r) => r,
        Err(_) => {
            // Never surface the upstream error text — it can echo request detail.
            write_status(&mut client, 502, "Bad Gateway");
            return Ok(());
        }
    };

    // Relay: status line, filtered headers, then stream the body until EOF under
    // connection-close framing (works for both bounded JSON and unbounded SSE).
    let status = resp.status();
    let reason = status.canonical_reason().unwrap_or("");
    let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), reason);
    for (name, value) in resp.headers() {
        if is_stripped_response_header(name.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            out.push_str(&format!("{}: {}\r\n", name.as_str(), v));
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    client.write_all(out.as_bytes())?;
    // Stream the decoded body as it arrives.
    let _ = std::io::copy(&mut resp, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    Ok(())
}

/// A short, unguessable per-run token the box presents to the proxy.
///
/// Not itself a credential, but it is the only thing standing between *other
/// local processes* and a proxy that injects the real one, so the entropy has to
/// come from the OS rather than from a fast general-purpose PRNG whose state a
/// same-host attacker could plausibly reconstruct. 128 bits, hex.
///
/// Fails rather than falls back: a host that cannot produce entropy must not get
/// a guessable gate on a credential-injecting proxy.
pub fn new_client_token() -> Result<String, H5iError> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| {
        H5iError::Metadata(format!(
            "auth proxy: no OS entropy for the per-run token ({e}) — refusing to mint a \
             guessable one (fail-closed)"
        ))
    })?;
    let mut out = String::with_capacity("h5i-proxy-".len() + raw.len() * 2);
    out.push_str("h5i-proxy-");
    for b in raw {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// The operator opt-out: keep the box on its own in-box login (e.g. to bill a
/// subscription logged in inside the box rather than a host-exported token).
pub fn opted_out() -> bool {
    std::env::var("H5I_CREDENTIAL_PROXY").is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        v == "off" || v == "0" || v == "false"
    })
}

/// A live credential-proxy engagement for a run.
pub struct Engagement {
    /// Hold for the box's lifetime; dropping it shuts the proxy (and the only
    /// in-memory copy of the credential) down.
    pub handle: AuthProxyHandle,
    /// Box env additions: base-URL override + per-run dummy token + `NO_PROXY`.
    pub box_env: Vec<(String, String)>,
    /// The runtime, so a kernel-tier caller knows which credential file to scrub
    /// from its per-env HOME copy. Read by `supervisor::run_supervised` on the
    /// kernel-tier targets (Linux x86_64/aarch64, macOS); the container tier
    /// ignores it. On any other target that reader is `cfg`-compiled out, so the
    /// field is legitimately unread — allow it there (the `cfg` mirrors the
    /// reader's gate).
    #[cfg_attr(
        not(any(
            all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
            target_os = "macos"
        )),
        allow(dead_code)
    )]
    pub runtime: AgentRuntime,
}

/// Decide + spawn the credential-injecting proxy for a run. Shared by both the
/// container and supervised backends (their only difference is `tier_ok` —
/// whether the box can reach the host proxy on this tier).
///
/// `Ok(None)` means the proxy does not apply: opted out, an unsupported tier, a
/// non-agent profile, or no host credential to broker. Those keep the box on
/// its existing in-box-login path and are not a downgrade of anything.
///
/// `Err` means the proxy was *supposed* to engage and could not. That is a
/// different thing entirely and must not be swallowed: the caller would keep
/// the real credential in the box's HOME copy *and* open the full `net.egress`
/// allowlist instead of narrowing to the proxy port, which is precisely the
/// state this module's header claims is unreachable.
pub fn engage(profile_name: &str, tier_ok: bool) -> Result<Option<Engagement>, H5iError> {
    engage_at(profile_name, tier_ok, SLIRP_GATEWAY_HOST)
}

/// [`engage`] for a backend whose box reaches the host proxy at `host` rather
/// than the slirp gateway — the macOS Seatbelt tiers, where the box shares the
/// host's loopback instead of living in its own network namespace.
pub fn engage_at(
    profile_name: &str,
    tier_ok: bool,
    host: &str,
) -> Result<Option<Engagement>, H5iError> {
    if !tier_ok || opted_out() {
        return Ok(None);
    }
    let Some(rt) = AgentRuntime::from_profile_name(profile_name) else {
        return Ok(None);
    };
    let Some(cred) = resolve_credential(rt) else {
        return Ok(None);
    };
    // From here the proxy is expected. Entropy or bind failure is a refusal,
    // not a fallback: silently continuing would put the long-lived token back
    // in the box and widen its egress at the same time.
    let token = new_client_token()?;
    let handle = spawn(rt, cred, token.clone()).map_err(|e| {
        H5iError::Metadata(format!(
            "credential proxy failed to start ({e}) — refusing to run the box with its own \
             credentials and the full egress allowlist instead (fail-closed). Set \
             H5I_CREDENTIAL_PROXY=off to opt out deliberately."
        ))
    })?;
    Ok(Some(Engagement {
        box_env: box_env_at(rt, host, handle.port, &token),
        handle,
        runtime: rt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    /// Serializes the env-mutating tests: cargo runs tests as parallel threads
    /// that share one process environment, so credential-resolution tests would
    /// otherwise race on the same vars. Poison-tolerant (a panicking test must
    /// not wedge the rest).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The not-applicable cases stay `Ok(None)`: they keep a working in-box
    /// login working and are not a downgrade of anything.
    #[test]
    fn engage_reports_not_applicable_as_ok_none() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Unsupported tier.
        assert!(engage_at("agent-claude", false, "127.0.0.1").unwrap().is_none());
        // Not an agent profile.
        assert!(engage_at("default", true, "127.0.0.1").unwrap().is_none());
        // Agent profile with no host credential to broker.
        for v in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
            // Safety: single-threaded test; no other thread reads the environment.
            unsafe {
                std::env::remove_var(v);
            }
        }
        assert!(engage_at("agent-claude", true, "127.0.0.1").unwrap().is_none());
    }

    #[test]
    fn resolve_credential_precedence_claude() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for v in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
            // Safety: single-threaded test; no other thread reads the environment.
            unsafe {
                std::env::remove_var(v);
            }
        }
        assert!(resolve_credential(AgentRuntime::Claude).is_none());

        // API key → x-api-key.
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-xyz");
        }
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::XApiKey);
        assert_eq!(c.value, "sk-ant-xyz");

        // Bearer token wins over the API key.
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("ANTHROPIC_AUTH_TOKEN", "brr");
        }
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "brr");

        // With neither, the long-lived OAuth token is the last resort (Bearer).
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "oauth-abc");
        }
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "oauth-abc");

        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }

    #[test]
    fn credential_debug_is_redacted() {
        let c = Credential { header: CredHeader::Bearer, value: "super-secret".into() };
        let shown = format!("{c:?}");
        assert!(!shown.contains("super-secret"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn parse_head_strips_auth_and_host_keeps_others() {
        let head = b"POST /v1/messages HTTP/1.1\r\nHost: 10.0.2.2:9\r\nAuthorization: Bearer dummy\r\nx-api-key: dummy\r\nanthropic-version: 2023-06-01\r\nContent-Length: 12\r\nContent-Type: application/json\r\n\r\n";
        let req = parse_request_head(head).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/messages");
        assert_eq!(req.content_length, 12);
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"anthropic-version"));
        assert!(names.contains(&"Content-Type"));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("host")));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("authorization")));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("x-api-key")));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("content-length")));
    }

    #[test]
    fn parse_head_rejects_junk() {
        assert!(parse_request_head(b"garbage\r\n\r\n").is_none());
        assert!(parse_request_head(b"GET /\r\n\r\n").is_none()); // no HTTP version
    }

    #[test]
    fn client_authorized_matches_dummy_only() {
        let head = b"POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer the-dummy\r\n\r\n";
        assert!(client_authorized(head, "the-dummy"));
        assert!(!client_authorized(head, "other"));
        let xk = b"POST /v1 HTTP/1.1\r\nx-api-key: the-dummy\r\n\r\n";
        assert!(client_authorized(xk, "the-dummy"));
        let none = b"POST /v1 HTTP/1.1\r\nContent-Type: x\r\n\r\n";
        assert!(!client_authorized(none, "the-dummy"));
        assert!(!client_authorized(head, "")); // empty token never authorizes
    }

    #[test]
    fn a_grant_with_no_host_side_credential_is_refused() {
        let grant = crate::sandbox_policy::AuthGrant {
            host: "api.example.com".into(),
            credential_env: "H5I_TEST_ABSENT_CREDENTIAL".into(),
            base_url_var: "EXAMPLE_BASE_URL".into(),
            token_var: "EXAMPLE_TOKEN".into(),
        };
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_ABSENT_CREDENTIAL");
        }
        let err = match engage_grant(&grant, true) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a grant with no credential must be refused"),
        };
        // Fail-closed, and the message says which variable and which host, so
        // the fix is obvious rather than a hunt.
        assert!(err.contains("api.example.com"), "{err}");
        assert!(err.contains("H5I_TEST_ABSENT_CREDENTIAL"), "{err}");
        assert!(err.contains("unauthenticated"), "{err}");
    }

    #[test]
    fn a_grant_is_inert_on_a_tier_that_cannot_reach_the_proxy() {
        let grant = crate::sandbox_policy::AuthGrant {
            host: "api.example.com".into(),
            credential_env: "H5I_TEST_ABSENT_CREDENTIAL".into(),
            base_url_var: "EXAMPLE_BASE_URL".into(),
            token_var: "EXAMPLE_TOKEN".into(),
        };
        // No proxy path from the box → no engagement, and notably no error
        // about the missing credential: there was nothing to authenticate.
        assert!(matches!(engage_grant(&grant, false), Ok(None)));
    }

    #[test]
    fn box_env_points_at_proxy_and_excludes_gateway_proxy() {
        let env = box_env(AgentRuntime::Claude, 4321, "h5i-proxy-abc");
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("ANTHROPIC_BASE_URL"), Some("http://10.0.2.2:4321"));
        assert_eq!(get("ANTHROPIC_AUTH_TOKEN"), Some("h5i-proxy-abc"));
        assert!(get("NO_PROXY").unwrap().contains("10.0.2.2"));
        let env = box_env(AgentRuntime::Codex, 22, "t");
        assert!(env.iter().any(|(n, _)| n == "OPENAI_BASE_URL"));
        assert!(env.iter().any(|(n, _)| n == "OPENAI_API_KEY"));
    }

    /// End-to-end over loopback with a fake plain-HTTP "upstream": the proxy must
    /// (a) reject a request lacking the dummy, (b) strip the dummy and inject the
    /// real credential, and (c) stream the response body back verbatim.
    #[test]
    fn forwards_and_injects_real_credential() {
        use std::sync::mpsc;

        // Fake upstream: records the auth header it received, streams a 2-line body.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            // Only the authorized request reaches upstream (the 403 is rejected
            // at the proxy before any upstream connection).
            for stream in upstream.incoming().take(1) {
                let mut s = stream.unwrap();
                let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
                let mut seen_auth = String::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if low.starts_with("authorization:") || low.starts_with("x-api-key:") {
                        seen_auth = line.trim().to_string();
                    }
                }
                let _ = tx.send(seen_auth);
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 12\r\n\r\ndata: hello\n").unwrap();
            }
        });

        let cred = Credential { header: CredHeader::Bearer, value: "REAL-TOKEN".into() };
        let handle = spawn_to_upstream(
            format!("http://127.0.0.1:{up_port}"),
            "127.0.0.1".into(),
            cred,
            "the-dummy".into(),
            false,
        )
        .unwrap();

        // (a) A request without the dummy is rejected with 403 and never reaches upstream.
        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        c.write_all(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");

        // (b)+(c) A request WITH the dummy is forwarded with the real token injected.
        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        c.write_all(b"POST /v1/messages HTTP/1.1\r\nHost: 10.0.2.2\r\nAuthorization: Bearer the-dummy\r\nContent-Length: 5\r\n\r\nhello").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();

        // Header names are case-insensitive on the wire (reqwest lowercases them).
        let seen = rx.recv_timeout(Duration::from_secs(5)).unwrap().to_ascii_lowercase();
        assert_eq!(seen, "authorization: bearer real-token", "real credential must be injected");
        assert!(resp.contains("data: hello"), "body must stream back: {resp}");
        assert!(!resp.contains("the-dummy"), "dummy must not leak downstream");
    }

    /// A request whose body arrives after its head must still be forwarded.
    ///
    /// The accept loop polls a non-blocking listener, and on macOS (BSD
    /// `accept`; Linux's `accept4` differs) the accepted socket inherits that
    /// flag — so `read_head`/`read_exact` returned `WouldBlock` the moment a
    /// request spanned more than one read, and a well-formed call was answered
    /// `400`. Every real prompt is larger than one segment, and this proxy is
    /// the *primary* egress path on the macOS supervised tier.
    ///
    /// Driven through `handle_client` with a deliberately non-blocking socket,
    /// so the Darwin condition is reproduced on every platform rather than
    /// tested only where the OS happens to create it.
    #[test]
    fn a_request_split_across_reads_is_forwarded_not_rejected() {
        use std::sync::mpsc;

        // Fake upstream: reports the body it received.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let (mut s, _) = upstream.accept().unwrap();
            let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
            let mut len = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let _ = tx.send(String::from_utf8_lossy(&body).to_string());
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });

        let state = Arc::new(ProxyState {
            upstream: reqwest::Url::parse(&format!("http://127.0.0.1:{up_port}")).unwrap(),
            upstream_host: "127.0.0.1".into(),
            credential: Credential {
                header: CredHeader::Bearer,
                value: "REAL-TOKEN".into(),
            },
            client_token: "the-dummy".into(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
            in_flight: std::sync::atomic::AtomicUsize::new(0),
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_port = front.local_addr().unwrap().port();
        let served = std::thread::spawn(move || {
            let (s, _) = front.accept().unwrap();
            // What Darwin hands the accept loop. The handler must correct it.
            s.set_nonblocking(true).unwrap();
            handle_client(s, &state)
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // Head first, body only after a pause: the handler is blocked in a read
        // with nothing buffered, which is where the inherited flag bit.
        c.write_all(
            b"POST /v1/messages HTTP/1.1\r\nHost: 10.0.2.2\r\nAuthorization: Bearer the-dummy\r\nContent-Length: 11\r\n\r\n",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        c.write_all(b"hello-split").unwrap();

        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 200"),
            "a split request must be forwarded, got: {resp}"
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "hello-split",
            "the whole body must reach upstream"
        );
        served.join().unwrap().unwrap();
    }

    /// A chunked request must be refused, not silently re-originated bodyless
    /// with the real credential attached. Only `Content-Length` framing is read,
    /// and `transfer-encoding` is stripped on the way out, so forwarding one
    /// sent upstream a request the client never made — authenticated with the
    /// genuine token.
    #[test]
    fn a_chunked_request_is_refused_and_never_reaches_upstream() {
        // An upstream that fails the test if it is ever contacted.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        upstream.set_nonblocking(true).unwrap();

        let handle = spawn_to_upstream(
            format!("http://127.0.0.1:{up_port}"),
            "127.0.0.1".into(),
            Credential { header: CredHeader::Bearer, value: "REAL-TOKEN".into() },
            "the-dummy".into(),
            false,
        )
        .unwrap();

        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(
            b"POST /v1/messages HTTP/1.1\r\nHost: 10.0.2.2\r\nAuthorization: Bearer the-dummy\r\n\
              Transfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 411"), "chunked must be refused, got: {resp}");
        assert!(
            upstream.accept().is_err(),
            "a refused request must never open an upstream connection"
        );
    }

    /// The over-capacity `503` is written on the accept thread, so its drain
    /// must never wait on the peer. A box that holds the [`MAX_IN_FLIGHT`] slots
    /// and then connects without ever closing would otherwise park the accept
    /// loop for the whole drain window on every excess connection — and that
    /// loop is the only way the proxy picks up the connections that free those
    /// slots, so the refusal would cost the box the authenticated egress the
    /// bound exists to protect.
    #[test]
    fn the_accept_loop_refusal_does_not_wait_on_the_peer() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Deliberately kept open for the whole test: the peer sends a request
        // and then goes quiet without closing, which is the case that stalls a
        // waiting drain.
        let mut peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (mut server, _) = l.accept().unwrap();
        peer.write_all(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        // Wait for the request to actually land before timing the drain: a
        // buffered drain takes what is in the receive buffer, and asserting on
        // an empty one would pass for the wrong reason and race the close below.
        server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        server.peek(&mut [0u8; 1]).unwrap();

        let t0 = std::time::Instant::now();
        write_status_draining(&mut server, 503, "Service Unavailable", Drain::Buffered);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(250),
            "the accept-loop drain must not wait on a peer that stays open (took {elapsed:?})"
        );

        // And it is still a drain: what the peer already sent is consumed, so
        // the close is clean and the status arrives instead of being discarded
        // by an RST.
        drop(server);
        let mut resp = String::new();
        peer.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 503"), "the refusal must reach the peer, got: {resp}");
    }

    #[test]
    fn parse_head_flags_chunked_framing() {
        let head = b"POST /v1 HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(parse_request_head(head).unwrap().chunked);
        // `identity` is the no-op encoding: Content-Length framing still applies.
        let head = b"POST /v1 HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: identity\r\nContent-Length: 2\r\n\r\n";
        let req = parse_request_head(head).unwrap();
        assert!(!req.chunked);
        assert_eq!(req.content_length, 2);
        let head = b"POST /v1 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n";
        assert!(!parse_request_head(head).unwrap().chunked);
    }

    /// The refusal message must name the variable [`opted_out`] actually reads.
    /// It named `H5I_NO_AUTH_PROXY`, which nothing in the tree consults — so the
    /// one escape hatch offered by a fail-closed error did nothing.
    #[test]
    fn the_opt_out_named_in_the_refusal_is_the_one_that_works() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_CREDENTIAL_PROXY", "off");
        }
        assert!(opted_out(), "the documented spelling must opt out");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_CREDENTIAL_PROXY");
        }
        assert!(!opted_out());
    }

    #[test]
    fn runtime_proxy_maps_each_runtime() {
        let c = runtime_proxy(AgentRuntime::Claude);
        assert_eq!(c.upstream_host, "api.anthropic.com");
        assert_eq!(c.base_url_var, "ANTHROPIC_BASE_URL");
        assert_eq!(c.dummy_var, "ANTHROPIC_AUTH_TOKEN");
        let o = runtime_proxy(AgentRuntime::Codex);
        assert_eq!(o.upstream_host, "api.openai.com");
        assert_eq!(o.base_url_var, "OPENAI_BASE_URL");
        assert_eq!(o.dummy_var, "OPENAI_API_KEY");
    }

    #[test]
    fn resolve_credential_codex_bearer() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert!(resolve_credential(AgentRuntime::Codex).is_none());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-openai");
        }
        let c = resolve_credential(AgentRuntime::Codex).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "sk-openai");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    #[test]
    fn nonempty_env_rejects_blank() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_BLANK_VAR", "   ");
        }
        assert!(nonempty_env("H5I_TEST_BLANK_VAR").is_none());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_BLANK_VAR", "v");
        }
        assert_eq!(nonempty_env("H5I_TEST_BLANK_VAR").as_deref(), Some("v"));
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_BLANK_VAR");
        }
    }

    #[test]
    fn parse_head_get_without_length_and_preserves_query() {
        let head = b"GET /v1/models?limit=5 HTTP/1.1\r\nHost: x\r\nAccept: application/json\r\n\r\n";
        let req = parse_request_head(head).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/models?limit=5", "query string must be preserved");
        assert_eq!(req.content_length, 0);
    }

    #[test]
    fn parse_head_rejects_bad_content_length() {
        let head = b"POST /v1 HTTP/1.1\r\nContent-Length: notanumber\r\n\r\n";
        assert!(parse_request_head(head).is_none());
    }

    #[test]
    fn client_authorized_accepts_lowercase_bearer() {
        let head = b"POST /v1 HTTP/1.1\r\nauthorization: bearer the-dummy\r\n\r\n";
        assert!(client_authorized(head, "the-dummy"));
    }

    #[test]
    fn request_and_response_header_stripping() {
        for h in ["Host", "AUTHORIZATION", "x-api-key", "content-length", "Connection", "TE", "upgrade"] {
            assert!(is_stripped_request_header(h), "request header {h} must be stripped");
        }
        for h in ["anthropic-version", "content-type", "accept"] {
            assert!(!is_stripped_request_header(h), "request header {h} must survive");
        }
        for h in ["Content-Length", "transfer-encoding", "Connection", "keep-alive"] {
            assert!(is_stripped_response_header(h), "response header {h} must be stripped");
        }
        assert!(!is_stripped_response_header("content-type"));
    }

    #[test]
    fn client_token_is_unguessable_and_unique() {
        let a = new_client_token().unwrap();
        let b = new_client_token().unwrap();
        assert_ne!(a, b, "each run gets a distinct token");
        assert!(a.starts_with("h5i-proxy-"));
        // 32 hex chars of entropy after the prefix.
        assert_eq!(a.len(), "h5i-proxy-".len() + 32);
    }

    /// SSRF + injection + response-header hardening: the proxy must ignore the
    /// client's `Host`, force the pinned upstream host, preserve the path (query
    /// included), inject an **x-api-key** credential, and strip upstream framing
    /// headers on the way back.
    #[test]
    fn forces_upstream_host_injects_api_key_and_strips_framing() {
        use std::sync::mpsc;

        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for stream in upstream.incoming().take(1) {
                let mut s = stream.unwrap();
                let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
                let mut head = String::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let _ = tx.send(head);
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n{}").unwrap();
            }
        });

        let cred = Credential { header: CredHeader::XApiKey, value: "REAL-KEY".into() };
        let handle = spawn_to_upstream(
            format!("http://127.0.0.1:{up_port}"),
            "pinned.example".into(), // the forced Host, distinct from the connect target
            cred,
            "dummy-tok".into(),
            false,
        )
        .unwrap();

        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        // Client lies about Host (an SSRF attempt) and authenticates with the dummy.
        c.write_all(b"POST /v1/messages?beta=true HTTP/1.1\r\nHost: evil.attacker\r\nx-api-key: dummy-tok\r\nContent-Length: 0\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();

        let up_head = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let low = up_head.to_ascii_lowercase();
        assert!(low.contains("x-api-key: real-key"), "api key injected: {up_head}");
        assert!(low.contains("host: pinned.example"), "forced upstream host, not client's: {up_head}");
        assert!(!low.contains("evil.attacker"), "client Host must not reach upstream");
        assert!(up_head.contains("/v1/messages?beta=true"), "path+query preserved: {up_head}");

        // Downstream: upstream framing headers stripped, connection-close applied.
        let head_lower = resp.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
        assert!(head_lower.contains("content-type: application/json"), "safe header kept: {resp}");
        assert!(!head_lower.contains("content-length"), "upstream content-length stripped: {resp}");
        assert!(!head_lower.contains("transfer-encoding"), "upstream transfer-encoding stripped: {resp}");
        assert!(head_lower.contains("connection: close"));
    }

    /// The SSRF gate, as a table. A URL's authority ends at the first `/`, so a
    /// target that does not start with one can rewrite the host it is appended
    /// to — `@evil/…` most sharply, via userinfo.
    #[test]
    fn only_origin_form_targets_are_accepted() {
        for good in ["/", "/v1/messages", "/v1/models?limit=5", "/a/b?q=x&y=z#frag"] {
            assert!(is_origin_form(good), "must accept {good}");
        }
        for bad in [
            "@evil.example/v1/messages",   // userinfo: the host becomes evil.example
            "@evil.example",               //
            ":9999@evil.example/v1",       // userinfo with a password field
            "//evil.example/v1",           // protocol-relative
            "https://evil.example/v1",     // absolute-form
            "v1/messages",                 // bare relative
            "",                            //
            "/v1 /messages",               // whitespace: request smuggling
            "/v1\r\nX-Injected: 1",        // CRLF injection
            "/v1\\..\\evil",               // backslash
        ] {
            assert!(!is_origin_form(bad), "must reject {bad:?}");
        }
    }

    /// The regression that matters most in this file: a box that presents the
    /// per-run token must not be able to aim the *real* credential at a host of
    /// its choosing. The rogue upstream here stands in for an attacker-controlled
    /// domain (which, having an ordinary valid certificate for its own name,
    /// would satisfy TLS verification — the pinned name is not what gets
    /// checked once the URL's host has moved).
    #[test]
    fn a_userinfo_target_cannot_redirect_the_credential() {
        use std::sync::mpsc;

        let rogue = TcpListener::bind("127.0.0.1:0").unwrap();
        let rogue_port = rogue.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            // Any connection at all is a failure: the proxy must refuse before
            // it opens one.
            for _stream in rogue.incoming().take(1) {
                let _ = tx.send(());
            }
        });

        let cred = Credential { header: CredHeader::Bearer, value: "REAL-TOKEN-SECRET".into() };
        let handle = spawn_to_upstream(
            "https://api.anthropic.com".into(),
            "api.anthropic.com".into(),
            cred,
            "dummy-tok".into(),
            false,
        )
        .unwrap();

        for target in [
            format!("@127.0.0.1:{rogue_port}/v1/messages"),
            format!("@localhost:{rogue_port}/v1/messages"),
            format!("https://127.0.0.1:{rogue_port}/v1/messages"),
        ] {
            let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
            let req = format!(
                "POST {target} HTTP/1.1\r\nHost: 10.0.2.2\r\nAuthorization: Bearer dummy-tok\r\n\
                 Content-Length: 0\r\n\r\n"
            );
            c.write_all(req.as_bytes()).unwrap();
            let mut resp = String::new();
            let _ = c.read_to_string(&mut resp);
            assert!(
                resp.starts_with("HTTP/1.1 403"),
                "target {target:?} must be refused, got: {resp}"
            );
        }

        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "the proxy connected to an attacker-chosen host — the credential is exfiltratable"
        );
    }

    /// `Content-Length` is written by the box. Sizing a buffer from it lets one
    /// request exhaust host memory, so an oversized declaration is refused
    /// before anything is allocated.
    #[test]
    fn an_oversized_content_length_is_refused_not_allocated() {
        let cred = Credential { header: CredHeader::Bearer, value: "R".into() };
        let handle = spawn_to_upstream(
            "http://127.0.0.1:1".into(), // never contacted
            "pinned.example".into(),
            cred,
            "dummy-tok".into(),
            false,
        )
        .unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        c.write_all(
            b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer dummy-tok\r\n\
              Content-Length: 999999999999\r\n\r\n",
        )
        .unwrap();
        let mut resp = String::new();
        let _ = c.read_to_string(&mut resp);
        assert!(resp.starts_with("HTTP/1.1 413"), "{resp}");
    }

    #[test]
    fn malformed_request_after_auth_is_rejected() {
        let cred = Credential { header: CredHeader::Bearer, value: "R".into() };
        let handle = spawn_to_upstream(
            "http://127.0.0.1:1".into(), // never contacted — request is rejected first
            "pinned.example".into(),
            cred,
            "dummy-tok".into(),
            false,
        )
        .unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", handle.port)).unwrap();
        // Authorized (presents the dummy) but the request line has no HTTP version.
        c.write_all(b"POST /v1\r\nAuthorization: Bearer dummy-tok\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
    }
}
