//! Moving bytes between two halves of a share, and counting them as they go.
//!
//! Deliberately dumb. Once a connection is authorized the bridge has no opinion
//! about what travels over it: it is HTTP, or a WebSocket carrying hot-reload
//! messages, or server-sent events, or the app's own protocol. Anything that
//! tried to understand the payload would be a thing that gets it wrong for one
//! framework and silently breaks the share.
//!
//! Two things it does owe the caller.
//!
//! Counting as it goes, not at the end. A revoke kills live connections by
//! dropping the future mid-copy, and a total that only existed in the return
//! value would be lost exactly for the connections a reviewer most wants to see.
//! So the totals live in atomics the caller owns and reads afterwards either
//! way.
//!
//! An honest shutdown in both directions. A half-closed peer must not leave
//! the other side blocked on a socket that will never say anything again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 32 KiB. Large enough that a page load is a handful of syscalls, small enough
/// that a thousand idle connections are not a hundred megabytes of buffers.
const CHUNK: usize = 32 * 1024;

/// How long the surviving half of a half-closed connection gets to finish.
///
/// Applied only *after* the other direction has ended, which is what makes it
/// safe to be this blunt: while both halves are live nothing here has a clock,
/// so a hot-reload socket or an event stream runs for as long as the ticket
/// does. A half-close is the browser having gone, and there is nobody left to
/// read a response that arrives four minutes later.
///
/// Unbounded, it was a slot leak with a one-line reproducer. A browser closing
/// a WebSocket makes the peer-to-box direction reach EOF, which shuts down the
/// dev server's socket write half; a server that ignores EOF and stays open but
/// silent then leaves the box-to-peer direction reading forever. Over P2P
/// closing that one browser stream does not close the shared QUIC connection,
/// so the outer `conn.closed()` arm never fires either. Each occurrence keeps a
/// `Bridge::admit` permit, and sixty-four ordinary page reloads against such a
/// server made the share answer `busy` until the ticket expired.
const DRAIN_GRACE: Duration = Duration::from_secs(60);

/// How a direction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The reader ran out, or the far side went away. The other direction may
    /// legitimately still be going. That is a half-close.
    Cleanly,
    /// The far side stopped *reading*. Nothing more can be delivered to it, in
    /// either direction, so the connection is over.
    WriteStalled,
    /// The gate refused. Like [`Ended::WriteStalled`], this ends the whole
    /// connection rather than half of it: the reason a gate says no,
    /// `--direct-only` and a path that is no longer direct, is a fact about
    /// the connection, not about one direction of it.
    Barred,
}

/// Whether bytes may still move, asked immediately before each write.
///
/// The one caller that supplies a real one is `--direct-only` (see
/// [`crate::p2p`]): the connection's selected path is watched by a once-a-second
/// poll, and a direct path that falls back to a relay between two of those polls
/// used to mean up to a second of application traffic crossing a third party
/// before the connection was closed. For a flag whose whole promise is that
/// none does. Consulted here, the bound becomes "whatever QUIC had already
/// accepted at the instant the path changed" rather than "a second's worth",
/// and nothing this copy has not yet handed over is handed over afterwards.
pub type Gate = dyn Fn() -> bool + Send + Sync;

/// Copy one direction, adding to `counter` after every write that lands.
async fn copy_counted<R, W>(mut r: R, mut w: W, counter: &AtomicU64, gate: Option<&Gate>) -> Ended
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Asked between the read and the write, so a refusal stops bytes that
        // have not yet been handed to the transport.
        if let Some(gate) = gate
            && !gate()
        {
            let _ = w.shutdown().await;
            return Ended::Barred;
        }
        // Deadlined, like every other write in the crate. A peer that stops
        // reading an upgraded connection, a backgrounded tab holding a
        // hot-reload socket, would otherwise park this copy forever, holding
        // one of the share's slots for the life of the ticket.
        if let Err(e) = crate::http_front::write_timed(&mut w, &buf[..n]).await {
            let _ = w.shutdown().await;
            return if e.kind() == std::io::ErrorKind::TimedOut {
                Ended::WriteStalled
            } else {
                Ended::Cleanly
            };
        }
        // After the write, so the number describes bytes that were delivered
        // rather than bytes that were read and then dropped on the floor.
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
    // Tell the far side this direction is done. Without it, a browser that goes
    // away mid-request leaves the copy from the dev server waiting forever, and
    // the connection never finishes closing.
    let _ = w.shutdown().await;
    Ended::Cleanly
}

/// Copy in both directions until both are done, counting into the two atomics.
pub async fn duplex<RA, WA, RB, WB>(
    read_a: RA,
    write_a: WA,
    read_b: RB,
    write_b: WB,
    a_to_b: &AtomicU64,
    b_to_a: &AtomicU64,
) where
    RA: AsyncRead + Unpin + Send,
    WA: AsyncWrite + Unpin + Send,
    RB: AsyncRead + Unpin + Send,
    WB: AsyncWrite + Unpin + Send,
{
    duplex_gated(read_a, write_a, read_b, write_b, a_to_b, b_to_a, None).await
}

/// [`duplex`], with a gate consulted before every write. See [`Gate`].
#[allow(clippy::too_many_arguments)]
pub async fn duplex_gated<RA, WA, RB, WB>(
    read_a: RA,
    write_a: WA,
    read_b: RB,
    write_b: WB,
    a_to_b: &AtomicU64,
    b_to_a: &AtomicU64,
    gate: Option<&Gate>,
) where
    RA: AsyncRead + Unpin + Send,
    WA: AsyncWrite + Unpin + Send,
    RB: AsyncRead + Unpin + Send,
    WB: AsyncWrite + Unpin + Send,
{
    // Not `join!`. A peer that stops reading, the zero-receive-window case the
    // write deadline exists for, leaves the *other* direction parked on a
    // socket that is open and silent, so waiting for both meant the deadline
    // fired and the connection was held anyway, which is the thing it was added
    // to prevent. A stalled write ends the whole connection; anything else is a
    // half-close and the other direction is still owed its chance to finish.
    let a = copy_counted(read_a, write_b, a_to_b, gate);
    let b = copy_counted(read_b, write_a, b_to_a, gate);
    tokio::pin!(a, b);
    // "Still owed its chance to finish" is not "owed forever", which is what
    // the bare `.await` here meant. See [`DRAIN_GRACE`] for the shape that made
    // sixty-four page reloads exhaust a share's capacity until the ticket
    // expired.
    tokio::select! {
        ended = &mut a => {
            if ended == Ended::Cleanly {
                let _ = tokio::time::timeout(DRAIN_GRACE, b).await;
            }
        }
        ended = &mut b => {
            if ended == Ended::Cleanly {
                let _ = tokio::time::timeout(DRAIN_GRACE, a).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bytes_go_both_ways_and_are_counted() {
        let (mut peer, peer_side) = tokio::io::duplex(1024);
        let (mut server, server_side) = tokio::io::duplex(1024);
        let (peer_r, peer_w) = tokio::io::split(peer_side);
        let (server_r, server_w) = tokio::io::split(server_side);

        let to_server = std::sync::Arc::new(AtomicU64::new(0));
        let to_peer = std::sync::Arc::new(AtomicU64::new(0));
        let (a, b) = (to_server.clone(), to_peer.clone());
        let joined =
            tokio::spawn(async move { duplex(peer_r, peer_w, server_r, server_w, &a, &b).await });

        peer.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        peer.shutdown().await.unwrap();
        let mut got = Vec::new();
        server.read_to_end(&mut got).await.unwrap();
        assert_eq!(&got, b"GET / HTTP/1.1\r\n\r\n");

        server
            .write_all(b"HTTP/1.1 200 OK\r\n\r\nhi")
            .await
            .unwrap();
        server.shutdown().await.unwrap();
        let mut back = Vec::new();
        peer.read_to_end(&mut back).await.unwrap();
        assert_eq!(&back, b"HTTP/1.1 200 OK\r\n\r\nhi");

        joined.await.unwrap();
        assert_eq!(to_server.load(Ordering::Relaxed), 18);
        assert_eq!(to_peer.load(Ordering::Relaxed), 21);
    }

    #[tokio::test]
    async fn a_connection_killed_mid_copy_still_reports_what_it_moved() {
        // The reason the counters are atomics the caller owns: a revoke drops
        // this future, and the bytes it had already carried are exactly what
        // the receipt needs.
        let (mut peer, peer_side) = tokio::io::duplex(1024);
        let (mut server, server_side) = tokio::io::duplex(1024);
        let (peer_r, peer_w) = tokio::io::split(peer_side);
        let (server_r, server_w) = tokio::io::split(server_side);

        let to_server = std::sync::Arc::new(AtomicU64::new(0));
        let to_peer = std::sync::Arc::new(AtomicU64::new(0));
        let (a, b) = (to_server.clone(), to_peer.clone());
        let task =
            tokio::spawn(async move { duplex(peer_r, peer_w, server_r, server_w, &a, &b).await });

        peer.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        server.read_exact(&mut buf).await.unwrap();
        // Now cut it off the way the revoke watchdog does.
        task.abort();
        let _ = task.await;
        assert_eq!(to_server.load(Ordering::Relaxed), 5);
    }

    /// A half-close against a server that ignores it does not hold the slot.
    ///
    /// The existing "one side going away" test drops *both* endpoints, so it
    /// never exercised this: the peer goes and the server stays open and
    /// silent, which is the ordinary shape of an agent-written dev server that
    /// does not act on EOF. Unbounded, the box-to-peer copy read forever and
    /// the `Bridge::admit` permit went with it. Sixty-four reloads and the
    /// share answers `busy` until the ticket expires.
    #[tokio::test(start_paused = true)]
    async fn a_silent_server_after_the_peer_hangs_up_does_not_hold_the_slot() {
        let (peer, peer_side) = tokio::io::duplex(64);
        // Held, not dropped: this end stays open and says nothing, which is
        // exactly the case the drain exists for.
        let (_server, server_side) = tokio::io::duplex(64);
        let (peer_r, peer_w) = tokio::io::split(peer_side);
        let (server_r, server_w) = tokio::io::split(server_side);
        let a = AtomicU64::new(0);
        let b = AtomicU64::new(0);
        drop(peer);

        let out = tokio::time::timeout(
            DRAIN_GRACE + Duration::from_secs(30),
            duplex(peer_r, peer_w, server_r, server_w, &a, &b),
        )
        .await;
        assert!(
            out.is_ok(),
            "the surviving half waited past the drain window for a server that had nothing to say"
        );
    }

    /// A gate that says no stops bytes that have not yet been written.
    ///
    /// This is `--direct-only`'s enforcement below the once-a-second watchdog:
    /// between two polls, a direct path falling back to a relay used to mean up
    /// to a second of application traffic crossing a third party for a flag
    /// that promises none does.
    #[tokio::test]
    async fn a_closed_gate_stops_the_next_byte_rather_than_the_next_poll() {
        let (mut peer, peer_side) = tokio::io::duplex(1024);
        let (mut server, server_side) = tokio::io::duplex(1024);
        let (peer_r, peer_w) = tokio::io::split(peer_side);
        let (server_r, server_w) = tokio::io::split(server_side);

        let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = {
            let open = open.clone();
            std::sync::Arc::new(move || open.load(Ordering::Relaxed))
        };
        let to_server = std::sync::Arc::new(AtomicU64::new(0));
        let to_peer = std::sync::Arc::new(AtomicU64::new(0));
        let (a, b, g) = (to_server.clone(), to_peer.clone(), gate.clone());
        let task = tokio::spawn(async move {
            duplex_gated(peer_r, peer_w, server_r, server_w, &a, &b, Some(g.as_ref())).await
        });

        peer.write_all(b"first").await.unwrap();
        let mut buf = [0u8; 5];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"first");

        // The path went relayed. Nothing after this may reach the box.
        open.store(false, Ordering::Relaxed);
        peer.write_all(b"second").await.unwrap();

        let ended = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(ended.is_ok(), "a barred pump did not finish");
        assert_eq!(
            to_server.load(Ordering::Relaxed),
            5,
            "bytes read after the gate closed were still delivered"
        );
        // The box side sees the shutdown, and nothing after it.
        let mut leaked = [0u8; 6];
        let got = tokio::time::timeout(Duration::from_secs(1), server.read(&mut leaked))
            .await
            .expect("the box side should not be left hanging");
        assert!(
            matches!(got, Ok(0) | Err(_)),
            "the second write crossed a gate that had closed: {got:?}"
        );
    }

    #[tokio::test]
    async fn one_side_going_away_does_not_wedge_the_other() {
        // The shape this prevents: a browser tab closing mid-request leaves the
        // copy from the dev server blocked forever, and the share stops
        // accepting anyone.
        let (peer, peer_side) = tokio::io::duplex(64);
        let (server, server_side) = tokio::io::duplex(64);
        let (peer_r, peer_w) = tokio::io::split(peer_side);
        let (server_r, server_w) = tokio::io::split(server_side);
        let a = AtomicU64::new(0);
        let b = AtomicU64::new(0);
        drop(peer);
        drop(server);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            duplex(peer_r, peer_w, server_r, server_w, &a, &b),
        )
        .await;
        assert!(
            out.is_ok(),
            "duplex did not finish when both peers went away"
        );
    }
}
