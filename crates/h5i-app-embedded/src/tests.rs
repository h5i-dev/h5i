extern crate std;

use std::string::String;
use std::vec::Vec;

use embedded_io_adapters::tokio_1::FromTokio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

struct Counter {
    hits: core::cell::Cell<u32>,
}

async fn hello(_: &Request<'_>, _: &Counter, out: &mut [u8]) -> Response {
    Response::text(out, 200, b"hello")
}

async fn echo(req: &Request<'_>, st: &Counter, out: &mut [u8]) -> Response {
    st.hits.set(st.hits.get() + 1);
    Response::bytes(out, 200, b"application/octet-stream", req.body)
}

async fn relay(req: &Request<'_>, _: &Counter, out: &mut [u8]) -> Response {
    match req.param_u32(0) {
        Some(id) if id < 4 => {
            let n = out.len().min(1);
            out[..n].copy_from_slice(&[b'0' + id as u8][..n]);
            Response { status: 200, content_type: b"text/plain", len: n, close: false }
        }
        _ => Response::status(404),
    }
}

/// Send `input` (then half-close), and return everything the server wrote.
async fn exchange(cfg: Config, input: &[u8]) -> (String, u32) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let router = Router::new().get(b"/hello", hello).post(b"/echo", echo).post(b"/relay/{id}", relay);
    let st = Counter { hits: core::cell::Cell::new(0) };
    let (mut cr, mut cw) = tokio::io::split(client);
    let input = input.to_vec();
    let writer = async move {
        cw.write_all(&input).await.unwrap();
        cw.shutdown().await.unwrap();
        cw
    };
    let reader = async move {
        let mut v = Vec::new();
        cr.read_to_end(&mut v).await.unwrap();
        v
    };
    let srv = async {
        let mut io = FromTokio::new(server);
        let mut rx = [0u8; 2048];
        let mut tx = [0u8; 1024];
        let r = serve(&cfg, &router, &st, &mut rx, &mut tx, &mut io).await;
        drop(io);
        r
    };
    let (_w, out, r) = tokio::join!(writer, reader, srv);
    let served = r.unwrap();
    (String::from_utf8_lossy(&out).into_owned(), served)
}

fn run(input: &[u8]) -> (String, u32) {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(exchange(Config::default(), input))
}

#[test]
fn get_and_post() {
    let (out, n) = run(b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
    assert_eq!(n, 1);
    assert_eq!(out, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello");
    let (out, _) = run(b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\n\r\nabc");
    assert!(out.ends_with("Content-Length: 3\r\n\r\nabc"), "{out}");
    let (out, _) = run(b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n");
    assert!(out.ends_with("Content-Length: 5\r\n\r\nabcde"), "{out}");
    let (out, _) = run(b"POST /relay/2 HTTP/1.1\r\nHost: d\r\n\r\n");
    assert!(out.ends_with("\r\n\r\n2"), "{out}");
}

#[test]
fn routing_errors() {
    assert!(run(b"GET /nope HTTP/1.1\r\nHost: d\r\n\r\n").0.starts_with("HTTP/1.1 404 "));
    assert!(run(b"GET /echo HTTP/1.1\r\nHost: d\r\n\r\n").0.starts_with("HTTP/1.1 405 "));
    assert!(run(b"POST /relay/9 HTTP/1.1\r\nHost: d\r\n\r\n").0.starts_with("HTTP/1.1 404 "));
    assert!(run(b"POST /relay/x HTTP/1.1\r\nHost: d\r\n\r\n").0.starts_with("HTTP/1.1 404 "));
}

#[test]
fn pipelined_and_close() {
    let req = b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\nPOST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 2\r\n\r\nxyPOST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\nz\r\n0\r\n\r\nGET /hello HTTP/1.1\r\nHost: d\r\n\r\n";
    let (out, n) = run(req);
    assert_eq!(n, 3, "{out}");
    assert_eq!(out.matches("HTTP/1.1 200 OK").count(), 3);
    assert!(out.contains("\r\n\r\nxy"));
    assert!(out.ends_with("Connection: close\r\n\r\nz"), "{out}");
    // HTTP/1.0 closes after one response.
    let (out, n) = run(b"GET /hello HTTP/1.0\r\n\r\nGET /hello HTTP/1.0\r\n\r\n");
    assert_eq!(n, 1);
    assert!(out.contains("Connection: close"));
}

/// The smuggling corpus: each is answered with an error and the connection
/// closed, so the trailing request is never served.
pub const SMUGGLING: &[(&[u8], u16)] = &[
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\nabcd", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length : 3\r\n\r\nabc", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding : chunked\r\n\r\n0\r\n\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nX: a\r\n Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\nContent-Length: 3\r\n\r\nabc", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked, identity\r\n\r\n0\r\n\r\n", 501),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n3;x=1\r\nabc\r\n0\r\n\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Trailer: y\r\n\r\n", 400),
    (b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 99999\r\n\r\n", 413),
    (b"POST /echo HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
    (b"GET /hello HTTP/1.1\r\n\r\n", 400),
    (b"GET /hello HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n", 400),
];

#[test]
fn smuggling_corpus() {
    for (req, status) in SMUGGLING {
        let mut input = req.to_vec();
        input.extend_from_slice(b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
        let (out, n) = run(&input);
        assert_eq!(n, 0, "{:?}: {out}", core::str::from_utf8(req));
        let want = std::format!("HTTP/1.1 {status} ");
        assert!(out.starts_with(&want), "{:?}: {out}", core::str::from_utf8(req));
        assert!(out.contains("Connection: close"));
        assert_eq!(out.matches("HTTP/1.1").count(), 1, "{out}");
    }
}

#[test]
fn oversized_head() {
    let mut big = Vec::new();
    big.extend_from_slice(b"GET /hello HTTP/1.1\r\nHost: d\r\nX: ");
    big.resize(1500, b'a');
    big.extend_from_slice(b"\r\n\r\n");
    assert!(run(&big).0.starts_with("HTTP/1.1 431 "));
    let mut long = Vec::new();
    long.extend_from_slice(b"GET /");
    long.resize(1500, b'a');
    assert!(run(&long).0.starts_with("HTTP/1.1 414 "));
}

#[test]
fn truncated_requests_are_400() {
    assert!(run(b"GET /hello HTTP/1.1\r\nHost").0.starts_with("HTTP/1.1 400 "));
    assert!(run(b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 5\r\n\r\nab").0.starts_with("HTTP/1.1 400 "));
    assert_eq!(run(b"").1, 0);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(300))]
    /// Any bytes: the server answers or closes, and never panics.
    #[test]
    fn arbitrary_bytes(input in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..600)) {
        let _ = run(&input);
    }

    /// A valid request with mutated bytes: every response is well framed.
    #[test]
    fn mutated(pos in 0usize..60, b in proptest::prelude::any::<u8>()) {
        let mut req = b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 4\r\n\r\nabcd".to_vec();
        let p = pos % req.len();
        req[p] = b;
        let (out, _) = run(&req);
        proptest::prop_assert!(out.is_empty() || out.starts_with("HTTP/1.1 "));
    }
}
