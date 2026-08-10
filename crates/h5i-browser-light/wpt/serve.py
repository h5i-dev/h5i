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
import posixpath
import re
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


# ── generated endpoints ─────────────────────────────────────────────────────
#
# WPT keeps a large share of its tests as bare JavaScript and builds the HTML
# around them at serve time: `x.any.js` is served as `x.any.html`,
# `x.any.worker.html` and more, and none of those files exist on disk. Skipping
# them left 3,083 files — and the several thousand subtests inside them —
# outside every measurement this harness produced.
#
# Only the *window* wrapper is built here. The worker variants need Workers,
# which this engine does not have, and inventing an HTML page that pretends to
# be a worker scope would produce failures that blame the engine for the
# harness's fiction.

META = re.compile(r"^//\s*META:\s*([a-z]+)=(.*)$")


def directives(source):
    """The `// META:` lines at the top of a WPT script, as a list of pairs.

    They stop at the first line that is not a META comment, which is what
    wptserve does — a `// META:` further down is a comment, not a directive.
    """
    found = []
    for line in source.splitlines():
        match = META.match(line.strip())
        if not match:
            if line.strip().startswith("//") or not line.strip():
                continue
            break
        found.append((match.group(1), match.group(2).strip()))
    return found


def runs_in_window(source):
    """Whether this test has a window variant at all.

    `// META: global=worker` means exactly that, and building a window page for
    it would score a test the author never wrote.
    """
    for key, value in directives(source):
        if key == "global":
            scopes = {scope.strip() for scope in value.split(",")}
            return bool(scopes & {"window", "!dedicatedworker", "!worker"}) or not scopes
    return True


def wrapper_for(js_path: str, source: str) -> str:
    """The HTML wptserve would have generated for this script."""
    title = ""
    scripts = []
    for key, value in directives(source):
        if key == "title":
            title = value
        elif key == "script":
            scripts.append(value)

    base = posixpath.dirname(js_path)
    tags = []
    for script in scripts:
        src = script if script.startswith("/") else posixpath.normpath(posixpath.join(base, script))
        tags.append(f'<script src="{src}"></script>')

    return (
        "<!doctype html>\n<meta charset=utf-8>\n"
        f"<title>{title}</title>\n"
        '<script src="/resources/testharness.js"></script>\n'
        '<script src="/resources/testharnessreport.js"></script>\n'
        + "\n".join(tags)
        + '\n<div id="log"></div>\n'
        f'<script src="{js_path}"></script>\n'
    )


def generated_source(root: str, path: str):
    """The `.js` behind a generated `.html` endpoint, or None if there is none."""
    for suffix in (".any.html", ".window.html"):
        if not path.endswith(suffix):
            continue
        js_path = path[: -len(".html")] + ".js"
        on_disk = os.path.join(root, js_path.lstrip("/"))
        if not os.path.isfile(on_disk):
            return None
        try:
            with open(on_disk, encoding="utf8", errors="replace") as handle:
                source = handle.read()
        except OSError:
            return None
        if not runs_in_window(source):
            return None
        return js_path, source
    return None


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=WPT_ROOT, **kw)

    def do_GET(self):
        path = self.path.split("?")[0].split("#")[0]

        built = generated_source(WPT_ROOT, path)
        if built is not None:
            js_path, source = built
            body = wrapper_for(js_path, source).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # `resources/WebIDLParser.js` is a build artifact, not a checked-in
        # file: `webidl2/build.sh` copies the bundle there. A checkout that
        # never ran it 404s, and the 211 `idlharness` endpoints across WPT then
        # hang on a script that will never arrive and report a timeout that says
        # nothing about this engine. Served from the bundle that is present,
        # which is exactly what the build would have produced.
        if path == "/resources/WebIDLParser.js":
            bundle = os.path.join(WPT_ROOT, "resources", "webidl2", "lib", "webidl2.js")
            if os.path.isfile(bundle):
                with open(bundle, "rb") as handle:
                    body = handle.read()
                self.send_response(200)
                self.send_header("Content-Type", "text/javascript")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return

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
