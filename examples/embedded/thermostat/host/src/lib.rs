//! The host runner's accept loop, kept apart from `main` so it can be tested.

use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use embedded_io_adapters::tokio_1::FromTokio;
use h5i_app_embedded::{Config, serve};
use thermostat_app::{App, router};

/// The firmware's buffers.
pub const RX: usize = 2048;
pub const TX: usize = 1024;

/// How long an accept loop waits after its `n`th error in a row (`n >= 1`):
/// 50 ms doubling to at most 2 s, so a persistent error (EMFILE) cannot spin.
pub fn backoff(n: u32) -> Duration {
    Duration::from_millis(50u64.saturating_mul(1u64 << (n.saturating_sub(1)).min(6))).min(Duration::from_secs(2))
}

/// Something that yields connections.
pub trait Accept {
    type Conn;
    fn accept(&self) -> impl Future<Output = std::io::Result<Self::Conn>>;
}

impl Accept for tokio::net::TcpListener {
    type Conn = tokio::net::TcpStream;
    async fn accept(&self) -> std::io::Result<tokio::net::TcpStream> {
        tokio::net::TcpListener::accept(self).await.map(|(s, _)| s)
    }
}

/// The next connection, waiting `backoff` after each error.
pub async fn accept_with_backoff<L: Accept>(l: &L) -> L::Conn {
    let mut errors = 0u32;
    loop {
        match l.accept().await {
            Ok(c) => return c,
            Err(e) => {
                errors += 1;
                if errors == 1 || errors.is_power_of_two() {
                    eprintln!("accept: {e} ({errors} in a row)");
                }
                tokio::time::sleep(backoff(errors)).await;
            }
        }
    }
}

/// Serve connections on `listener`, at most `max_conns` at once (the rest
/// wait in the listen backlog), each for at most `per_conn`. Run inside a
/// `LocalSet`.
pub async fn run(listener: tokio::net::TcpListener, app: &'static App, max_conns: usize, per_conn: Duration) {
    let live = Rc::new(Cell::new(0usize));
    loop {
        while live.get() >= max_conns {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let sock = accept_with_backoff(&listener).await;
        live.set(live.get() + 1);
        let live = live.clone();
        tokio::task::spawn_local(async move {
            let mut io = FromTokio::new(sock);
            let mut rx = [0u8; RX];
            let mut tx = [0u8; TX];
            let routes = router();
            // The shell has no clock: a slow or idle connection is cut here.
            let _ = tokio::time::timeout(per_conn, serve(&Config::default(), &routes, app, &mut rx, &mut tx, &mut io)).await;
            live.set(live.get() - 1);
        });
    }
}
