# h5i-app-embedded

An async, `no_std`, heap-free HTTP/1.1 server for h5i-app kernels on
microcontrollers. It is the trusted shell around the verified
[`h5i-app-h1`](../h5i-app-h1) core.

The receive loop is h1's `recv_step` and `carry`. `recv_step` parses the
head, applies the body framing, decodes chunked bodies in place and answers
400, 413 or 431; `carry` finds where the next pipelined request starts. In
that loop the shell only reads: when asked, it reads into `rx[filled..]` and
adds the count to `filled`. Outside it, the shell routes and writes
response heads with `write_head_with` (status line, framing fields, the
extra fields through `write_field`, CRLF).

```rust
let k = Calls::<Thermostat, Wire, App>::new();
let router = Router::new()
    .get(b"/state", k.call(guest, |_| Ok(Command::Read)))
    .put(b"/mode", k.call(admin, decode_mode));
let mut rx = [0u8; 2048];
let mut tx = [0u8; 1024];
serve(&Config::default(), &router, &app, &mut rx, &mut tx, &mut io, &bound).await?;
```

## Modules

| Module | Feature | What it is |
|---|---|---|
| (crate root) | | `serve`, `Router`, `Request`, `Response` |
| `form` | | query and form pairs, percent-decoding, decimal parsing |
| `json` | | a JSON writer and a strict flat-object reader |
| `kernel` | | runs an h5i-app `Kernel` on the device |
| `hmac` | `hmac` | signed, replay-protected requests |
| `store` | `flash` | snapshots in NOR flash that survive a reset |
| `tcp` | `embassy-net` | an embassy-net TCP listener |
| `link` | | connections over a byte link such as a UART |

## Serving

I/O is `embedded_io_async::{Read, Write}`, so the server runs over tokio
(through `embedded-io-adapters`), embassy-net or a UART.

The crate has no clock. The caller passes a `Bound`, such as embassy-time's
`with_timeout` or tokio's `timeout`, and it limits how long each request may
take to arrive and each response to leave. A request that started and did
not finish in time gets a 408. Use `NoBound` for I/O that times out by
itself.

Routes form a tuple-typed chain (`Router<Route<H, Route<…, Nil>>>`), with no
`dyn` and no allocation. A pattern is made of literal segments and `{name}`
segments.

A handler is an `async fn(&Request<'_>, &S, &mut [u8]) -> Response`. It
writes the body at the start of the slice and returns its length, and
`Content-Length` always comes from that length.

`Response::header` adds up to four fields. Each goes through the verified
`write_field`, which refuses:

- the framing names `Content-Length`, `Transfer-Encoding`, `Connection`,
  `Content-Type`, `Trailer` and `Upgrade`;
- names that are not tokens;
- values with control characters.

A refused field turns the response into an empty 500 and closes the
connection.

A few HTTP rules apply to every route. A HEAD request runs the GET handler
and sends no body. Responses with status 1xx, 204 or 304 carry no
`Content-Length` and no body. Any `Connection: close` token closes the
connection.

## Bodies (`form`, `json`)

`form` splits query strings and form bodies into pairs, decodes percent
escapes and parses decimals.

`json::Writer` writes through `h5i-app-json`'s proven slice writer.
`json::Object` reads one flat object and refuses nesting, duplicate keys,
bad escapes and invalid UTF-8.

## Running a kernel (`kernel`)

`Calls::call` binds each route to one command decoder, so a path produces
only its own decoder's commands.

`DeviceEngine` holds one tenant's snapshot. It runs commands the way
`MemoryEngine::execute_at` does (stamp, `transition`, `apply`), one write at
a time.

- A write is saved to the engine's `Store` before anyone can see it. If the
  save fails, nothing changes and the client gets 503.
- Device tasks see every commit through `DeviceEngine::changed`, and run
  their own commands with `execute`.

## Authentication

`PskAuth` (in `kernel`) checks a bearer key in constant time. The key
travels with every request, so use it only on a link nobody else can read.

`HmacAuth` (in `hmac`) verifies HMAC-SHA256 signatures. Each signature is
bound to a per-boot id and a strictly increasing counter. The key never
crosses the link, and a captured request cannot be replayed. It does not
encrypt anything and does not authenticate replies.

## Keeping state across resets (`store`)

`FlashStore` keeps snapshots in NOR flash, in two alternating regions with
a CRC per record. A power cut at any point leaves either the new snapshot or
the one before it. A save identical to the last one is skipped.

## Transports

`serve_tcp` (in `tcp`) accepts and serves connections on one embassy-net
port. Run several of them for concurrent connections. The firmware picks
embassy-net's IP and medium features.

`link` carries connections over a byte link that has none of its own, such
as a UART shared through a TCP bridge.

- `FLAG 'O'/'C' id` markers open and close each connection, and the data
  between them is escaped.
- `serve_link` gives each connection a fresh buffer and a time bound.
- A new connection aborts the old one.

The host side is `scripts/embedded/uart-gateway.py`.

## Memory

Memory per connection is `rx + tx` plus the handler's future, all fixed at
compile time. The receive buffer caps the parser's limits, so a whole
request always fits.

## Testing

This crate is trusted, not proven, so it relies on tests. `src/tests.rs`
covers:

- pipelining, and the same streams split into random reads;
- a smuggling corpus, oversized heads and truncation;
- slow clients;
- header refusals and route binding;
- signed and replayed requests;
- proptest fuzzing.

The `store` tests cut power at every byte, the `json` tests compare against
serde_json, and the `tcp` test joins two embassy-net stacks over an
in-memory link.

The examples in [`examples/embedded`](../../examples/embedded) run the
server on a host and in Renode.
