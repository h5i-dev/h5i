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

use thermostat_app::App;

/// Connections served at once; more wait in the listen backlog.
const MAX_CONNS: usize = 32;
/// The longest one connection may stay open.
const PER_CONN: Duration = Duration::from_secs(30);

fn main() {
    let addr = std::env::var("THERMOSTAT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let key = std::env::var("THERMOSTAT_KEY").unwrap_or_else(|_| "dev-key".into());
    let app: &'static App = Box::leak(Box::new(App::new(Box::leak(key.into_bytes().into_boxed_slice()))));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
        eprintln!("thermostat listening on http://{addr}");
        thermostat_host::run(listener, app, MAX_CONNS, PER_CONN).await;
    });
}
