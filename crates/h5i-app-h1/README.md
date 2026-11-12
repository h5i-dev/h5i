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

The crate does no I/O. The caller reads, and the crate says what the bytes
mean:

- `recv_step` is one turn of the receive loop. It parses the head with
  `parse_head`, reads the body with `read_body`, and asks for another read
  when it needs more bytes.
- `read_body` finds where the body and the message end in the receive
  buffer and decodes a chunked body in place. After appending bytes, call it
  again with the state it returned.
- `chunk_step` decodes a `chunked` body one byte at a time. Its state is four
  integers.
- `carry` moves the bytes after a request to the start of the buffer, where
  the next request begins.
- `fit_limits` lowers the limits until a whole request fits in the buffer.
- `must_close` decides whether the connection stays open.
- `write_head` writes `HTTP/1.1 <status> <reason>`, `Content-Type`,
  `Content-Length` (always from the given length) and `Connection: close`.
  `write_head_open`, `write_field` and `end_head` write the same head with
  extra fields between the framing fields and the blank line.

`h5i-app-embedded` is the async server built on this crate.

## Accepted grammar

A subset of HTTP/1.1. The parser rejects some valid requests: absolute-form,
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

The theorems are about the Rust code as extracted to Lean, not about a model
of it. Each item ends with the theorem names.

### Parsing the head (`H1Proofs.lean`, `H1Props.lean`, `H1Prefix.lean`)

- `parse_head` computes `Spec.parseHead` for every input and limit, so it
  never panics (`parse_head_spec`).
- It reads at most `max_head` bytes: once that many are buffered, the
  result depends on them alone (`parse_head_reads`, `parseHead_reads`).
- Once it returns `Done` or `Reject`, a longer buffer gives the same answer.
  So the result does not depend on how the stream was split into reads
  (`parseHead_prefix`, `parse_head_prefix`, `parseHead_split_invariant`).
- Every head it accepts has token names, values without CR, LF or NUL, the
  `Host`, `Content-Length` and `Transfer-Encoding` counts from "Accepted
  grammar", and a framing that matches its fields (`accept_wellformed`,
  `parse_head_wellformed`).

### Checked against an independent spec (`H1Valid.lean`, `H1Sound.lean`, `H1Smuggle.lean`, `H1Chunked.lean`)

`H1Valid.lean` defines a valid request from scratch, with its own byte
classes, lowercasing and literals. It imports nothing from the parser's spec.

- Every head `parse_head` accepts is an `H1Valid.ValidHead`: request line,
  field lines, origin-form target, `Host` value and the policy above. Its
  framing is `H1Valid.framing` of the field lines (RFC 9112 §6.3), and the
  method, target and field spans hold exactly those parts
  (`parse_head_sound`, `parseHead_valid`).
- No request smuggling: every parser in the lenient family `Interp`, which
  reads names with `H1Valid` instead of the parser's `classify`, finds the
  same head length and framing on every head we accept (`no_smuggling`,
  `no_smuggling_valid`).
- Every chunked body that `Spec.dechunk`, `read_body` or `recv_step` accepts
  is an `H1Valid.ValidChunked`: RFC 9112 §7.1 with 1 to 8 hex digits, no
  extensions or trailers, and a payload of at most `max_body`. The received
  bytes `[start, used)` are that body and `rx[start..end]` is its payload
  (`dechunk_valid`, `read_body_chunked_valid`, `recv_step_chunked_valid`).

### Reading the body (`H1Proofs.lean`, `H1Body.lean`, `H1Read.lean`)

- `chunk_step` computes `Spec.chunkStep` for every state, byte and limit
  (`chunk_step_spec`).
- `Spec.dechunk` defines the whole chunked body chunk by chunk: the payload
  and bytes consumed, a rejection, or a need for more. Folding `chunkStep`
  over the bytes gives the same result (`feed_dechunk`), and bytes after the
  body's end do not change it (`dechunk_local`).
- On a chunked body, `read_body` returns `Spec.dechunk` of the bytes
  received so far. The payload is at `buf[start..end]` and
  `used = start + consumed`. Bytes before the first write, and from the read
  cursor `r` on, are unchanged (`read_body_chunked`, `dechunk_spec`).
- Those results assume the invariant `ChunkInv`. `body_start` establishes
  it, appending bytes at `filled` keeps it, and `read_body` restores it
  whenever it returns `More` (`inv_start`, `inv_extend`).
- With `Content-Length: n`, the body is done at `start + n` once `filled`
  reaches it, or 413 if it cannot fit. Without a body it is done at
  `start`. Neither changes `buf` (`read_body_length`, `read_body_empty`).

### The receive loop (`H1Recv.lean`)

- One `recv_step` call returns `recvS` of the bytes received for the
  request: `parseHead`, then the body by its framing, or 431 or 413 if the
  buffer fills first (`recv_step_spec`).
- On `Ready(head, start, end, used)`, the payload is at `rx[start..end]` and
  `used` is the head length plus the body's length on the wire. The head
  bytes and `rx[used..filled]` are as received.
- After `NeedRead` the invariant `RInv` holds, and appending a read at
  `filled` keeps it (`RInv.extend`).
- The server loop `Drive` reads only after `NeedRead` and passes `eof` after
  a read of 0. For every sequence of reads it ends with `recvS` of all the
  bytes read. So two splits of the same bytes give the same action, payload
  and `filled`, and so does one call with all of them at once
  (`drive_view`, `drive_split`, `drive_at_once`).
- Once the result is decided, more bytes or the peer closing change nothing.
  `NeedRead` comes only while `rx` has room (`recvS_prefix`, `drive_room`).
- After `Ready`, `carry` leaves exactly the bytes received after `used` at
  the start of `rx`, where the next request begins (`drive_next`,
  `carry_spec`).
- `fit_limits` returns limits whose `max_head + max_body` fits in the
  buffer. `must_close` is true for HTTP/1.0 and from the `max_requests`-th
  request on (`fit_limits_spec`, `must_close_spec`).

### Writing the response head (`H1Proofs.lean`, `H1Fields.lean`)

- None of the writers panics, for any buffer length, position, name or value
  (`write_head_total`, `write_head_open_total`, `end_head_total`,
  `write_field_total`, `write_head_with_total`).
- When `write_head` returns `Some(p)`, the status is in 100..=599, the
  content type is field-value bytes, and the first `p` bytes of `out` are
  exactly `Spec.renderHead`. The rest of `out` is unchanged
  (`write_head_spec`).
- `write_head_open` and `end_head` do the same for the open head
  (`Spec.renderOpen`) and the final CRLF. Each writer only appends, so open,
  fields and end together write `Spec.renderHeadWith` of the fields
  (`write_head_open_spec`, `end_head_W`, `renderHeadWith_snoc`).
- When `write_field` returns `Some(p)`, the name is a non-empty token that
  is not reserved, and the value is field-value bytes with no leading or
  trailing SP or HTAB. It wrote exactly `name: value` CRLF at `pos` and
  changed nothing else (`write_field_spec`). The reserved names
  (`reservedSpec`) are `Content-Length`, `Transfer-Encoding`,
  `Content-Type`, `Connection`, `Trailer` and `Upgrade`, in any case.
- `write_head_with` writes the open head, the first `n` of up to four extra
  fields through `write_field`, and the final CRLF. On `Some(p)`, every
  field passed `write_field`'s checks and the first `p` bytes of `out` are
  exactly `Spec.renderHeadWith` of those fields (`write_head_with_spec`,
  `write_fields_spec`).
- The helpers write exactly their own bytes after what is already written
  (`dec_digits_spec`, `put_W`, `put1_W`, `put_dec_W`, `put_reason_W`).

## Not proven

- completeness, meaning that every valid head within the limits is accepted
  and every valid chunked body is decoded;
- that `wants_close` finds exactly the `close` options (this is tested);
- anything about time.

The theorems assume a read changes nothing below `filled`.
`h5i-app-embedded` passes only `rx[filled..]` to the read. A read that
reports more bytes than it was given makes `filled > rx.len()`, and
`recv_step` answers that with 500.

## Checking and testing

To check the proofs, run `cargo app check crates/h5i-app-h1`. It confirms
there is no `sorry`, no `native_decide`, and no axiom beyond Lean's three.

The unit tests are in `src/tests.rs`. `tests/differential.rs` runs
`parse_head` and `httparse` on 9,000 generated, mutated and random inputs.
Where both accept, they must agree on every part of the head, and `parse_head`
may not accept anything `httparse` refuses. Where only `httparse` accepts,
the input must break a rule we chose on purpose (`stricter_by_design`): its
own grammar for the target and `Host`, the framing policy, or CRLF-only line
ends. The test also checks the well-formedness properties at runtime.
