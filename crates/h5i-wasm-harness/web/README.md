# Running the harness in a browser

The `.wasm` module has no imports, so a browser loads it with plain
`WebAssembly.instantiate` and drives the agent loop from JavaScript: the model
call goes through `fetch`, the tools run against an in-memory filesystem.

- **`host.mjs`** — the host loop, environment-agnostic. It decodes the module's
  `(ptr << 32) | len` return convention, dispatches `Effect`s, and ships helpers
  (an in-memory FS, a scripted mock model, and `fetch`-based real models, one of
  which streams SSE and renders tokens live). The same file runs under Node and
  in the browser.
- **`index.html`** — a terminal-style REPL over `host.mjs`: a prompt, streaming
  output, tool calls as terminal lines, and slash commands (`/model <url>`,
  `/mock`, `/open`, `/files`, `/reset`, `/clear`, `/help`). Multi-turn by default.
- **`node-demo.mjs`** — the same loop under Node, as a check.

## Try it

From the crate directory, `just web` does all three steps and prints the URL:

```bash
just web
# open the printed http://localhost:8000/web/
```

Or the same steps by hand:

```bash
# 1. Build the module (writes ../build/h5i-agent.wasm)
crates/h5i-wasm-harness/scripts/build-wasm.sh

# 2. Serve the crate directory (module scripts and .wasm need http, not file://)
( cd crates/h5i-wasm-harness && python3 -m http.server 8000 )

# 3. Open the page
#    http://localhost:8000/web/
```

It opens in an offline **mock** mode: a scripted model that writes `hello.txt`
and reads it back, so you can watch the whole loop run with no network. Type
`/model http://localhost:8080/v1/chat/completions` to point it at a live
OpenAI-compatible endpoint, and tokens stream in live.

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
A local `llama.cpp` / Ollama server usually does. The Vertex/Gemini proxy in
[`../adapters/`](../adapters/README.md) sends them for **loopback origins**
(`http://localhost:*` / `http://127.0.0.1:*`), so the page can use it directly:
run the proxy, then `/model http://127.0.0.1:8137/v1/chat/completions`. A hosted
API reached straight from the page will be blocked unless something in front of
it adds `Access-Control-Allow-Origin`. The module itself never makes a request;
all networking is the host's, so this is purely a host-side concern.

## Notes

- `host.mjs` re-views `memory.buffer` before every access because the module's
  bump allocator can call `memory.grow`, which detaches the old `ArrayBuffer`.
- In live mode the page keeps one instance and calls `agent.resume(task)` for
  each turn (a real conversation); mock mode instantiates a fresh module per
  submit, since the allocator never frees. `node-demo.mjs` shows both paths.
- No bundler, no dependencies: two files and the `.wasm`.

## Workspace: memory or a real folder

By default the file tools (`read_file` / `write_file` / `list_dir`) run against
an **in-memory** map that lives only in the tab and is cleared on reload. Nothing
touches your disk.

Type **`/open`** (or the "open folder…" button) to pick a real local directory
via the **File System Access API** (Chromium/Edge). After you grant it, the same
three tools operate on that actual folder — the browser's sanctioned equivalent
of the CLI's `--workdir`. Paths stay confined to the chosen directory (absolute
paths and `..` escapes are rejected). Firefox/Safari have no such API, so they
stay in-memory. There is still no `bash` in the browser — there is no shell to
give it.
