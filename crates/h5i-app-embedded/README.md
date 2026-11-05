# h5i-app-embedded

An async, `no_std`, heap-free HTTP/1.1 server for h5i-app kernels on
microcontrollers. It is the trusted shell around the verified
[`h5i-app-h1`](../h5i-app-h1) parser: it reads into a caller-provided buffer,
parses with `parse_head`, decodes chunked bodies in place with `chunk_step`,
routes, and writes response heads with `write_head`.

```rust
let router = Router::new().get(b"/state", read).put(b"/relay/{i}", write);
let mut rx = [0u8; 2048];
let mut tx = [0u8; 1024];
serve(&Config::default(), &router, &app, &mut rx, &mut tx, &mut io).await?;

async fn write(req: &Request<'_>, app: &App, out: &mut [u8]) -> Response {
    h5i_app_embedded::kernel::respond::<Thermostat, Wire>(&app.engine, &app.auth, req, out)
}
```

- I/O is `embedded_io_async::{Read, Write}`: tokio through
  `embedded-io-adapters`, embassy-net, or a UART. The crate has no runtime and
  no clock; timeouts belong to the I/O object.
- Routing is a tuple-typed chain (`Router<Route<H, Route<…, Nil>>>`), no `dyn`
  and no allocation. Patterns are literal segments and `{name}` segments.
- Handlers are `async fn(&Request<'_>, &S, &mut [u8]) -> Response`: they write
  the body at the start of the slice. `Content-Length` always comes from the
  length they return.
- `kernel` binds an h5i-app `Kernel`: `DeviceEngine` (one tenant's snapshot
  behind a `critical_section` mutex; `transition` then `apply`, as
  `MemoryEngine::execute`), `PskAuth` (a pre-shared bearer key compared in
  constant time), `Api` (trusted command decoding and reply encoding) and
  `respond`.

Memory per connection is `rx + tx` plus the handler's future: fixed at
compile time. The receive buffer bounds the parser's limits, so a whole
request fits.

Trusted, tested (`src/tests.rs`: pipelining, a smuggling corpus, oversized
heads, truncation, proptest fuzzing). The examples in
[`examples/embedded`](../../examples/embedded) run it on a host and in Renode.
