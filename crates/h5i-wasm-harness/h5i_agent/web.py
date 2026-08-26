"""`h5i-agent web`: run the agent in your browser.

The page (web/index.html) drives the loop in the tab: it instantiates the module
with plain `WebAssembly`, calls the model over `fetch`, and runs the tools
against an in-memory filesystem. It only needs the files served over http
(module scripts and the .wasm do not load from file://), which is all this does:
lay the bundled assets out in a temp directory, serve it, and open the page.
"""

import argparse
import functools
import http.server
import os
import shutil
import sys
import tempfile
import webbrowser

from . import _assets


def _serve_root():
    """A temp directory laid out as the page expects: web/ and build/."""
    root = tempfile.mkdtemp(prefix="h5i-agent-web-")
    os.makedirs(os.path.join(root, "web"))
    os.makedirs(os.path.join(root, "build"))
    shutil.copy(_assets.path("index.html"), os.path.join(root, "web", "index.html"))
    shutil.copy(_assets.path("host.mjs"), os.path.join(root, "web", "host.mjs"))
    shutil.copy(_assets.path("h5i-agent.wasm"), os.path.join(root, "build", "h5i-agent.wasm"))
    return root


def main(argv=None):
    ap = argparse.ArgumentParser(prog="h5i-agent web", description="Run the agent in a browser.")
    ap.add_argument("--port", type=int, default=8000, help="port to serve on (default 8000)")
    ap.add_argument("--no-open", action="store_true", help="do not open a browser automatically")
    args = ap.parse_args(argv)

    root = _serve_root()
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=root)
    # Loopback only: this serves local files and the browser makes the model
    # calls, so there is no reason to expose it on the network.
    try:
        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    except OSError as e:
        sys.exit(f"could not bind 127.0.0.1:{args.port} ({e}). Try --port with a free port.")

    url = f"http://localhost:{args.port}/web/"
    print(f"\n  h5i-agent console  ->  {url}")
    print("  offline scripted demo by default; type /model <url> in the page for a live endpoint.")
    print("  Ctrl-C to stop.\n")

    if not args.no_open:
        webbrowser.open_new_tab(url)

    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped.")
    finally:
        httpd.server_close()
        shutil.rmtree(root, ignore_errors=True)
