//! The loopback HTTP front that both browser-facing sides of a share use.
//!
//! Two places serve HTTP to a browser and are otherwise nothing alike: the
//! sharer's quick tunnel ([`crate::tunnel`]), whose visitor is on the open
//! internet behind Cloudflare, and the joiner's own proxy ([`crate::join`]),
//! whose visitor is a tab on the same machine. What they share is the part that
//! must not be written twice: read a bounded head, find the credential, bounce
//! the first request so the token leaves the URL, and never pass the credential
//! upstream.
//!
//! The joiner's side needs this as much as the sharer's, which is the
//! non-obvious half: a proxy bound on the joiner's loopback is reachable by
//! every process on their machine and every page they have open. Same problem
//! as the viewer forward, arriving on somebody else's computer, so it gets the
//! same answer: a token, and a refusal without it.

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

/// A response head past this is relayed as-is rather than parsed. Generous,
/// a real one is a few kilobytes, and its only job is to stop an unbounded
/// buffer.
const MAX_RESPONSE_HEAD: usize = 256 * 1024;

/// How long a response with no declared length may go silent before the
/// connection is closed. Long enough for a server-sent-events stream that
/// sends a heartbeat; short enough that a forgotten connection does not hold a
/// share slot for the life of the grant.
const RESPONSE_IDLE: Duration = Duration::from_secs(300);

/// How long a peer may pause part-way through a body it declared.
const BODY_IDLE: Duration = Duration::from_secs(30);

/// How long a single write may make no progress before the connection is
/// abandoned.
///
/// Every deadline in this file used to guard a *read*, which left the whole set
/// unreachable: a visitor who stops reading (a paused download, a backgrounded
/// tab, a deliberately zero receive window) parks `write_all` forever, and a
/// loop parked in a write never reaches the loop-top check meant to bound it.
/// One such request holds a share slot and a socket into the box for the life
/// of the ticket; sixty-four take the share down and make the receipt blame the
/// honest visitors.
///
/// Generous, because a slow link is not a hostile one: TCP unblocks the moment
/// the far side reads anything.
const WRITE_STALL: Duration = Duration::from_secs(120);

/// The longest a chunk-size line or a single trailer may be.
const MAX_CHUNK_LINE: usize = 1024;
/// The longest a whole trailer section may be.
const MAX_TRAILERS: usize = 8 * 1024;

/// The longest an unframed response may stay open, whatever either end does.
///
/// A server-sent-events stream is the legitimate case and an hour is generous
/// for a share whose ticket lasts one by default. It applies to framed
/// responses too. "They end when their body does" is a promise agent-written
/// code never made, and a box dribbling a byte a minute would otherwise reset
/// the idle timer for as long as it liked.
const UNFRAMED_LIFETIME: Duration = Duration::from_secs(3600);

/// The longest a response with a declared length may take.
///
/// Longer than the unframed cap because the two bound different risks. An
/// unframed response is open-ended by construction and an hour is generous for
/// it. A framed one is bounded in *bytes* already, and an hour of it is a
/// perfectly ordinary large download on a slow link. A hundred megabytes over
/// a hundred kilobits is over two hours. Cutting that off at an hour truncated
/// it silently, which is the shape of failure a visitor cannot debug.
const FRAMED_LIFETIME: Duration = Duration::from_secs(6 * 3600);

/// The longest a request body may take to arrive, whatever the peer does.
///
/// Its own constant rather than the response's, because it answers a different
/// question: not "how long may a stream stay quiet" but "how long may somebody
/// take to finish saying what they said they would". An hour is far past any
/// upload a share is for, and without it a peer declaring a body of
/// `u64::MAX` and dribbling a byte a minute holds a slot for the life of its
/// ticket.
const BODY_LIFETIME: Duration = Duration::from_secs(3600);

/// How many `1xx` heads may precede the real response. A server sends at most
/// one or two; this is a backstop, not a budget.
const MAX_INTERIM_RESPONSES: usize = 8;

/// What to do with a connection once its head has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Write these bytes and close: the redirect that sets the cookie, or a
    /// refusal.
    ///
    /// The second field is the refusal's own reason, when there is one; the
    /// redirect has none. Carried rather than recovered, because the caller was
    /// recovering it by testing the rendered bytes for `HTTP/1.1 400`, which
    /// reached the `400`s and left both `403`s counted nowhere. Those are the gate
    /// refusing a foreign-origin browser request carrying the share cookie, and
    /// refusing a service worker registration that would keep control of the
    /// joiner's loopback origin after the share ends.
    Respond(String, Option<gate::Refusal>),
    /// Authorized: open the upstream, send this head, then move bytes.
    Proxy {
        /// The head as it should reach the box: share credential removed.
        head: String,
        /// The parsed request, which is what [`proxy_one`] needs to know where
        /// this request ends.
        req: gate::Request,
    },
}

/// Why there is no head to work with.
///
/// Two answers, not one, because they are different facts and only one belongs
/// in an ingress receipt. The head reader refuses several shapes before
/// [`gate::parse`] sees them (a header block past [`gate::MAX_HEAD`], bytes
/// that are not UTF-8, with a TLS `ClientHello` sent to a plaintext front the
/// everyday case, an incomplete head the peer stops feeding) and the caller had
/// no way to tell those from a peer that opened a socket and said nothing. So
/// it dropped all of them silently, and the largest smuggling-shaped probe a
/// share can be sent was the one thing its receipt could not mention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoHead {
    /// Nothing arrived, or the peer went away before saying anything. A
    /// browser's speculative preconnect is exactly this shape and so is every
    /// port scan, so it is not worth a line. Recording it would bury the rows
    /// that mean something under noise anybody can generate.
    Silent,
    /// Bytes arrived and they are not a head this share will read. Worth
    /// recording: it is the same fact as a head the gate refuses, arriving one
    /// step earlier.
    Refused,
}

/// Read up to the end of the headers.
///
/// Returns the head and whatever arrived after it. A request body usually
/// follows in the same packet, and a proxy that read it and threw it away would
/// break every form on the shared app.
pub async fn read_head<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
) -> Result<(String, Vec<u8>), NoHead> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + HEAD_TIMEOUT;
    // "Did this peer ever say anything" is the whole distinction, so it is read
    // off the buffer rather than guessed at from which arm ended the loop.
    let said_something = |buf: &Vec<u8>| {
        if buf.is_empty() {
            NoHead::Silent
        } else {
            NoHead::Refused
        }
    };
    loop {
        let n = match tokio::time::timeout_at(deadline, r.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            // A deadline or a read error part-way through a head is a peer that
            // started a request and did not finish it; before any of it, it is
            // a peer that never made one.
            _ => return Err(said_something(&buf)),
        };
        if n == 0 {
            return Err(said_something(&buf));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_head_end(&buf) {
            return match String::from_utf8(buf[..end].to_vec()) {
                Ok(head) => Ok((head, buf[end..].to_vec())),
                // A complete head that is not text. `Refused` unconditionally:
                // it arrived in full, so there is nothing silent about it.
                Err(_) => Err(NoHead::Refused),
            };
        }
        // Bounded before it is parsed. A peer that sends headers forever must
        // not be able to make this allocate forever.
        if buf.len() > gate::MAX_HEAD {
            return Err(NoHead::Refused);
        }
    }
}

/// Write, or give up if nothing moves for [`WRITE_STALL`].
///
/// The error is `TimedOut` so a caller can tell "this peer stopped listening"
/// from "this peer sent nonsense", which are different answers to give.
pub async fn write_timed<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    buf: &[u8],
) -> std::io::Result<()> {
    match tokio::time::timeout(WRITE_STALL, w.write_all(buf)).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the far side stopped reading",
        )),
    }
}

/// Write to the box, turning a stall into an error that names the right side.
///
/// A stalled write is `TimedOut` whichever direction it went, and the two
/// directions deserve opposite answers: a peer that stopped reading gets
/// nothing more, a *box* that stopped reading is a `502`. The peer did
/// nothing wrong and telling them their upload was too slow sends them to fix
/// something that is not broken.
async fn write_to_box<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    buf: &[u8],
) -> std::io::Result<()> {
    write_timed(w, buf).await.map_err(|e| {
        std::io::Error::other(BoxWrite {
            stalled: e.kind() == std::io::ErrorKind::TimedOut,
        })
    })
}

/// A write to the box failed. A marker only this module can construct.
///
/// It carries two things, both of which change what the visitor is told. That
/// the failure was on the *box* side at all: an earlier version used
/// `ErrorKind::BrokenPipe`, which the kernel also produces, so a peer's own
/// disconnect could be reported as the box's. And whether the box *stalled*
/// (nothing moved for two minutes, so it is wedged and the visitor gets a
/// `502`) or *closed* (it answered early and hung up, and its answer is already
/// in the socket waiting to be relayed).
#[derive(Debug)]
struct BoxWrite {
    stalled: bool,
}

impl std::fmt::Display for BoxWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.stalled {
            write!(f, "the box stopped reading")
        } else {
            write!(f, "the box closed while the request was being sent")
        }
    }
}

impl std::error::Error for BoxWrite {}

/// `Some(stalled)` when this failure was a write to the box.
fn box_write_failure(e: &std::io::Error) -> Option<bool> {
    e.get_ref()
        .and_then(|r| r.downcast_ref::<BoxWrite>())
        .map(|b| b.stalled)
}

/// Index just past the blank line that ends an HTTP head.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Decide what a request gets.
///
/// `authorize` is the caller's grant check. The bridge's table on the sharer's
/// side, a single local token on the joiner's. It is only ever called with a
/// token that already looks like one, because [`gate::parse`] refuses the rest.
pub fn decide(
    head: &str,
    cookie: &str,
    authorize: impl FnOnce(&str) -> bool,
    secure: bool,
    app: Option<&gate::AppCookies>,
) -> Next {
    let refuse = |r: gate::Refusal| Next::Respond(gate::refusal_response(r), Some(r));
    let Some(req) = gate::parse(head, cookie) else {
        return refuse(gate::Refusal::Malformed);
    };
    // Refused before the credential is even weighed: this is not about who is
    // asking, it is about what a registration would leave behind on the
    // joiner's machine once the share is over.
    if req.service_worker {
        return refuse(gate::Refusal::ServiceWorker);
    }
    // Also before the credential: the cookie is exactly what makes this worth
    // refusing, because the browser attaches it to a request the page had no
    // business making.
    if req.cross_origin {
        return refuse(gate::Refusal::ForeignOrigin);
    }
    let Some(token) = req.token.clone() else {
        return refuse(gate::Refusal::NotAuthorized);
    };
    if !authorize(&token) {
        return refuse(gate::Refusal::NotAuthorized);
    }
    if req.from_query {
        // Authorized, but the token is in the URL. Set the cookie and send the
        // browser to the same page without it, rather than proxying this one: a
        // request that reached the app with the token in its target would put it in
        // the app's logs and in `Referer` on every link out of the page.
        //
        // An upgrade is the one thing that cannot be redirected, since a WebSocket
        // handshake follows no 302, but a browser never opens one before the page
        // that opens it, so by then the cookie is set. Not a refusal.
        return Next::Respond(
            gate::set_cookie_redirect(
                cookie,
                &token,
                &gate::safe_location(&req.clean_target),
                secure,
            ),
            None,
        );
    }
    Next::Proxy {
        head: gate::rewrite_for_upstream(head, &req, cookie, app),
        req,
    }
}

/// The request as it should reach the box, and what it takes to know where it
/// ends.
pub struct Forwarded<'a> {
    /// Rewritten head: share credential removed, `Connection: close` forced.
    pub head: &'a str,
    /// Bytes that arrived in the same read as the head. The start of the body.
    pub rest: &'a [u8],
    pub req: &'a gate::Request,
}

/// Bytes moved, counted as they go so a connection cut short by a revoke still
/// reports what it carried.
#[derive(Debug, Default)]
pub struct Counters {
    pub to_box: std::sync::atomic::AtomicU64,
    pub to_peer: std::sync::atomic::AtomicU64,
    /// Set when the box left a response unfinished. Silent truncation is the
    /// part a visitor cannot debug and the sharer cannot see, so it is carried
    /// back out to the receipt, and a visitor who cancelled a download is
    /// deliberately not counted, because that is not the box's doing.
    pub truncated: std::sync::atomic::AtomicBool,
}

impl Counters {
    pub fn was_truncated(&self) -> bool {
        self.truncated.load(std::sync::atomic::Ordering::Relaxed)
    }

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
/// This is where the one-request rule is enforced. Adding `Connection: close`
/// to the request asks the box to hang up, and the box runs agent-written code
/// that may decline; a proxy that then kept copying would hold an ungated pipe
/// on which a second request, from anyone whose bytes reach this connection,
/// would reach the box without passing the gate.
///
/// So the client's side is *forwarded* for exactly the bytes this request
/// declared and not one more. It is still read after that, and everything it
/// says is thrown away: that is how a client hanging up is noticed, and
/// discarding is as strong a guarantee as not reading.
///
/// The exception is a real upgrade, checked rather than believed: the box has
/// to answer `101` before the connection becomes a two-way pipe. A request that
/// merely *asked* to upgrade and got an ordinary response is a single request.
pub async fn proxy_one<UR, UW>(
    client: tokio::net::TcpStream,
    up_r: UR,
    mut up_w: UW,
    request: Forwarded<'_>,
    counts: &Counters,
    app: Option<&gate::AppCookies>,
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
    // the header is not passed on. The box would otherwise send a second,
    // useless `100` behind the body.
    if req.expects_continue {
        write_timed(&mut peer_w, b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        to_peer.fetch_add(25, Ordering::Relaxed);
    }

    // The box refusing the head at all. Wedged, or gone. There is no answer
    // to go and read, so this is the one write to the box that always ends the
    // request.
    if write_to_box(&mut up_w, head.as_bytes()).await.is_err() {
        return refuse_the_response(
            &mut peer_r,
            &mut peer_w,
            unreachable_box_response(),
            to_peer,
        )
        .await;
    }
    to_box.fetch_add(head.len() as u64, Ordering::Relaxed);

    // The declared body, and only the declared body. Anything past it on this
    // connection is a pipelined second request, and dropping it is the point.
    //
    // Watched while it is forwarded, not after. The box's answer used to be read
    // only once the whole body had gone across, so the one way an early rejection
    // reached the visitor was the box *closing* its read side, which the test
    // server does and a real HTTP server need not. A server that writes `413`,
    // `401` or `417` after the head and leaves its read side open, in front of a
    // client that declared a large body and then paused, left h5i waiting out
    // `BODY_IDLE` and then replacing that already-delivered answer with its own
    // `408`. `Expect: 100-continue` makes it likelier: h5i synthesises the `100`
    // itself and takes the header out, so the box never gets the chance to refuse.
    //
    // So the two run together, and a final answer (anything `>= 200`) stops the
    // body. Interim heads are the response phase's business.
    let mut early = Vec::new();
    let mut up_r = up_r;
    let from_rest;
    if req.chunked {
        // Parsed rather than refused, and the refusal was not a small cost: a browser
        // sends `Content-Length` for a form, but `cloudflared` bridges HTTP/2 to the
        // box's HTTP/1.1 and has no length to carry over, so it chunks *every* request
        // with a body. Refusing chunked meant every POST through a tunnel share
        // answered `501`.
        //
        // Forwarded verbatim, chunk framing and all: the box speaks HTTP and this
        // proxy only needs to know where the request ends.
        let forwarded = tokio::select! {
            r = forward_chunked(&mut peer_r, &mut up_w, rest_from_client, to_box) => Some(r),
            // The box answered before the upload finished. Abandoning the
            // forward mid-chunk truncates what the box receives, which is what
            // it asked for by answering: it has already decided.
            _ = watch_for_final_response(&mut up_r, &mut early) => None,
        };
        if let Some(Err(e)) = forwarded {
            // Answered rather than dropped, and answered with the right thing.
            // "Your request is malformed" and "your upload was too slow" are
            // different sentences, and on a tunnel share, where `cloudflared`
            // chunks every body it forwards, the slow one is the common case,
            // so telling a visitor on hotel wifi that their request is not one
            // this share can serve is the message they would actually get.
            match box_write_failure(&e) {
                // The box closed its read side. It has almost certainly already
                // written the answer that explains why (a `413`, a `401`) so
                // stop sending and go and read it, exactly as the
                // `Content-Length` path does. Answering `400` here told the
                // visitor their request was malformed when it was not, and
                // threw the box's real answer away.
                Some(false) => {}
                Some(true) => {
                    return refuse_the_response(
                        &mut peer_r,
                        &mut peer_w,
                        unreachable_box_response(),
                        to_peer,
                    )
                    .await
                }
                None => {
                    let reply = if e.kind() == std::io::ErrorKind::TimedOut {
                        timed_out_response()
                    } else {
                        gate::refusal_response(gate::Refusal::Malformed)
                    };
                    return refuse_the_response(&mut peer_r, &mut peer_w, reply, to_peer).await;
                }
            }
        }
        from_rest = rest_from_client.len();
    } else {
        let want = req.content_length.unwrap_or(0);
        // Set when the box stopped taking the body and its answer is waiting.
        let mut remaining_override = false;
        // The same wall clock the chunked path has. `Content-Length` is
        // deliberately accepted all the way to `u64::MAX`, so without one an
        // authorized peer declaring a body it never finishes, one byte every
        // twenty-nine seconds, holds a share slot and a socket into the box
        // for the life of its ticket, which is up to a day.
        let body_deadline = tokio::time::Instant::now() + BODY_LIFETIME;
        from_rest = (rest.len() as u64).min(want) as usize;
        if from_rest > 0 {
            // Handled like every other write of the body, rather than dropped:
            // this is the first chunk, so it is the *likeliest* place a box
            // that rejects early closes its read side, and the answer it has
            // already written is the one the visitor should get.
            if let Err(e) = write_to_box(&mut up_w, &rest[..from_rest]).await {
                if box_write_failure(&e) == Some(true) {
                    return refuse_the_response(
                        &mut peer_r,
                        &mut peer_w,
                        unreachable_box_response(),
                        to_peer,
                    )
                    .await;
                }
                remaining_override = true;
            }
            if !remaining_override {
                to_box.fetch_add(from_rest as u64, Ordering::Relaxed);
            }
        }
        let mut remaining = if remaining_override {
            0
        } else {
            want - from_rest as u64
        };
        let mut buf = vec![0u8; 32 * 1024];
        while remaining > 0 {
            if tokio::time::Instant::now() >= body_deadline {
                // Answered rather than dropped. The peer is still there, that
                // is the whole problem, and a closed socket with no reply is
                // the least informative thing to hand somebody whose upload
                // was too slow.
                return refuse_the_response(
                    &mut peer_r,
                    &mut peer_w,
                    timed_out_response(),
                    to_peer,
                )
                .await;
            }
            let take = (remaining as usize).min(buf.len());
            // Deadlined per read. Declaring a megabyte, sending one byte and
            // going quiet used to hold a connection into the box, and one of
            // the share's slots, for as long as the peer felt like it.
            let read = tokio::select! {
                r = tokio::time::timeout(BODY_IDLE, peer_r.read(&mut buf[..take])) => r,
                // The box answered while the client was still uploading. Same
                // ending as the box closing its read side below: stop sending
                // and go and read what it said. Without this arm, a client that
                // paused mid-body had the box's `413` replaced by our `408`
                // thirty seconds later.
                _ = watch_for_final_response(&mut up_r, &mut early) => break,
            };
            let n = match read {
                Ok(r) => r?,
                // The stall a real mobile client actually hits, and it used to
                // be a bare connection reset. The once-an-hour cap above got an
                // answer a round ago and this one did not.
                Err(_) => {
                    return refuse_the_response(
                        &mut peer_r,
                        &mut peer_w,
                        timed_out_response(),
                        to_peer,
                    )
                    .await;
                }
            };
            if n == 0 {
                // The client promised a body and then hung up. Waiting for a
                // response now means waiting for a box that is itself waiting
                // for the rest of a request nobody is going to send.
                return Ok(());
            }
            if let Err(e) = write_to_box(&mut up_w, &buf[..n]).await {
                if box_write_failure(&e) == Some(true) {
                    return refuse_the_response(
                        &mut peer_r,
                        &mut peer_w,
                        unreachable_box_response(),
                        to_peer,
                    )
                    .await;
                }
                // Not a stall: the box closed its read side, which is what a
                // dev server does when it answers before reading the body. A
                // `413` from a size limit, a `401` from auth. Its answer is
                // already in the socket, so stop sending and go and read it
                // rather than inventing one.
                break;
            }
            to_box.fetch_add(n as u64, Ordering::Relaxed);
            remaining -= n as u64;
        }
    }

    // An informational response is a head that is not *the* response: `100
    // Continue` when the client said `Expect: 100-continue`, `103 Early Hints`
    // from a server that sends them. Each is relayed exactly as it came, since
    // rewriting `Connection` on a `100` would tell the client the connection is
    // over before the answer has started, and then the real head is read behind
    // it. Seeded with whatever the early-answer watch already read off the box:
    // those bytes are the front of its response head, and dropping them would
    // leave `read_response` parsing from the middle of one.
    let mut rest = early;
    let mut interim = 0;
    // One deadline for the whole head phase, not one per head. `read_response`
    // computes its own from whenever it is called, so a box that dribbles a
    // `103` just inside the timeout used to multiply the budget by the number
    // of interim heads allowed, while holding one of the share's connection
    // slots the entire time.
    let head_deadline = tokio::time::Instant::now() + RESPONSE_HEAD_TIMEOUT;
    let (head, complete) = loop {
        let (head, complete) = read_response(&mut up_r, &mut rest, head_deadline).await;
        if !(complete && is_informational(&head)) {
            break (head, complete);
        }
        // Held to the same line discipline as the final head. Adding that check
        // to `relay_response` alone left the two paths that relay a head
        // *without* going through it, this one and the `101`, exactly as they
        // were, which is to say the bare CR the fix was written for still
        // walked a `Set-Cookie` past the filter.
        if !head_is_well_formed(&head) {
            return refuse_the_response(
                &mut peer_r,
                &mut peer_w,
                malformed_head_response(),
                to_peer,
            )
            .await;
        }
        // Relayed as it came *except* for the share-cookie filter: an interim
        // head must keep its `Connection` (the answer has not started) but it
        // is still a place the box could set a cookie it has no business
        // setting.
        learn_app_cookies(&head, app);
        let cleaned = strip_share_cookies(&head);
        write_timed(&mut peer_w, &cleaned).await?;
        to_peer.fetch_add(cleaned.len() as u64, Ordering::Relaxed);
        // Bounded. A box streaming interim heads forever is not a thing a real
        // server does, and it is a thing agent-written code can do by accident.
        interim += 1;
        if interim >= MAX_INTERIM_RESPONSES {
            finish_with(&mut peer_r, &mut peer_w).await;
            return Ok(());
        }
    };

    // The head the visitor's browser will act on, whether it becomes a `101` or
    // an ordinary response. Learned here, once, before either path relays it.
    //
    // A cookie set by *this* response is not yet known to a request already in
    // flight beside it, so that request drops it and the next one carries it.
    // Transient, self-correcting, and in the only direction this is allowed to
    // be wrong in.
    learn_app_cookies(&head, app);

    if req.upgrade {
        // Believe the box, not the client. Only a `101` earns a two-way pipe.
        if head.is_empty() && !complete {
            // The box hung up on a handshake without answering it. This used to
            // return with nothing written at all, so the visitor's WebSocket
            // failed with a bare close and no status. The one shape a browser
            // console reports as "connection closed before receiving a
            // handshake response", which says nothing about where the fault is.
            // The same `502` every other silent box gets is at least readable.
            return refuse_the_response(
                &mut peer_r,
                &mut peer_w,
                unreachable_box_response(),
                to_peer,
            )
            .await;
        }
        let switched = complete && is_switching(&head);
        if switched {
            if !head_is_well_formed(&head) {
                return refuse_the_response(
                    &mut peer_r,
                    &mut peer_w,
                    malformed_head_response(),
                    to_peer,
                )
                .await;
            }
            // Filtered like every other response head. This was the one that
            // was not, and it is the only `1xx` a dev server actually sends,
            // so it was strictly more reachable than the `103` case that got
            // the filter first. `Connection` is left alone: an upgrade is where
            // that header means something.
            let cleaned = strip_share_cookies(&head);
            write_timed(&mut peer_w, &cleaned).await?;
            if !rest.is_empty() {
                write_timed(&mut peer_w, &rest).await?;
            }
            to_peer.fetch_add((cleaned.len() + rest.len()) as u64, Ordering::Relaxed);
            // Whatever the client sent in the same read as its handshake. An
            // upgrade has no `Content-Length`, so the body loop above forwarded
            // none of it, and a client that speaks first would have had its
            // opening frames silently dropped.
            if from_rest < rest_from_client.len() {
                let tail = &rest_from_client[from_rest..];
                write_to_box(&mut up_w, tail).await?;
                to_box.fetch_add(tail.len() as u64, Ordering::Relaxed);
            }
            crate::pump::duplex(peer_r, peer_w, up_r, up_w, to_box, to_peer).await;
            return Ok(());
        }
    }

    let bodyless = has_no_body(&req.method, &head);
    relay_response(
        peer_r,
        peer_w,
        up_r,
        head,
        rest,
        complete,
        bodyless,
        to_peer,
        &counts.truncated,
    )
    .await
}

/// A `101`: the box agreeing to change protocols.
///
/// One predicate, because there were two and they disagreed about `HTTP/1.0
/// 101`: the upgrade check accepted it and the interim check called it an
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
///
/// Reads the first *non-empty* line, which is the line [`head_is_well_formed`]
/// accepts as the status line and `rebuild_head` absorbs the blanks before.
/// Taking the bytes up to the first `\r` instead meant that for the one shape
/// both of those deliberately tolerate, a leading CRLF, this returned `None`,
/// and every caller read that as "not that status":
///
/// * an accepted `100 Continue` was not informational, so an interim answer was
///   treated as the final one;
/// * an accepted `101` was not a switch, so the connection never entered the
///   upgraded duplex path and the WebSocket died;
/// * an accepted `204` or `304` was not bodyless, so the relay waited for a
///   body that is never sent until the idle timeout fired, and `304` is what a
///   dev server answers for every asset the browser already has.
fn status_code(head: &[u8]) -> Option<u16> {
    let line = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .find(|l| !l.is_empty())?;
    let code = line.split(|&b| b == b' ').nth(1)?;
    std::str::from_utf8(code).ok()?.parse().ok()
}

/// Does this response carry a body at all?
///
/// Three cases where a `Content-Length` is present and describes a body that is
/// never sent, and waiting for it would hang the connection until an idle
/// timeout: the answer to a `HEAD`, a `204 No Content`, and a `304 Not
/// Modified`. The last is not exotic. It is what a dev server answers for
/// every asset a browser already has.
fn has_no_body(method: &str, head: &[u8]) -> bool {
    if method.eq_ignore_ascii_case("HEAD") {
        return true;
    }
    matches!(status_code(head), Some(204) | Some(304))
}

/// Forward a chunked request body, verbatim, and stop at its end.
///
/// The point is not to understand the body, which is passed through byte for
/// byte, but to know where it stops, so what follows on the connection is the
/// pipelined request this proxy exists to drop rather than something it
/// forwards by accident.
///
/// Everything is bounded before it is believed: the size line, the trailer
/// section, and the whole body's wall clock. A chunk *size* is not bounded,
/// because a legitimate upload is as big as it is and the peer sending it is
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
    forward_chunked_within(r, w, initial, to_box, BODY_LIFETIME).await
}

/// [`forward_chunked`] with the lifetime given, so a test can drive the bound
/// without waiting an hour for it.
async fn forward_chunked_within<R, W>(
    r: &mut R,
    w: &mut W,
    initial: &[u8],
    to_box: &std::sync::atomic::AtomicU64,
    lifetime: Duration,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    use std::sync::atomic::Ordering;
    let mut buf = initial.to_vec();
    let deadline = tokio::time::Instant::now() + lifetime;
    let mut trailer_bytes = 0usize;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "chunked body took too long",
            ));
        }
        let line = read_crlf_line(r, &mut buf, MAX_CHUNK_LINE).await?;
        let size = chunk_size(&line).ok_or_else(|| std::io::Error::other("not a chunk size"))?;
        write_to_box(w, &line).await?;
        to_box.fetch_add(line.len() as u64, Ordering::Relaxed);

        if size == 0 {
            // The terminating chunk, then any trailers, then a blank line.
            loop {
                let t = read_crlf_line(r, &mut buf, MAX_CHUNK_LINE).await?;
                if !is_trailer(&t) {
                    return Err(std::io::Error::other("not a trailer"));
                }
                write_to_box(w, &t).await?;
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

        // The declared data, then the CRLF that closes it. *Checked*, not counted.
        //
        // This used to consume `size + 2` bytes and forward them without looking at
        // the last two, so `1\r\nAXX0\r\n\r\n` was a complete request: one byte of
        // data, `XX` where its CRLF should be, and the `0` chunk read out of the
        // middle of the stream. Every conforming server rejects that, so h5i had
        // declared a request finished at a point nothing else agrees is a boundary,
        // and the chunk framing is the only thing telling this proxy where the request
        // ends. Found by the chunked fuzz, which reads the same stream leniently and
        // could not find an end where this claimed one.
        //
        // No `checked_add` is needed now the two are separate: `size` is consumed as
        // itself, so `u64::MAX` is a body this streams until the peer stops.
        let mut left = size;
        while left > 0 {
            // Inside the loop, not only at the top of the outer one. The
            // `Content-Length` path checks its deadline on every read for
            // exactly this reason, and `BODY_LIFETIME`'s own doc names the
            // attack: "a peer declaring a body of `u64::MAX` and dribbling a
            // byte a minute holds a slot for the life of its ticket". A single
            // chunk header declaring `u64::MAX` never returns to the outer loop,
            // so the check up there could not fire and the bound was one the
            // peer chose whether to be bound by.
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "chunked body took too long",
                ));
            }
            if buf.is_empty() {
                fill(r, &mut buf).await?;
            }
            // `try_from`, not `as`: narrowing first and comparing after makes
            // `take` zero for a `left` past `usize` on a 32-bit target, and the
            // loop then spins forever making no progress.
            let take = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
            write_to_box(w, &buf[..take]).await?;
            to_box.fetch_add(take as u64, Ordering::Relaxed);
            buf.drain(..take);
            left -= take as u64;
        }
        while buf.len() < 2 {
            fill(r, &mut buf).await?;
        }
        if &buf[..2] != b"\r\n" {
            return Err(std::io::Error::other("a chunk not closed by CRLF"));
        }
        write_to_box(w, &buf[..2]).await?;
        to_box.fetch_add(2, Ordering::Relaxed);
        buf.drain(..2);
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
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "the peer stopped mid-body")
        })??;
    if n == 0 {
        return Err(std::io::Error::other("the peer hung up mid-body"));
    }
    buf.extend_from_slice(&chunk[..n]);
    Ok(())
}

/// The size from a chunk header line, ignoring any extension after `;`.
///
/// Held to the same discipline as a header line, for the same reason. The gate
/// refuses a bare LF in the head because it is how a line means one thing to
/// this proxy and another to the box; a chunk-size line is the *only* thing
/// telling this proxy where the request ends, so it is the last place to be
/// relaxed about it. No embedded CR or LF, no controls, no surrounding
/// whitespace (the grammar allows none), and hex that is hex.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let line = line.strip_suffix(b"\r\n")?;
    if !is_clean_line(line) {
        return None;
    }
    let hex = line.split(|&b| b == b';').next()?;
    let hex = std::str::from_utf8(hex).ok()?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// No control characters, and nothing that another parser would read as the
/// end of a line.
fn is_clean_line(line: &[u8]) -> bool {
    line.iter().all(|&b| b >= 0x20 && b != 0x7f || b == b'\t')
}

/// A trailer line: a header, held to the header rules.
fn is_trailer(line: &[u8]) -> bool {
    let Some(line) = line.strip_suffix(b"\r\n") else {
        return false;
    };
    if line.is_empty() {
        return true;
    }
    if !is_clean_line(line) || line.starts_with(b" ") || line.starts_with(b"\t") {
        return false;
    }
    match line.iter().position(|&b| b == b':') {
        Some(colon) => colon > 0 && line[..colon].iter().all(|&b| crate::gate::is_tchar(b)),
        None => false,
    }
}

/// Read the box's response head.
///
/// Byte-oriented, unlike the request reader, and not out of fussiness: a
/// response head may carry bytes that are not UTF-8 (a `filename=` in a legacy
/// encoding is the ordinary case), and a reader that gave up on those would
/// delete a response the box had already produced.
///
/// Returns what was read either way. `complete` says whether the blank line
/// arrived; when it did not, the caller relays the bytes verbatim rather than
/// discarding them, because a truncated response is something a person can
/// debug and a silently empty one is not.
///
/// Resolves when the box has written a *final* response head. Run beside the
/// request-body forward, so an answer that arrives before the upload finishes
/// is what ends the upload rather than something discovered thirty seconds
/// later. Everything it reads is accumulated into `buf`, which the caller
/// passes on as `read_response`'s pending bytes.
///
/// Interim heads (`1xx`, including `101`) do not count: `100 Continue` is
/// permission to keep sending, `103 Early Hints` is not an answer, and a `101`
/// belongs to the upgrade path, which has no request body to race. Never
/// resolves if the box says nothing, which is the ordinary case: this is always
/// one arm of a `select!`.
async fn watch_for_final_response<R: tokio::io::AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) {
    let mut chunk = [0u8; 4096];
    loop {
        // Bounded by the same cap the response reader uses. A box dribbling a
        // head that never ends must not grow this without limit; leaving the
        // loop hands the oversized head to `read_response`, which refuses it
        // for the same reason.
        if buf.len() > MAX_RESPONSE_HEAD {
            break;
        }
        // Look before reading, so bytes already carried in are enough on their
        // own.
        let mut scanned = 0;
        while let Some(end) = find_head_end(&buf[scanned..]) {
            let head = &buf[scanned..scanned + end];
            match status_code(head) {
                Some(c) if c >= 200 => return,
                // An interim head. Step over it and look at what follows: a
                // `100 Continue` and the real answer often arrive together.
                _ => scanned += end,
            }
            if scanned >= buf.len() {
                break;
            }
        }
        match r.read(&mut chunk).await {
            // The box closed or errored. Not this function's business (the
            // body forward will notice, or `read_response` will) and
            // resolving here would race a legitimate "the box hung up"
            // ending into looking like an answer.
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    // Nothing more to watch for. Park rather than resolve: resolving would tell
    // the caller a final answer had arrived when none had.
    std::future::pending::<()>().await
}

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
/// The head is rewritten to say `Connection: close`, which is a correctness fix
/// rather than tidiness. The proxy stops reading the client after one request;
/// a response telling the client "keep this connection, send me another"
/// produced exactly that: a connection the client believed reusable that
/// answered nothing, which on the tunnel path is an intermittent hang and a
/// `502` for every POST the client will not retry.
///
/// The body is framed by `Content-Length` when there is one, so the connection
/// ends when the response does rather than waiting on a box that may never hang
/// up. Without one it is relayed until the box closes or goes quiet.
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
    truncated: &std::sync::atomic::AtomicBool,
) -> std::io::Result<()>
where
    PR: tokio::io::AsyncRead + Unpin + Send,
    PW: tokio::io::AsyncWrite + Unpin + Send,
    UR: tokio::io::AsyncRead + Unpin + Send,
{
    use std::sync::atomic::Ordering;

    // The same line discipline the request head is held to. `gate` refuses a
    // bare CR or LF there because normalising "leaves two parsers and hopes
    // they agree"; the response side used to do exactly that, and a bare CR
    // walked a `Connection: keep-alive` and a `Set-Cookie` past both filters.
    // The box deciding whether the sanitiser applied to it.
    if complete && !head_is_well_formed(&head) {
        // Its own answer. The box *did* finish this head (it is malformed, not
        // truncated) and telling the sharer to look for a truncation sends
        // them after something that is not happening, while discarding the one
        // signal that would tell them their app emits injectable headers.
        return refuse_the_response(&mut peer_r, &mut peer_w, malformed_head_response(), to_peer)
            .await;
    }

    if !complete && head.is_empty() {
        // The box closed without saying anything at all. That is a different
        // fact from starting a reply and not finishing it, and it is what
        // happens when a dev server rejects a request by hanging up, so the
        // sharer is told the box answered nothing rather than sent to look for
        // a truncation.
        return refuse_the_response(
            &mut peer_r,
            &mut peer_w,
            unreachable_box_response(),
            to_peer,
        )
        .await;
    }

    if !complete {
        // A head the box never finished is not a response, and relaying the bytes
        // verbatim (which is what this did, to avoid "silently deleting a
        // response") hands the visitor a head that skipped every rewrite: no forced
        // `close`, no share-cookie filter, no length sanitising. The box chooses
        // when to stop talking, so that put the whole sanitiser behind a door the
        // box holds.
        //
        // Recorded as a truncation, because that is what it is. The visitor gets a
        // readable `502` either way, and without this the receipt was silent about
        // a real box failure and the sharer would go looking elsewhere.
        truncated.store(true, Ordering::Relaxed);
        return refuse_the_response(&mut peer_r, &mut peer_w, unfinished_response(), to_peer).await;
    }

    let (out, body_len, body_kind) = {
        let framing = response_framing(&head);
        let rewritten = close_the_connection(&head);
        // The head is sanitised on its own terms, and the body length is a separate
        // question. Deciding both at once put "no body" and "framing we refuse to
        // trust" through the same branch, which is how an ambiguous *page* ended up
        // delivered with no content at all.
        //
        // Two `Content-Length` headers are a response-smuggling shape the request
        // side refuses outright, and a client that reads it either errors or picks
        // one, which is the same disagreement one hop further out. Stripped even
        // when the answer carries no body: a `304` is the commonest response a dev
        // server produces, and "this branch has no body so the headers cannot hurt"
        // is how an invariant ends up holding on one path and not the other.
        //
        // What the visitor is left with is one framing rather than two.
        let out = if framing.strip_lengths {
            strip_lengths(&rewritten)
        } else {
            rewritten
        };
        // A `HEAD`, a `204` or a `304` declares a length and sends nothing.
        // Waiting for that body would hold the connection until the idle
        // timeout, and a `304` is what a dev server answers for every asset the
        // browser already has, so this is the common case, not the exotic one.
        let body_len = if bodyless {
            Some(0)
        } else {
            match framing.body {
                Body::Length(n) => Some(n),
                // Nothing this proxy can count, so the ending is either the
                // terminating chunk (watched for below) or the connection
                // closing, which is exactly what the head now says.
                Body::Chunked | Body::UntilClose => None,
            }
        };
        (out, body_len, framing.body)
    };
    // Only for a body whose framing says where it stops. A `HEAD`/`204`/`304`
    // carries no chunk stream to scan even if the head claims one.
    let mut scan = (body_kind == Body::Chunked && !bodyless).then(Scan::new);
    if !out.is_empty() {
        write_timed(&mut peer_w, &out).await?;
        to_peer.fetch_add(out.len() as u64, Ordering::Relaxed);
    }
    let mut sent_body = 0u64;
    if !rest.is_empty() {
        let take = match body_len {
            Some(n) => rest.len().min((n - sent_body) as usize),
            None => rest.len(),
        };
        // Fed *before* the write, not after, because the answer is how much of
        // this belongs to the response. A declared length clamps by arithmetic;
        // this is the same clamp for the framing that has no arithmetic.
        let take = match scan.as_mut() {
            Some(s) => s.feed(&rest[..take]),
            None => take,
        };
        write_timed(&mut peer_w, &rest[..take]).await?;
        to_peer.fetch_add(take as u64, Ordering::Relaxed);
        sent_body += take as u64;
    }

    if body_len == Some(sent_body) || scan.as_ref().is_some_and(Scan::is_done) {
        // The response is complete. Do not just drop the socket: the peer's own
        // request body may still be unread, a dev server that answers before
        // reading it is the ordinary case, and closing on unread bytes resets
        // the connection, which throws away the answer just delivered.
        finish_with(peer_r, peer_w).await;
        return Ok(());
    }

    let mut buf = vec![0u8; 32 * 1024];
    let mut discard = vec![0u8; 8 * 1024];
    // The peer has stopped *sending*. Not the same as the peer going away, and
    // the difference is a truncated download. See the discard branch below.
    let mut peer_said_all_it_will = false;
    // Whether a clock, rather than either end, is what ended this. Only matters
    // once the peer has half-closed: from then on nothing can tell us it left,
    // so a timer firing is as consistent with a client that died as with a box
    // that went quiet.
    let mut a_timer_ended_it = false;
    // Which side ended the relay. Truncation is a claim about the *box*, and
    // the visitor closing a tab mid-download is the commonest way this loop
    // ends. Reporting that as the box cutting a response short libels it in
    // the one artifact that is supposed to be evidence.
    let mut box_ended_it = false;
    // The box's side stopped without ending: a timer fired, or the stream
    // errored. For an unframed response a clean close *is* the ending, so this
    // is the only thing that can make one short.
    let mut cut_short = false;
    // An absolute cap as well as an idle one. The idle timeout is rebuilt every
    // time round the loop, including when the *client* says something, so a
    // peer dribbling a byte a minute could hold an unframed response open for
    // as long as it liked, which is the ceiling this share has instead of a
    // per-peer limit.
    let hard_deadline = tokio::time::Instant::now()
        + if body_len.is_some() {
            FRAMED_LIFETIME
        } else {
            UNFRAMED_LIFETIME
        };
    loop {
        // Applied whether or not the response declared a length. A box
        // answering `Content-Length: 1000000000` and sending one byte every
        // four minutes resets the idle timer forever, and "framed responses end
        // when their body does" is a promise agent-written code never made.
        if tokio::time::Instant::now() >= hard_deadline {
            box_ended_it = true;
            cut_short = true;
            a_timer_ended_it = true;
            break;
        }
        tokio::select! {
            from_box = tokio::time::timeout(RESPONSE_IDLE, up_r.read(&mut buf)) => {
                let n = match from_box {
                    // The box stopped: an end of stream, a reset, or five
                    // minutes of silence. Whether that leaves the response
                    // short is decided below; that it was the box's doing is
                    // decided here.
                    // A clean end of stream. For an until-close response that
                    // *is* the ending; for a declared length it is short.
                    Ok(Ok(0)) => {
                        box_ended_it = true;
                        break;
                    }
                    // A read error is not an ending, it is an interruption.
                    // The box's side went away mid-stream. Short either way.
                    Ok(Err(_)) => {
                        box_ended_it = true;
                        cut_short = true;
                        break;
                    }
                    // The idle timeout: the box went quiet without ever
                    // finishing. For an unframed response that is the only way
                    // to know it was cut short.
                    Err(_) => {
                        box_ended_it = true;
                        cut_short = true;
                        a_timer_ended_it = true;
                        break;
                    }
                    Ok(Ok(n)) => n,
                };
                let take = match body_len {
                    Some(total) => n.min((total - sent_body) as usize),
                    None => n,
                };
                // Same clamp as the first block above: nothing past the
                // terminating chunk is this response's.
                let take = match scan.as_mut() {
                    Some(s) => s.feed(&buf[..take]),
                    None => take,
                };
                if write_timed(&mut peer_w, &buf[..take]).await.is_err() {
                    break;
                }
                to_peer.fetch_add(take as u64, Ordering::Relaxed);
                sent_body += take as u64;
                // The declared length was reached, or the terminating chunk
                // went by. Either way the visitor has a complete response and
                // this connection's permit belongs to the next one.
                if body_len == Some(sent_body) || scan.as_ref().is_some_and(Scan::is_done) {
                    break;
                }
            }
            // The client's side is still read, and everything it says is
            // dropped. Two reasons, and the first is why this is not simply
            // "stop reading": it is how a client hanging up is noticed, and
            // without it a connection where both ends went quiet would hold one
            // of the share's slots until the grant expired. Discarding is as
            // strong a guarantee as not reading. What matters is that no byte
            // the client sends after its first request reaches the box.
            from_peer = peer_r.read(&mut discard), if !peer_said_all_it_will => {
                match from_peer {
                    // Not the end of the connection. A client that sends its request and
                    // then shuts down its write side is doing something HTTP/1.1
                    // explicitly allows, and it is what anything built out of a single
                    // `write` then a read does: `curl -T- </dev/null`, a CI scraper,
                    // `printf ... | nc`. Treating that EOF as "the visitor left" ended the
                    // relay on the spot, so those clients got the first 32 KiB of a
                    // download and a clean close, with nothing recorded: the response was
                    // cut short and the receipt said the box was fine, because by then it
                    // was the *front* cutting it. Stop watching, keep sending.
                    Ok(0) => peer_said_all_it_will = true,
                    // An error is the connection failing, which is different.
                    Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }
    // Two questions, and the flag needs both. *Did the box end it*, because a
    // visitor closing a tab mid-download is the commonest way out of this loop
    // and is nobody's fault but theirs. And *was what arrived short*, which
    // for a declared length is arithmetic, and for an unframed response is
    // knowable only when the box stopped without ever closing its stream:
    // the deadline and the idle timeout, both of which cut a stream the client
    // is still owed the end of.
    let short = match body_len {
        Some(n) => sent_body < n,
        // Chunked: the terminating chunk is the ending, so a stream that
        // stopped without one is short however it stopped, including the
        // clean close that would be a complete answer for a response with no
        // framing at all. A body whose framing this could not follow
        // (`Scan::Lost`) falls back to the unframed rule rather than accusing
        // the box of a truncation on the strength of a parse this proxy gave
        // up on.
        None => match &scan {
            Some(Scan::Done) => false,
            Some(Scan::Lost) | None => cut_short,
            Some(_) => true,
        },
    };
    // And a third question, for the one case where the first cannot be answered.
    // Once the peer has half-closed there is nothing left that could tell us it
    // went away. The discard branch is disabled and only a failed write would
    // notice, and an idle box writes nothing. So a *timer* ending a half-closed
    // connection is exactly as consistent with a client that died as with a box
    // that went quiet, and recording it as the box's doing puts a claim in the
    // evidence that this code cannot support. The arithmetic cases are still
    // recorded: a declared length the box closed short of is unambiguous.
    let attributable = !(peer_said_all_it_will && a_timer_ended_it);
    if box_ended_it && short && attributable {
        truncated.store(true, Ordering::Relaxed);
    }
    finish_with(peer_r, peer_w).await;
    Ok(())
}

/// The response head with its connection-lifetime headers replaced by one that
/// says the connection ends here, and any share cookie the box tried to set
/// taken out.
fn close_the_connection(head: &[u8]) -> Vec<u8> {
    rebuild_head(
        head,
        |line| {
            let name = header_name(line);
            name == "connection" || name == "keep-alive" || sets_a_share_cookie(line)
        },
        b"Connection: close\r\n",
    )
}

/// Drop every `Content-Length` from a head whose framing we refused to trust.
fn strip_lengths(head: &[u8]) -> Vec<u8> {
    rebuild_head(head, |line| header_name(line) == "content-length", b"")
}

/// Where a response's body ends, as far as this proxy will commit to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    /// Exactly this many bytes.
    Length(u64),
    /// At the terminating zero-length chunk, which this proxy watches for as it
    /// relays. It does not *parse* the body, which goes through untouched; it
    /// only tracks where the framing says the message stops.
    ///
    /// It used to be folded into [`Body::UntilClose`], at a cost of a slot held
    /// for five minutes per request. This code treats "an agent-written server
    /// that ignores `Connection: close`" as a supported case; against one, the
    /// browser had a complete response in hand while h5i sat waiting for a close
    /// that was not coming, until `RESPONSE_IDLE` fired. Sixty-four requests and
    /// the share answered `busy` for the rest of that interval.
    Chunked,
    /// The connection closing is the framing, and there is nothing else to go
    /// on.
    UntilClose,
}

/// How a response's body is framed, and whether its head can be passed on as
/// it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Framing {
    body: Body,
    /// Drop every `Content-Length` on the way out. Set whenever the head said
    /// two things about where the body ends: the visitor gets one framing
    /// rather than two, because a client that has to pick between them is the
    /// same disagreement one hop further out.
    strip_lengths: bool,
}

/// Read the response's framing out of its head.
///
/// `Transfer-Encoding` beats `Content-Length` for every HTTP client, so framing
/// on the length when both are present sends the client a prefix of the *chunk
/// framing* and closes: a truncated stream that reads as the app being broken.
///
/// *Any* transfer coding does that, not only `chunked`, and that was the hole:
/// a response carrying `Transfer-Encoding: gzip` *and* `Content-Length: 4` was
/// graded `Length(4)` here, so both headers reached the visitor and h5i stopped
/// after four bytes, while the recipient, for which a transfer coding is
/// authoritative, was reading a stream that ends when the connection does. A
/// coding this proxy cannot follow means the connection closing is the only
/// framing it will commit to, and the length does not travel.
///
/// `chunked` has to be the *final* coding to be worth anything: `chunked, gzip`
/// is invalid and means the octets on the wire are not chunk-framed, so
/// watching for a terminating chunk would be watching for a byte pattern in
/// compressed data.
fn response_framing(head: &[u8]) -> Framing {
    let mut length = None;
    let mut lengths = 0;
    let mut codings: Vec<String> = Vec::new();
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = String::from_utf8_lossy(&line[..colon])
            .trim()
            .to_ascii_lowercase();
        let value = String::from_utf8_lossy(&line[colon + 1..])
            .trim()
            .to_string();
        match name.as_str() {
            "transfer-encoding" => {
                // Token-wise, not substring: `contains` would read a value like
                // `x-chunked-foo` as chunked, and the whole point of this is to
                // agree with what the box is actually going to send.
                //
                // `identity` is legal, deprecated, and means "no encoding", so
                // it is dropped here rather than counted as a coding. Grading
                // it as one would strip a perfectly good `Content-Length`.
                codings.extend(
                    value
                        .split(',')
                        .map(|t| t.trim().to_ascii_lowercase())
                        .filter(|t| !t.is_empty() && t != "identity"),
                );
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
    if !codings.is_empty() {
        return Framing {
            body: if codings.last().is_some_and(|c| c == "chunked") {
                Body::Chunked
            } else {
                Body::UntilClose
            },
            strip_lengths: lengths > 0,
        };
    }
    match (lengths, length) {
        (1, Some(n)) => Framing {
            body: Body::Length(n),
            strip_lengths: false,
        },
        (0, _) => Framing {
            body: Body::UntilClose,
            strip_lengths: false,
        },
        _ => Framing {
            body: Body::UntilClose,
            strip_lengths: true,
        },
    }
}

/// Longest chunk-size or trailer line the response scanner will buffer.
const MAX_SCAN_LINE: usize = 16 * 1024;

/// Where a relayed chunked body has got to.
///
/// Fed the same bytes that go to the visitor, in the same order, and asked
/// after each write whether the message has ended. It reads only the framing
/// (the size lines, the trailers and the blank line that closes them) and skips
/// the data, so a body is never buffered and never inspected.
///
/// A body it cannot follow ends up [`Scan::Lost`], which is not an error: the
/// relay falls back to waiting for the close, which is what it did for every
/// chunked response before this existed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scan {
    /// Reading a size line, or a trailer line once the zero chunk has been seen.
    Line { after_zero: bool, line: Vec<u8> },
    /// Passing the declared chunk data.
    Data(u64),
    /// The CRLF that closes a chunk's data, as much of it as has gone by.
    ///
    /// Its own state rather than two extra bytes on [`Scan::Data`], and the
    /// difference is whether those bytes are *checked*. Counted, the scanner
    /// found the end of `1\r\nAXX0\r\n\r\n` (one byte of data, `XX` where its
    /// CRLF belongs) at a boundary no client recognises, and since the relay
    /// clamps at whatever the scanner calls the end, that turned a malformed
    /// response into one h5i truncated on the box's behalf. Checked, the same
    /// stream is [`Scan::Lost`] and the visitor gets every byte the box sent.
    Closing(u8),
    /// The terminating chunk and its trailers have gone by.
    Done,
    /// The framing did not parse. Nothing is refused on this basis; the
    /// response simply goes back to being framed by the connection closing.
    Lost,
}

impl Scan {
    fn new() -> Scan {
        Scan::Line {
            after_zero: false,
            line: Vec::new(),
        }
    }

    fn is_done(&self) -> bool {
        matches!(self, Scan::Done)
    }

    /// Advance over bytes about to be relayed, and answer how many of them
    /// belong to this response.
    ///
    /// The count is what makes the chunked path enforce the same rule the
    /// `Content-Length` path gets from arithmetic. Without it everything the box
    /// wrote past the terminating chunk went to the visitor: a second status
    /// line, its own `Set-Cookie`, its own `Connection`, a head that reached the
    /// visitor's client having skipped `close_the_connection` and
    /// `strip_share_cookies`. Conforming clients discard it; the objection is
    /// that whether the sanitiser applies was the box's choice.
    ///
    /// `Lost` answers "all of them": a body whose framing this could not follow
    /// falls back to being framed by the connection closing.
    fn feed(&mut self, bytes: &[u8]) -> usize {
        let total = bytes.len();
        let mut rest = bytes;
        loop {
            match self {
                // Nothing after the terminating chunk is part of this message.
                Scan::Done => return total - rest.len(),
                // The framing is not something to clamp on.
                Scan::Lost => return total,
                _ if rest.is_empty() => return total,
                Scan::Data(left) => {
                    let take = (*left).min(rest.len() as u64) as usize;
                    *left -= take as u64;
                    rest = &rest[take..];
                    if *left == 0 {
                        *self = Scan::Closing(0);
                    }
                }
                // The two bytes after the data, one at a time because they can
                // arrive in separate reads.
                Scan::Closing(seen) => {
                    let want = if *seen == 0 { b'\r' } else { b'\n' };
                    if rest[0] != want {
                        *self = Scan::Lost;
                        continue;
                    }
                    rest = &rest[1..];
                    *self = if *seen == 0 {
                        Scan::Closing(1)
                    } else {
                        Scan::Line {
                            after_zero: false,
                            line: Vec::new(),
                        }
                    };
                }
                Scan::Line { after_zero, line } => match rest.iter().position(|&b| b == b'\n') {
                    Some(i) => {
                        line.extend_from_slice(&rest[..=i]);
                        rest = &rest[i + 1..];
                        let complete = std::mem::take(line);
                        let after = *after_zero;
                        *self = Scan::after_line(&complete, after);
                    }
                    None => {
                        line.extend_from_slice(rest);
                        if line.len() > MAX_SCAN_LINE {
                            *self = Scan::Lost;
                        }
                        return total;
                    }
                },
            }
        }
    }

    fn after_line(line: &[u8], after_zero: bool) -> Scan {
        if after_zero {
            // Trailers, then the blank line that ends them. CRLF only: a bare
            // LF was accepted here while `is_trailer` refused one on the line
            // above, so a response framed with bare LFs had its end found by
            // the terminator and its trailers rejected. Two readings of the
            // same stream, in the one place that decides where a message stops.
            // Now it is `Lost`, which means framed by the close.
            return if line == b"\r\n" {
                Scan::Done
            } else if is_trailer(line) {
                Scan::Line {
                    after_zero: true,
                    line: Vec::new(),
                }
            } else {
                Scan::Lost
            };
        }
        match chunk_size(line) {
            Some(0) => Scan::Line {
                after_zero: true,
                line: Vec::new(),
            },
            // The data, and then its closing CRLF as a state of its own.
            // Checked rather than counted. No `checked_add` is needed for that
            // reason: `u64::MAX` is a chunk this streams rather than an
            // overflow.
            Some(n) => Scan::Data(n),
            None => Scan::Lost,
        }
    }
}

/// What a visitor gets when the box starts a response and never finishes it.
const UNFINISHED_BODY: &str =
    "The app in this box began a reply and did not finish it. Whoever shared it needs to look.";

/// What a visitor gets when the box's answer is shaped in a way this proxy
/// will not pass on.
const MALFORMED_HEAD_BODY: &str = "The app in this box sent a reply h5i will not forward: its headers are not well formed. Whoever shared it needs to look.";

fn malformed_head_response() -> String {
    format!(
        "HTTP/1.1 502 Bad Gateway\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{MALFORMED_HEAD_BODY}",
        MALFORMED_HEAD_BODY.len()
    )
}

/// Answer the visitor on the box's behalf and close.
async fn refuse_the_response<PR, PW>(
    peer_r: PR,
    mut peer_w: PW,
    reply: String,
    to_peer: &std::sync::atomic::AtomicU64,
) -> std::io::Result<()>
where
    PR: tokio::io::AsyncRead + Unpin,
    PW: tokio::io::AsyncWrite + Unpin,
{
    use std::sync::atomic::Ordering;
    if write_timed(&mut peer_w, reply.as_bytes()).await.is_ok() {
        to_peer.fetch_add(reply.len() as u64, Ordering::Relaxed);
    }
    // Drained, like every other close. These are the paths where the peer is
    // *most* likely to still be sending, it declared a body and we refused
    // part-way through it, so they are the ones a reset would most reliably
    // destroy the answer on.
    finish_with(peer_r, peer_w).await;
    Ok(())
}

/// How long to spend swallowing what the peer is still sending, so the socket
/// can close without a reset.
const LINGER_DRAIN: Duration = Duration::from_secs(2);

/// How long to wait to find out whether there is anything to swallow at all.
///
/// A read that times out having returned nothing means the receive queue is
/// empty *now*, and an empty receive queue is the whole condition for closing
/// without a reset. So the common case, a peer that sent one request, got its
/// answer and is waiting for the close, costs this and not [`LINGER_DRAIN`].
///
/// Short, because anything already queued is returned by the first `read`
/// immediately; the wait only buys bytes still in flight. A peer that neither
/// closes nor sends pays it in full, and on a tunnel share that peer is
/// `cloudflared`, which pools connections. Without the split every request held
/// one of the share's 64 slots for two seconds past its response, and a page of
/// fifty subresources would have started answering `503 This share is busy`.
const LINGER_PROBE: Duration = Duration::from_millis(40);

/// And how much to swallow once there is something. Plenty for a form post that
/// was refused; nowhere near a file upload nobody is going to look at.
const MAX_LINGER_BYTES: u64 = 4 * 1024 * 1024;

/// Finish with a peer: stop writing, swallow whatever it is still sending, and
/// only then let the socket go.
///
/// Closing a TCP socket with unread bytes still in its receive queue makes the
/// kernel send an RST instead of finishing the close, and an RST discards this
/// side's *unflushed send queue*. So the shape to avoid is a response still
/// buffered locally when the socket closes on an upload nobody read.
///
/// How much this is worth, honestly. We could not construct a case on Linux
/// where deleting this drain changed what a visitor received: the relay loop
/// already reads and discards everything the peer sends while a body is in
/// flight, so where responses are large there is nothing left unread. What is
/// left is the refusal paths, and there the answer is a few dozen bytes, on the
/// wire long before the close, and Linux does not purge the *peer's* receive
/// queue on an RST.
///
/// It is kept because it is bounded (see the two constants) and because the
/// thing at the other end of a tunnel share is `cloudflared`, and what an
/// intermediary does with an origin that resets rather than closes is its
/// business.
async fn finish_with<PR, PW>(mut peer_r: PR, mut peer_w: PW)
where
    PR: tokio::io::AsyncRead + Unpin,
    PW: tokio::io::AsyncWrite + Unpin,
{
    let _ = peer_w.shutdown().await;
    let mut discard = vec![0u8; 8 * 1024];
    // Is there anything queued at all? If not, the close is already graceful
    // and waiting out the full drain buys nothing but a held slot.
    let mut swallowed = match tokio::time::timeout(LINGER_PROBE, peer_r.read(&mut discard)).await {
        Ok(Ok(n)) if n > 0 => n as u64,
        _ => return,
    };
    let _ = tokio::time::timeout(LINGER_DRAIN, async {
        while let Ok(n) = peer_r.read(&mut discard).await {
            if n == 0 {
                break;
            }
            // Bounded in bytes as well as time. The point is to let the kernel
            // close cleanly, not to read out an upload nobody wants; past this
            // the reset is the lesser cost.
            swallowed += n as u64;
            if swallowed > MAX_LINGER_BYTES {
                break;
            }
        }
    })
    .await;
}

/// What a peer gets when the box stopped reading its request.
const UNREACHABLE_BOX_BODY: &str =
    "The app in this box stopped reading. Whoever shared it needs to look.";

fn unreachable_box_response() -> String {
    format!(
        "HTTP/1.1 502 Bad Gateway\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{UNREACHABLE_BOX_BODY}",
        UNREACHABLE_BOX_BODY.len()
    )
}

/// What a peer gets when it declared a body and took too long to send it.
const TIMED_OUT_BODY: &str = "That request took too long to arrive. Try it again.";

fn timed_out_response() -> String {
    format!(
        "HTTP/1.1 408 Request Timeout\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{TIMED_OUT_BODY}",
        TIMED_OUT_BODY.len()
    )
}

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

/// `HTTP/<d>.<d> <3 digits>`, and then whatever reason phrase it likes.
///
/// A `starts_with(b"HTTP/")` test was the first attempt and is not the same
/// thing: `HTTP/1.1`, `HTTP/`, `HTTP/oops` and `HTTP/1.1: 200 OK` all pass it,
/// and a third of the heads the fuzz corpus produced that got through the check
/// had a first line that was not a status line. Those relay verbatim, since no
/// filter predicate can match a `HTTP/`-prefixed line, so the visitor's browser
/// reports the same nameless protocol error the check was added to cure.
fn is_status_line(l: &[u8]) -> bool {
    let Some(rest) = l.strip_prefix(b"HTTP/") else {
        return false;
    };
    // `1.1`, `1.0`, and `2`/`3` for a box that answers with a version this
    // proxy does not speak. Refusing those is the same readable `502`.
    let mut it = rest.splitn(2, |&b| b == b' ');
    let Some(version) = it.next() else {
        return false;
    };
    let version_ok = match version {
        [maj] => maj.is_ascii_digit(),
        [maj, b'.', min] => maj.is_ascii_digit() && min.is_ascii_digit(),
        _ => false,
    };
    let Some(rest) = it.next() else {
        return false;
    };
    let mut parts = rest.splitn(2, |&b| b == b' ');
    let status = parts.next().unwrap_or(b"");
    // The reason phrase is the box's to choose and it reaches the visitor
    // verbatim, so it is held to what RFC 7230 allows: HTAB, SP, visible ASCII
    // and obs-text. A box answering `HTTP/1.1 200 \x1b[31mCOMPROMISED` had
    // those escapes relayed to whatever the visitor was reading with: inert
    // in a browser, not inert in a terminal, and malformed either way.
    let reason_ok = parts.next().is_none_or(|r| {
        r.iter()
            .all(|b| *b == b'\t' || (0x20..0x7f).contains(b) || *b >= 0x80)
    });
    version_ok && reason_ok && status.len() == 3 && status.iter().all(|b| b.is_ascii_digit())
}

/// Is every line ending in this head a CRLF, and is every header line a header
/// rather than a continuation of one?
///
/// Both rules are the request side's, for the request side's reason: a line
/// this proxy reads as one thing and the visitor's browser reads as two is a
/// header walking past the filters. Obs-fold is the second door: `header_name`
/// trims, so a folded continuation matches a filter and gets dropped, or
/// survives a dropped line and reattaches itself to the header above.
fn head_is_well_formed(head: &[u8]) -> bool {
    // It has to be a response at all. The sanitiser rebuilds a head by dropping
    // the lines a filter rejects, and it drops empty ones, so a box whose first
    // line is a header rather than a status line had that header *promoted* into
    // the status line's place, and the visitor got a reply beginning
    // `Connection: close` with no status in it.
    //
    // A leading blank line before a real status line is absorbed: clients have
    // always tolerated one, and refusing it would fail a response that is only
    // untidy.
    let first = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .find(|l| !l.is_empty());
    match first {
        Some(l) if is_status_line(l) => {}
        _ => return false,
    }
    for (i, &c) in head.iter().enumerate() {
        if c == b'\n' && (i == 0 || head[i - 1] != b'\r') {
            return false;
        }
        if c == b'\r' && head.get(i + 1) != Some(&b'\n') {
            return false;
        }
    }
    // Skip the status line; a continuation cannot be the first thing.
    head.split(|&b| b == b'\n')
        .skip(1)
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty())
        .all(|l| !l.starts_with(b" ") && !l.starts_with(b"\t"))
}

/// Rebuild a response head, dropping the lines a filter rejects and appending
/// whatever the caller wants at the end.
///
/// One loop, because there were three near-identical ones (the `Connection`
/// rewrite, the length strip, the cookie filter) and three copies of a parser
/// is a divergence waiting for the round that finds it.
///
/// The status line survives every filter, and it is worth being precise about
/// why: not because it has no colon (`HTTP/1.1 500 Error: bad thing` does), but
/// because no predicate here matches what `header_name` makes of one. A future
/// filter has to keep that true rather than inherit it.
fn rebuild_head(head: &[u8], drop_line: impl Fn(&[u8]) -> bool, extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len() + extra.len());
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || drop_line(line) {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(extra);
    out.extend_from_slice(b"\r\n");
    out
}

/// A header line's name, lowercased. Empty for a line with no colon.
fn header_name(line: &[u8]) -> String {
    match line.iter().position(|&b| b == b':') {
        Some(colon) => String::from_utf8_lossy(&line[..colon])
            .trim()
            .to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Drop every `Set-Cookie` that sets a share cookie, and nothing else.
fn strip_share_cookies(head: &[u8]) -> Vec<u8> {
    rebuild_head(head, sets_a_share_cookie, b"")
}

/// The name a `Set-Cookie` line sets, or `None` for any other header.
///
/// Not `set_cookie_pair().map(name)`, which is how it was written. That
/// requires an `=`, so `Set-Cookie: h5i_share`, a header with a name and no
/// value, was not a `Set-Cookie` for one of ours and travelled to the visitor
/// untouched. Found by the response fuzzer at 147,554 rounds.
///
/// Not a leak on its own: RFC 6265 §5.2 says a set-cookie-string whose
/// name-value pair lacks an `=` is ignored entirely. It is the same rule
/// applied on one branch and not the other, and the answer matches
/// `split_cookie` on the request side: whatever is named like ours does not
/// travel, however it is spelled.
fn set_cookie_name(line: &[u8]) -> Option<String> {
    if header_name(line) != "set-cookie" {
        return None;
    }
    let colon = line.iter().position(|&b| b == b':')?;
    let value = String::from_utf8_lossy(&line[colon + 1..]);
    let first = value.trim_start().split(';').next().unwrap_or("");
    // The name is what precedes the `=`, and the whole segment when there is
    // none. A browser ignores the second shape; this does not have to.
    Some(
        first
            .split_once('=')
            .map(|(name, _)| name)
            .unwrap_or(first)
            .trim()
            .to_string(),
    )
}

/// The name *and value* a `Set-Cookie` line assigns, for the allowlist.
///
/// Still requires an `=`, unlike [`set_cookie_name`], and the asymmetry is the
/// point: this feeds [`gate::AppCookies`], which decides what may be sent *to*
/// the box, and a cookie with no value is one no browser stores and therefore
/// one no browser sends back. Learning a name from it would put an entry on the
/// allowlist only somebody else's cookie could match.
///
/// Both, because a name alone cannot tell one cookie from another in a jar this
/// proxy shares with the rest of the machine: two cookies may carry one name
/// when their paths differ, and the browser sends both back in one header.
///
/// `split_once`, not `split`: a cookie value may itself contain `=` (base64
/// padding is the everyday case), and taking the first field would learn a
/// value the browser never sends back, dropping the cookie of any app whose
/// session id ends in `=`.
fn set_cookie_pair(line: &[u8]) -> Option<(String, String)> {
    if header_name(line) != "set-cookie" {
        return None;
    }
    let colon = line.iter().position(|&b| b == b':')?;
    let value = String::from_utf8_lossy(&line[colon + 1..]);
    let first = value.trim_start().split(';').next().unwrap_or("");
    let (name, val) = first.split_once('=')?;
    Some((name.trim().to_string(), val.trim().to_string()))
}

/// Note every cookie name this head sets, so the ones the visitor sends back
/// can be told from the rest of a shared jar's contents.
///
/// Called on every response head that reaches the visitor, interim and final
/// alike, not because a `1xx` is a likely place to set a cookie, but because
/// "the rule ran on two of the three paths" is the defect this file keeps
/// having. Missing one costs an app its cookie; it cannot leak anything.
///
/// `None` is the ordinary case: a share whose cookie jar is its own has nothing
/// to tell apart. See [`gate::AppCookies`].
fn learn_app_cookies(head: &[u8], app: Option<&gate::AppCookies>) {
    let Some(app) = app else {
        return;
    };
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some((name, value)) = set_cookie_pair(line) {
            app.learn(&name, &value);
        }
    }
}

/// Is this header line a `Set-Cookie` for one of ours?
///
/// The box cannot guess a token, so the worst it could do is clear one, but
/// cookies ignore the port, so on the joining side one box could log a visitor
/// out of a *different* share. Nothing legitimate sets a cookie by this name.
fn sets_a_share_cookie(line: &[u8]) -> bool {
    // The *name*, by the one predicate both directions share. A raw
    // `starts_with` here deleted an app's `Set-Cookie: h5i_shared_theme=dark`
    // from every response, so the app lost state only when viewed through a
    // share. See `gate::is_share_cookie_name`.
    set_cookie_name(line).is_some_and(|n| crate::gate::is_share_cookie_name(&n))
}

#[cfg(test)]
mod status_line_tests {
    use super::*;

    #[test]
    fn a_reply_with_no_status_line_is_not_a_response() {
        // Found by the fuzzer. The sanitiser rebuilds a head by dropping the
        // lines a filter rejects, and it drops empty ones, so a box whose
        // first line is a header rather than a status line had that header
        // promoted into the status line's place, and the visitor got a reply
        // that began `Connection: close` with no status in it at all. A browser
        // reports that as a protocol error and says nothing about the cause.
        assert!(!head_is_well_formed(b"Connection: close\r\n\r\n"));
        assert!(!head_is_well_formed(b"\r\nConnection: close\r\n\r\n"));
        assert!(!head_is_well_formed(b"\r\n\r\n"));
        assert!(!head_is_well_formed(b""));

        // And a `HTTP/` prefix is not a status line. Every one of these got
        // through the first version of the check and relayed verbatim, which
        // is the same nameless protocol error the check exists to cure.
        assert!(!head_is_well_formed(
            b"HTTP/1.1\r\nContent-Length: 0\r\n\r\n"
        ));
        assert!(!head_is_well_formed(b"HTTP/\r\n\r\n"));
        assert!(!head_is_well_formed(b"HTTP/1.1: 200 OK\r\nX: y\r\n\r\n"));
        assert!(!head_is_well_formed(b"HTTP/oops\r\nX: y\r\n\r\n"));
        assert!(!head_is_well_formed(b"HTTP/1.1 20 OK\r\n\r\n"));
        assert!(!head_is_well_formed(b"HTTP/1.1 2000 OK\r\n\r\n"));

        // The real ones, including a bare version and no reason phrase.
        assert!(head_is_well_formed(b"HTTP/1.1 200 OK\r\n\r\n"));
        assert!(head_is_well_formed(b"HTTP/1.1 204\r\n\r\n"));
        assert!(head_is_well_formed(b"HTTP/2 200 OK\r\n\r\n"));
        assert!(head_is_well_formed(
            b"HTTP/1.1 500 Error: bad thing\r\n\r\n"
        ));

        // A leading blank line before a real status line is only untidy, and
        // clients have always tolerated one. It is absorbed, not refused.
        assert!(head_is_well_formed(b"\r\nHTTP/1.1 200 OK\r\n\r\n"));
        let out = close_the_connection(b"\r\nHTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\n");
        assert_eq!(&out, b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
    }

    /// The head this file accepts is the head it reads the status from.
    ///
    /// `head_is_well_formed` and `rebuild_head` both tolerate a leading CRLF
    /// and `status_code` did not, so for that one accepted shape every caller
    /// got `None` and read it as "not that status". The test above only used a
    /// `200`, whose behaviour is the same either way, so all three consequences
    /// went uncovered: an interim answer treated as final, an upgrade that never
    /// enters the duplex path, and a bodyless response the relay waits out.
    #[test]
    fn a_status_line_after_a_blank_line_is_still_a_status_line() {
        for (head, code) in [
            (&b"\r\nHTTP/1.1 100 Continue\r\n\r\n"[..], 100u16),
            (&b"\r\nHTTP/1.1 101 Switching Protocols\r\n\r\n"[..], 101),
            (&b"\r\nHTTP/1.1 204 No Content\r\n\r\n"[..], 204),
            (&b"\r\nHTTP/1.1 304 Not Modified\r\n\r\n"[..], 304),
            // And more than one, since `rebuild_head` drops them all.
            (&b"\r\n\r\nHTTP/1.1 304 Not Modified\r\n\r\n"[..], 304),
        ] {
            assert!(
                head_is_well_formed(head),
                "{:?}",
                String::from_utf8_lossy(head)
            );
            assert_eq!(
                status_code(head),
                Some(code),
                "the status of an accepted head was unreadable: {:?}",
                String::from_utf8_lossy(head)
            );
        }

        // The three decisions that were wrong as a result.
        assert!(is_informational(b"\r\nHTTP/1.1 100 Continue\r\n\r\n"));
        assert!(is_switching(
            b"\r\nHTTP/1.1 101 Switching Protocols\r\n\r\n"
        ));
        assert!(has_no_body("GET", b"\r\nHTTP/1.1 304 Not Modified\r\n\r\n"));
        assert!(has_no_body("GET", b"\r\nHTTP/1.1 204 No Content\r\n\r\n"));
        // And a `101` is still not an interim answer.
        assert!(!is_informational(
            b"\r\nHTTP/1.1 101 Switching Protocols\r\n\r\n"
        ));
    }

    /// A chunked body ends where its framing says, and a body this cannot
    /// follow is not claimed to have ended anywhere.
    #[test]
    fn the_terminating_chunk_is_found_however_the_bytes_arrive() {
        let body = b"2\r\nhi\r\n3\r\n th\r\n0\r\n\r\n";

        // All at once.
        let mut s = Scan::new();
        s.feed(body);
        assert!(s.is_done());

        // And one byte at a time, which is what a slow origin produces.
        let mut s = Scan::new();
        for b in body {
            assert!(!s.is_done(), "the scanner finished early");
            s.feed(&[*b]);
        }
        assert!(s.is_done());

        // Trailers between the zero chunk and the blank line.
        let mut s = Scan::new();
        s.feed(b"0\r\nExpires: never\r\n\r\n");
        assert!(s.is_done());

        // Short of the end is not the end.
        let mut s = Scan::new();
        s.feed(b"2\r\nhi\r\n");
        assert!(!s.is_done());
        let mut s = Scan::new();
        s.feed(b"0\r\n");
        assert!(!s.is_done(), "the trailer section had not been closed");

        // Framing this cannot follow becomes `Lost`, not `Done`: the relay
        // falls back to waiting for the close, which is what it did for every
        // chunked response before the scanner existed.
        let mut s = Scan::new();
        s.feed(b"not a chunk size\r\n");
        assert_eq!(s, Scan::Lost);
        assert!(!s.is_done());

        // And the answer is where the body ends, not where the buffer does:
        // this is what the relay clamps on, so a scanner that consumed the
        // whole buffer would relay a second response the box appended.
        let mut s = Scan::new();
        assert_eq!(s.feed(b"0\r\n\r\nHTTP/1.1 200 OK\r\n\r\n"), 5);
        assert!(s.is_done());
        // Past the end nothing more belongs to it.
        assert_eq!(s.feed(b"more"), 0);
    }

    /// A chunk whose data is not closed by CRLF is framing this will not
    /// follow, not framing it follows to a made-up end.
    ///
    /// The mirror of the request side's bug, and the relay's clamp is what made
    /// it consequential: the closing two bytes were *counted* with the data
    /// rather than checked, so the scanner found the end of `1\r\nAXX0\r\n\r\n`
    /// at a boundary no client recognises, and h5i truncated the response there
    /// on behalf of a box that had sent something no client would parse. `Lost`
    /// is the honest answer: relay it all and let the visitor's client refuse it.
    #[test]
    fn a_chunk_the_box_did_not_close_is_framing_this_will_not_follow() {
        for body in [
            &b"1\r\nAXX0\r\n\r\n"[..],
            &b"2\r\nabXX0\r\n\r\n"[..],
            // A bare LF where the closing CRLF belongs, and a bare-LF
            // terminator: the second used to end the body while a bare-LF
            // *trailer* was refused one line above it. Two readings of the one
            // stream that decides where a message stops.
            &b"1\r\nA\n0\r\n\r\n"[..],
            &b"0\n\n"[..],
        ] {
            let mut s = Scan::new();
            s.feed(body);
            assert_eq!(
                s,
                Scan::Lost,
                "followed framing to a made-up end: {:?}",
                String::from_utf8_lossy(body)
            );
            assert!(!s.is_done());
        }

        // The well-formed ones still end where they end, including a chunk
        // whose closing CRLF is split across two reads.
        let mut s = Scan::new();
        assert_eq!(s.feed(b"1\r\nA\r"), 5);
        assert_eq!(s.feed(b"\n0\r\n\r\n"), 6);
        assert!(s.is_done());
    }
}

#[cfg(test)]
mod response_fuzz {
    use super::*;
    use crate::fuzz::{response_head, rounds, Rng};

    /// What the response side owes a visitor, for every head a box can emit.
    ///
    /// The box is agent-written code we are deliberately showing somebody, so
    /// "a real server would not send that" is not an argument available here.
    #[test]
    fn a_sanitised_response_head_has_one_framing_one_connection_and_no_cookie() {
        let mut rng = Rng::new(0xC0FFEE);
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let head = response_head(&mut one);
            let ctx = || {
                format!(
                    "round {i}, seed {seed:#x}, head {:?}",
                    String::from_utf8_lossy(&head)
                )
            };

            // Everything below runs only for heads the front is willing to
            // relay; a malformed one is answered with our own page, and that
            // page is a constant.
            if !head_is_well_formed(&head) {
                continue;
            }

            let framing = response_framing(&head);
            let rewritten = close_the_connection(&head);
            let out = if framing.strip_lengths {
                strip_lengths(&rewritten)
            } else {
                rewritten
            };

            // One `Connection`, and it says the connection ends here. This is
            // what makes one-request-per-connection true rather than requested.
            let conns: Vec<String> = header_values(&out, "connection");
            assert_eq!(
                conns.len(),
                1,
                "not exactly one Connection: {} -> {out:?}",
                ctx()
            );
            assert!(
                conns[0].eq_ignore_ascii_case("close"),
                "the connection was not closed: {} -> {conns:?}",
                ctx()
            );

            // No share cookie, ever. The box cannot guess a token, but it can
            // clear one, and cookies ignore the port, so on the joining side
            // one box could log a visitor out of a different share.
            for v in header_values(&out, "set-cookie") {
                let name = v
                    .trim_start()
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .trim();
                assert!(
                    !crate::gate::is_share_cookie_name(name),
                    "the box set a share cookie: {} -> {v:?}",
                    ctx()
                );
            }

            // One framing on the way out. Two lengths, or a length beside
            // *any* transfer coding, is a response-smuggling shape: the
            // visitor's client picks one and we picked the other. Not only
            // `chunked`: a recipient treats `Transfer-Encoding: gzip` as
            // authoritative over a length just the same.
            let lengths = header_values(&out, "content-length").len();
            let encoded = header_values(&out, "transfer-encoding").iter().any(|v| {
                v.split(',').any(|t| {
                    let t = t.trim();
                    !t.is_empty() && !t.eq_ignore_ascii_case("identity")
                })
            });
            assert!(
                lengths <= 1 && !(encoded && lengths > 0),
                "two framings reached the visitor ({lengths} lengths, encoded={encoded}): {} -> {out:?}",
                ctx()
            );

            // And whatever we decided about the body has to be a decision the
            // sanitised head still supports.
            match framing.body {
                // By value, not by spelling. `007` is a legal length and
                // every parser reads it as 7, so requiring the text to match
                // would be asserting about formatting rather than about
                // whether two parsers can disagree.
                Body::Length(n) => {
                    let declared: Vec<Option<u64>> = header_values(&out, "content-length")
                        .iter()
                        .map(|v| v.parse::<u64>().ok())
                        .collect();
                    assert_eq!(
                        declared,
                        vec![Some(n)],
                        "the head disagrees with the length we will read: {}",
                        ctx()
                    );
                }
                // Anything else is framed by the chunk stream or by the close,
                // and a length beside either is one framing too many.
                Body::Chunked | Body::UntilClose => assert!(
                    !framing.strip_lengths || lengths == 0,
                    "a head we refused to trust kept a length: {} -> {out:?}",
                    ctx()
                ),
            }

            // Line discipline, out as well as in.
            assert!(
                head_is_well_formed(&out),
                "the sanitiser emitted a head it would itself refuse: {} -> {out:?}",
                ctx()
            );
        }
    }

    /// A `Set-Cookie` with a name and no value is still one of ours.
    ///
    /// Found by the soak above at 147,554 rounds. `set_cookie_name` was
    /// `set_cookie_pair().map(name)`, which requires an `=`, so the box could
    /// emit `Set-Cookie: h5i_share` and have it relayed. A conforming browser
    /// ignores that header (RFC 6265 §5.2), so this was a rule with a hole
    /// rather than a leak, and the request side had already closed the same
    /// hole, in the same words, on the way in.
    #[test]
    fn a_set_cookie_with_no_value_is_still_not_the_boxs_to_send() {
        for line in [
            &b"Set-Cookie: h5i_share"[..],
            &b"Set-Cookie: h5i_share_8899"[..],
            &b"Set-Cookie:  h5i_share ; Path=/"[..],
            &b"set-cookie: h5i_share"[..],
        ] {
            assert!(
                sets_a_share_cookie(line),
                "not recognised as ours: {:?}",
                String::from_utf8_lossy(line)
            );
        }
        // And the app's own, valueless or not, still travels.
        for line in [
            &b"Set-Cookie: sid"[..],
            &b"Set-Cookie: h5i_shared_theme"[..],
            &b"Set-Cookie: h5i_share_0"[..],
        ] {
            assert!(
                !sets_a_share_cookie(line),
                "the app's own was deleted: {:?}",
                String::from_utf8_lossy(line)
            );
        }

        // End to end through the sanitiser both paths use.
        let head = b"HTTP/1.1 200 OK\r\nSet-Cookie: h5i_share\r\nSet-Cookie: sid=9\r\n\r\n";
        let out = strip_share_cookies(head);
        assert!(!String::from_utf8_lossy(&out).contains("h5i_share"));
        assert!(String::from_utf8_lossy(&out).contains("sid=9"));

        // The allowlist still refuses to learn from a valueless header: a
        // cookie a browser never stores is one it never sends back, so an
        // entry for it could only ever match somebody else's.
        let jar = crate::gate::AppCookies::default();
        learn_app_cookies(b"HTTP/1.1 200 OK\r\nSet-Cookie: sid\r\n\r\n", Some(&jar));
        assert!(!jar.knows("sid", ""));
    }

    /// Every value for a header name, as a downstream parser would read them.
    fn header_values(head: &[u8], name: &str) -> Vec<String> {
        head.split(|&b| b == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .skip(1)
            .filter_map(|l| {
                let colon = l.iter().position(|&b| b == b':')?;
                let k = String::from_utf8_lossy(&l[..colon])
                    .trim()
                    .to_ascii_lowercase();
                (k == name).then(|| String::from_utf8_lossy(&l[colon + 1..]).trim().to_string())
            })
            .collect()
    }
}

#[cfg(test)]
mod chunked_fuzz {
    use super::*;
    use crate::fuzz::{chunked_body, rounds, Rng, PIPELINED};

    /// Nothing past the end of a chunked request body reaches the box.
    ///
    /// This is the one place h5i and the box can disagree about where a
    /// *request* ends, which is the definition of request smuggling: the front
    /// authorizes one request and forwards exactly its bytes, and anything past
    /// the body's end is a second request that never passed the gate.
    ///
    /// The oracle is an independent reader, deliberately more permissive than
    /// `chunk_size`: it accepts the bare-LF and whitespace-padded size lines h5i
    /// refuses, because a *lenient* box is the dangerous counterparty. If h5i
    /// forwards bytes this reader places after the body's end, they are bytes
    /// some server would read as a new request.
    #[tokio::test]
    async fn a_chunked_body_never_carries_a_second_request_into_the_box() {
        let mut rng = Rng::new(0xC4A4);
        let mut accepted = 0usize;
        let mut refused = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let body = chunked_body(&mut one);
            let ctx = || format!("round {i}, seed {seed:#x}, body {body:?}");

            let mut peer = body.as_bytes();
            let mut out: Vec<u8> = Vec::new();
            let counted = std::sync::atomic::AtomicU64::new(0);
            let r = forward_chunked(&mut peer, &mut out, &[], &counted).await;

            // Whatever happened, what reached the box is a prefix of what the
            // client sent. This proxy forwards, it does not rewrite.
            assert!(
                body.as_bytes().starts_with(&out),
                "the box received bytes the client did not send: {} -> {:?}",
                ctx(),
                String::from_utf8_lossy(&out)
            );
            // And the counter describes those bytes rather than some others.
            assert_eq!(
                counted.load(std::sync::atomic::Ordering::Relaxed) as usize,
                out.len(),
                "the byte count and the bytes disagree: {}",
                ctx()
            );

            // The property, and it is about the *boundary* rather than about a
            // string: bytes past where the body ends are a second request, and
            // bytes before it are this request's, however they read. A client
            // that declares a chunk long enough to cover the tail has made
            // those bytes its own body, and the box's parser agrees, so
            // "SMUGGLED must never appear" is the wrong assertion and this is
            // the right one.
            let end = body_end(body.as_bytes());
            if let Some(end) = end {
                assert!(
                    out.len() <= end,
                    "the box was given bytes from past the end of the body ({} of {end}): {}",
                    out.len(),
                    ctx()
                );
            }

            match r {
                Ok(()) => {
                    accepted += 1;
                    // A completed forward stopped exactly where an independent,
                    // and deliberately *more permissive*, reader places the
                    // end. Accepting a body this reader cannot end is h5i
                    // calling a request complete at a boundary no server
                    // recognises, which is how the missing CRLF check showed up.
                    let end = end.unwrap_or_else(|| {
                        panic!(
                            "h5i accepted a body this reader cannot find the end of: {}",
                            ctx()
                        )
                    });
                    assert_eq!(
                        out.len(),
                        end,
                        "h5i stopped somewhere other than the end of the body: {}",
                        ctx()
                    );
                    // And the tail really was left behind.
                    assert!(
                        !String::from_utf8_lossy(&out).contains("SMUGGLED"),
                        "a completed body carried the pipelined request: {} -> {:?}",
                        ctx(),
                        String::from_utf8_lossy(&out)
                    );
                }
                Err(_) => refused += 1,
            }
        }

        let n = rounds();
        if n < 1_000 {
            return;
        }
        assert!(
            accepted * 20 > n,
            "almost nothing was accepted, so the assertion about where an accepted body \
             stops ran on nothing: {accepted} of {n}"
        );
        assert!(
            refused * 20 > n,
            "almost nothing was refused, so the half of this that keeps a malformed body \
             out of the box was barely exercised: {refused} of {n}"
        );
    }

    /// Where a chunked body ends, read leniently.
    ///
    /// Independent of `chunk_size` on purpose, and *more* permissive: bare LF
    /// endings, whitespace around the size, and a hex value of any length are
    /// accepted here and refused there. A lenient box is the dangerous
    /// counterparty, so the oracle has to be the lenient one.
    ///
    /// `None` means this reader cannot find an end either, in which case h5i
    /// must not have claimed to.
    fn body_end(b: &[u8]) -> Option<usize> {
        let mut at = 0usize;
        loop {
            let (line, next) = line_at(b, at)?;
            let hex: String = String::from_utf8_lossy(line)
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if hex.is_empty() || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let size = u64::from_str_radix(&hex, 16).ok()?;
            at = next;
            if size == 0 {
                // Trailers, then the blank line that closes them.
                loop {
                    let (t, next) = line_at(b, at)?;
                    at = next;
                    if t.is_empty() {
                        return Some(at);
                    }
                }
            }
            at = at.checked_add(usize::try_from(size).ok()?)?;
            // The CRLF (or LF) that closes the data.
            let (closing, next) = line_at(b, at)?;
            if !closing.is_empty() {
                return None;
            }
            at = next;
        }
    }

    /// One line and the offset past its ending, accepting CRLF or a bare LF.
    fn line_at(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
        let rest = b.get(at..)?;
        let nl = rest.iter().position(|&c| c == b'\n')?;
        let line = rest[..nl].strip_suffix(b"\r").unwrap_or(&rest[..nl]);
        Some((line, at + nl + 1))
    }

    /// The two chunk readers in this file agree about where a body ends.
    ///
    /// `forward_chunked` decides it for a *request* and `Scan` for a
    /// *response*, they are written separately, and both were loose about the
    /// same two bytes, which is what two copies of a parser do. Nothing forces
    /// them to agree except this.
    #[test]
    fn the_request_and_response_readers_end_a_body_in_the_same_place() {
        let mut rng = Rng::new(0x5CA7);
        let mut agreed = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let body = chunked_body(&mut one);
            let ctx = || format!("round {i}, seed {seed:#x}, body {body:?}");

            let mut s = Scan::new();
            let consumed = s.feed(body.as_bytes());
            let scan_end = s.is_done().then_some(consumed);

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let forwarded = rt.block_on(async {
                let mut peer = body.as_bytes();
                let mut out: Vec<u8> = Vec::new();
                let counted = std::sync::atomic::AtomicU64::new(0);
                forward_chunked(&mut peer, &mut out, &[], &counted)
                    .await
                    .ok()
                    .map(|()| out.len())
            });

            assert_eq!(
                scan_end,
                forwarded,
                "the two chunk readers disagree about where this body ends: {}",
                ctx()
            );
            if scan_end.is_some() {
                agreed += 1;
            }
        }
        let n = rounds();
        if n >= 1_000 {
            assert!(
                agreed * 20 > n,
                "almost no body was followed to its end by either reader, so this compared \
                 two `None`s: {agreed} of {n}"
            );
        }
    }

    /// The generator really does put a pipelined request on every body, so the
    /// assertion above has something to look for.
    #[test]
    fn the_generator_offers_a_second_request_to_smuggle() {
        let mut rng = Rng::new(1);
        for _ in 0..50 {
            assert!(chunked_body(&mut rng).contains(PIPELINED));
        }
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(5)
    }

    /// A chunked response ends at its terminating chunk, and what the box wrote
    /// after it does not reach the visitor.
    ///
    /// The `Content-Length` path has always clamped, being arithmetic, and the
    /// chunked path relayed the lot: a second status line, a `Set-Cookie` of the
    /// box's choosing, a `Connection` of its choosing, all having skipped
    /// `close_the_connection` and `strip_share_cookies`. A conforming client
    /// discards it; what is wrong is that whether the sanitiser applied was the
    /// box's decision.
    #[tokio::test]
    async fn nothing_the_box_wrote_after_the_terminating_chunk_reaches_the_visitor() {
        let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let tail =
            "HTTP/1.1 200 OK\r\nSet-Cookie: h5i_share=x\r\nConnection: keep-alive\r\n\r\nEXTRA";

        // Arriving with the head, and arriving later off the box's socket:
        // both go through the same clamp, and both used to relay the tail.
        for (rest, upstream) in [
            (format!("5\r\nhello\r\n0\r\n\r\n{tail}"), String::new()),
            (String::new(), format!("5\r\nhello\r\n0\r\n\r\n{tail}")),
            ("5\r\nhello\r\n".to_string(), format!("0\r\n\r\n{tail}")),
        ] {
            let mut out: Vec<u8> = Vec::new();
            let to_peer = std::sync::atomic::AtomicU64::new(0);
            let truncated = std::sync::atomic::AtomicBool::new(false);
            relay_response(
                tokio::io::empty(),
                &mut out,
                std::io::Cursor::new(upstream.into_bytes()),
                head.clone(),
                rest.clone().into_bytes(),
                true,
                false,
                &to_peer,
                &truncated,
            )
            .await
            .expect("relay");
            let text = String::from_utf8_lossy(&out).into_owned();
            assert!(text.ends_with("0\r\n\r\n"), "{text:?}");
            assert!(
                !text.contains("EXTRA"),
                "a second response reached the visitor: {text:?}"
            );
            assert!(
                !text.contains("h5i_share"),
                "an unsanitised head reached the visitor: {text:?}"
            );
            // The body it *was* owed is intact, and the count matches the
            // bytes: a clamp that lied about either would be a truncation.
            assert!(text.contains("5\r\nhello\r\n"), "{text:?}");
            assert_eq!(
                to_peer.load(std::sync::atomic::Ordering::Relaxed) as usize,
                out.len(),
                "the byte count and the bytes disagree"
            );
            assert!(
                !truncated.load(std::sync::atomic::Ordering::Relaxed),
                "a complete response was recorded as truncated"
            );
        }

        // And a body whose framing the scanner cannot follow still falls back
        // to being framed by the close, which is what it did before the
        // scanner existed. Nothing is clamped away on a guess.
        let mut out: Vec<u8> = Vec::new();
        let to_peer = std::sync::atomic::AtomicU64::new(0);
        let truncated = std::sync::atomic::AtomicBool::new(false);
        relay_response(
            tokio::io::empty(),
            &mut out,
            tokio::io::empty(),
            head,
            b"not chunk framing at all".to_vec(),
            true,
            false,
            &to_peer,
            &truncated,
        )
        .await
        .expect("relay");
        assert!(
            String::from_utf8_lossy(&out).contains("not chunk framing at all"),
            "a body this could not follow was thrown away"
        );
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
        assert!(
            !text.contains("SMUGGLED"),
            "a pipelined request was forwarded: {text}"
        );
    }

    /// A chunk whose data is not closed by CRLF is not a chunk.
    ///
    /// The size and the two bytes after it used to be one count, `size + 2`
    /// bytes consumed and forwarded without looking, so `1\r\nAXX0\r\n\r\n` was
    /// a *complete request*: one byte of data, `XX` where its CRLF belongs, and
    /// the terminating `0` read out of the middle of the stream. Every
    /// conforming server rejects that, so h5i was declaring a request finished
    /// at a boundary nothing else recognises.
    ///
    /// Found by the chunked fuzz, which reads the same stream more leniently
    /// than h5i does and could not find an end where h5i claimed one.
    #[tokio::test]
    async fn a_chunk_the_client_did_not_close_is_not_a_finished_request() {
        for body in [
            &b"1\r\nAXX0\r\n\r\n"[..],
            &b"1\r\nAX\r\n0\r\n\r\n"[..],
            &b"2\r\nabXX0\r\n\r\n"[..],
            // A bare LF where the closing CRLF belongs: one byte to a parser
            // splitting on CRLF and a line ending to almost everything else,
            // which is the disagreement the head reader refuses by name.
            &b"1\r\nA\n0\r\n\r\n"[..],
        ] {
            let mut peer = body;
            let mut out: Vec<u8> = Vec::new();
            let counted = std::sync::atomic::AtomicU64::new(0);
            assert!(
                forward_chunked(&mut peer, &mut out, &[], &counted)
                    .await
                    .is_err(),
                "accepted a chunk that is not closed by CRLF: {:?}",
                String::from_utf8_lossy(body)
            );
        }

        // And the well-formed ones still work, including a chunk of zero
        // length inside the body and an extension on the size line.
        for body in [
            &b"1\r\nA\r\n0\r\n\r\n"[..],
            &b"5;a=b\r\nhello\r\n0\r\n\r\n"[..],
            &b"0\r\n\r\n"[..],
        ] {
            let mut peer = body;
            let mut out: Vec<u8> = Vec::new();
            let counted = std::sync::atomic::AtomicU64::new(0);
            forward_chunked(&mut peer, &mut out, &[], &counted)
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "refused a well-formed body {:?}: {e}",
                        String::from_utf8_lossy(body)
                    )
                });
            assert_eq!(out, body, "a well-formed body was not forwarded verbatim");
        }
    }

    #[tokio::test]
    async fn chunk_framing_that_is_not_framing_is_refused() {
        for body in [
            &b"zz\r\nhello\r\n"[..],
            &b"5\r\nhel"[..], // hung up mid-chunk
            &b"0\r\n"[..],    // no closing blank line
        ] {
            let mut peer = body;
            let mut out: Vec<u8> = Vec::new();
            let counted = std::sync::atomic::AtomicU64::new(0);
            assert!(
                forward_chunked(&mut peer, &mut out, &[], &counted)
                    .await
                    .is_err(),
                "accepted {body:?}"
            );
        }
    }

    #[test]
    fn a_chunk_line_is_held_to_the_rules_a_header_line_is() {
        // The gate refuses a bare LF in the head because it is how a line means
        // one thing here and another to the box. A chunk-size line is the only
        // thing telling this proxy where the request ends, so it is the last
        // place to be relaxed about it.
        assert_eq!(chunk_size(b"5\r\n"), Some(5));
        for bad in [
            &b" 5 \r\n"[..], // the grammar allows no whitespace
            &b"\t5\r\n"[..],
            &b"5\x0c\r\n"[..], // form feed, which `trim` used to eat
            &b"5\r\r\n"[..],   // an embedded bare CR
            &b"5;x\nGET / HTTP/1.1\r\n"[..],
            &b"5;x\x00y\r\n"[..],
        ] {
            assert_eq!(chunk_size(bad), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn a_websocket_handshake_survives_the_cookie_filter_byte_for_byte() {
        // The `101` head goes through the filter like every other response
        // head, and a client checks `Sec-WebSocket-Accept` against a hash it
        // computed itself, so rebuilding this head has to come out identical
        // when there is nothing to strip.
        let head = b"HTTP/1.1 101 Switching Protocols\r\n\
                     Upgrade: websocket\r\n\
                     Connection: Upgrade\r\n\
                     Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                     Sec-WebSocket-Protocol: vite-hmr\r\n\
                     Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n\r\n";
        assert_eq!(strip_share_cookies(head), head.to_vec());
    }

    #[tokio::test]
    async fn a_real_trailer_is_forwarded_and_still_ends_the_body() {
        // Trailers are rare and legal, and refusing them would break anything
        // that puts a checksum after the body.
        let body = b"5\r\nhello\r\n0\r\nX-Checksum: abc\r\n\r\nGET /SMUGGLED HTTP/1.1\r\n\r\n";
        let mut peer = &body[..];
        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        forward_chunked(&mut peer, &mut out, &[], &counted)
            .await
            .expect("a chunked body with a real trailer");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("X-Checksum: abc"), "{text}");
        assert!(!text.contains("SMUGGLED"), "{text}");
    }

    #[tokio::test]
    async fn a_quoted_chunk_extension_is_not_mistaken_for_junk() {
        // `5;sig="a b;c"` is legal, and the space and the semicolon inside the
        // quotes are exactly what a stricter size parser would trip over.
        let body = b"5;sig=\"a b;c\"\r\nhello\r\n0\r\n\r\n";
        let mut peer = &body[..];
        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        forward_chunked(&mut peer, &mut out, &[], &counted)
            .await
            .expect("a quoted chunk extension");
    }

    #[test]
    fn a_trailer_is_held_to_the_same_rules() {
        assert!(is_trailer(b"\r\n"));
        assert!(is_trailer(b"X-Checksum: abc\r\n"));
        for bad in [
            &b"X: a\nGET / HTTP/1.1\r\n"[..],
            &b"X: a\x00b\r\n"[..],
            &b" X: folded\r\n"[..],
            &b"no colon\r\n"[..],
            &b"X\x0c: a\r\n"[..],
        ] {
            assert!(!is_trailer(bad), "accepted {bad:?}");
        }
    }

    #[tokio::test]
    async fn a_chunk_size_at_the_edge_of_the_type_is_refused() {
        // `u64::MAX` is a well-formed chunk header, and `size + 2` would panic
        // in a debug build and wrap to one in a release build. Forwarding two
        // bytes and then reading the peer's body as chunk headers.
        let body = b"ffffffffffffffff\r\n";
        let mut peer = &body[..];
        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        assert!(forward_chunked(&mut peer, &mut out, &[], &counted)
            .await
            .is_err());
    }

    /// `BODY_LIFETIME`'s own doc names the attack it exists to stop: "a peer
    /// declaring a body of `u64::MAX` and dribbling a byte a minute holds a slot
    /// for the life of its ticket". The chunked path checked it only at the top
    /// of the per-chunk loop, and a single chunk header declaring `u64::MAX`
    /// never returns there, so the bound was one the peer chose whether to be
    /// bound by. The `Content-Length` path beside it has always checked on every
    /// read.
    #[tokio::test]
    async fn one_enormous_chunk_cannot_outlive_the_body_deadline() {
        // A reader that always has another byte, exactly like a peer dribbling.
        struct Endless;
        impl tokio::io::AsyncRead for Endless {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                b: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                b.put_slice(b"x");
                std::task::Poll::Ready(Ok(()))
            }
        }

        let mut out: Vec<u8> = Vec::new();
        let counted = std::sync::atomic::AtomicU64::new(0);
        let started = std::time::Instant::now();
        // One chunk header for the largest body the type can hold, then bytes
        // forever.
        //
        // No `tokio::time::timeout` wrapper, because it could not fire: this
        // reader is always `Ready`, so the task never yields and the runtime never
        // advances a timer. That is the point. The *only* thing that can end this
        // loop is the synchronous deadline check inside it, and against a build
        // without that check this test hangs rather than failing.
        let err = forward_chunked_within(
            &mut Endless,
            &mut out,
            b"ffffffffffffffff\r\n",
            &counted,
            Duration::from_millis(50),
        )
        .await
        .expect_err("a body past its lifetime is refused");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(started.elapsed() < Duration::from_secs(10), "it took {:?}", started.elapsed());
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
        // that body holds the connection until the idle timeout. On the
        // commonest response there is.
        assert!(has_no_body(
            "GET",
            b"HTTP/1.1 304 Not Modified\r\nContent-Length: 42\r\n\r\n"
        ));
        assert!(has_no_body("GET", b"HTTP/1.1 204 No Content\r\n\r\n"));
        assert!(has_no_body(
            "HEAD",
            b"HTTP/1.1 200 OK\r\nContent-Length: 9000\r\n\r\n"
        ));
        assert!(has_no_body("head", b"HTTP/1.1 200 OK\r\n\r\n"));
        assert!(!has_no_body(
            "GET",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n"
        ));
    }

    #[test]
    fn a_box_that_stalled_and_a_box_that_hung_up_are_told_apart() {
        // They call for opposite handling: a stalled box is wedged and the
        // visitor gets a `502`, a closed one has already written the answer
        // that explains itself (a `413`, a `401`) and the right move is to go
        // and read it rather than invent one.
        let stalled = std::io::Error::other(BoxWrite { stalled: true });
        let closed = std::io::Error::other(BoxWrite { stalled: false });
        assert_eq!(box_write_failure(&stalled), Some(true));
        assert_eq!(box_write_failure(&closed), Some(false));
        // And a failure that was not a write to the box is neither. An earlier
        // version signalled this with `ErrorKind::BrokenPipe`, which the kernel
        // also produces, so a peer's own disconnect could be read as the box's.
        assert_eq!(
            box_write_failure(&std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            None
        );
        assert_eq!(
            box_write_failure(&std::io::Error::from(std::io::ErrorKind::TimedOut)),
            None
        );
    }

    #[test]
    fn a_stalled_write_names_the_side_that_stalled() {
        // Both directions stall as `TimedOut`, and they deserve opposite
        // answers: a peer that stopped reading gets nothing more, a *box* that
        // stopped reading is a `502`. Telling a visitor their upload was too
        // slow when the dev server stopped draining its socket sends them to
        // fix something that is not broken.
        let peer = timed_out_response();
        let boxed = unreachable_box_response();
        assert!(peer.starts_with("HTTP/1.1 408 "), "{peer}");
        assert!(boxed.starts_with("HTTP/1.1 502 "), "{boxed}");
        assert!(boxed.contains("stopped reading"), "{boxed}");
    }

    #[test]
    fn every_built_in_answer_declares_the_length_it_has() {
        for r in [
            unfinished_response(),
            timed_out_response(),
            unreachable_box_response(),
        ] {
            let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
            assert!(
                head.contains(&format!("Content-Length: {}", body.len())),
                "{r}"
            );
            assert!(head.contains("Connection: close"), "{r}");
        }
        assert!(timed_out_response().starts_with("HTTP/1.1 408 "));
    }

    #[test]
    fn a_response_head_gets_the_line_discipline_the_request_head_gets() {
        // A bare CR walked a `Connection: keep-alive` and a `Set-Cookie` past
        // both response filters, because `rebuild_head` splits on LF and
        // `header_name` reads whatever that produced. The request side refuses
        // this outright and says why; the response side used to normalise and
        // hope.
        assert!(head_is_well_formed(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-A: b\r\n\r\n"
        ));
        for bad in [
            &b"HTTP/1.1 200 OK\r\nX-Pad: a\rConnection: keep-alive\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\nX-A: b\r\n\r\n"[..],
            // Obs-fold: `header_name` trims, so a continuation either matches a
            // filter and vanishes, or survives a dropped line and reattaches
            // itself to the header above it.
            &b"HTTP/1.1 200 OK\r\nX-Pad: a\r\n Connection: keep-alive\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nConnection:\r\n\tkeep-alive\r\n\r\n"[..],
        ] {
            assert!(
                !head_is_well_formed(bad),
                "accepted {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_head_the_box_never_finished_is_not_relayed_raw() {
        // Relaying an unfinished head verbatim put the whole sanitiser behind a
        // door the box holds: stop talking mid-head and `Connection`, the
        // share-cookie filter and the length checks are all skipped.
        let r = unfinished_response();
        let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.starts_with("HTTP/1.1 502 "), "{r}");
        assert!(
            head.contains(&format!("Content-Length: {}", body.len())),
            "{r}"
        );
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

    /// Shorthand for the two-field framing, so a test reads as one assertion.
    fn framed(body: Body, strip_lengths: bool) -> Framing {
        Framing {
            body,
            strip_lengths,
        }
    }

    #[test]
    fn a_bodyless_answer_with_ambiguous_lengths_is_still_sanitised() {
        // A `304` is the commonest response a dev server makes, and "this
        // branch has no body so the headers cannot hurt" is how an invariant
        // ends up holding on one path and not the other.
        let head = b"HTTP/1.1 304 Not Modified\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n";
        assert_eq!(response_framing(head), framed(Body::UntilClose, true));
        let text = String::from_utf8(strip_lengths(&close_the_connection(head))).expect("utf8");
        assert!(!text.to_lowercase().contains("content-length"), "{text}");
    }

    #[test]
    fn a_header_value_that_merely_contains_the_word_is_not_chunked() {
        // `contains` would read this as chunked and stop framing on a perfectly
        // good length, which is a truncated download.
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: x-chunked-foo\r\nContent-Length: 4\r\n\r\n"
            ),
            // Not chunked, but it *is* a transfer coding, and a recipient
            // honours one over a length whatever it is called. The length does
            // not travel and the close is the framing.
            framed(Body::UntilClose, true)
        );
        // And a real list still is, with `chunked` last where it must be.
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n"),
            framed(Body::Chunked, false)
        );
    }

    #[test]
    fn identity_transfer_encoding_is_not_chunked() {
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\nContent-Length: 4\r\n\r\n"
            ),
            framed(Body::Length(4), false)
        );
    }

    #[test]
    fn an_interim_answer_is_told_apart_from_the_real_one() {
        assert!(is_informational(b"HTTP/1.1 100 Continue\r\n\r\n"));
        assert!(is_informational(
            b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>\r\n\r\n"
        ));
        // A `101` is an upgrade, which is a different thing entirely.
        assert!(!is_informational(
            b"HTTP/1.1 101 Switching Protocols\r\n\r\n"
        ));
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
            framed(Body::Length(17), false)
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            framed(Body::Chunked, false)
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\n\r\n"),
            framed(Body::UntilClose, false)
        );
        // A length beside a `Transfer-Encoding` is the shape that truncated a
        // chunked body to five bytes of its own framing: every client honours
        // the encoding over the length, and this proxy honoured the length.
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            framed(Body::Chunked, true)
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"),
            framed(Body::UntilClose, true)
        );
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\n"),
            framed(Body::UntilClose, true)
        );
        // A transfer coding that is not `chunked`, beside a length. Both
        // headers used to reach the visitor with h5i framing on the length,
        // so h5i stopped after four bytes and the client, for which the coding
        // is authoritative, waited for a stream that had already stopped.
        assert_eq!(
            response_framing(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nContent-Length: 4\r\n\r\n"
            ),
            framed(Body::UntilClose, true)
        );
        // `chunked` not last: the octets on the wire are not chunk-framed, so
        // there is no terminating chunk to watch for.
        assert_eq!(
            response_framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n"),
            framed(Body::UntilClose, false)
        );
    }

    #[test]
    fn an_ambiguous_length_is_not_passed_on_to_the_visitor() {
        // Two `Content-Length` headers is a response-smuggling shape the
        // request side refuses outright. Relaying it to the browser is the same
        // disagreement, one hop further out.
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\nX-A: b\r\n\r\n";
        let out = strip_lengths(&close_the_connection(head));
        let text = String::from_utf8(out).expect("utf8");
        assert!(!text.to_lowercase().contains("content-length"), "{text}");
        assert!(text.contains("X-A: b"), "{text}");
        assert!(text.contains("Connection: close"), "{text}");
    }

    #[test]
    fn the_box_cannot_clobber_the_visitors_share_cookie() {
        // It cannot guess a token, so the worst it could do is clear one, but
        // cookies ignore the port, so on the joining side one box could log a
        // visitor out of a different share.
        let head = b"HTTP/1.1 200 OK\r\nSet-Cookie: h5i_share_1234=junk; Path=/\r\n\
                     Set-Cookie: sid=9\r\n\r\n";
        let text = String::from_utf8(close_the_connection(head)).expect("utf8");
        assert!(!text.contains("h5i_share"), "{text}");
        assert!(text.contains("Set-Cookie: sid=9"), "{text}");
    }

    /// Which cookies the box has set, read off its own responses.
    ///
    /// On the joining side's fallback address the visitor's `Cookie` header is
    /// the whole machine's, and this is the only thing that can tell the box's
    /// own cookies from the joiner's. It reads a head the same way the filters
    /// beside it do, so the two cannot disagree about where a name ends.
    #[test]
    fn the_names_the_box_sets_are_learned_off_its_responses() {
        let jar = gate::AppCookies::default();
        let head = b"HTTP/1.1 200 OK\r\n\
                     Set-Cookie: sid=9; Path=/; HttpOnly\r\n\
                     Set-Cookie:   spaced =1\r\n\
                     Set-Cookie: h5i_share_1234=junk\r\n\
                     X-Not-A-Cookie: nope=1\r\n\r\n";
        learn_app_cookies(head, Some(&jar));

        assert!(jar.knows("sid", "9"));
        // The value is part of it: the same name with somebody else's value is
        // not this box's cookie. See `gate::AppCookies`.
        assert!(!jar.knows("sid", "joiners-own-secret"));
        // The attributes are not names, and neither is a header that merely
        // looks like one.
        assert!(!jar.knows("Path", "/"));
        assert!(!jar.knows("HttpOnly", ""));
        assert!(!jar.knows("nope", "1"));
        // Whitespace around the name is the server's formatting, not part of
        // it: a browser sends `spaced=1` back and an untrimmed list would drop
        // it on every request.
        assert!(jar.knows("spaced", "1"));
        // Never ours, however it arrives.
        assert!(!jar.knows("h5i_share_1234", "junk"));

        // A value carrying `=` is learned whole. Base64 padding makes this the
        // everyday case for a session id, and learning only up to the first
        // `=` would drop the cookie on every request the browser sent it on.
        let padded = b"HTTP/1.1 200 OK\r\nSet-Cookie: s=YWJj==; Path=/\r\n\r\n";
        learn_app_cookies(padded, Some(&jar));
        assert!(jar.knows("s", "YWJj=="));

        // `None` is the ordinary case, a jar this share has to itself, and
        // it must not fall over on a head it is not keeping.
        learn_app_cookies(head, None);
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
        assert_eq!(response_framing(&head), framed(Body::Length(2), false));
    }
}

/// Write a response and close politely.
pub async fn respond<S>(sock: &mut S, body: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Deadlined like every other write. Small ones into an empty send buffer,
    // so a stall needs a receive window closed from the first byte, but "every
    // write has a deadline" is either true or it is not.
    let _ = write_timed(sock, body.as_bytes()).await;
    let _ = sock.flush().await;
    // And drained like every other close. This is the writer for every
    // front-door answer (the `401`, the `503`, the invite redirect) and a
    // refusal is precisely when a peer is most likely to still be sending a
    // body nobody read, which is what turns the close into a reset that
    // destroys the answer.
    let (r, w) = tokio::io::split(sock);
    finish_with(r, w).await;
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
        let raw = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n",
            "a".repeat(gate::MAX_HEAD + 10)
        );
        let mut r = raw.as_bytes();
        // `Refused`, not `Silent`, and the distinction is what the receipt is
        // owed: this is a peer that sent 32 KB of headers, which is the shape
        // the gate exists to refuse, and every one of them used to be dropped
        // with no line anywhere. A peer that said nothing still gets none.
        assert_eq!(read_head(&mut r).await, Err(NoHead::Refused));
    }

    /// The reader tells "nothing arrived" from "that is not a head".
    ///
    /// Only the second belongs in an ingress receipt, and before this the
    /// caller could not tell them apart, so it recorded neither: a 32 KB header
    /// block and a TLS `ClientHello` sent to the plaintext front were both as
    /// invisible as a browser's speculative preconnect.
    #[tokio::test]
    async fn a_peer_that_said_nothing_is_not_a_peer_that_was_refused() {
        // Silent: opened and closed, saying nothing at all.
        let mut nothing = &b""[..];
        assert_eq!(read_head(&mut nothing).await, Err(NoHead::Silent));

        // Refused: started a head and stopped. The peer made a request; it did
        // not finish it.
        let mut partial = &b"GET / HTTP/1.1\r\nHost: x\r\n"[..];
        assert_eq!(read_head(&mut partial).await, Err(NoHead::Refused));

        // Refused: a complete head that is not text. A TLS hello aimed at a
        // plaintext listener is the ordinary way this happens.
        let mut tls = &b"\x16\x03\x01\x00\xff\x01\xff\xff\r\n\r\n"[..];
        assert_eq!(read_head(&mut tls).await, Err(NoHead::Refused));

        // And a head that is a head is still one.
        let mut good = &b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"[..];
        assert!(read_head(&mut good).await.is_ok());
    }

    #[test]
    fn the_first_visit_is_bounced_so_the_token_leaves_the_url() {
        let head = "GET /dash?h5i=abc123&tab=2 HTTP/1.1\r\nHost: x\r\n\r\n";
        let Next::Respond(r, _) = decide(head, gate::COOKIE, yes, true, None) else {
            panic!("a token in the URL must be redirected, not proxied");
        };
        assert!(r.contains("302"));
        assert!(r.contains("Location: /dash?tab=2"));
        assert!(r.contains("Set-Cookie: h5i_share=abc123"));
    }

    #[test]
    fn a_request_with_the_cookie_is_proxied_without_it() {
        let head = "GET /app.js HTTP/1.1\r\nHost: x\r\nCookie: h5i_share=abc123; sid=9\r\n\r\n";
        let Next::Proxy { head: up, req } = decide(head, gate::COOKIE, yes, true, None) else {
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
        let (Next::Respond(a, _), Next::Respond(b, _)) = (
            decide(none, gate::COOKIE, yes, true, None),
            decide(wrong, gate::COOKIE, yes, true, None),
        ) else {
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
        let Next::Proxy { req, .. } = decide(head, gate::COOKIE, yes, true, None) else {
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
            None,
        );
        assert!(matches!(out, Next::Respond(..)));
        assert!(!called, "malformed input must not reach the grant table");
    }

    #[test]
    fn loopback_gets_a_cookie_a_loopback_browser_will_actually_store() {
        let head = "GET /?h5i=abc123 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let Next::Respond(r, _) = decide(head, gate::COOKIE, yes, false, None) else {
            panic!("redirect expected");
        };
        assert!(!r.contains("Secure"));
    }
}
