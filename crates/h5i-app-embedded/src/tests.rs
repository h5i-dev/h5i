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
            Response::new(200, b"text/plain", n)
        }
        _ => Response::status(404),
    }
}

async fn status_with_body(req: &Request<'_>, _: &Counter, out: &mut [u8]) -> Response {
    let st = req.param_u32(0).unwrap_or(200) as u16;
    let mut r = Response::text(out, st, b"body!");
    if st == 299 {
        // A content type that would split the response.
        r.content_type = b"text/plain\r\nX: y";
    }
    r
}

/// Send `input` (then half-close), and return everything the server wrote.
async fn exchange(cfg: Config, input: &[u8]) -> (String, u32) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let router = Router::new()
        .get(b"/hello", hello)
        .post(b"/echo", echo)
        .post(b"/relay/{id}", relay)
        .get(b"/status/{s}", status_with_body);
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
        let r = serve(&cfg, &router, &st, &mut rx, &mut tx, &mut io, &NoBound).await;
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
    let (out, _) =
        run(b"POST /echo HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n");
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

#[test]
fn bodyless_responses() {
    // HEAD: the GET handler's length, no body; the connection goes on.
    let (out, n) = run(b"HEAD /hello HTTP/1.1\r\nHost: d\r\n\r\nGET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
    assert_eq!(n, 2);
    assert!(
        out.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nHTTP/1.1 200"),
        "{out}"
    );
    assert!(out.ends_with("\r\n\r\nhello"));
    // 204 and 304: neither Content-Length nor body.
    for st in [204, 304] {
        let req = std::format!("GET /status/{st} HTTP/1.1\r\nHost: d\r\n\r\n");
        let (out, _) = run(req.as_bytes());
        assert!(out.starts_with(&std::format!("HTTP/1.1 {st} ")), "{out}");
        assert!(!out.contains("Content-Length") && !out.contains("body!") && out.ends_with("\r\n\r\n"), "{out}");
    }
    // A 1xx from a handler is not a final response: 500, and the connection closes.
    let (out, n) = run(b"GET /status/101 HTTP/1.1\r\nHost: d\r\n\r\nGET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
    assert_eq!(n, 1);
    assert!(out.starts_with("HTTP/1.1 500 ") && out.contains("Connection: close") && !out.contains("body!"), "{out}");
}

#[test]
fn fallback_500_closes() {
    let (out, n) = run(b"GET /status/299 HTTP/1.1\r\nHost: d\r\n\r\nGET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
    assert_eq!(n, 1, "{out}");
    assert_eq!(out, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

#[test]
fn connection_close_tokens() {
    let next = b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n";
    for c in [&b"close"[..], b"CLOSE", b"keep-alive, Close", b"upgrade ,close , te", b"x\r\nConnection: close"] {
        let mut req = b"GET /hello HTTP/1.1\r\nHost: d\r\nConnection: ".to_vec();
        req.extend_from_slice(c);
        req.extend_from_slice(b"\r\n\r\n");
        req.extend_from_slice(next);
        let (out, n) = run(&req);
        assert_eq!(n, 1, "{:?}: {out}", core::str::from_utf8(c));
        assert!(out.contains("Connection: close"), "{out}");
    }
    for c in [&b"keep-alive"[..], b"closed", b"x-close"] {
        let mut req = b"GET /hello HTTP/1.1\r\nHost: d\r\nConnection: ".to_vec();
        req.extend_from_slice(c);
        req.extend_from_slice(b"\r\n\r\n");
        req.extend_from_slice(next);
        assert_eq!(run(&req).1, 2, "{:?}", core::str::from_utf8(c));
    }
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

// ---------------------------------------------------------------------------
// A shared byte link (a UART): without framing, one client's partial request
// is completed by the next client's bytes; with `link`, it is not.

mod uart {
    use super::*;
    use crate::link::{self, Bound, CLOSE, FLAG, Link, OPEN, serve_link};
    use std::time::Duration;
    use std::vec;

    struct TokioBound(Duration);

    impl Bound for TokioBound {
        async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
            tokio::time::timeout(self.0, f).await.ok()
        }
    }

    fn router() -> Router<impl Routes<Counter>> {
        Router::new().get(b"/hello", hello).post(b"/echo", echo).post(b"/relay/{id}", relay)
    }

    const ATTACKER: &[u8] = b"POST /relay/3 HTTP/1.1\r\nX-Pad: ";
    const VICTIM: &[u8] = b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 2\r\n\r\non";

    /// What the firmware did before the fix: `serve` again and again on the
    /// raw line. The victim's request runs as the attacker's.
    #[test]
    fn raw_uart_is_hijackable() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let out = rt.block_on(async {
            let (line, dev) = tokio::io::duplex(1 << 16);
            let (mut lr, mut lw) = tokio::io::split(line);
            let st = Counter { hits: core::cell::Cell::new(0) };
            let r = router();
            let device = async {
                let mut io = FromTokio::new(dev);
                let mut rx = [0u8; 2048];
                let mut tx = [0u8; 1024];
                for _ in 0..2 {
                    let _ = serve(&Config::default(), &r, &st, &mut rx, &mut tx, &mut io, &NoBound).await;
                }
            };
            let clients = async move {
                lw.write_all(ATTACKER).await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
                lw.write_all(VICTIM).await.unwrap();
                lw.shutdown().await.unwrap();
                let mut v = Vec::new();
                lr.read_to_end(&mut v).await.unwrap();
                v
            };
            let ((), v) = tokio::join!(device, clients);
            String::from_utf8_lossy(&v).into_owned()
        });
        // The handler for the attacker's path answered the victim.
        assert!(out.starts_with("HTTP/1.1 200 OK") && out.ends_with("\r\n\r\n3"), "{out}");
    }

    /// The gateway's side: `data` for connection `id`.
    fn framed(id: u8, data: &[u8], close: bool) -> Vec<u8> {
        let mut v = vec![FLAG, OPEN, id];
        let mut buf = vec![0u8; 2 * data.len()];
        let n = link::escape(data, &mut buf).unwrap();
        v.extend_from_slice(&buf[..n]);
        if close {
            v.extend_from_slice(&[FLAG, CLOSE, id]);
        }
        v
    }

    /// The device's output, split by connection: (id, bytes, closed).
    fn segments(out: &[u8]) -> Vec<(u8, Vec<u8>, bool)> {
        let mut segs: Vec<(u8, Vec<u8>, bool)> = Vec::new();
        let mut i = 0;
        while i < out.len() {
            match out[i] {
                FLAG if out[i + 1] == OPEN => {
                    segs.push((out[i + 2], Vec::new(), false));
                    i += 3;
                }
                FLAG if out[i + 1] == CLOSE => {
                    let s = segs.last_mut().unwrap();
                    assert_eq!(s.0, out[i + 2]);
                    s.2 = true;
                    i += 3;
                }
                link::ESC => {
                    segs.last_mut().unwrap().1.push(out[i + 1] ^ 0x20);
                    i += 2;
                }
                b => {
                    segs.last_mut().unwrap().1.push(b);
                    i += 1;
                }
            }
        }
        segs
    }

    /// Run the device loop on a line; `script` writes to it (and may wait).
    fn on_line<F, Fut>(bound: Duration, script: F) -> Vec<(u8, Vec<u8>, bool)>
    where
        F: FnOnce(tokio::io::WriteHalf<tokio::io::DuplexStream>) -> Fut,
        Fut: core::future::Future<Output = tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (line, dev) = tokio::io::duplex(1 << 16);
            let (mut lr, lw) = tokio::io::split(line);
            let st = Counter { hits: core::cell::Cell::new(0) };
            let r = router();
            let device = async {
                let mut l = Link::new(FromTokio::new(dev));
                let mut rx = [0u8; 2048];
                let mut tx = [0u8; 1024];
                serve_link(&Config::default(), &r, &st, &mut rx, &mut tx, &mut l, &TokioBound(bound)).await.unwrap();
            };
            let clients = async move {
                let mut lw = script(lw).await;
                lw.shutdown().await.unwrap();
                let mut v = Vec::new();
                lr.read_to_end(&mut v).await.unwrap();
                v
            };
            let ((), v) = tokio::join!(device, clients);
            segments(&v)
        })
    }

    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    #[test]
    fn framed_uart_isolates_connections() {
        let segs = on_line(Duration::from_secs(5), |mut w| async move {
            // The attacker leaves a partial head and goes; the victim follows at once.
            w.write_all(&framed(1, ATTACKER, true)).await.unwrap();
            w.write_all(&framed(2, VICTIM, false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            w
        });
        let victim = segs.iter().find(|s| s.0 == 2).unwrap();
        let v = text(&victim.1);
        assert!(v.starts_with("HTTP/1.1 200 OK") && v.ends_with("\r\n\r\non"), "{v}");
        // The attacker's connection got at most a rejection of its own partial head.
        for s in segs.iter().filter(|s| s.0 == 1) {
            let a = text(&s.1);
            assert!(a.is_empty() || a.starts_with("HTTP/1.1 400 "), "{a}");
        }
        // Even without its CLOSE, a new OPEN ends the attacker's connection.
        let segs = on_line(Duration::from_secs(5), |mut w| async move {
            w.write_all(&framed(1, ATTACKER, false)).await.unwrap();
            w.write_all(&framed(2, VICTIM, false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            w
        });
        let v = text(&segs.iter().find(|s| s.0 == 2).unwrap().1);
        assert!(v.ends_with("\r\n\r\non"), "{v}");
        assert!(segs.iter().filter(|s| s.0 == 1).all(|s| s.1.is_empty()), "{segs:?}");
    }

    #[test]
    fn a_silent_client_is_cut_off_within_the_bound() {
        let t0 = std::time::Instant::now();
        let segs = on_line(Duration::from_millis(300), |mut w| async move {
            w.write_all(&framed(1, b"GET /hello HTTP/1.1\r\n", false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            w.write_all(&framed(2, b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n", true)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            w
        });
        assert!(t0.elapsed() < Duration::from_secs(3));
        // Connection 1 was closed by the device, with nothing served.
        assert_eq!(segs[0].0, 1);
        assert!(segs[0].2 && segs[0].1.is_empty(), "{segs:?}");
        let v = text(&segs.iter().find(|s| s.0 == 2).unwrap().1);
        assert!(v.ends_with("\r\n\r\nhello"), "{v}");
    }

    #[test]
    fn no_stale_response_reaches_the_next_connection() {
        let segs = on_line(Duration::from_secs(5), |mut w| async move {
            // Two pipelined requests, then the next client before they are read.
            let mut a = b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 5\r\n\r\nfirst".to_vec();
            a.extend_from_slice(b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 6\r\n\r\nsecond");
            w.write_all(&framed(1, &a, false)).await.unwrap();
            w.write_all(&framed(2, b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 4\r\n\r\nmine", true))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            w
        });
        for s in &segs {
            let t = text(&s.1);
            if s.0 == 2 {
                assert_eq!(t.matches("HTTP/1.1").count(), 1, "{t}");
                assert!(t.ends_with("\r\n\r\nmine"), "{t}");
            } else {
                assert!(!t.contains("mine"), "{t}");
            }
        }
    }

    /// Payload bytes equal to the markers are escaped and arrive intact.
    #[test]
    fn marker_bytes_in_bodies_survive() {
        let body = [b'a', FLAG, OPEN, 9, link::ESC, b'z'];
        let mut req = b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 6\r\n\r\n".to_vec();
        req.extend_from_slice(&body);
        let segs = on_line(Duration::from_secs(5), |mut w| async move {
            w.write_all(&framed(7, &req, true)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            w
        });
        assert_eq!(segs.len(), 1);
        assert!(segs[0].1.ends_with(&body), "{segs:?}");
    }
}

/// One request through `router` with `cfg`; everything the server wrote.
async fn exchange_with<S, R: Routes<S>>(cfg: Config, router: &Router<R>, st: &S, input: &[u8]) -> String {
    let (client, server) = tokio::io::duplex(1 << 16);
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
        let _ = serve(&cfg, router, st, &mut rx, &mut tx, &mut io, &NoBound).await;
        drop(io);
    };
    let (_w, out, ()) = tokio::join!(writer, reader, srv);
    String::from_utf8_lossy(&out).into_owned()
}

fn block_on<F: core::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

mod fields {
    use super::*;

    async fn cors(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::text(out, 200, b"ok")
            .header(b"Access-Control-Allow-Origin", b"*")
            .header(b"Cache-Control", b"no-store")
            .header_copy(b"ETag", b"\"v42\"")
    }

    async fn reserved(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::text(out, 200, b"smuggled").header(b"Content-Length", b"0")
    }

    async fn split(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::text(out, 200, b"x").header(b"X-A", b"a\r\nSet-Cookie: s=1")
    }

    async fn too_many(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        let mut r = Response::text(out, 200, b"x");
        for _ in 0..=MAX_FIELDS {
            r = r.header(b"X-A", b"a");
        }
        r
    }

    async fn too_long(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::text(out, 200, b"x").header_copy(b"X-A", &[b'a'; INLINE_VALUE + 1])
    }

    async fn query(req: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        let mut buf = [0u8; 16];
        match req.query_param(b"q").and_then(|v| form::decode(v, &mut buf)) {
            Some(v) => Response::bytes(out, 200, b"text/plain", v),
            None => Response::status(400),
        }
    }

    async fn patched(_: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::text(out, 200, b"patched")
    }

    fn router() -> Router<impl Routes<()>> {
        Router::new()
            .get(b"/cors", cors)
            .get(b"/reserved", reserved)
            .get(b"/split", split)
            .get(b"/many", too_many)
            .get(b"/long", too_long)
            .get(b"/q", query)
            .patch(b"/p", patched)
    }

    fn get(path: &str) -> String {
        let req = std::format!("GET {path} HTTP/1.1\r\nHost: d\r\n\r\n");
        block_on(exchange_with(Config::default(), &router(), &(), req.as_bytes()))
    }

    #[test]
    fn extra_fields_are_written() {
        assert_eq!(
            get("/cors"),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nETag: \"v42\"\r\n\r\nok"
        );
    }

    #[test]
    fn refused_fields_are_a_closing_500() {
        for p in ["/reserved", "/split", "/many", "/long"] {
            let out = get(p);
            assert_eq!(
                out, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "{p}"
            );
        }
    }

    #[test]
    fn head_room_bounds_the_head() {
        // Too little room for the head and its fields: 500, and the body is not sent.
        let cfg = Config { head_room: 80, ..Config::default() };
        let out = block_on(exchange_with(cfg, &router(), &(), b"GET /cors HTTP/1.1\r\nHost: d\r\n\r\n"));
        assert!(out.starts_with("HTTP/1.1 500 "), "{out}");
        let cfg = Config { head_room: 1024, ..Config::default() };
        let out = block_on(exchange_with(cfg, &router(), &(), b"GET /cors HTTP/1.1\r\nHost: d\r\n\r\n"));
        assert_eq!(out, "", "tx no longer than head_room: nothing served");
    }

    #[test]
    fn query_and_patch() {
        assert!(get("/q?a=1&q=x%20y").ends_with("\r\n\r\nx y"));
        assert!(get("/q?a=1").starts_with("HTTP/1.1 400 "));
        let out = block_on(exchange_with(Config::default(), &router(), &(), b"PATCH /p HTTP/1.1\r\nHost: d\r\n\r\n"));
        assert!(out.ends_with("\r\n\r\npatched"), "{out}");
    }
}

mod bounds {
    use super::*;
    use std::time::Duration;

    pub struct TokioBound(pub Duration);

    impl Bound for TokioBound {
        async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
            tokio::time::timeout(self.0, f).await.ok()
        }
    }

    /// The client sends `parts` with `gap` between them and keeps its side open.
    fn trickle(parts: &[&[u8]], gap: Duration, bound: Duration) -> (String, Result<u32, Closed<std::io::Error>>) {
        block_on(async {
            let (client, server) = tokio::io::duplex(1 << 16);
            let (mut cr, mut cw) = tokio::io::split(client);
            let parts: Vec<Vec<u8>> = parts.iter().map(|p| p.to_vec()).collect();
            let writer = async move {
                for p in parts {
                    if cw.write_all(&p).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(gap).await;
                }
                cw
            };
            let reader = async move {
                let mut v = Vec::new();
                let _ = cr.read_to_end(&mut v).await;
                v
            };
            let router = Router::new().get(b"/hello", hello);
            let st = Counter { hits: core::cell::Cell::new(0) };
            let srv = async {
                let mut io = FromTokio::new(server);
                let mut rx = [0u8; 2048];
                let mut tx = [0u8; 1024];
                let r = serve(&Config::default(), &router, &st, &mut rx, &mut tx, &mut io, &TokioBound(bound)).await;
                drop(io);
                r
            };
            let (_w, out, r) = tokio::join!(writer, reader, srv);
            (
                String::from_utf8_lossy(&out).into_owned(),
                r.map_err(|e| match e {
                    Closed::Io(e) => Closed::Io(std::io::Error::other(std::format!("{e:?}"))),
                    Closed::Done { requests } => Closed::Done { requests },
                    Closed::TxTooSmall => Closed::TxTooSmall,
                    Closed::WriteTimeout => Closed::WriteTimeout,
                }),
            )
        })
    }

    #[test]
    fn a_slow_request_gets_408() {
        // Each byte within any per-read timeout, the whole head not within the bound.
        let head = b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n";
        let parts: Vec<&[u8]> = head.chunks(1).collect();
        let (out, r) = trickle(&parts, Duration::from_millis(20), Duration::from_millis(150));
        assert_eq!(out, "HTTP/1.1 408 Request Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        assert_eq!(r.unwrap(), 0);
    }

    #[test]
    fn an_idle_connection_closes_quietly() {
        let (out, r) =
            trickle(&[b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n"], Duration::from_secs(5), Duration::from_millis(100));
        assert!(out.starts_with("HTTP/1.1 200 OK") && out.ends_with("hello"), "{out}");
        assert_eq!(r.unwrap(), 1);
    }

    #[test]
    fn a_reader_that_never_reads_is_cut() {
        block_on(async {
            // A 64-byte pipe the client never drains.
            let (_client, server) = tokio::io::duplex(64);
            let router = Router::new().get(b"/hello", hello);
            let st = Counter { hits: core::cell::Cell::new(0) };
            let mut io = FromTokio::new(server);
            let mut rx = [0u8; 2048];
            let mut tx = [0u8; 1024];
            let mut input = Vec::new();
            for _ in 0..8 {
                input.extend_from_slice(b"GET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
            }
            // Feed requests through a second pipe; responses go to the undrained one.
            struct Split<R, W>(R, W);
            impl<R: embedded_io_async::ErrorType<Error = W::Error>, W: embedded_io_async::ErrorType>
                embedded_io_async::ErrorType for Split<R, W>
            {
                type Error = W::Error;
            }
            impl<
                R: embedded_io_async::Read + embedded_io_async::ErrorType<Error = W::Error>,
                W: embedded_io_async::ErrorType,
            > embedded_io_async::Read for Split<R, W>
            {
                async fn read(&mut self, b: &mut [u8]) -> Result<usize, Self::Error> {
                    self.0.read(b).await
                }
            }
            impl<R: embedded_io_async::ErrorType<Error = W::Error>, W: embedded_io_async::Write>
                embedded_io_async::Write for Split<R, W>
            {
                async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
                    self.1.write(b).await
                }
                async fn flush(&mut self) -> Result<(), Self::Error> {
                    self.1.flush().await
                }
            }
            let (mut feed, inp) = tokio::io::duplex(1 << 16);
            feed.write_all(&input).await.unwrap();
            let mut both = Split(FromTokio::new(inp), &mut io);
            let r = serve(
                &Config::default(),
                &router,
                &st,
                &mut rx,
                &mut tx,
                &mut both,
                &TokioBound(Duration::from_millis(100)),
            )
            .await;
            assert!(matches!(r, Err(Closed::WriteTimeout)), "{r:?}");
        });
    }
}

mod kernel_binding {
    use super::*;
    use crate::kernel::*;
    use core::cell::Cell;
    use h5i_app_core::{Kernel, TenantId, Timestamp};

    /// A counter: `Add` needs an admin, `Read` writes nothing, `Now` replies
    /// with the stamped time.
    struct Ctr;

    #[derive(Clone, Debug, PartialEq)]
    struct Who {
        admin: bool,
        now: u64,
    }

    #[derive(Debug, PartialEq)]
    enum Cmd {
        Add(u32),
        Read,
        Now,
    }

    impl Kernel for Ctr {
        type Principal = Who;
        type Snapshot = u32;
        type Command = Cmd;
        type WriteSet = u32;
        type Reply = u64;
        type Error = u16;
        fn tenant(_: &Who) -> TenantId {
            TenantId(0)
        }
        fn transition(a: &Who, s: &u32, c: &Cmd) -> Result<(u32, u64), u16> {
            match c {
                Cmd::Add(n) if a.admin => Ok((s + n, (s + n) as u64)),
                Cmd::Add(_) => Err(403),
                Cmd::Read => Ok((*s, *s as u64)),
                Cmd::Now => Ok((*s, a.now)),
            }
        }
        fn apply(_: &u32, w: &u32) -> u32 {
            *w
        }
        fn stamp(a: &mut Who, now: Timestamp) {
            a.now = now.secs();
        }
        fn read_only(c: &Cmd) -> bool {
            *c == Cmd::Read
        }
    }

    /// Saves into a cell; fails while `broken`.
    struct Mem<'a> {
        saved: &'a Cell<Option<u32>>,
        broken: &'a Cell<bool>,
    }

    impl Store<Ctr> for Mem<'_> {
        type Error = ();
        async fn load(&mut self) -> Result<Option<u32>, ()> {
            Ok(self.saved.get())
        }
        async fn save(&mut self, s: &u32) -> Result<(), ()> {
            if self.broken.get() {
                return Err(());
            }
            self.saved.set(Some(*s));
            Ok(())
        }
    }

    struct Codec;

    impl crate::kernel::Codec<Ctr> for Codec {
        fn encode_reply(r: &u64, out: &mut [u8]) -> Option<usize> {
            let mut w = crate::json::Writer::new(out);
            w.uint(*r);
            w.finish()
        }
        fn encode_error(e: &u16, _: &mut [u8]) -> (u16, usize) {
            (*e, 0)
        }
    }

    struct App<'a> {
        engine: DeviceEngine<Ctr, Mem<'a>>,
        admin: PskAuth<Who>,
        anyone: Anyone<Who>,
        clock: Cell<u64>,
    }

    impl<'a> Device<Ctr> for App<'a> {
        type Store = Mem<'a>;
        fn engine(&self) -> &DeviceEngine<Ctr, Mem<'a>> {
            &self.engine
        }
        fn now(&self) -> Option<Timestamp> {
            Some(Timestamp::from_secs(self.clock.get()))
        }
    }

    fn add(req: &Request<'_>) -> Result<Cmd, u16> {
        req.param_u32(0).map(Cmd::Add).ok_or(404)
    }

    fn router<'a>() -> Router<impl Routes<App<'a>>> {
        let k = Calls::<Ctr, Codec, App<'a>>::new();
        Router::new()
            .get(b"/n", k.call(|a: &App<'a>| &a.anyone, |_| Ok(Cmd::Read)))
            .get(b"/now", k.call(|a: &App<'a>| &a.anyone, |_| Ok(Cmd::Now)))
            .post(b"/add/{n}", k.call(|a: &App<'a>| &a.admin, add))
            // Same decoder, no key: the kernel refuses it for a non-admin.
            .post(b"/guest-add/{n}", k.call(|a: &App<'a>| &a.anyone, add))
    }

    fn body(out: &str) -> &str {
        out.split("\r\n\r\n").nth(1).unwrap_or("")
    }

    #[test]
    fn routes_commit_and_refuse() {
        let saved = Cell::new(None);
        let broken = Cell::new(false);
        let app = App {
            engine: DeviceEngine::with_store(0, Mem { saved: &saved, broken: &broken }),
            admin: PskAuth { key: b"k", principal: Who { admin: true, now: 0 } },
            anyone: Anyone(Who { admin: false, now: 0 }),
            clock: Cell::new(100),
        };
        let r = router();
        let go = |req: &str| block_on(exchange_with(Config::default(), &r, &app, req.as_bytes()));

        let out = go("POST /add/5 HTTP/1.1\r\nHost: d\r\nAuthorization: Bearer k\r\n\r\n");
        assert_eq!(body(&out), "5", "{out}");
        assert_eq!(saved.get(), Some(5));
        let out = go("POST /add/5 HTTP/1.1\r\nHost: d\r\n\r\n");
        assert!(out.starts_with("HTTP/1.1 401 ") && out.contains("WWW-Authenticate: Bearer\r\n"), "{out}");
        assert!(go("POST /guest-add/5 HTTP/1.1\r\nHost: d\r\n\r\n").starts_with("HTTP/1.1 403 "));
        assert_eq!(body(&go("GET /n HTTP/1.1\r\nHost: d\r\n\r\n")), "5");

        // The store fails: 503, and nothing changed.
        broken.set(true);
        assert!(go("POST /add/1 HTTP/1.1\r\nHost: d\r\nAuthorization: Bearer k\r\n\r\n").starts_with("HTTP/1.1 503 "));
        assert_eq!(body(&go("GET /n HTTP/1.1\r\nHost: d\r\n\r\n")), "5");
        assert_eq!(saved.get(), Some(5));
        // Reads do not touch the store.
        assert!(go("GET /n HTTP/1.1\r\nHost: d\r\n\r\n").starts_with("HTTP/1.1 200 "));
        broken.set(false);

        // Stamped with the device clock, never earlier than the last commit.
        assert_eq!(body(&go("GET /now HTTP/1.1\r\nHost: d\r\n\r\n")), "100");
        app.clock.set(50);
        assert_eq!(body(&go("GET /now HTTP/1.1\r\nHost: d\r\n\r\n")), "100");
        app.clock.set(200);
        assert_eq!(body(&go("GET /now HTTP/1.1\r\nHost: d\r\n\r\n")), "200");
    }

    #[cfg(feature = "hmac")]
    #[test]
    fn signed_requests() {
        use crate::hmac::{HmacAuth, authorization};
        struct Signed {
            engine: DeviceEngine<Ctr>,
            auth: HmacAuth<Who>,
        }
        impl Device<Ctr> for Signed {
            type Store = NoStore;
            fn engine(&self) -> &DeviceEngine<Ctr> {
                &self.engine
            }
        }
        let app = Signed {
            engine: DeviceEngine::new(0),
            auth: HmacAuth::new(b"secret", Who { admin: true, now: 0 }, 0xabcd),
        };
        let k = Calls::<Ctr, Codec, Signed>::new();
        let r = Router::new().post(b"/add/{n}", k.call(|a: &Signed| &a.auth, add));
        let go = |req: std::string::String| block_on(exchange_with(Config::default(), &r, &app, req.as_bytes()));
        let signed = |key: &[u8], boot: u32, ctr: u64, target: &str, body: &str, sent_target: &str, sent_body: &str| {
            let mut a = [0u8; 128];
            let n = authorization(key, boot, ctr, b"POST", target.as_bytes(), body.as_bytes(), &mut a);
            let a = std::string::String::from_utf8(a[..n].to_vec()).unwrap();
            std::format!(
                "POST {sent_target} HTTP/1.1\r\nHost: d\r\nAuthorization: {a}\r\nContent-Length: {}\r\n\r\n{sent_body}",
                sent_body.len()
            )
        };
        let ok = signed(b"secret", 0xabcd, 10, "/add/2", "x", "/add/2", "x");
        assert_eq!(body(&go(ok.clone())), "2");
        // The same request again: a replay.
        let out = go(ok);
        assert!(
            out.starts_with("HTTP/1.1 401 ") && out.contains("WWW-Authenticate: H5i-HMAC boot=0000abcd,last=10\r\n"),
            "{out}"
        );
        // A lower counter, another boot, another key, a changed target or body.
        for req in [
            signed(b"secret", 0xabcd, 9, "/add/2", "x", "/add/2", "x"),
            signed(b"secret", 0xabce, 11, "/add/2", "x", "/add/2", "x"),
            signed(b"other", 0xabcd, 11, "/add/2", "x", "/add/2", "x"),
            signed(b"secret", 0xabcd, 11, "/add/2", "x", "/add/9", "x"),
            signed(b"secret", 0xabcd, 11, "/add/2", "x", "/add/2", "y"),
        ] {
            assert!(go(req).starts_with("HTTP/1.1 401 "));
        }
        assert_eq!(body(&go(signed(b"secret", 0xabcd, 11, "/add/3", "", "/add/3", ""))), "5");
        assert!(
            go("POST /add/1 HTTP/1.1\r\nHost: d\r\nAuthorization: Bearer secret\r\n\r\n".into())
                .starts_with("HTTP/1.1 401 ")
        );
    }

    /// Pipelined signed requests, arriving a few bytes per read.
    #[cfg(feature = "hmac")]
    #[test]
    fn signed_requests_in_pieces() {
        use crate::hmac::{HmacAuth, authorization};
        struct Signed {
            engine: DeviceEngine<Ctr>,
            auth: HmacAuth<Who>,
        }
        impl Device<Ctr> for Signed {
            type Store = NoStore;
            fn engine(&self) -> &DeviceEngine<Ctr> {
                &self.engine
            }
        }
        struct Pieces<'a> {
            input: &'a [u8],
            at: usize,
            step: usize,
            out: Vec<u8>,
        }
        impl embedded_io_async::ErrorType for Pieces<'_> {
            type Error = core::convert::Infallible;
        }
        impl embedded_io_async::Read for Pieces<'_> {
            async fn read(&mut self, b: &mut [u8]) -> Result<usize, Self::Error> {
                let n = self.step.min(b.len()).min(self.input.len() - self.at);
                b[..n].copy_from_slice(&self.input[self.at..self.at + n]);
                self.at += n;
                Ok(n)
            }
        }
        impl embedded_io_async::Write for Pieces<'_> {
            async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
                self.out.extend_from_slice(b);
                Ok(b.len())
            }
            async fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        let signed = |ctr: u64, path: &str, body: &str| {
            let mut a = [0u8; 128];
            let n = authorization(b"secret", 7, ctr, b"POST", path.as_bytes(), body.as_bytes(), &mut a);
            let a = std::string::String::from_utf8(a[..n].to_vec()).unwrap();
            std::format!("POST {path} HTTP/1.1\r\nHost: d\r\nAuthorization: {a}\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        };
        let input = signed(1, "/add/1", "x") + "GET /n HTTP/1.1\r\nHost: d\r\n\r\n" + &signed(2, "/add/2", "yz");
        for step in 1..80 {
            let app = Signed { engine: DeviceEngine::new(0), auth: HmacAuth::new(b"secret", Who { admin: true, now: 0 }, 7) };
            let k = Calls::<Ctr, Codec, Signed>::new();
            let r = Router::new().post(b"/add/{n}", k.call(|a: &Signed| &a.auth, add)).get(b"/n", k.call(|a: &Signed| &a.auth, |_| Ok(Cmd::Read)));
            let mut io = Pieces { input: input.as_bytes(), at: 0, step, out: Vec::new() };
            let mut rx = [0u8; 2048];
            let mut tx = [0u8; 1024];
            let _ = block_on(serve(&Config::default(), &r, &app, &mut rx, &mut tx, &mut io, &NoBound));
            let out = std::string::String::from_utf8_lossy(&io.out).into_owned();
            assert_eq!(out.matches(" 200 OK").count(), 2, "step {step}: {out}");
            assert!(out.ends_with("\r\n\r\n3"), "step {step}: {out}");
        }
    }

    #[test]
    fn restore_and_watch() {
        let saved = Cell::new(Some(41));
        let broken = Cell::new(false);
        let engine = DeviceEngine::with_store(0, Mem { saved: &saved, broken: &broken });
        let admin = Who { admin: true, now: 0 };
        block_on(async {
            assert_eq!(engine.restore().await, Ok(true));
            assert_eq!(engine.snapshot(), 41);
            let mut seen = engine.version();
            let watcher = async {
                let mut got = std::vec::Vec::new();
                while got.last() != Some(&44) {
                    got.push(engine.changed(&mut seen).await);
                }
                got
            };
            let writer = async {
                for _ in 0..3 {
                    tokio::task::yield_now().await;
                    engine.execute(&admin, &Cmd::Add(1), None).await.unwrap();
                }
                // A refusal and a read commit nothing.
                assert_eq!(
                    engine.execute(&Who { admin: false, now: 0 }, &Cmd::Add(1), None).await,
                    Err(Refused::Kernel(403))
                );
                engine.execute(&admin, &Cmd::Read, None).await.unwrap();
            };
            let (got, ()) = tokio::join!(watcher, writer);
            assert_eq!(got.last(), Some(&44));
            assert!(got.windows(2).all(|w| w[0] < w[1]), "{got:?}");
        });
    }
}
