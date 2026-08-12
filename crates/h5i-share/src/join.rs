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
    /// The ticket worked, but the share said something worth repeating: it is
    /// full, or the box has nothing listening on the shared port yet.
    pub warning: Option<String>,
    /// This join had to fall back to `127.0.0.1`, so it shares a cookie jar
    /// with every other local service. See [`bind_loopback`] for what that
    /// costs and why it is said out loud rather than assumed away.
    pub shared_jar: bool,
}

/// Bind this join's own address on the loopback interface.
///
/// **The address is the isolation.** Cookies are scoped by *host* and ignore
/// the port, so a proxy on `127.0.0.1:8899` shares one jar with every other
/// HTTP service on this machine, and that goes wrong in both directions at
/// once:
///
/// * **Outward.** The cookie this proxy sets — `h5i_share_<port>=<token>` —
///   is sent by the browser to *every* `127.0.0.1` service the joiner visits
///   while joined. `HttpOnly` is no help: this is the server-side `Cookie`
///   header. Any such service learns the port from the cookie's own name and
///   the token from its value, and can then reach the remote box. The token is
///   minted here precisely because every local process is outside the gate,
///   and it was being handed to all of them.
///
/// * **Inward, which is worse.** Every cookie any *other* loopback service has
///   set on `127.0.0.1` is sent here too, and forwarded upstream — so a
///   `session=<secret>` belonging to the joiner's own local app arrives at
///   agent-written code inside somebody else's box on its first request. The
///   joiner is the person who did not choose that risk.
///
/// A different loopback address is a different cookie host, with no DNS, no
/// `/etc/hosts`, and no browser-specific `*.localhost` handling involved:
/// `127.0.0.0/8` is all loopback, so `127.x.y.z` is reachable from this
/// machine and from nowhere else, and the browser keeps a jar for it that only
/// this share has ever written to.
///
/// Linux routes the whole `/8` by default and this simply works. macOS
/// configures only `127.0.0.1` on `lo0`, so the bind fails there and this
/// falls back — the fallback is reported rather than silent, because the two
/// leaks above come back with it.
async fn bind_loopback(port: u16) -> Result<(tokio::net::TcpListener, bool), H5iError> {
    // Random, so a local process cannot find this share's jar by guessing.
    // `x.0.0` and `x.255.255` are avoided only to stay clear of anything that
    // treats them as network or broadcast addresses.
    for _ in 0..8 {
        let r = h5i_core::token::hex(3)?;
        let b = u8::from_str_radix(&r[0..2], 16).unwrap_or(1);
        let c = u8::from_str_radix(&r[2..4], 16).unwrap_or(1);
        let d = u8::from_str_radix(&r[4..6], 16).unwrap_or(1).clamp(1, 254);
        // Never `127.0.0.1` itself: the whole point is a host nothing else has
        // written a cookie for.
        let addr = std::net::Ipv4Addr::new(127, b.clamp(1, 254), c, d);
        if let Ok(l) = tokio::net::TcpListener::bind((addr, port)).await {
            return Ok((l, false));
        }
    }
    let l = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| H5iError::Metadata(format!("could not bind a local port: {e}")))?;
    Ok((l, true))
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
) -> Result<String, H5iError> {
    let now = chrono::Utc::now().timestamp();
    if ticket.remaining(now).is_none() {
        return Err(H5iError::Metadata(expired_here(ticket.expires_at, now)));
    }

    let endpoint = crate::p2p::bind_joiner().await?;
    let conn = crate::p2p::dial(&endpoint, &ticket.addr).await?;
    let path = crate::p2p::path_of(&conn);

    // Presented once, before anything is announced. See `verify_ticket`: it
    // turns "joined" into a statement about the ticket rather than about the
    // network, and it keeps a joiner who has not opened the page yet from being
    // hung up on thirty seconds later.
    let warning = check_outcome(crate::p2p::verify_ticket(&conn, &ticket.secret).await)?;

    // Loopback only. Never an external address, on any code path: this proxy
    // exists to give one browser on this machine a door, not to republish
    // someone else's dev server.
    let (listener, shared_jar) = bind_loopback(port).await?;
    let local = listener.local_addr()?;

    // Minted here, and deliberately not the ticket secret. The browser gets a
    // credential for *this* proxy; the credential for the share never leaves
    // this process.
    let local_token = h5i_core::token::hex(16)?;

    announce(&Joined {
        url: format!(
            "http://{}:{}/?{}={local_token}",
            local.ip(),
            local.port(),
            crate::gate::QUERY_PARAM
        ),
        path,
        box_id: ticket.box_id.clone(),
        warning,
        shared_jar,
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
                // Not an error. A share ending is the most ordinary thing that
                // happens to one, and this used to exit non-zero with
                // `Error: Metadata error: the share ended: closed by peer:
                // h5i: this share has ended (code 5)` — an internal enum name,
                // the same fact three times, and a wire constant, for a
                // revoke, an expiry, a Ctrl-C and a stopped box alike.
                //
                // Sanitised: these are bytes the *sharer* chose, arriving on
                // the joiner's terminal. quinn renders an application close
                // reason with `from_utf8_lossy`, so a `\r` or an `\x1b[2J` in
                // it can erase or forge the lines around it.
                let said = h5i_core::redact::sanitize_display(&reason.to_string());
                // A share *ending* is ordinary; a connection *failing* is not,
                // and the first version returned `Ok` for both. So a sharer
                // killed mid-session — or a laptop lid, or a dropped link —
                // printed "they stopped sharing, or the ticket ran out" and
                // exited 0, when the ticket was almost certainly still good
                // and joining again would have worked. Only the sharer's own
                // application close is an ending; everything else is the error
                // it was before.
                return match &reason {
                    iroh::endpoint::ConnectionError::ApplicationClosed(_)
                    | iroh::endpoint::ConnectionError::LocallyClosed => {
                        Ok(ending_line(&said))
                    }
                    _ => Err(H5iError::Metadata(format!(
                        "the connection to the sharer failed: {said}. Their machine or the \
                         network went away rather than the share ending — the ticket is \
                         probably still good, so joining again may just work."
                    ))),
                };
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
        Next::Respond(body, _why) => {
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
        eprintln!("join: the box left a response unfinished; what arrived is incomplete");
    }
    Ok(())
}

/// What the join-time ticket check means for the command.
///
/// Its own function so the decision can be tested: `run` binds a listener and
/// serves until interrupted, so a test that went through it would be a test of
/// tokio. This is the whole behaviour change of the check — which of the four
/// answers stops the command and which is only worth repeating.
fn check_outcome(r: Result<(), crate::p2p::OpenError>) -> Result<Option<String>, H5iError> {
    match r {
        Ok(()) => Ok(None),
        // The route into the box is broken on the sharer's side. Not fatal to
        // the join — a restarted share on the same ticket would work — but
        // worth saying up front rather than letting the first page load say it.
        Err(e @ crate::p2p::OpenError::RouteBroken) => Ok(Some(format!("{e}"))),
        // Refused is the only answer about the ticket itself, so it is the only
        // one that stops the command for that reason. Busy and unreachable are
        // both conditions that clear on their own — the share fills up and
        // empties again, the dev server is started a minute later — and failing
        // on them would tell somebody their invite is broken when it is not.
        // A sharer that cannot read its own grant table is in the second group:
        // a freed-up disk or descriptor fixes it without a new ticket.
        Err(e @ crate::p2p::OpenError::Refused) => Err(H5iError::Metadata(format!("{e}"))),
        // Also fatal, for the opposite reason: not "your ticket is bad" but
        // "there is no share left to join". Waiting on it is pointless, since
        // a share started again hands out a new ticket.
        Err(e @ crate::p2p::OpenError::ShareOver) => Err(H5iError::Metadata(format!("{e}"))),
        // Not a statement about the ticket at all: the connection is unusable,
        // so there is nothing to go on to.
        Err(e @ crate::p2p::OpenError::Transport(_)) => Err(H5iError::Metadata(format!("{e}"))),
        Err(e) => Ok(Some(format!("{e}"))),
    }
}

/// What to say about a ticket that looks expired *on this machine*.
///
/// The check is local, and a local check against a wrong clock is an
/// accusation. A joiner two hours fast refuses a ticket the sharer's own
/// `share status` shows with 58 minutes left, tells the person to ask for a
/// replacement, and every replacement fails the same way — with nothing in the
/// message pointing at the actual problem. So: how long ago, in this machine's
/// opinion, and what that opinion depends on. A ticket that expired minutes ago
/// is almost certainly just expired; one that "expired" hours before it was
/// sent is a clock.
fn expired_here(expires_at: i64, now: i64) -> String {
    let ago = now.saturating_sub(expires_at);
    format!(
        "this ticket expired {} ago, by this machine's clock. Ask whoever shared it for a new          one — and if they say it is still good, check the clocks: the two are compared here,          so a machine that is set wrong refuses good tickets and a new one will fail the same          way.",
        crate::session::humanise(ago)
    )
}

/// The whole sentence a joiner reads when its connection is closed.
///
/// The headline is not always "the share ended". A revoke cuts *this peer* off
/// and leaves the share running for everyone else — that is the advertised
/// behaviour of `share revoke` — so announcing the share's end to the one
/// person it was aimed at was a statement about somebody else's screen. The
/// clause after it was already right; the sentence in front of it was not.
fn ending_line(said: &str) -> String {
    let cause = why_it_ended(said);
    if said.contains("revoked") {
        format!("your access to this share ended{cause}")
    } else {
        format!("the share ended{cause}")
    }
}

/// The cause, in the joiner's words, when the sharer gave one.
///
/// The wire reasons are written for the other end of a socket; this is the
/// half a person reads. Anything unrecognised becomes the honest hedge rather
/// than being dressed up as an explanation.
fn why_it_ended(said: &str) -> String {
    if said.contains("revoked") || said.contains("expired") {
        " — that ticket was revoked or ran out".into()
    } else if said.contains("this share has ended") {
        " — they stopped sharing".into()
    } else if said.contains("direct path") {
        // Matches both of the sharer's `--direct-only` closes. The first
        // version matched only "no direct path", which is sent *before* the
        // joiner finishes its handshake and so never reaches here; the one
        // that does reach here says "the direct path was lost", which it
        // missed — so it printed the raw wire text instead.
        " — the direct connection was lost, and they shared with --direct-only".into()
    } else if said.contains("no ticket was presented") {
        // A joiner that connected and never opened the URL. The wire string is
        // written for the other end of a socket, and "h5i: no ticket was
        // presented" told the person nothing about what to do.
        " — nothing was opened, so the sharer hung up. Open the link next time before it does"
            .into()
    } else if said.contains("h5i:") {
        format!(" — {}", said.replace("h5i: ", ""))
    } else {
        " — they stopped sharing, or the ticket ran out".into()
    }
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
        // The same two codes the tunnel front uses for the same two answers,
        // so the browser's own handling — and anything reading a log of
        // statuses — does not depend on which transport carried it.
        crate::p2p::OpenError::ShareOver => (410, "Gone"),
        crate::p2p::OpenError::SharerFault => (503, "Service Unavailable"),
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

    #[test]
    fn each_ending_is_attributed_to_what_actually_happened() {
        // Every one of these was wrong at some point in the last two rounds:
        // the `--direct-only` arm matched the close the joiner never sees and
        // missed the one it does; the grace hang-up leaked a wire string with
        // its `h5i:` prefix intact as a success; and a killed sharer was
        // reported as "they stopped sharing, or the ticket ran out".
        let cases = [
            (
                "h5i: this ticket was revoked or has expired",
                "revoked or ran out",
            ),
            ("h5i: this share has ended", "they stopped sharing"),
            (
                "h5i: --direct-only, and the direct path was lost",
                "direct connection was lost",
            ),
            ("h5i: no ticket was presented", "nothing was opened"),
        ];
        for (wire, expected) in cases {
            let said = why_it_ended(wire);
            assert!(said.contains(expected), "{wire} -> {said}");
            // And the wire's own prefix does not reach the person.
            assert!(!said.contains("h5i:"), "{wire} -> {said}");
        }

        // Anything unrecognised is the honest hedge rather than a guess.
        let said = why_it_ended("closed by peer: 0");
        assert!(
            said.contains("stopped sharing, or the ticket ran out"),
            "{said}"
        );
    }

    #[test]
    fn a_revoke_is_not_announced_as_the_end_of_the_share() {
        // `share revoke` cuts one peer off and leaves the share serving
        // everybody else — that is what it is for. The person it was aimed at
        // was told "the share ended", which is a claim about other people's
        // screens and the wrong thing to tell somebody deciding whether to ask
        // for a new link.
        let revoked = ending_line("h5i: this ticket was revoked or has expired");
        assert!(
            revoked.starts_with("your access to this share ended"),
            "{revoked}"
        );
        assert!(revoked.contains("revoked or ran out"), "{revoked}");

        // A stop really is the end of the share, for everyone.
        let stopped = ending_line("h5i: this share has ended");
        assert!(stopped.starts_with("the share ended"), "{stopped}");
        assert!(stopped.contains("they stopped sharing"), "{stopped}");
    }

    #[test]
    fn a_ticket_this_machine_thinks_is_expired_says_whose_opinion_that_is() {
        // The check is local. A joiner whose clock is two hours fast refuses a
        // ticket the sharer's own `share status` shows with 58 minutes left,
        // and the old sentence — "this ticket has expired, ask for a new one"
        // — sent them round a loop where every replacement fails identically,
        // with nothing naming the cause.
        let msg = expired_here(1_000, 1_000 + 7_200);
        assert!(msg.contains("2h0m ago"), "{msg}");
        assert!(msg.contains("by this machine's clock"), "{msg}");
        assert!(msg.contains("check the clocks"), "{msg}");

        // A ticket that ran out a minute ago is almost certainly just expired,
        // and the sentence still leads with that rather than with the clock.
        let msg = expired_here(1_000, 1_060);
        assert!(msg.starts_with("this ticket expired 1m ago"), "{msg}");
    }

    #[test]
    fn only_a_refused_ticket_stops_the_command() {
        use crate::p2p::OpenError;
        // The whole point of checking at join time is that a bad ticket fails
        // here rather than at the first page load. The equally important half
        // is that the other three answers do *not* fail: a share that is
        // momentarily full, or a dev server that has not been started yet, are
        // both things that fix themselves, and telling somebody their invite is
        // broken sends them back to ask for a ticket that works no better.
        assert!(check_outcome(Ok(())).expect("a good ticket").is_none());

        let err = check_outcome(Err(OpenError::Refused)).expect_err("refused is fatal");
        assert!(format!("{err}").contains("refused"), "{err}");

        let err = check_outcome(Err(OpenError::Transport("gone".into())))
            .expect_err("an unusable connection is fatal");
        assert!(format!("{err}").contains("gone"), "{err}");

        // And a broken route says so instead of blaming their dev server.
        let broken = check_outcome(Err(OpenError::RouteBroken)).expect("not fatal");
        let said = broken.expect("a warning");
        assert!(said.contains("cannot reach inside the box"), "{said}");
        assert!(!said.contains("start their dev server"), "{said}");

        let busy = check_outcome(Err(OpenError::Busy)).expect("busy is not fatal");
        assert!(busy.expect("a warning").contains("as many connections"));

        let down = check_outcome(Err(OpenError::Unreachable)).expect("unreachable is not fatal");
        assert!(down.is_some(), "an unreachable box said nothing at all");

        // A share that has ended is fatal for the opposite reason to a refusal
        // — not "your ticket is bad" but "there is nothing left to join" — and
        // must not say either of the two sentences that send somebody off to
        // ask for a replacement invite.
        let over = check_outcome(Err(OpenError::ShareOver)).expect_err("a dead share is fatal");
        let over = format!("{over}");
        assert!(over.contains("has ended"), "{over}");
        assert!(!over.contains("refused"), "{over}");
        assert!(!over.contains("expired"), "{over}");

        // A sharer that cannot read its own grant table is not fatal: a freed
        // disk or descriptor fixes it, on the ticket already in hand.
        let fault = check_outcome(Err(OpenError::SharerFault)).expect("not fatal");
        let fault = fault.expect("a warning");
        assert!(fault.contains("could not read its own record"), "{fault}");
        assert!(!fault.contains("ask for a new"), "{fault}");
    }

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

        // A share that has ended is `410`, not `502`: the resource is gone,
        // and it is the same code the tunnel front answers with, so a visitor
        // sees one behaviour whichever transport carried them.
        let over = upstream_failure(&crate::p2p::OpenError::ShareOver);
        assert!(over.starts_with("HTTP/1.1 410 "), "{over}");
        assert!(over.contains("has ended"), "{over}");
        let fault = upstream_failure(&crate::p2p::OpenError::SharerFault);
        assert!(fault.starts_with("HTTP/1.1 503 "), "{fault}");

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

    /// Each join gets its own cookie jar, because it gets its own host.
    ///
    /// Cookies are scoped by host and ignore the port, so binding
    /// `127.0.0.1` put this proxy in one jar with every other local service —
    /// and that leaked both ways at once. Outward: the token this proxy sets
    /// went to every `127.0.0.1` service the joiner visited while joined,
    /// with the port in the cookie's own name, so any of them could reach the
    /// remote box. Inward, and worse: every cookie those services had set
    /// came here and was forwarded, so a `session=<secret>` for the joiner's
    /// own local app arrived at agent-written code inside somebody else's box.
    #[tokio::test]
    async fn a_join_binds_a_loopback_address_of_its_own() {
        let (a, shared_a) = bind_loopback(0).await.expect("bind");
        let (b, shared_b) = bind_loopback(0).await.expect("bind");
        let (ip_a, ip_b) = (a.local_addr().unwrap().ip(), b.local_addr().unwrap().ip());

        // On a host that routes all of `127.0.0.0/8` — Linux — both joins get
        // an address of their own, and it is never `127.0.0.1`. macOS
        // configures only `127.0.0.1` on `lo0`, so there this falls back and
        // says so rather than pretending the jar is private.
        if shared_a || shared_b {
            assert_eq!(ip_a.to_string(), "127.0.0.1");
            assert_eq!(ip_b.to_string(), "127.0.0.1");
            return;
        }
        assert_ne!(
            ip_a.to_string(),
            "127.0.0.1",
            "the fallback was taken silently"
        );
        assert_ne!(ip_b.to_string(), "127.0.0.1");
        assert_ne!(ip_a, ip_b, "two joins landed in one cookie jar");
        assert!(
            ip_a.is_loopback(),
            "a join bound something reachable: {ip_a}"
        );
        assert!(
            ip_b.is_loopback(),
            "a join bound something reachable: {ip_b}"
        );

        // And it is a real listener a browser on this machine can reach.
        let port = a.local_addr().unwrap().port();
        assert!(
            tokio::net::TcpStream::connect((ip_a, port)).await.is_ok(),
            "the address a joiner is told to open does not accept connections"
        );
    }

    #[tokio::test]
    async fn an_expired_ticket_is_refused_before_anything_is_dialled() {
        // Checked here as well as by the sharer. The joiner should be told
        // plainly rather than watching a connection fail for reasons that look
        // like a network problem.
        let err = run(ticket(1), 0, |_| {
            panic!("must not get as far as announcing")
        })
        .await
        .expect_err("expired");
        assert!(format!("{err}").contains("expired"));
    }
}
