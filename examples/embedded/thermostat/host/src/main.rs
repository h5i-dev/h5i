//! The thermostat server on a host, over tokio: the same `thermostat-app`
//! routes and `h5i-app-embedded` shell the firmware runs, with the same
//! buffer sizes, plus `POST /temp` in place of the sensor.
//!
//! ```text
//! cargo run -p thermostat-host            # 127.0.0.1:8080, key "dev-key"
//! curl -s localhost:8080/state
//! scripts/embedded/h5i-hmac.py --key dev-key PUT http://127.0.0.1:8080/setpoint 215
//! ```
//!
//! `THERMOSTAT_ADDR` and `THERMOSTAT_KEY` override the address and key.

use std::time::Duration;

use thermostat_app::App;

/// Connections served at once; more wait in the listen backlog.
const MAX_CONNS: usize = 32;
/// The longest a request may take to arrive, or a response to leave.
const BOUND: Duration = Duration::from_secs(10);

fn main() {
    let addr = std::env::var("THERMOSTAT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let key = std::env::var("THERMOSTAT_KEY").unwrap_or_else(|_| "dev-key".into());
    let key: &'static [u8] = Box::leak(key.into_bytes().into_boxed_slice());
    let app: &'static App = Box::leak(Box::new(App::new(key, thermostat_host::boot_id())));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
        eprintln!("thermostat listening on http://{addr} (boot {:08x})", app.admin.boot());
        thermostat_host::run(listener, app, MAX_CONNS, BOUND).await;
    });
}
