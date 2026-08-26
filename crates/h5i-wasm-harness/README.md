# h5i-wasm-harness

A minimal coding-agent harness that runs both natively and as WebAssembly, from
one `#![no_std]` core with **zero dependencies**. It came out of a forum
experiment where three agents converged on the design; this crate is the
converged prototype, ported into the workspace.

The whole idea is one boundary: the `.wasm` module is a **sans-io state
machine** and the host performs every side effect. The module never opens a
socket or touches a file — it emits an *effect* (call the model, run a tool,
finish) and waits for the host to feed the *result* back. That inversion is what
lets a single binary serve a browser (JS host over `fetch`), a WASI runtime, and
the native `i5h` host below, unchanged.

## Layout

| File | Role |
|---|---|
| `src/agent.rs` | The loop: build request → parse reply → dispatch tools → repeat until the model stops calling tools. Pure; no I/O. |
| `src/json.rs` | A tiny no_std JSON parser/serializer (so no `serde`, so the wasm build stays trivial). |
| `src/proto.rs` | The JSON wire schema for the effect/event boundary and the `init`/`step`/`dump` contract. |
| `src/wasm.rs` | The `wasm32`-only ABI: `alloc`/`dealloc`/`agent_init`/`agent_step`/`agent_dump` + memory, no imports. |
| `src/bin/i5h/` | The native host binary `i5h`: real filesystem tools, a scripted mock model, an optional local HTTP model. |

## Run it (native, `i5h`)

`i5h` needs a model source. Point it at a real OpenAI-compatible local server
with `--model-url http://127.0.0.1:8080/v1/chat/completions` (http:// only — no
TLS without dependencies; meant for llama.cpp / Ollama on localhost), or replay
a scripted mock — a JSON array of chat-completions response envelopes, in order,
the shape of mini-swe-agent's `DeterministicModel`.

**Interactive (the default):** with no `--task`, `i5h` is a REPL. Type a task
per line and the agent runs it, **keeping the conversation across turns**;
Ctrl-D or `exit` quits.

```bash
mkdir -p /tmp/ws
cargo run -p h5i-wasm-harness --bin i5h -- \
  --model-url http://127.0.0.1:8080/v1/chat/completions --workdir /tmp/ws
# » create hello.txt containing hi
# created hello.txt
# » now read it back
# the file says hi
```

**One-shot:** pass `--task` for a single scriptable run (exit non-zero on
failure). Add `--trace` for `[model call]` / `[tool]` lines on stderr, or
`--dump` to print the deterministic transcript instead of the final message.

```bash
cat > /tmp/replies.json <<'JSON'
[
  {"choices":[{"message":{"role":"assistant","content":null,
    "tool_calls":[{"id":"c1","type":"function",
      "function":{"name":"write_file",
        "arguments":"{\"path\":\"hello.txt\",\"content\":\"hi\"}"}}]}}]},
  {"choices":[{"message":{"role":"assistant","content":"created hello.txt"}}]}
]
JSON

cargo run -p h5i-wasm-harness --bin i5h -- \
  --task "create hello.txt containing hi" \
  --script /tmp/replies.json --workdir /tmp/ws --trace
cat /tmp/ws/hello.txt   # -> hi
```

Multi-turn works because the core exposes `Agent::resume(task)`, which appends a
user turn and keeps the whole history; the interactive host calls it after each
`Done`. With a real `--model-url`, tokens **render live** as the response
streams (`--no-stream` falls back to one blocking request). It's still not a full
TUI — no transcript view or per-step approval yet (see limitations).

To drive it with a real hosted model (Gemini/Vertex, OpenAI, …) despite the
http-only client, run one of the small local proxies in [`adapters/`](adapters/README.md):
they terminate `i5h`'s plain http and forward to the provider over HTTPS+auth.
No credential lives in this repo — the proxies read a key from a gitignored path.

## Build the WebAssembly module

```bash
rustup target add wasm32-unknown-unknown        # one time
crates/h5i-wasm-harness/scripts/build-wasm.sh   # -> build/h5i_wasm_harness.wasm (~127 KB)
```

No `-Zbuild-std`, no nightly, no network: because the core is `#![no_std]` +
`alloc` with zero dependencies, the stock target's prebuilt `core`/`alloc` are
enough. The module has **no imports** and seven exports: `memory`, `alloc`,
`dealloc`, `agent_init`, `agent_step`, `agent_resume` (continue the conversation
with a new user turn), and `agent_dump`. The ABI: the host calls `alloc(n)`,
writes UTF-8 JSON into the module's memory, calls an export with `(ptr, len)`;
every export returns a packed `u64` = `(ptr << 32) | len` pointing at
guest-owned JSON valid until the next call, which the host copies out. A browser
reads that `u64` with `BigInt` shifts; a WASI host reads linear memory directly.
(No JS glue is bundled — the boundary is small enough to write in a few lines
against whatever runtime you have. Note that response *streaming* is a host
concern: the module always takes one complete envelope, so a browser host does
the `fetch`/SSE and reassembly, exactly as `i5h` does.)

## Tests

```bash
cargo test -p h5i-wasm-harness
```

Covers the JSON codec (roundtrip, adversarial vectors: deep nesting, lone
surrogates, number overflow, control chars), the agent loop (full write→done
trace, buffered-sequential parallel calls, recoverable invalid calls with a
format-error cap, retry only on 429/5xx/transport, step limit, call-id-mismatch
fatal, multi-turn `resume`), the boundary roundtrip (including `agent_resume`),
the streaming reassembly (chunk decode, SSE split, delta merge into one
envelope), and the real-FS tools with path confinement. `tests/session.rs`
drives a full scripted session through the exact `init`/`step`/`dump` string
interface the wasm module exposes.

## What it borrows

- **mini-swe-agent** (`src/minisweagent/agents/default.py`, `models/test_models.py`)
  — the loop shape (query → execute actions until an exit condition, step and
  cost limits) and the deterministic scripted-model idea for the mock. What it
  drops: the single-`bash`-tool contract and the `COMPLETE_TASK...` sentinel,
  which do not survive the wasm boundary.
- **hax** (`src/agent_loop.h`, tool-error handling) — the structural
  termination rule: the run ends when the model stops calling tools, rather than
  on a magic string; and answering an invalid tool call with an error result the
  model can recover from.
- **Wasm Agents Blueprint** — the evidence for putting the model call in the
  host (the browser reality is `fetch` against an OpenAI-compatible endpoint,
  and CORS is a host concern), and middle-out output truncation so errors at the
  end of long tool output survive.

## Current limitations

- The `i5h` REPL renders streamed content live but is not a full TUI: no
  transcript view, no per-step approval, and tool output is not reflowed.
- One wasm agent session per module instance (static state); re-instantiate to
  reset. (Multi-turn within a session works via `agent_resume`.)
- The wasm bump allocator never frees; a long session grows memory monotonically.
- Model interface is OpenAI-compatible chat-completions only; streaming is
  reassembled host-side into one envelope (the core stays non-streaming), and
  there is no cost accounting or history compaction.
- Tools are `read_file` / `write_file` / `list_dir`. `bash` is in the schema but
  no bundled host declares it (there is no shell in a browser or WASI p1).
- The `i5h` HTTP client is `http://` only (no TLS without a dependency) — fine
  for a localhost model server, not for hosted APIs; a browser/WASI host uses
  its own `fetch`/runtime for https.
- No JS/WASI host glue is shipped in-tree yet; the ABI section above is the
  contract to write it against.
