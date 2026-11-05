//! The thermostat API over the real shell, through an in-memory socket.

use embedded_io_adapters::tokio_1::FromTokio;
use h5i_app_embedded::{Config, serve};
use thermostat_app::{App, router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static KEY: &[u8] = b"k";

/// One connection: send `input`, half-close, collect the answer.
fn exchange(app: &App, input: &[u8]) -> String {
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
            let routes = router();
            let _ = serve(&Config::default(), &routes, app, &mut rx, &mut tx, &mut io).await;
        };
        let ((), out, ()) = tokio::join!(w, r, s);
        String::from_utf8_lossy(&out).into_owned()
    })
}

fn put(path: &str, body: &str) -> Vec<u8> {
    format!("PUT {path} HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer k\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        .into_bytes()
}

#[test]
fn state_and_writes() {
    let app = App::new(KEY);
    let out = exchange(&app, b"GET /state HTTP/1.1\r\nHost: t\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"), "{out}");
    assert!(out.ends_with(r#"{"setpoint_dc":200,"temp_dc":200,"heater":false,"relays":[false,false,false,false],"mode":"off"}"#));
    let out = exchange(&app, &put("/mode", "heat"));
    assert!(out.contains(r#""heater":true"#), "{out}");
    let out = exchange(&app, &put("/relay/2", "on"));
    assert!(out.contains(r#""relays":[false,false,true,false]"#), "{out}");
    let out = exchange(&app, b"POST /temp HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer k\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n360\r\n0\r\n\r\n");
    assert!(out.contains(r#""temp_dc":360,"heater":false"#), "{out}");
}

#[test]
fn refusals() {
    let app = App::new(KEY);
    assert!(exchange(&app, &put("/relay/4", "on")).starts_with("HTTP/1.1 404 "));
    assert!(exchange(&app, &put("/relay/255", "on")).contains("no_such_relay"));
    assert!(exchange(&app, &put("/relay/256", "on")).starts_with("HTTP/1.1 404 "));
    assert!(exchange(&app, &put("/setpoint", "301")).starts_with("HTTP/1.1 422 "));
    assert!(exchange(&app, &put("/setpoint", "x")).starts_with("HTTP/1.1 400 "));
    let anon = b"PUT /mode HTTP/1.1\r\nHost: t\r\nContent-Length: 4\r\n\r\nheat";
    assert!(exchange(&app, anon).starts_with("HTTP/1.1 401 "));
    let wrong = b"PUT /mode HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer x\r\nContent-Length: 4\r\n\r\nheat";
    assert!(exchange(&app, wrong).starts_with("HTTP/1.1 401 "));
    // Nothing above changed the state.
    assert!(exchange(&app, b"GET /state HTTP/1.1\r\nHost: t\r\n\r\n").contains(r#""mode":"off""#));
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(200))]
    /// Any sequence of commands keeps the interlock the kernel proves.
    #[test]
    fn interlock_holds(cmds in proptest::collection::vec((0u8..4, -400i16..400, 0u8..8), 1..30)) {
        let app = App::new(KEY);
        let mut input = Vec::new();
        for (k, v, i) in cmds {
            input.extend(match k {
                0 => put("/setpoint", &v.to_string()),
                1 => format!("POST /temp HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n{v}", v.to_string().len()).into_bytes(),
                2 => put("/mode", ["off", "heat", "auto"][(i % 3) as usize]),
                _ => put(&format!("/relay/{i}"), "on"),
            });
        }
        let _ = exchange(&app, &input);
        let s = app.engine.snapshot();
        proptest::prop_assert!(!s.heater || s.temp_dc < thermostat_kernel::MAX_TEMP_DC);
        proptest::prop_assert!((50..=300).contains(&s.setpoint_dc));
    }
}
