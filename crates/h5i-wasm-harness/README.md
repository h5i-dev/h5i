<h1 align="center">h5i-wasm-harness</h1>

<p align="center"><strong>A minimal coding-agent loop that runs in a browser, in WASI, and natively — from one no_std core with zero dependencies.</strong></p>

<p align="center">
  <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
</p>

The module does no I/O. It **emits an effect** — call the model, run a tool, or
finish — and the host performs it and feeds the result back. That inversion is
the whole design: the same `.wasm` (no imports) loads under a browser's
`WebAssembly.instantiate` and under any WASI runtime, and the native `i5h`
binary runs byte-identical logic. The model call and the tools live in the host,
where the browser's `fetch` and a real filesystem already are.

```bash
# talk to a local OpenAI-compatible model server; type a task, watch it work
cargo run -p h5i-wasm-harness --bin i5h -- \
  --model-url http://127.0.0.1:8080/v1/chat/completions --workdir /tmp/ws
# » create hello.txt containing hi, then read it back
```

It came out of a forum experiment where three agents converged on the design;
this crate is that converged prototype, ported into the workspace.

## Highlights

- **One binary, three environments.** [The boundary](#the-boundary) is a handful
  of effects crossing as JSON, so the same core is a browser module, a WASI
  module, and a native host — no `#[cfg]` forks in the loop.
- **Zero dependencies.** `#![no_std]` + `alloc` and a [hand-rolled JSON
  codec](src/json.rs), so the [wasm build](#build-the-wasm-module) needs no
  `-Zbuild-std`, no nightly, and nothing from crates.io.
- **Seven exports, no imports.** [The whole ABI](#the-boundary) is
  `alloc`/`dealloc` plus `init`/`step`/`resume`/`dump`, each a packed `u64`.
- [**Multi-turn.**](#run-it) `agent_resume` keeps the conversation; the `i5h`
  REPL is multi-turn by default.
- [**Live streaming, host-side.**](#run-it) `i5h` renders tokens as they arrive;
  the core itself stays non-streaming and never sees a partial response.
- [**Real tool-calling.**](#test-against-a-real-model) OpenAI chat-completions
  with native `tool_calls` — verified end-to-end against a live Gemini model.
- [**Honest about its limits.**](#what-it-cannot-do) Missing pieces are named,
  not stubbed.

## Run it

`i5h` is the native host. It needs a model source: a real
`--model-url http://…` (http:// only — no TLS without a dependency; meant for
llama.cpp / Ollama on localhost), or a scripted mock — a JSON array of
chat-completions envelopes replayed in order, the shape of mini-swe-agent's
`DeterministicModel`.

#### Interactive (the default)

With no `--task`, `i5h` is a REPL: type a task per line, and the agent runs it
**keeping the conversation across turns**. With a real `--model-url`, tokens
**render live** as the response streams. Ctrl-D or `exit` quits.

```console
$ cargo run -p h5i-wasm-harness --bin i5h -- --model-url http://127.0.0.1:8080/v1/chat/completions
» create hello.txt containing hi
created hello.txt
» now read it back
the file says hi
```

#### One-shot

Pass `--task` for a single scriptable run (exit non-zero on failure). `--trace`
adds `[model call]` / `[tool]` lines on stderr; `--dump` prints the
deterministic transcript; `--no-stream` falls back to one blocking request.

```bash
cat > replies.json <<'JSON'
[ {"choices":[{"message":{"role":"assistant","content":null,
    "tool_calls":[{"id":"c1","type":"function","function":{"name":"write_file",
      "arguments":"{\"path\":\"hello.txt\",\"content\":\"hi\"}"}}]}}]},
  {"choices":[{"message":{"role":"assistant","content":"created hello.txt"}}]} ]
JSON

cargo run -p h5i-wasm-harness --bin i5h -- \
  --task "create hello.txt containing hi" \
  --script replies.json --workdir /tmp/ws --trace
```

## The boundary

The core is a **sans-io state machine**. It never opens a socket or touches a
file — it returns one *effect* and waits for the host to hand back the matching
*event*.

| The module emits (`Effect`) | The host does |
| --- | --- |
| `call_model {request}` | POST the OpenAI chat-completions body, return the reply |
| `run_tool {call_id, name, args}` | run the tool, return its output |
| `done {status, result}` | the run is over |

| The host feeds back (`Event`) | when |
| --- | --- |
| `model_reply {body}` | a 2xx response body (envelope parsing is the module's) |
| `model_failed {status, body}` | non-2xx, or `0` for a transport/CORS failure |
| `tool_finished {call_id, ok, output}` | a tool finished |

Across the wasm edge that exchange is seven exports and **no imports**:

| Export | In → out |
| --- | --- |
| `memory` | the module's linear memory |
| `alloc(len) → ptr` | host gets a guest buffer to write UTF-8 JSON into |
| `dealloc(ptr, len)` | no-op under the bump allocator; kept so the ABI outlives it |
| `agent_init(ptr, len) → u64` | init JSON → the first effect |
| `agent_step(ptr, len) → u64` | one event → the next effect |
| `agent_resume(ptr, len) → u64` | `{"task": …}` → the first effect of a new turn |
| `agent_dump() → u64` | → the deterministic transcript |

The host calls `alloc(n)`, writes JSON there, and calls an export with
`(ptr, len)`. Every export returns a packed `u64 = (ptr << 32) | len` pointing at
guest-owned JSON valid until the next call, which the host copies out. A browser
reads that `u64` with `BigInt` shifts; a WASI host reads linear memory directly.
Response *streaming* is a host concern — the module always takes one complete
envelope — so a browser host does the `fetch`/SSE and reassembly, exactly as
`i5h` does.

## Build the wasm module

```bash
rustup target add wasm32-unknown-unknown        # one time
crates/h5i-wasm-harness/scripts/build-wasm.sh   # -> build/h5i_wasm_harness.wasm (~130 KB)
```

No `-Zbuild-std`, no nightly, no network: the core is `#![no_std]` + `alloc`
with zero dependencies, so the stock target's prebuilt `core`/`alloc` are
enough. (No JS glue is bundled — the boundary above is small enough to write in
a few lines against whatever runtime you have.)

## Test against a real model

`i5h` is http-only and dependency-free, so a small local proxy bridges it to a
hosted provider over HTTPS+auth. [`adapters/`](adapters/README.md) has one for
**Google Gemini via Vertex AI**: it mints an OAuth token from a service-account
key and forwards to Vertex's OpenAI-compatible endpoint, streaming back — no
format translation, tool-calling included. No credential lives in this repo; the
proxy reads a key from a gitignored path at runtime.

```bash
python3 crates/h5i-wasm-harness/adapters/vertex_openai_proxy.py --port 8137 &
cargo run -p h5i-wasm-harness --bin i5h -- \
  --model-url http://127.0.0.1:8137/v1/chat/completions \
  --task "create note.txt containing hi, then read it back" --workdir /tmp/ws --trace
```

## Tests

```bash
cargo test -p h5i-wasm-harness
```

Covers the JSON codec (roundtrip; adversarial vectors — deep nesting, lone
surrogates, number overflow, control chars), the agent loop (full write→done
trace, buffered-sequential parallel calls, recoverable invalid calls with a
format-error cap, retry only on 429/5xx/transport, step limit, call-id-mismatch
fatal, multi-turn `resume`), the `init`/`step`/`resume`/`dump` boundary, the
streaming reassembly (chunk decode, SSE split, delta merge), and the real-FS
tools with path confinement. `tests/session.rs` drives a full scripted session
through the exact string interface the wasm module exposes.

## How it is built

Assembled from three reference projects, keeping the parts that survive the wasm
boundary.

| Borrowed | From |
| --- | --- |
| The loop shape (query → execute until an exit condition; step limits) and the deterministic scripted mock | **mini-swe-agent** (`agents/default.py`, `models/test_models.py`) |
| Structural termination — the run ends when the model stops calling tools — and answering an invalid tool call with a recoverable error | **hax** (`src/agent_loop.h`) |
| The model call belongs in the host (the browser reality is `fetch` + CORS), and middle-out output truncation so errors at the end of long output survive | **Wasm Agents Blueprint** |

Dropped on the way: mini-swe-agent's single-`bash`-tool contract and its
`COMPLETE_TASK…` sentinel, neither of which has a place once tool calls are
structured and there is no shell in a browser.

## What it cannot do

- **Not a full TUI.** The `i5h` REPL renders streamed content live, but there is
  no transcript view, no per-step approval, and tool output is not reflowed.
- **One wasm session per module instance** (static state); re-instantiate to
  reset. Multi-turn *within* a session works via `agent_resume`.
- **The wasm bump allocator never frees** — a long session grows memory
  monotonically.
- **OpenAI chat-completions only.** Streaming is reassembled host-side into one
  envelope (the core stays non-streaming); no cost accounting, no history
  compaction, no Anthropic Messages shape yet.
- **Tools are `read_file` / `write_file` / `list_dir`.** `bash` is in the schema
  but no bundled host declares it — there is no shell in a browser or WASI p1.
- **`i5h`'s HTTP client is `http://` only** (no TLS without a dependency) — fine
  for a localhost model server or the proxy above, not for hosted APIs directly.

## License

Apache-2.0. See [LICENSE](../../LICENSE).
