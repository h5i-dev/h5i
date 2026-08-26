# Running the harness in a browser

The `.wasm` module has no imports, so a browser can load it with plain
`WebAssembly.instantiate` and drive the agent loop from JavaScript — the model
call goes through `fetch`, the tools run against an in-memory filesystem.

- **`host.mjs`** — the host loop, environment-agnostic. It decodes the module's
  `(ptr << 32) | len` return convention, dispatches `Effect`s, and ships helpers
  (an in-memory FS, a scripted mock model, a `fetch`-based real model). The same
  file runs under Node and in the browser.
- **`index.html`** — a small page that uses `host.mjs`: a task box, a live view
  of model calls / tool calls / the final answer, and the in-memory workspace.
- **`node-demo.mjs`** — the same loop under Node, as a check.

## Try it

```bash
# 1. Build the module (writes ../build/h5i_wasm_harness.wasm)
crates/h5i-wasm-harness/scripts/build-wasm.sh

# 2. Serve the crate directory (module scripts and .wasm need http, not file://)
cd crates/h5i-wasm-harness && python3 -m http.server 8000

# 3. Open the page
#    http://localhost:8000/web/
```

It opens in **Mock** mode: an offline scripted model that writes `hello.txt` and
reads it back, so you can watch the whole loop run with no network. Switch to
**Live endpoint** to point it at an OpenAI-compatible URL.

## Verify without a browser

`host.mjs` is the same code the page runs, so Node proves the module works
end-to-end (it uses the identical `WebAssembly` API):

```bash
node crates/h5i-wasm-harness/web/node-demo.mjs
# effects: call_model → run_tool → call_model → run_tool → call_model → done
# OK — the wasm module runs end-to-end through the JS host loop (incl. resume).
```

## Live models and CORS

A browser enforces CORS, so a live endpoint must send permissive CORS headers.
A local `llama.cpp` / Ollama server usually does; a hosted API does not, and
neither does the Vertex/Gemini proxy in [`../adapters/`](../adapters/README.md)
as written (it is meant for the `i5h` CLI). To use a real hosted model from the
page, front it with a proxy that adds `Access-Control-Allow-Origin`. The module
itself never makes a request — all networking is the host's, so this is purely a
host-side concern.

## Notes

- `host.mjs` re-views `memory.buffer` before every access because the module's
  bump allocator can call `memory.grow`, which detaches the old `ArrayBuffer`.
- Each **Run** instantiates a fresh module (the allocator never frees). For a
  persistent multi-turn chat, keep one instance and call `agent.resume(task)` —
  `node-demo.mjs` shows both.
- No bundler, no dependencies: two files and the `.wasm`.
