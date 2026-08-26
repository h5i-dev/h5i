# Adapters — testing `i5h` against real models

`i5h` speaks OpenAI `chat/completions` over **plain http** and deliberately has
no TLS or auth (zero dependencies). These small local proxies bridge that to a
real provider so you can drive the harness with a live model.

## `vertex_openai_proxy.py` — Google Gemini via Vertex AI

Bridges `http://127.0.0.1:PORT` → Vertex AI's **OpenAI-compatible** endpoint over
HTTPS. It mints a short-lived OAuth token from a Google **service-account key**
and forwards each request unchanged (Vertex already speaks OpenAI shape, tools
included), streaming the response straight back. No format translation.

Needs a Python with `google-auth` and `requests`.

```bash
# 1. Provide a service-account JSON key at a gitignored path (see security note).
#    e.g. crates/h5i-wasm-harness/.secrets/vertex-sa.json

# 2. Run the proxy (any Python with google-auth + requests):
VERTEX_SA_JSON=crates/h5i-wasm-harness/.secrets/vertex-sa.json \
  python3 crates/h5i-wasm-harness/adapters/vertex_openai_proxy.py --port 8137

# 3. Point i5h at it — plain http, streams live, real tool-calling:
cargo run -p h5i-wasm-harness --bin i5h -- \
  --model-url http://127.0.0.1:8137/v1/chat/completions \
  --task "create note.txt containing hi, then read it back" \
  --workdir /tmp/ws --trace
```

Config via flags or env: `--model` / `VERTEX_MODEL` (default `google/gemini-2.5-pro`),
`--location` / `VERTEX_LOCATION` (default `us-central1`), `--sa` / `VERTEX_SA_JSON`.
The project id is read from the key file.

### Security

- **No credential is stored in this repo.** The service-account key is read from
  a **gitignored** path at runtime (`/.secrets/` is in the crate `.gitignore`).
  Supply your own key there; it is never committed, and only the derived bearer
  token — not the key — is ever sent to Google.
- The proxy binds `127.0.0.1` only and prints nothing secret.
- If you rotate or revoke the key, just replace the file; the code has no key
  material in it.
