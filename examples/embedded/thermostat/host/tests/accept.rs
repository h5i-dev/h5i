//! The host accept loop: errors back off, connections are capped.

use std::cell::Cell;
use std::time::{Duration, Instant};

use thermostat_app::App;
use thermostat_host::{Accept, accept_with_backoff, backoff, run};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Failing {
    calls: Cell<u32>,
    fail: u32,
}

impl Accept for Failing {
    type Conn = ();
    async fn accept(&self) -> std::io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() <= self.fail {
            Err(std::io::Error::other("EMFILE"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn backoff_grows_and_is_capped() {
    assert_eq!(backoff(1), Duration::from_millis(50));
    assert_eq!(backoff(2), Duration::from_millis(100));
    assert_eq!(backoff(7), Duration::from_millis(2000));
    assert_eq!(backoff(1000), Duration::from_secs(2));
}

/// A listener that keeps failing is retried a few times a second, not spun.
#[test]
fn accept_errors_do_not_spin() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let l = Failing { calls: Cell::new(0), fail: u32::MAX };
        let _ = tokio::time::timeout(Duration::from_millis(1000), accept_with_backoff(&l)).await;
        assert!(l.calls.get() <= 6, "{} accept calls in 1 s", l.calls.get());
        let l = Failing { calls: Cell::new(0), fail: 2 };
        accept_with_backoff(&l).await;
        assert_eq!(l.calls.get(), 3);
    });
}

/// With a cap of one, a second client waits while the first holds its
/// connection; the first is cut once its request takes longer than the bound.
#[test]
fn connections_are_capped() {
    static APP: App = App::new(b"k", 1);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::task::spawn_local(run(listener, &APP, 1, Duration::from_millis(800)));
        let mut first = tokio::net::TcpStream::connect(addr).await.unwrap();
        first.write_all(b"GET /state HTTP/1.1\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t0 = Instant::now();
        let mut second = tokio::net::TcpStream::connect(addr).await.unwrap();
        second.write_all(b"GET /state HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut out = Vec::new();
        second.read_to_end(&mut out).await.unwrap();
        // Served only once the first request's 800 ms were up.
        assert!(t0.elapsed() >= Duration::from_millis(500), "{:?}", t0.elapsed());
        assert!(out.starts_with(b"HTTP/1.1 200 OK"), "{}", String::from_utf8_lossy(&out));
        let mut cut = Vec::new();
        first.read_to_end(&mut cut).await.unwrap();
        assert!(cut.starts_with(b"HTTP/1.1 408 "), "{}", String::from_utf8_lossy(&cut));
    });
}
