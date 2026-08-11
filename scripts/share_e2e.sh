#!/usr/bin/env bash
#
# End-to-end checks for `h5i box share`, against a real box.
#
# These are the things that kept breaking and that the unit tests cannot see,
# because they live in the seams between the CLI, `run.rs`, the signal
# handlers and a real network: a first Ctrl-C during the teardown, a joiner
# nobody has visited yet, a client that half-closes, `stop` racing `grant`.
# Every one of them was found by a reviewer and then verified by hand with a
# stopwatch, five rounds running. This is that by-hand check, written down.
#
# Not a `cargo test`: it needs a real box, a real network namespace, and
# (for the tunnel half) `cloudflared` and the internet. Run it deliberately.
#
#   scripts/share_e2e.sh           # everything that works offline
#   scripts/share_e2e.sh --tunnel  # and the Cloudflare path as well
#
# Exits non-zero on the first failure and leaves the box behind for inspection.

set -uo pipefail

H5I="${H5I:-./target/debug/h5i}"
BOX="${BOX:-e2e-share}"
PORT=3000
WITH_TUNNEL=0
[ "${1:-}" = "--tunnel" ] && WITH_TUNNEL=1

WORK="$(mktemp -d)"
FAILED=0

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pass() { printf '  \033[32mok\033[0m   %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAILED=$((FAILED + 1)); }
check() { if [ "$1" = "$2" ]; then pass "$3"; else fail "$3 (wanted $2, got $1)"; fi; }

cleanup() {
  [ -n "${JOIN_PID:-}" ] && kill "$JOIN_PID" 2>/dev/null
  "$H5I" box share stop "$BOX" --force >/dev/null 2>&1
  for p in $(pgrep -f "box run $BOX"); do kill "$p" 2>/dev/null; done
  # Kept on failure: the logs in it are the only account of what happened.
  if [ "$FAILED" = 0 ]; then rm -rf "$WORK"; else echo "logs left in $WORK"; fi
}
trap cleanup EXIT

# ── a box with a dev server in it ───────────────────────────────────────────

say "setting up"
[ -x "$H5I" ] || { echo "no h5i binary at $H5I — cargo build first"; exit 2; }

# A previous failed run leaves the box behind on purpose, and its dev server
# with it — which holds the box busy, so `rm` refuses until that is gone.
for p in $(pgrep -f "box run $BOX"); do kill "$p" 2>/dev/null; done
sleep 1
"$H5I" box rm "$BOX" --force >/dev/null 2>&1
git worktree prune 2>/dev/null
"$H5I" box --new --name "$BOX" >/dev/null 2>&1 || {
  echo "could not create a box — is one called $BOX still running? \`h5i box ls\`"
  exit 2
}
ENV_DIR="$(git rev-parse --git-dir)/.h5i/env/human/$BOX"

# Answers with four megabytes, so a truncation is visible rather than a matter
# of counting bytes in a header.
cat > "$WORK/serve.py" <<'PY'
import http.server, socketserver
BODY = b'b' * (4 * 1024 * 1024)
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)
    def log_message(self, *a): pass
socketserver.TCPServer.allow_reuse_address = True
socketserver.ThreadingTCPServer(("127.0.0.1", 3000), H).serve_forever()
PY
cp "$WORK/serve.py" "$ENV_DIR/work/serve.py"
"$H5I" box run "$BOX" -- pwd >/dev/null 2>&1
("$H5I" box run "$BOX" -- python3 serve.py >/dev/null 2>&1 &)
sleep 8
pass "a box with a dev server on port $PORT"

receipts() {
  python3 - "$ENV_DIR/receipt.jsonl" <<'PY'
import json, sys
try:
    print(sum(1 for l in open(sys.argv[1]) if json.loads(l).get("source") == "share"))
except FileNotFoundError:
    print(0)
PY
}

# By pid, not by pattern: a joiner left behind by a previous run would
# otherwise be counted as this one, and a check that passes because somebody
# else's process is alive is worse than no check.
joiner_alive() { kill -0 "$JOIN_PID" 2>/dev/null; }

# `share ls` says "No box on this clone is being shared." when there is none.
no_record() { "$H5I" box share ls 2>/dev/null | grep -q "No box on this clone"; }

share_pid() { "$H5I" box share status "$BOX" 2>/dev/null | sed -n 's/.*pid \([0-9]*\).*/\1/p' | head -1; }

# ── peer to peer ────────────────────────────────────────────────────────────

say "peer to peer"
setsid "$H5I" box share "$BOX" --port $PORT --expire 10m --label e2e > "$WORK/share.log" 2>&1 &
sleep 12
TICKET="$(grep -o 'h5i1_[A-Za-z0-9_-]*' "$WORK/share.log" | head -1)"
[ -n "$TICKET" ] && pass "the share announced a ticket" || {
  fail "no ticket"; sed -n '1,6p' "$WORK/share.log"; exit 1; }

"$H5I" join "$TICKET" --port 8899 > "$WORK/join.log" 2>&1 &
JOIN_PID=$!
sleep 8
grep -q "joined" "$WORK/join.log" && pass "the joiner joined" || fail "the joiner did not join"
TOKEN="$(grep -o 'h5i=[a-f0-9]*' "$WORK/join.log" | head -1 | cut -d= -f2)"

# The joiner must survive with nobody visiting it. It presents its ticket once
# at connect time for exactly this reason; without that the sharer hangs up
# after thirty seconds and `h5i join` exits with "no ticket was presented".
say "a joiner nobody has visited yet"
sleep 40
joiner_alive && pass "still connected past the unauthenticated grace" || fail "the sharer hung up on an idle joiner"

say "a whole response, and a client that half-closes"
# `-c`, because the first request is answered with a redirect that sets the
# cookie; without a jar the followed request arrives anonymous and gets a 401.
GOT=$(curl -sL -c "$WORK/jar" -o /dev/null -w '%{size_download}' --max-time 90 "http://127.0.0.1:8899/?h5i=$TOKEN")
check "$GOT" "4194304" "4 MiB over p2p"

# `nc -q` shuts down its write side after sending. That is legal HTTP/1.1 and
# is what anything built out of one write and one read does; it used to be read
# as "the visitor left" and the download stopped after the first read.
HALF=$(printf "GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share_8899=$TOKEN\r\n\r\n" \
       | timeout 90 nc -q 60 127.0.0.1 8899 | wc -c)
if [ "$HALF" -gt 4194304 ]; then pass "a half-closing client got the whole body ($HALF bytes)"; else fail "half-close truncated the response ($HALF bytes)"; fi

say "an anonymous request is refused without killing anything"
CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 "http://127.0.0.1:8899/")
check "$CODE" "401" "no credential gets a 401"
joiner_alive && pass "the joiner survived it" || fail "an anonymous request ended the join"

kill "$JOIN_PID" 2>/dev/null

# ── the verbs, and the windows between them ─────────────────────────────────

say "stop, and the window after it"
BEFORE=$(receipts)
"$H5I" box share stop "$BOX" >/dev/null 2>&1
# A grant landing in the gap used to mint a doomed ticket — and worse, the new
# grant was live, which is the condition the serving process polls for, so a
# stopped share could come back from the dead.
OUT=$("$H5I" box share grant "$BOX" --label late 2>&1)
echo "$OUT" | grep -q "shutting down" && pass "a grant racing the stop is refused" || fail "a racing grant was accepted: $OUT"
sleep 5
AFTER=$(receipts)
[ "$AFTER" -gt "$BEFORE" ] && pass "stopping wrote a receipt" || fail "stopping wrote no receipt"
no_record && pass "the record was cleared" || fail "a record survived the stop"

# ── signals ─────────────────────────────────────────────────────────────────

say "a plain Ctrl-C on a serving share"
BEFORE=$(receipts)
setsid "$H5I" box share "$BOX" --port $PORT --expire 10m > "$WORK/s2.log" 2>&1 &
sleep 12
P=$(share_pid)
[ -n "$P" ] && kill -INT "$P" || fail "no share to interrupt"
sleep 7
AFTER=$(receipts)
[ "$AFTER" -gt "$BEFORE" ] && pass "Ctrl-C wrote a receipt" || fail "Ctrl-C lost the receipt"
no_record && pass "and cleared the record" || fail "Ctrl-C left a record behind"

# The regression that made this file worth writing: arming the hard-exit
# watcher after the select meant the operator's FIRST Ctrl-C hit a watcher
# built for their second — "interrupted again", no receipt, exit. Pressing it
# once to get a prompt back destroyed the artifact the feature exists for.
say "a first Ctrl-C during the teardown"
BEFORE=$(receipts)
setsid "$H5I" box share "$BOX" --port $PORT --expire 10m > "$WORK/s3.log" 2>&1 &
sleep 12
P=$(share_pid)
"$H5I" box share stop "$BOX" >/dev/null 2>&1
sleep 0.4
kill -INT "$P" 2>/dev/null
sleep 6
AFTER=$(receipts)
[ "$AFTER" -gt "$BEFORE" ] && pass "the receipt survived an interrupt mid-teardown" || fail "an interrupt during the teardown lost the receipt"
grep -q "interrupted again" "$WORK/s3.log" && fail "a first interrupt was treated as a second" || pass "and was not called a second interrupt"

# ── the tunnel ──────────────────────────────────────────────────────────────

if [ "$WITH_TUNNEL" = "1" ]; then
  say "cloudflare quick tunnel"
  command -v cloudflared >/dev/null || { fail "cloudflared is not installed"; }
  setsid "$H5I" box share "$BOX" --port $PORT --tunnel --expire 10m > "$WORK/t.log" 2>&1 &
  sleep 32
  URL="$(grep -o 'https://[^ ]*' "$WORK/t.log" | head -1)"
  [ -n "$URL" ] && pass "the tunnel announced a URL" || fail "no tunnel URL"
  GOT=$(curl -sL -c "$WORK/jar" -o /dev/null -w '%{size_download}' --max-time 120 "$URL")
  check "$GOT" "4194304" "4 MiB over the internet"
  SLOW=$(curl -s -b "$WORK/jar" --limit-rate 800k -o /dev/null -w '%{size_download}' --max-time 120 "${URL%%\?*}/")
  check "$SLOW" "4194304" "and to a client that reads slowly"
  ANON=$(curl -s -o /dev/null -w '%{http_code}' --max-time 60 "${URL%%\?*}/")
  check "$ANON" "401" "an anonymous visitor gets a 401"
  "$H5I" box share stop "$BOX" >/dev/null 2>&1
  sleep 5
fi

# ── the receipt says what happened ──────────────────────────────────────────

say "the receipt"
python3 - "$ENV_DIR" <<'PY'
import json, os, sys
d = sys.argv[1]
rs = [json.loads(l) for l in open(os.path.join(d, "receipt.jsonl")) if json.loads(l).get("source") == "share"]
last = rs[-1]
blob = os.path.join(d, "receipts", last["raw_oid"].split(":")[1][:16] + ".raw")
text = open(blob).read()
print(text)
assert "never published on the host" in text, "the receipt stopped saying the port was not published"
assert "peers" in text, "the receipt has no peer section"
PY
[ $? = 0 ] && pass "the last receipt reads as it should" || fail "the receipt is not what it claims"

say "done"
if [ "$FAILED" = 0 ]; then
  "$H5I" box rm "$BOX" --force >/dev/null 2>&1
  git worktree prune
  printf '\033[32mall checks passed\033[0m\n'
else
  printf '\033[31m%d check(s) failed — the box has been left at %s\033[0m\n' "$FAILED" "$BOX"
fi
exit $FAILED
