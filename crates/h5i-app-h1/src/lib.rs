//! A strict HTTP/1.1 request-head parser, chunked body decoder and response
//! head writer for devices with a few hundred KB of RAM.
//!
//! `#![no_std]`, no `alloc`, no dependencies. Every function is total and
//! written in the Rust subset Charon and Aeneas translate to Lean; the proofs
//! in `proofs/` are about this code as extracted
//! (`docs/design/design-app-embedded.md`). Results are offsets into the
//! caller's buffer, never borrows.
//!
//! The accepted grammar is a strict subset of RFC 9112: CRLF line ends only,
//! no obs-fold, no whitespace before a colon, origin-form targets, exactly one
//! `Host` on HTTP/1.1, at most one `Content-Length` (digits only) and at most
//! one `Transfer-Encoding` (exactly `chunked`), never both.

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

/// A byte of an origin-form target: visible ASCII.
pub fn is_target_byte(b: u8) -> bool {
    b >= 33 && b <= 126
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

/// First index in `[i, end)` whose byte is not a target byte, or `end`.
fn target_end(buf: &[u8], i: usize, end: usize) -> usize {
    let mut k = i;
    while k < end {
        if !is_target_byte(buf[k]) {
            return k;
        }
        k += 1;
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

/// `method SP origin-form SP HTTP-version` in `buf[i..j]`.
fn request_line(buf: &[u8], i: usize, j: usize) -> ReqLine {
    let m = token_end(buf, i, j);
    if m == i || m >= j {
        return ReqLine::Bad(BAD_REQUEST);
    }
    if buf[m] != SP {
        return ReqLine::Bad(BAD_REQUEST);
    }
    let ts = m + 1;
    let te = target_end(buf, ts, j);
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
        if g.host {
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
    if status < 100 || status > 599 {
        return None;
    }
    if !field_bytes(content_type, 0, content_type.len()) {
        return None;
    }
    let ct = content_type.len() > 0;
    let ct_name: &[u8] = if ct { b"Content-Type: " } else { b"" };
    let ct_end: &[u8] = if ct { b"\r\n" } else { b"" };
    let conn: &[u8] = if close { b"Connection: close\r\n" } else { b"" };
    let cl = has_body(status);
    let cl_name: &[u8] = if cl { b"Content-Length: " } else { b"" };
    let cl_end: &[u8] = if cl { b"\r\n" } else { b"" };
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
    at = match put(out, at, ct_name) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, content_type) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, ct_end) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, cl_name) {
        None => return None,
        Some(a) => a,
    };
    if cl {
        at = match put_dec(out, at, body_len) {
            None => return None,
            Some(a) => a,
        };
    }
    at = match put(out, at, cl_end) {
        None => return None,
        Some(a) => a,
    };
    at = match put(out, at, conn) {
        None => return None,
        Some(a) => a,
    };
    put(out, at, b"\r\n")
}

#[cfg(test)]
mod tests;
