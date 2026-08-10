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
//! **Counting as it goes, not at the end.** A revoke kills live connections by
//! dropping the future mid-copy, and a total that only existed in the return
//! value would be lost exactly for the connections a reviewer most wants to see.
//! So the totals live in atomics the caller owns and reads afterwards either
//! way.
//!
//! **An honest shutdown in both directions.** A half-closed peer must not leave
//! the other side blocked on a socket that will never say anything again.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 32 KiB. Large enough that a page load is a handful of syscalls, small enough
/// that a thousand idle connections are not a hundred megabytes of buffers.
const CHUNK: usize = 32 * 1024;

/// Copy one direction, adding to `counter` after every write that lands.
async fn copy_counted<R, W>(mut r: R, mut w: W, counter: &AtomicU64)
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
        if w.write_all(&buf[..n]).await.is_err() {
            break;
        }
        // After the write, so the number describes bytes that were delivered
        // rather than bytes that were read and then dropped on the floor.
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
    // Tell the far side this direction is done. Without it, a browser that goes
    // away mid-request leaves the copy from the dev server waiting forever, and
    // the connection never finishes closing.
    let _ = w.shutdown().await;
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
    tokio::join!(
        copy_counted(read_a, write_b, a_to_b),
        copy_counted(read_b, write_a, b_to_a),
    );
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

        server.write_all(b"HTTP/1.1 200 OK\r\n\r\nhi").await.unwrap();
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
        assert!(out.is_ok(), "duplex did not finish when both peers went away");
    }
}
