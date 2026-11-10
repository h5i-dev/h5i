//! A strict HTTP/1.1 request-head parser, chunked body decoder and response
//! head writer for devices with a few hundred KB of RAM.
//!
//! `#![no_std]`, no `alloc`, no dependencies. Every function is total and
//! written in the Rust subset Charon and Aeneas translate to Lean; the proofs
//! in `proofs/` are about this code as extracted
//! (`docs/design/design-app-embedded.md`). Results are offsets into the
//! caller's buffer, never borrows.
//!
//! It accepts a subset of HTTP/1.1 and rejects some valid requests: CRLF line
//! ends only, no obs-fold, no whitespace before a colon, origin-form targets
//! (`absolute-path [ "?" query ]`), exactly one `Host` on HTTP/1.1 with a
//! `uri-host [ ":" port ]` value, at most one `Content-Length` (digits only)
//! and at most one `Transfer-Encoding` (exactly `chunked`), never both.

#![no_std]

pub const CR: u8 = 13;
pub const LF: u8 = 10;
pub const SP: u8 = 32;
pub const HTAB: u8 = 9;
pub const COLON: u8 = 58;

/// Header fields kept per request. One more is 431.
pub const MAX_HEADERS: usize = 16;
/// Hex digits in a chunk size. One more is 400.
pub const MAX_CHUNK_DIGITS: u64 = 8;

pub const BAD_REQUEST: u16 = 400;
pub const PAYLOAD_TOO_LARGE: u16 = 413;
pub const URI_TOO_LONG: u16 = 414;
pub const HEADERS_TOO_LARGE: u16 = 431;
pub const NOT_IMPLEMENTED: u16 = 501;
pub const VERSION_NOT_SUPPORTED: u16 = 505;

/// Per-server limits. `max_head` counts the whole head, final CRLF CRLF
/// included; `max_body` bounds `Content-Length` and the sum of chunk sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_head: usize,
    pub max_body: u32,
}

/// `buf[start..end]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A header field: its name, and its value without surrounding whitespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: Span,
    pub value: Span,
}

/// Where the body ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// No `Content-Length` and no `Transfer-Encoding`: no body.
    Empty,
    /// `Content-Length`, at most `max_body`.
    Length(u64),
    /// `Transfer-Encoding: chunked`, decoded by [`chunk_step`].
    Chunked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub method: Span,
    pub target: Span,
    /// `HTTP/1.<minor>`: 0 or 1.
    pub minor: u8,
    /// The first `count` are the request's fields, in order.
    pub headers: [Header; MAX_HEADERS],
    pub count: usize,
    /// Bytes of the head, final CRLF CRLF included. The body starts here.
    pub len: usize,
    pub framing: Framing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadResult {
    /// Read more and call again with the longer buffer.
    Incomplete,
    /// Answer with this status and close the connection.
    Reject(u16),
    Done(Head),
}

/// `tchar` (RFC 9110 §5.6.2): `!#$%&'*+-.^_`|~`, digits and letters. One
/// test per range, so the extracted Lean has one branch per range.
pub fn is_tchar(b: u8) -> bool {
    if b < 33 {
        false
    } else if b < 48 {
        b != 34 && b != 40 && b != 41 && b != 44 && b != 47
    } else if b < 58 {
        true
    } else if b < 65 {
        false
    } else if b < 91 {
        true
    } else if b < 94 {
        false
    } else if b < 123 {
        true
    } else {
        b == 124 || b == 126
    }
}

/// A byte of a field value: VCHAR, obs-text, SP or HTAB.
pub fn is_field_byte(b: u8) -> bool {
    b == HTAB || (b >= SP && b != 127)
}

/// `unreserved` or `sub-delims` (RFC 3986 §2.2, §2.3): a byte of a
/// `reg-name` other than `%`. One test per range, as in [`is_tchar`].
pub fn is_reg_byte(b: u8) -> bool {
    if b < 33 {
        false
    } else if b < 48 {
        b != 34 && b != 35 && b != 37 && b != 47
    } else if b < 58 {
        true
    } else if b < 65 {
        b == 59 || b == 61
    } else if b < 91 {
        true
    } else if b < 97 {
        b == 95
    } else if b < 123 {
        true
    } else {
        b == 126
    }
}

/// Byte classes for [`span_end`].
pub const CLASS_REG: u8 = 0;
pub const CLASS_TARGET: u8 = 1;
pub const CLASS_LITERAL: u8 = 2;
pub const CLASS_DIGIT: u8 = 3;

/// A byte of class `class` other than `%`: a `reg-name` byte; a target byte
/// (`pchar`, `/` or `?`); an IP-literal byte (`unreserved`, `sub-delims`,
/// `:`); a digit.
pub fn byte_in(b: u8, class: u8) -> bool {
    if class == CLASS_DIGIT {
        b >= 48 && b <= 57
    } else if is_reg_byte(b) {
        true
    } else if class == CLASS_TARGET {
        b == COLON || b == 64 || b == 47 || b == 63
    } else if class == CLASS_LITERAL {
        b == COLON
    } else {
        false
    }
}

fn is_ows(b: u8) -> bool {
    b == SP || b == HTAB
}

/// ASCII lowercase.
pub fn lower(b: u8) -> u8 {
    if b >= 65 && b <= 90 { b + 32 } else { b }
}

/// First index in `[i, end)` whose byte is not a `tchar`, or `end`.
fn token_end(buf: &[u8], i: usize, end: usize) -> usize {
    let mut k = i;
    while k < end {
        if !is_tchar(buf[k]) {
            return k;
        }
        k += 1;
    }
    k
}

/// The end of the longest run from `i` within `[i, end)` of bytes of
/// `class` ([`byte_in`]) and, for a `reg-name` or a target, `%` HEXDIG
/// HEXDIG. A `%` not followed by two hex digits ends the run at the `%`.
fn span_end(buf: &[u8], i: usize, end: usize, class: u8) -> usize {
    let mut k = i;
    while k < end {
        let b = buf[k];
        if b == 37 && class <= CLASS_TARGET {
            if end - k > 2 && hex_value(buf[k + 1]) < 16 && hex_value(buf[k + 2]) < 16 {
                k += 3;
            } else {
                return k;
            }
        } else if byte_in(b, class) {
            k += 1;
        } else {
            return k;
        }
    }
    k
}

/// First index in `[i, n)` holding CR or LF, or `n`.
fn line_end(buf: &[u8], i: usize, n: usize) -> usize {
    let mut j = i;
    while j < n {
        let b = buf[j];
        if b == CR || b == LF {
            return j;
        }
        j += 1;
    }
    j
}

/// How the line ending at `j` (from [`line_end`]) is terminated within
/// `buf[..n]`: 0 by CRLF, 1 not yet (more bytes needed), 2 by a bare CR or LF.
fn line_term(buf: &[u8], j: usize, n: usize) -> u8 {
    if j >= n {
        return 1;
    }
    if buf[j] == LF {
        return 2;
    }
    if j + 1 >= n {
        return 1;
    }
    if buf[j + 1] != LF {
        return 2;
    }
    0
}

/// `buf[s..s + lit.len()]` is exactly `lit`.
fn eq_at(buf: &[u8], s: usize, lit: &[u8]) -> bool {
    let mut q = 0;
    while q < lit.len() {
        if buf[s + q] != lit[q] {
            return false;
        }
        q += 1;
    }
    true
}

/// `buf[s..e]` is `HTTP/1.0` (0), `HTTP/1.1` (1), another `HTTP/d.d` (2),
/// or something else (3).
fn version(buf: &[u8], s: usize, e: usize) -> u8 {
    if e >= s && e - s == 8 {
        if eq_at(buf, s, b"HTTP/") {
            return version_digits(buf[s + 5], buf[s + 6], buf[s + 7]);
        }
    }
    3
}

/// The version from the bytes after `HTTP/`: `a "." b`.
fn version_digits(a: u8, dot: u8, b: u8) -> u8 {
    if dot != 46 || a < 48 || a > 57 || b < 48 || b > 57 {
        return 3;
    }
    if a != 49 {
        return 2;
    }
    if b == 48 {
        0
    } else if b == 49 {
        1
    } else {
        2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReqLine {
    Bad(u16),
    /// Method, target, minor version.
    Ok(Span, Span, u8),
}

/// `method SP origin-form SP HTTP-version` in `buf[i..j]`. The target is
/// `absolute-path [ "?" query ]` (RFC 9112 §3.2.1): `/` then `pchar`, `/`,
/// `?` and `%` HEXDIG HEXDIG; anything else, `#` included, is 400.
fn request_line(buf: &[u8], i: usize, j: usize) -> ReqLine {
    let m = token_end(buf, i, j);
    if m == i || m >= j {
        return ReqLine::Bad(BAD_REQUEST);
    }
    if buf[m] != SP {
        return ReqLine::Bad(BAD_REQUEST);
    }
    let ts = m + 1;
    let te = span_end(buf, ts, j, CLASS_TARGET);
    if te == ts || te >= j {
        return ReqLine::Bad(BAD_REQUEST);
    }
    if buf[ts] != 47 || buf[te] != SP {
        return ReqLine::Bad(BAD_REQUEST);
    }
    let v = version(buf, te + 1, j);
    if v == 3 {
        return ReqLine::Bad(BAD_REQUEST);
    }
    if v == 2 {
        return ReqLine::Bad(VERSION_NOT_SUPPORTED);
    }
    ReqLine::Ok(Span { start: i, end: m }, Span { start: ts, end: te }, v)
}

/// Every byte of `buf[s..e]` is a field-value byte.
fn field_bytes(buf: &[u8], s: usize, e: usize) -> bool {
    let mut p = s;
    while p < e {
        if !is_field_byte(buf[p]) {
            return false;
        }
        p += 1;
    }
    true
}

/// First index in `[s, e)` that is not SP or HTAB, or `e`.
fn skip_ows(buf: &[u8], s: usize, e: usize) -> usize {
    let mut p = s;
    while p < e {
        if !is_ows(buf[p]) {
            return p;
        }
        p += 1;
    }
    p
}

/// `e` less the SP and HTAB that end `buf[s..e]`.
fn trim_ows_end(buf: &[u8], s: usize, e: usize) -> usize {
    let mut p = e;
    while p > s {
        if !is_ows(buf[p - 1]) {
            return p;
        }
        p -= 1;
    }
    p
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrLine {
    Bad(u16),
    /// The colon's index, and the trimmed value's start and end.
    Ok(usize, usize, usize),
}

/// `field-name ":" OWS field-value OWS` in `buf[i..j]`. A line that starts
/// with SP or HTAB (obs-fold) and a name followed by whitespace are rejected
/// here, since neither is a `tchar` nor a colon.
fn header_line(buf: &[u8], i: usize, j: usize) -> HdrLine {
    let k = token_end(buf, i, j);
    if k == i || k >= j {
        return HdrLine::Bad(BAD_REQUEST);
    }
    if buf[k] != COLON {
        return HdrLine::Bad(BAD_REQUEST);
    }
    if !field_bytes(buf, k + 1, j) {
        return HdrLine::Bad(BAD_REQUEST);
    }
    let vs = skip_ows(buf, k + 1, j);
    let ve = trim_ows_end(buf, vs, j);
    HdrLine::Ok(k, vs, ve)
}

/// `buf[s..e]` equals the lowercase `lit`, ignoring ASCII case.
fn eq_ci(buf: &[u8], s: usize, e: usize, lit: &[u8]) -> bool {
    if e < s || e - s != lit.len() {
        return false;
    }
    let mut q = 0;
    while q < lit.len() {
        if lower(buf[s + q]) != lit[q] {
            return false;
        }
        q += 1;
    }
    true
}

/// `[p, e)` is empty or `":" *DIGIT`.
fn port_ok(buf: &[u8], p: usize, e: usize) -> bool {
    if p >= e {
        return true;
    }
    if buf[p] != COLON {
        return false;
    }
    span_end(buf, p + 1, e, CLASS_DIGIT) == e
}

/// `buf[s..e]` is a `Host` value: `uri-host [ ":" port ]` (RFC 9110 §7.2),
/// where `uri-host` is a `reg-name` (which covers IPv4 addresses) or `[`
/// IP-literal bytes `]`. The empty value is allowed.
fn host_ok(buf: &[u8], s: usize, e: usize) -> bool {
    if s < e && buf[s] == 91 {
        let k = span_end(buf, s + 1, e, CLASS_LITERAL);
        if k == s + 1 || k >= e || buf[k] != 93 {
            return false;
        }
        port_ok(buf, k + 1, e)
    } else {
        port_ok(buf, span_end(buf, s, e, CLASS_REG), e)
    }
}

pub const KIND_OTHER: u8 = 0;
pub const KIND_HOST: u8 = 1;
pub const KIND_CONTENT_LENGTH: u8 = 2;
pub const KIND_TRANSFER_ENCODING: u8 = 3;

/// Which of the fields that decide framing `buf[s..e]` names.
fn classify(buf: &[u8], s: usize, e: usize) -> u8 {
    if eq_ci(buf, s, e, b"host") {
        KIND_HOST
    } else if eq_ci(buf, s, e, b"content-length") {
        KIND_CONTENT_LENGTH
    } else if eq_ci(buf, s, e, b"transfer-encoding") {
        KIND_TRANSFER_ENCODING
    } else {
        KIND_OTHER
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Num {
    Bad(u16),
    Ok(u64),
}

/// `1*DIGIT` in `buf[s..e]`, at most `max`. Larger is 413, checked after
/// each digit, so the value never exceeds `10 * max + 9`.
fn content_length(buf: &[u8], s: usize, e: usize, max: u64) -> Num {
    if s >= e {
        return Num::Bad(BAD_REQUEST);
    }
    let mut acc: u64 = 0;
    let mut p = s;
    while p < e {
        let c = buf[p];
        if c < 48 || c > 57 {
            return Num::Bad(BAD_REQUEST);
        }
        acc = acc * 10 + (c - 48) as u64;
        if acc > max {
            return Num::Bad(PAYLOAD_TOO_LARGE);
        }
        p += 1;
    }
    Num::Ok(acc)
}

/// The fields read so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub headers: [Header; MAX_HEADERS],
    pub count: usize,
    pub host: bool,
    pub has_cl: bool,
    pub cl: u64,
    pub te: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    Bad(u16),
    Ok(Fields),
}

/// Record the field `buf[ns..ne]: buf[vs..ve]`, from the line ending at
/// `le`. `Content-Length` and `Transfer-Encoding` values may not end in
/// whitespace (`ve == le`): a parser that keeps trailing whitespace would read
/// `chunked ` as another coding.
fn add_header(
    buf: &[u8],
    f: Fields,
    ns: usize,
    ne: usize,
    vs: usize,
    ve: usize,
    le: usize,
    minor: u8,
    max_body: u64,
) -> Added {
    if f.count >= MAX_HEADERS {
        return Added::Bad(HEADERS_TOO_LARGE);
    }
    let mut g = f;
    let kind = classify(buf, ns, ne);
    if kind == KIND_HOST {
        if g.host || !host_ok(buf, vs, ve) {
            return Added::Bad(BAD_REQUEST);
        }
        g.host = true;
    } else if kind == KIND_CONTENT_LENGTH {
        if g.has_cl || g.te || ve != le {
            return Added::Bad(BAD_REQUEST);
        }
        match content_length(buf, vs, ve, max_body) {
            Num::Bad(s) => return Added::Bad(s),
            Num::Ok(v) => {
                g.has_cl = true;
                g.cl = v;
            }
        }
    } else if kind == KIND_TRANSFER_ENCODING {
        if minor == 0 || g.te || g.has_cl || ve != le {
            return Added::Bad(BAD_REQUEST);
        }
        if !eq_ci(buf, vs, ve, b"chunked") {
            return Added::Bad(NOT_IMPLEMENTED);
        }
        g.te = true;
    }
    g.headers[g.count] = Header { name: Span { start: ns, end: ne }, value: Span { start: vs, end: ve } };
    g.count += 1;
    Added::Ok(g)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fielded {
    /// More bytes needed (or the limit was reached, if the buffer is full).
    More,
    Bad(u16),
    /// The fields and the head's length.
    Done(Fields, usize),
}

/// The header lines from `i` to the empty line, within `buf[..n]`.
fn fields(buf: &[u8], n: usize, i: usize, f: Fields, minor: u8, max_body: u64) -> Fielded {
    let mut i = i;
    let mut f = f;
    while i < n {
        let j = line_end(buf, i, n);
        let t = line_term(buf, j, n);
        if t == 1 {
            return Fielded::More;
        }
        if t == 2 {
            return Fielded::Bad(BAD_REQUEST);
        }
        if j == i {
            return Fielded::Done(f, j + 2);
        }
        match header_line(buf, i, j) {
            HdrLine::Bad(s) => return Fielded::Bad(s),
            HdrLine::Ok(k, vs, ve) => match add_header(buf, f, i, k, vs, ve, j, minor, max_body) {
                Added::Bad(s) => return Fielded::Bad(s),
                Added::Ok(g) => f = g,
            },
        }
        i = j + 2;
    }
    Fielded::More
}

const NO_SPAN: Span = Span { start: 0, end: 0 };
const NO_HEADER: Header = Header { name: NO_SPAN, value: NO_SPAN };

/// Parse the request head at the start of `buf`.
///
/// Reads at most `lim.max_head` bytes. `Incomplete` only while
/// `buf.len() < lim.max_head`; a buffer that long without a complete head is
/// 431 (414 if the request line itself is unfinished).
pub fn parse_head(buf: &[u8], lim: &Limits) -> HeadResult {
    let full = buf.len() >= lim.max_head;
    let n = if full { lim.max_head } else { buf.len() };
    let j = line_end(buf, 0, n);
    let t = line_term(buf, j, n);
    if t == 1 {
        if full {
            return HeadResult::Reject(URI_TOO_LONG);
        }
        return HeadResult::Incomplete;
    }
    if t == 2 {
        return HeadResult::Reject(BAD_REQUEST);
    }
    match request_line(buf, 0, j) {
        ReqLine::Bad(s) => HeadResult::Reject(s),
        ReqLine::Ok(method, target, minor) => {
            let f0 = Fields { headers: [NO_HEADER; MAX_HEADERS], count: 0, host: false, has_cl: false, cl: 0, te: false };
            match fields(buf, n, j + 2, f0, minor, lim.max_body as u64) {
                Fielded::More => {
                    if full {
                        HeadResult::Reject(HEADERS_TOO_LARGE)
                    } else {
                        HeadResult::Incomplete
                    }
                }
                Fielded::Bad(s) => HeadResult::Reject(s),
                Fielded::Done(f, len) => {
                    if minor == 1 && !f.host {
                        return HeadResult::Reject(BAD_REQUEST);
                    }
                    let framing = if f.te {
                        Framing::Chunked
                    } else if f.has_cl {
                        Framing::Length(f.cl)
                    } else {
                        Framing::Empty
                    };
                    HeadResult::Done(Head { method, target, minor, headers: f.headers, count: f.count, len, framing })
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Chunked bodies

/// Reading chunk-size hex digits.
pub const CH_SIZE: u8 = 0;
/// After the size's CR: the LF.
pub const CH_SIZE_LF: u8 = 1;
/// Inside chunk-data.
pub const CH_DATA: u8 = 2;
/// After chunk-data: the CR.
pub const CH_DATA_CR: u8 = 3;
/// After chunk-data's CR: the LF.
pub const CH_DATA_LF: u8 = 4;
/// After the last chunk's `0 CRLF`: the CR of the empty trailer section.
pub const CH_LAST_CR: u8 = 5;
/// Its LF.
pub const CH_LAST_LF: u8 = 6;
/// The body has ended; any further byte is an error.
pub const CH_END: u8 = 7;

/// The chunked decoder's state: constant size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkState {
    pub phase: u8,
    /// The chunk size read so far, or the bytes left in the chunk.
    pub size: u64,
    /// Hex digits read for this size.
    pub digits: u64,
    /// Sum of the chunk sizes so far.
    pub total: u64,
}

pub const CHUNK_START: ChunkState = ChunkState { phase: CH_SIZE, size: 0, digits: 0, total: 0 };

/// What one byte of a chunked body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkOut {
    /// Framing.
    Nothing,
    /// A byte of the body.
    Data,
    /// The last byte of the message.
    Done,
    Reject(u16),
}

/// Value of a hex digit (either case), or 16.
pub fn hex_value(c: u8) -> u64 {
    if c >= 48 && c <= 57 {
        (c - 48) as u64
    } else if c >= 97 && c <= 102 {
        (c - 87) as u64
    } else if c >= 65 && c <= 70 {
        (c - 55) as u64
    } else {
        16
    }
}

/// Decode one byte of `chunked` (RFC 9112 §7.1): `1*8HEXDIG CRLF` sizes, no
/// chunk extensions, no trailer fields, sizes summing to at most `max_body`.
/// Loop-free; the state is four integers.
pub fn chunk_step(st: ChunkState, b: u8, max_body: u32) -> (ChunkState, ChunkOut) {
    if st.phase == CH_SIZE {
        chunk_size(st, b, max_body)
    } else if st.phase == CH_SIZE_LF {
        chunk_size_lf(st, b)
    } else if st.phase == CH_DATA {
        chunk_data(st)
    } else if st.phase == CH_DATA_CR {
        expect(st, b, CR, CH_DATA_LF, ChunkOut::Nothing)
    } else if st.phase == CH_DATA_LF {
        let s = ChunkState { phase: st.phase, size: 0, digits: st.digits, total: st.total };
        expect(s, b, LF, CH_SIZE, ChunkOut::Nothing)
    } else if st.phase == CH_LAST_CR {
        expect(st, b, CR, CH_LAST_LF, ChunkOut::Nothing)
    } else if st.phase == CH_LAST_LF {
        expect(st, b, LF, CH_END, ChunkOut::Done)
    } else {
        (st, ChunkOut::Reject(BAD_REQUEST))
    }
}

/// A size digit, or the CR after the size.
fn chunk_size(st: ChunkState, b: u8, max_body: u32) -> (ChunkState, ChunkOut) {
    let v = hex_value(b);
    if v < 16 {
        // `size < 16^digits`, so the second test only matters for a state
        // no byte sequence reaches; it keeps `chunk_step` total.
        if st.digits >= MAX_CHUNK_DIGITS || st.size > 268435455 {
            return (st, ChunkOut::Reject(BAD_REQUEST));
        }
        let s = ChunkState { phase: st.phase, size: st.size * 16 + v, digits: st.digits + 1, total: st.total };
        return (s, ChunkOut::Nothing);
    }
    if b == CR && st.digits > 0 {
        let max = max_body as u64;
        if st.total > max || st.size > max - st.total {
            return (st, ChunkOut::Reject(PAYLOAD_TOO_LARGE));
        }
        let s = ChunkState { phase: CH_SIZE_LF, size: st.size, digits: st.digits, total: st.total + st.size };
        return (s, ChunkOut::Nothing);
    }
    (st, ChunkOut::Reject(BAD_REQUEST))
}

/// The LF after the size: data next, or the end for size 0.
fn chunk_size_lf(st: ChunkState, b: u8) -> (ChunkState, ChunkOut) {
    if b != LF {
        return (st, ChunkOut::Reject(BAD_REQUEST));
    }
    let next = if st.size == 0 { CH_LAST_CR } else { CH_DATA };
    (ChunkState { phase: next, size: st.size, digits: 0, total: st.total }, ChunkOut::Nothing)
}

/// A byte of chunk-data, whatever its value.
fn chunk_data(st: ChunkState) -> (ChunkState, ChunkOut) {
    if st.size <= 1 {
        (ChunkState { phase: CH_DATA_CR, size: 0, digits: st.digits, total: st.total }, ChunkOut::Data)
    } else {
        (ChunkState { phase: st.phase, size: st.size - 1, digits: st.digits, total: st.total }, ChunkOut::Data)
    }
}

/// Exactly `want`, then phase `next` with output `out`.
fn expect(st: ChunkState, b: u8, want: u8, next: u8, out: ChunkOut) -> (ChunkState, ChunkOut) {
    if b != want {
        return (st, ChunkOut::Reject(BAD_REQUEST));
    }
    (ChunkState { phase: next, size: st.size, digits: st.digits, total: st.total }, out)
}

// ---------------------------------------------------------------------------
// Bodies in the receive buffer

/// Progress through a body in the receive buffer: the chunked decoder's
/// state, the next byte to read (`r`) and where the next payload byte goes
/// (`w`). A chunked body is decoded in place, so `w <= r`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyState {
    pub chunk: ChunkState,
    pub r: usize,
    pub w: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyOut {
    /// Append more bytes at `filled` and call again with the returned state.
    More,
    /// The payload is `buf[start..end]` and the message is `buf[..used]`:
    /// `Done(end, used)`.
    Done(usize, usize),
    /// Answer with this status and close the connection.
    Reject(u16),
}

pub const INTERNAL_ERROR: u16 = 500;

/// The state before the first body byte, for a head of `start` bytes.
pub fn body_start(start: usize) -> BodyState {
    BodyState { chunk: CHUNK_START, r: start, w: start }
}

/// Read the body framed by `framing` from `buf[..filled]`, which holds a
/// head of `start` bytes and then the body bytes received so far. A chunked
/// body is decoded in place: its payload ends up at `buf[start..end]`.
/// `st` is [`body_start`] on the first call and the returned state after.
/// Reads and writes only below `filled`; a `Content-Length` that cannot fit
/// in `buf` is 413.
pub fn read_body(
    buf: &mut [u8],
    filled: usize,
    start: usize,
    framing: Framing,
    st: BodyState,
    max_body: u32,
) -> (BodyState, BodyOut) {
    match framing {
        Framing::Empty => (st, BodyOut::Done(start, start)),
        Framing::Length(n) => {
            if start > buf.len() || n > (buf.len() - start) as u64 {
                return (st, BodyOut::Reject(PAYLOAD_TOO_LARGE));
            }
            let end = start + n as usize;
            if filled >= end {
                (st, BodyOut::Done(end, end))
            } else {
                (st, BodyOut::More)
            }
        }
        Framing::Chunked => dechunk(buf, filled, st, max_body),
    }
}

/// Run [`chunk_step`] over `buf[st.r..filled]`, moving each payload byte
/// down to `st.w`. `Reject(500)` if `st.w > st.r` or `filled > buf.len()`,
/// which no state from [`body_start`] reaches.
pub fn dechunk(buf: &mut [u8], filled: usize, st: BodyState, max_body: u32) -> (BodyState, BodyOut) {
    if st.w > st.r || filled > buf.len() {
        return (st, BodyOut::Reject(INTERNAL_ERROR));
    }
    let mut cs = st.chunk;
    let mut r = st.r;
    let mut w = st.w;
    while r < filled {
        let b = buf[r];
        let (s, out) = chunk_step(cs, b, max_body);
        cs = s;
        r += 1;
        match out {
            ChunkOut::Nothing => {}
            ChunkOut::Data => {
                buf[w] = b;
                w += 1;
            }
            ChunkOut::Done => return (BodyState { chunk: cs, r, w }, BodyOut::Done(w, r)),
            ChunkOut::Reject(x) => return (BodyState { chunk: cs, r, w }, BodyOut::Reject(x)),
        }
    }
    (BodyState { chunk: cs, r, w }, BodyOut::More)
}

// ---------------------------------------------------------------------------
// Receiving requests, without I/O

/// Where [`recv_step`] is in the current request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvState {
    /// No complete head yet.
    Head,
    /// The head is parsed and its body is being read.
    Body(Head, BodyState),
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Read more bytes into `rx[filled..]` (never empty), add their number to
    /// `filled` (or call again with `eof` if the read returned 0), and call
    /// again with the returned state.
    NeedRead,
    /// A request: its head, its payload at `rx[start..end]` and its message
    /// at `rx[..used]`: `Ready(head, start, end, used)`. `rx[used..filled]`
    /// is unchanged, the start of the next request ([`carry`]).
    Ready(Head, usize, usize, usize),
    /// Answer with this status and close the connection.
    Reject(u16),
    /// The peer closed the connection between requests.
    Eof,
}

/// The state before a request.
pub fn recv_start() -> RecvState {
    RecvState::Head
}

/// Decide what to do with the bytes `rx[..filled]` received so far for the
/// current request. `eof`: the peer closed the connection and no more
/// bytes will come. A head or body that cannot be decided before `rx` is
/// full is 431 or 413, so `NeedRead` always leaves room in `rx`. Reads and
/// writes only `rx[..filled]`; a chunked body is decoded in place.
pub fn recv_step(st: RecvState, rx: &mut [u8], filled: usize, eof: bool, lim: &Limits) -> (RecvState, Action) {
    if filled > rx.len() {
        return (RecvState::Head, Action::Reject(INTERNAL_ERROR));
    }
    match st {
        RecvState::Head => match parse_head(&rx[..filled], lim) {
            HeadResult::Done(h) => recv_body(h, body_start(h.len), rx, filled, eof, lim.max_body),
            HeadResult::Reject(s) => (RecvState::Head, Action::Reject(s)),
            HeadResult::Incomplete => {
                let a = undecided(filled, rx.len(), eof, true, HEADERS_TOO_LARGE);
                (RecvState::Head, a)
            }
        },
        RecvState::Body(h, bs) => recv_body(h, bs, rx, filled, eof, lim.max_body),
    }
}

/// The body step of [`recv_step`].
fn recv_body(
    h: Head,
    bs: BodyState,
    rx: &mut [u8],
    filled: usize,
    eof: bool,
    max_body: u32,
) -> (RecvState, Action) {
    let (bs2, out) = read_body(rx, filled, h.len, h.framing, bs, max_body);
    match out {
        BodyOut::Done(end, used) => (RecvState::Head, Action::Ready(h, h.len, end, used)),
        BodyOut::Reject(s) => (RecvState::Head, Action::Reject(s)),
        BodyOut::More => {
            let a = undecided(filled, rx.len(), eof, false, PAYLOAD_TOO_LARGE);
            (RecvState::Body(h, bs2), a)
        }
    }
}

/// Nothing decided on `filled` bytes of a buffer of `cap`: `full` if the
/// buffer is full; else `Eof` if the peer closed between requests, 400 if
/// it closed in one; else read. (Full first: the answer to a full buffer
/// does not depend on whether a close follows.)
fn undecided(filled: usize, cap: usize, eof: bool, between: bool, full: u16) -> Action {
    if filled >= cap {
        Action::Reject(full)
    } else if eof {
        if between && filled == 0 { Action::Eof } else { Action::Reject(BAD_REQUEST) }
    } else {
        Action::NeedRead
    }
}

/// After a request that used `rx[..used]`: move `rx[used..filled]` to the
/// start of `rx` for the next request, and return its length (the next
/// `filled`). 0 if `used > filled` or `filled > rx.len()`, which no `Ready`
/// gives.
pub fn carry(rx: &mut [u8], used: usize, filled: usize) -> usize {
    if used > filled || filled > rx.len() {
        return 0;
    }
    let n = filled - used;
    let mut i = 0;
    while i < n {
        rx[i] = rx[used + i];
        i += 1;
    }
    n
}

/// `lim` cut down so that a whole request fits in a receive buffer of
/// `cap` bytes: `max_head <= cap` and `max_head + max_body <= cap`.
pub fn fit_limits(lim: &Limits, cap: usize) -> Limits {
    let max_head = if lim.max_head < cap { lim.max_head } else { cap };
    let rest = cap - max_head;
    let rest32 = if rest < 4294967295 { rest as u32 } else { 4294967295 };
    let max_body = if lim.max_body < rest32 { lim.max_body } else { rest32 };
    Limits { max_head, max_body }
}

/// Whether the connection closes after this request: HTTP/1.0, the
/// `max_requests`-th request (`served` counts it), or a `Connection` field
/// that lists `close`.
pub fn must_close(buf: &[u8], head: &Head, served: u32, max_requests: u32) -> bool {
    head.minor == 0 || served >= max_requests || wants_close(buf, head)
}

/// Whether any `Connection` field of `head` lists the `close` option
/// (RFC 9110 §7.6.1: comma-separated, case-insensitive, OWS around each).
/// Spans outside `buf` count as no match.
pub fn wants_close(buf: &[u8], head: &Head) -> bool {
    let mut i = 0;
    while i < head.count && i < MAX_HEADERS {
        let f = head.headers[i];
        let named = f.name.end <= buf.len() && eq_ci(buf, f.name.start, f.name.end, b"connection");
        if named && lists_close(buf, f.value.start, f.value.end) {
            return true;
        }
        i += 1;
    }
    false
}

/// `buf[s..e]`, split at commas, has an element that is `close` once
/// trimmed of OWS. False if the span is not inside `buf`.
fn lists_close(buf: &[u8], s: usize, e: usize) -> bool {
    if s > e || e > buf.len() {
        return false;
    }
    let mut a = s;
    let mut k = s;
    while k < e {
        if buf[k] == 44 {
            if is_close(buf, a, k) {
                return true;
            }
            a = k + 1;
        }
        k += 1;
    }
    is_close(buf, a, e)
}

/// `buf[s..e]` is `close` once trimmed of OWS (`s <= e <= buf.len()`).
fn is_close(buf: &[u8], s: usize, e: usize) -> bool {
    let mut a = s;
    while a < e && is_ows(buf[a]) {
        a += 1;
    }
    let mut b = e;
    while b > a && is_ows(buf[b - 1]) {
        b -= 1;
    }
    eq_ci(buf, a, b, b"close")
}

// ---------------------------------------------------------------------------
// Response heads

/// Copy `src` into `out` at `pos`, or `None` if it does not fit.
fn put(out: &mut [u8], pos: usize, src: &[u8]) -> Option<usize> {
    put_n(out, pos, src, src.len())
}

/// One byte.
fn put1(out: &mut [u8], pos: usize, b: u8) -> Option<usize> {
    if pos >= out.len() {
        return None;
    }
    out[pos] = b;
    Some(pos + 1)
}

const POW10: [u32; 10] = [1_000_000_000, 100_000_000, 10_000_000, 1_000_000, 100_000, 10_000, 1_000, 100, 10, 1];

/// `n` in decimal, without leading zeros, at the start of `buf`; the number
/// of digits.
fn dec_digits(n: u32, buf: &mut [u8; 10]) -> usize {
    let mut c = 0;
    let mut k = 0;
    while c < 10 {
        let d = (n / POW10[c]) % 10;
        if d != 0 || k > 0 || c == 9 {
            buf[k] = 48 + d as u8;
            k += 1;
        }
        c += 1;
    }
    k
}

/// The reason phrase sent with `status` at `pos`; none for codes without one
/// here (RFC 9112 §4 allows an empty reason).
fn put_reason(out: &mut [u8], pos: usize, status: u16) -> Option<usize> {
    match status {
        200 => put(out, pos, b"OK"),
        201 => put(out, pos, b"Created"),
        204 => put(out, pos, b"No Content"),
        400 => put(out, pos, b"Bad Request"),
        401 => put(out, pos, b"Unauthorized"),
        403 => put(out, pos, b"Forbidden"),
        404 => put(out, pos, b"Not Found"),
        405 => put(out, pos, b"Method Not Allowed"),
        408 => put(out, pos, b"Request Timeout"),
        409 => put(out, pos, b"Conflict"),
        413 => put(out, pos, b"Content Too Large"),
        414 => put(out, pos, b"URI Too Long"),
        422 => put(out, pos, b"Unprocessable Content"),
        431 => put(out, pos, b"Request Header Fields Too Large"),
        500 => put(out, pos, b"Internal Server Error"),
        501 => put(out, pos, b"Not Implemented"),
        503 => put(out, pos, b"Service Unavailable"),
        505 => put(out, pos, b"HTTP Version Not Supported"),
        _ => Some(pos),
    }
}

/// `n` in decimal at `pos`.
fn put_dec(out: &mut [u8], pos: usize, n: u32) -> Option<usize> {
    let mut digits = [0u8; 10];
    let nd = dec_digits(n, &mut digits);
    put_n(out, pos, &digits, nd)
}

/// Copy the first `n` bytes of `src` into `out` at `pos`.
fn put_n(out: &mut [u8], pos: usize, src: &[u8], n: usize) -> Option<usize> {
    if n > src.len() || pos > out.len() || n > out.len() - pos {
        return None;
    }
    let mut q = 0;
    while q < n {
        out[pos + q] = src[q];
        q += 1;
    }
    Some(pos + n)
}

/// Whether a response with `status` carries a body (and `Content-Length`):
/// not 1xx, 204 or 304.
pub fn has_body(status: u16) -> bool {
    if status < 200 { false } else { status != 204 && status != 304 }
}

/// Write a response head into `out`:
///
/// ```text
/// HTTP/1.1 <status> <reason>CRLF
/// Content-Type: <content_type>CRLF        (omitted when empty)
/// Content-Length: <body_len>CRLF           (omitted for 1xx, 204 and 304)
/// Connection: close CRLF                   (when close)
/// CRLF
/// ```
///
/// Returns the head's length, or `None` if `out` is too small, `status` is
/// not in 100..=599, or `content_type` holds a byte that is not a field-value
/// byte (CR, LF, NUL and other controls), so the app cannot split the
/// response. `Content-Length` always comes from `body_len`; a 1xx, 204 or 304
/// response has no body (RFC 9112 §6.3), so it gets none and the caller must
/// send no body bytes ([`has_body`]).
pub fn write_head(out: &mut [u8], status: u16, content_type: &[u8], body_len: u32, close: bool) -> Option<usize> {
    let at = match write_head_open(out, status, content_type, body_len, close) {
        None => return None,
        Some(a) => a,
    };
    end_head(out, at)
}

/// [`write_head`] without the final CRLF: [`write_field`] may add fields at
/// the position it returns, and [`end_head`] ends the head.
pub fn write_head_open(out: &mut [u8], status: u16, content_type: &[u8], body_len: u32, close: bool) -> Option<usize> {
    if status < 100 || status > 599 {
        return None;
    }
    if !field_bytes(content_type, 0, content_type.len()) {
        return None;
    }
    let ct = content_type.len() > 0;
    let cl = has_body(status);
    let mut at = match put(out, 0, b"HTTP/1.1 ") {
        None => return None,
        Some(a) => a,
    };
    at = match put1(out, at, 48 + (status / 100) as u8) {
        None => return None,
        Some(a) => a,
    };
    at = match put1(out, at, 48 + ((status / 10) % 10) as u8) {
        None => return None,
        Some(a) => a,
    };
    at = match put1(out, at, 48 + (status % 10) as u8) {
        None => return None,
        Some(a) => a,
    };
    at = match put1(out, at, SP) {
        None => return None,
        Some(a) => a,
    };
    at = match put_reason(out, at, status) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, b"\r\n") {
        None => return None,
        Some(a) => a,
    };
    at = match put_if(out, at, ct, b"Content-Type: ") {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, content_type) {
        None => return None,
        Some(a) => a,
    };
    at = match put_if(out, at, ct, b"\r\n") {
        None => return None,
        Some(a) => a,
    };
    at = match put_if(out, at, cl, b"Content-Length: ") {
        None => return None,
        Some(a) => a,
    };
    at = match put_dec_if(out, at, cl, body_len) {
        None => return None,
        Some(a) => a,
    };
    at = match put_if(out, at, cl, b"\r\n") {
        None => return None,
        Some(a) => a,
    };
    put_if(out, at, close, b"Connection: close\r\n")
}

/// `src` at `pos` when `cond`, else nothing. Keeps `write_head_open` one
/// straight chain of writes.
fn put_if(out: &mut [u8], pos: usize, cond: bool, src: &[u8]) -> Option<usize> {
    if cond { put(out, pos, src) } else { Some(pos) }
}

/// `n` in decimal at `pos` when `cond`, else nothing.
fn put_dec_if(out: &mut [u8], pos: usize, cond: bool, n: u32) -> Option<usize> {
    if cond { put_dec(out, pos, n) } else { Some(pos) }
}


/// A field the shell writes itself, or one that would change how the
/// response is framed or what happens to the connection: `content-length`,
/// `transfer-encoding`, `content-type`, `connection`, `trailer`, `upgrade`
/// (any case). One test each, so the extracted Lean stays small.
pub fn reserved_name(name: &[u8]) -> bool {
    let n = name.len();
    if eq_ci(name, 0, n, b"content-length") {
        return true;
    }
    if eq_ci(name, 0, n, b"transfer-encoding") {
        return true;
    }
    if eq_ci(name, 0, n, b"content-type") {
        return true;
    }
    if eq_ci(name, 0, n, b"connection") {
        return true;
    }
    if eq_ci(name, 0, n, b"trailer") {
        return true;
    }
    eq_ci(name, 0, n, b"upgrade")
}

/// Every byte of `buf` is a `tchar`, and there is at least one.
fn is_token(buf: &[u8]) -> bool {
    buf.len() > 0 && token_end(buf, 0, buf.len()) == buf.len()
}

/// Write the field `name: value` CRLF at `pos`; the position after it.
///
/// `None` if it does not fit, `name` is not a token or is
/// [`reserved_name`], or `value` holds a byte that is not a field-value
/// byte, or starts or ends with SP or HTAB. So an extra field can neither
/// end the head early, add a field of its own, nor change the framing.
pub fn write_field(out: &mut [u8], pos: usize, name: &[u8], value: &[u8]) -> Option<usize> {
    if !is_token(name) {
        return None;
    }
    if reserved_name(name) {
        return None;
    }
    if !field_bytes(value, 0, value.len()) {
        return None;
    }
    if value.len() > 0 {
        if is_ows(value[0]) {
            return None;
        }
        if is_ows(value[value.len() - 1]) {
            return None;
        }
    }
    let mut at = match put(out, pos, name) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, b": ") {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, value) {
        None => return None,
        Some(a) => a,
    };
    put(out, at, b"\r\n")
}

/// The CRLF that ends a head, at `pos`.
pub fn end_head(out: &mut [u8], pos: usize) -> Option<usize> {
    put(out, pos, b"\r\n")
}

#[cfg(test)]
mod tests;
