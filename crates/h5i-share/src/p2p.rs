//! Transport one: peer to peer, over iroh.
//!
//! Both sides run h5i, and the bytes go between them. QUIC, end-to-end
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
//!    is authorized on its own, so revoking a grant stops the *next*
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
use crate::session::Denied;
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
/// How many addresses one ticket may name. A real one has a relay and a few
/// interfaces; two hundred is a way of making somebody's machine spray packets.
const MAX_TICKET_ADDRS: usize = 12;

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

/// How many streams one peer may have open on one connection at once.
///
/// A browser opens several connections to an origin and a handful of streams
/// on each. The page, its assets, an event source, a hot-reload socket. Twelve
/// is comfortably above that and far below quinn's default of a hundred, which
/// is the number that made the arithmetic below come out in the gigabytes.
const MAX_PEER_STREAMS: u32 = 12;

/// Bind an endpoint that can accept shares.
///
/// The transport limits are set here for the same reason [`bind_joiner`] sets
/// them, and this side had the harder version of the problem: it is the side
/// anyone can dial.
///
/// quinn's defaults are 100 bidirectional and 100 unidirectional streams per
/// connection with about 1.25 MiB of receive window each, and data on a stream
/// nobody accepts stays buffered. This protocol never accepts a
/// unidirectional stream at all, nothing calls `accept_uni`, so an
/// unauthenticated peer could fill a hundred of them and have the memory held
/// by a connection whose application code would never read a byte of it. The
/// 256-connection ceiling turned that into tens of gigabytes, and the same
/// hundred bidirectional streams into 25,600 tasks parked in the greeting
/// read, each holding its slot for the full [`HELLO_TIMEOUT`].
///
/// So: no remote unidirectional streams, and a bidirectional limit sized for a
/// browser rather than for a protocol nobody wrote down.
pub async fn bind_sharer() -> Result<Endpoint, H5iError> {
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .max_concurrent_uni_streams(0u32.into())
        .max_concurrent_bidi_streams(MAX_PEER_STREAMS.into())
        .build();
    Endpoint::builder(presets::N0)
        .alpns(vec![wire::ALPN.to_vec()])
        .transport_config(transport)
        .bind()
        .await
        .map_err(|e| H5iError::Metadata(format!("could not start the peer-to-peer endpoint: {e}")))
}

/// Bind an endpoint that only dials.
///
/// No address lookup is configured, and that is the security property.
/// The obvious build (`PkarrResolver` plus `DnsAddressLookup`, iroh's usual
/// pair) was here, and it went around the ticket filter below entirely. Every
/// address in a ticket is checked before iroh is handed it; discovered
/// addresses are not, because they arrive later, inside the endpoint, and are
/// merged straight into the dial candidates. The endpoint id is chosen by
/// whoever wrote the ticket, so they hold the key that signs that endpoint's
/// pkarr record: publish `127.0.0.1:11434` under it, hand out a ticket whose
/// own address list is impeccable, and the joiner's machine dials its own
/// loopback anyway. iroh triggers the lookup whenever there is no selected
/// path, which on a fresh `connect` is always.
///
/// A ticket is self-contained by construction, [`addressing`] refuses to mint
/// one that names nowhere, so there is nothing for discovery to add that the
/// ticket did not already say. What is given up is the case where the sharer's
/// addressing changed between minting and joining; the joiner gets a dial
/// failure and asks for a fresh ticket, which is a worse minute and not a
/// worse outcome.
pub async fn bind_joiner() -> Result<Endpoint, H5iError> {
    // No ALPN is configured and `accept()` is never called, so a third party
    // cannot open a connection here. What it *could* do is open streams on the
    // connection the joiner established: quinn's defaults allow 100 bidi and
    // 100 uni, each with about 1.25 MB of receive window, and data on a stream
    // nobody accepts stays buffered. Roughly 250 MB of the joiner's memory,
    // from a sharer doing nothing but opening streams. iroh's own
    // documentation for this setter says protocols that forbid
    // remotely-initiated streams should set both to zero, and this one does:
    // every stream in this protocol is opened by the joiner.
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(0u32.into())
        .max_concurrent_uni_streams(0u32.into())
        .build();
    // `presets::N0` minus the publisher, assembled by hand. That preset
    // installs a `PkarrPublisher`, which puts *this* endpoint's direct
    // addresses (every local interface, so the LAN and any VPN) into a
    // public directory keyed by its public key, and leaves them there for the
    // record's lifetime. The sharer learns that key from the QUIC handshake,
    // so it can look the joiner up afterwards.
    //
    // A joiner has no use for it: it configures no ALPN, never calls
    // `accept()`, and nobody needs to find it by id. The *resolvers* are gone
    // for the reason in this function's doc comment. The relay stays, to reach
    // a sharer behind a NAT; address exchange for hole punching happens in-band
    // on the connection itself.
    Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::endpoint::default_relay_mode())
        .transport_config(transport)
        .bind()
        .await
        .map_err(|e| H5iError::Metadata(format!("could not start the peer-to-peer endpoint: {e}")))
}

/// How long to wait for a home relay before minting a ticket anyway.
///
/// `Endpoint::online()` resolves when a relay connection is up, and iroh
/// documents it as waiting *indefinitely* when there is no WAN. Awaited
/// unconditionally, `h5i box share` hung before printing anything on an
/// offline or relay-blocked network, including the LAN-only case, where the
/// direct addresses it was waiting to supplement were already there, and
/// including `--direct-only`, which had come to depend on reaching the very
/// relay it promises not to put application traffic on.
const RELAY_WAIT: Duration = Duration::from_secs(10);

/// Wait until this endpoint knows how it can be reached, then describe it.
///
/// Done before the ticket is printed rather than after: a ticket minted before
/// the endpoint has any addresses is a ticket that names nowhere, and the
/// person holding it has no way to tell that from a network problem on their
/// end.
///
/// Three things happen here, and only the first was happening before.
///
/// 1. Wait for a relay, but not forever. See [`RELAY_WAIT`]. Past the
///    deadline this carries on with whatever direct addresses the endpoint
///    has, which on a LAN is the whole answer.
/// 2. Refuse to mint a ticket that names nowhere. With the wait bounded,
///    "no relay *and* no direct addresses" is now reachable, and it is a
///    failure with a sentence rather than a ticket that cannot work. It is
///    also what lets the joiner drop address discovery: see [`bind_joiner`].
/// 3. Apply the join side's own policy to what is about to be printed.
///    `ep.addr()` is the current relay plus *every* direct address, and a host
///    with enough Docker bridges, VPN tunnels and dual-stack interfaces
///    produced more than [`MAX_TICKET_ADDRS`] of them, so the sharer printed
///    a confident invite that this same version of `h5i join` refused as
///    attacker-shaped. The candidates are trimmed here, relay first, and then
///    the emitted value goes through the exact function the joiner will run.
pub async fn addressing(ep: &Endpoint) -> Result<(String, serde_json::Value), H5iError> {
    let _ = tokio::time::timeout(RELAY_WAIT, ep.online()).await;
    let addr = trim_addressing(ep.addr());
    let id = addr.id.to_string();
    if addr.addrs.is_empty() {
        return Err(H5iError::Metadata(
            "this machine has no address a peer could reach it on: no relay answered within \
             10s and no usable direct address was found. A ticket minted now would name \
             nowhere. Check the network — or use `--tunnel`, which needs only an outbound \
             connection."
                .into(),
        ));
    }
    let value = serde_json::to_value(&addr)?;
    // The same check the joiner runs, against the bytes about to be printed.
    // Trimming and validating are separate on purpose: if a future iroh grows
    // an address shape the trim does not know how to prefer, this fails loudly
    // here rather than producing an invite that only fails on somebody else's
    // machine.
    refuse_addresses_that_point_inward(&value).map_err(|e| {
        H5iError::Metadata(format!(
            "this machine's own addressing is not something a ticket may carry: {e}"
        ))
    })?;
    Ok((id, value))
}

/// Cut an endpoint's addressing down to something a ticket may carry.
///
/// Relay first, it is the one address that works from anywhere, then direct
/// candidates, dropping the ones the join side would refuse anyway (loopback,
/// unspecified, link-local) before counting, so a machine with many interfaces
/// spends its budget on addresses that could actually carry a connection.
fn trim_addressing(addr: EndpointAddr) -> EndpointAddr {
    use iroh::TransportAddr;
    let (relays, rest): (Vec<_>, Vec<_>) =
        addr.addrs.into_iter().partition(TransportAddr::is_relay);
    let usable = rest.into_iter().filter(|a| match a {
        TransportAddr::Ip(s) => !points_inward(&s.ip()),
        _ => true,
    });
    EndpointAddr::from_parts(
        addr.id,
        relays.into_iter().chain(usable).take(MAX_TICKET_ADDRS),
    )
}

fn parse_addr(value: &serde_json::Value) -> Result<EndpointAddr, H5iError> {
    // Checked on the JSON, before serde builds an `EndpointAddr` out of it.
    refuse_addresses_that_point_inward(value)?;
    let addr: EndpointAddr = serde_json::from_value(value.clone()).map_err(|e| {
        H5iError::Metadata(format!(
            "this ticket's addressing is not something this h5i understands ({e}) — the two \
             sides are probably different versions"
        ))
    })?;
    Ok(addr)
}

/// Refuse a ticket that would make the joiner dial its own machine, and cap how
/// many places one ticket may point at.
///
/// The addressing in a ticket is chosen by whoever wrote the ticket, and a
/// ticket is something people paste out of a chat window. Two things have to be
/// true of it before iroh is handed it.
///
/// Nowhere on the joiner's own machine. The first version of this check
/// formatted each address with `{:?}` and looked for an IP literal in the text,
/// which caught `127.0.0.1` and missed every other spelling of the same place:
/// `0.0.0.0` and `[::ffff:127.0.0.1]` both `connect()` to loopback on Linux,
/// and neither is `is_loopback()`. Demonstrated: a ticket naming
/// `http://0.0.0.0:39271/` made `h5i join` open a plaintext connection to a
/// listener on `127.0.0.1:39271` and send it a request. The unauthenticated
/// admin surfaces on a developer's loopback (a Docker socket, an Ollama, an
/// Elasticsearch, their own `h5i ui`) are unauthenticated *because* they are
/// loopback-only. It decides on parsed addresses now, and refuses loopback,
/// unspecified and link-local, after unwrapping IPv4-mapped IPv6.
///
/// Not very many places. The 8 KiB ticket cap leaves room for about two
/// hundred addresses, and iroh probes every one: measured, one pasted ticket
/// produced 2,940 packets and 3.5 MB of UDP to 196 destinations of the ticket
/// author's choosing, sent from inside the joiner's network and attributed to
/// them. A real ticket names a handful.
///
/// What is deliberately *not* refused is a private LAN address. Two machines
/// on one office network is the case direct P2P exists for.
fn refuse_addresses_that_point_inward(value: &serde_json::Value) -> Result<(), H5iError> {
    let addrs = value.get("addrs").and_then(|a| a.as_array());
    let listed = addrs.map(|a| a.len()).unwrap_or(0);
    if listed > MAX_TICKET_ADDRS {
        return Err(H5iError::Metadata(format!(
            "this ticket names {listed} addresses. A real one names a handful; this many is a \
             way of making your machine spray packets at somebody else's choosing. Ask for a \
             ticket minted by a current h5i."
        )));
    }
    for entry in addrs.into_iter().flatten() {
        // Decided on the parsed address, not on a debug rendering of it.
        let bad = match entry {
            serde_json::Value::Object(o) => {
                if let Some(ip) = o.get("Ip").and_then(|v| v.as_str()) {
                    ip.parse::<std::net::SocketAddr>()
                        .ok()
                        .map(|s| points_inward(&s.ip()))
                        .unwrap_or(false)
                } else if let Some(url) = o.get("Relay").and_then(|v| v.as_str()) {
                    !relay_is_allowed(url)
                } else {
                    false
                }
            }
            _ => false,
        };
        if bad {
            return Err(H5iError::Metadata(format!(
                "this ticket names {entry}, which is not somewhere h5i will dial: either an \
                 address on your own machine rather than on the sharer's, or a relay outside \
                 the set h5i uses. Ask for a ticket minted on the machine that is actually \
                 sharing, by a current h5i."
            )));
        }
    }
    Ok(())
}

/// The DNS suffix h5i's relays live under.
///
/// Both sides of a share bind with iroh's `N0` relay defaults, so every relay
/// a legitimate h5i ticket can name is one of these. Nothing self-hosted is
/// supported today, and a ticket is not the place to introduce it: the URL in
/// one is chosen by whoever wrote the ticket.
const RELAY_SUFFIX: &str = ".relay.n0.iroh.link";

/// May the joiner's relay client dial this URL?
///
/// An allowlist, and it took two goes to get here.
///
/// First the parsing. The authority was being taken apart by hand (split
/// on `://`, split on `/`, `rsplit` on `:`) and that is not how a URL parser
/// reads one. `http://attacker@localhost:11434/` came out of the hand-rolled
/// version as the host `attacker@localhost`, which is not `localhost`, so it
/// passed. `RelayUrl` is a `url::Url` underneath and the relay client asks
/// *it* for the host, which is `localhost`, and dials loopback. Same trick with
/// an IP literal. So the check reads the host through the same parser that
/// will do the dialling, and refuses userinfo outright, no legitimate relay
/// URL has any, and its only use here is to make the two readings disagree.
///
/// Then the resolution. Refusing loopback literals and `.localhost` still
/// accepted every other name, and a name is resolved *later*, by the relay
/// client, which then dials whatever came back. `evil.example.com A 127.0.0.1`
/// restores the whole problem, and re-resolving it here would only add a
/// rebinding window between the check and the dial. A hostname allowlist has
/// neither hole: `use1-1.relay.n0.iroh.link.` is what an honest ticket names,
/// and if that name ever resolves to loopback the joiner has bigger problems
/// than this share.
fn relay_is_allowed(url: &str) -> bool {
    let Ok(parsed) = url.parse::<iroh::RelayUrl>() else {
        return false;
    };
    // Userinfo is the whole trick in the first paragraph above, and no relay
    // URL has a use for it.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    // An IP literal is never one of h5i's relays, so this is `false` by the
    // suffix test alone, but say it, because "no literal reaches the relay
    // client" is the property, not a side effect of a string comparison.
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    host.ends_with(RELAY_SUFFIX)
}

/// Is this an address on the machine doing the dialling?
fn points_inward(ip: &std::net::IpAddr) -> bool {
    // `::ffff:127.0.0.1` is loopback wearing an IPv6 hat, and `connect()`
    // treats it as such. Unwrap before asking.
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(*ip),
        v4 => *v4,
    };
    match ip {
        // `0.0.0.0` and `::` both land on loopback when connected to.
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
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
            return Some(if p.is_relay() {
                Path::Relayed
            } else {
                Path::Direct
            });
        }
    }
    None
}

/// Is a *relay* carrying this connection right now?
///
/// The one question `--direct-only` asks, in the one place both of its
/// enforcement points can read it, and they did not agree. The watchdog polls
/// `observed_path` and acts only on `Some(Path::Relayed)`, deliberately: a
/// connection has no selected path for an instant after a NAT rebinding or a
/// local address change, and treating that as a relay closed healthy
/// connections and put a false relay claim in the receipt. The per-write gate
/// added later to close the one-second window between polls asked the opposite
/// question, `== Some(Path::Direct)`, so the same instant barred the write,
/// ended that direction of the pump, and took the connection with it. Silently:
/// nothing on that path records a turned-away connection, so a `--direct-only`
/// share dropped streams during ordinary rebinding and its receipt said nothing
/// at all.
///
/// "No path selected" is not a relay carrying traffic; it is nothing carrying
/// traffic, which is what makes barring on it both wrong and unnecessary.
fn a_relay_is_carrying_it(conn: &Connection) -> bool {
    relay_is_carrying(observed_path(conn))
}

/// The rule itself, over the three answers [`observed_path`] can give.
///
/// Split from the `Connection` so it can be stated in a test: a live iroh
/// connection cannot be built in a unit test, and what was wrong here was never
/// the transport but which of those three answers maps to which decision.
fn relay_is_carrying(path: Option<Path>) -> bool {
    path == Some(Path::Relayed)
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
/// produce a direct path, the connection is closed before any application
/// byte crosses it, and the peer is told why. A flag that merely preferred a
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
            // to a peer that has not identified itself, but it is recorded,
            // because a share taken down at its own front door should not read
            // as a share nobody used.
            bridge.record_front_refusal();
            continue;
        };
        // The same barrier the tunnel front takes, for the same reason: a
        // connection accepted and not yet authorized is work teardown has to
        // wait for. See `Bridge::enter_front`.
        let Some(front) = bridge.enter_front() else {
            continue;
        };
        let bridge = bridge.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _front = front;
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
        bridge.record_turned_away(crate::bridge::TurnedAwayReason::NoDirectPath);
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
    // The grant that admitted this connection, set by whichever stream
    // authorized first, including a bare ticket check, which opens no stream
    // for revocation to act on and would otherwise sit outside it entirely.
    // `serve_stream` refuses a second grant on the same connection, so this is
    // *the* grant rather than the most recent one.
    let grant_id: Arc<std::sync::Mutex<Option<String>>> = Default::default();

    // What this connection's watchdog keeps doing for its whole life.
    //
    // *Revocation*, for the connection's own grant. Per-stream enforcement
    // covers a stream that is carrying traffic; this covers the joiner sitting
    // on a checked ticket with no page open yet, and it closes the whole
    // connection because one connection is one grant.
    //
    // *`--direct-only`*, because a direct path can die and iroh will fall
    // back to a relay, so a promise checked only at setup is a preference. The
    // poll is the coarse half of that enforcement: the fine half is the gate in
    // `serve_stream`, consulted before every write.
    //
    // The receipt's record of the path, because a long-lived stream sampled
    // once at its start would be recorded as direct for a session that spent
    // most of itself on a relay.
    let watchdog = AbortOnDrop({
        let bridge = bridge.clone();
        let conn = conn.clone();
        let peer_id = peer_id.clone();
        let grant_id = grant_id.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + UNAUTHENTICATED_GRACE;
            // The share winding up is a close with a reason, said here rather
            // than left to `Endpoint::close`, which has none. A joiner whose
            // ticket simply ran out was told "closed by peer: 0", where a
            // revoked one got a sentence. Same ending, and only one of them
            // explained itself.
            let winding_up = {
                let bridge = bridge.clone();
                let conn = conn.clone();
                tokio::spawn(async move {
                    bridge.shutting_down().await;
                    conn.close(5u32.into(), b"h5i: this share has ended");
                })
            };
            let _guard = AbortOnDrop(winding_up);
            loop {
                tokio::time::sleep(REVOKE_POLL).await;
                let seen = *peer_id.lock().unwrap_or_else(|p| p.into_inner());

                // A connection that completes the QUIC handshake and then never
                // opens a stream costs a task and a poll per second for as long
                // as it likes, and iroh's own keep-alive means it never idles
                // out. Nothing has been authorized, so nothing is lost by
                // hanging up on it.
                if seen.is_none() && tokio::time::Instant::now() >= deadline {
                    bridge.record_turned_away(crate::bridge::TurnedAwayReason::NeverGreeted);
                    conn.close(4u32.into(), b"h5i: no ticket was presented");
                    return;
                }

                // The grant this whole connection was admitted by. For a
                // joiner that has only checked its ticket this is the only
                // place a revoke can reach it, it has no stream open, and
                // without it `h5i join` kept its "joined" banner up for a
                // share it had been cut off from.
                let probed = grant_id.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(id) = probed
                    && !bridge.grant_is_live(&id)
                {
                    // Which of the two endings this is. `share stop`
                    // revokes every grant and marks the share winding up in
                    // the same write, so this branch fires first every time,
                    // and said "your ticket was revoked or has expired"
                    // for a share somebody had simply stopped. Half of that
                    // sentence is a false claim about time.
                    if bridge.share_is_ending() {
                        conn.close(5u32.into(), b"h5i: this share has ended");
                    } else {
                        conn.close(6u32.into(), b"h5i: this ticket was revoked or has expired");
                    }
                    return;
                }

                if let (Some(id), Some(p)) = (seen, observed_path(&conn)) {
                    bridge.peer_path(id, p);
                }
                // Only a *selected relay path* is evidence of relaying.
                // "Nothing selected" happens for an instant after a NAT
                // rebinding, and treating it as a relay closed healthy
                // connections and libelled honest ones in the receipt. One
                // predicate, shared with the per-write gate in `serve_stream`,
                // because the two asked opposite questions about exactly that
                // instant. See [`a_relay_is_carrying_it`].
                if direct_only && a_relay_is_carrying_it(&conn) {
                    bridge.record_turned_away(crate::bridge::TurnedAwayReason::NoDirectPath);
                    conn.close(
                        3u32.into(),
                        b"h5i: --direct-only, and the direct path was lost",
                    );
                    return;
                }
            }
        })
    });
    let mut streams = tokio::task::JoinSet::new();
    loop {
        // Reap what has finished. A `JoinSet` only releases a task's allocation
        // and its result when it is joined, so a peer opening and closing
        // streams in a loop would grow this without bound. Reaping here is
        // accept-driven, so a connection that opens streams and then goes quiet
        // holds its finished slots until it ends. Bounded by QUIC's own
        // concurrent-stream limit, which is the case this does not need to
        // cover.
        while streams.try_join_next().is_some() {}
        let Ok((send, recv)) = conn.accept_bi().await else {
            break;
        };
        // Spawned, not awaited. A browser opens several connections to one
        // origin and holds them open. The page, its assets, an event source,
        // a hot-reload socket. Serving streams one after another would make
        // every share single-file behind whichever connection is longest-lived,
        // which for a dev server is the one that never ends.
        let bridge = bridge.clone();
        let who = who.clone();
        let peer_id = peer_id.clone();
        let grant_id = grant_id.clone();
        let path = observed_path(&conn);
        let conn_for_stream = conn.clone();
        streams.spawn(async move {
            let on = OnThisConnection {
                who: &who,
                path,
                peer_id: &peer_id,
                grant_id: &grant_id,
                conn: &conn_for_stream,
                direct_only,
            };
            if let Err(e) = serve_stream(&bridge, send, recv, &on).await {
                eprintln!("share: {e}");
            }
        });
    }

    // Explicit, and also guarded: `serve_connection` is spawned detached today,
    // so it always reaches here, but a future caller that drops the future
    // instead would otherwise leave the watchdog, its `Connection` clone and
    // its own child task polling forever. The leak-proof idiom was already one
    // level down; it belongs here too.
    watchdog.0.abort();
    // Recorded before the drain, not after. The peer has gone the moment
    // `accept_bi` stops answering; noting it afterwards meant the shutdown path,
    // the only path where it matters, almost always finished first and every
    // peer's closing time came out as "still connected at the end".
    let id = *peer_id.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(id) = id {
        bridge.peer_left(id);
    }
    // Drained rather than aborted. Each stream records its byte counts when its
    // pump returns, and aborting drops that future mid-copy, so a share cut
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

/// What every stream on one connection shares.
///
/// A struct rather than six more parameters: these are all facts about the
/// *connection*, and threading them individually is how the two mutexes ended
/// up being passed to a function that only reads one of them.
struct OnThisConnection<'a> {
    who: &'a str,
    path: Option<Path>,
    /// Set by the first stream that authorizes; every later one is counted
    /// against the same record.
    ///
    /// Both of these mutexes are taken with a poison recovery rather than an
    /// `expect`, which is the discipline `Bridge::tally` states and the reason
    /// matters here: `peer_joined` is called while the first is held, and the
    /// *watchdog* reads both once a second. An `expect` therefore turned one
    /// panic under either lock into a connection whose watchdog was dead,
    /// which is the task that closes it on a revoke. The share would keep
    /// carrying that peer's open streams with nothing left to cut them off.
    /// Neither value has an invariant across fields for a recovery to break:
    /// worst case the grant is unclaimed again, and a stream presenting a
    /// second grant is refused by `Bridge::authorize` regardless.
    peer_id: &'a std::sync::Mutex<Option<crate::bridge::PeerId>>,
    /// The grant this connection belongs to, set by whichever stream
    /// authorized first. See [`serve_stream`] for why a second grant on the
    /// same connection is refused rather than accounted for.
    grant_id: &'a std::sync::Mutex<Option<String>>,
    conn: &'a Connection,
    direct_only: bool,
}

async fn serve_stream(
    bridge: &Arc<Bridge>,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    on: &OnThisConnection<'_>,
) -> Result<(), H5iError> {
    let OnThisConnection {
        who,
        path,
        peer_id,
        grant_id,
        conn,
        direct_only,
    } = *on;
    let mut hello = [0u8; wire::HELLO_LEN];
    // Deadlined, because this is the one read an unauthenticated peer gets to
    // make us wait on. Sixty-eight of sixty-nine bytes and then silence would
    // otherwise hold a task for as long as the connection lasted. The HTTP
    // fronts have had this since they were written; this one did not.
    let greeted = tokio::time::timeout(HELLO_TIMEOUT, recv.read_exact(&mut hello)).await;
    if !matches!(greeted, Ok(Ok(()))) {
        return Ok(());
    }
    let Some((intent, secret)) = wire::decode_hello(&hello) else {
        let _ = send.write_all(&[wire::REPLY_DENIED]).await;
        return Ok(());
    };
    let grant = match bridge.authorize(&secret) {
        Ok(g) => g,
        Err(denied) => {
            // Three of the five refusals are about the visitor's ticket and two
            // are about this machine. Answering all of them with one byte sent
            // somebody whose only problem was that the share had stopped away
            // to ask for a replacement invite.
            let code = match denied {
                Denied::ShareOver => wire::REPLY_SHARE_OVER,
                Denied::TableUnreadable => wire::REPLY_SHARER_FAULT,
                _ => wire::REPLY_DENIED,
            };
            let _ = send.write_all(&[code]).await;
            let _ = send.finish();
            eprintln!("share: refused {} — {}", short(who), denied.explain());
            return Ok(());
        }
    };

    // One grant per connection. A stream carries its own ticket, so two
    // streams on one QUIC connection could in principle present two different
    // grants, and everything downstream assumed they could not. The peer
    // record is created once per connection by whichever stream authorized
    // first, so a second grant's connections, bytes, path and duration were
    // all added to the *first* grant's row in the receipt; and the watchdog
    // that closes a connection when its probe grant is revoked would have cut
    // off streams belonging to a grant that was still perfectly live.
    //
    // Keeping the accounting per grant is the other way to fix it and buys
    // nothing: `h5i join` holds one ticket and opens one connection, so a
    // second grant here is not a case anybody has, and refusing it is a
    // sentence rather than a second bookkeeping path nothing exercises.
    let mismatch = {
        let mut owner = grant_id.lock().unwrap_or_else(|p| p.into_inner());
        match &*owner {
            Some(first) if *first != grant.id => Some(first.clone()),
            Some(_) => None,
            None => {
                *owner = Some(grant.id.clone());
                None
            }
        }
    };
    if let Some(first) = mismatch {
        let _ = send.write_all(&[wire::REPLY_DENIED]).await;
        let _ = send.finish();
        eprintln!(
            "share: refused {} — this connection was admitted by grant {first}, and a stream on \
             it presented grant {}. One connection carries one ticket.",
            short(who),
            grant.id
        );
        return Ok(());
    }

    // Registered before anything is dialled, so an authorized peer always
    // appears in the receipt, even if the box turns out to have nothing
    // listening. Doing it after the dial left a share whose dev server was
    // down reading as one nobody ever tried to use, which is the tunnel front's
    // behaviour inverted for no reason.
    let id = {
        let mut slot = peer_id.lock().unwrap_or_else(|p| p.into_inner());
        *slot.get_or_insert_with(|| bridge.peer_joined(short(who), &grant, path))
    };
    // Recorded only when something actually observed it. Feeding the join-time
    // value back through `peer_path` marked a guess as an observation, which is
    // the thing that flag exists to prevent.
    if let Some(p) = path {
        bridge.peer_path(id, p);
    }

    // A ticket check and nothing else. Answered from the grant table, without a
    // slot and without touching the box: `h5i join` does this once at startup,
    // and if it went the whole way it would open a connection to the dev server
    // per join, spend one of the share's 64 slots on it, and, for any dev
    // server that does not close when its client stops writing, leave a pump
    // parked on that slot until the joiner went away.
    //
    // The peer is still recorded. Somebody presented a valid ticket for this
    // share; that they then never loaded the page is a fact the receipt should
    // show rather than hide, and it shows as a peer with no connections.
    if intent == wire::Intent::Probe {
        // The connection's grant is already recorded above, which is what lets
        // a revoke reach a joiner who has connected and not yet opened the
        // page: without it a connection carrying no streams sat outside
        // revocation entirely, and `h5i join` kept its "joined" banner up after
        // the sharer had cut it off.
        let _ = send.write_all(&[wire::REPLY_OK]).await;
        let _ = send.finish();
        return Ok(());
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
        // Raced against this stream's own grant and the share ending, which is
        // not what the watchdog below can do for it: the `revoked` arm of that
        // `select!` is installed *after* this await returns. The dialer
        // serialises every request behind one mutex and allows each namespace
        // connect ten seconds, so a dev server with a full accept queue let
        // authorized requests pile up in exactly this gap, and `revoke`,
        // which promises open connections are dropped within a second,
        // returned to a terminal while sixty-four of them sat here holding
        // every permit, ready to forward into the box the moment the port
        // started accepting.
        //
        // The blocking dial itself cannot be cancelled, it is a syscall on a
        // pool thread, so this drops the *result* rather than interrupting
        // the work: the connection into the box is opened and immediately
        // closed, and nothing the revoked peer sent reaches it.
        let opened = tokio::select! {
            r = tokio::task::spawn_blocking(move || bridge2.open_upstream()) => {
                r.map_err(|e| H5iError::Metadata(format!("the box dialer panicked: {e}")))?
            }
            _ = revoked(bridge.clone(), grant.id.clone()) => {
                let _ = send.write_all(&[wire::REPLY_DENIED]).await;
                let _ = send.finish();
                return Ok(());
            }
            _ = bridge.shutting_down() => {
                let _ = send.write_all(&[wire::REPLY_SHARE_OVER]).await;
                let _ = send.finish();
                return Ok(());
            }
        };
        match opened {
            Ok(s) => s,
            Err(e) => {
                // A good ticket that found nothing listening. Told to the peer
                // as its own answer, "ask for a new ticket" would send them
                // chasing something that is not the problem, and recorded,
                // because otherwise a share where the dev server was down reads
                // as one nobody ever tried to use.
                let code = if e.to_string().contains("dialer") || e.to_string().contains("loopback")
                {
                    wire::REPLY_ROUTE_BROKEN
                } else {
                    wire::REPLY_UNREACHABLE
                };
                let _ = send.write_all(&[code]).await;
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
    // as it likes. That is what the ticket is. The HTTP framing that matters
    // for this path happens on the joiner's side, before the stream is opened.
    //
    // Counted into atomics rather than taken from a return value, because none
    // of the three ways this ends returns one: a revoke, the connection
    // closing, or the copy finishing.
    let from_peer = std::sync::atomic::AtomicU64::new(0);
    let to_peer = std::sync::atomic::AtomicU64::new(0);
    // `--direct-only`, enforced where the bytes are rather than only by the
    // watchdog that polls once a second. A direct path that falls back to a
    // relay just after a poll used to mean up to a second of application
    // traffic across a third party before the connection was closed, for a
    // flag that promises none crosses one at all. Asked immediately before
    // every write, the residue is what QUIC had already accepted at the
    // instant the path changed, which nothing above the transport can
    // recall, and not a second's worth of fresh reads.
    //
    // The question is "is a relay carrying this", not "is a direct path
    // carrying this", and they are not complements: between them sits the
    // instant with no selected path at all, which the watchdog has always
    // tolerated and this gate used to bar. Ending the pump, and the
    // connection, on an ordinary NAT rebinding. One predicate now answers for
    // both. See [`a_relay_is_carrying_it`].
    let barred: Option<Box<crate::pump::Gate>> = direct_only.then(|| {
        let conn = conn.clone();
        Box::new(move || !a_relay_is_carrying_it(&conn)) as Box<crate::pump::Gate>
    });
    tokio::select! {
        _ = crate::pump::duplex_gated(
            recv,
            send,
            up_r,
            up_w,
            &from_peer,
            &to_peer,
            barred.as_deref(),
        ) => {}
        // This stream's own grant, not the share's. A stream knows what
        // admitted it; the connection it arrived on may be carrying more than
        // one, and enforcing a revoke of the wrong one is worse than not
        // enforcing it at all.
        _ = revoked(bridge.clone(), grant.id.clone()) => {}
        // The connection going away has to end this too. The pump's box-side
        // read has nothing to interrupt it: `duplex` shuts the write half when
        // the peer side ends, and a dev server that holds its socket open after
        // seeing that would leave this parked forever. Losing the counts for
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

/// Aborts a spawned task when it goes out of scope.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Endpoint ids are 52 characters of base32 and nobody reads them whole. The
/// receipt and the terminal show enough to tell two peers apart.
///
/// Cut on a character boundary, not on byte 12. Every caller today passes an
/// `iroh` node id, which is base32 and therefore ASCII, so `&id[..12]` is
/// correct for all of them, and panics for the first one that is not. This is
/// a `pub` helper called from the connection-accept path, where a panic is the
/// share going down, and "no caller does that yet" is a property of the callers
/// rather than of this function. Making it true here costs one line.
pub fn short(id: &str) -> String {
    if id.len() <= 12 {
        return id.to_string();
    }
    // The last boundary at or before byte 12; `floor_char_boundary` is not
    // stable, so this is the same thing spelled out.
    let cut = (0..=12)
        .rev()
        .find(|&i| id.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}…", &id[..cut])
}

// ─── the joining side ───────────────────────────────────────────────────────

/// Dial the sharer named by a ticket.
pub async fn dial(endpoint: &Endpoint, addr: &serde_json::Value) -> Result<Connection, H5iError> {
    let addr = parse_addr(addr)?;
    endpoint
        .connect(addr, wire::ALPN)
        .await
        .map_err(|e| dial_failure(&h5i_core::redact::sanitize_display(&e.to_string())))
}

/// What a failed dial means, in words.
///
/// Its own function so it can be tested in microseconds. Testing it through
/// `connect` costs thirty seconds of real handshake timeout, which is the sort
/// of thing that gets a test deleted rather than kept.
fn dial_failure(said: &str) -> H5iError {
    // A protocol disagreement is not a network problem, and telling somebody
    // the sharer "may have stopped sharing" sends them to check two things
    // that are both fine. The ALPN was bumped precisely so version skew fails
    // at the transport rather than as a refused ticket; this is the half that
    // says which it was. QUIC reports it as "peer doesn't support any known
    // protocol", which is true and means nothing to anyone who has not read
    // the RFC.
    if said.contains("known protocol") || said.contains("no application protocol") {
        return H5iError::Metadata(format!(
            "this ticket was minted by a different version of h5i than the one you are \
             running, and the two do not speak the same protocol. Whoever shared it needs to \
             update, or you do. ({said})"
        ));
    }
    // Three causes, because this case cannot tell them apart and naming two of
    // them sends people to check the wrong thing. A sharer running an *older*
    // h5i does not reject the newer protocol, it simply never completes the
    // handshake, so that skew arrives here as a timeout, indistinguishable
    // from a peer that is not there. Measured: thirty seconds of waiting and
    // then a sentence about the network.
    H5iError::Metadata(format!(
        "could not reach the sharer: {said}. They may have stopped sharing; the ticket may \
         have been minted by a machine that has since moved networks; or the two of you may \
         be running versions of h5i that do not speak the same protocol."
    ))
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
    /// The ticket was fine and h5i could not reach the box at all.
    RouteBroken,
    /// The share is over. Not a judgement on the ticket. There was nothing
    /// left to judge it against.
    ShareOver,
    /// The sharer could not read its own grant table.
    SharerFault,
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
            OpenError::RouteBroken => write!(
                f,
                "the share is up and h5i cannot reach inside the box any more. Nothing is \
                 wrong with your ticket and nothing is wrong with their dev server — whoever \
                 shared it needs to restart the share."
            ),
            OpenError::ShareOver => write!(
                f,
                "that share has ended. Nothing is wrong with your ticket — whoever shared it \
                 stopped the share, and would have to start a new one."
            ),
            OpenError::SharerFault => write!(
                f,
                "the sharing machine could not read its own record of who is invited. Nothing \
                 is wrong with your ticket, and a new one would fail the same way — whoever \
                 shared it needs to look at their machine."
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
/// How long the joiner will wait on any single step of the handshake.
///
/// Every one of the three was unbounded, and a hostile sharer controls all
/// three: advertise no stream credit and `open_bi` parks, advertise a zero
/// window and the write parks, simply never answer and the read parks. iroh
/// keeps the connection alive from both sides, so nothing times out on its own.
/// The sharer's side of the same frame has had a deadline since it was written
/// (`HELLO_TIMEOUT`); the joiner's never got one, so `h5i join` printed
/// nothing at all and hung, before the listener was even bound.
const JOINER_HANDSHAKE: Duration = Duration::from_secs(15);

async fn deadlined<T, F>(what: &str, f: F) -> Result<T, OpenError>
where
    F: std::future::Future<Output = Result<T, OpenError>>,
{
    match tokio::time::timeout(JOINER_HANDSHAKE, f).await {
        Ok(r) => r,
        Err(_) => Err(OpenError::Transport(format!(
            "the sharer stopped responding while {what}"
        ))),
    }
}

pub async fn open_stream(
    conn: &Connection,
    secret: &str,
) -> Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream), OpenError> {
    let hello = wire::encode_hello(secret)
        .ok_or_else(|| OpenError::Transport("this ticket's secret is malformed".into()))?;
    let (mut send, mut recv) = deadlined("opening a stream", async {
        conn.open_bi().await.map_err(|e| {
            OpenError::Transport(format!(
                "the sharer closed the connection: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    deadlined("sending the ticket", async {
        send.write_all(&hello).await.map_err(|e| {
            OpenError::Transport(format!(
                "could not greet the sharer: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    let mut reply = [0u8; 1];
    deadlined("waiting for the answer", async {
        recv.read_exact(&mut reply).await.map_err(|e| {
            OpenError::Transport(format!(
                "the sharer did not answer the handshake: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    match reply[0] {
        wire::REPLY_OK => Ok((send, recv)),
        wire::REPLY_BUSY => Err(OpenError::Busy),
        wire::REPLY_UNREACHABLE => Err(OpenError::Unreachable),
        wire::REPLY_ROUTE_BROKEN => Err(OpenError::RouteBroken),
        wire::REPLY_SHARE_OVER => Err(OpenError::ShareOver),
        wire::REPLY_SHARER_FAULT => Err(OpenError::SharerFault),
        _ => Err(OpenError::Refused),
    }
}

/// Present the ticket once, right after connecting, and close the stream again.
///
/// Two things this buys, and the second one is a bug fix.
///
/// `h5i join` finds out now whether the ticket works. Before this it
/// printed "joined", along with the warning about running somebody else's
/// agent's code, on the strength of a QUIC handshake alone. A revoked or
/// expired ticket looked exactly like a good one until the first page load
/// answered `502`.
///
/// A joiner that nobody has visited yet stops being killed. The sharer hangs
/// up on a connection that has never authorized a stream, after
/// `UNAUTHENTICATED_GRACE`, because an endpoint anyone can dial must not be
/// holdable for free. But the normal way this feature is used is: send someone a
/// ticket, they run `h5i join`, and *then* they open the browser, so the real
/// client was the one being cut off, thirty seconds in, with "closed by peer:
/// h5i: no ticket was presented".
pub async fn verify_ticket(conn: &Connection, secret: &str) -> Result<(), OpenError> {
    let hello = wire::encode_probe(secret)
        .ok_or_else(|| OpenError::Transport("this ticket's secret is malformed".into()))?;
    let (mut send, mut recv) = deadlined("opening a stream", async {
        conn.open_bi().await.map_err(|e| {
            OpenError::Transport(format!(
                "the sharer closed the connection: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    deadlined("sending the ticket", async {
        send.write_all(&hello).await.map_err(|e| {
            OpenError::Transport(format!(
                "could not greet the sharer: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    let mut reply = [0u8; 1];
    deadlined("waiting for the answer", async {
        recv.read_exact(&mut reply).await.map_err(|e| {
            OpenError::Transport(format!(
                "the sharer did not answer the handshake: {}",
                h5i_core::redact::sanitize_display(&e.to_string())
            ))
        })
    })
    .await?;
    let _ = send.finish();
    match reply[0] {
        wire::REPLY_OK => Ok(()),
        wire::REPLY_BUSY => Err(OpenError::Busy),
        wire::REPLY_UNREACHABLE => Err(OpenError::Unreachable),
        wire::REPLY_ROUTE_BROKEN => Err(OpenError::RouteBroken),
        wire::REPLY_SHARE_OVER => Err(OpenError::ShareOver),
        wire::REPLY_SHARER_FAULT => Err(OpenError::SharerFault),
        _ => Err(OpenError::Refused),
    }
}

/// Report how a joined connection is actually carried, for the joiner's own
/// terminal. The sharer records the same observation in the receipt; this is so
/// the person clicking around knows whether a relay is in the path.
pub fn path_of(conn: &Connection) -> Option<Path> {
    observed_path(conn)
}

// Transport tests, and every one of them dials into a box: the dialer forks a
// helper into a network namespace, which is Linux. Sharing itself refuses on
// other platforms, so there is nothing here for them to check.
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::session::{self, ShareSession, Transport};

    /// `--direct-only`'s two enforcement points answer the same question.
    ///
    /// The watchdog polls once a second and the pump's gate is consulted before
    /// every write, and they were written to opposite predicates: the watchdog
    /// acted on "a relay is carrying it" and the gate on "a direct path is
    /// carrying it". Those are not complements. Between them is the instant
    /// with no selected path at all, which happens after a NAT rebinding or a
    /// local address change. The watchdog tolerates it *by name*, with a
    /// comment saying that treating it as a relay closed healthy connections;
    /// the gate barred it, which ends that direction of the pump and takes the
    /// connection with it. Silently, too: nothing on that path records a
    /// turned-away connection, so the flag h5i advertises as its strongest
    /// guarantee dropped streams during ordinary rebinding and the receipt said
    /// nothing.
    ///
    /// Stated over the three answers `observed_path` can give, because the
    /// middle one is the whole defect and a test using a real relay could not
    /// produce it.
    #[test]
    fn the_two_halves_of_direct_only_agree_about_a_path_that_is_settling() {
        // The gate bars exactly when the watchdog would close, and no oftener.
        // Written as a table over the answers rather than against a live
        // connection: `Connection` cannot be constructed in a unit test, and
        // the thing that was wrong is which answers map to which decision.
        for (path, should_bar) in [
            (Some(Path::Relayed), true),
            (Some(Path::Direct), false),
            (Some(Path::Tunnel), false),
            // The one that mattered. Not a relay carrying traffic. Nothing
            // carrying traffic.
            (None, false),
        ] {
            assert_eq!(
                relay_is_carrying(path),
                should_bar,
                "the watchdog's rule is wrong about {path:?}"
            );
            // The pump's gate is the negation of the same call, "may bytes
            // move", so asserting it here is what stops the two drifting back
            // apart.
            assert_eq!(
                !relay_is_carrying(path),
                !should_bar,
                "the write gate disagrees with the watchdog about {path:?}"
            );
        }
    }

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
    /// finish would mean waiting for a connection the test is not going to make,
    /// which is exactly the hang this comment exists to stop somebody
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
                crate::bridge::ClaimedRecord::on_disk(dir),
            )),
            secret,
        )
    }

    /// One QUIC connection carries one ticket.
    ///
    /// The protocol authorizes per stream, so two streams on one connection
    /// could present two different grants, and everything downstream assumed
    /// they could not. The peer record is created once per connection by
    /// whichever stream authorized first, so the second grant's connections,
    /// bytes and duration landed on the first grant's row in the receipt; and
    /// the connection-wide watchdog would close streams belonging to a live
    /// grant when a *different* one was revoked. Refused here rather than
    /// accounted for: `h5i join` holds one ticket and opens one connection, so
    /// this is nobody's case.
    #[tokio::test]
    async fn a_second_grant_on_one_connection_is_refused() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, first) = test_bridge(dir.path(), port);

        // A second, equally valid grant on the same share.
        let (grant, second) =
            session::mint_grant(Some("someone else".into()), 4_000_000_000).unwrap();
        session::update(dir.path(), |s| {
            s.grants.push(grant.clone());
            Ok(s.clone())
        })
        .expect("add a second grant");

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

        // The first ticket claims the connection.
        let (mut send, mut recv) = conn.open_bi().await.expect("open a stream");
        send.write_all(&wire::encode_hello(&first).expect("hello"))
            .await
            .expect("greet");
        let mut reply = [0u8; 1];
        recv.read_exact(&mut reply).await.expect("a reply");
        assert_eq!(reply[0], wire::REPLY_OK, "the first ticket was refused");

        // The second one, on the same connection, is not admitted, even
        // though it would be admitted on a connection of its own.
        let (mut send2, mut recv2) = conn.open_bi().await.expect("open a second stream");
        send2
            .write_all(&wire::encode_hello(&second).expect("hello"))
            .await
            .expect("greet");
        let mut reply2 = [0u8; 1];
        recv2.read_exact(&mut reply2).await.expect("a reply");
        assert_eq!(
            reply2[0],
            wire::REPLY_DENIED,
            "a second grant was admitted on a connection another grant owns"
        );

        // And the receipt attributes nothing to it: one peer, one grant.
        let summary = bridge.snapshot();
        assert_eq!(summary.peers.len(), 1, "a second peer record appeared");
        assert_eq!(summary.peers[0].grant, grant_label(&first, dir.path()));

        conn.close(0u32.into(), b"done");
        serving.abort();
    }

    /// The grant id a secret maps to, read back out of the table on disk.
    fn grant_label(secret: &str, dir: &std::path::Path) -> String {
        let sess = session::read(dir).expect("a session");
        let want = session::hash_secret(secret);
        sess.grants
            .iter()
            .find(|g| g.secret_sha256 == want)
            .expect("the grant this secret belongs to")
            .id
            .clone()
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
        // connection. Authorization is per stream, not per peer.
        let wrong = "ff".repeat(crate::ticket::SECRET_BYTES);
        let err = open_stream(&conn, &wrong).await.expect_err("refused");
        assert!(format!("{err}").contains("refused"), "{err}");

        // And the share recorded the peer that did get in.
        assert_eq!(bridge.peer_count(), 1);

        serving.abort();
        drop(conn);
    }

    /// A joiner that has connected but whose browser has not been opened yet.
    #[test]
    fn a_version_skew_is_not_reported_as_a_network_problem() {
        // The ALPN was bumped to `h5i/share/2` so two h5i versions fail to
        // agree *before* either speaks, rather than the newer joiner's probe
        // being read as junk by the older sharer and answered "your ticket was
        // refused. Ask for a new one" forever. What this pins is the half
        // that was still wrong: what the person is then told.
        //
        // Both directions, because they do not look the same on the wire.
        // A joiner older than the sharer gets a rejection the transport names:
        let said = format!(
            "{}",
            dial_failure("aborted by peer: the cryptographic handshake failed: error 120: peer doesn't support any known protocol")
        );
        assert!(said.contains("different version of h5i"), "{said}");
        assert!(!said.contains("moved networks"), "{said}");

        // A joiner *newer* than the sharer gets nothing at all: the old
        // endpoint does not reject the protocol it has never heard of, it just
        // never finishes the handshake. Measured through a real `connect`:
        // thirty seconds, then a timeout. That is indistinguishable from a
        // peer who is not there, so the message must not name only the two
        // causes that are not it.
        let said = format!("{}", dial_failure("timed out"));
        assert!(said.contains("do not speak the same protocol"), "{said}");
        assert!(said.contains("may have stopped sharing"), "{said}");
    }

    #[tokio::test]
    async fn a_ticket_presented_once_keeps_an_idle_joiner_alive() {
        // The sharer hangs up on a connection that never authorizes a stream,
        // because an endpoint anyone can dial must not be holdable for free.
        // But the normal way this feature is used is: send someone a ticket,
        // they run `h5i join`, and *then* they open the browser. That person
        // was being cut off thirty seconds in, with "closed by peer: h5i: no
        // ticket was presented", for doing nothing wrong.
        //
        // Time is paused, so this asserts about the grace itself rather than
        // about how long a test is willing to sit still.
        tokio::time::pause();
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
            .connect(addr.clone(), wire::ALPN)
            .await
            .expect("connect to the sharer");

        // What `h5i join` now does before it prints anything.
        verify_ticket(&conn, &secret)
            .await
            .expect("the ticket works");

        // And then nobody opens the page for a long time.
        tokio::time::advance(UNAUTHENTICATED_GRACE * 3).await;
        tokio::task::yield_now().await;
        assert!(
            conn.close_reason().is_none(),
            "an idle joiner that had presented its ticket was hung up on: {:?}",
            conn.close_reason()
        );

        // The connection is still usable, which is the point.
        let (mut send, mut recv) = open_stream(&conn, &secret).await.expect("still authorized");
        send.write_all(b"hello").await.expect("write");
        let mut got = [0u8; 9];
        recv.read_exact(&mut got).await.expect("read the reply");
        assert_eq!(&got, b"saw 5 byt");

        // And the grace has not been softened into nothing: a connection that
        // presents no ticket at all is still hung up on, which is the whole
        // reason it exists on an endpoint anyone can dial.
        let silent = local_endpoint(false).await;
        let quiet = silent.connect(addr, wire::ALPN).await.expect("connect");
        for _ in 0..20 {
            tokio::time::advance(UNAUTHENTICATED_GRACE).await;
            tokio::task::yield_now().await;
            if quiet.close_reason().is_some() {
                break;
            }
        }
        assert!(
            format!("{:?}", quiet.close_reason()).contains("no ticket"),
            "a connection that never presented a ticket was left alone: {:?}",
            quiet.close_reason()
        );

        serving.abort();
        drop(conn);
    }

    /// Counts connections and never answers, so a stream that reaches it stays
    /// reached: the shape that made the first version of the join-time probe
    /// hold one of the share's slots for the life of the joiner.
    fn counting_deaf_server() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        let counter = seen.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in l.incoming() {
                let Ok(c) = conn else { continue };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(c);
            }
        });
        (port, seen)
    }

    #[test]
    fn every_spelling_of_your_own_machine_is_refused() {
        // The first version of this check read an IP literal out of a `{:?}`
        // rendering, which caught `127.0.0.1` and missed the other spellings
        // of the same place. Demonstrated live at the time: a ticket naming
        // `http://0.0.0.0:39271/` made `h5i join` open a plaintext connection
        // to a listener on `127.0.0.1:39271`.
        let id = "ab".repeat(32);
        for bad in [
            r#"{"Ip":"127.0.0.1:2375"}"#,
            r#"{"Ip":"0.0.0.0:2375"}"#,
            r#"{"Ip":"[::1]:2375"}"#,
            r#"{"Ip":"[::]:2375"}"#,
            r#"{"Ip":"[::ffff:127.0.0.1]:2375"}"#,
            r#"{"Ip":"169.254.169.254:80"}"#,
            r#"{"Relay":"http://127.0.0.1:9200/"}"#,
            r#"{"Relay":"http://0.0.0.0:9200/"}"#,
            r#"{"Relay":"http://[::ffff:127.0.0.1]:9200/"}"#,
            r#"{"Relay":"https://[::1]/"}"#,
            // Found by the ticket fuzzer: a hostname, so the filter's IP
            // parsing saw nothing and let it through. 11434 is what an Ollama
            // listens on. RFC 6761 reserves this name to loopback, so no
            // resolver is needed to know what it means.
            r#"{"Relay":"http://localhost:11434/"}"#,
            r#"{"Relay":"http://LocalHost.:11434/"}"#,
            r#"{"Relay":"http://anything.localhost/"}"#,
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"id":"{id}","addrs":[{bad}]}}"#)).expect("json");
            assert!(
                refuse_addresses_that_point_inward(&v).is_err(),
                "a ticket naming {bad} was accepted"
            );
        }

        // And the ones that must still work: a LAN address, because two
        // machines on one office network is what direct p2p is for, and a real
        // relay by name.
        for ok in [
            r#"{"Ip":"192.168.1.20:41641"}"#,
            r#"{"Ip":"10.0.0.5:41641"}"#,
            r#"{"Relay":"https://use1-1.relay.n0.iroh.link./"}"#,
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"id":"{id}","addrs":[{ok}]}}"#)).expect("json");
            assert!(
                refuse_addresses_that_point_inward(&v).is_ok(),
                "a ticket naming {ok} was refused, which breaks the main use"
            );
        }
    }

    /// A relay URL is read by the parser that will dial it, and only h5i's own
    /// relays are dialled at all.
    ///
    /// Two separate ways past the previous check, both of which end with the
    /// joiner's relay client speaking HTTPS to something of the ticket
    /// author's choosing.
    #[test]
    fn a_relay_url_is_read_the_way_the_relay_client_reads_it() {
        let id = "ab".repeat(32);
        let refused = |entry: &str| {
            let v: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"id":"{id}","addrs":[{entry}]}}"#))
                    .expect("json");
            refuse_addresses_that_point_inward(&v).is_err()
        };

        // Userinfo. The hand-rolled authority split read the host of
        // `http://attacker@localhost:11434/` as `attacker@localhost`, which is
        // not `localhost` and so passed; `url::Url`, which is what the relay
        // client asks, reads it as `localhost` and dials loopback.
        assert!(refused(r#"{"Relay":"http://attacker@localhost:11434/"}"#));
        assert!(refused(r#"{"Relay":"http://user:pw@127.0.0.1:9200/"}"#));
        assert!(refused(
            r#"{"Relay":"https://x@use1-1.relay.n0.iroh.link./"}"#
        ));

        // A name the joiner would resolve. Refusing only literals and
        // `.localhost` left every other hostname, and the relay client
        // resolves it later and dials what comes back, so an `A 127.0.0.1`
        // record under a name the ticket's author controls restored the whole
        // problem. Re-resolving here would only add a rebinding window.
        assert!(refused(r#"{"Relay":"https://evil.example.com/"}"#));
        assert!(refused(
            r#"{"Relay":"https://relay.n0.iroh.link.evil.example/"}"#
        ));
        // Not a URL at all, and a scheme with no host.
        assert!(refused(r#"{"Relay":"not a url"}"#));
        assert!(refused(r#"{"Relay":"file:///etc/passwd"}"#));

        // The relays an honest ticket names, in the two spellings iroh emits.
        for ok in [
            r#"{"Relay":"https://use1-1.relay.n0.iroh.link./"}"#,
            r#"{"Relay":"https://euc1-1.relay.n0.iroh.link/"}"#,
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"id":"{id}","addrs":[{ok}]}}"#)).expect("json");
            assert!(
                refuse_addresses_that_point_inward(&v).is_ok(),
                "a ticket naming {ok} was refused, which breaks every share behind a NAT"
            );
        }
    }

    /// What the sharer prints passes the check the joiner will run.
    ///
    /// `ep.addr()` is the current relay plus *every* direct address, and a host
    /// with enough Docker bridges, VPN tunnels and dual-stack interfaces goes
    /// past the cap, so the sharer printed a confident invite that this same
    /// version of `h5i join` refused as attacker-shaped, with nothing on either
    /// side explaining why.
    #[test]
    fn a_minted_ticket_is_one_this_h5i_would_accept() {
        use iroh::TransportAddr;
        // A real key, because `EndpointId` is a curve point and not 32 bytes of
        // anything. The JSON-level tests above can use a hex string because
        // they never build an `EndpointAddr` out of it.
        let id = iroh::SecretKey::generate().public();

        // Thirteen candidates, one of them loopback: past the cap, and
        // carrying an address the join side refuses outright.
        let relay: iroh::RelayUrl = "https://use1-1.relay.n0.iroh.link./"
            .parse()
            .expect("relay");
        let mut addrs = vec![TransportAddr::Relay(relay)];
        addrs.push(TransportAddr::Ip("127.0.0.1:41641".parse().expect("ip")));
        for i in 1..=13u8 {
            addrs.push(TransportAddr::Ip(
                format!("192.168.1.{i}:41641").parse().expect("ip"),
            ));
        }
        let trimmed = trim_addressing(iroh::EndpointAddr::from_parts(id, addrs));

        assert!(trimmed.addrs.len() <= MAX_TICKET_ADDRS);
        // The relay survives the trim: it is the one address that works from
        // anywhere, so spending the budget on interfaces first would produce a
        // ticket that only works on the same LAN.
        assert!(trimmed.addrs.iter().any(TransportAddr::is_relay));
        let value = serde_json::to_value(&trimmed).expect("serialize");
        assert!(
            refuse_addresses_that_point_inward(&value).is_ok(),
            "the sharer minted a ticket its own joiner would refuse: {value}"
        );
    }

    #[test]
    fn a_ticket_cannot_name_two_hundred_places() {
        // Measured before this cap: one pasted ticket filled to the 8 KiB
        // limit produced 2,940 packets and 3.5 MB of UDP to 196 destinations
        // of the ticket author's choosing, from inside the joiner's network.
        let id = "ab".repeat(32);
        let many: Vec<String> = (1..200)
            .map(|i| format!(r#"{{"Ip":"192.168.1.{}:4164{}"}}"#, i % 250 + 1, i % 10))
            .collect();
        let v: serde_json::Value =
            serde_json::from_str(&format!(r#"{{"id":"{id}","addrs":[{}]}}"#, many.join(",")))
                .expect("json");
        let err = refuse_addresses_that_point_inward(&v).expect_err("199 addresses");
        assert!(format!("{err}").contains("names 199 addresses"), "{err}");

        // A handful is what a real ticket looks like.
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"id":"{id}","addrs":[{}]}}"#,
            many[..4].join(",")
        ))
        .expect("json");
        assert!(refuse_addresses_that_point_inward(&v).is_ok());
    }

    /// A dev server that accepts, reads, and then never answers.
    fn silent_after_read() -> u16 {
        use std::io::Read;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 1024];
                let _ = c.read(&mut buf);
                held.push(c);
            }
        });
        port
    }

    #[tokio::test]
    async fn a_revoke_reaches_every_stream_in_flight() {
        // The tunnel's watchdog is per *connection*; this one is per *stream*,
        // because one QUIC connection can carry two grants and enforcing a
        // revoke of the wrong one is worse than not enforcing it. Two
        // implementations of the same promise, and only one of them had been
        // tested with more than a single thing open.
        let port = silent_after_read();
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

        // Twenty streams on one connection, each parked waiting for a dev
        // server that will never answer.
        const N: usize = 20;
        let mut held = Vec::new();
        for _ in 0..N {
            let (mut send, recv) = open_stream(&conn, &secret).await.expect("authorized");
            send.write_all(b"GET / HTTP/1.1\r\n\r\n")
                .await
                .expect("write");
            held.push((send, recv));
        }
        for _ in 0..400 {
            if bridge.free_slots() == crate::bridge::Bridge::MAX_CONNECTIONS - N {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            crate::bridge::Bridge::MAX_CONNECTIONS - N,
            "the streams never all took a slot"
        );

        let id = session::read(dir.path()).expect("session").grants[0]
            .id
            .clone();
        crate::run::revoke(dir.path(), &id).expect("revoke");

        for _ in 0..400 {
            if bridge.free_slots() == crate::bridge::Bridge::MAX_CONNECTIONS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            crate::bridge::Bridge::MAX_CONNECTIONS,
            "a revoke left streams serving"
        );

        serving.abort();
        drop(held);
        drop(conn);
    }

    #[tokio::test]
    async fn a_flood_at_the_front_door_is_recorded_as_a_flood() {
        // Never tested. `front_refused` is the counter that distinguishes "this
        // share was hammered" from "somebody guessed at tickets", and every
        // test that exercised the sentence built the `Summary` by hand, so the
        // refusal, the increment, and the fact that a refused connection costs
        // no task were all unverified.
        //
        // Two different numbers on purpose: an endpoint anyone can dial is
        // refused *before* a credential is asked for, so this must not land in
        // the denial list.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret) = test_bridge(dir.path(), port);

        let sharer = local_endpoint(true).await;
        let addr = dialable(&sharer).await;
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, sharer, false).await }
        });

        // Hold every front-door slot open. Each connection presents its ticket
        // so the unauthenticated watchdog leaves it alone.
        let mut held = Vec::new();
        for _ in 0..MAX_LIVE_CONNECTIONS {
            let ep = local_endpoint(false).await;
            let conn = match ep.connect(addr.clone(), wire::ALPN).await {
                Ok(c) => c,
                Err(_) => break,
            };
            let _ = verify_ticket(&conn, &secret).await;
            held.push((ep, conn));
        }
        assert_eq!(
            held.len(),
            MAX_LIVE_CONNECTIONS,
            "could not open the front door's worth of connections"
        );

        // One more. It is refused at the transport, with no task and no
        // credential asked for.
        let extra = local_endpoint(false).await;
        if let Ok(conn) = extra.connect(addr, wire::ALPN).await {
            let _ = verify_ticket(&conn, &secret).await;
        }
        for _ in 0..200 {
            if bridge.snapshot().front_refused > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let s = bridge.snapshot();
        assert!(s.front_refused > 0, "a flood left no trace");
        assert!(
            s.denied.is_empty(),
            "a connection refused before any credential was counted as a bad ticket"
        );

        serving.abort();
        drop(held);
    }

    #[tokio::test]
    async fn checking_a_ticket_does_not_touch_the_box() {
        // The first version of this check opened a normal stream, so every
        // `h5i join` cost a connection to the dev server, one of the share's 64
        // slots, and (against a dev server that does not close when its client
        // stops writing, which is what this one imitates) a pump parked on
        // that slot until the joiner went away.
        use std::sync::atomic::Ordering as O;
        let (port, seen) = counting_deaf_server();
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

        for _ in 0..8 {
            verify_ticket(&conn, &secret)
                .await
                .expect("the ticket works");
        }
        // Give anything that was going to dial time to have done so.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(seen.load(O::SeqCst), 0, "a ticket check reached the box");

        // The peer is recorded even so: somebody presented a valid ticket, and
        // that they never loaded the page is a fact, not a reason to hide them.
        assert_eq!(bridge.peer_count(), 1);

        // And none of them took a slot. Asserting that "a stream still works"
        // would not show it: eight leaked permits out of sixty-four leaves that
        // true. So check the count the permits come from.
        assert_eq!(
            bridge.free_slots(),
            Bridge::MAX_CONNECTIONS,
            "a ticket check held one of the share's slots"
        );

        // A real stream still gets through afterwards.
        let (mut send, _recv) = open_stream(&conn, &secret).await.expect("still admitted");
        send.write_all(b"hello").await.expect("write");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            seen.load(O::SeqCst),
            1,
            "the real stream did not reach the box"
        );

        serving.abort();
        drop(conn);
    }

    #[tokio::test]
    async fn a_revoked_ticket_is_refused_at_join_time_rather_than_at_first_page_load() {
        // `h5i join` used to print "joined", and the warning about running
        // somebody else's agent's code, on the strength of a QUIC handshake
        // alone. A revoked ticket looked exactly like a good one until the
        // first page load answered 502.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret) = test_bridge(dir.path(), port);

        let sharer = local_endpoint(true).await;
        let addr = dialable(&sharer).await;
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, sharer, false).await }
        });

        crate::run::stop(dir.path()).expect("stop the share");

        let joiner = local_endpoint(false).await;
        let conn = joiner
            .connect(addr, wire::ALPN)
            .await
            .expect("connect to the sharer");
        let err = verify_ticket(&conn, &secret).await.expect_err("revoked");
        assert!(matches!(err, OpenError::Refused), "{err}");

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

        // And it cuts on a character, not on a byte. Byte 12 lands inside the
        // sixth two-byte character here, which `&id[..12]` panics on. In a
        // function called from the connection-accept path, where that is the
        // share going down. No caller passes this today; that is a fact about
        // the callers, and this is the function.
        assert_eq!(short("aααααααααααα"), "aααααα…");
        // The boundary case just below: an exact cut is still taken whole.
        assert_eq!(short("αααααα0123456789"), "αααααα…");
        // A single character wider than the budget leaves nothing to show
        // rather than splitting it.
        assert_eq!(short("👍👍👍👍"), "👍👍👍…");
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

#[cfg(test)]
mod ticket_fuzz {
    use super::*;
    use crate::fuzz::{encode_ticket, rounds, ticket_json, Rng};

    /// What a pasted ticket may make this machine do.
    ///
    /// The two defects hand-written review found here were both about what an
    /// accepted ticket is allowed to contain, so these are properties of the
    /// *accepted* set rather than of the parser's mood: nothing that points at
    /// the joiner's own machine, and not very many places at all.
    #[test]
    fn no_ticket_this_accepts_points_at_the_joiner_or_names_a_crowd() {
        let mut rng = Rng::new(0x71C4E7);
        let mut decoded = 0usize;
        let mut with_addrs = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let body = ticket_json(&mut one);
            let text = encode_ticket(&body);
            let ctx = || format!("round {i}, seed {seed:#x}, body {body}");

            // Never panics, whatever it is handed.
            let Ok(t) = crate::ticket::Ticket::decode(&text) else {
                continue;
            };
            decoded += 1;

            // A decoded ticket's secret is always something the gate can
            // compare without arguing about encodings.
            assert_eq!(
                t.secret.len(),
                crate::ticket::SECRET_BYTES * 2,
                "a decoded ticket carried a secret of the wrong width: {}",
                ctx()
            );
            assert!(
                t.secret.bytes().all(|b| b.is_ascii_hexdigit()),
                "a decoded ticket carried a secret that is not hex: {}",
                ctx()
            );

            // And what the joiner would then dial.
            let listed = t
                .addr
                .get("addrs")
                .and_then(|a| a.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if listed > 0 {
                with_addrs += 1;
            }
            if refuse_addresses_that_point_inward(&t.addr).is_ok() {
                assert!(
                    listed <= MAX_TICKET_ADDRS,
                    "an accepted ticket named {listed} places: {}",
                    ctx()
                );
                for entry in t
                    .addr
                    .get("addrs")
                    .and_then(|a| a.as_array())
                    .into_iter()
                    .flatten()
                {
                    let text = entry.to_string();
                    for inward in [
                        "127.0.0.1",
                        "0.0.0.0",
                        "[::1]",
                        "[::]",
                        "::ffff:127.0.0.1",
                        "169.254.",
                        "localhost",
                    ] {
                        assert!(
                            !text.contains(inward),
                            "an accepted ticket named {inward}: {}",
                            ctx()
                        );
                    }
                }
            }
        }

        // Floors, so a generator that stops producing tickets this can decode
        // cannot read as coverage. Learned the hard way on the HTTP fuzzer,
        // whose headline invariant had never executed once.
        let n = rounds();
        assert!(
            decoded * 20 > n,
            "only {decoded} of {n} generated tickets decoded at all"
        );
        assert!(
            with_addrs * 50 > n,
            "only {with_addrs} of {n} carried any addressing, so the filter was barely asked"
        );
    }
}
