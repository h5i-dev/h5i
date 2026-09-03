#!/usr/bin/env bash
# End-to-end checks for `h5i box share`, against a real box.

set -uo pipefail

H5I="${H5I:-./target/release/h5i}"
# Unique per run, box and ports alike. Fixed names collided with anything else
# on the machine doing the same thing — another session running this script, or
# a review agent that happened to pick the same box name — and the collision
# presented as a product failure: a joiner that could not bind its port, a
# `curl` that got nothing, a smuggling probe with no listener to refuse it.
# A harness that reports somebody else's box as a bug in this one is worse
# than no harness.
BOX="${BOX:-e2e-share-$$}"
PORT=3000
WITH_TUNNEL=0
WITH_LEAK=0
for a in "$@"; do
  [ "$a" = "--tunnel" ] && WITH_TUNNEL=1
  [ "$a" = "--leak" ] && WITH_LEAK=1
done

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
[ -x "$H5I" ] || { echo "no h5i binary at $H5I — cargo build --release first"; exit 2; }

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

# The host a join actually bound, read from what it printed.
#
# Not `127.0.0.1`, which this used to assume everywhere. A join takes a
# loopback address of its own — `127.<x>.<y>.<z>`, so its cookie jar is not
# shared with every other local service on this machine — and falls back to
# `127.0.0.1` only where that bind is refused, which is macOS. The port is
# still whatever `--port` asked for; the address is not ours to predict.
join_host() {
  grep -ao 'http://127\.[0-9.]*:[0-9]*/' "$1" 2>/dev/null | head -1 |
    sed 's|^http://||; s|/$||; s|:[0-9]*$||'
}


last_receipt() {
  python3 - "$ENV_DIR" <<'PY'
import json, os, sys
d = sys.argv[1]
rs = [json.loads(l) for l in open(os.path.join(d, "receipt.jsonl"))
      if json.loads(l).get("source") == "share"]
last = rs[-1]
print(open(os.path.join(d, "receipts", last["raw_oid"].split(":")[1][:16] + ".raw")).read())
PY
}

# This box's own record, not the clone-wide listing. `share ls` answers for
# every box, so the first version failed whenever *anything else* on the
# machine was being shared — which, on a machine also running review agents,
# is a false failure reported as a product bug. The property is about one box.
#
# Polled rather than slept for: the record goes about a second after `stop`,
# but the teardown can take up to about six with connections still finishing,
# and a fixed sleep shorter than that is a stopwatch pretending to be a check.
no_record() {
  for _ in $(seq 1 100); do
    [ -e "$ENV_DIR/share.json" ] || return 0
    sleep 0.1
  done
  return 1
}

share_pid() { "$H5I" box share status "$BOX" 2>/dev/null | sed -n 's/.*pid \([0-9]*\).*/\1/p' | head -1; }

# ── peer to peer ────────────────────────────────────────────────────────────

say "peer to peer"
setsid "$H5I" box share "$BOX" --port $PORT --expire 10m --label e2e > "$WORK/share.log" 2>&1 &
sleep 12
TICKET="$(grep -o 'h5i1_[A-Za-z0-9_-]*' "$WORK/share.log" | head -1)"
[ -n "$TICKET" ] && pass "the share announced a ticket" || {
  fail "no ticket"; sed -n '1,6p' "$WORK/share.log"; exit 1; }

# `--shared-jar` so this runs on a host whose only loopback address is
# `127.0.0.1`. A join there shares its cookie jar with every local service, so
# `h5i join` refuses it unless asked — which would read here as a share that
# could not be joined. On Linux the flag changes nothing.
"$H5I" join "$TICKET" --port 8899 --shared-jar > "$WORK/join.log" 2>&1 &
JOIN_PID=$!
sleep 8
grep -q "joined" "$WORK/join.log" && pass "the joiner joined" || fail "the joiner did not join"
TOKEN="$(grep -o 'h5i=[a-f0-9]*' "$WORK/join.log" | head -1 | cut -d= -f2)"
JHOST="$(join_host "$WORK/join.log")"; JHOST="${JHOST:-127.0.0.1}"

# The joiner must survive with nobody visiting it. It presents its ticket once
# at connect time for exactly this reason; without that the sharer hangs up
# after thirty seconds and `h5i join` exits with "no ticket was presented".
say "a joiner nobody has visited yet"
sleep 40
joiner_alive && pass "still connected past the unauthenticated grace" || fail "the sharer hung up on an idle joiner"

say "a whole response, and a client that half-closes"
# `-c`, because the first request is answered with a redirect that sets the
# cookie; without a jar the followed request arrives anonymous and gets a 401.
GOT=$(curl -sL -c "$WORK/jar" -o /dev/null -w '%{size_download}' --max-time 90 "http://$JHOST:8899/?h5i=$TOKEN")
check "$GOT" "4194304" "4 MiB over p2p"

# `nc -q` shuts down its write side after sending. That is legal HTTP/1.1 and
# is what anything built out of one write and one read does; it used to be read
# as "the visitor left" and the download stopped after the first read.
HALF=$(printf "GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share_8899=$TOKEN\r\n\r\n" \
       | timeout 90 nc -q 60 "$JHOST" 8899 | wc -c)
if [ "$HALF" -gt 4194304 ]; then pass "a half-closing client got the whole body ($HALF bytes)"; else fail "half-close truncated the response ($HALF bytes)"; fi

say "an anonymous request is refused without killing anything"
CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 "http://$JHOST:8899/")
check "$CODE" "401" "no credential gets a 401"
joiner_alive && pass "the joiner survived it" || fail "an anonymous request ended the join"

# A request the gate refuses outright — the smuggling shape this crate spent
# fifteen rounds hardening against. Sent here rather than at the tunnel,
# because the first version of this check spoke plaintext HTTP to port 443 and
# Cloudflare's edge dropped it: the bytes never reached h5i at all, so the
# counter it was written for had no coverage whatsoever.
SMUG=$(printf 'GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share_8899=%s\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n' "$TOKEN" \
  | timeout 30 nc -q 2 "$JHOST" 8899 | head -1)
case "$SMUG" in
  HTTP/1.1\ 400*) pass "two Content-Lengths are refused" ;;
  *) fail "a smuggling shape was not refused: $SMUG" ;;
esac

# And a service worker registration, which would outlive the share and keep
# control of this address afterwards.
SW=$(printf 'GET /sw.js HTTP/1.1\r\nHost: t\r\nCookie: h5i_share_8899=%s\r\nService-Worker: script\r\n\r\n' "$TOKEN" \
  | timeout 30 nc -q 2 "$JHOST" 8899 | head -1)
case "$SW" in
  HTTP/1.1\ 403*) pass "a service worker registration is refused" ;;
  *) fail "a service worker registration was allowed: $SW" ;;
esac

# A request from a page on another loopback port. Two `h5i join` proxies are
# the same *site* to a browser, so `SameSite` does not hold the cookie back
# between them — the credential is attached and the request used to arrive.
FOREIGN=$(printf 'GET / HTTP/1.1\r\nHost: %s:8899\r\nCookie: h5i_share_8899=%s\r\nOrigin: http://%s:8900\r\n\r\n' "$JHOST" "$TOKEN" "$JHOST" \
  | timeout 30 nc -q 2 "$JHOST" 8899 | head -1)
case "$FOREIGN" in
  HTTP/1.1\ 403*) pass "a page on another loopback port is refused" ;;
  *) fail "a cross-origin request was served: $FOREIGN" ;;
esac

# And the share's own page is not foreign to itself.
OWN=$(printf 'GET / HTTP/1.1\r\nHost: %s:8899\r\nCookie: h5i_share_8899=%s\r\nOrigin: http://%s:8899\r\n\r\n' "$JHOST" "$TOKEN" "$JHOST" \
  | timeout 60 nc -q 5 "$JHOST" 8899 | head -1)
case "$OWN" in
  HTTP/1.1\ 200*) pass "the shared page can still call itself" ;;
  *) fail "the origin check refused the share's own page: $OWN" ;;
esac

joiner_alive && pass "and the joiner survived all of them" || fail "a refused request ended the join"

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
# Wait for the window rather than guessing at it. The serving process learns
# about the stop by polling at one-second intervals, so the old `sleep 0.4`
# signalled it while it was still in the main select — an ordinary first
# Ctrl-C, which is the test above this one. It passed for the wrong reason.
for _ in $(seq 1 100); do
  grep -q "shutting down" "$WORK/s3.log" && break
  sleep 0.05
done
grep -q "shutting down" "$WORK/s3.log" \
  && pass "the teardown window was reached" \
  || fail "never saw the teardown start, so the interrupt below proves nothing"
kill -INT "$P" 2>/dev/null
sleep 6
AFTER=$(receipts)
[ "$AFTER" -gt "$BEFORE" ] && pass "the receipt survived an interrupt mid-teardown" || fail "an interrupt during the teardown lost the receipt"
grep -q "interrupted again" "$WORK/s3.log" && fail "a first interrupt was treated as a second" || pass "and was not called a second interrupt"

# ── the sharer disappears mid-download ──────────────────────────────────────

say "the sharer vanishes while somebody is downloading"
# Not a graceful stop: `kill -9` on the serving process, which is what a laptop
# lid or an OOM kill looks like. The visitor is mid-transfer. What they must not
# get is a hang — a browser spinning on a connection nobody will ever answer is
# the worst of the three possible endings.
setsid "$H5I" box share "$BOX" --port $PORT --expire 10m > "$WORK/v.log" 2>&1 &
sleep 12
VT=$(grep -o 'h5i1_[A-Za-z0-9_-]*' "$WORK/v.log" | head -1)
"$H5I" join "$VT" --port 8951 --shared-jar > "$WORK/vjoin.log" 2>&1 &
VJ=$!
sleep 8
VTOK=$(grep -o 'h5i=[a-f0-9]*' "$WORK/vjoin.log" | head -1 | cut -d= -f2)
VHOST="$(join_host "$WORK/vjoin.log")"; VHOST="${VHOST:-127.0.0.1}"
VP=$(share_pid)

# A download slow enough that the kill lands in the middle of it.
( curl -s -b "h5i_share_8951=$VTOK" --limit-rate 60k -o "$WORK/partial.bin"     --max-time 120 "http://$VHOST:8951/" ; echo "$?" > "$WORK/curl.rc" ) &
sleep 4
kill -9 "$VP" 2>/dev/null

# The client has to come back, and "come back" means on its own rather than on
# its own timeout. Waiting a fixed sixty seconds was wrong twice over: the
# download legitimately takes about seventy at this rate when the joiner
# already had the bytes, so a completed transfer read as a hang. What is
# actually being asserted is that curl does not sit until `--max-time`, so the
# wait is longer than that and `28` — curl's timeout — is the failure.
for _ in $(seq 1 260); do
  [ -f "$WORK/curl.rc" ] && break
  sleep 0.5
done
rc=$(cat "$WORK/curl.rc" 2>/dev/null || echo missing)
# The bare status, not `curl=$?`. Written with that prefix, `$rc` was
# `curl=0` and neither `28` nor `missing` could ever match — so the one
# ending this check exists to catch, a client sitting until its own timeout,
# fell through to the pass arm. A check that cannot fail is worse than no
# check: this one reported "the visitor's client returned on its own" for a
# hang. The `(curl=curl=0)` in its own output was the tell.
case "$rc" in
  28)      fail "the visitor's client sat until its own timeout — that is the hang" ;;
  missing) fail "the visitor's client never returned at all" ;;
  *)       pass "the visitor's client returned on its own (curl=$rc)" ;;
esac
# Deliberately not asserted: whether the body arrived whole. That is a race
# between the rate limit and the kill, not a property — a sharer killed after
# the bytes were already delivered has delivered them, and asserting otherwise
# pins how fast this machine happens to be. What matters is the two endings:
# the client comes back, and the joiner does not sit there offering a share
# with nothing behind it.

# And the joiner notices, rather than sitting there offering a dead share.
for _ in $(seq 1 60); do
  joiner_dead=1
  kill -0 "$VJ" 2>/dev/null && joiner_dead=0
  [ "$joiner_dead" = 1 ] && break
  sleep 0.5
done
if kill -0 "$VJ" 2>/dev/null; then
  fail "the joiner is still running with no sharer behind it"
  kill "$VJ" 2>/dev/null
else
  pass "the joiner noticed and exited"
fi
grep -qi "share" "$WORK/vjoin.log" && pass "and said something about it"   || fail "the joiner exited silently"

"$H5I" box share stop "$BOX" --force >/dev/null 2>&1

# ── descriptors and memory, under sustained use ─────────────────────────────

if [ "$WITH_LEAK" = "1" ]; then
  say "five hundred requests, watching what grows"
  # The dialer hands sockets across a socketpair with SCM_RIGHTS, and several
  # paths close a stray descriptor that arrived beside an error status. A leak
  # there is invisible until a long-lived share stops being able to open
  # anything, which presents as a 502 on a share that worked an hour ago.
  setsid "$H5I" box share "$BOX" --port $PORT --expire 20m > "$WORK/leak.log" 2>&1 &
  sleep 12
  SP=$(share_pid)
  LT=$(grep -o 'h5i1_[A-Za-z0-9_-]*' "$WORK/leak.log" | head -1)
  # `--shared-jar` for the reason given at the first join above; this one was
  # missed because `--leak` is opt-in and reads its counts out of `/proc`, so
  # nobody had run it on the platform where the refusal fires.
  "$H5I" join "$LT" --port 8941 --shared-jar > "$WORK/leakjoin.log" 2>&1 &
  LJ=$!
  sleep 8
  LTOK=$(grep -o 'h5i=[a-f0-9]*' "$WORK/leakjoin.log" | head -1 | cut -d= -f2)
  LHOST="$(join_host "$WORK/leakjoin.log")"; LHOST="${LHOST:-127.0.0.1}"

  fds() { ls "/proc/$1/fd" 2>/dev/null | wc -l; }
  curl -s -b "h5i_share_8941=$LTOK" -o /dev/null --max-time 20 "http://$LHOST:8941/" >/dev/null
  sleep 1
  BASE_S=$(fds "$SP"); BASE_J=$(fds "$LJ")
  for _ in $(seq 1 400); do
    curl -s -b "h5i_share_8941=$LTOK" -o /dev/null --max-time 20 "http://$LHOST:8941/" >/dev/null 2>&1
  done
  # And a hundred refused ones, which leave by a different door.
  for _ in $(seq 1 100); do
    curl -s -o /dev/null --max-time 10 "http://$LHOST:8941/" >/dev/null 2>&1
  done
  sleep 3
  END_S=$(fds "$SP"); END_J=$(fds "$LJ")
  [ "$END_S" -le "$((BASE_S + 2))" ] \
    && pass "the sharer held its descriptors ($BASE_S then $END_S)" \
    || fail "the sharer leaked descriptors ($BASE_S then $END_S)"
  [ "$END_J" -le "$((BASE_J + 2))" ] \
    && pass "the joiner held its descriptors ($BASE_J then $END_J)" \
    || fail "the joiner leaked descriptors ($BASE_J then $END_J)"
  kill "$LJ" 2>/dev/null
  "$H5I" box share stop "$BOX" >/dev/null 2>&1
  sleep 6
fi

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
  sleep 6
  LAST=$(last_receipt)
  echo "$LAST" | grep -qE "[1-9][0-9]* presented no invite" \
    && pass "an anonymous knock is recorded as one, not as an unknown ticket" \
    || fail "the anonymous knock was miscounted: $(echo "$LAST" | grep refused)"
  echo "$LAST" | grep -q "an unknown ticket, 0 expired" \
    && pass "and the refusal line still names every reason" \
    || fail "the refusal line changed shape"
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

# And one that actually carried somebody. The check above reads the *last*
# receipt, and the last share of this run is one nobody visited — so its
# "peers none" passed while the peer accounting, which is what several of
# this feature's findings were about, went unread. The p2p share at the top
# moved 4 MiB twice and refused a handful of requests; this is that receipt.
python3 - "$ENV_DIR" <<'PY2'
import json, os, re, sys
d = sys.argv[1]
rs = [json.loads(l) for l in open(os.path.join(d, "receipt.jsonl"))
      if json.loads(l).get("source") == "share"]
seen = []
for r in rs:
    blob = os.path.join(d, "receipts", r["raw_oid"].split(":")[1][:16] + ".raw")
    try:
        seen.append(open(blob).read())
    except FileNotFoundError:
        pass
withpeers = [t for t in seen if re.search(r"^peers\s+[1-9]", t, re.M)]
assert withpeers, "no receipt in this run recorded a peer at all"
t = withpeers[0]
assert re.search(r"\d+ connections", t), "a peer row carries no connection count"
assert re.search(r"(KiB|MiB|B) in / ", t), "a peer row carries no byte counts"
assert "MiB out" in t or "KiB out" in t, "no bytes were recorded as delivered"
PY2
[ $? = 0 ] && pass "a receipt that carried somebody counted them" || fail "the peer accounting is not in the receipt"

say "done"
if [ "$FAILED" = 0 ]; then
  # The dev server holds the box busy, so `rm` refuses until it is gone. The
  # first version of this script left a box behind on every clean run.
  for p in $(pgrep -f "box run $BOX"); do kill "$p" 2>/dev/null; done
  sleep 2
  "$H5I" box rm "$BOX" --force >/dev/null 2>&1
  git worktree prune
  printf '\033[32mall checks passed\033[0m\n'
else
  printf '\033[31m%d check(s) failed — the box has been left at %s\033[0m\n' "$FAILED" "$BOX"
fi
exit $FAILED
