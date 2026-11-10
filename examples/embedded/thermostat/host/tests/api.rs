//! The thermostat API over the real shell, through an in-memory socket.

use std::cell::Cell;

use embedded_io_adapters::tokio_1::FromTokio;
use h5i_app_embedded::hmac::authorization;
use h5i_app_embedded::{Config, NoBound, serve};
use thermostat_app::{App, router, sim_router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static KEY: &[u8] = b"k";
const BOOT: u32 = 0x1234_5678;

/// One connection: send `input`, half-close, collect the answer. `sim` adds `POST /temp`.
fn exchange_on(app: &App, input: &[u8], sim: bool) -> String {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        let (client, server) = tokio::io::duplex(1 << 16);
        let (mut cr, mut cw) = tokio::io::split(client);
        let input = input.to_vec();
        let w = async move {
            cw.write_all(&input).await.unwrap();
            cw.shutdown().await.unwrap();
        };
        let r = async move {
            let mut v = Vec::new();
            cr.read_to_end(&mut v).await.unwrap();
            v
        };
        let s = async {
            let mut io = FromTokio::new(server);
            let mut rx = [0u8; 2048];
            let mut tx = [0u8; 1024];
            if sim {
                let _ = serve(&Config::default(), &sim_router(), app, &mut rx, &mut tx, &mut io, &NoBound).await;
            } else {
                let _ = serve(&Config::default(), &router(), app, &mut rx, &mut tx, &mut io, &NoBound).await;
            }
        };
        let ((), out, ()) = tokio::join!(w, r, s);
        String::from_utf8_lossy(&out).into_owned()
    })
}

fn exchange(app: &App, input: &[u8]) -> String {
    exchange_on(app, input, true)
}

/// Signs requests with increasing counters.
struct Client {
    counter: Cell<u64>,
}

impl Client {
    fn new() -> Self {
        Client { counter: Cell::new(0) }
    }

    fn signed(&self, method: &str, path: &str, body: &str) -> Vec<u8> {
        self.counter.set(self.counter.get() + 1);
        let mut a = [0u8; 128];
        let n = authorization(KEY, BOOT, self.counter.get(), method.as_bytes(), path.as_bytes(), body.as_bytes(), &mut a);
        let a = std::str::from_utf8(&a[..n]).unwrap();
        format!("{method} {path} HTTP/1.1\r\nHost: t\r\nAuthorization: {a}\r\nContent-Length: {}\r\n\r\n{body}", body.len())
            .into_bytes()
    }

    fn put(&self, path: &str, body: &str) -> Vec<u8> {
        self.signed("PUT", path, body)
    }
}

#[test]
fn state_and_writes() {
    let app = App::new(KEY, BOOT);
    let c = Client::new();
    let out = exchange(&app, b"GET /state HTTP/1.1\r\nHost: t\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"), "{out}");
    assert!(out.ends_with(r#"{"setpoint_dc":200,"temp_dc":200,"heater":false,"relays":[false,false,false,false],"mode":"off"}"#));
    let out = exchange(&app, &c.put("/mode", "heat"));
    assert!(out.contains(r#""heater":true"#), "{out}");
    let out = exchange(&app, &c.put("/relay/2", "on"));
    assert!(out.contains(r#""relays":[false,false,true,false]"#), "{out}");
    let out = exchange(&app, &c.signed("POST", "/temp", "-12"));
    assert!(out.contains(r#""temp_dc":-12,"heater":true"#), "{out}");
    let out = exchange(&app, &c.signed("POST", "/temp", "360"));
    assert!(out.contains(r#""temp_dc":360,"heater":false"#), "{out}");
}

#[test]
fn refusals() {
    let app = App::new(KEY, BOOT);
    let c = Client::new();
    assert!(exchange(&app, &c.put("/relay/4", "on")).starts_with("HTTP/1.1 404 "));
    assert!(exchange(&app, &c.put("/relay/255", "on")).contains(r#"{"error":"no_such_relay"}"#));
    assert!(exchange(&app, &c.put("/relay/256", "on")).starts_with("HTTP/1.1 404 "));
    assert!(exchange(&app, &c.put("/setpoint", "301")).starts_with("HTTP/1.1 422 "));
    assert!(exchange(&app, &c.put("/setpoint", "x")).starts_with("HTTP/1.1 400 "));
    let anon = b"PUT /mode HTTP/1.1\r\nHost: t\r\nContent-Length: 4\r\n\r\nheat";
    let out = exchange(&app, anon);
    assert!(out.starts_with("HTTP/1.1 401 ") && out.contains("WWW-Authenticate: H5i-HMAC boot=12345678,last="), "{out}");
    let bearer = b"PUT /mode HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer k\r\nContent-Length: 4\r\n\r\nheat";
    assert!(exchange(&app, bearer).starts_with("HTTP/1.1 401 "));
    // A captured request replayed.
    let req = c.put("/mode", "auto");
    assert!(exchange(&app, &req).starts_with("HTTP/1.1 200 "));
    assert!(exchange(&app, &c.put("/mode", "off")).starts_with("HTTP/1.1 200 "));
    assert!(exchange(&app, &req).starts_with("HTTP/1.1 401 "));
    assert!(exchange(&app, b"GET /state HTTP/1.1\r\nHost: t\r\n\r\n").contains(r#""mode":"off""#));
}

#[test]
fn pipelined_signed_writes() {
    let app = App::new(KEY, BOOT);
    let c = Client::new();
    let mut input = c.put("/setpoint", "150");
    input.extend(b"GET /state HTTP/1.1\r\nHost: t\r\n\r\n");
    input.extend(c.put("/setpoint", "215"));
    let out = exchange(&app, &input);
    assert_eq!(out.matches("HTTP/1.1 200 OK").count(), 3, "{out}");
    assert!(out.ends_with(r#""setpoint_dc":215,"temp_dc":200,"heater":false,"relays":[false,false,false,false],"mode":"off"}"#), "{out}");
}

/// On the device's router no client can report a temperature, signed or not.
#[test]
fn no_temperature_over_http_on_the_device() {
    let app = App::new(KEY, BOOT);
    let c = Client::new();
    let out = exchange_on(&app, &c.signed("POST", "/temp", "100"), false);
    assert!(out.starts_with("HTTP/1.1 404 "), "{out}");
    // Every route of the device decodes only its own command: a body meant for
    // one route on another is a 400, never another command.
    assert!(exchange_on(&app, &c.put("/mode", "100"), false).starts_with("HTTP/1.1 400 "));
    assert!(exchange_on(&app, &c.put("/setpoint", "heat"), false).starts_with("HTTP/1.1 400 "));
    assert_eq!(app.engine.snapshot(), thermostat_kernel::INITIAL);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(200))]
    /// Any sequence of commands keeps the interlock the kernel proves.
    #[test]
    fn interlock_holds(cmds in proptest::collection::vec((0u8..4, -400i16..400, 0u8..8), 1..30)) {
        let app = App::new(KEY, BOOT);
        let c = Client::new();
        let mut input = Vec::new();
        for (k, v, i) in cmds {
            input.extend(match k {
                0 => c.put("/setpoint", &v.to_string()),
                1 => c.signed("POST", "/temp", &v.to_string()),
                2 => c.put("/mode", ["off", "heat", "auto"][(i % 3) as usize]),
                _ => c.put(&format!("/relay/{i}"), "on"),
            });
        }
        let _ = exchange(&app, &input);
        let s = app.engine.snapshot();
        proptest::prop_assert!(!s.heater || s.temp_dc < thermostat_kernel::MAX_TEMP_DC);
        proptest::prop_assert!((50..=300).contains(&s.setpoint_dc));
    }
}
