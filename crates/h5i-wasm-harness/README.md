<h1 align="center">h5i-wasm-harness</h1>

<p align="center"><strong>A minimal coding-agent loop that runs in a browser, in WASI, and natively, from one no_std core with zero dependencies.</strong></p>

<p align="center">
  <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
</p>

The module does no I/O. It **emits an effect** (call the model, run a tool, or
finish) and the host performs it and feeds the result back. That inversion is
the design: the same `.wasm` (no imports) loads under a browser's
`WebAssembly.instantiate` and under any WASI runtime, and the native `h5i-agent`
binary runs the same logic. The model call and the tools live in the host, next
to the browser's `fetch` and a real filesystem.

It came out of a forum experiment where three agents converged on this design.

## Install

```bash
cargo install --path crates/h5i-wasm-harness   # installs the `h5i-agent` binary
```

That puts `h5i-agent` on your `PATH`; the examples below call it directly. To
put the agent *inside* a browser or WASI host instead of running the CLI, see
[Embed it as wasm](#embed-it-as-wasm).

## Highlights

- **One binary, three environments.** [The boundary](#the-boundary) is a few
  effects crossing as JSON, so the same core is a browser module, a WASI module,
  and a native host, with no `#[cfg]` forks in the loop.
- **Zero dependencies.** `#![no_std]` + `alloc` and a [hand-rolled JSON
  codec](src/json.rs), so the [wasm build](#embed-it-as-wasm) needs no
  `-Zbuild-std`, no nightly, and nothing from crates.io.
- **Seven exports, no imports.** [The whole ABI](#the-boundary) is
  `alloc`/`dealloc` plus `init`/`step`/`resume`/`dump`, each returning a packed
  `u64`.
- [**Multi-turn.**](#run-it) `agent_resume` keeps the conversation, so the REPL
  is multi-turn by default.
- [**Live streaming.**](#run-it) `h5i-agent` renders tokens as they arrive; the
  core itself stays non-streaming.
- [**Native tool-calling.**](#test-against-a-real-model) OpenAI chat-completions
  with real `tool_calls`, not a text protocol; verified against a live Gemini model.

## Run it

`h5i-agent` needs a model source: a real `--model-url http://…` (http:// only,
for a local llama.cpp / Ollama server), or a scripted mock, which is a JSON
array of chat-completions envelopes replayed in order.

#### Interactive (the default)

With no `--task`, `h5i-agent` is a REPL. Type a task per line and it runs it,
keeping the conversation across turns. With a real `--model-url`, tokens render
live as the response streams. Ctrl-D or `exit` quits.

```console
$ h5i-agent --model-url http://127.0.0.1:8080/v1/chat/completions
» create hello.txt containing hi
created hello.txt
» now read it back
the file says hi
```

#### One-shot

Pass `--task` for a single scriptable run; it exits non-zero on failure.
`--trace` prints `[model call]` / `[tool]` lines on stderr, `--dump` prints the
deterministic transcript, `--no-stream` sends one blocking request.

```bash
cat > replies.json <<'JSON'
[ {"choices":[{"message":{"role":"assistant","content":null,
    "tool_calls":[{"id":"c1","type":"function","function":{"name":"write_file",
      "arguments":"{\"path\":\"hello.txt\",\"content\":\"hi\"}"}}]}}]},
  {"choices":[{"message":{"role":"assistant","content":"created hello.txt"}}]} ]
JSON

h5i-agent --task "create hello.txt containing hi" --script replies.json --workdir /tmp/ws --trace
```

## The boundary

The core is a sans-io state machine. It never opens a socket or touches a file.
It returns one *effect* and waits for the host to hand back the matching *event*.

| The module emits (`Effect`) | The host does |
| --- | --- |
| `call_model {request}` | POST the OpenAI chat-completions body, return the reply |
| `run_tool {call_id, name, args}` | run the tool, return its output |
| `done {status, result}` | the run is over |

| The host feeds back (`Event`) | when |
| --- | --- |
| `model_reply {body}` | a 2xx response body (the module parses the envelope) |
| `model_failed {status, body}` | non-2xx, or `0` for a transport/CORS failure |
| `tool_finished {call_id, ok, output}` | a tool finished |

Across the wasm edge that exchange is seven exports and no imports:

| Export | In → out |
| --- | --- |
| `memory` | the module's linear memory |
| `alloc(len) → ptr` | host gets a guest buffer to write UTF-8 JSON into |
| `dealloc(ptr, len)` | no-op under the bump allocator; kept so the ABI outlives it |
| `agent_init(ptr, len) → u64` | init JSON to the first effect |
| `agent_step(ptr, len) → u64` | one event to the next effect |
| `agent_resume(ptr, len) → u64` | `{"task": …}` to the first effect of a new turn |
| `agent_dump() → u64` | the deterministic transcript |

The host calls `alloc(n)`, writes JSON there, and calls an export with
`(ptr, len)`. Every export returns a packed `u64 = (ptr << 32) | len` pointing at
guest-owned JSON valid until the next call, which the host copies out. A browser
reads that `u64` with `BigInt` shifts; a WASI host reads linear memory directly.
Streaming stays a host concern, since the module always takes one complete
envelope.

## Embed it as wasm

`h5i-agent` (the CLI) is the agent running as its own host. The wasm build is
the other half: the agent **core** compiled as a **guest** for you to embed in a
browser page or a WASI runtime, which then performs its effects. It is not a
binary you run on its own; without a host it does nothing.

```bash
rustup target add wasm32-unknown-unknown        # one time
crates/h5i-wasm-harness/scripts/build-wasm.sh   # -> build/h5i-agent.wasm (~130 KB)
```

No `-Zbuild-std`, no nightly, no network: the core is `#![no_std]` + `alloc`
with zero dependencies, so the stock target's prebuilt `core`/`alloc` are
enough.

The module has zero imports, so any wasm runtime can embed it. Worked hosts:

| Host | Runtime | Run |
| --- | --- | --- |
| [`web/index.html`](web/README.md) | a browser's `WebAssembly` | serve `web/`, open the page |
| [`web/node-demo.mjs`](web/README.md) | Node's engine | `node web/node-demo.mjs` |
| [`hosts/wasmtime_host.py`](hosts/wasmtime_host.py) | wasmtime (standalone) | `pip install wasmtime` then `python3 hosts/wasmtime_host.py` |

Each is a small program that calls the exports and performs the effects.
`wasmtime run h5i-agent.wasm` on its own does nothing: the module is a reactor
with custom exports, not a WASI command with a `_start`.

## Run it in the browser

The module has no imports, so a browser loads it with plain
`WebAssembly.instantiate` and drives the loop from JavaScript: the model call
goes through `fetch`, the tools run against an in-memory filesystem.
[`web/`](web/README.md) has the whole host: `host.mjs` (the loop plus helpers,
about 120 lines, no bundler, no dependencies) and `index.html` (a page that runs
it).

```bash
crates/h5i-wasm-harness/scripts/build-wasm.sh          # build the module
cd crates/h5i-wasm-harness && python3 -m http.server 8000
# open http://localhost:8000/web/  — starts in an offline scripted demo
```

The same `host.mjs` runs under Node, so `node web/node-demo.mjs` exercises the
module end-to-end without a browser (the `WebAssembly` API is identical).

## Test against a real model

`h5i-agent` is http-only and dependency-free, so a small local proxy bridges it
to a hosted provider over HTTPS with auth. [`adapters/`](adapters/README.md) has
one for **Google Gemini via Vertex AI**: it mints an OAuth token from a
service-account key and forwards to Vertex's OpenAI-compatible endpoint,
streaming back, with no format translation and tool-calling included. No
credential lives in this repo; the proxy reads a key from a gitignored path at
runtime.

```bash
python3 crates/h5i-wasm-harness/adapters/vertex_openai_proxy.py --port 8137 &
h5i-agent --model-url http://127.0.0.1:8137/v1/chat/completions \
  --task "create note.txt containing hi, then read it back" --workdir /tmp/ws --trace
```

## Tests

```bash
cargo test -p h5i-wasm-harness
```

The suite covers the JSON codec (roundtrip plus adversarial vectors: deep
nesting, lone surrogates, number overflow, control chars), the agent loop (a
full write-then-done trace, buffered-sequential parallel calls, recoverable
invalid calls with a format-error cap, retry only on 429/5xx/transport, the step
limit, a fatal call-id mismatch, and multi-turn `resume`), the
`init`/`step`/`resume`/`dump` boundary, the streaming reassembly, and the
real-filesystem tools with path confinement. `tests/session.rs` drives a full
scripted session through the same string interface the wasm module exposes.

## License

Apache-2.0. See [LICENSE](../../LICENSE).
