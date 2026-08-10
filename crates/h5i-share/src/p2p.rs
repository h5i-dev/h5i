//! Transport one: peer to peer, over iroh.
//!
//! Both sides run h5i, and the bytes go between them — QUIC, end-to-end
//! encrypted, hole-punched to a direct path when the networks allow it and
//! carried by a relay when they do not. The relay moves sealed packets: it
//! learns both addresses, the timing and the volume, and none of the content.
//! Nothing about the shared app is visible to any third party on this path,
//! which is why it is the default and the tunnel is not.
//!
//! What is on the wire, in order:
//!
//! 1. QUIC, with an ALPN of [`crate::wire::ALPN`]. A peer that does not speak
//!    exactly this is dropped by the transport before either side is asked
//!    anything.
//! 2. Per stream, a fixed-size greeting carrying the ticket secret
//!    ([`crate::wire`]). One stream is one TCP connection into the box, and it
//!    is authorized on its own — so revoking a grant stops the *next*
//!    connection, not just the next session.
//! 3. Raw bytes, both ways, until someone hangs up.
//!
//! Live connections are not left to the per-stream check alone: a watchdog
//! closes the whole QUIC connection when the grant table stops admitting
//! anyone, so `revoke` reaches a WebSocket that is already open.

use std::sync::Arc;
use std::time::Duration;

use h5i_error::H5iError;
use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr};

use crate::bridge::{Bridge, Path};
use crate::wire;

/// How often the watchdog asks whether the share still admits anyone.
const REVOKE_POLL: Duration = Duration::from_secs(1);

/// How long an unauthenticated peer may take to send its 69-byte greeting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// How many QUIC connections this process will carry at once.
///
/// Deliberately larger than the box's connection ceiling: several of these are
/// ordinary (a browser, a second tab), and one connection carries many streams.
/// It is a backstop against an endpoint anyone can dial, not a usage limit.
const MAX_LIVE_CONNECTIONS: usize = 256;

/// How long a connection may sit with no stream ever authorized.
///
/// iroh keeps a connection alive from this side, so a peer that completes the
/// handshake and then says nothing never idles out on its own. Nothing has been
/// authorized, so nothing is lost by hanging up.
const UNAUTHENTICATED_GRACE: Duration = Duration::from_secs(30);

/// How long to wait for a closing connection's pumps to report their byte
/// counts before giving up on them.
const STREAM_DRAIN: Duration = Duration::from_secs(5);

/// How long `--direct-only` waits for hole punching before giving up.
///
/// Direct paths are usually established within a second or two of the first
/// packets; a peer that has not got one by now is behind something that will
/// not yield, and the honest move is to fail rather than to keep trying while
/// the human wonders what is happening.
const DIRECT_WAIT: Duration = Duration::from_secs(12);

/// Bind an endpoint that can accept shares.
pub async fn bind_sharer() -> Result<Endpoint, H5iError> {
    Endpoint::builder(presets::N0)
        .alpns(vec![wire::ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| H5iError::Metadata(format!("could not start the peer-to-peer endpoint: {e}")))
}

/// Bind an endpoint that only dials.
pub async fn bind_joiner() -> Result<Endpoint, H5iError> {
    Endpoint::bind(presets::N0)
        .await
        .map_err(|e| H5iError::Metadata(format!("could not start the peer-to-peer endpoint: {e}")))
}

/// Wait until this endpoint knows how it can be reached, then describe it.
///
/// Done before the ticket is printed rather than after: a ticket minted before
/// the endpoint has any addresses is a ticket that names nowhere, and the
/// person holding it has no way to tell that from a network problem on their
/// end.
pub async fn addressing(ep: &Endpoint) -> Result<(String, serde_json::Value), H5iError> {
    ep.online().await;
    let addr = ep.addr();
    let id = addr.id.to_string();
    let value = serde_json::to_value(&addr)?;
    Ok((id, value))
}

fn parse_addr(value: &serde_json::Value) -> Result<EndpointAddr, H5iError> {
    serde_json::from_value(value.clone()).map_err(|e| {
        H5iError::Metadata(format!(
            "this ticket's addressing is not something this h5i understands ({e}) — the two \
             sides are probably different versions"
        ))
    })
}

/// Which path the bytes are taking right now, as the transport reports it.
///
/// `None` is a real answer and not a synonym for "relayed": a connection has no
/// selected path for a moment after a NAT rebinding or a local address change,
/// and treating that instant as a relay was both closing healthy `--direct-only`
/// connections and stamping a false relay on the receipt. A receipt is
/// evidence, and a wrong relay claim is as wrong as a wrong direct one.
fn observed_path(conn: &Connection) -> Option<Path> {
    for p in conn.paths().iter() {
        if p.is_selected() {
            return Some(if p.is_relay() { Path::Relayed } else { Path::Direct });
        }
    }
    None
}

/// Block until a direct path is carrying this connection, or give up.
async fn wait_for_direct(conn: &Connection) -> bool {
    let deadline = tokio::time::Instant::now() + DIRECT_WAIT;
    loop {
        if observed_path(conn) == Some(Path::Direct) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Serve a share until the process is asked to stop.
///
/// `direct_only` is not a hint. When it is set and hole punching does not
/// produce a direct path, the connection is closed **before any application
/// byte crosses it**, and the peer is told why. A flag that merely preferred a
/// direct path would be worse than no flag: it would let someone believe no
/// third party was on the wire while one was.
pub async fn serve(
    bridge: Arc<Bridge>,
    endpoint: Endpoint,
    direct_only: bool,
) -> Result<(), H5iError> {
    // A ceiling on connections the *host* carries, which is a different number
    // from the sockets into the box. `Bridge::admit` bounds the latter and is
    // taken after authorization; a peer that completes a QUIC handshake and
    // presents no ticket costs a task and a watchdog and passes that check
    // never. Without this, an endpoint anyone can dial is an endpoint anyone
    // can grow this process with.
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_LIVE_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            // Refused at the transport, without a task. There is nothing to say
            // to a peer that has not identified itself — but it is recorded,
            // because a share taken down at its own front door should not read
            // as a share nobody used.
            bridge.record_front_refusal();
            continue;
        };
        let bridge = bridge.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let conn = match incoming.await {
                Ok(c) => c,
                // A half-open connection from a scanner is ordinary noise on a
                // public endpoint, and not worth a line in the sharer's
                // terminal.
                Err(_) => return,
            };
            if let Err(e) = serve_connection(bridge, conn, direct_only).await {
                eprintln!("share: {e}");
            }
        });
    }
    Ok(())
}

async fn serve_connection(
    bridge: Arc<Bridge>,
    conn: Connection,
    direct_only: bool,
) -> Result<(), H5iError> {
    let who = conn.remote_id().to_string();

    if direct_only && !wait_for_direct(&conn).await {
        // Said on both sides. The peer gets a close reason it can print, and
        // the sharer gets a line, because "nothing happened" is the failure
        // mode a person cannot debug.
        conn.close(
            1u32.into(),
            b"h5i: --direct-only, and no direct path could be established",
        );
        eprintln!(
            "share: refused {} — no direct path, and --direct-only means no application traffic \
             is relayed",
            short(&who)
        );
        return Ok(());
    }

    // The first stream that authorizes registers the peer; every later stream
    // on the same connection is counted against the same record.
    let peer_id: Arc<std::sync::Mutex<Option<crate::bridge::PeerId>>> = Default::default();

    // Two jobs this connection's watchdog keeps doing for its whole life, and
    // one it used to do that has moved.
    //
    // **Moved:** revocation. It is decided per *stream* now (`serve_stream`),
    // because a stream knows the grant that admitted it and a connection does
    // not — a connection carrying two grants could only ever watch one of them,
    // and would enforce a revoke of the wrong one.
    //
    // **Kept:** `--direct-only`, because a direct path can die and iroh will
    // fall back to a relay, so a promise checked only at setup is a preference.
    // And keeping the receipt's record of the path honest, because a long-lived
    // stream sampled once at its start would be recorded as direct for a
    // session that spent most of itself on a relay.
    let watchdog = {
        let bridge = bridge.clone();
        let conn = conn.clone();
        let peer_id = peer_id.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + UNAUTHENTICATED_GRACE;
            loop {
                tokio::time::sleep(REVOKE_POLL).await;
                let seen = *peer_id.lock().expect("peer id");

                // A connection that completes the QUIC handshake and then never
                // opens a stream costs a task and a poll per second for as long
                // as it likes — and iroh's own keep-alive means it never idles
                // out. Nothing has been authorized, so nothing is lost by
                // hanging up on it.
                if seen.is_none() && tokio::time::Instant::now() >= deadline {
                    conn.close(4u32.into(), b"h5i: no ticket was presented");
                    return;
                }

                if let (Some(id), Some(p)) = (seen, observed_path(&conn)) {
                    bridge.peer_path(id, p);
                }
                match observed_path(&conn) {
                    // Only a *selected relay path* is evidence of relaying.
                    // "Nothing selected" happens for an instant after a NAT
                    // rebinding, and treating it as a relay closed healthy
                    // connections and libelled honest ones in the receipt.
                    Some(Path::Relayed) => {
                        if direct_only {
                            conn.close(
                                3u32.into(),
                                b"h5i: --direct-only, and the direct path was lost",
                            );
                            return;
                        }
                    }
                    Some(Path::Direct) | Some(Path::Tunnel) | None => {}
                }
            }
        })
    };
    let mut streams = tokio::task::JoinSet::new();
    loop {
        // Reap what has finished. A `JoinSet` only releases a task's allocation
        // and its result when it is joined, so a peer opening and closing
        // streams in a loop would grow this without bound. Reaping here is
        // accept-driven, so a connection that opens streams and then goes quiet
        // holds its finished slots until it ends — bounded by QUIC's own
        // concurrent-stream limit, which is the case this does not need to
        // cover.
        while streams.try_join_next().is_some() {}
        let Ok((send, recv)) = conn.accept_bi().await else {
            break;
        };
        // Spawned, not awaited. A browser opens several connections to one
        // origin and holds them open — the page, its assets, an event source,
        // a hot-reload socket. Serving streams one after another would make
        // every share single-file behind whichever connection is longest-lived,
        // which for a dev server is the one that never ends.
        let bridge = bridge.clone();
        let who = who.clone();
        let peer_id = peer_id.clone();
        let path = observed_path(&conn);
        let conn_for_stream = conn.clone();
        streams.spawn(async move {
            if let Err(e) =
                serve_stream(&bridge, send, recv, &who, path, &peer_id, &conn_for_stream).await
            {
                eprintln!("share: {e}");
            }
        });
    }

    watchdog.abort();
    // Recorded before the drain, not after. The peer has gone the moment
    // `accept_bi` stops answering; noting it afterwards meant the shutdown path
    // — the only path where it matters — almost always finished first and every
    // peer's closing time came out as "still connected at the end".
    let id = *peer_id.lock().expect("peer id");
    if let Some(id) = id {
        bridge.peer_left(id);
    }
    // Drained rather than aborted. Each stream records its byte counts when its
    // pump returns, and aborting drops that future mid-copy — so a share cut
    // off by a revoke would report zero bytes for exactly the long-lived
    // connection a reviewer most wants to see. The connection is already
    // closed, so the pumps are ending anyway; this waits briefly for them to
    // say what they moved, then stops waiting.
    let drained = tokio::time::timeout(STREAM_DRAIN, async {
        while streams.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        streams.abort_all();
    }
    Ok(())
}

async fn serve_stream(
    bridge: &Arc<Bridge>,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    who: &str,
    path: Option<Path>,
    peer_id: &std::sync::Mutex<Option<crate::bridge::PeerId>>,
    conn: &Connection,
) -> Result<(), H5iError> {
    let mut hello = [0u8; wire::HELLO_LEN];
    // Deadlined, because this is the one read an unauthenticated peer gets to
    // make us wait on. Sixty-eight of sixty-nine bytes and then silence would
    // otherwise hold a task for as long as the connection lasted. The HTTP
    // fronts have had this since they were written; this one did not.
    let greeted = tokio::time::timeout(HELLO_TIMEOUT, recv.read_exact(&mut hello)).await;
    if !matches!(greeted, Ok(Ok(()))) {
        return Ok(());
    }
    let Some(secret) = wire::decode_hello(&hello) else {
        let _ = send.write_all(&[wire::REPLY_DENIED]).await;
        return Ok(());
    };
    let grant = match bridge.authorize(&secret) {
        Ok(g) => g,
        Err(denied) => {
            let _ = send.write_all(&[wire::REPLY_DENIED]).await;
            let _ = send.finish();
            eprintln!("share: refused {} — {}", short(who), denied.explain());
            return Ok(());
        }
    };

    // Registered before anything is dialled, so an authorized peer always
    // appears in the receipt — even if the box turns out to have nothing
    // listening. Doing it after the dial left a share whose dev server was
    // down reading as one nobody ever tried to use, which is the tunnel front's
    // behaviour inverted for no reason.
    let id = {
        let mut slot = peer_id.lock().expect("peer id");
        *slot.get_or_insert_with(|| bridge.peer_joined(short(who), &grant, path))
    };
    // Recorded only when something actually observed it. Feeding the join-time
    // value back through `peer_path` marked a guess as an observation, which is
    // the thing that flag exists to prevent.
    if let Some(p) = path {
        bridge.peer_path(id, p);
    }

    // Authorized, but the share may already be carrying all it will. Checked
    // before the dialer rather than after, so a flood costs a permit lookup
    // rather than a connection into the box.
    let Some(_slot) = bridge.admit() else {
        let _ = send.write_all(&[wire::REPLY_BUSY]).await;
        let _ = send.finish();
        return Ok(());
    };

    // Only now is there a socket into the box. Everything above this line runs
    // for an unauthenticated peer; nothing above it touches the box.
    //
    // On a blocking pool because it is blocking: the dialer talks to its helper
    // over a socketpair and waits for the connect to come back. Microseconds on
    // loopback, but a runtime worker parked on a syscall is a worker not
    // serving the other connections of the same page.
    let upstream = {
        let bridge2 = bridge.clone();
        let opened = tokio::task::spawn_blocking(move || bridge2.open_upstream())
            .await
            .map_err(|e| H5iError::Metadata(format!("the box dialer panicked: {e}")))?;
        match opened {
            Ok(s) => s,
            Err(e) => {
                // A good ticket that found nothing listening. Told to the peer
                // as its own answer — "ask for a new ticket" would send them
                // chasing something that is not the problem — and recorded,
                // because otherwise a share where the dev server was down reads
                // as one nobody ever tried to use.
                bridge.record_unreachable();
                let _ = send.write_all(&[wire::REPLY_UNREACHABLE]).await;
                let _ = send.finish();
                return Err(e);
            }
        }
    };
    upstream.set_nonblocking(true)?;
    let upstream = tokio::net::TcpStream::from_std(upstream)?;

    bridge.peer_connection(id);

    send.write_all(&[wire::REPLY_OK])
        .await
        .map_err(|e| H5iError::Metadata(format!("could not answer a peer: {e}")))?;

    let (up_r, up_w) = upstream.into_split();
    // A raw pipe, deliberately, and worth saying why when the tunnel front goes
    // to such lengths not to be one. There the gate runs because a single TCP
    // connection is shared by `cloudflared`'s pool across visitors, so "this
    // connection was authorized" says nothing about the request now arriving on
    // it. Here the unit of authorization *is* this stream: it carried its own
    // ticket, one greeting, one grant. Everything on it comes from the peer
    // that ticket admitted, and a peer with a ticket may make as many requests
    // as it likes — that is what the ticket is. The HTTP framing that matters
    // for this path happens on the joiner's side, before the stream is opened.
    //
    // Counted into atomics rather than taken from a return value, because none
    // of the three ways this ends returns one: a revoke, the connection
    // closing, or the copy finishing.
    let from_peer = std::sync::atomic::AtomicU64::new(0);
    let to_peer = std::sync::atomic::AtomicU64::new(0);
    tokio::select! {
        _ = crate::pump::duplex(recv, send, up_r, up_w, &from_peer, &to_peer) => {}
        // This stream's own grant, not the share's. A stream knows what
        // admitted it; the connection it arrived on may be carrying more than
        // one, and enforcing a revoke of the wrong one is worse than not
        // enforcing it at all.
        _ = revoked(bridge.clone(), grant.id.clone()) => {}
        // The connection going away has to end this too. The pump's box-side
        // read has nothing to interrupt it: `duplex` shuts the write half when
        // the peer side ends, and a dev server that holds its socket open after
        // seeing that would leave this parked forever — losing the counts for
        // exactly the long-lived stream they exist for.
        _ = conn.closed() => {}
        // And the share winding up, so a shutdown does not wait out an idle
        // timeout on a stream that is going to end anyway.
        _ = bridge.shutting_down() => {}
    }
    use std::sync::atomic::Ordering;
    bridge.peer_bytes(
        id,
        to_peer.load(Ordering::Relaxed),
        from_peer.load(Ordering::Relaxed),
    );
    Ok(())
}

/// Resolves when this stream's own grant stops admitting anyone.
async fn revoked(bridge: Arc<Bridge>, grant_id: String) {
    loop {
        tokio::time::sleep(REVOKE_POLL).await;
        if !bridge.grant_is_live(&grant_id) {
            return;
        }
    }
}

/// Endpoint ids are 52 characters of base32 and nobody reads them whole. The
/// receipt and the terminal show enough to tell two peers apart.
pub fn short(id: &str) -> String {
    if id.len() <= 12 {
        return id.to_string();
    }
    format!("{}…", &id[..12])
}

// ─── the joining side ───────────────────────────────────────────────────────

/// Dial the sharer named by a ticket.
pub async fn dial(
    endpoint: &Endpoint,
    addr: &serde_json::Value,
) -> Result<Connection, H5iError> {
    let addr = parse_addr(addr)?;
    endpoint
        .connect(addr, wire::ALPN)
        .await
        .map_err(|e| {
            H5iError::Metadata(format!(
                "could not reach the sharer: {e}. They may have stopped sharing, or the ticket \
                 may have been minted by a machine that has since moved networks."
            ))
        })
}

/// Why a joiner could not get a stream.
///
/// Typed rather than a formatted string, because the joiner's proxy has to turn
/// this into an HTTP status for a browser, and deciding that by matching on
/// prose is how a "busy, try again" ends up rendering as "your invite is bad".
#[derive(Debug)]
pub enum OpenError {
    /// The share is at its connection ceiling. The ticket is fine.
    Busy,
    /// The ticket was not accepted: unknown, expired or revoked.
    Refused,
    /// The ticket was fine; the box had nothing listening on the shared port.
    Unreachable,
    /// Something below the handshake went wrong.
    Transport(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Busy => write!(
                f,
                "the share is already carrying as many connections as it will. Nothing is wrong \
                 with your ticket — wait a moment and reload."
            ),
            OpenError::Refused => write!(
                f,
                "the sharer refused this ticket. It may have expired, or been revoked — ask for \
                 a new one."
            ),
            OpenError::Unreachable => write!(
                f,
                "the share is up, but nothing is listening on the port inside the box. Your \
                 ticket is fine — whoever shared it needs to start their dev server."
            ),
            OpenError::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl From<OpenError> for H5iError {
    fn from(e: OpenError) -> H5iError {
        H5iError::Metadata(e.to_string())
    }
}

/// Open one authorized stream: the joiner's half of the handshake.
///
/// Returns the two halves of a stream that is already through the gate, so the
/// caller has nothing left to do but move bytes.
pub async fn open_stream(
    conn: &Connection,
    secret: &str,
) -> Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream), OpenError> {
    let hello = wire::encode_hello(secret)
        .ok_or_else(|| OpenError::Transport("this ticket's secret is malformed".into()))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| OpenError::Transport(format!("the sharer closed the connection: {e}")))?;
    send.write_all(&hello)
        .await
        .map_err(|e| OpenError::Transport(format!("could not greet the sharer: {e}")))?;
    let mut reply = [0u8; 1];
    recv.read_exact(&mut reply)
        .await
        .map_err(|e| OpenError::Transport(format!("the sharer did not answer the handshake: {e}")))?;
    match reply[0] {
        wire::REPLY_OK => Ok((send, recv)),
        wire::REPLY_BUSY => Err(OpenError::Busy),
        wire::REPLY_UNREACHABLE => Err(OpenError::Unreachable),
        _ => Err(OpenError::Refused),
    }
}

/// Report how a joined connection is actually carried, for the joiner's own
/// terminal. The sharer records the same observation in the receipt; this is so
/// the person clicking around knows whether a relay is in the path.
pub fn path_of(conn: &Connection) -> Option<Path> {
    observed_path(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::session::{self, ShareSession, Transport};

    /// Two endpoints in one process, with no relay and no discovery service.
    ///
    /// `presets::Minimal` is the point: the suite must not depend on n0's relay
    /// infrastructure being reachable, and it must not quietly start passing
    /// because a relay carried what a direct path could not. Everything here
    /// goes over this machine's own addresses.
    async fn local_endpoint(accepting: bool) -> Endpoint {
        let b = Endpoint::builder(presets::Minimal);
        let b = if accepting {
            b.alpns(vec![wire::ALPN.to_vec()])
        } else {
            b
        };
        b.bind().await.expect("bind a local endpoint")
    }

    /// Wait until an endpoint has an address a peer could dial.
    async fn dialable(ep: &Endpoint) -> EndpointAddr {
        for _ in 0..200 {
            let addr = ep.addr();
            if !addr.addrs.is_empty() {
                return addr;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the endpoint never learned an address it could be reached at");
    }

    /// A stand-in for the dev server inside a box: answers one line per
    /// connection and closes.
    ///
    /// Deliberately never joined. It blocks in `accept`, so waiting for it to
    /// finish would mean waiting for a connection the test is not going to make
    /// — which is exactly the hang this comment exists to stop somebody
    /// reintroducing. The thread goes when the test process does.
    fn fake_dev_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 64];
                let n = c.read(&mut buf).unwrap_or(0);
                let _ = c.write_all(format!("saw {n} bytes", n = n).as_bytes());
            }
        });
        port
    }

    /// A bridge over a temp box directory, with one grant. Returns the secret.
    fn test_bridge(dir: &std::path::Path, port: u16) -> (std::sync::Arc<Bridge>, String) {
        let mut sess = ShareSession::new(
            "env/test/demo",
            port,
            Transport::P2p,
            "local",
            chrono::Utc::now(),
        );
        let (grant, secret) = session::mint_grant(Some("peer".into()), 4_000_000_000).unwrap();
        sess.grants.push(grant);
        session::write(dir, &sess).expect("write session");
        let dialer = crate::dialer::Dialer::spawn_local(port).expect("dialer");
        (
            std::sync::Arc::new(Bridge::new(
                dir.to_path_buf(),
                "env/test/demo".into(),
                "digest".into(),
                "demo".into(),
                Transport::P2p,
                "local".into(),
                dialer,
            )),
            secret,
        )
    }

    /// Everything between a ticket and the dev server, exercised in one go:
    /// QUIC, the greeting, the grant table, the dialer, and the byte pump.
    #[tokio::test]
    async fn a_ticket_reaches_the_dev_server_and_a_bad_one_does_not() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret) = test_bridge(dir.path(), port);

        let sharer = local_endpoint(true).await;
        let addr = dialable(&sharer).await;
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, sharer, false).await }
        });

        let joiner = local_endpoint(false).await;
        let conn = joiner
            .connect(addr, wire::ALPN)
            .await
            .expect("connect to the sharer");

        // The good ticket goes all the way through to the dev server.
        let (mut send, mut recv) = open_stream(&conn, &secret).await.expect("authorized");
        send.write_all(b"hello").await.expect("write");
        let mut got = [0u8; 9];
        recv.read_exact(&mut got).await.expect("read the reply");
        assert_eq!(&got, b"saw 5 byt");

        // A ticket the grant table does not know gets nothing, on the same
        // connection — authorization is per stream, not per peer.
        let wrong = "ff".repeat(crate::ticket::SECRET_BYTES);
        let err = open_stream(&conn, &wrong).await.expect_err("refused");
        assert!(format!("{err}").contains("refused"), "{err}");

        // And the share recorded the peer that did get in.
        assert_eq!(bridge.peer_count(), 1);

        serving.abort();
        drop(conn);
    }

    /// Revocation reaches the *next* connection without restarting anything,
    /// because the grant table is read from disk every time.
    #[tokio::test]
    async fn a_revoke_from_another_process_stops_the_next_connection() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret) = test_bridge(dir.path(), port);

        let sharer = local_endpoint(true).await;
        let addr = dialable(&sharer).await;
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, sharer, false).await }
        });

        let joiner = local_endpoint(false).await;
        let conn = joiner.connect(addr, wire::ALPN).await.expect("connect");
        open_stream(&conn, &secret).await.expect("admitted once");

        // What `h5i box share stop` does, from a different process entirely.
        crate::run::stop(dir.path()).expect("stop");

        let err = open_stream(&conn, &secret)
            .await
            .expect_err("a revoked ticket must not be admitted again");
        assert!(format!("{err}").contains("revoked"), "{err}");

        serving.abort();
        drop(conn);
    }

    #[test]
    fn an_endpoint_id_is_shortened_for_people_to_read() {
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("0123456789abcdefghij"), "0123456789ab…");
    }

    #[test]
    fn addressing_that_came_from_a_stranger_is_refused_rather_than_trusted() {
        // A ticket is pasted from wherever, so its addressing is attacker
        // input. It must fail to parse rather than half-parse into something
        // the dialer will act on.
        for junk in [
            serde_json::json!(null),
            serde_json::json!("not an address"),
            serde_json::json!({"id": "nonsense", "addrs": []}),
            serde_json::json!({"unexpected": true}),
        ] {
            assert!(parse_addr(&junk).is_err(), "accepted {junk}");
        }
    }
}
