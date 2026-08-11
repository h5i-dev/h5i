//! The other side: `h5i join <ticket>`.
//!
//! Turns a ticket into a URL on the joiner's own machine. Every request to that
//! URL becomes one QUIC stream to the sharer, which becomes one TCP connection
//! to the dev server inside their box. The joiner's browser sees an ordinary
//! local web app: HTML, hot reload, XHR, the lot.
//!
//! Two things about this side are worth being explicit about, because both are
//! easy to get wrong in a way that only hurts the person who was doing someone
//! else a favour by joining.
//!
//! **The local listener is gated.** A port bound on loopback is reachable by
//! every process on this machine and by every page open in this browser. That
//! is the same problem the viewer forward solves on the sharer's machine, and
//! it arrives here on somebody else's computer, so it gets the same answer: a
//! token in the URL, moved into a cookie on first use, and a refusal without
//! it. The token is minted **here** and is not the ticket secret — nothing that
//! authorizes the share is ever handed to a browser.
//!
//! **The page is untrusted code, and a loopback origin is a privileged place to
//! run it.** The app being shared was written by someone else's agent. Served
//! from `127.0.0.1` it sits on an origin that browsers exempt from their
//! private-network protections, so it has an easier reach at this machine's own
//! local services than the same page on a public origin would. That is stated
//! in MANUAL.md and printed at join time rather than buried: it is the
//! joiner's risk, and they are the one person who did not choose to take it.

use h5i_error::H5iError;

use crate::bridge::Path;
use crate::http_front::{self, Next};
use crate::ticket::Ticket;

/// What the joiner needs to print before anything happens.
pub struct Joined {
    /// The URL to open, token included.
    pub url: String,
    /// How the connection is carried, as observed at join time. `None` while
    /// the transport has not settled on a path yet, which is a real answer and
    /// not a synonym for "relayed".
    pub path: Option<Path>,
    /// The box on the far end, as the ticket names it.
    pub box_id: String,
}

/// Connect, bind, and serve until interrupted.
///
/// `port` of 0 asks the operating system for a free one, which is the default:
/// a fixed port would collide with whatever else the joiner is running, and
/// there is nothing to bookmark because the URL carries a fresh token anyway.
pub async fn run(
    ticket: Ticket,
    port: u16,
    announce: impl FnOnce(&Joined),
) -> Result<(), H5iError> {
    let now = chrono::Utc::now().timestamp();
    if ticket.remaining(now).is_none() {
        return Err(H5iError::Metadata(
            "this ticket has expired. Ask whoever shared it for a new one.".into(),
        ));
    }

    let endpoint = crate::p2p::bind_joiner().await?;
    let conn = crate::p2p::dial(&endpoint, &ticket.addr).await?;
    let path = crate::p2p::path_of(&conn);

    // Loopback only. Never an external address, on any code path: this proxy
    // exists to give one browser on this machine a door, not to republish
    // someone else's dev server.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| H5iError::Metadata(format!("could not bind a local port: {e}")))?;
    let local = listener.local_addr()?;

    // Minted here, and deliberately not the ticket secret. The browser gets a
    // credential for *this* proxy; the credential for the share never leaves
    // this process.
    let local_token = h5i_core::token::hex(16)?;

    announce(&Joined {
        url: format!(
            "http://127.0.0.1:{}/?{}={local_token}",
            local.port(),
            crate::gate::QUERY_PARAM
        ),
        path,
        box_id: ticket.box_id.clone(),
    });

    let secret = std::sync::Arc::new(ticket.secret.clone());
    let local_token = std::sync::Arc::new(local_token);
    // Named after the port this proxy bound, so two `h5i join` sessions on one
    // machine do not overwrite each other's cookie on `127.0.0.1`.
    let cookie = std::sync::Arc::new(crate::gate::cookie_for_port(local.port()));
    // Bounded like the sharer's front, and for a smaller but real reason: this
    // listener is reachable by every process on this machine.
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(256));
    let conn = std::sync::Arc::new(conn);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                // Not a bare `continue`: tokio only clears readiness on
                // `WouldBlock`, so a persistent error (`EMFILE`) would return
                // instantly every time and spin a core forever.
                let sock = match accepted {
                    Ok((sock, _)) => sock,
                    Err(e) => {
                        eprintln!("join: could not accept a connection: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let (secret, local_token, conn, cookie) = (
                    secret.clone(),
                    local_token.clone(),
                    conn.clone(),
                    cookie.clone(),
                );
                tokio::spawn(async move {
                    let _slot = slot;
                    if let Err(e) = handle(sock, &conn, &secret, &local_token, &cookie).await {
                        eprintln!("join: {e}");
                    }
                });
            }
            // The sharer stopped sharing, or revoked this ticket. Say so once
            // and exit, rather than leaving a local URL that answers every
            // request with a failure the joiner has to interpret.
            reason = conn.closed() => {
                return Err(H5iError::Metadata(format!(
                    "the share ended: {reason}"
                )));
            }
        }
    }
}

async fn handle(
    mut sock: tokio::net::TcpStream,
    conn: &iroh::endpoint::Connection,
    secret: &str,
    local_token: &str,
    cookie: &str,
) -> Result<(), H5iError> {
    let Some((head, rest)) = http_front::read_head(&mut sock).await else {
        return Ok(());
    };
    let next = http_front::decide(
        &head,
        cookie,
        |t| crate::session::secret_matches(t, local_token),
        // Loopback is http, and a `Secure` cookie there is one some browsers
        // decline to store.
        false,
    );
    let (head, req) = match next {
        Next::Respond(body) => {
            http_front::respond(&mut sock, &body).await;
            return Ok(());
        }
        Next::Proxy { head, req } => (head, req),
    };

    // Only now does anything cross the network. Everything above runs for a
    // request that may have come from any page this browser has open.
    //
    // A failure here is answered with an HTTP response rather than by dropping
    // the connection. A browser shown a closed socket says "the connection was
    // reset", which tells the person nothing about a share that is busy or a
    // ticket that was revoked — and they are the one who has to decide whether
    // to wait or to ask for a new invite.
    let (send, recv) = match crate::p2p::open_stream(conn, secret).await {
        Ok(pair) => pair,
        Err(e) => {
            http_front::respond(&mut sock, &upstream_failure(&e)).await;
            return Ok(());
        }
    };

    // The same one-request-per-connection rule as the sharer's front, and for
    // the same reason: this proxy is on somebody's loopback, where every page
    // in their browser can reach it.
    let counts = http_front::Counters::default();
    let forwarded = http_front::Forwarded {
        head: &head,
        rest: &rest,
        req: &req,
    };
    let _ = http_front::proxy_one(sock, recv, send, forwarded, &counts).await;
    if counts.was_truncated() {
        // The joiner has no receipt of its own, so this is the only place a
        // person learns their download was cut off rather than finished.
        eprintln!("join: a response was cut off after taking too long; it is incomplete");
    }
    Ok(())
}

/// Turn a failure to reach the sharer into something a browser renders.
///
/// `503` for busy, because reloading is the right move; `502` for everything
/// else, because the problem is between here and the box rather than with the
/// request. Never `401`: the joiner's own token was fine, and telling them
/// otherwise sends them hunting for the wrong thing.
fn upstream_failure(e: &crate::p2p::OpenError) -> String {
    let (code, reason) = match e {
        crate::p2p::OpenError::Busy => (503, "Service Unavailable"),
        _ => (502, "Bad Gateway"),
    };
    let body = e.to_string();
    format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(expires_at: i64) -> Ticket {
        Ticket {
            v: 1,
            box_id: "env/agent/demo".into(),
            port: 3000,
            grant: "a1b2c3d4".into(),
            expires_at,
            secret: "ab".repeat(crate::ticket::SECRET_BYTES),
            addr: serde_json::json!({}),
        }
    }

    #[test]
    fn a_busy_share_and_a_bad_ticket_do_not_look_the_same_to_the_browser() {
        // The distinction the person on this end has to act on: wait, or go and
        // ask for a new invite.
        let busy = upstream_failure(&crate::p2p::OpenError::Busy);
        assert!(busy.starts_with("HTTP/1.1 503 "), "{busy}");
        assert!(busy.contains("wait a moment"), "{busy}");

        let refused = upstream_failure(&crate::p2p::OpenError::Refused);
        assert!(refused.starts_with("HTTP/1.1 502 "), "{refused}");
        assert!(refused.contains("ask for"), "{refused}");

        // Neither is a 401: this proxy's own token was fine.
        assert!(!busy.contains("401") && !refused.contains("401"));
        for r in [busy, refused] {
            let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
            assert!(head.contains(&format!("Content-Length: {}", body.len())));
        }
    }

    #[tokio::test]
    async fn an_expired_ticket_is_refused_before_anything_is_dialled() {
        // Checked here as well as by the sharer. The joiner should be told
        // plainly rather than watching a connection fail for reasons that look
        // like a network problem.
        let err = run(ticket(1), 0, |_| panic!("must not get as far as announcing"))
            .await
            .expect_err("expired");
        assert!(format!("{err}").contains("expired"));
    }
}
