# h5i-app-h1

A strict HTTP/1.1 request-head parser, chunked-body decoder and response-head
writer for microcontrollers. `#![no_std]`, no allocation, no dependencies.
Written in the Rust subset Charon and Aeneas translate to Lean 4; the proofs in
`proofs/` are about the extracted code (`proofs/generated/H5iAppH1.lean`, which
CI re-extracts and diffs).

```rust
use h5i_app_h1::{parse_head, HeadResult, Limits, Framing};

let lim = Limits { max_head: 1024, max_body: 1024 };
match parse_head(&buf[..filled], &lim) {
    HeadResult::Incomplete => { /* read more */ }
    HeadResult::Reject(status) => { /* answer `status`, close */ }
    HeadResult::Done(h) => { /* h.len, h.framing, h.headers[..h.count] */ }
}
```

`chunk_step` decodes a `chunked` body one byte at a time with a four-integer
state; `write_head` writes `HTTP/1.1 <status> <reason>`, `Content-Type`,
`Content-Length` (always from the given length) and `Connection: close`.
`h5i-app-embedded` is the async server built on it.

## Accepted grammar

Origin-form targets, `HTTP/1.0` or `HTTP/1.1`, CRLF line ends only, no obs-fold,
no whitespace before a colon, field values of VCHAR / obs-text / SP / HTAB,
exactly one `Host` on HTTP/1.1, at most one `Content-Length` (digits, at most
`max_body`) and at most one `Transfer-Encoding` (exactly `chunked`, HTTP/1.1
only), never both, and neither value may end in whitespace. Chunk sizes are
1 to 8 hex digits; chunk extensions and trailer fields are rejected.

## What is proven (`proofs/`)

| Theorem | File | Says |
|---|---|---|
| `parse_head_spec` | `H1Proofs.lean` | for every input and limit, `parse_head` returns what `Spec.parseHead` computes; so it never panics |
| `chunk_step_spec` | `H1Proofs.lean` | `chunk_step` computes `Spec.chunkStep` for every state, byte and limit |
| `write_head_total` | `H1Proofs.lean` | `write_head` never panics, for every buffer length |
| `dec_digits_spec`, `put_W`, `put1_W`, `put_dec_W`, `put_reason_W` | `H1Proofs.lean` | the head writer's pieces write exactly their bytes after what is already written |
| `parse_head_reads`, `parseHead_reads` | `H1Props.lean` | once `max_head` bytes are buffered, the result depends on them alone |
| `accept_wellformed`, `parse_head_wellformed` | `H1Props.lean` | every accepted head has token names, CR/LF/NUL-free values, the `Host`/`Content-Length`/`Transfer-Encoding` counts above, and a framing that matches its fields |
| `no_smuggling` | `H1Smuggle.lean` | every parser in the lenient family `Interp` computes the same head length and framing on every head we accept |

Not proven: that `write_head`'s bytes equal `Spec.renderHead` (tested), the
whole-body chunked decoding against a list-level spec (each step is proven),
and anything about time. Check with `cargo app check crates/h5i-app-h1`
(no `sorry`, `native_decide` or axioms beyond Lean's three).

Tests: unit tests in `src/tests.rs`; `tests/differential.rs` compares
`parse_head` with `httparse` on 9,000 generated, mutated and random inputs and
checks the well-formedness properties at runtime.
