//! Random heads, thrown at the two parsers, checking the properties that must
//! hold for *every* input rather than for the ones somebody thought of.
//!
//! Fifteen rounds of adversarial reading found real defects in this crate's
//! HTTP handling, and every one of them was a case a person constructed:
//! `Content-Length\u{0c}`, a bare CR in a response header, a second
//! `Content-Length` beside a `Transfer-Encoding`. That is a good way to find
//! the bugs somebody can imagine, and a bad way to find the rest. This module
//! generates heads instead — from a grammar seeded with the nastiest tokens the
//! previous rounds turned up, plus mutation and plain noise — and asserts the
//! invariants the rest of the crate is entitled to assume.
//!
//! Deterministic on purpose. A fuzzer that finds a failure CI cannot reproduce
//! is a fuzzer that reports flakes, so the generator is a fixed sequence and a
//! failure prints the seed that produced it. `H5I_FUZZ_ROUNDS` turns the same
//! test into a soak.

/// xorshift64*. Small, no dependency, and identical on every platform — which
/// is the only property that matters here.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // Zero is a fixed point of xorshift, so it cannot be a seed.
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

/// How many heads each fuzz test generates. Enough to be worth running in CI,
/// and `H5I_FUZZ_ROUNDS` raises it for a soak.
pub fn rounds() -> usize {
    std::env::var("H5I_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

/// Header names, weighted towards the ones that decide something.
///
/// The awkward spellings are not decoration: every one of them is a shape a
/// previous round found a bug with, or the neighbour of one. A generator that
/// only emits well-formed names tests the easy half.
const NAMES: &[&str] = &[
    "Host",
    "Connection",
    "Content-Length",
    "content-length",
    "CONTENT-LENGTH",
    "Content-Length\u{0c}",
    "Content-Length\t",
    " Content-Length",
    "Transfer-Encoding",
    "transfer-encoding",
    "Transfer-Encoding\u{0b}",
    "Cookie",
    "cookie",
    "Set-Cookie",
    "Upgrade",
    "Expect",
    "Referer",
    "X-Forwarded-For",
    "Accept",
    "Trailer",
    "TE",
    "",
    "A",
    "X-\u{85}Weird",
    "X-Very-Long-Name-That-Goes-On-And-On-And-On-And-On-And-On-And-On",
];

const VALUES: &[&str] = &[
    "0",
    "10",
    "-1",
    "+5",
    "007",
    "18446744073709551615",
    "18446744073709551616",
    "999999999999999999999999",
    "1 ",
    " 1",
    "1,1",
    "1, 2",
    "0x10",
    "chunked",
    "Chunked",
    "CHUNKED",
    "gzip, chunked",
    "chunked, gzip",
    "identity",
    "close",
    "keep-alive",
    "Keep-Alive, Upgrade",
    "upgrade",
    "Upgrade",
    "websocket",
    "100-continue",
    "100-Continue",
    "",
    " ",
    "\t",
    "h5i_share=deadbeef",
    "h5i_share_8080=deadbeef",
    "sid=9; h5i_share=deadbeef; other=1",
    "h5i_shareX=deadbeef",
    "a=1;;b=2",
    "//evil.example",
    "https://evil.example/",
    "/ok",
    "/ok?h5i=deadbeef",
    "\u{85}",
    "value with spaces",
];

const TARGETS: &[&str] = &[
    "/",
    "/index.html",
    "/?h5i=deadbeefdeadbeefdeadbeefdeadbeef",
    "/a?h5i=x&b=2",
    "/a?b=2&h5i=x",
    "/a?h5i=",
    "/a?h5ix=1",
    "//evil.example/",
    "///evil.example/",
    "/\\evil.example/",
    "http://evil.example/",
    "*",
    "",
    "/%2e%2e/",
    "/a?a=1&&b=2",
    "/very/long/path/that/keeps/going/and/going/and/going/and/going",
];

const METHODS: &[&str] = &[
    "GET",
    "POST",
    "HEAD",
    "PUT",
    "DELETE",
    "OPTIONS",
    "CONNECT",
    "TRACE",
    "get",
    "",
    "G E T",
    "GETGETGETGETGETGETGETGETGETGETGET",
];

/// Line endings, sampled **once per head** rather than once per line.
///
/// Sampled per line, with four of the five illegal, acceptance decayed as
/// (1/5)^(headers+1): measured against the real parser, 1.88% of heads got
/// past `gate::parse`, and of two million heads **zero** carried both framings
/// and about 157 carried a share cookie. The generator was emitting
/// `Content-Length\u{0c}` and obs-folds in more than half of all heads and the
/// parser was rejecting every one of them on line discipline before the
/// assertions ran. So the test's headline property — the credential never
/// reaches the box — was being checked on roughly one input per run, and the
/// two-framings assertion on none at all.
///
/// A whole head in one ending is also the realistic shape: a client that emits
/// bare LF emits it consistently.
const EOLS: &[&str] = &[
    "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n", "\r\n",
    "\r\n", "\r\n", "\r\n", "\r\n", "\n", "\r", "\r\r\n", "\n\r",
];

const STATUS_LINES: &[&str] = &[
    "HTTP/1.1 200 OK",
    "HTTP/1.1 204 No Content",
    "HTTP/1.1 304 Not Modified",
    "HTTP/1.1 101 Switching Protocols",
    "HTTP/1.1 100 Continue",
    "HTTP/1.1 103 Early Hints",
    "HTTP/1.0 200 OK",
    "HTTP/1.1 200",
    "HTTP/1.1",
    "200 OK",
    "",
    "HTTP/1.1 999 Nonsense",
];

/// Build a request head. Sometimes a plausible one, sometimes not.
pub fn request_head(rng: &mut Rng) -> String {
    let eol = rng.pick(EOLS);
    let mut out = String::new();
    out.push_str(rng.pick(METHODS));
    out.push(' ');
    out.push_str(rng.pick(TARGETS));
    out.push(' ');
    out.push_str(if rng.chance(8) {
        "HTTP/1.0"
    } else {
        "HTTP/1.1"
    });
    out.push_str(eol);
    for _ in 0..rng.below(7) {
        out.push_str(rng.pick(NAMES));
        out.push(':');
        if rng.chance(3) {
            out.push(' ');
        }
        out.push_str(rng.pick(VALUES));
        out.push_str(eol);
        // An obs-fold continuation, which is the shape that let a smuggled
        // length hide behind a name the sanitiser had already passed.
        if rng.chance(9) {
            out.push_str(if rng.chance(2) { " " } else { "\t" });
            out.push_str(rng.pick(VALUES));
            out.push_str(eol);
        }
    }
    out.push_str("\r\n");
    // Most heads are left alone. Mutation is what makes the nasty ones, and it
    // is also what makes them unparseable — mutating every head meant the
    // parser rejected almost all of them and the assertions past it ran on
    // roughly one input per run.
    if rng.chance(3) {
        return mutate(rng, out);
    }
    out
}

/// Build a response head.
pub fn response_head(rng: &mut Rng) -> Vec<u8> {
    let eol = rng.pick(EOLS);
    let mut out = String::new();
    out.push_str(rng.pick(STATUS_LINES));
    out.push_str(eol);
    for _ in 0..rng.below(7) {
        out.push_str(rng.pick(NAMES));
        out.push(':');
        if rng.chance(3) {
            out.push(' ');
        }
        out.push_str(rng.pick(VALUES));
        out.push_str(eol);
        if rng.chance(9) {
            out.push('\t');
            out.push_str(rng.pick(VALUES));
            out.push_str(eol);
        }
    }
    out.push_str("\r\n");
    if rng.chance(3) {
        return mutate(rng, out).into_bytes();
    }
    out.into_bytes()
}

/// Corrupt a head the way a hostile client would: splice, truncate, and inject
/// the bytes that mean something to a parser.
fn mutate(rng: &mut Rng, mut s: String) -> String {
    for _ in 0..rng.below(3) {
        if s.is_empty() {
            break;
        }
        match rng.below(5) {
            0 => {
                // Truncate. Half of the response-side bugs were about a head
                // the box never finished.
                let at = floor_char(&s, rng.below(s.len()));
                s.truncate(at);
            }
            1 => {
                // Splice in a control byte at a character boundary.
                let at = floor_char(&s, rng.below(s.len()));
                let b = rng.pick(&["\r", "\n", "\0", "\u{0b}", "\u{0c}", " ", "\t", ":", ";"]);
                s.insert_str(at, b);
            }
            2 => {
                // Duplicate a line.
                let lines: Vec<&str> = s.split_inclusive('\n').collect();
                if !lines.is_empty() {
                    let line = lines[rng.below(lines.len())].to_string();
                    s.push_str(&line);
                }
            }
            3 => s.push_str(rng.pick(NAMES)),
            _ => {
                let at = floor_char(&s, rng.below(s.len()));
                s.insert_str(at, rng.pick(VALUES));
            }
        }
    }
    s
}

/// Nearest character boundary at or below `i`, so `insert_str`/`truncate`
/// cannot panic on a multi-byte value from the pool.
fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Pieces of a ticket, including every shape a hostile one would use.
///
/// A ticket is the one thing on the joiner's side that arrives entirely from
/// somebody else and is pasted in by hand. Hand-written review of it has
/// already found two real defects — an address filter that caught one spelling
/// of loopback out of four, and no cap on how many places one ticket could aim
/// at — so it is worth generating rather than only imagining.
const TICKET_ADDRS: &[&str] = &[
    r#"{"Ip":"127.0.0.1:2375"}"#,
    r#"{"Ip":"0.0.0.0:2375"}"#,
    r#"{"Ip":"[::1]:9200"}"#,
    r#"{"Ip":"[::]:9200"}"#,
    r#"{"Ip":"[::ffff:127.0.0.1]:9200"}"#,
    r#"{"Ip":"169.254.169.254:80"}"#,
    r#"{"Ip":"192.168.1.20:41641"}"#,
    r#"{"Ip":"10.0.0.5:41641"}"#,
    r#"{"Ip":"8.8.8.8:41641"}"#,
    r#"{"Relay":"http://127.0.0.1:9200/"}"#,
    r#"{"Relay":"http://0.0.0.0:9200/"}"#,
    r#"{"Relay":"http://[::ffff:127.0.0.1]:9200/"}"#,
    r#"{"Relay":"https://use1-1.relay.n0.iroh.link./"}"#,
    r#"{"Relay":"file:///etc/passwd"}"#,
    r#"{"Relay":"http://localhost:11434/"}"#,
    r#"{"Custom":"whatever"}"#,
    "null",
    "42",
    r#""a string""#,
];

const TICKET_SECRETS: &[&str] = &["", "ff", "zz", "not-hex-at-all", "0011"];

/// Build something ticket-shaped. Sometimes valid, usually not.
pub fn ticket_json(rng: &mut Rng) -> String {
    let mut addrs: Vec<String> = Vec::new();
    let many = rng.chance(6);
    for _ in 0..rng.below(if many { 300 } else { 6 }) {
        addrs.push(rng.pick(TICKET_ADDRS).to_string());
    }
    let good = "ab".repeat(crate::ticket::SECRET_BYTES);
    // Weighted toward valid. The interesting properties are about what an
    // *accepted* ticket may contain, so a generator that mostly produces
    // tickets the decoder throws out is an expensive way of testing the
    // decoder's mood: measured at 3.6% accepted before this, against floors
    // that then failed and said so.
    let secret = if rng.chance(4) {
        rng.pick(TICKET_SECRETS)
    } else {
        good.as_str()
    };
    let id = if rng.chance(20) {
        String::new()
    } else {
        "cd".repeat(32)
    };
    format!(
        concat!(
            r#"{{"v":{},"box_id":{},"port":{},"grant":"g1","expires_at":{},"#,
            r#""secret":"{}","addr":{{"id":"{}","addrs":[{}]}}}}"#
        ),
        rng.pick(&[1u32, 1, 1, 1, 1, 1, 0, 2, 99]),
        rng.pick(&[
            r#""env/human/demo""#,
            r#""env/human/demo""#,
            r#""env/human/demo""#,
            r#""esc-here""#,
            r#""""#,
            "null",
            "5"
        ]),
        rng.pick(&[3000u32, 0, 65535, 70000]),
        rng.pick(&[4_000_000_000i64, 0, -1, i64::MAX]),
        secret,
        id,
        addrs.join(",")
    )
}

/// Wrap a ticket body the way `h5i box share` prints it.
pub fn encode_ticket(body: &str) -> String {
    use base64::Engine as _;
    format!(
        "h5i1_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.as_bytes())
    )
}
