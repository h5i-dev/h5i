use super::*;

const LIM: Limits = Limits { max_head: 1024, max_body: 1024 };

fn head(b: &[u8]) -> Head {
    match parse_head(b, &LIM) {
        HeadResult::Done(h) => h,
        r => panic!("{:?}: {r:?}", core::str::from_utf8(b)),
    }
}

fn reject(b: &[u8]) -> u16 {
    match parse_head(b, &LIM) {
        HeadResult::Reject(s) => s,
        r => panic!("{:?}: {r:?}", core::str::from_utf8(b)),
    }
}

fn span(b: &[u8], s: Span) -> &[u8] {
    &b[s.start..s.end]
}

#[test]
fn get() {
    let b = b"GET /relay/2?x=1 HTTP/1.1\r\nHost: dev\r\nX-A:  a b \t\r\n\r\nrest";
    let h = head(b);
    assert_eq!(span(b, h.method), b"GET");
    assert_eq!(span(b, h.target), b"/relay/2?x=1");
    assert_eq!(h.minor, 1);
    assert_eq!(h.count, 2);
    assert_eq!(span(b, h.headers[0].name), b"Host");
    assert_eq!(span(b, h.headers[0].value), b"dev");
    assert_eq!(span(b, h.headers[1].name), b"X-A");
    assert_eq!(span(b, h.headers[1].value), b"a b");
    assert_eq!(h.len, b.len() - 4);
    assert_eq!(h.framing, Framing::Empty);
}

#[test]
fn framing() {
    assert_eq!(head(b"POST / HTTP/1.1\r\nhost: x\r\ncontent-LENGTH: 12\r\n\r\n").framing, Framing::Length(12));
    assert_eq!(head(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0007\r\n\r\n").framing, Framing::Length(7));
    assert_eq!(head(b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: ChUnKeD \r\n\r\n").framing, Framing::Chunked);
    // HTTP/1.0 needs no Host.
    assert_eq!(head(b"GET / HTTP/1.0\r\n\r\n").minor, 0);
    assert_eq!(head(b"GET / HTTP/1.0\r\nContent-Length: 1024\r\n\r\n").framing, Framing::Length(1024));
}

#[test]
fn incomplete() {
    let full = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n";
    for n in 0..full.len() {
        assert_eq!(parse_head(&full[..n], &LIM), HeadResult::Incomplete, "{n}");
    }
    assert_eq!(head(full).len, full.len());
}

#[test]
fn rejects() {
    let cases: &[(&[u8], u16)] = &[
        // Smuggling: both framings, duplicates, whitespace before the colon, obs-fold.
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length : 3\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding\t: chunked\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nX: a\r\n b\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nX: a\r\n\tb\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3, 3\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: +3\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: \r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1025\r\n\r\n", 413),
        (b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999999999999999999\r\n\r\n", 413),
        (b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip, chunked\r\n\r\n", 501),
        (b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked, gzip\r\n\r\n", 501),
        (b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n", 400),
        // Line ends.
        (b"GET / HTTP/1.1\nHost: x\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\r\n\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\rY: z\r\n\r\n", 400),
        // Host.
        (b"GET / HTTP/1.1\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: a\r\nhost: b\r\n\r\n", 400),
        // Request line.
        (b"GET http://x/ HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"OPTIONS * HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"CONNECT x:443 HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"GET  / HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"GET / HTTP/1.1 \r\nHost: x\r\n\r\n", 400),
        (b"GET /\x7f HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"G(T / HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        (b"GET / HTTP/2.0\r\nHost: x\r\n\r\n", 505),
        (b"GET / HTTP/1.2\r\nHost: x\r\n\r\n", 505),
        (b"GET / http/1.1\r\nHost: x\r\n\r\n", 400),
        (b"\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n", 400),
        // Fields.
        (b"GET / HTTP/1.1\r\nHost: x\r\nX: a\x00b\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\r\nX: a\x7fb\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\r\n: b\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\r\nX b\r\n\r\n", 400),
        (b"GET / HTTP/1.1\r\nHost: x\r\nX(: b\r\n\r\n", 400),
    ];
    for (b, s) in cases {
        assert_eq!(reject(b), *s, "{:?}", core::str::from_utf8(b));
    }
}

#[test]
fn limits() {
    let lim = Limits { max_head: 64, max_body: 10 };
    // A head of exactly max_head bytes is accepted; one more byte is 431.
    let mut b = [0u8; 80];
    let pre = b"GET / HTTP/1.1\r\nHost: x\r\nX: ";
    b[..pre.len()].copy_from_slice(pre);
    for k in pre.len()..60 {
        b[k] = b'a';
    }
    b[60..64].copy_from_slice(b"\r\n\r\n");
    assert!(matches!(parse_head(&b[..64], &lim), HeadResult::Done(h) if h.len == 64));
    assert!(matches!(parse_head(&b[..80], &lim), HeadResult::Done(h) if h.len == 64));
    let mut c = [b'a'; 80];
    c[..60].copy_from_slice(&b[..60]);
    c[61..65].copy_from_slice(b"\r\n\r\n");
    assert_eq!(parse_head(&c[..65], &lim), HeadResult::Reject(431));
    assert_eq!(parse_head(&c[..63], &lim), HeadResult::Incomplete);
    assert_eq!(parse_head(&c[..64], &lim), HeadResult::Reject(431));
    // An unfinished request line at the limit is 414.
    let mut d = [b'/'; 80];
    d[..4].copy_from_slice(b"GET ");
    assert_eq!(parse_head(&d, &lim), HeadResult::Reject(414));
    // MAX_HEADERS fields are accepted, one more is 431.
    let mut e = [0u8; 400];
    let mut at = 0;
    let mut push = |e: &mut [u8; 400], part: &[u8]| {
        e[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    };
    push(&mut e, b"GET / HTTP/1.1\r\nHost: x\r\n");
    for _ in 1..MAX_HEADERS {
        push(&mut e, b"A: b\r\n");
    }
    push(&mut e, b"\r\n");
    let n = at;
    let big = Limits { max_head: 1024, max_body: 0 };
    assert!(matches!(parse_head(&e[..n], &big), HeadResult::Done(h) if h.count == MAX_HEADERS));
    let mut f = [0u8; 400];
    f[..n - 2].copy_from_slice(&e[..n - 2]);
    f[n - 2..n + 6].copy_from_slice(b"A: b\r\n\r\n");
    assert_eq!(parse_head(&f[..n + 6], &big), HeadResult::Reject(431));
    assert_eq!(parse_head(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 11\r\n\r\n", &lim), HeadResult::Reject(413));
}

/// Decode a whole chunked body: the data and the bytes consumed.
fn dechunk(b: &[u8], max: u32) -> Result<([u8; 256], usize, usize), u16> {
    let mut st = CHUNK_START;
    let mut out = [0u8; 256];
    let mut n = 0;
    for (i, &c) in b.iter().enumerate() {
        let (s, o) = chunk_step(st, c, max);
        st = s;
        match o {
            ChunkOut::Nothing => {}
            ChunkOut::Data => {
                out[n] = c;
                n += 1;
            }
            ChunkOut::Done => return Ok((out, n, i + 1)),
            ChunkOut::Reject(s) => return Err(s),
        }
    }
    Err(0)
}

#[test]
fn chunked() {
    let (o, n, used) = dechunk(b"3\r\nabc\r\nA\r\n0123456789\r\n0\r\n\r\nNEXT", 100).unwrap();
    assert_eq!(&o[..n], b"abc0123456789");
    assert_eq!(used, 28);
    assert_eq!(dechunk(b"0\r\n\r\n", 0).map(|r| (r.1, r.2)), Ok((0, 5)));
    assert_eq!(dechunk(b"ff\r\n", 300).err(), Some(0));
    for (bad, s) in [
        (&b"3;ext=1\r\nabc\r\n0\r\n\r\n"[..], 400),
        (b"3 \r\nabc\r\n0\r\n\r\n", 400),
        (b"\r\n", 400),
        (b"3\nabc\r\n0\r\n\r\n", 400),
        (b"3\r\nabcd\r\n0\r\n\r\n", 400),
        (b"3\r\nabc\n0\r\n\r\n", 400),
        (b"0\r\nTrailer: x\r\n\r\n", 400),
        (b"0\r\n\n", 400),
        (b"123456789\r\n", 400),
        (b"-1\r\n", 400),
        (b"65\r\n", 413),
    ] {
        let r = dechunk(bad, 100);
        assert_eq!(r.map(|x| x.1), Err(s), "{:?}", core::str::from_utf8(bad));
    }
    // The total counts every chunk.
    assert_eq!(dechunk(b"32\r\n", 100).err(), Some(0));
    let mut b = [0u8; 120];
    b[..4].copy_from_slice(b"32\r\n");
    b[4..54].copy_from_slice(&[b'x'; 50]);
    b[54..60].copy_from_slice(b"\r\n33\r\n");
    assert_eq!(dechunk(&b[..60], 100).err(), Some(413));
    // States no byte sequence reaches are still total.
    for phase in 0..10u8 {
        for b in [0u8, 10, 13, 48, 70, 255] {
            let _ = chunk_step(ChunkState { phase, size: u64::MAX, digits: u64::MAX, total: u64::MAX }, b, u32::MAX);
        }
    }
}

#[test]
fn response_head() {
    let mut out = [b'#'; 128];
    let n = write_head(&mut out, 200, b"application/json", 1234, false).unwrap();
    assert_eq!(&out[..n], b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1234\r\n\r\n");
    assert!(out[n..].iter().all(|&c| c == b'#'));
    let n = write_head(&mut out, 431, b"", 0, true).unwrap();
    assert_eq!(&out[..n], b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let n = write_head(&mut out, 299, b"", u32::MAX, false).unwrap();
    assert_eq!(&out[..n], b"HTTP/1.1 299 \r\nContent-Length: 4294967295\r\n\r\n");
    assert_eq!(write_head(&mut out, 200, b"text/plain\r\nSet-Cookie: x", 0, false), None);
    assert_eq!(write_head(&mut out, 99, b"", 0, false), None);
    assert_eq!(write_head(&mut out, 600, b"", 0, false), None);
    let need = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".len();
    for k in 0..need {
        assert_eq!(write_head(&mut out[..k], 200, b"", 5, false), None, "{k}");
    }
    assert_eq!(write_head(&mut out[..need], 200, b"", 5, false), Some(need));
}
