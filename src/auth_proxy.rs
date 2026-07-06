//! Host-side **credential-injecting** egress proxy ("option 2", see
//! `docs/credential-proxy-design.md`). The keystone that lets an agent box
//! authenticate to its provider API **without the long-lived token ever entering
//! the box**.
//!
//! The [`crate::container`] egress proxy tunnels TLS (`CONNECT`) and so can never
//! inject an `Authorization`/`x-api-key` header — the bytes are end-to-end
//! encrypted. This proxy instead *terminates* the box→proxy hop in cleartext on
//! host loopback and re-originates a fresh TLS request upstream, injecting the
//! real credential host-side. The agent is pointed at it with a base-URL override
//! (`ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL`) and a **dummy** token; the genuine
//! token lives only in this process's memory.
//!
//! Security properties (all fail-closed):
//! - **Token never in the box.** The box sees only the base URL + a per-run dummy.
//!   The real credential is resolved from *h5i's own* host environment and handed
//!   straight to the upstream request — never an env var, mount, or argv in the box.
//! - **No SSRF.** The upstream host is pinned to the runtime's single API host
//!   ([`RuntimeProxy::upstream_host`]); a request's own `Host`/authority is ignored
//!   and only its path is reused, so a prompt-injected box cannot aim the real
//!   token at an attacker host.
//! - **DNS-rebinding resistant.** The upstream host is resolved+pinned once at
//!   spawn (mirrors the egress proxy's `pin_dns`).
//! - **Loopback + shared-secret gated.** The listener binds `127.0.0.1` (reachable
//!   from the box via slirp `allow_host_loopback` at `10.0.2.2`). Because loopback
//!   is also reachable by *other* host processes, the proxy injects the real
//!   credential only for a request that presents the per-run dummy token — an
//!   unguessable secret other host users don't hold. (The box is *allowed* to use
//!   the proxy; other host users are not.)
//! - **Never logs the token or bodies.** [`Credential`]'s `Debug` is redacted.
//!
//! The live forwarder uses `reqwest` (blocking + rustls) so TLS, chunked decoding,
//! and streaming (SSE) are handled by a vetted client rather than hand-rolled.

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
fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
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

/// Immutable per-proxy state shared with worker threads.
struct ProxyState {
    /// `https://api.anthropic.com` in production; an `http://127.0.0.1:<port>`
    /// origin in tests. No trailing slash.
    upstream_base: String,
    /// The single upstream host (for the forced `Host` header / SSRF pin).
    upstream_host: String,
    /// The genuine credential injected into every forwarded request.
    credential: Credential,
    /// The per-run dummy token the box must present (shared-secret gate).
    client_token: String,
    /// Blocking HTTP client (TLS via rustls, DNS pinned, no proxy, no redirects).
    client: reqwest::blocking::Client,
}

/// Spawn the proxy for a runtime with a resolved credential. `client_token` is
/// the unguessable per-run dummy the box will present. Production entry point:
/// forwards to the runtime's HTTPS API host, DNS-pinned.
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
    if pin_dns {
        if let Ok(mut addrs) = (upstream_host.as_str(), 443u16).to_socket_addrs() {
            if let Some(addr) = addrs.next() {
                builder = builder.resolve(&upstream_host, addr);
            }
        }
    }
    let client = builder
        .build()
        .map_err(|e| H5iError::Metadata(format!("auth proxy: build HTTP client: {e}")))?;

    let listener = TcpListener::bind("127.0.0.1:0").map_err(H5iError::Io)?;
    let port = listener.local_addr().map_err(H5iError::Io)?.port();
    listener.set_nonblocking(true).map_err(H5iError::Io)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let state = Arc::new(ProxyState {
        upstream_base,
        upstream_host,
        credential,
        client_token,
        client,
    });

    let join = std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((client, _)) => {
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    let state = state.clone();
                    std::thread::spawn(move || {
                        let _ = handle_client(client, &state);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    Ok(AuthProxyHandle { port, stop, join: Some(join) })
}

/// Box env additions that point the agent at the proxy: the base-URL override,
/// the per-run dummy token, and a `NO_PROXY` that excludes the slirp gateway so
/// the base URL is dialed directly (not re-wrapped through the egress CONNECT
/// proxy). All non-secret — safe to pass by value.
pub fn box_env(rt: AgentRuntime, port: u16, client_token: &str) -> Vec<(String, String)> {
    let rp = runtime_proxy(rt);
    vec![
        (rp.base_url_var.to_string(), format!("http://10.0.2.2:{port}")),
        (rp.dummy_var.to_string(), client_token.to_string()),
        ("NO_PROXY".to_string(), "localhost,127.0.0.1,10.0.2.2".to_string()),
        ("no_proxy".to_string(), "localhost,127.0.0.1,10.0.2.2".to_string()),
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
        if !is_stripped_request_header(name) {
            headers.push((name.to_string(), value.to_string()));
        }
    }
    Some(ParsedReq { method, path, headers, content_length })
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
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
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

fn write_status(client: &mut TcpStream, code: u16, reason: &str) {
    let _ = client.write_all(format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").as_bytes());
}

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

    // Read the request body (Content-Length framed; these SDKs always send one).
    let mut body = vec![0u8; req.content_length];
    if req.content_length > 0 && client.read_exact(&mut body).is_err() {
        write_status(&mut client, 400, "Bad Request");
        return Ok(());
    }

    // Build the upstream request against the PINNED host (path reused, authority
    // ignored → no SSRF), injecting the genuine credential.
    let url = format!("{}{}", state.upstream_base, req.path);
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            write_status(&mut client, 400, "Bad Request");
            return Ok(());
        }
    };
    let mut builder = state.client.request(method, &url).header("host", &state.upstream_host);
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

/// A short, unguessable per-run token the box presents to the proxy. Not a
/// credential — it only distinguishes the box from other host processes on
/// loopback, so plain PRNG entropy is sufficient.
pub fn new_client_token() -> String {
    format!("h5i-proxy-{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..))
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

    #[test]
    fn resolve_credential_precedence_claude() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for v in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
            std::env::remove_var(v);
        }
        assert!(resolve_credential(AgentRuntime::Claude).is_none());

        // API key → x-api-key.
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-xyz");
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::XApiKey);
        assert_eq!(c.value, "sk-ant-xyz");

        // Bearer token wins over the API key.
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "brr");
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "brr");

        // With neither, the long-lived OAuth token is the last resort (Bearer).
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "oauth-abc");
        let c = resolve_credential(AgentRuntime::Claude).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "oauth-abc");

        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
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
        std::env::remove_var("OPENAI_API_KEY");
        assert!(resolve_credential(AgentRuntime::Codex).is_none());
        std::env::set_var("OPENAI_API_KEY", "sk-openai");
        let c = resolve_credential(AgentRuntime::Codex).unwrap();
        assert_eq!(c.header, CredHeader::Bearer);
        assert_eq!(c.value, "sk-openai");
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn nonempty_env_rejects_blank() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("H5I_TEST_BLANK_VAR", "   ");
        assert!(nonempty_env("H5I_TEST_BLANK_VAR").is_none());
        std::env::set_var("H5I_TEST_BLANK_VAR", "v");
        assert_eq!(nonempty_env("H5I_TEST_BLANK_VAR").as_deref(), Some("v"));
        std::env::remove_var("H5I_TEST_BLANK_VAR");
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
        let a = new_client_token();
        let b = new_client_token();
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
