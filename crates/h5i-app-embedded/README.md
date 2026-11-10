# h5i-app-embedded

An async, `no_std`, heap-free HTTP/1.1 server for h5i-app kernels on
microcontrollers. It is the trusted shell around the verified
[`h5i-app-h1`](../h5i-app-h1) parser: it reads into a caller-provided buffer,
parses with `parse_head`, finds where the body and message end with
`read_body` (chunked bodies are decoded in place), routes, and writes
response heads with `write_head_open`, `write_field` and `end_head`.

```rust
let k = Calls::<Thermostat, Wire, App>::new();
let router = Router::new()
    .get(b"/state", k.call(guest, |_| Ok(Command::Read)))
    .put(b"/mode", k.call(admin, decode_mode));
let mut rx = [0u8; 2048];
let mut tx = [0u8; 1024];
serve(&Config::default(), &router, &app, &mut rx, &mut tx, &mut io, &bound).await?;
```

- I/O is `embedded_io_async::{Read, Write}`: tokio through
  `embedded-io-adapters`, embassy-net or a UART. The crate has no clock: the
  caller's `Bound` (embassy-time's `with_timeout`, tokio's `timeout`) limits
  each request's arrival and each response's write. A request that started
  and did not finish in time gets a 408. `NoBound` is for I/O that times out
  by itself.
- Routing is a tuple-typed chain (`Router<Route<H, Route<…, Nil>>>`), no `dyn`
  and no allocation. Patterns are literal segments and `{name}` segments.
- Handlers are `async fn(&Request<'_>, &S, &mut [u8]) -> Response`: they
  write the body at the start of the slice. `Content-Length` always comes
  from the length they return. `Response::header` adds up to 4 fields; each
  goes through the verified `write_field`, which refuses framing names
  (`Content-Length`, `Transfer-Encoding`, `Connection`, `Content-Type`,
  `Trailer`, `Upgrade`), non-token names and values with controls. A refused
  field is an empty 500 that closes.
- `form`: query and form pairs, percent-decoding, decimal parsing.
- `json`: `Writer` writes through `h5i-app-json`'s proven slice writer;
  `Object` reads one flat object strictly (no nesting, no duplicate keys,
  valid escapes and UTF-8).
- `kernel` binds an h5i-app `Kernel`. `Calls::call` binds each route to one
  command decoder, so a path produces only its own decoder's commands.
  `DeviceEngine` holds one tenant's snapshot and runs commands as
  `MemoryEngine::execute_at` does (stamp, `transition`, `apply`), one write
  at a time; a write is saved to the engine's `Store` before it is visible,
  and a failed save changes nothing (503). Device tasks see every commit
  through `DeviceEngine::changed` and run commands of their own with
  `execute`. `PskAuth` checks a bearer key in constant time; use it only on
  a link nobody else can read.
- `hmac` (feature `hmac`): `HmacAuth` verifies HMAC-SHA256 signatures bound
  to a per-boot id and a strictly increasing counter, so the key never
  crosses the link and a captured request cannot be replayed. It gives no
  confidentiality and does not authenticate replies.
- `store` (feature `flash`): `FlashStore` keeps snapshots in NOR flash in two
  alternating regions with a CRC per record; a power cut at any point leaves
  the new snapshot or the one before. Identical saves are skipped.
- `tcp` (feature `embassy-net`): `serve_tcp` accepts and serves connections
  on one embassy-net port; run several for concurrent connections. The
  firmware picks embassy-net's IP and medium features.
- `link` frames a byte link that has no connections (a UART shared through
  a TCP bridge): `FLAG 'O'/'C' id` markers and escaped data, `serve_link`
  serving each connection with a fresh buffer under a time bound, and a new
  connection aborting the old one. Its host side is
  `scripts/embedded/uart-gateway.py`.
- A HEAD request runs the GET handler without sending the body; 1xx, 204
  and 304 get no `Content-Length` and no body; any `Connection: close` token
  closes.

Memory per connection is `rx + tx` plus the handler's future: fixed at
compile time. The receive buffer bounds the parser's limits, so a whole
request fits.

Trusted, tested (`src/tests.rs`: pipelining, a smuggling corpus, oversized
heads, truncation, slow clients, header refusals, route binding, signed and
replayed requests, proptest fuzzing; `store`: power cuts at every byte;
`json`: agreement with serde_json; `tcp`: two embassy-net stacks over an
in-memory link). The examples in [`examples/embedded`](../../examples/embedded)
run it on a host and in Renode.
