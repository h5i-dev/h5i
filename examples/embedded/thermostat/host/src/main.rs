//! The thermostat server on a host, over tokio: the same `thermostat-app`
//! router and `h5i-app-embedded` shell the firmware runs, with the same
//! buffer sizes.
//!
//! ```text
//! cargo run -p thermostat-host            # 127.0.0.1:8080, key "dev-key"
//! curl -s localhost:8080/state
//! curl -s -X PUT -H 'Authorization: Bearer dev-key' -d 215 localhost:8080/setpoint
//! ```
//!
//! `THERMOSTAT_ADDR` and `THERMOSTAT_KEY` override the address and key.

use std::time::Duration;

use embedded_io_adapters::tokio_1::FromTokio;
use h5i_app_embedded::{Config, serve};
use thermostat_app::{App, router};

/// The firmware's buffers.
pub const RX: usize = 2048;
pub const TX: usize = 1024;

fn main() {
    let addr = std::env::var("THERMOSTAT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let key = std::env::var("THERMOSTAT_KEY").unwrap_or_else(|_| "dev-key".into());
    let app: &'static App = Box::leak(Box::new(App::new(Box::leak(key.into_bytes().into_boxed_slice()))));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
        eprintln!("thermostat listening on http://{addr}");
        loop {
            let (sock, peer) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("accept: {e}");
                    continue;
                }
            };
            tokio::task::spawn_local(async move {
                let mut io = FromTokio::new(sock);
                let mut rx = [0u8; RX];
                let mut tx = [0u8; TX];
                let routes = router();
                // The shell has no clock: an idle connection is closed here.
                let r = tokio::time::timeout(
                    Duration::from_secs(30),
                    serve(&Config::default(), &routes, app, &mut rx, &mut tx, &mut io),
                )
                .await;
                match r {
                    Ok(Ok(n)) => eprintln!("{peer}: {n} request(s)"),
                    Ok(Err(e)) => eprintln!("{peer}: {e:?}"),
                    Err(_) => eprintln!("{peer}: timed out"),
                }
            });
        }
    });
}
