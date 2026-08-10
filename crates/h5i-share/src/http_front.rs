//! The loopback HTTP front that both browser-facing sides of a share use.
//!
//! Two places serve HTTP to a browser and they are otherwise nothing alike: the
//! sharer's quick tunnel ([`crate::tunnel`]), whose visitor is on the open
//! internet behind Cloudflare, and the joiner's own proxy ([`crate::join`]),
//! whose visitor is a tab on the same machine. What they have in common is the
//! part that must not be written twice: read a bounded head, find the
//! credential, bounce the first request so the token leaves the URL, and never
//! pass the credential upstream.
//!
//! The joiner's side needs this as much as the sharer's, and that is worth
//! saying because it is the non-obvious half. A proxy bound on the joiner's
//! loopback is reachable by every process on their machine and by every page
//! they have open — the same problem the viewer forward has, arriving on
//! somebody else's computer. So it gets the same answer: a token, and a refusal
//! without it.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::gate;

/// How long a connection may take to send its head. A browser's speculative
/// preconnect opens a socket and sends nothing; without this it would hold a
/// task until it gave up on its own.
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

/// What to do with a connection once its head has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Write these bytes and close. The redirect that sets the cookie, or a
    /// refusal.
    Respond(String),
    /// Authorized: open the upstream, send this head, then move bytes.
    Proxy {
        /// The head as it should reach the box: share credential removed.
        head: String,
        /// Whether this was an upgrade, for the sharer's terminal line.
        upgrade: bool,
    },
}

/// Read up to the end of the headers.
///
/// Returns the head and whatever arrived after it — a request body usually
/// follows in the same packet, and a proxy that read it and threw it away would
/// break every form on the shared app.
pub async fn read_head<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + HEAD_TIMEOUT;
    loop {
        let n = tokio::time::timeout_at(deadline, r.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_head_end(&buf) {
            let head = String::from_utf8(buf[..end].to_vec()).ok()?;
            return Some((head, buf[end..].to_vec()));
        }
        // Bounded before it is parsed. A peer that sends headers forever must
        // not be able to make this allocate forever.
        if buf.len() > gate::MAX_HEAD {
            return None;
        }
    }
}

/// Index just past the blank line that ends an HTTP head.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Decide what a request gets.
///
/// `authorize` is the caller's grant check — the bridge's table on the sharer's
/// side, a single local token on the joiner's. It is only ever called with a
/// token that already looks like one, because [`gate::parse`] refuses the rest.
pub fn decide(head: &str, authorize: impl FnOnce(&str) -> bool, secure: bool) -> Next {
    let Some(req) = gate::parse(head) else {
        return Next::Respond(gate::refusal_response(gate::Refusal::Malformed));
    };
    let Some(token) = req.token.clone() else {
        return Next::Respond(gate::refusal_response(gate::Refusal::NotAuthorized));
    };
    if !authorize(&token) {
        return Next::Respond(gate::refusal_response(gate::Refusal::NotAuthorized));
    }
    if req.from_query {
        // Authorized, but the token is in the URL. Set the cookie and send the
        // browser to the same page without it, rather than proxying this one:
        // a request that reached the app with the token in its target would put
        // it in the app's logs and in `Referer` on every link out of the page.
        //
        // An upgrade is the one thing that cannot be redirected — a WebSocket
        // handshake follows no 302 — but a browser never opens one before the
        // page that opens it, so by then the cookie is set.
        return Next::Respond(gate::set_cookie_redirect(&token, &req.clean_target, secure));
    }
    Next::Proxy {
        head: gate::rewrite_for_upstream(head, &req),
        upgrade: req.upgrade,
    }
}

/// Write a response and close politely.
pub async fn respond<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, body: &str) {
    let _ = w.write_all(body.as_bytes()).await;
    let _ = w.flush().await;
    let _ = w.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "abc123";

    fn yes(t: &str) -> bool {
        t == TOKEN
    }

    #[tokio::test]
    async fn the_head_is_read_and_the_body_after_it_is_kept() {
        // A POST usually arrives head-and-body in one packet. Dropping the tail
        // here would break every form on the shared app, and it would look like
        // the app's bug rather than the proxy's.
        let raw = b"POST /submit HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let mut r = &raw[..];
        let (head, rest) = read_head(&mut r).await.expect("head");
        assert!(head.starts_with("POST /submit"));
        assert_eq!(rest, b"hello");
    }

    #[tokio::test]
    async fn a_head_that_never_ends_is_refused_rather_than_buffered() {
        let raw = format!("GET / HTTP/1.1\r\nX: {}\r\n", "a".repeat(gate::MAX_HEAD + 10));
        let mut r = raw.as_bytes();
        assert!(read_head(&mut r).await.is_none());
    }

    #[test]
    fn the_first_visit_is_bounced_so_the_token_leaves_the_url() {
        let head = "GET /dash?h5i=abc123&tab=2 HTTP/1.1\r\nHost: x\r\n\r\n";
        let Next::Respond(r) = decide(head, yes, true) else {
            panic!("a token in the URL must be redirected, not proxied");
        };
        assert!(r.contains("302"));
        assert!(r.contains("Location: /dash?tab=2"));
        assert!(r.contains("Set-Cookie: h5i_share=abc123"));
    }

    #[test]
    fn a_request_with_the_cookie_is_proxied_without_it() {
        let head = "GET /app.js HTTP/1.1\r\nHost: x\r\nCookie: h5i_share=abc123; sid=9\r\n\r\n";
        let Next::Proxy { head: up, upgrade } = decide(head, yes, true) else {
            panic!("an authorized request must be proxied");
        };
        assert!(!up.contains("abc123"), "credential reached the box: {up}");
        assert!(up.contains("Cookie: sid=9"));
        assert!(!upgrade);
    }

    #[test]
    fn no_credential_and_a_wrong_credential_get_the_same_answer() {
        let none = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let wrong = "GET / HTTP/1.1\r\nHost: x\r\nCookie: h5i_share=nope\r\n\r\n";
        let (Next::Respond(a), Next::Respond(b)) = (decide(none, yes, true), decide(wrong, yes, true))
        else {
            panic!("neither may be proxied");
        };
        assert_eq!(a, b);
        assert!(a.contains("401"));
    }

    #[test]
    fn a_websocket_upgrade_is_proxied_and_flagged() {
        // Hot reload is a WebSocket. A share where the page loads but never
        // updates is not a share of a dev server.
        let head = "GET /hmr HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                    Cookie: h5i_share=abc123\r\n\r\n";
        let Next::Proxy { upgrade, .. } = decide(head, yes, true) else {
            panic!("an upgrade must be proxied");
        };
        assert!(upgrade);
    }

    #[test]
    fn something_that_is_not_http_never_reaches_the_authorizer() {
        // The gate is the first thing an anonymous connection touches, so it
        // has to refuse without consulting anything.
        let mut called = false;
        let out = decide("\u{16}\u{3}\u{1} a TLS ClientHello, not a request", |_| {
            called = true;
            true
        }, true);
        assert!(matches!(out, Next::Respond(_)));
        assert!(!called, "malformed input must not reach the grant table");
    }

    #[test]
    fn loopback_gets_a_cookie_a_loopback_browser_will_actually_store() {
        let head = "GET /?h5i=abc123 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let Next::Respond(r) = decide(head, yes, false) else {
            panic!("redirect expected");
        };
        assert!(!r.contains("Secure"));
    }
}
