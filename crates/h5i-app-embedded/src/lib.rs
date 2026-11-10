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
//! (`embedded-io-adapters`), embassy-net TCP or a UART. The shell has no
//! clock: the caller passes a [`Bound`] (embassy-time's `with_timeout`,
//! tokio's `timeout`) that limits how long each request may take to arrive
//! and each response to leave, so a client that trickles bytes cannot hold a
//! connection.

#![no_std]
// Handlers run on a single-threaded executor (embassy, a tokio LocalSet), so
// their futures need not be `Send`.
#![allow(async_fn_in_trait)]

// Tests run on a host; tokio's macros name `::std`.
#[cfg(test)]
extern crate std;

pub mod form;
#[cfg(feature = "hmac")]
pub mod hmac;
pub mod json;
pub mod kernel;
pub mod link;
mod path;
#[cfg(feature = "flash")]
pub mod store;
#[cfg(feature = "embassy-net")]
pub mod tcp;
#[cfg(test)]
mod tests;

use embedded_io_async::{Read, Write};
use h5i_app_h1::{self as h1, BodyOut, HeadResult};
pub use h5i_app_h1::{Head, Limits};

/// Server limits.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// `max_head` and `max_body` for the verified parser. The receive buffer
    /// passed to [`serve`] bounds both further: a request's head and body
    /// must fit in it together.
    pub limits: Limits,
    /// Requests served on one connection before it is closed.
    pub max_requests: u32,
    /// Bytes reserved at the start of the transmit buffer for the response
    /// head; the handler's body goes after them. A head that does not fit
    /// (too many or too long [`Response::header`]s) becomes an empty 500.
    pub head_room: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { limits: Limits { max_head: 1024, max_body: 1024 }, max_requests: 100, head_room: HEAD_ROOM }
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
        form::parse_u32(self.param(i)?)
    }

    /// The value of `name` in the query (`a=1&b=2`), undecoded; see [`form`].
    pub fn query_param(&self, name: &[u8]) -> Option<&'r [u8]> {
        form::param(self.query, name)
    }

    /// The method as sent.
    pub fn method_bytes(&self) -> &'r [u8] {
        &self.raw[self.head.method.start..self.head.method.end]
    }

    /// The request target as sent: the path and, after `?`, the query.
    pub fn target(&self) -> &'r [u8] {
        &self.raw[self.head.target.start..self.head.target.end]
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

/// Extra response fields a handler may add ([`Response::header`]).
pub const MAX_FIELDS: usize = 4;
/// Bytes [`Response::header_copy`] may copy, over all its fields.
pub const INLINE_VALUE: usize = 64;

/// A field value: a constant, or a span of the response's inline bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Static(&'static [u8]),
    Inline(u8, u8),
}

/// What a handler answers: the status, a content type, how many bytes of
/// body it wrote at the start of its output buffer, and up to
/// [`MAX_FIELDS`] extra fields.
///
/// The extra fields go through the verified `write_field`, which refuses a
/// name that is not a token, a framing name (`Content-Length`,
/// `Transfer-Encoding`, `Connection`, `Content-Type`, `Trailer`, `Upgrade`)
/// and a value with CR, LF or other controls. A refused field, one field too
/// many or copied values over [`INLINE_VALUE`] bytes turn the response into
/// an empty 500 that closes the connection, as a bad content type does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static [u8],
    pub len: usize,
    /// Close the connection after this response.
    pub close: bool,
    fields: [(&'static [u8], Value); MAX_FIELDS],
    /// How many of `fields` are used; `u8::MAX` once one did not fit.
    nfields: u8,
    inline: [u8; INLINE_VALUE],
    ninline: u8,
}

const NO_FIELD: (&[u8], Value) = (b"", Value::Static(b""));

impl Response {
    /// `len` bytes of body with this status and content type.
    pub const fn new(status: u16, content_type: &'static [u8], len: usize) -> Response {
        Response {
            status,
            content_type,
            len,
            close: false,
            fields: [NO_FIELD; MAX_FIELDS],
            nfields: 0,
            inline: [0; INLINE_VALUE],
            ninline: 0,
        }
    }

    /// No body.
    pub const fn status(status: u16) -> Response {
        Response::new(status, b"", 0)
    }

    /// `body` copied into `out`; 500 with no body if it does not fit.
    pub fn bytes(out: &mut [u8], status: u16, content_type: &'static [u8], body: &[u8]) -> Response {
        if body.len() > out.len() {
            return Response::status(500);
        }
        out[..body.len()].copy_from_slice(body);
        Response::new(status, content_type, body.len())
    }

    pub fn text(out: &mut [u8], status: u16, body: &[u8]) -> Response {
        Response::bytes(out, status, b"text/plain", body)
    }

    pub fn json(out: &mut [u8], status: u16, body: &[u8]) -> Response {
        Response::bytes(out, status, b"application/json", body)
    }

    /// Close the connection after this response.
    pub fn closing(mut self) -> Response {
        self.close = true;
        self
    }

    /// Add the field `name: value`.
    pub fn header(self, name: &'static [u8], value: &'static [u8]) -> Response {
        self.field(name, Value::Static(value))
    }

    /// Add the field `name: value`, copying `value` into the response.
    pub fn header_copy(mut self, name: &'static [u8], value: &[u8]) -> Response {
        let at = self.ninline as usize;
        if value.len() > INLINE_VALUE - at {
            return self.overflow();
        }
        self.inline[at..at + value.len()].copy_from_slice(value);
        self.ninline += value.len() as u8;
        self.field(name, Value::Inline(at as u8, value.len() as u8))
    }

    fn field(mut self, name: &'static [u8], value: Value) -> Response {
        let n = self.nfields as usize;
        if n >= MAX_FIELDS {
            return self.overflow();
        }
        self.fields[n] = (name, value);
        self.nfields += 1;
        self
    }

    fn overflow(mut self) -> Response {
        self.nfields = u8::MAX;
        self
    }

    /// Whether every added field fit.
    pub fn fields_fit(&self) -> bool {
        self.nfields as usize <= MAX_FIELDS
    }

    /// The extra fields that fit, in order.
    pub fn fields(&self) -> impl Iterator<Item = (&'static [u8], &[u8])> + '_ {
        let n = (self.nfields as usize).min(MAX_FIELDS);
        self.fields[..n].iter().map(move |&(name, v)| match v {
            Value::Static(b) => (name, b),
            Value::Inline(s, l) => (name, &self.inline[s as usize..s as usize + l as usize]),
        })
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

    pub fn patch<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Patch, handler)
    }

    pub fn delete<H>(self, pattern: &'static [u8], handler: H) -> Router<Route<H, R>> {
        self.route(pattern, Method::Delete, handler)
    }
}

/// The default [`Config::head_room`]: the status line, the framing fields
/// and a few extra fields.
pub const HEAD_ROOM: usize = 256;

/// Runs a future under a time bound. The device provides it (embassy-time's
/// `with_timeout`, tokio's `timeout`), since the shell has no clock.
pub trait Bound {
    /// `None` if `f` did not finish in time; it is then dropped.
    async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output>;
}

/// No bound: for an I/O object that times out by itself, or for tests.
pub struct NoBound;

impl Bound for NoBound {
    async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
        Some(f.await)
    }
}

/// Why [`serve`] stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Closed<E> {
    /// The peer closed the connection, or we did after a `close` response.
    Done {
        requests: u32,
    },
    Io(E),
    /// `tx` is not longer than [`Config::head_room`].
    TxTooSmall,
    /// A response did not leave within the bound.
    WriteTimeout,
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
///
/// Each request must arrive whole within `bound`, counted from when the
/// server starts waiting for it (so an idle keep-alive connection is closed
/// after one bound), and each response must be written within `bound`. A
/// request that started but did not finish in time gets a 408 and the
/// connection closes. The handler itself is not bounded.
pub async fn serve<IO, S, R, B>(
    cfg: &Config,
    router: &Router<R>,
    state: &S,
    rx: &mut [u8],
    tx: &mut [u8],
    io: &mut IO,
    bound: &B,
) -> Result<u32, Closed<IO::Error>>
where
    IO: Read + Write,
    R: Routes<S>,
    B: Bound,
{
    let room = cfg.head_room;
    if tx.len() <= room {
        return Err(Closed::TxTooSmall);
    }
    // The receive buffer bounds the limits: a whole request must fit.
    let max_head = cfg.limits.max_head.min(rx.len());
    let rest = (rx.len() - max_head).min(u32::MAX as usize) as u32;
    let lim = Limits { max_head, max_body: cfg.limits.max_body.min(rest) };
    let mut filled = 0usize;
    let mut served = 0u32;
    loop {
        let next = match bound.within(read_request(&lim, rx, &mut filled, io)).await {
            Some(Ok(n)) => n,
            Some(Err(e)) => return Err(Closed::Io(e)),
            // Idle: close quietly. Part of a request: 408.
            None if filled == 0 => return Ok(served),
            None => Next::Reject(408),
        };
        let (head, body_start, body_end, used) = match next {
            Next::Eof => return Ok(served),
            Next::Reject(status) => {
                send(io, tx, room, Response::status(status).closing(), false, bound).await?;
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
            let (_, body_out) = tx.split_at_mut(room);
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
        if send(io, tx, room, resp, head_only, bound).await? {
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
    // The verified `read_body` decides where the body and the message end;
    // this loop only appends bytes at `filled`.
    let start = head.len;
    let mut st = h1::body_start(start);
    loop {
        let (s, out) = h1::read_body(rx, *filled, start, head.framing, st, lim.max_body);
        st = s;
        match out {
            BodyOut::Done(end, used) => return Ok(Next::Ready(head, start, end, used)),
            BodyOut::Reject(s) => return Ok(Next::Reject(s)),
            BodyOut::More => {
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

/// The head of `resp` in `head`: the verified writer's status line and
/// framing fields, then each extra field through the verified
/// `write_field`. `None` if anything was refused or did not fit.
fn write_response_head(head: &mut [u8], resp: &Response, body_len: u32) -> Option<usize> {
    if resp.status < 200 {
        return None;
    }
    if !resp.fields_fit() {
        return None;
    }
    let mut at = h1::write_head_open(head, resp.status, resp.content_type, body_len, resp.close)?;
    for (name, value) in resp.fields() {
        at = h1::write_field(head, at, name, value)?;
    }
    h1::end_head(head, at)
}

/// Write `resp`, whose body is at `tx[room..]`; whether the connection
/// must now close. No body bytes go out for a HEAD request (`head_only`) or a
/// status that has none (1xx, 204, 304). A 1xx from a handler, a status out
/// of range, a bad content type or a refused extra field become an empty 500
/// that closes.
async fn send<IO: Write, B: Bound>(
    io: &mut IO,
    tx: &mut [u8],
    room: usize,
    resp: Response,
    head_only: bool,
    bound: &B,
) -> Result<bool, Closed<IO::Error>> {
    let (head, body) = tx.split_at_mut(room);
    let len = resp.len.min(body.len());
    let blen = len.min(u32::MAX as usize) as u32;
    let (n, body_len, close) = match write_response_head(head, &resp, blen) {
        Some(n) => {
            let send_body = !head_only && h1::has_body(resp.status);
            (n, if send_body { len } else { 0 }, resp.close)
        }
        None => match h1::write_head(head, 500, b"", 0, true) {
            Some(n) => (n, 0, true),
            None => return Ok(true),
        },
    };
    let write = async {
        io.write_all(&head[..n]).await?;
        io.write_all(&body[..body_len]).await?;
        io.flush().await
    };
    match bound.within(write).await {
        Some(r) => r.map_err(Closed::Io)?,
        None => return Err(Closed::WriteTimeout),
    }
    Ok(close)
}
