#!/usr/bin/env python3
"""A dependency-free shared counter for demonstrating `h5i box share`.

Every connected browser reads and updates the same in-memory counter. The app
uses only Python's standard library, so it can run in a fresh h5i sandbox
without downloading packages.
"""

from __future__ import annotations

import argparse
import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


HTML = r"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>h5i P2P Counter</title>
  <style>
    :root {
      color-scheme: dark;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont,
        "Segoe UI", sans-serif;
      background: #080b12;
      color: #f8fafc;
    }
    * { box-sizing: border-box; }
    body {
      min-height: 100vh;
      margin: 0;
      display: grid;
      place-items: center;
      padding: 24px;
      background:
        radial-gradient(circle at 20% 10%, #16305f 0, transparent 35%),
        radial-gradient(circle at 80% 90%, #124f47 0, transparent 35%),
        #080b12;
    }
    main {
      width: min(100%, 500px);
      padding: 42px;
      text-align: center;
      border: 1px solid rgba(148, 163, 184, .22);
      border-radius: 28px;
      background: rgba(15, 23, 42, .82);
      box-shadow: 0 24px 80px rgba(0, 0, 0, .42);
      backdrop-filter: blur(18px);
    }
    .eyebrow {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      margin-bottom: 16px;
      color: #7dd3fc;
      font-size: 13px;
      font-weight: 700;
      letter-spacing: .12em;
      text-transform: uppercase;
    }
    .dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: #34d399;
      box-shadow: 0 0 14px #34d399;
    }
    h1 { margin: 0; font-size: clamp(28px, 8vw, 44px); letter-spacing: -.04em; }
    .description {
      max-width: 380px;
      margin: 12px auto 30px;
      color: #a8b3c7;
      line-height: 1.6;
    }
    #count {
      min-height: 138px;
      display: grid;
      place-items: center;
      font-size: clamp(82px, 25vw, 132px);
      font-variant-numeric: tabular-nums;
      font-weight: 800;
      line-height: 1;
      letter-spacing: -.08em;
      background: linear-gradient(135deg, #f8fafc, #7dd3fc 55%, #34d399);
      -webkit-background-clip: text;
      background-clip: text;
      color: transparent;
      transition: transform .12s ease;
    }
    #count.bump { transform: scale(1.08); }
    .controls { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
    button {
      appearance: none;
      border: 1px solid rgba(125, 211, 252, .35);
      border-radius: 14px;
      padding: 14px 18px;
      color: #f8fafc;
      background: rgba(30, 41, 59, .9);
      font: inherit;
      font-size: 18px;
      font-weight: 700;
      cursor: pointer;
      transition: transform .12s ease, border-color .12s ease, background .12s ease;
    }
    button:hover { transform: translateY(-2px); border-color: #7dd3fc; }
    button:active { transform: translateY(0); }
    button.primary {
      border-color: transparent;
      color: #052e2b;
      background: linear-gradient(135deg, #7dd3fc, #34d399);
    }
    button.reset {
      grid-column: 1 / -1;
      padding: 10px;
      border-color: transparent;
      color: #94a3b8;
      background: transparent;
      font-size: 14px;
      font-weight: 600;
    }
    #status {
      margin-top: 22px;
      color: #94a3b8;
      font-size: 13px;
    }
    #status.offline { color: #fca5a5; }
    @media (max-width: 520px) { main { padding: 32px 22px; } }
  </style>
</head>
<body>
  <main>
    <div class="eyebrow"><span class="dot"></span> h5i P2P demo</div>
    <h1>Shared counter</h1>
    <p class="description">
      Update it from any connected browser. Everyone sees the same value.
    </p>
    <div id="count" aria-live="polite">0</div>
    <div class="controls">
      <button type="button" data-action="decrement">&minus; 1</button>
      <button type="button" class="primary" data-action="increment">+ 1</button>
      <button type="button" class="reset" data-action="reset">Reset counter</button>
    </div>
    <div id="status">Connecting…</div>
  </main>
  <script>
    const count = document.querySelector("#count");
    const status = document.querySelector("#status");
    let lastVersion = -1;
    let polling = false;

    function render(data) {
      if (data.version !== lastVersion) {
        count.textContent = data.count;
        count.classList.remove("bump");
        void count.offsetWidth;
        count.classList.add("bump");
        lastVersion = data.version;
      }
      status.textContent = "Connected · updates are shared live";
      status.classList.remove("offline");
    }

    async function request(path, method = "GET") {
      const response = await fetch(path, { method, cache: "no-store" });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return response.json();
    }

    async function refresh() {
      if (polling) return;
      polling = true;
      try {
        render(await request("/api/count"));
      } catch (error) {
        status.textContent = "Connection lost · retrying…";
        status.classList.add("offline");
      } finally {
        polling = false;
      }
    }

    document.querySelectorAll("button[data-action]").forEach((button) => {
      button.addEventListener("click", async () => {
        button.disabled = true;
        try {
          render(await request(`/api/${button.dataset.action}`, "POST"));
        } catch (error) {
          status.textContent = "Update failed · retrying…";
          status.classList.add("offline");
        } finally {
          button.disabled = false;
        }
      });
    });

    refresh();
    setInterval(refresh, 600);
  </script>
</body>
</html>
"""


class Counter:
    """A small thread-safe, in-memory counter."""

    def __init__(self) -> None:
        self._value = 0
        self._version = 0
        self._lock = threading.Lock()

    def snapshot(self) -> dict[str, int]:
        with self._lock:
            return {"count": self._value, "version": self._version}

    def change(self, delta: int) -> dict[str, int]:
        with self._lock:
            self._value += delta
            self._version += 1
            return {"count": self._value, "version": self._version}

    def reset(self) -> dict[str, int]:
        with self._lock:
            self._value = 0
            self._version += 1
            return {"count": self._value, "version": self._version}


class CounterHandler(BaseHTTPRequestHandler):
    counter = Counter()

    def do_GET(self) -> None:  # noqa: N802 - required by BaseHTTPRequestHandler
        path = self.path.partition("?")[0]
        if path == "/":
            self._send_bytes(HTML.encode(), "text/html; charset=utf-8")
        elif path == "/api/count":
            self._send_json(self.counter.snapshot())
        elif path == "/favicon.ico":
            self.send_error(HTTPStatus.NOT_FOUND)
        else:
            self.send_error(HTTPStatus.NOT_FOUND, "Not found")

    def do_POST(self) -> None:  # noqa: N802 - required by BaseHTTPRequestHandler
        path = self.path.partition("?")[0]
        actions = {
            "/api/increment": lambda: self.counter.change(1),
            "/api/decrement": lambda: self.counter.change(-1),
            "/api/reset": self.counter.reset,
        }
        action = actions.get(path)
        if action is None:
            self.send_error(HTTPStatus.NOT_FOUND, "Not found")
            return
        self._send_json(action())

    def _send_json(self, payload: dict[str, Any]) -> None:
        self._send_bytes(
            json.dumps(payload, separators=(",", ":")).encode(),
            "application/json; charset=utf-8",
        )

    def _send_bytes(self, body: bytes, content_type: str) -> None:
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: Any) -> None:
        # Polling /api/count every 600 ms would otherwise obscure useful logs.
        if not (self.command == "GET" and self.path.startswith("/api/count")):
            super().log_message(format, *args)


class CounterServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run the h5i P2P counter demo")
    parser.add_argument("--host", default="127.0.0.1", help="address to bind (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=3000, help="port to bind (default: 3000)")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    server = CounterServer((args.host, args.port), CounterHandler)
    print(f"h5i P2P counter listening on http://{args.host}:{args.port}", flush=True)
    print("Press Ctrl+C to stop.", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopping counter…", flush=True)
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
