#!/usr/bin/env python3
"""Serve a Web Platform Tests checkout, with the vendor reporter hook filled in.

WPT ships `resources/testharnessreport.js` as an empty seam for exactly this:
a vendor drops in code that collects results when a file finishes. We serve our
own rather than writing into the checkout, so the checkout stays a pristine
`git status` and can be shared with any other runner.

The results come back out through the console, because that is a channel the
engine already has and `open --json` already reports. Nothing new is added to
the engine to be measured, which matters: an instrument that requires the
subject to grow a port for it is measuring something other than the subject.
"""

import http.server
import os
import socketserver
import sys
import threading

WPT_ROOT = os.environ.get("WPT_ROOT", os.path.expanduser("~/Dev/wpt"))

# The marker is deliberately long and unlikely: console output is page-
# controlled, and a page that printed our marker could otherwise report its own
# score. Tests are trusted here, but the runner should not be forgeable by
# accident either.
MARKER = "H5I-WPT-RESULT-6a7f2c1b"

REPORTER = (
    """
add_completion_callback(function (tests, status) {
  var out = {
    status: status.status,
    message: status.message,
    tests: []
  };
  for (var i = 0; i < tests.length; i++) {
    out.tests.push({
      name: tests[i].name,
      status: tests[i].status,
      message: tests[i].message
    });
  }
  console.log("%s" + JSON.stringify(out));
});
"""
    % MARKER
)


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=WPT_ROOT, **kw)

    def do_GET(self):
        path = self.path.split("?")[0].split("#")[0]
        if path == "/resources/testharnessreport.js":
            body = REPORTER.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/javascript")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        return super().do_GET()

    def log_message(self, *a):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def start(port=0):
    """Start the server on a background thread. Returns the bound port."""
    httpd = Server(("127.0.0.1", port), Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    return httpd, httpd.server_address[1]


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    httpd, port = start(port)
    print(f"serving {WPT_ROOT} on http://127.0.0.1:{port}", flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
