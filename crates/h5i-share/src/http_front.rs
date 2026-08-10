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

/// How long the *box* may take to finish its response head. Longer than the
/// client's, and a different question: a dev server cold-optimising its
/// dependencies on the first request is slow for a reason, and cutting it off
/// would look to the visitor like the share was broken.
const RESPONSE_HEAD_TIMEOUT: Duration = Duration::from_secs(120);

/// A response head past this is relayed as-is rather than parsed. Generous —
/// a real one is a few kilobytes — and its only job is to stop an unbounded
/// buffer.
const MAX_RESPONSE_HEAD: usize = 256 * 1024;

/// How long a response with no declared length may go silent before the
/// connection is closed. Long enough for a server-sent-events stream that
/// sends a heartbeat; short enough that a forgotten connection does not hold a
/// share slot for the life of the grant.
const RESPONSE_IDLE: Duration = Duration::from_secs(300);

/// How long a peer may pause part-way through a body it declared.
const BODY_IDLE: Duration = Duration::from_secs(30);

/// The longest a chunk-size line or a single trailer may be.
const MAX_CHUNK_LINE: usize = 1024;
/// The longest a whole trailer section may be.
const MAX_TRAILERS: usize = 8 * 1024;

/// The longest an unframed response may stay open, whatever either end does.
///
/// A server-sent-events stream is the legitimate case and an hour is generous
/// for a share whose ticket lasts one by default. Framed responses are not
/// subject to it: they end when their body does.
const UNFRAMED_LIFETIME: Duration = Duration::from_secs(3600);

/// How many `1xx` heads may precede the real response. A server sends at most
/// one or two; this is a backstop, not a budget.
const MAX_INTERIM_RESPONSES: usize = 8;

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
        /// The parsed request, which is what [`proxy_one`] needs to know where
        /// this request ends.
        req: gate::Request,
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
pub fn decide(
    head: &str,
    cookie: &str,
    authorize: impl FnOnce(&str) -> bool,
    secure: bool,
) -> Next {
    let Some(req) = gate::parse(head, cookie) else {
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
        return Next::Respond(gate::set_cookie_redirect(
            cookie,
            &token,
            &gate::safe_location(&req.clean_target),
            secure,
        ));
    }
    Next::Proxy {
        head: gate::rewrite_for_upstream(head, &req, cookie),
        req,
    }
}

/// The request as it should reach the box, and what it takes to know where it
/// ends.
pub struct Forwarded<'a> {
    /// Rewritten head: share credential removed, `Connection: close` forced.
    pub head: &'a str,
    /// Bytes that arrived in the same read as the head — the start of the body.
    pub rest: &'a [u8],
    pub req: &'a gate::Request,
}

/// Bytes moved, counted as they go so a connection cut short by a revoke still
/// reports what it carried.
#[derive(Debug, Default)]
pub struct Counters {
    pub to_box: std::sync::atomic::AtomicU64,
    pub to_peer: std::sync::atomic::AtomicU64,
}

impl Counters {
    pub fn read(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.to_box.load(Ordering::Relaxed),
            self.to_peer.load(Ordering::Relaxed),
        )
    }
}

/// Forward exactly one request into the box and one response back, then stop.
///
/// **This is where the one-request rule is actually enforced.** Adding
/// `Connection: close` to the request asks the box to hang up, and the box runs
/// agent-written code that may decline; a proxy that then kept copying would
/// hold an ungated pipe on which a second request — from anyone whose bytes
/// reach this connection — would reach the box without passing the gate.
///
/// So the client's side is **forwarded** for exactly the bytes this request
/// declared and not one more. It is still read after that, and everything it
/// says is thrown away — that is how a client hanging up is noticed, and
/// discarding is as strong a guarantee as not reading, because what matters is
/// that nothing the client sends after its first request reaches the box.
/// Whatever the box does with `Connection: close` is then its own business.
///
/// The exception is a real upgrade, and it is checked rather than believed: the
/// box has to answer `101` before the connection becomes a two-way pipe. A
/// request that merely *asked* to upgrade and got an ordinary response is
/// treated like any other single request.
pub async fn proxy_one<UR, UW>(
    client: tokio::net::TcpStream,
    up_r: UR,
    mut up_w: UW,
    request: Forwarded<'_>,
    counts: &Counters,
) -> std::io::Result<()>
where
    UR: tokio::io::AsyncRead + Unpin + Send,
    UW: tokio::io::AsyncWrite + Unpin + Send,
{
    use std::sync::atomic::Ordering;
    let Forwarded { head, rest, req } = request;
    // Kept under its own name: `rest` is shadowed below by the response's
    // trailing bytes, and the upgrade path needs the client's.
    let rest_from_client = rest;
    let (to_box, to_peer) = (&counts.to_box, &counts.to_peer);

    let (mut peer_r, mut peer_w) = client.into_split();

    // A client that said `Expect: 100-continue` is waiting for permission it
    // will never get otherwise: this proxy forwards the whole body before it
    // reads anything from the box, so the box's own `100` cannot arrive until
    // after the body it was gating. curl gives up after a second and sends
    // anyway; a client that waits without a timeout used to stall until the
    // body deadline and then get no answer at all. So the proxy answers, and
    // the header is not passed on — the box would otherwise send a second,
    // useless `100` behind the body.
    if req.expects_continue {
        peer_w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        to_peer.fetch_add(25, Ordering::Relaxed);
    }

    up_w.write_all(head.as_bytes()).await?;
    to_box.fetch_add(head.len() as u64, Ordering::Relaxed);

    // The declared body, and only the declared body. Anything past it on this
    // connection is a pipelined second request, and dropping it on the floor is
    // the point rather than an oversight.
    let from_rest;
    if req.chunked {
        // Parsed rather than refused, and the refusal was not a small cost: a
        // browser sends `Content-Length` for a form, but `cloudflared` bridges
        // HTTP/2 to the box's HTTP/1.1 and has no length to carry over, so it
        // chunks *every* request with a body. Refusing chunked meant every
        // POST through a tunnel share answered `501`, which is most of what
        // "let somebody try the web app" means.
        //
        // Forwarded verbatim, chunk framing and all — the box speaks HTTP and
        // this proxy does not need to change the encoding, only to know where
        // the request ends.
        if forward_chunked(&mut peer_r, &mut up_w, rest_from_client, to_box)
            .await
            .is_err()
        {
            // Answered rather than dropped. A body whose framing does not parse
            // is the peer's mistake or somebody's probe, and a closed socket
            // with no reply is the least informative thing to hand either.
            let _ = peer_w
                .write_all(gate::refusal_response(gate::Refusal::Malformed).as_bytes())
                .await;
            let _ = peer_w.shutdown().await;
            return Ok(());
        }
        from_rest = rest_from_client.len();
    } else {
        let want = req.content_length.unwrap_or(0);
        from_rest = (rest.len() as u64).min(want) as usize;
        if from_rest > 0 {
            up_w.write_all(&rest[..from_rest]).await?;
            to_box.fetch_add(from_rest as u64, Ordering::Relaxed);
        }
        let mut remaining = want - from_rest as u64;
        let mut buf = vec![0u8; 32 * 1024];
        while remaining > 0 {
            let take = (remaining as usize).min(buf.len());
            // Deadlined per read. Declaring a megabyte, sending one byte and
            // going quiet used to hold a connection into the box, and one of
            // the share's slots, for as long as the peer felt like it.
            let n = match tokio::time::timeout(BODY_IDLE, peer_r.read(&mut buf[..take])).await {
                Ok(r) => r?,
                Err(_) => return Ok(()),
            };
            if n == 0 {
                // The client promised a body and then hung up. Waiting for a
                // response now means waiting for a box that is itself waiting
                // for the rest of a request nobody is going to send.
                return Ok(());
            }
            up_w.write_all(&buf[..n]).await?;
            to_box.fetch_add(n as u64, Ordering::Relaxed);
            remaining -= n as u64;
        }
    }

    let mut up_r = up_r;

    // An informational response is a head that is not *the* response: `100
    // Continue` when the client said `Expect: 100-continue` (curl does, for a
    // body over a kilobyte), `103 Early Hints` from a server that sends them.
    // Each is relayed exactly as it came — rewriting `Connection` on a `100`
    // would be telling the client the connection is over before the answer has
    // started — and then the real head is read behind it.
    let mut rest = Vec::new();
    let mut interim = 0;
    // One deadline for the whole head phase, not one per head. `read_response`
    // computes its own from whenever it is called, so a box that dribbles a
    // `103` just inside the timeout used to multiply the budget by the number
    // of interim heads allowed — while holding one of the share's connection
    // slots the entire time.
    let head_deadline = tokio::time::Instant::now() + RESPONSE_HEAD_TIMEOUT;
    let (head, complete) = loop {
        let (head, complete) = read_response(&mut up_r, &mut rest, head_deadline).await;
        if !(complete && is_informational(&head)) {
            break (head, complete);
        }
        // Relayed as it came *except* for the share-cookie filter: an interim
        // head must keep its `Connection` (the answer has not started) but it
        // is still a place the box could set a cookie it has no business
        // setting.
        let cleaned = strip_share_cookies(&head);
        peer_w.write_all(&cleaned).await?;
        to_peer.fetch_add(cleaned.len() as u64, Ordering::Relaxed);
        // Bounded. A box streaming interim heads forever is not a thing a real
        // server does, and it is a thing agent-written code can do by accident.
        interim += 1;
        if interim >= MAX_INTERIM_RESPONSES {
            let _ = peer_w.shutdown().await;
            return Ok(());
        }
    };

    if req.upgrade {
        // Believe the box, not the client. Only a `101` earns a two-way pipe.
        if head.is_empty() && !complete {
            return Ok(());
        }
        let switched = complete && is_switching(&head);
        if switched {
            peer_w.write_all(&head).await?;
            if !rest.is_empty() {
                peer_w.write_all(&rest).await?;
            }
            to_peer.fetch_add((head.len() + rest.len()) as u64, Ordering::Relaxed);
            // Whatever the client sent in the same read as its handshake. An
            // upgrade has no `Content-Length`, so the body loop above forwarded
            // none of it, and a client that speaks first would have had its
            // opening frames silently dropped.
            if from_rest < rest_from_client.len() {
                let tail = &rest_from_client[from_rest..];
                up_w.write_all(tail).await?;
                to_box.fetch_add(tail.len() as u64, Ordering::Relaxed);
            }
            crate::pump::duplex(peer_r, peer_w, up_r, up_w, to_box, to_peer).await;
            return Ok(());
        }
    }

    let bodyless = has_no_body(&req.method, &head);
    relay_response(peer_r, peer_w, up_r, head, rest, complete, bodyless, to_peer).await
}

/// A `101`: the box agreeing to change protocols.
///
/// One predicate, because there were two and they disagreed about `HTTP/1.0
/// 101` — the upgrade check accepted it and the interim check called it an
/// interim head, so a box answering that way had its response relayed as an
/// interim and the proxy then waited two minutes for a "real" one.
fn is_switching(head: &[u8]) -> bool {
    status_code(head) == Some(101)
}

/// A `1xx` head that is *not* a `101`: an interim answer, with the real one
/// still to come.
fn is_informational(head: &[u8]) -> bool {
    status_code(head).is_some_and(|c| (100..200).contains(&c)) && !is_switching(head)
}

/// The status code from a response head.
fn status_code(head: &[u8]) -> Option<u16> {
    let line = head.split(|&b| b == b'\r').next()?;
    let code = line.split(|&b| b == b' ').nth(1)?;
    std::str::from_utf8(code).ok()?.parse().ok()
}

/// Does this response carry a body at all?
///
/// Three cases where a `Content-Length` is present and describes a body that is
/// never sent, and waiting for it would hang the connection until an idle
/// timeout: the answer to a `HEAD`, a `204 No Content`, and a `304 Not
/// Modified`. The last is not exotic — it is what a dev server answers for
/// every asset a browser already has.
fn has_no_body(method: &str, head: &[u8]) -> bool {
    if method.eq_ignore_ascii_case("HEAD") {
        return true;
    }
    matches!(status_code(head), Some(204) | Some(304))
}

/// Forward a chunked request body, verbatim, and stop at its end.
///
/// The point is not to understand the body — it is passed through byte for
/// byte — but to know where it stops, so that what follows on the connection is
/// the pipelined request this proxy exists to drop rather than something it
/// forwards by accident.
///
/// Everything is bounded before it is believed: the size line, the trailer
/// section, and the whole body's wall clock. A chunk *size* is not bounded,
/// because a legitimate upload is as big as it is, and the peer sending it is
/// one the grant table already admitted.
async fn forward_chunked<R, W>(
    r: &mut R,
    w: &mut W,
    initial: &[u8],
    to_box: &std::sync::atomic::AtomicU64,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    use std::sync::atomic::Ordering;
    let mut buf = initial.to_vec();
    let deadline = tokio::time::Instant::now() + UNFRAMED_LIFETIME;
    let mut trailer_bytes = 0usize;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::other("chunked body took too long"));
        }
        let line = read_crlf_line(r, &mut buf, MAX_CHUNK_LINE).await?;
        let size = chunk_size(&line)
            .ok_or_else(|| std::io::Error::other("not a chunk size"))?;
        w.write_all(&line).await?;
        to_box.fetch_add(line.len() as u64, Ordering::Relaxed);

        if size == 0 {
            // The terminating chunk, then any trailers, then a blank line.
            loop {
                let t = read_crlf_line(r, &mut buf, MAX_CHUNK_LINE).await?;
                w.write_all(&t).await?;
                to_box.fetch_add(t.len() as u64, Ordering::Relaxed);
                if t.as_slice() == b"\r\n" {
                    return Ok(());
                }
                trailer_bytes += t.len();
                if trailer_bytes > MAX_TRAILERS {
                    return Err(std::io::Error::other("trailers too long"));
                }
            }
        }

        // The data, plus the CRLF that closes it.
        //
        // `checked_add` because `size` came off the wire as hex and `u64::MAX`
        // is a perfectly well-formed chunk header: `size + 2` would panic in a
        // debug build and wrap to one in a release one, which would forward two
        // bytes and then read the rest of the peer's body as chunk headers.
        let Some(mut left) = size.checked_add(2) else {
            return Err(std::io::Error::other("chunk size out of range"));
        };
        while left > 0 {
            if buf.is_empty() {
                fill(r, &mut buf).await?;
            }
            let take = (left as usize).min(buf.len());
            w.write_all(&buf[..take]).await?;
            to_box.fetch_add(take as u64, Ordering::Relaxed);
            buf.drain(..take);
            left -= take as u64;
        }
    }
}

/// One CRLF-terminated line, from the buffer and then from the reader.
async fn read_crlf_line<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    loop {
        if let Some(i) = buf.windows(2).position(|w| w == b"\r\n") {
            let line = buf.drain(..i + 2).collect();
            return Ok(line);
        }
        if buf.len() > cap {
            return Err(std::io::Error::other("line too long"));
        }
        fill(r, buf).await?;
    }
}

async fn fill<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<()> {
    let mut chunk = [0u8; 8192];
    let n = tokio::time::timeout(BODY_IDLE, r.read(&mut chunk))
        .await
        .map_err(|_| std::io::Error::other("the peer stopped mid-body"))??;
    if n == 0 {
        return Err(std::io::Error::other("the peer hung up mid-body"));
    }
    buf.extend_from_slice(&chunk[..n]);
    Ok(())
}

/// The size from a chunk header line, ignoring any extension after `;`.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let line = line.strip_suffix(b"\r\n")?;
    let hex = line.split(|&b| b == b';').next()?;
    let hex = std::str::from_utf8(hex).ok()?.trim();
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// Read the box's response head.
///
/// Byte-oriented, unlike the request reader, and that is not fussiness: a
/// response head is allowed to carry bytes that are not UTF-8 (a `filename=`
/// in a legacy encoding is the ordinary case), and a reader that gave up on
/// those would delete a response the box had already produced.
///
/// Returns what was read either way. `complete` says whether the blank line
/// arrived; when it did not — a head past the cap, a box that stopped talking —
/// the caller relays the bytes verbatim rather than discarding them, because a
/// truncated response is something a person can debug and a silently empty one
/// is not.
async fn read_response<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    pending: &mut Vec<u8>,
    deadline: tokio::time::Instant,
) -> (Vec<u8>, bool) {
    // `pending` carries whatever the last read went past. An interim `100
    // Continue` and the real response often arrive in one packet, and without
    // a carry-over the second head would be read as the first one's body.
    let mut buf = std::mem::take(pending);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = find_head_end(&buf) {
            *pending = buf.split_off(end);
            return (buf, true);
        }
        if buf.len() > MAX_RESPONSE_HEAD {
            return (buf, false);
        }
        let n = match tokio::time::timeout_at(deadline, r.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            _ => return (buf, false),
        };
        if n == 0 {
            return (buf, false);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Relay one response and close.
///
/// The head is rewritten to say `Connection: close`, and that is a correctness
/// fix rather than tidiness. The proxy stops reading the client after one
/// request; a response that told the client "keep this connection, send me
/// another" produced exactly that — a connection the client believed reusable
/// and that answered nothing, which on the tunnel path is an intermittent hang
/// and a `502` for every POST the client will not retry.
///
/// The body is framed by `Content-Length` when there is one, so the connection
/// ends when the response does rather than waiting on a box that may never hang
/// up. Without one it is relayed until the box closes or goes quiet for a long
/// time, which is the best that can be done without parsing chunked encoding.
#[allow(clippy::too_many_arguments)]
async fn relay_response<PR, PW, UR>(
    mut peer_r: PR,
    mut peer_w: PW,
    mut up_r: UR,
    head: Vec<u8>,
    rest: Vec<u8>,
    complete: bool,
    bodyless: bool,
    to_peer: &std::sync::atomic::AtomicU64,
) -> std::io::Result<()>
where
    PR: tokio::io::AsyncRead + Unpin + Send,
    PW: tokio::io::AsyncWrite + Unpin + Send,
    UR: tokio::io::AsyncRead + Unpin + Send,
{
    use std::sync::atomic::Ordering;

    if !complete {
        // A head the box never finished is not a response, and relaying the
        // bytes verbatim — which is what this did, to avoid "silently deleting
        // a response" — hands the visitor a head that skipped every rewrite:
        // no forced `close`, no share-cookie filter, no length sanitising. The
        // box chooses when to stop talking, so that was the whole sanitiser
        // behind a door the box holds. A refusal the visitor can read is the
        // better half of the trade.
        let _ = peer_w
            .write_all(unfinished_response().as_bytes())
            .await;
        let _ = peer_w.shutdown().await;
        return Ok(());
    }

    let (out, body_len) = {
        let framing = response_framing(&head);
        let rewritten = close_the_connection(&head);
        // The head is sanitised on its own terms, and the body length is a
        // separate question. Deciding both at once put "no body" and "framing
        // we refuse to trust" through the same branch, which is how an
        // ambiguous *page* ended up delivered with no content at all.
        let out = match framing {
            // Not passed on. Handing the visitor two `Content-Length` headers
            // is a response-smuggling shape the request side refuses outright,
            // and a client that reads it either errors or picks one — which is
            // the disagreement all over again, one hop further out. Stripped
            // even when the answer carries no body: a `304` is the commonest
            // response a dev server produces, and "this branch has no body so
            // the headers cannot hurt" is how an invariant ends up holding on
            // one path and not the other.
            Framing::Ambiguous => strip_lengths(rewritten),
            _ => rewritten,
        };
        // A `HEAD`, a `204` or a `304` declares a length and sends nothing.
        // Waiting for that body would hold the connection until the idle
        // timeout, and a `304` is what a dev server answers for every asset the
        // browser already has — so this is the common case, not the exotic one.
        let body_len = if bodyless {
            Some(0)
        } else {
            match framing {
                Framing::Length(n) => Some(n),
                // Nothing to frame on, so the connection closing is the
                // framing — which is exactly what the stripped head now says.
                Framing::UntilClose | Framing::Ambiguous => None,
            }
        };
        (out, body_len)
    };
    if !out.is_empty() {
        peer_w.write_all(&out).await?;
        to_peer.fetch_add(out.len() as u64, Ordering::Relaxed);
    }
    let mut sent_body = 0u64;
    if !rest.is_empty() {
        let take = match body_len {
            Some(n) => rest.len().min((n - sent_body) as usize),
            None => rest.len(),
        };
        peer_w.write_all(&rest[..take]).await?;
        to_peer.fetch_add(take as u64, Ordering::Relaxed);
        sent_body += take as u64;
    }

    if body_len == Some(sent_body) {
        let _ = peer_w.shutdown().await;
        return Ok(());
    }

    let mut buf = vec![0u8; 32 * 1024];
    let mut discard = vec![0u8; 8 * 1024];
    // An absolute cap as well as an idle one. The idle timeout is rebuilt every
    // time round the loop, including when the *client* says something — so a
    // peer dribbling a byte a minute could hold an unframed response open for
    // as long as it liked, which is the ceiling this share has instead of a
    // per-peer limit.
    let hard_deadline = tokio::time::Instant::now() + UNFRAMED_LIFETIME;
    loop {
        // Applied whether or not the response declared a length. A box
        // answering `Content-Length: 1000000000` and sending one byte every
        // four minutes resets the idle timer forever, and "framed responses end
        // when their body does" is a promise agent-written code never made.
        if tokio::time::Instant::now() >= hard_deadline {
            break;
        }
        tokio::select! {
            from_box = tokio::time::timeout(RESPONSE_IDLE, up_r.read(&mut buf)) => {
                let n = match from_box {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(n)) => n,
                };
                let take = match body_len {
                    Some(total) => n.min((total - sent_body) as usize),
                    None => n,
                };
                if peer_w.write_all(&buf[..take]).await.is_err() {
                    break;
                }
                to_peer.fetch_add(take as u64, Ordering::Relaxed);
                sent_body += take as u64;
                if body_len == Some(sent_body) {
                    break;
                }
            }
            // The client's side is still read, and everything it says is
            // dropped. Two reasons, and the first is why this is not simply
            // "stop reading": it is how a client hanging up is noticed, and
            // without it a connection where both ends went quiet would hold one
            // of the share's slots until the grant expired. Discarding is as
            // strong a guarantee as not reading — what matters is that no byte
            // the client sends after its first request reaches the box.
            from_peer = peer_r.read(&mut discard) => {
                if matches!(from_peer, Ok(0) | Err(_)) {
                    break;
                }
            }
        }
    }
    let _ = peer_w.shutdown().await;
    Ok(())
}

/// The response head with its connection-lifetime headers replaced by one that
/// says the connection ends here.
fn close_the_connection(head: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len() + 20);
    let mut first = true;
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if first {
            first = false;
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            continue;
        }
        let name = line.split(|&b| b == b':').next().unwrap_or(b"");
        let name = String::from_utf8_lossy(name).trim().to_ascii_lowercase();
        if name == "connection" || name == "keep-alive" {
            continue;
        }
        if sets_a_share_cookie(line) {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out
}

/// How a response's body is framed, as far as this proxy will commit to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// Exactly this many bytes.
    Length(u64),
    /// Until the box closes. `Transfer-Encoding` says so, and this proxy does
    /// not parse chunks — it relays them, and the forced `Connection: close`
    /// is what ends the response.
    UntilClose,
    /// The head said two contradictory things. Nothing is framed **and** the
    /// contradiction is not passed on: `strip_lengths` takes the lengths out on
    /// the way to the client, so the response the visitor gets is framed by the
    /// connection closing and by nothing else.
    Ambiguous,
}

/// Read the response's framing out of its head.
///
/// `Transfer-Encoding` beats `Content-Length` for every HTTP client, so framing
/// on the length when both are present sends the client a prefix of the *chunk
/// framing* and closes — a truncated stream that reads as the app being broken.
/// The request side has refused that combination since round three; the
/// response side was still trusting it.
fn response_framing(head: &[u8]) -> Framing {
    let mut length = None;
    let mut lengths = 0;
    let mut chunked = false;
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = String::from_utf8_lossy(&line[..colon]).trim().to_ascii_lowercase();
        let value = String::from_utf8_lossy(&line[colon + 1..]).trim().to_string();
        match name.as_str() {
            // Only `chunked` frames a body. `identity` is legal, deprecated,
            // and means "no encoding" — grading it as chunked stripped a
            // perfectly good `Content-Length`.
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                chunked = true;
            }
            "content-length" => {
                lengths += 1;
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    lengths += 1; // unparseable counts as a disagreement
                } else {
                    length = value.parse::<u64>().ok();
                }
            }
            _ => {}
        }
    }
    if chunked {
        return if lengths > 0 {
            Framing::Ambiguous
        } else {
            Framing::UntilClose
        };
    }
    match (lengths, length) {
        (1, Some(n)) => Framing::Length(n),
        (0, _) => Framing::UntilClose,
        _ => Framing::Ambiguous,
    }
}

/// What a visitor gets when the box starts a response and never finishes it.
const UNFINISHED_BODY: &str =
    "The app in this box began a reply and did not finish it. Whoever shared it needs to look.";

/// Built from the body, because a hand-counted length is a truncated page.
fn unfinished_response() -> String {
    format!(
        "HTTP/1.1 502 Bad Gateway\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{UNFINISHED_BODY}",
        UNFINISHED_BODY.len()
    )
}

/// Drop every `Set-Cookie` that sets a share cookie, and nothing else.
fn strip_share_cookies(head: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len());
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if sets_a_share_cookie(line) {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Is this header line a `Set-Cookie` for one of ours?
///
/// The box cannot guess a token, so the worst it could do is clear one — but
/// cookies ignore the port, so on the joining side one box could log a visitor
/// out of a *different* share. Nothing legitimate sets a cookie by this name.
fn sets_a_share_cookie(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return false;
    };
    if !String::from_utf8_lossy(&line[..colon])
        .trim()
        .eq_ignore_ascii_case("set-cookie")
    {
        return false;
    }
    String::from_utf8_lossy(&line[colon + 1..])
        .trim_start()
        .starts_with(crate::gate::COOKIE)
}

/// Drop every `Content-Length` from a head whose framing we refused to trust.
fn strip_lengths(head: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len());
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let name = line.split(|&b| b == b':').next().unwrap_or(b"");
        if String::from_utf8_lossy(name).trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod response_tests {
    use super::*;

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(5)
    }

    #[tokio::test]
    async fn a_chunked_body_is_forwarded_and_stops_where_it_ends() {
        // `cloudflared` bridges HTTP/2 to the box's HTTP/1.1 and has no length
        // to carry over, so it chunks every request with a body. Refusing
        // chunked meant every POST through a tunnel share answered `501`.
        let body = b"5\r\nhello\r\n3\r\n123\r\n0\r\n\r\nGET /SMUGGLED HTTP/1.1\r\n\r\n";
        let mut peer = &body[..];
        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        forward_chunked(&mut peer, &mut out, &[], &counted)
            .await
            .expect("a well-formed chunked body");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("123"), "{text}");
        assert!(text.ends_with("0\r\n\r\n"), "{text}");
        // And it stopped: what followed the terminating chunk is a pipelined
        // request, which is the thing this proxy exists to not forward.
        assert!(!text.contains("SMUGGLED"), "a pipelined request was forwarded: {text}");
    }

    #[tokio::test]
    async fn chunk_framing_that_is_not_framing_is_refused() {
        for body in [
            &b"zz\r\nhello\r\n"[..],
            &b"5\r\nhel"[..],                    // hung up mid-chunk
            &b"0\r\n"[..],                       // no closing blank line
        ] {
            let mut peer = body;
            let mut out: Vec<u8> = Vec::new();
            let counted = std::sync::atomic::AtomicU64::new(0);
            assert!(
                forward_chunked(&mut peer, &mut out, &[], &counted).await.is_err(),
                "accepted {body:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_chunk_size_at_the_edge_of_the_type_is_refused() {
        // `u64::MAX` is a well-formed chunk header, and `size + 2` would panic
        // in a debug build and wrap to one in a release build — forwarding two
        // bytes and then reading the peer's body as chunk headers.
        let body = b"ffffffffffffffff\r\n";
        let mut peer = &body[..];
        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        assert!(forward_chunked(&mut peer, &mut out, &[], &counted).await.is_err());
    }

    #[test]
    fn a_chunk_size_is_read_the_way_the_spec_writes_one() {
        assert_eq!(chunk_size(b"5\r\n"), Some(5));
        assert_eq!(chunk_size(b"1f\r\n"), Some(31));
        assert_eq!(chunk_size(b"0\r\n"), Some(0));
        // Extensions after `;` are ignored, not refused.
        assert_eq!(chunk_size(b"a;name=value\r\n"), Some(10));
        assert_eq!(chunk_size(b"\r\n"), None);
        assert_eq!(chunk_size(b"-5\r\n"), None);
        assert_eq!(chunk_size(b"5"), None);
    }

    #[test]
    fn a_bodyless_answer_is_not_waited_on() {
        // A `304` is what a dev server answers for every asset the browser
        // already has, and it declares a length it never sends. Waiting for
        // that body holds the connection until the idle timeout — on the
        // commonest response there is.
        assert!(has_no_body("GET", b"HTTP/1.1 304 Not Modified\r\nContent-Length: 42\r\n\r\n"));
        assert!(has_no_body("GET", b"HTTP/1.1 204 No Content\r\n\r\n"));
        assert!(has_no_body("HEAD", b"HTTP/1.1 200 OK\r\nContent-Length: 9000\r\n\r\n"));
        assert!(has_no_body("head", b"HTTP/1.1 200 OK\r\n\r\n"));
        assert!(!has_no_body("GET", b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n"));
    }

    #[test]
    fn a_head_the_box_never_finished_is_not_relayed_raw() {
        // Relaying an unfinished head verbatim put the whole sanitiser behind a
        // door the box holds: stop talking mid-head and `Connection`, the
        // share-cookie filter and the length checks are all skipped.
        let r = unfinished_response();
        let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.starts_with("HTTP/1.1 502 "), "{r}");
        assert!(head.contains(&format!("Content-Length: {}", body.len())), "{r}");
        assert!(head.contains("Connection: close"), "{r}");
    }

    #[test]
    fn an_interim_head_cannot_set_a_share_cookie_either() {
        let head = b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\n\
                     Set-Cookie: h5i_share_43821=x; Max-Age=0\r\n\r\n";
        let text = String::from_utf8(strip_share_cookies(head)).expect("utf8");
        assert!(!text.contains("h5i_share"), "{text}");
        assert!(text.contains("Link: </a.css>"), "{text}");
        // And it keeps its `Connection`: the answer has not started yet.
        assert!(!text.contains("Connection: close"), "{text}");
    }

    #[test]
    fn a_bodyless_answer_with_ambiguous_lengths_is_still_sanitised() {
        // A `304` is the commonest response a dev server makes, and "this
        // branch has no body so the headers cannot hurt" is how an invariant
        // ends up holding on one path and not the other.
        let head = b"HTTP/1.1 304 Not Modified\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n";
        assert_eq!(response_framing(head), Framing::Ambiguous);
        let text = String::from_utf8(strip_lengths(close_the_connection(head))).expect("utf8");
        assert!(!text.to_lowercase().contains("content-length"), "{text}");
    }

    #[test]
    fn identity_transfer_encoding_is_not_chunked() {
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\nContent-Length: 4\r\n\r\n"
            ),
            Framing::Length(4)
        );
    }

    #[test]
    fn an_interim_answer_is_told_apart_from_the_real_one() {
        assert!(is_informational(b"HTTP/1.1 100 Continue\r\n\r\n"));
        assert!(is_informational(b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>\r\n\r\n"));
        // A `101` is an upgrade, which is a different thing entirely.
        assert!(!is_informational(b"HTTP/1.1 101 Switching Protocols\r\n\r\n"));
        assert!(!is_informational(b"HTTP/1.1 200 OK\r\n\r\n"));
        assert!(!is_informational(b"garbage"));
    }

    #[test]
    fn the_connection_headers_are_replaced_and_nothing_else_is() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\
                     Keep-Alive: timeout=60\r\nX-Connection-Id: abc\r\n\r\n";
        let out = close_the_connection(head);
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("Connection: close"), "{text}");
        assert!(!text.contains("keep-alive"), "{text}");
        assert!(!text.contains("timeout=60"), "{text}");
        // A header that merely has "connection" in its *name* is not ours.
        assert!(text.contains("X-Connection-Id: abc"), "{text}");
        assert!(text.contains("Content-Length: 2"), "{text}");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\n"), "{text}");
    }

    #[test]
    fn a_length_is_read_only_when_it_is_unambiguous() {
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n"),
            Framing::Length(17)
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Framing::UntilClose
        );
        assert_eq!(response_framing(b"HTTP/1.1 200 OK\r\n\r\n"), Framing::UntilClose);
        // A length beside a `Transfer-Encoding` is the shape that truncated a
        // chunked body to five bytes of its own framing: every client honours
        // the encoding over the length, and this proxy honoured the length.
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            Framing::Ambiguous
        );
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"
            ),
            Framing::Ambiguous
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\n"),
            Framing::Ambiguous
        );
    }

    #[test]
    fn an_ambiguous_length_is_not_passed_on_to_the_visitor() {
        // Two `Content-Length` headers is a response-smuggling shape the
        // request side refuses outright. Relaying it to the browser is the same
        // disagreement, one hop further out.
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\nX-A: b\r\n\r\n";
        let out = strip_lengths(close_the_connection(head));
        let text = String::from_utf8(out).expect("utf8");
        assert!(!text.to_lowercase().contains("content-length"), "{text}");
        assert!(text.contains("X-A: b"), "{text}");
        assert!(text.contains("Connection: close"), "{text}");
    }

    #[test]
    fn the_box_cannot_clobber_the_visitors_share_cookie() {
        // It cannot guess a token, so the worst it could do is clear one — but
        // cookies ignore the port, so on the joining side one box could log a
        // visitor out of a different share.
        let head = b"HTTP/1.1 200 OK\r\nSet-Cookie: h5i_share_1234=junk; Path=/\r\n\
                     Set-Cookie: sid=9\r\n\r\n";
        let text = String::from_utf8(close_the_connection(head)).expect("utf8");
        assert!(!text.contains("h5i_share"), "{text}");
        assert!(text.contains("Set-Cookie: sid=9"), "{text}");
    }

    #[tokio::test]
    async fn an_interim_head_arriving_with_the_real_one_is_not_read_as_its_body() {
        // `100 Continue` and the answer behind it usually share a packet. Without
        // a carry-over buffer the second head is read as the first one's body,
        // and the client is handed a `100` with the real response glued to it.
        let raw: Vec<u8> =
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_vec();
        let mut r = &raw[..];
        let mut rest = Vec::new();
        let (first, complete) = read_response(&mut r, &mut rest, soon()).await;
        assert!(complete && is_informational(&first));
        let (second, complete) = read_response(&mut r, &mut rest, soon()).await;
        assert!(complete);
        assert!(second.starts_with(b"HTTP/1.1 200 OK"));
        assert_eq!(rest, b"hi");
    }

    #[tokio::test]
    async fn a_response_head_that_is_not_utf8_is_relayed_rather_than_deleted() {
        // A `filename=` in a legacy encoding is ordinary, and a reader that
        // gave up on it used to discard a response the box had already made.
        let raw: Vec<u8> = b"HTTP/1.1 200 OK\r\nContent-Disposition: attachment; filename=\"caf\xe9.txt\"\r\nContent-Length: 2\r\n\r\nhi".to_vec();
        let mut r = &raw[..];
        let mut rest = Vec::new();
        let (head, complete) = read_response(&mut r, &mut rest, soon()).await;
        assert!(complete);
        assert_eq!(rest, b"hi");
        assert!(head.windows(3).any(|w| w == b"caf"));
        assert_eq!(response_framing(&head), Framing::Length(2));
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
        let Next::Respond(r) = decide(head, gate::COOKIE, yes, true) else {
            panic!("a token in the URL must be redirected, not proxied");
        };
        assert!(r.contains("302"));
        assert!(r.contains("Location: /dash?tab=2"));
        assert!(r.contains("Set-Cookie: h5i_share=abc123"));
    }

    #[test]
    fn a_request_with_the_cookie_is_proxied_without_it() {
        let head = "GET /app.js HTTP/1.1\r\nHost: x\r\nCookie: h5i_share=abc123; sid=9\r\n\r\n";
        let Next::Proxy { head: up, req } = decide(head, gate::COOKIE, yes, true) else {
            panic!("an authorized request must be proxied");
        };
        assert!(!up.contains("abc123"), "credential reached the box: {up}");
        assert!(up.contains("Cookie: sid=9"));
        assert!(!req.upgrade);
    }

    #[test]
    fn no_credential_and_a_wrong_credential_get_the_same_answer() {
        let none = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let wrong = "GET / HTTP/1.1\r\nHost: x\r\nCookie: h5i_share=nope\r\n\r\n";
        let (Next::Respond(a), Next::Respond(b)) = (decide(none, gate::COOKIE, yes, true), decide(wrong, gate::COOKIE, yes, true))
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
                    Connection: Upgrade\r\nCookie: h5i_share=abc123\r\n\r\n";
        let Next::Proxy { req, .. } = decide(head, gate::COOKIE, yes, true) else {
            panic!("an upgrade must be proxied");
        };
        assert!(req.upgrade);
    }

    #[test]
    fn something_that_is_not_http_never_reaches_the_authorizer() {
        // The gate is the first thing an anonymous connection touches, so it
        // has to refuse without consulting anything.
        let mut called = false;
        let out = decide(
            "\u{16}\u{3}\u{1} a TLS ClientHello, not a request",
            gate::COOKIE,
            |_| {
                called = true;
                true
            },
            true,
        );
        assert!(matches!(out, Next::Respond(_)));
        assert!(!called, "malformed input must not reach the grant table");
    }

    #[test]
    fn loopback_gets_a_cookie_a_loopback_browser_will_actually_store() {
        let head = "GET /?h5i=abc123 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let Next::Respond(r) = decide(head, gate::COOKIE, yes, false) else {
            panic!("redirect expected");
        };
        assert!(!r.contains("Secure"));
    }
}
