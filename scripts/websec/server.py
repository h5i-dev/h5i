#!/usr/bin/env python3
"""A deliberately small target for the websec smoke tests.

Four behaviours, each the smallest thing that exercises one verb end to end:
an IDOR the tool should find, an ownership check it should *not* be able to
break, a single-use CSRF token that makes a two-step flow necessary, and a page
with subresources so the request log has something to be narrowed.

Not a benchmark. The benchmark the roadmap asks for (docs/design/design-websec.md
W20) is a corpus of real problems; this is the fixture that proves the verbs
work at all.
"""
import http.server
import json
import secrets
import sys
from urllib.parse import parse_qs, urlparse

PORT = int(sys.argv[1])

# Two users, one document each. `/profile` does not check who is asking (the
# IDOR); `/doc` does (the negative case that has to stay negative).
SESSIONS = {"alice-token": "alice", "bob-token": "bob"}
USERS = {
    "1": {"id": 1, "name": "alice", "email": "alice@example.test", "role": "user"},
    "2": {"id": 2, "name": "bob", "email": "bob@example.test", "role": "admin"},
}
DOCS = {"1": {"owner": "alice", "secret": "alice's diary"},
        "2": {"owner": "bob", "secret": "bob's diary"}}
ISSUED = set()


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, code, body, kind="application/json", extra=()):
        self.send_response(code)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def who(self):
        cookie = self.headers.get("Cookie", "")
        if "session=" not in cookie:
            return None
        return SESSIONS.get(cookie.split("session=")[-1].split(";")[0])

    def do_GET(self):
        parsed = urlparse(self.path)
        query = parse_qs(parsed.query)
        path = parsed.path

        if path == "/login":
            who = query.get("who", ["alice"])[0]
            self.reply(200, b"logged in", "text/plain",
                       [("Set-Cookie", f"session={who}-token; Path=/")])

        elif path == "/profile":
            # No ownership check: whoever asks gets whatever id they name.
            want = query.get("user_id", ["1"])[0]
            found = USERS.get(want)
            self.reply(200 if found else 404,
                       json.dumps(found or {"error": "no such user"}).encode())

        elif path == "/doc":
            who, want = self.who(), query.get("id", ["1"])[0]
            doc = DOCS.get(want)
            if who is None:
                self.reply(401, b'{"error":"who are you"}')
            elif doc is None:
                self.reply(404, b'{"error":"no such doc"}')
            elif doc["owner"] != who:
                self.reply(403, json.dumps({"error": "not yours"}).encode())
            else:
                self.reply(200, json.dumps(doc).encode())

        elif path == "/form":
            token = "tok_" + secrets.token_hex(4)
            ISSUED.add(token)
            self.reply(200, f'<html><body><form>'
                            f'<input name="csrf" value="{token}"></form></body></html>'.encode(),
                       "text/html")

        elif path == "/settings":
            token = self.headers.get("X-CSRF-Token", "")
            if token not in ISSUED:
                self.reply(403, json.dumps({"error": "bad or missing csrf token"}).encode())
            else:
                ISSUED.discard(token)  # single use, like a real one
                self.reply(200, json.dumps(
                    {"ok": True, "role": self.headers.get("X-Role", "user")}).encode())

        elif path == "/slow":
            # A server that decides slowly: the shape a blind, time-based
            # injection produces, and the only thing a timing test can see.
            import time
            time.sleep(0.4 if query.get("wait", ["0"])[0] == "1" else 0)
            self.reply(200, b'{"ok":true}')

        elif path == "/page":
            self.reply(200, b'<html><head><link rel="stylesheet" href="/a.css">'
                            b'<script src="/b.js"></script></head><body>hi</body></html>',
                       "text/html")

        elif path == "/missing":
            self.reply(404, b"gone", "text/plain")

        else:
            self.reply(200, b"x", "text/plain")

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
