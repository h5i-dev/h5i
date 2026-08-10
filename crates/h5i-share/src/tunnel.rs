//! Transport two: a Cloudflare quick tunnel, for a visitor who has no h5i.
//!
//! The peer-to-peer transport needs h5i on both ends, because a browser cannot
//! speak QUIC to an endpoint id. That rules out the person you most often want
//! clicking a prototype — a designer, a product manager, a customer — and no
//! amount of work on the P2P path reaches them. So this one trades the
//! property P2P has for the one it does not: anybody with the link can open it
//! in any browser.
//!
//! **What it costs, stated here and in the receipt rather than only in the
//! docs:**
//!
//! * **TLS terminates at Cloudflare.** This path is not end to end. Cloudflare
//!   can read the traffic between the visitor and this machine. For an
//!   agent-built prototype that is usually an acceptable trade and it is never
//!   ours to assume — [`crate::bridge::render_receipt`] writes it into the
//!   export so the decision is visible later.
//! * **`cloudflared` is somebody else's binary.** We neither ship it nor pin
//!   it. If it is not installed, the failure says so and names the alternative.
//! * **Quick tunnels are explicitly not a production service.** Cloudflare caps
//!   concurrency and does not support server-sent events on them. This is the
//!   no-install mode, not the default mode.
//!
//! What does **not** change is the bridge underneath. The URL carries a token,
//! the token is checked against the same grant table on every connection, live
//! connections are dropped when a grant is revoked, and the credential is
//! stripped before anything reaches the box. The capability degrades from "hold
//! the secret" to "hold the link"; it does not degrade to nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use h5i_error::H5iError;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::bridge::{Bridge, Path};
use crate::http_front::{self, Next};

/// How long to wait for `cloudflared` to publish a URL before giving up.
const URL_TIMEOUT: Duration = Duration::from_secs(45);
/// How often a connection asks whether its grant still admits anyone.
const REVOKE_POLL: Duration = Duration::from_secs(1);
/// How long to pause after an `accept` error before trying again.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A running `cloudflared`, killed when this is dropped.
#[derive(Debug)]
pub struct Tunnel {
    child: tokio::process::Child,
    pub url: String,
}

impl Tunnel {
    /// The origin a visitor opens, with no token in it.
    pub fn origin(&self) -> &str {
        &self.url
    }

    /// Stop the tunnel. Called on the way out so a quick tunnel does not
    /// outlive the share that created it.
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Pull a quick-tunnel URL out of a line of `cloudflared` logging.
///
/// Strict about what it will accept: the host must be a `trycloudflare.com`
/// subdomain made of the characters a hostname is allowed to have. This is a
/// URL we are about to print and tell someone to open, and `cloudflared`'s
/// output is a log format rather than an interface — a looser match would let a
/// change in its banner, or anything that got into its logs, choose what we
/// hand a person.
pub fn extract_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '<')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');
    let host = url.strip_prefix("https://")?;
    if !host.ends_with(".trycloudflare.com") {
        return None;
    }
    let label = host.strip_suffix(".trycloudflare.com")?;
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    Some(url.to_string())
}

/// Start `cloudflared` pointed at a loopback port, and wait for its URL.
///
/// The argv is built here, never through a shell: the only value that varies is
/// a port number this process chose.
pub async fn start(local_port: u16) -> Result<Tunnel, H5iError> {
    let mut child = tokio::process::Command::new("cloudflared")
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{local_port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                H5iError::Metadata(
                    "`cloudflared` is not installed, and `--tunnel` is a wrapper around it. \
                     Install it (https://developers.cloudflare.com/cloudflare-one/connections/\
                     connect-networks/downloads/), or share peer-to-peer with `h5i box share` \
                     and have the other side run `h5i join`."
                        .into(),
                )
            } else {
                H5iError::Metadata(format!("could not start `cloudflared`: {e}"))
            }
        })?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| H5iError::Metadata("cloudflared produced no output to read".into()))?;
    let mut lines = BufReader::new(stderr).lines();

    let found = tokio::time::timeout(URL_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = extract_url(&line) {
                return Some(url);
            }
        }
        None
    })
    .await;

    match found {
        Ok(Some(url)) => {
            // Keep reading. Dropping the pipe here closes its read end, and the
            // next time `cloudflared` fills the kernel buffer and writes it
            // takes an EPIPE — which Go turns into a fatal signal on fd 2. A
            // tunnel that dies mid-share for that reason would report nothing
            // at all, so the lines are consumed and discarded for as long as it
            // runs.
            tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
            Ok(Tunnel { child, url })
        }
        Ok(None) => {
            let _ = child.kill().await;
            Err(H5iError::Metadata(
                "`cloudflared` exited without publishing a URL. Run it once by hand to see what \
                 it says: `cloudflared tunnel --url http://127.0.0.1:3000`."
                    .into(),
            ))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(H5iError::Metadata(format!(
                "`cloudflared` did not publish a URL within {}s. Quick tunnels need outbound \
                 network access to Cloudflare.",
                URL_TIMEOUT.as_secs()
            )))
        }
    }
}

/// What a visitor gets when the share is at capacity. Deliberately not a `401`:
/// their link is fine, and telling them otherwise sends them back to ask for a
/// new one that would work no better.
const BUSY_BODY: &str = "This share is busy right now. Wait a moment and reload the page.";

/// Built rather than written out, because a hand-counted `Content-Length` is a
/// truncated page waiting for somebody to edit the sentence.
fn busy_response() -> String {
    format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Retry-After: 2\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{BUSY_BODY}",
        BUSY_BODY.len()
    )
}

/// The visitor-facing link: the origin plus the token that authorizes one grant.
pub fn invite_url(origin: &str, secret: &str) -> String {
    format!("{origin}/?{}={secret}", crate::gate::QUERY_PARAM)
}

/// Serve the tunnel's loopback side until the process stops.
///
/// The listener is bound on `127.0.0.1` and nothing else, on every path: what
/// reaches the internet is `cloudflared`'s outbound connection, not a port on
/// this machine.
pub async fn serve(bridge: Arc<Bridge>, listener: tokio::net::TcpListener) -> Result<(), H5iError> {
    // One entry per grant, because a tunnel genuinely cannot tell two browsers
    // apart — the peers it sees are Cloudflare's. Counting per grant is the
    // finest honest granularity, and the receipt says so rather than implying
    // a precision the transport does not have.
    let peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>> = Default::default();

    loop {
        let sock = match listener.accept().await {
            Ok((sock, _)) => sock,
            // Not `continue`: tokio only clears a listener's readiness on
            // `WouldBlock`, so a persistent error — `EMFILE` is the one that
            // actually happens — returns immediately every time and turns this
            // into a busy loop that never recovers. Pausing gives descriptors a
            // chance to come back and keeps the share responsive if they do.
            Err(e) => {
                eprintln!("share: could not accept a connection: {e}");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let bridge = bridge.clone();
        let peers = peers.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(bridge, peers, sock).await {
                eprintln!("share: {e}");
            }
        });
    }
}

/// Resolves when this connection's own grant stops admitting anyone.
///
/// Per grant rather than per share: revoking one peer has to cut that peer off
/// even while somebody else's ticket is still good, and a check on "is the
/// whole share spent" would not.
async fn revoked(bridge: Arc<Bridge>, grant_id: String) {
    loop {
        tokio::time::sleep(REVOKE_POLL).await;
        if !bridge.grant_is_live(&grant_id) {
            return;
        }
    }
}

async fn handle(
    bridge: Arc<Bridge>,
    peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>>,
    mut sock: tokio::net::TcpStream,
) -> Result<(), H5iError> {
    let Some((head, rest)) = http_front::read_head(&mut sock).await else {
        return Ok(());
    };

    // Resolved once, here, so the decision and the accounting agree about which
    // grant let this connection in.
    let mut grant = None;
    let next = http_front::decide(
        &head,
        // The bare name: a quick tunnel's host is its own site (trycloudflare
        // is on the Public Suffix List), so there is no other origin to
        // collide with.
        crate::gate::COOKIE,
        |token| match bridge.authorize(token) {
            Ok(g) => {
                grant = Some(g);
                true
            }
            Err(_) => false,
        },
        // The visitor's origin is https, because Cloudflare terminates it.
        true,
    );

    let (head, req) = match next {
        Next::Respond(body) => {
            http_front::respond(&mut sock, &body).await;
            return Ok(());
        }
        Next::Proxy { head, req } => (head, req),
    };
    let Some(grant) = grant else {
        return Ok(());
    };

    // Authorized, but the share may already be carrying all it will. A refusal
    // here is a `503` with a `Retry-After`, because unlike a bad token this one
    // is worth trying again.
    let Some(_slot) = bridge.admit() else {
        http_front::respond(&mut sock, &busy_response()).await;
        return Ok(());
    };

    // On a blocking pool because it is blocking: the dialer waits for its
    // helper to hand back a connected socket. A runtime worker parked on that
    // syscall is a worker not serving the other requests of the same page.
    let upstream = {
        let bridge = bridge.clone();
        tokio::task::spawn_blocking(move || bridge.open_upstream())
            .await
            .map_err(|e| H5iError::Metadata(format!("the box dialer panicked: {e}")))??
    };
    upstream.set_nonblocking(true)?;
    let upstream = tokio::net::TcpStream::from_std(upstream)?;

    let id = {
        let mut map = peers.lock().expect("peer map");
        *map.entry(grant.id.clone()).or_insert_with(|| {
            bridge.peer_joined(
                "a browser (the tunnel cannot tell two apart)".into(),
                &grant,
                Path::Tunnel,
            )
        })
    };
    bridge.peer_connection(id);

    let (up_r, up_w) = upstream.into_split();
    let counts = http_front::Counters::default();
    let forwarded = http_front::Forwarded {
        head: &head,
        rest: &rest,
        req: &req,
    };
    tokio::select! {
        _ = http_front::proxy_one(sock, up_r, up_w, forwarded, &counts) => {}
        _ = revoked(bridge.clone(), grant.id.clone()) => {}
    }
    // Outside the select, so a connection cut short by a revoke still reports
    // what it had already moved.
    let (to_box, to_peer) = counts.read();
    bridge.peer_bytes(id, to_peer, to_box);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_read_out_of_cloudflareds_banner() {
        let line = "2026-08-10T10:00:00Z INF |  https://odd-cat-1234.trycloudflare.com  |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://odd-cat-1234.trycloudflare.com")
        );
    }

    #[test]
    fn only_a_quick_tunnel_host_is_accepted_as_one() {
        // `cloudflared`'s log format is not an interface, and its output is not
        // all ours. A looser match would let a banner change — or anything that
        // got into its logs — choose the URL we hand a person to open.
        for line in [
            "INF Visit https://developers.cloudflare.com/argo-tunnel for docs",
            "INF see https://example.test/",
            "INF https://evil.test/?x=https://a.trycloudflare.com",
            "INF https://.trycloudflare.com",
            "INF https://a b.trycloudflare.com",
            "nothing here at all",
        ] {
            assert_eq!(extract_url(line), None, "accepted a URL from: {line}");
        }
    }

    #[test]
    fn a_trailing_slash_or_quote_does_not_end_up_in_the_url() {
        assert_eq!(
            extract_url(r#"INF url="https://a-b-c.trycloudflare.com/""#).as_deref(),
            Some("https://a-b-c.trycloudflare.com")
        );
    }

    #[test]
    fn the_invite_link_carries_the_token_and_nothing_else() {
        let url = invite_url("https://odd-cat.trycloudflare.com", "abc123");
        assert_eq!(url, "https://odd-cat.trycloudflare.com/?h5i=abc123");
    }

    #[tokio::test]
    async fn a_missing_cloudflared_says_what_to_install_and_what_else_to_try() {
        // Only meaningful when cloudflared really is absent; where it is
        // installed this asserts nothing, which is the right trade for not
        // making the suite depend on a third-party binary.
        if which_cloudflared() {
            return;
        }
        let err = start(3000).await.expect_err("no cloudflared");
        let msg = format!("{err}");
        assert!(msg.contains("cloudflared"), "{msg}");
        assert!(msg.contains("h5i join"), "{msg}");
    }

    // ─── the loopback side, end to end ──────────────────────────────────────
    //
    // `cloudflared` is a plain reverse proxy into this listener, so everything
    // between it and the dev server can be tested by connecting to the listener
    // directly. That covers the gate, the grant table, the dialer and the byte
    // pump — every part of the tunnel path except Cloudflare itself.

    use crate::session::{self, ShareSession};

    /// A stand-in for the dev server in a box. Answers one canned response per
    /// connection. Never joined — see the note in `p2p`'s equivalent.
    fn fake_dev_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                // Read until the peer pauses, not once. A request's head and its
                // body are separate writes and TCP is free to deliver them
                // separately, so a single `read` sees the head alone often
                // enough to make a body assertion flaky under load.
                let _ = c.set_read_timeout(Some(Duration::from_millis(250)));
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = c.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&got).to_string();
                // Echo the request back in the body, so a test can assert on
                // what the box actually received.
                let body = format!("SAW<{head}>");
                let _ = c.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }

    async fn tunnel_bridge(
        dir: &std::path::Path,
        port: u16,
    ) -> (Arc<Bridge>, String, tokio::net::TcpListener) {
        let mut sess = ShareSession::new(
            "env/test/demo",
            port,
            crate::session::Transport::Tunnel,
            "https://test.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (grant, secret) = session::mint_grant(None, 4_000_000_000).unwrap();
        sess.grants.push(grant);
        session::write(dir, &sess).expect("write session");
        let dialer = crate::dialer::Dialer::spawn_local(port).expect("dialer");
        let bridge = Arc::new(Bridge::new(
            dir.to_path_buf(),
            "env/test/demo".into(),
            "digest".into(),
            "demo".into(),
            crate::session::Transport::Tunnel,
            "https://test.trycloudflare.com".into(),
            dialer,
        ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        (bridge, secret, listener)
    }

    /// One request, one connection, exactly as `cloudflared` would send it.
    async fn request(addr: std::net::SocketAddr, head: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        c.write_all(head.as_bytes()).await.expect("write");
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).to_string()
    }

    #[tokio::test]
    async fn the_link_admits_a_browser_and_nothing_else_does() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // No credential at all: refused without touching the box.
        let anon = request(addr, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;
        assert!(anon.starts_with("HTTP/1.1 401 "), "{anon}");
        assert!(!anon.contains("SAW<"), "an anonymous request reached the box");

        // Following the invite link: bounced, with the token moved to a cookie.
        let first = request(
            addr,
            "GET /dash HTTP/1.1\r\nHost: t\r\nCookie: x=1\r\n\r\n",
        )
        .await;
        assert!(first.starts_with("HTTP/1.1 401 "), "{first}");
        let invited = request(
            addr,
            &format!("GET /dash?h5i={secret} HTTP/1.1\r\nHost: t\r\n\r\n"),
        )
        .await;
        assert!(invited.contains("302"), "{invited}");
        assert!(invited.contains("Location: /dash"), "{invited}");
        assert!(invited.contains("Set-Cookie: h5i_share="), "{invited}");
        assert!(!invited.contains("SAW<"), "the invite request reached the box");

        // With the cookie, it reaches the dev server — and the dev server never
        // sees the credential that admitted the visitor.
        let served = request(
            addr,
            &format!("GET /dash HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}; sid=9\r\n\r\n"),
        )
        .await;
        assert!(served.contains("SAW<"), "{served}");
        assert!(!served.contains(&secret), "the credential reached the box: {served}");
        assert!(served.contains("Cookie: sid=9"), "the app's own cookie was dropped: {served}");
        assert!(served.contains("Connection: close"), "{served}");

        serving.abort();
    }

    /// A dev server that does **not** honour `Connection: close`: it answers
    /// keep-alive and stays open. Records every request head it sees.
    fn stubborn_keepalive_server() -> (u16, Arc<Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let seen: Arc<Mutex<Vec<String>>> = Default::default();
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        let out = seen.clone();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let seen = out.clone();
                std::thread::spawn(move || loop {
                    let mut buf = [0u8; 4096];
                    let n = match c.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf[..n]).to_string());
                    let _ = c.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nhi",
                    );
                });
            }
        });
        (port, seen)
    }

    #[tokio::test]
    async fn a_second_request_cannot_ride_in_on_an_authorized_connection() {
        // The control this feature rests on, tested against the case that
        // breaks the polite version of it: a dev server that ignores
        // `Connection: close`. The box runs agent-written code, so asking it to
        // hang up is a request, not a guarantee — the proxy has to stop reading
        // the client itself.
        let (port, seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Both requests in one write, as a connection-pool reuse or a
        // pipelining client would deliver them. The second carries no
        // credential at all.
        let pipelined = format!(
            "GET /first HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n\
             GET /smuggled HTTP/1.1\r\nHost: t\r\n\r\n"
        );
        let got = request(addr, &pipelined).await;
        assert!(got.contains("hi"), "the first request should be served: {got}");

        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = seen.lock().unwrap().join("");
        assert!(seen.contains("/first"), "the first request never arrived: {seen}");
        assert!(
            !seen.contains("/smuggled"),
            "an ungated second request reached the box: {seen}"
        );

        serving.abort();
    }

    #[tokio::test]
    async fn a_connection_both_ends_stop_using_does_not_hold_a_slot_forever() {
        // The box may ignore `Connection: close`, so the proxy cannot wait for
        // it to hang up. If nobody watched the client either, a quiet
        // connection would hold one of the share's slots until the grant
        // expired — up to a day.
        let (port, _seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(
                format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
            let mut buf = [0u8; 128];
            let _ = tokio::time::timeout(Duration::from_secs(2), c.read(&mut buf)).await;
            // Walk away. The box's side is still open and still says
            // keep-alive, so only the client going quiet can end this.
            drop(c);
        }

        // The slot has to come back. All 64 of them, in fact — if the
        // connection were still holding one, only 63 would be free.
        //
        // Polled rather than slept, with a budget far past what this ever
        // needs: the release happens when a task the test does not own gets
        // scheduled, and a fixed wait is a test that fails on a busy machine
        // for a reason that has nothing to do with the code. It costs nothing
        // when it passes, which is every time the release actually happens.
        let mut free = 0;
        for _ in 0..600 {
            let mut held = Vec::new();
            while let Some(p) = bridge.admit() {
                held.push(p);
            }
            free = held.len();
            if free == 64 {
                break;
            }
            drop(held);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(free, 64, "a finished connection is still holding a slot");

        serving.abort();
    }

    #[tokio::test]
    async fn a_body_the_proxy_cannot_measure_is_refused_rather_than_streamed() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!(
                "POST /upload HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Transfer-Encoding: chunked\r\n\r\n"
            ),
        )
        .await;
        assert!(got.starts_with("HTTP/1.1 501 "), "{got}");
        assert!(!got.contains("SAW<"), "a chunked body reached the box");

        serving.abort();
    }

    #[tokio::test]
    async fn a_declared_body_reaches_the_box_whole() {
        // The other half of the one-request rule: stopping at the declared
        // length must not truncate a real form post.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!(
                "POST /form HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Content-Length: 9\r\n\r\nname=alex"
            ),
        )
        .await;
        assert!(got.contains("name=alex"), "the body was truncated: {got}");

        serving.abort();
    }

    #[tokio::test]
    async fn a_stopped_share_stops_admitting_the_link() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let ok = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(ok.contains("SAW<"), "{ok}");

        // `h5i box share stop`, from another process.
        crate::run::stop(dir.path()).expect("stop");

        let after = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(after.starts_with("HTTP/1.1 401 "), "{after}");
        assert!(!after.contains("SAW<"), "a stopped share still reached the box");

        serving.abort();
    }

    #[test]
    fn the_busy_page_declares_the_length_it_actually_has() {
        // Hand-counted lengths are how a body gets truncated the first time
        // somebody rewords the sentence.
        let r = busy_response();
        let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.contains(&format!("Content-Length: {}", body.len())), "{r}");
        assert!(head.starts_with("HTTP/1.1 503 "));
        assert!(head.contains("Retry-After:"));
    }

    fn which_cloudflared() -> bool {
        std::process::Command::new("cloudflared")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
}
