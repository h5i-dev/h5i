//! `parse_head` against `httparse` on generated and mutated requests.
//!
//! Wherever both accept, the method, target, version and every header agree.
//! Where only one accepts, the difference must be one we chose
//! (`stricter_by_design`): `httparse` is a lenient tokenizer that leaves
//! framing to its caller, while `parse_head` also decides framing and
//! rejects what RFC 9112 lets a server reject.
//!
//! The runtime half of `accept_wellformed` (proven in `H1Props.lean`) is
//! asserted for every accepted input.

use h5i_app_h1::{Framing, HeadResult, Limits, MAX_HEADERS, parse_head};
use proptest::prelude::*;

const LIM: Limits = Limits { max_head: 1024, max_body: 4096 };

fn lower(s: &[u8]) -> Vec<u8> {
    s.to_ascii_lowercase()
}

/// The properties `accept_wellformed` proves, checked on one accepted input.
fn check_wellformed(buf: &[u8], h: &h5i_app_h1::Head) {
    assert!(h.len <= LIM.max_head && h.len <= buf.len());
    assert_eq!(&buf[h.len - 4..h.len], b"\r\n\r\n");
    assert!(h.count <= MAX_HEADERS);
    let mut host = 0;
    let mut cl = 0;
    let mut te = 0;
    for f in &h.headers[..h.count] {
        let name = &buf[f.name.start..f.name.end];
        let value = &buf[f.value.start..f.value.end];
        assert!(!name.is_empty());
        assert!(name.iter().all(|&c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)));
        assert!(value.iter().all(|&c| c == b'\t' || (c >= 0x20 && c != 0x7f)));
        match lower(name).as_slice() {
            b"host" => host += 1,
            b"content-length" => cl += 1,
            b"transfer-encoding" => {
                te += 1;
                assert!(value.eq_ignore_ascii_case(b"chunked"));
            }
            _ => {}
        }
    }
    assert!(host <= 1 && cl <= 1 && te <= 1 && !(cl == 1 && te == 1));
    if h.minor == 1 {
        assert_eq!(host, 1);
    }
    match h.framing {
        Framing::Chunked => assert_eq!(te, 1),
        Framing::Length(n) => {
            assert_eq!(cl, 1);
            assert!(n <= LIM.max_body as u64);
        }
        Framing::Empty => assert_eq!((cl, te), (0, 0)),
    }
}

/// Why we may reject what httparse accepts.
fn stricter_by_design(buf: &[u8], req: &httparse::Request) -> bool {
    let path = req.path.unwrap_or("");
    let minor = req.version.unwrap_or(1);
    let mut host = 0;
    let mut cl = 0;
    let mut te = 0;
    for h in req.headers.iter() {
        let n = h.name.to_ascii_lowercase();
        let v = h.value;
        if v.iter().any(|&c| c < 0x20 && c != b'\t' || c == 0x7f) {
            return true;
        }
        match n.as_str() {
            "host" => host += 1,
            "content-length" => {
                cl += 1;
                if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
                    return true;
                }
                let s = std::str::from_utf8(v).unwrap();
                if s.parse::<u64>().map_or(true, |x| x > LIM.max_body as u64) {
                    return true;
                }
            }
            "transfer-encoding" => {
                te += 1;
                if !v.trim_ascii().eq_ignore_ascii_case(b"chunked") {
                    return true;
                }
            }
            _ => {}
        }
    }
    !path.starts_with('/')
        || path.bytes().any(|c| !(33..=126).contains(&c))
        || (minor == 1 && host != 1)
        || host > 1
        || cl > 1
        || te > 1
        || (cl > 0 && te > 0)
        || (minor == 0 && te > 0)
        || req.headers.len() > MAX_HEADERS
        // Bare LF line ends, which httparse allows.
        || buf.windows(2).any(|w| w[1] == b'\n' && w[0] != b'\r')
        || buf.starts_with(b"\n")
}

fn compare(buf: &[u8]) {
    let ours = parse_head(buf, &LIM);
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut hs);
    let theirs = req.parse(buf);
    match (ours, theirs) {
        (HeadResult::Done(h), Ok(httparse::Status::Complete(n))) => {
            check_wellformed(buf, &h);
            assert_eq!(h.len, n, "{:?}", String::from_utf8_lossy(buf));
            assert_eq!(&buf[h.method.start..h.method.end], req.method.unwrap().as_bytes());
            assert_eq!(&buf[h.target.start..h.target.end], req.path.unwrap().as_bytes());
            assert_eq!(h.minor, req.version.unwrap());
            assert_eq!(h.count, req.headers.len());
            for (f, g) in h.headers[..h.count].iter().zip(req.headers.iter()) {
                assert_eq!(&buf[f.name.start..f.name.end], g.name.as_bytes());
                assert_eq!(&buf[f.value.start..f.value.end], g.value.trim_ascii_end());
            }
        }
        (HeadResult::Done(h), other) => {
            // We accept, httparse does not: only for a version httparse
            // does not parse, or a head it reads as partial.
            check_wellformed(buf, &h);
            panic!("we accept, httparse says {other:?}: {:?}", String::from_utf8_lossy(buf));
        }
        (HeadResult::Reject(_), Ok(httparse::Status::Complete(_))) => {
            assert!(stricter_by_design(buf, &req), "we reject, httparse accepts: {:?}", String::from_utf8_lossy(buf));
        }
        _ => {}
    }
}

fn token() -> impl Strategy<Value = String> {
    "[A-Za-z0-9!#$%&'*+.^_`|~-]{1,12}"
}

fn header() -> impl Strategy<Value = (String, String)> {
    prop_oneof![
        (token(), "[ -~]{0,20}"),
        (Just("Host".to_string()), "[a-z.]{1,10}"),
        (Just("Content-Length".to_string()), "[0-9]{1,5}"),
        (Just("Transfer-Encoding".to_string()), prop_oneof![Just("chunked".to_string()), Just("gzip".to_string())]),
    ]
}

fn request() -> impl Strategy<Value = Vec<u8>> {
    (
        prop_oneof![Just("GET"), Just("POST"), Just("PUT"), Just("X-1")],
        "/[!-~]{0,30}",
        prop_oneof![Just("HTTP/1.1"), Just("HTTP/1.0")],
        proptest::collection::vec(header(), 0..20),
    )
        .prop_map(|(m, t, v, hs)| {
            let mut s = format!("{m} {t} {v}\r\n");
            for (n, val) in hs {
                s.push_str(&format!("{n}: {val}\r\n"));
            }
            s.push_str("\r\n");
            s.into_bytes()
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn generated(req in request()) {
        compare(&req);
    }

    #[test]
    fn mutated(req in request(), edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..4)) {
        let mut b = req;
        for (p, c) in edits {
            let i = p % b.len();
            b[i] = c;
        }
        compare(&b);
    }

    #[test]
    fn random_bytes(b in proptest::collection::vec(any::<u8>(), 0..300)) {
        compare(&b);
    }
}
