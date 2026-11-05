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

#[test]
fn bodyless_responses() {
    // HEAD: the GET handler's length, no body; the connection goes on.
    let (out, n) = run(b"HEAD /hello HTTP/1.1\r\nHost: d\r\n\r\nGET /hello HTTP/1.1\r\nHost: d\r\n\r\n");
    assert_eq!(n, 2);
    assert!(out.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nHTTP/1.1 200"), "{out}");
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
    use std::vec;
    use crate::link::{self, Bound, CLOSE, FLAG, Link, OPEN, serve_link};
    use std::time::Duration;

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
                    let _ = serve(&Config::default(), &r, &st, &mut rx, &mut tx, &mut io).await;
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
            w.write_all(&framed(2, b"POST /echo HTTP/1.1\r\nHost: d\r\nContent-Length: 4\r\n\r\nmine", true)).await.unwrap();
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
