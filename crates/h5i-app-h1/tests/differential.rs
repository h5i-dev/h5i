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

const SUB_DELIMS: &[u8] = b"!$&'()*+,;=";

fn unreserved_or_sub(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"-._~".contains(&c) || SUB_DELIMS.contains(&c)
}

/// `s` is a run of `ok` bytes and `%` HEXDIG HEXDIG.
fn pct_run(s: &[u8], ok: impl Fn(u8) -> bool) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            if i + 2 >= s.len() || !s[i + 1].is_ascii_hexdigit() || !s[i + 2].is_ascii_hexdigit() {
                return false;
            }
            i += 3;
        } else if ok(s[i]) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

/// `absolute-path [ "?" query ]` (RFC 9112 §3.2.1, RFC 3986 §3.3-3.4).
fn origin_form(t: &[u8]) -> bool {
    t.first() == Some(&b'/') && pct_run(t, |c| unreserved_or_sub(c) || b":@/?".contains(&c))
}

/// `uri-host [ ":" port ]` (RFC 9110 §7.2), the empty value included.
fn host_value(v: &[u8]) -> bool {
    let rest = if v.first() == Some(&b'[') {
        match v.iter().position(|&c| c == b']') {
            Some(k) if k > 1 && v[1..k].iter().all(|&c| unreserved_or_sub(c) || c == b':') => &v[k + 1..],
            _ => return false,
        }
    } else {
        let k = v.iter().position(|&c| c == b':').unwrap_or(v.len());
        if !pct_run(&v[..k], unreserved_or_sub) {
            return false;
        }
        &v[k..]
    };
    rest.is_empty() || (rest[0] == b':' && rest[1..].iter().all(u8::is_ascii_digit))
}

/// The properties `accept_wellformed` proves, checked on one accepted input.
fn check_wellformed(buf: &[u8], h: &h5i_app_h1::Head) {
    assert!(h.len <= LIM.max_head && h.len <= buf.len());
    assert_eq!(&buf[h.len - 4..h.len], b"\r\n\r\n");
    assert!(h.count <= MAX_HEADERS);
    assert!(origin_form(&buf[h.target.start..h.target.end]));
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
            b"host" => {
                host += 1;
                assert!(host_value(value));
            }
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
            "host" => {
                host += 1;
                if !host_value(v.trim_ascii()) {
                    return true;
                }
            }
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
                // Framing values may not end in whitespace.
                if v.trim_ascii_end() != v || !v.trim_ascii().eq_ignore_ascii_case(b"chunked") {
                    return true;
                }
            }
            _ => {}
        }
    }
    !origin_form(path.as_bytes())
        || (minor == 1 && host != 1)
        || host > 1
        || cl > 1
        || te > 1
        || (cl > 0 && te > 0)
        || (minor == 0 && te > 0)
        || req.headers.len() > MAX_HEADERS
        || framing_value_ends_in_ows(buf)
        // Bare LF line ends, which httparse allows.
        || buf.windows(2).any(|w| w[1] == b'\n' && w[0] != b'\r')
        || buf.starts_with(b"\n")
}

/// A `Content-Length` or `Transfer-Encoding` line whose raw value ends in
/// SP or HTAB (httparse trims it; we refuse it).
fn framing_value_ends_in_ows(buf: &[u8]) -> bool {
    buf.split(|&c| c == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&c| c == b':') else { return false };
        let name = line[..colon].to_ascii_lowercase();
        (name == b"content-length" || name == b"transfer-encoding")
            && line.last().is_some_and(|&c| c == b' ' || c == b'\t')
    })
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
        (Just("Host".to_string()), prop_oneof!["[a-z.]{1,10}", "[a-z0-9.%-]{0,10}(:[0-9]{0,5})?", "\\[[0-9a-f:.]{0,8}\\](:[0-9]{1,4})?", "[!-~]{0,8}"]),
        (Just("Content-Length".to_string()), "[0-9]{1,5}"),
        (Just("Transfer-Encoding".to_string()), prop_oneof![Just("chunked".to_string()), Just("gzip".to_string()), Just("chunked ".to_string())]),
        (Just("Content-Length".to_string()), "[0-9]{1,3}[ \t]"),
    ]
}

fn request() -> impl Strategy<Value = Vec<u8>> {
    (
        prop_oneof![Just("GET"), Just("POST"), Just("PUT"), Just("X-1")],
        prop_oneof![
            "/([A-Za-z0-9._~!$&'()*+,;=:@/?-]|%[0-9A-Fa-f]{2}){0,20}",
            "/[!-~]{0,30}",
        ],
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
