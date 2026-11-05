//! An async, `no_std`, heap-free HTTP/1.1 server for h5i-app kernels.
//!
//! The byte-level work is done by [`h5i_app_h1`], which is extracted to Lean
//! and proven (panic freedom, the parser's spec, well-formedness of what it
//! accepts, the read bound). This crate is the trusted shell around it: it
//! reads from a socket into a caller-provided buffer, calls the verified
//! parser and chunked decoder, routes the request to a handler, and writes
//! the response head with the verified writer. It allocates nothing; every
//! buffer is the caller's.
//!
//! ```ignore
//! let router = Router::new()
//!     .get(b"/status", status)
//!     .post(b"/relay/{id}", relay);
//! let mut rx = [0u8; 2048];
//! let mut tx = [0u8; 1024];
//! serve(&Config::default(), &router, &state, &mut rx, &mut tx, &mut socket).await?;
//!
//! async fn relay(req: &Request<'_>, st: &State, out: &mut [u8]) -> Response {
//!     let Some(id) = req.param_u32(0) else { return Response::status(404) };
//!     ...
//! }
//! ```
//!
//! I/O is [`embedded_io_async`], so the same code runs over tokio on a host
//! (`embedded-io-adapters`), embassy-net TCP or a UART. Timeouts belong to
//! the I/O object (embassy-net's `set_timeout`, `tokio::time::timeout` on a
//! host); the shell has no clock.

#![no_std]
// Handlers run on a single-threaded executor (embassy, a tokio LocalSet), so
// their futures need not be `Send`.
#![allow(async_fn_in_trait)]

pub mod kernel;
pub mod link;
mod path;
#[cfg(test)]
mod tests;

pub use h5i_app_h1::{Head, Limits};
use h5i_app_h1::{self as h1, ChunkOut, Framing, HeadResult};
use embedded_io_async::{Read, Write};

/// Server limits.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// `max_head` and `max_body` for the verified parser. The receive buffer
    /// passed to [`serve`] bounds both further: a request's head and body
    /// must fit in it together.
    pub limits: Limits,
    /// Requests served on one connection before it is closed.
    pub max_requests: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { limits: Limits { max_head: 1024, max_body: 1024 }, max_requests: 100 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Other,
}

impl Method {
    fn from_bytes(m: &[u8]) -> Method {
        match m {
            b"GET" => Method::Get,
            b"HEAD" => Method::Head,
            b"POST" => Method::Post,
            b"PUT" => Method::Put,
            b"PATCH" => Method::Patch,
            b"DELETE" => Method::Delete,
            _ => Method::Other,
        }
    }
}

/// Path parameters (`{name}` segments), as spans of the path.
pub const MAX_PARAMS: usize = 4;

/// A request: views into the receive buffer.
pub struct Request<'r> {
    pub method: Method,
    /// The target before `?`.
    pub path: &'r [u8],
    /// The target after `?`, without it; empty if there is none.
    pub query: &'r [u8],
    /// The body: `Content-Length` bytes, or the decoded chunks.
    pub body: &'r [u8],
    head: Head,
    raw: &'r [u8],
    params: [(usize, usize); MAX_PARAMS],
    nparams: usize,
}

impl<'r> Request<'r> {
    /// The first field named `name` (ASCII case-insensitive), without
    /// surrounding whitespace.
    pub fn header(&self, name: &[u8]) -> Option<&'r [u8]> {
        let raw = self.raw;
        self.head.headers[..self.head.count]
            .iter()
            .find(|h| raw[h.name.start..h.name.end].eq_ignore_ascii_case(name))
            .map(|h| &raw[h.value.start..h.value.end])
    }

    /// Every field, in order.
    pub fn headers(&self) -> impl Iterator<Item = (&'r [u8], &'r [u8])> + '_ {
        let raw = self.raw;
        self.head.headers[..self.head.count]
            .iter()
            .map(move |h| (&raw[h.name.start..h.name.end], &raw[h.value.start..h.value.end]))
    }

    /// The `i`th `{param}` of the matched route.
    pub fn param(&self, i: usize) -> Option<&'r [u8]> {
        if i < self.nparams {
            let (s, e) = self.params[i];
            Some(&self.path[s..e])
        } else {
            None
        }
    }

    /// The `i`th `{param}` as a decimal `u32` (no sign, no leading `+`).
    pub fn param_u32(&self, i: usize) -> Option<u32> {
        let p = self.param(i)?;
        if p.is_empty() || p.len() > 10 {
            return None;
        }
        let mut v: u64 = 0;
        for &c in p {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as u64;
        }
        u32::try_from(v).ok()
    }

    /// `HTTP/1.<minor>`.
    pub fn minor_version(&self) -> u8 {
        self.head.minor
    }

    /// The parsed head (offsets into the request's bytes).
    pub fn head(&self) -> &Head {
        &self.head
    }
}

/// What a handler answers: the status, a content type, and how many bytes of
/// body it wrote at the start of its output buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static [u8],
    pub len: usize,
    /// Close the connection after this response.
    pub close: bool,
}

impl Response {
    /// No body.
    pub fn status(status: u16) -> Response {
        Response { status, content_type: b"", len: 0, close: false }
    }

    /// `body` copied into `out`; 500 with no body if it does not fit.
    pub fn bytes(out: &mut [u8], status: u16, content_type: &'static [u8], body: &[u8]) -> Response {
        if body.len() > out.len() {
            return Response::status(500);
        }
        out[..body.len()].copy_from_slice(body);
        Response { status, content_type, len: body.len(), close: false }
    }

    pub fn text(out: &mut [u8], status: u16, body: &[u8]) -> Response {
        Response::bytes(out, status, b"text/plain", body)
    }

    pub fn json(out: &mut [u8], status: u16, body: &[u8]) -> Response {
        Response::bytes(out, status, b"application/json", body)
    }
}

/// A request handler. Implemented for every
/// `async fn(&Request<'_>, &S, &mut [u8]) -> Response`: the handler writes
/// its body at the start of the slice and says how long it is.
pub trait Handler<S> {
    async fn call(&self, req: &Request<'_>, state: &S, out: &mut [u8]) -> Response;
}

impl<S, F> Handler<S> for F
where
    F: AsyncFn(&Request<'_>, &S, &mut [u8]) -> Response,
{
    async fn call(&self, req: &Request<'_>, state: &S, out: &mut [u8]) -> Response {
        self(req, state, out).await
    }
}

/// The result of routing.
pub enum Dispatch {
    Done(Response),
    /// A route matched the path, none the method: 405.
    WrongMethod,
    NotFound,
}

/// A chain of routes, typed as a tuple list so routing allocates nothing
/// and dispatches statically.
pub trait Routes<S> {
    async fn dispatch(&self, req: &mut Request<'_>, state: &S, out: &mut [u8]) -> Dispatch;
}

/// No routes.
pub struct Nil;

impl<S> Routes<S> for Nil {
    async fn dispatch(&self, _: &mut Request<'_>, _: &S, _: &mut [u8]) -> Dispatch {
        Dispatch::NotFound
    }
}

/// One route, after the routes in `next` (which are tried first, so routes
/// match in the order they were added).
pub struct Route<H, N> {
    pattern: &'static [u8],
    method: Method,
    handler: H,
    next: N,
}

impl<S, H: Handler<S>, N: Routes<S>> Routes<S> for Route<H, N> {
    async fn dispatch(&self, req: &mut Request<'_>, state: &S, out: &mut [u8]) -> Dispatch {
        let earlier = self.next.dispatch(req, state, out).await;
        if let Dispatch::Done(_) = earlier {
            return earlier;
        }
        match path::matches(self.pattern, req.path, &mut req.params) {
            None => earlier,
            Some(n) => {
                // A HEAD request runs the GET handler; its body is not sent.
                let head_as_get = req.method == Method::Head && self.method == Method::Get;
                if self.method != req.method && !head_as_get {
                    return Dispatch::WrongMethod;
                }
                req.nparams = n;
                Dispatch::Done(self.handler.call(req, state, out).await)
            }
        }
    }
}

/// Routes by path pattern (`/relay/{id}`: literal segments and `{name}`
/// segments that match one non-empty segment) and method.
pub struct Router<R>(R);

impl Router<Nil> {
    pub fn new() -> Self {
        Router(Nil)
    }
}

impl Default for Router<Nil> {
    fn default() -> Self {
        Router::new()
    }
}

impl<R> Router<R> {
    pub fn route<H>(self, pattern: &'static [u8], method: Method, handler: H) -> Router<Route<H, R>> {
        Router(Route { pattern, method, handler, next: self.0 })
    }

    pub fn get<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Get, handler)
    }

    pub fn post<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Post, handler)
    }

    pub fn put<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Put, handler)
    }

    pub fn delete<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Delete, handler)
    }
}

/// Bytes reserved at the start of the transmit buffer for the response head.
/// The body is written after it.
pub const HEAD_ROOM: usize = 192;

/// Why [`serve`] stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Closed<E> {
    /// The peer closed the connection, or we did after a `close` response.
    Done { requests: u32 },
    Io(E),
    /// `tx` is not longer than [`HEAD_ROOM`].
    TxTooSmall,
}

/// What a connection's read loop decided about the next request.
enum Next {
    /// A request is ready: its head, the body span and the bytes it used.
    Ready(Head, usize, usize, usize),
    /// Answer with this status and close.
    Reject(u16),
    /// The peer closed the connection between requests.
    Eof,
}

/// Serve one connection until it closes. Returns how many requests it
/// answered.
pub async fn serve<IO, S, R>(
    cfg: &Config,
    router: &Router<R>,
    state: &S,
    rx: &mut [u8],
    tx: &mut [u8],
    io: &mut IO,
) -> Result<u32, Closed<IO::Error>>
where
    IO: Read + Write,
    R: Routes<S>,
{
    if tx.len() <= HEAD_ROOM {
        return Err(Closed::TxTooSmall);
    }
    // The receive buffer bounds the limits: a whole request must fit.
    let max_head = cfg.limits.max_head.min(rx.len());
    let room = (rx.len() - max_head).min(u32::MAX as usize) as u32;
    let lim = Limits { max_head, max_body: cfg.limits.max_body.min(room) };
    let mut filled = 0usize;
    let mut served = 0u32;
    loop {
        let next = match read_request(&lim, rx, &mut filled, io).await {
            Ok(n) => n,
            Err(e) => return Err(Closed::Io(e)),
        };
        let (head, body_start, body_end, used) = match next {
            Next::Eof => return Ok(served),
            Next::Reject(status) => {
                send(io, tx, Response { status, content_type: b"", len: 0, close: true }, false).await?;
                return Ok(served);
            }
            Next::Ready(h, s, e, u) => (h, s, e, u),
        };
        served += 1;
        let resp = {
            let raw = &rx[..used];
            let target = &raw[head.target.start..head.target.end];
            let (path, query) = match target.iter().position(|&c| c == b'?') {
                Some(q) => (&target[..q], &target[q + 1..]),
                None => (target, &target[..0]),
            };
            let mut req = Request {
                method: Method::from_bytes(&raw[head.method.start..head.method.end]),
                path,
                query,
                body: &raw[body_start..body_end],
                head,
                raw,
                params: [(0, 0); MAX_PARAMS],
                nparams: 0,
            };
            let close = head.minor == 0 || served >= cfg.max_requests || wants_close(&req);
            let (_, body_out) = tx.split_at_mut(HEAD_ROOM);
            let mut resp = match router.0.dispatch(&mut req, state, body_out).await {
                Dispatch::Done(r) => r,
                Dispatch::WrongMethod => Response::status(405),
                Dispatch::NotFound => Response::status(404),
            };
            if resp.len > body_out.len() {
                resp = Response::status(500);
            }
            resp.close |= close;
            (resp, req.method == Method::Head)
        };
        let (resp, head_only) = resp;
        if send(io, tx, resp, head_only).await? {
            return Ok(served);
        }
        // Pipelining: keep the bytes after this request.
        rx.copy_within(used..filled, 0);
        filled -= used;
    }
}

/// Read until `rx[..filled]` starts with a complete request, decoding a
/// chunked body in place.
async fn read_request<IO: Read>(
    lim: &Limits,
    rx: &mut [u8],
    filled: &mut usize,
    io: &mut IO,
) -> Result<Next, IO::Error> {
    let head = loop {
        match h1::parse_head(&rx[..*filled], lim) {
            HeadResult::Done(h) => break h,
            HeadResult::Reject(s) => return Ok(Next::Reject(s)),
            HeadResult::Incomplete => {
                // `parse_head` answers 431/414 itself once max_head bytes are in.
                let n = io.read(&mut rx[*filled..]).await?;
                if n == 0 {
                    return Ok(if *filled == 0 { Next::Eof } else { Next::Reject(400) });
                }
                *filled += n;
            }
        }
    };
    let start = head.len;
    match head.framing {
        Framing::Empty => Ok(Next::Ready(head, start, start, start)),
        Framing::Length(n) => {
            // n <= max_body <= rx.len() - max_head, and head.len <= max_head.
            let end = start + n as usize;
            while *filled < end {
                let k = io.read(&mut rx[*filled..]).await?;
                if k == 0 {
                    return Ok(Next::Reject(400));
                }
                *filled += k;
            }
            Ok(Next::Ready(head, start, end, end))
        }
        Framing::Chunked => {
            let mut st = h1::CHUNK_START;
            let mut r = start;
            let mut w = start;
            loop {
                while r < *filled {
                    let b = rx[r];
                    let (s, out) = h1::chunk_step(st, b, lim.max_body);
                    st = s;
                    r += 1;
                    match out {
                        ChunkOut::Nothing => {}
                        ChunkOut::Data => {
                            rx[w] = b;
                            w += 1;
                        }
                        ChunkOut::Done => return Ok(Next::Ready(head, start, w, r)),
                        ChunkOut::Reject(s) => return Ok(Next::Reject(s)),
                    }
                }
                if *filled == rx.len() {
                    // Framing bytes filled the buffer.
                    return Ok(Next::Reject(413));
                }
                let k = io.read(&mut rx[*filled..]).await?;
                if k == 0 {
                    return Ok(Next::Reject(400));
                }
                *filled += k;
            }
        }
    }
}

/// Whether any `Connection` field lists the `close` option (RFC 9110 §7.6.1:
/// a comma-separated list, names case-insensitive, possibly over several
/// fields).
pub fn wants_close(req: &Request<'_>) -> bool {
    req.headers()
        .filter(|(n, _)| n.eq_ignore_ascii_case(b"connection"))
        .any(|(_, v)| v.split(|&c| c == b',').any(|t| t.trim_ascii().eq_ignore_ascii_case(b"close")))
}

/// Write `resp`, whose body is at `tx[HEAD_ROOM..]`; whether the connection
/// must now close. No body bytes go out for a HEAD request (`head_only`) or a
/// status that has none (1xx, 204, 304). A 1xx from a handler, a status out
/// of range or a bad content type become an empty 500 that closes.
async fn send<IO: Write>(
    io: &mut IO,
    tx: &mut [u8],
    resp: Response,
    head_only: bool,
) -> Result<bool, Closed<IO::Error>> {
    let (head, body) = tx.split_at_mut(HEAD_ROOM);
    let len = resp.len.min(body.len());
    let blen = len.min(u32::MAX as usize) as u32;
    let written = if resp.status < 200 { None } else { h1::write_head(head, resp.status, resp.content_type, blen, resp.close) };
    let (n, body_len, close) = match written {
        Some(n) => {
            let send_body = !head_only && h1::has_body(resp.status);
            (n, if send_body { len } else { 0 }, resp.close)
        }
        None => match h1::write_head(head, 500, b"", 0, true) {
            Some(n) => (n, 0, true),
            None => return Ok(true),
        },
    };
    io.write_all(&head[..n]).await.map_err(Closed::Io)?;
    io.write_all(&body[..body_len]).await.map_err(Closed::Io)?;
    io.flush().await.map_err(Closed::Io)?;
    Ok(close)
}
