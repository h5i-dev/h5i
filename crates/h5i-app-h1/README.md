# h5i-app-h1

A strict HTTP/1.1 request-head parser, chunked-body decoder and response-head
writer for microcontrollers. `#![no_std]`, no allocation, no dependencies.
Written in the Rust subset Charon and Aeneas translate to Lean 4; the proofs in
`proofs/` are about the extracted code (`proofs/generated/H5iAppH1.lean`, which
CI re-extracts and diffs).

```rust
use h5i_app_h1::{carry, fit_limits, recv_start, recv_step, Action, Limits};

let lim = fit_limits(&Limits { max_head: 1024, max_body: 1024 }, rx.len());
let (mut st, mut eof) = (recv_start(), false);
loop {
    let (s, a) = recv_step(st, &mut rx, filled, eof, &lim);
    st = s;
    match a {
        Action::NeedRead => match read(&mut rx[filled..]) { 0 => eof = true, n => filled += n },
        Action::Ready(head, start, end, used) => {
            // the body is rx[start..end]; answer, then the next request:
            filled = carry(&mut rx, used, filled);
            (st, eof) = (recv_start(), false);
        }
        Action::Reject(status) => break, // answer `status`, close
        Action::Eof => break,            // the peer closed between requests
    }
}
```

`recv_step` is the receive loop without its I/O: it parses the head with
`parse_head`, then reads the body with `read_body`, and says when to read
more; `carry` moves the bytes after a request to the start of the buffer;
`fit_limits` cuts the limits so a whole request fits; `must_close` decides
keep-alive. `chunk_step` decodes a `chunked` body one byte at a time with a four-integer
state. `read_body` finds where the body and the message end in the receive
buffer, decoding a chunked body in place; call it again with its state after
appending bytes. `write_head` writes `HTTP/1.1 <status> <reason>`, `Content-Type`,
`Content-Length` (always from the given length) and `Connection: close`.
`write_head_open`, `write_field` and `end_head` write the same head with
extra fields between the framing fields and the blank line.
`h5i-app-embedded` is the async server built on it.

## Accepted grammar

A subset of HTTP/1.1. Some valid requests are rejected: absolute-form,
authority-form and asterisk-form targets, empty lines before the request
line, obs-fold, chunk extensions, trailer fields, `Transfer-Encoding` other
than exactly `chunked`, and `Content-Length` lists.

Accepted: `HTTP/1.0` or `HTTP/1.1`, CRLF line ends only, a token method, an
origin-form target (`/`, then `pchar`, `/`, `?` and `%` HEXDIG HEXDIG; no `#`),
no whitespace before a colon, field values of VCHAR / obs-text / SP / HTAB,
exactly one `Host` on HTTP/1.1 with an empty or `uri-host [ ":" port ]` value
(a `reg-name`, or `[` IP-literal bytes `]`), at most one `Content-Length`
(digits, at most `max_body`) and at most one `Transfer-Encoding` (HTTP/1.1
only), never both, and neither value may end in whitespace. Chunk sizes are
1 to 8 hex digits.

## What is proven (`proofs/`)

| Theorem | File | Says |
|---|---|---|
| `parse_head_spec` | `H1Proofs.lean` | for every input and limit, `parse_head` returns what `Spec.parseHead` computes; so it never panics |
| `chunk_step_spec` | `H1Proofs.lean` | `chunk_step` computes `Spec.chunkStep` for every state, byte and limit |
| `write_head_spec` | `H1Proofs.lean` | on `Some(p)`: the status is in 100..=599, the content type is field-value bytes, and `out` starts with exactly `Spec.renderHead`, `p` bytes long; the rest of `out` is unchanged |
| `write_head_open_spec`, `end_head_W` | `H1Proofs.lean` | the same for the open head (`Spec.renderOpen`) and the final CRLF; each writer extends what is written, so open, fields, end write `Spec.renderHeadWith` of the fields (`renderHeadWith_snoc`) |
| `write_head_total`, `write_head_open_total`, `end_head_total` | `H1Proofs.lean` | the head writers never panic, for every buffer length |
| `write_field_total` | `H1Proofs.lean` | `write_field` never panics, for every buffer, position, name and value |
| `write_field_spec` | `H1Proofs.lean` | on `Some(p)`: the name is a non-empty token and not reserved (`reservedSpec`: `Content-Length`, `Transfer-Encoding`, `Content-Type`, `Connection`, `Trailer`, `Upgrade`, any case), the value is field-value bytes without leading or trailing SP/HTAB, and exactly `name: value` CRLF was written at `pos`, nothing else changed |
| `write_head_with_spec`, `write_fields_spec`, `write_head_with_total` | `H1Fields.lean` | `write_head_with` (the open head, the first `n` of four extra fields through `write_field`, the final CRLF) never panics; on `Some(p)` every field written passed `write_field`'s checks and `out` starts with exactly `Spec.renderHeadWith` of those fields, `p` bytes long |
| `dec_digits_spec`, `put_W`, `put1_W`, `put_dec_W`, `put_reason_W` | `H1Proofs.lean` | the head writer's pieces write exactly their bytes after what is already written |
| `parse_head_reads`, `parseHead_reads` | `H1Props.lean` | once `max_head` bytes are buffered, the result depends on them alone |
| `parseHead_prefix`, `parse_head_prefix` | `H1Prefix.lean` | once the parser returns `Done` or `Reject` on a buffer, it returns the same on every longer buffer starting with it |
| `parseHead_split_invariant` | `H1Prefix.lean` | so however a stream is split into reads, the first prefix the parser decides on gives the same result |
| `feed_dechunk` | `H1Body.lean` | folding `chunkStep` from the start state over a byte list computes `Spec.dechunk`, a chunk-by-chunk definition of the whole body (payload and bytes consumed, a rejection, or more needed) |
| `dechunk_local` | `H1Body.lean` | if `dechunk L` ends the body after `k` bytes, then `k ≤ L.length` and `dechunk (L.take k ++ R)` gives the same, for every `R` |
| `read_body_chunked`, `dechunk_spec` | `H1Read.lean` | for a state satisfying `ChunkInv` (the fold so far, payload at `buf[start..w]`, `w ≤ r`, `buf[r..filled]` still the unread wire bytes): `read_body` returns `Spec.dechunk` of the body bytes received so far, with the payload at `buf[start..end]` and `used = start + consumed`; bytes below the first write and at or past `r` (so past `used`) are unchanged; on `More` the invariant holds at `filled` |
| `inv_start`, `inv_extend` | `H1Read.lean` | `body_start`'s state satisfies `ChunkInv`, and appending bytes at `filled` keeps it |
| `read_body_length`, `read_body_empty` | `H1Read.lean` | `Content-Length: n` is done at `start + n` once `filled` reaches it (413 if it cannot fit), no body is done at `start`; `buf` is not changed |
| `recv_step_spec`, `RInv.extend` | `H1Recv.lean` | one `recv_step` call returns `recvS` of the bytes received for the request (`parseHead`, then the body by its framing, `Spec.dechunk` if chunked; 431 or 413 if the buffer fills first): on `Ready(head, start, end, used)` the payload is at `rx[start..end]`, `used` is the head length plus the body's length on the wire, and the head bytes and `rx[used..filled]` are as received; after `NeedRead` the state satisfies `RInv`, and appending a read at `filled` keeps it |
| `drive_view`, `drive_split`, `drive_at_once` | `H1Recv.lean` | for every sequence of reads, the server's loop over the extracted `recv_step` (`Drive`: read only after `NeedRead`, `eof` after a read of 0) ends with `recvS` of all the bytes read; so two splits of the same bytes give the same action, payload and `filled`, and so does one call on all of them at once, whatever bytes follow |
| `recvS_prefix`, `drive_room` | `H1Recv.lean` | once decided, more bytes or the peer closing change nothing; `NeedRead` only with room left in `rx` |
| `drive_next`, `carry_spec` | `H1Recv.lean` | after `Ready`, `carry` leaves exactly the bytes received after `used` at the start of `rx`, as the next request's |
| `fit_limits_spec`, `must_close_spec` | `H1Recv.lean` | `max_head + max_body` fits in the buffer; `must_close` is true for HTTP/1.0 and from the `max_requests`-th request |
| `accept_wellformed`, `parse_head_wellformed` | `H1Props.lean` | every accepted head has token names, CR/LF/NUL-free values, the `Host`/`Content-Length`/`Transfer-Encoding` counts above, and a framing that matches its fields |
| `parse_head_sound`, `parseHead_valid` | `H1Sound.lean` | every head `parse_head` accepts is an `H1Valid.ValidHead`: request line, field lines, origin-form target, `Host` value and the policy above, defined in `H1Valid.lean` with its own byte classes, lowercasing and literals and no import from the parser's spec; `h.framing` is `H1Valid.framing` of the field lines (RFC 9112 §6.3), and the method, target and field spans hold exactly those parts |
| `no_smuggling`, `no_smuggling_valid` | `H1Smuggle.lean` | every parser in the lenient family `Interp` (which reads names with `H1Valid`, not the parser's `classify`) computes the same head length and framing on every head we accept, and that framing is `H1Valid.framing` |
| `dechunk_valid`, `read_body_chunked_valid`, `recv_step_chunked_valid` | `H1Chunked.lean` | every chunked body `Spec.dechunk`, `read_body` or `recv_step` accepts is an `H1Valid.ValidChunked` (RFC 9112 §7.1 with 1 to 8 hex digits, no extensions or trailers, payload at most `max_body`, its own hex values): the received bytes `[start, used)` are that body and `rx[start..end]` its payload |

Not proven: that every valid head within the limits is accepted, or every
valid chunked body decoded (completeness); that `wants_close` finds exactly the
`close` options (tested); anything about time. The theorems assume a read
changes nothing below `filled`: `h5i-app-embedded` passes only
`rx[filled..]` to the read, and a count past the end makes
`filled > rx.len()`, which `recv_step` answers with 500. Check with
`cargo app check crates/h5i-app-h1` (no `sorry`, `native_decide` or axioms
beyond Lean's three).

Tests: unit tests in `src/tests.rs`; `tests/differential.rs` compares
`parse_head` with `httparse` on 9,000 generated, mutated and random inputs
(where we are stricter, by an independent target and `Host` grammar) and
checks the well-formedness properties at runtime.
