#!/usr/bin/env python3
"""Local OpenAI-compatible → Vertex AI proxy, so `h5i-agent` can be tested against a
real Gemini model.

`h5i-agent` speaks OpenAI `chat/completions` over plain http and deliberately cannot
do TLS or OAuth (zero dependencies). This proxy terminates that http locally,
mints a short-lived Google OAuth access token from a service-account key, and
forwards each request to Vertex AI's *OpenAI-compatible* endpoint over HTTPS,
streaming the response straight back. No format translation is needed — Vertex's
endpoint already speaks OpenAI shape, including `tools` / `tool_calls`.

The service-account key never leaves this machine: it is read from a gitignored
path at runtime and only the derived bearer token is sent to Google. Nothing
secret is printed or written anywhere.

Run it with a Python that has `google-auth` and `requests` (e.g. GrantFuzz's
venv):

    VERTEX_SA_JSON=crates/h5i-wasm-harness/.secrets/vertex-sa.json \
    /path/to/python crates/h5i-wasm-harness/adapters/vertex_openai_proxy.py --port 8137

Then point h5i-agent-native at it (plain http, streams live):

    cargo run -p h5i-wasm-harness --bin h5i-agent-native -- \
      --model-url http://127.0.0.1:8137/v1/chat/completions \
      --task "create hello.txt containing hi" --workdir /tmp/ws --trace
"""

import argparse
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

import requests
from google.auth.transport.requests import Request as GoogleAuthRequest
from google.oauth2 import service_account

SCOPES = ["https://www.googleapis.com/auth/cloud-platform"]


def bearer(creds):
    """Return a valid access token, refreshing the service-account creds if needed."""
    if not creds.valid:
        creds.refresh(GoogleAuthRequest())
    return creds.token


class Handler(BaseHTTPRequestHandler):
    # HTTP/1.0 so the connection closes at end-of-response, which is exactly the
    # `Connection: close` + read-to-close shape h5i-agent's http client expects.
    protocol_version = "HTTP/1.0"

    def log_message(self, *_args):
        pass  # keep stdout/stderr clean; the model output is what matters

    def _allow_origin(self):
        # Let the local browser page (web/index.html) reach the proxy, but only
        # from a loopback origin — not any site the browser happens to visit,
        # which could otherwise use the credential this proxy holds.
        origin = self.headers.get("Origin")
        if origin and (origin.startswith("http://localhost") or origin.startswith("http://127.0.0.1")):
            return origin
        return None

    def _cors(self):
        origin = self._allow_origin()
        if origin:
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Access-Control-Allow-Headers", "Content-Type, Authorization")
            self.send_header("Access-Control-Allow-Methods", "POST, OPTIONS")
            self.send_header("Vary", "Origin")

    def do_OPTIONS(self):  # CORS preflight for the browser page
        self.send_response(204)
        self._cors()
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            self.send_error(400, "invalid JSON body")
            return

        # h5i-agent fills model with a placeholder; force the real Vertex model id.
        body["model"] = self.server.model
        streaming = bool(body.get("stream"))

        try:
            tok = bearer(self.server.creds)
        except Exception as e:  # noqa: BLE001 - surface auth failures to the client
            self.send_error(500, f"auth failed: {e}")
            return

        headers = {"Authorization": f"Bearer {tok}", "Content-Type": "application/json"}
        try:
            up = requests.post(
                self.server.endpoint,
                headers=headers,
                json=body,
                stream=streaming,
                timeout=180,
            )
        except requests.RequestException as e:
            self.send_error(502, f"upstream request failed: {e}")
            return

        self.send_response(up.status_code)
        self._cors()
        self.send_header("Content-Type", up.headers.get("Content-Type", "application/json"))
        self.send_header("Connection", "close")
        self.end_headers()
        if streaming:
            for chunk in up.iter_content(chunk_size=None):
                if chunk:
                    self.wfile.write(chunk)
                    self.wfile.flush()
        else:
            self.wfile.write(up.content)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8137)
    ap.add_argument(
        "--sa",
        default=os.environ.get(
            "VERTEX_SA_JSON", "crates/h5i-wasm-harness/.secrets/vertex-sa.json"
        ),
        help="path to the service-account JSON key (kept gitignored)",
    )
    ap.add_argument("--location", default=os.environ.get("VERTEX_LOCATION", "us-central1"))
    ap.add_argument(
        "--model",
        default=os.environ.get("VERTEX_MODEL", "google/gemini-2.5-pro"),
        help="Vertex OpenAI-compat model id (Google models are prefixed 'google/')",
    )
    args = ap.parse_args()

    if not os.path.exists(args.sa):
        sys.exit(f"service-account key not found: {args.sa}")

    creds = service_account.Credentials.from_service_account_file(args.sa, scopes=SCOPES)
    project = creds.project_id
    endpoint = (
        f"https://{args.location}-aiplatform.googleapis.com/v1beta1/"
        f"projects/{project}/locations/{args.location}/endpoints/openapi/chat/completions"
    )

    srv = HTTPServer((args.host, args.port), Handler)
    srv.creds = creds
    srv.endpoint = endpoint
    srv.model = args.model
    print(
        f"vertex proxy → http://{args.host}:{args.port}  "
        f"(model={args.model}, location={args.location})",
        file=sys.stderr,
    )
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
