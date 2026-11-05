# h5i-app on bare metal: a verified, async, `no_std` HTTP/1.1 server

Status: implemented on branch `support-embed-webserver`; see "Final state"
at the end for what was built, what is proven, and where the implementation
departs from this design and why.

## Goal

Run an h5i-app kernel on a microcontroller behind an axum-like, async,
`no_std`, heap-free HTTP server in the spirit of
[picoserve](https://github.com/sammhicks/picoserve), and prove in Lean 4 the
properties that matter on a device with a few hundred KB of RAM:

1. **Panic freedom.** No input bytes make the HTTP code panic. On a
   microcontroller a panic halts or resets the device.
2. **Protocol correctness.** The bytes we accept are exactly the RFC 9112
   subset we specify; everything else is rejected with a definite status.
   In particular no request smuggling: `Content-Length`/`Transfer-Encoding`
   handling leaves no room for two parsers to disagree on where a message ends.
3. **Resource bounds.** Memory is fixed at compile time and the work per
   input byte is bounded, for every input.

Safe Rust already gives memory safety. It does not give 1-3: for example
`&buf[1..1 + buf[0] as usize]` is memory-safe and still panics on short
input. Aeneas turns every panic, overflow and out-of-bounds index into `fail`
in the extracted Lean, so a total spec `f x ⦃ r => P r ⦄` proven for all `x`
is a panic-freedom proof.

## Why not run picoserve itself

Proving a parser is worthless if a different parser runs on the device.
picoserve's request parsing is internal to the crate, uses idioms Charon and
Aeneas do not extract (iterators, closures, `?`, trait-heavy generics), and
its README lists known gaps (no check that `Content-Length` matches the body,
little stress testing). So we take its API shape (axum-style router,
extractors, `async fn` handlers, caller-provided buffers, runtime-agnostic
I/O) and write the byte-level code ourselves in the Aeneas subset. The
verified code is the code that runs.

## Crates

```
crates/h5i-app-h1        verified, #![no_std], no alloc, no dependencies
crates/h5i-app-embedded  trusted async shell, #![no_std], no alloc
crates/h5i-app-core      gains a default `std` feature; Kernel works without it
examples/embedded/...    device kernel + host runner + Renode firmware
```

### `h5i-app-h1`: the verified HTTP/1.1 core (extracted to Lean)

Pure functions over byte slices and fixed-size arrays. No allocation, no
lifetimes in results (results are offsets and lengths into the input, which
keeps Aeneas away from borrow-returning functions), no dependencies, written
in the same subset as `h5i-app-token`/`h5i-app-json` (while loops over
indices, numeric byte constants, no `?`/closures/iterators, clippy allows in
`Cargo.toml`).

Spike first: confirm Charon/Aeneas at the pinned rev extract `&[u8]` reads,
`[T; N]` arrays and `&mut [u8]` writes cleanly and that `H5iAppLib` lemmas
or Aeneas' `Slice`/`Array` lemmas make the proofs workable. If slices are
unworkable, fall back to `&[u8; N]` plus an explicit `len`.

Limits are compile-time constants (or a `Limits` struct of `usize`s passed
in, with `MAX_HEADERS` a `const`):

| Limit | Default | Rejection |
|---|---|---|
| request head bytes (`max_head`) | 1024 | 431, or 414 for the target |
| header count (`MAX_HEADERS`) | 16 | 431 |
| body bytes (`max_body`) | 1024 | 413 |
| chunk-size hex digits | 8 | 400 |

API (names indicative):

- `parse_head(buf: &[u8], lim: &Limits) -> HeadResult` where
  `HeadResult = Incomplete | Reject(Status) | Done(Head)`. `Head` holds the
  method, target span, version, `[HeaderSpan; MAX_HEADERS]` with a count,
  the head length, and the decided `Framing`.
- `Framing = Empty | Length(u32) | Chunked`, decided inside `parse_head`.
- `ChunkState` plus `chunk_step(st: ChunkState, b: u8, lim) -> (ChunkState, Out)`
  where `Out = Nothing | Data | Done | Reject(Status)`: a byte-at-a-time
  chunked decoder with constant-size state.
- `write_head(out: &mut [u8], status: u16, content_type: &[u8], body_len: u32,
  close: bool) -> Option<usize>`: the response status line and headers.
  `None` when `out` is too small. `Content-Length` always comes from
  `body_len`, never from the app.
- `match_path(target, pattern)` for routes like `/relay/{id}`, if it can be
  kept small; otherwise routing stays in the trusted shell.

Accepted grammar (strict on purpose; a device has no reason to be lenient):

- Request line `method SP origin-form SP HTTP/1.1 CRLF` (also `HTTP/1.0`).
  Method is a token. Target starts with `/` and contains only visible ASCII.
  Absolute-form, authority-form and `*` are rejected.
- Lines end in CRLF. Bare CR and bare LF anywhere in the head are rejected.
- Header field `token ":" OWS value OWS CRLF`. Whitespace before the colon is
  rejected (RFC 9112 §5.1 says MUST reject). Obs-fold (a line starting with
  SP/HTAB) is rejected. Values may contain VCHAR, obs-text, SP, HTAB; NUL,
  CR, LF and other controls are rejected.
- Names compare case-insensitively.
- HTTP/1.1 needs exactly one `Host` (RFC 9112 §3.2); zero or several is 400.
- `Content-Length`: at most one field, value `1*DIGIT` with no list form, no
  sign, no overflow (checked against `max_body` before it can overflow).
- `Transfer-Encoding`: at most one field, value exactly `chunked`
  (case-insensitive). Anything else is 501. On HTTP/1.0 it is 400.
- Both `Content-Length` and `Transfer-Encoding` present: 400. RFC 9112 §6.3
  lets a server either reject or let TE win; rejecting removes the case where
  an upstream proxy picks the other one.
- Chunked: `chunk-size` hex with a digit cap, chunk extensions rejected (or
  skipped with a byte cap, decide during implementation and say which),
  trailers rejected, every chunk-data followed by CRLF, total ≤ `max_body`.

### What is proved (`crates/h5i-app-h1/proofs`)

Layout like `h5i-app-token/proofs`: `H1Spec.lean` (pure spec over
`List Nat`), `H1Proofs.lean`, `generated/` (Aeneas output, never edited),
`h5i-app.toml` with `start-from = ["parse_head", "chunk_step", "write_head", ...]`.
`h5i app check` gates `sorry`/`native_decide`/axioms as it does elsewhere.

**Panic freedom and termination**

- `parse_head_total : ∀ buf lim, parse_head buf lim ⦃ _ => True ⦄`, with no
  precondition beyond what the types give. Same for `chunk_step` and
  `write_head` (the latter for every `out` length, returning `None` when it
  does not fit).

**Functional correctness against the spec**

- `parse_head_spec : parse_head buf lim ⦃ r => toSpec r = Spec.parseHead (bytes buf) lim ⦄`.
  `Spec.parseHead` is the grammar above, written for readability, not speed.
- `chunk_decode_spec`: folding `chunk_step` over a byte list equals
  `Spec.dechunk`, which is defined on the whole list.
- `write_head_spec`: on `some n`, `out[..n]` equals `Spec.renderHead status
  ct body_len close`, and the bytes after `n` are unchanged.

**Rejection and smuggling**

- `accept_wellformed`: if `parse_head` returns `Done h` then: every header
  name is a token; no value contains CR, LF or NUL; for HTTP/1.1 exactly one
  `Host`; at most one `Content-Length`; at most one `Transfer-Encoding` and
  it is `chunked`; never both; `h.len ≤ max_head`; header count ≤ `MAX_HEADERS`.
- `no_smuggling`: define in Lean a family `Interp` of plausible lenient
  parsers that real servers and proxies have implemented: first vs last
  duplicate `Content-Length`, TE-wins vs CL-wins, accept LF-only line ends,
  accept whitespace before the colon (and so treat `Transfer-Encoding :` as a
  different header or as TE), unfold obs-fold, case-sensitive names, trim vs
  keep trailing whitespace. Theorem: for every input our parser accepts,
  every `i ∈ Interp` that parses it computes the same head length and the
  same body framing as ours. So no member of the family can see a message
  boundary in a different place than we do. State the family's limits in the
  doc: the theorem is relative to it.
- `pipeline_split`: for a byte stream that is a concatenation of requests we
  accept, the split into (head, body) pairs our loop computes is unique and
  equals the spec's split.
- `response_no_splitting`: the bytes `write_head` emits contain exactly one
  CRLFCRLF, at the end, and `Content-Length` equals `body_len`.

**Resource bounds**

- Memory: `h5i-app-h1` has no `alloc`, and every type has a size fixed by
  constants, so per-connection memory is `RX_BUF + TX_BUF + size_of::<Head>()`
  plus the handler future. This is structural, checked by `#![no_std]` with
  no `extern crate alloc`, and by a size assertion for the firmware's `.bss`
  and stack in the example (`cargo size` / linker `ASSERT`). Not a Lean theorem.
- Reads stay within the bound: `parse_head_reads` shows the result depends
  only on `buf[..min(len, max_head + 1)]`, so a peer sending more cannot make
  the server buffer more.
- Work per byte: each loop's index strictly increases and is bounded by the
  input length (the termination measure), so `parse_head` makes at most
  `min(len, max_head + 1)` iterations of its main loop and `chunk_step` is
  loop-free. State this as a theorem if a step counter can be threaded
  through cheaply; otherwise state it as the documented termination measure
  and do not claim more.
- Not proven: wall-clock time (needs assumptions about the network and the
  executor), number of connections (bounded structurally by the static task
  pool size), stack depth of the async shell.

### `h5i-app-embedded`: the async shell (trusted)

`#![no_std]`, no alloc. Its job is to move bytes between a socket and the
verified core, and between the core and a kernel.

- I/O: generic over `embedded_io_async::{Read, Write}`, so the same code
  runs over tokio on the host (`embedded-io-adapters`), embassy-net TCP, or a
  UART on the device. No runtime dependency in the crate.
- `Server::serve(&app, &mut rx: [u8; N], &mut tx: [u8; M], io)` loop: read
  into `rx`, call `parse_head`; on `Incomplete` read more (a full buffer is
  431 then close); on `Reject(s)` write the status with `Connection: close`
  and close; on `Done` read the body by `Framing` (via `chunk_step` for
  chunked), dispatch, write the response with `write_head`, then keep-alive
  or close. Read timeout and max requests per connection come from `Config`.
- Routing and handlers, axum-shaped: `Router::new().route("/relay/{id}",
  post(handler))`, handlers are `async fn` taking extractors (`Path<u32>`,
  `Body<'r>`, `Actor<K>`, `State<&S>`) and returning `IntoResponse`. Router
  is a tuple-typed chain like picoserve so no allocation or `dyn` is needed.
- Kernel binding mirrors `h5i-app-http`: `Authenticator<K>` (bearer tokens
  through the verified `h5i-app-token` parser if it can go `no_std`, or a
  pre-shared key compared in constant time), `Api<K>` with
  `decode_command(route, &[u8]) -> Result<Command, Status>` and
  `encode_reply(&Reply, &mut [u8]) -> Option<usize>`, and a `DeviceEngine<K>`
  holding one tenant's snapshot in a `critical_section`/embassy mutex, whose
  `execute` does `transition` then `apply` exactly like `MemoryEngine::execute`.
  Request decoding stays trusted, as in the axum shell: the kernel's theorems
  hold for every command, so a bad decoding can do no more than a client
  could ask for.

### `h5i-app-core` without `std`

- `#![cfg_attr(not(feature = "std"), no_std)]`, `default = ["std"]`,
  `alloc` feature implied by `std`.
- No `std`: `Kernel`, `TenantId`, `Timestamp` (without `now`).
- `alloc`: `ReplyCodec`, `Executor` (`std::error::Error` becomes
  `core::error::Error`, the same trait).
- `std`: `MemoryEngine`, `MemoryError`, `Timestamp::now`.
- Nothing changes for existing users; the workspace and `examples/app` must
  build and test as before.

## Examples

`examples/embedded/`, its own Cargo workspace like `examples/app`.

**`thermostat`**, a device kernel with an embedded safety property:

- `kernel/` (Aeneas subset, no alloc): snapshot `{ setpoint_dc: i16, temp_dc:
  i16, heater: bool, relays: [bool; 4], mode }`. Commands: `SetSetpoint`,
  `ReportTemp`, `SetRelay { idx, on }`, `SetMode`, `Read`.
- Proofs: `transition` never panics for any command, including `idx ≥ 4`
  (refused, not indexed); the interlock invariant `heater → temp_dc <
  MAX_TEMP` and `setpoint_dc ∈ [MIN, MAX]` holds in every reachable state;
  `Read` is read-only (`H5iAppLib.ReadOnly.Sound`).
- `host/`: runs the shell over tokio for development, fuzzing and curl.
- `firmware/`: Cortex-M binary run in Renode.

## Renode

- Install the portable Linux release of [Renode](https://github.com/renode/renode)
  under `~/.local/opt/renode` (scripted, version pinned in the script).
- Target: a Cortex-M board Renode models well (start with
  `stm32f4_discovery` or `nrf52840`), Rust target `thumbv7em-none-eabihf`,
  `cortex-m-rt` and `embassy-executor`.
- Transport, in order of preference, keep the first that works in Renode:
  1. Ethernet with `embassy-net` if Renode models the board's MAC well
     enough for it, bridged to the host with a TAP or Renode's switch.
  2. Raw HTTP over UART: `emulation CreateServerSocketTerminal 8080 "term"
     false` and `connector Connect sysbus.usartN term`, so `curl
     http://127.0.0.1:8080/...` reaches the firmware byte for byte. One
     connection at a time, which is fine for a demo.
- `scripts/embedded/renode-smoke.sh` starts Renode headless (`--disable-xwt
  --console`), loads the ELF, and checks with curl: a valid GET and POST,
  pipelined requests, and rejections (CL+TE, duplicate CL, obs-fold, space
  before colon, oversized head, bad chunk size). Then it checks the device is
  still serving, which shows the rejections did not crash it.

## Testing beyond proofs

- Host unit tests for the shell and the core.
- A differential test of `parse_head` against `httparse` on random and
  mutated inputs (`httparse` as a dev-dependency only): wherever both accept,
  method, target and headers agree; list where they differ and why (we are
  stricter by design).
- A fuzz target (cargo-fuzz or a bounded proptest) asserting no panic and the
  `accept_wellformed` properties at runtime.
- The smuggling corpus from the Renode script also runs on host.

## Trust summary (to fold into design-app-trust.md)

| Item | Status |
|---|---|
| `h5i-app-h1` parsing, chunked decoding, response head | proven (panic freedom, spec, smuggling relative to `Interp`, read bound) |
| thermostat `transition`/`apply` | proven (panic freedom, interlock invariant, read-only) |
| memory per connection, no heap | structural (`no_std`, no alloc, fixed buffers, size check) |
| `h5i-app-embedded` shell, router, extractors, command decoding | trusted, tested |
| `embassy`, HAL, `embedded-io`, Renode models, Charon/Aeneas/Lean | assumed |
| that the device runs the extracted code | CI re-extracts and diffs (A9), as for other crates |

## Final state

### Built

| Piece | Where |
|---|---|
| verified core | `crates/h5i-app-h1` (`src/lib.rs`, `proofs/H1Spec.lean`, `H1Proofs.lean`, `H1Props.lean`, `H1Smuggle.lean`) |
| async shell | `crates/h5i-app-embedded` (`serve`, `Router`, `kernel::{DeviceEngine, PskAuth, Api, respond}`) |
| `no_std` core | `crates/h5i-app-core` features `std` (default) / `alloc` |
| thermostat | `examples/embedded/thermostat/{kernel,proofs,app,host,firmware}` |
| Renode | `scripts/embedded/{install-renode.sh,thermostat.resc,renode-smoke.sh}` (Renode 1.17.0) |

### Proven (gate: `cargo app check`, no `sorry`/`native_decide`/axioms)

- `parse_head_spec`: `parse_head` = `Spec.parseHead` for every buffer and limit
  (panic freedom included). `chunk_step_spec`: `chunk_step` = `Spec.chunkStep`
  for every state. `write_head_total`: `write_head` never panics; its pieces
  (`dec_digits_spec`, `put_W`, `put1_W`, `put_dec_W`, `put_reason_W`) write
  exactly their bytes after a written prefix.
- `accept_wellformed` / `parse_head_wellformed` (`H1Props.lean`).
- `no_smuggling` (`H1Smuggle.lean`) relative to `Interp`: line ends CRLF or LF;
  first or last duplicate `Content-Length`; TE-wins or CL-wins; whitespace
  before the colon stripped or kept; obs-fold unfolded or ignored; values
  trimmed or kept. For every input `parse_head` accepts, every member, given
  any prefix that holds the head, computes the same head length and framing.
- `parseHead_reads` / `parse_head_reads`: the result depends only on the first
  `max_head` bytes once they are buffered.
- Thermostat: `transition_total`, `set_relay_refused`, `inv_reachable`,
  `interlock`, `ReadOnly.sound`; mutants 3/3 caught.

### Tested, not proven

- `write_head` output equals `Spec.renderHead` (unit tests; the Lean spec is
  written, the end-to-end theorem is not: Aeneas' `step` cannot infer the
  prefix lemmas' ghost arguments, and the manual chain was not finished).
- `response_no_splitting`: the content type is checked to hold only
  field-value bytes (proven to be checked: `write_head` returns `None`
  otherwise), but "exactly one CRLFCRLF" is not a theorem.
- Whole-body chunked decoding against a list-level `dechunk`, and
  `pipeline_split`: each byte step is proven; the fold and the stream split are
  tested (shell tests, Renode smoke).
- Work per byte: termination measures only; no step-counter theorem.
- Shell: unit tests, smuggling corpus, proptest; `parse_head` vs `httparse`
  (9,000 cases); thermostat API proptest of the interlock through HTTP;
  Renode smoke (25 checks).

### Deviations from the design, and why

1. **Slices, not `&[u8; N]`.** The spike extracted `&[u8]` reads, `&mut [u8]`
   writes and `[T; N]` cleanly, and `Slice`/`Array` lemmas sufficed.
2. **Aeneas limits shaped the code.** A function returning a `&'static [u8]`
   from a `match` fails ("no bottoms"), unsizing a local array (`&code`)
   and arrays of borrows are unimplemented. So reasons are written by
   `put_reason`, status digits by `put1`, and decimals through a local
   `[u8; 10]` and `put_n`. Long `||` chains blow up the extracted Lean, so
   `is_tchar` tests ranges.
3. **`chunk_step` split into phase helpers**, so its spec proof stays small.
4. **CL/TE values may not end in whitespace.** Needed for `no_smuggling`
   against the keep-trailing-whitespace member: `chunked ` would otherwise be
   another coding to it.
5. **Case-sensitive name matching is outside `Interp`.** A parser recognizing
   only `Content-Length` would frame a `content-length` body as the next
   request; agreeing with it and with a conforming parser would mean refusing
   every other spelling, which breaks proxies that lowercase names (HTTP/2
   downgrades). RFC 9110 §5.1 makes names case-insensitive.
6. **Read bound is `max_head`, not `max_head + 1`.** A buffer of `max_head`
   bytes without a complete head is already 431/414.
7. **`[authz]` binding needs `std`.** `h5i-app-std` is not `no_std`, so
   `h5i_app_core::authz` sits behind `std`; `alloc` alone gives `ReplyCodec`,
   `Executor` and `sorted_by_key`.
8. **Handlers take `(&Request, &S, &mut [u8])`** rather than per-argument
   extractors: borrowed extractors (`Body<'r>`) and HRTB `AsyncFn` bounds do
   not compose without allocation. `Request::param_u32`, `header`, `body`
   play the extractors' role.
9. **No read timeout in `Config`.** The shell has no clock; the host runner
   wraps `serve` in `tokio::time::timeout`, the firmware uses
   `embassy_time::with_timeout` to resynchronize the UART.
10. **Transport: UART, not Ethernet.** The UART path (choice 2) worked first
    try in Renode; Ethernet (embassy-net, a TAP bridge) was not attempted.
11. **`Head` holds header spans, the shell compares names** (`Request::header`)
    outside the verified code; the framing headers are classified inside it.
