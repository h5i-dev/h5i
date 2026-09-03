#!/usr/bin/env bash
# Does a macOS share reach the *box*, and refuse everything else?

set -uo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "share_macos.sh is about the macOS attribution route; this host is $(uname -s)."
  exit 0
fi

H5I="${H5I:-./target/release/h5i}"
# Unique per run, like the other share harnesses: a fixed name collides with
# anything else on the machine doing the same thing, and the collision reads as
# a product failure rather than as two scripts meeting.
BOX="${BOX:-mac-share-$$}"
WORK="$(mktemp -d)"
PIDS=()

pass() { printf '  \033[32mok\033[0m    %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; cleanup; exit 1; }
note() { printf '        %s\n' "$1"; }

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  pkill -f "box share $BOX" 2>/dev/null
  pkill -f "box share $BOX-vm" 2>/dev/null
}
trap cleanup EXIT

command -v python3 >/dev/null || { echo "needs python3"; exit 1; }
[ -x "$H5I" ] || { echo "no h5i binary at $H5I (cargo build --release)"; exit 1; }
H5I="$(cd "$(dirname "$H5I")" && pwd)/$(basename "$H5I")"

free_port() {
  python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()"
}

# End the box's current session and wait for it to let go.
kill_port_holders() {
  pkill -f "serve4.py" 2>/dev/null
  pkill -f "serve6.py" 2>/dev/null
  pkill -f "socketserver.TCPServer" 2>/dev/null
  for _ in $(seq 1 40); do
    lsof -nP -iTCP:"$STRANGER_PORT" -sTCP:LISTEN 2>/dev/null | grep -q . || return 0
    sleep 0.5
  done
}

stop_session() {
  pkill -f "box run $BOX" 2>/dev/null
  for _ in $(seq 1 20); do
    pgrep -f "box run $BOX" >/dev/null 2>&1 || { sleep 1; return 0; }
    sleep 1
  done
}

# Wait for a listener to actually exist, rather than sleeping and hoping.
wait_listen() {
  for _ in $(seq 1 40); do
    if lsof -nP -iTCP:"$1" -sTCP:LISTEN 2>/dev/null | grep -q "$2"; then return 0; fi
    sleep 1
  done
  if [ -n "${4:-}" ] && [ -s "${4:-}" ]; then
    note "what it said: $(tail -3 "$4" | tr '\n' ' ')"
  fi
  note "listeners on $1: $(lsof -nP -iTCP:"$1" -sTCP:LISTEN 2>/dev/null | tail -3 | tr '\n' ' ')"
  fail "$3 never started listening on port $1 — the check below would have passed for the wrong reason"
}

# Not 3000, and not any fixed number. The whole subject of this script is two
# processes wanting one port, so a hard-coded one guarantees the collision it
# is trying to stage deliberately — with whatever the developer already has
# running. Asked for and released; the stranger below claims it a moment later.
STRANGER_PORT="${STRANGER_PORT:-$(free_port)}"

# A repo of our own, so this never touches the checkout it is run from.
REPO="$WORK/repo"
mkdir -p "$REPO" && cd "$REPO" || exit 1
git init -q .
echo "the box's own page" > box-page.txt
git add -A && git -c user.email=t@t -c user.name=t commit -qm init

echo
echo "share_macos.sh — the box's port, and everybody else's"
echo

# ── 1. a stranger holds the port, and the box does not ───────────────────────
#
# First, because it is the one that must never regress: it is the exact shape
# the deleted macOS route got wrong.

python3 -c "
import http.server, socketserver
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        s.send_response(200); s.end_headers(); s.wfile.write(b'STRANGER')
    def log_message(*a): pass
class S(socketserver.TCPServer):
    # SO_REUSEADDR, which is what almost every dev server sets and what
    # \`http.server\` sets for you — and without it on *this* socket, BSD refuses
    # the box's later \`0.0.0.0\` bind outright with EADDRINUSE, so check 3 below
    # could never stage the shadowing it is about.
    allow_reuse_address = True
S(('127.0.0.1', $STRANGER_PORT), H).serve_forever()
" &
PIDS+=($!)
disown 2>/dev/null || true
sleep 2

wait_listen "$STRANGER_PORT" "127.0.0.1:$STRANGER_PORT" "the stranger"
if ! curl -sf -m 3 "http://127.0.0.1:$STRANGER_PORT/" | grep -q STRANGER; then
  fail "could not start a stranger on 127.0.0.1:$STRANGER_PORT (is it already taken?)"
fi

"$H5I" box create "$BOX" >"$WORK/create.log" 2>&1 \
  || fail "could not create a box: $(tail -1 "$WORK/create.log")"
"$H5I" box run "$BOX" -- sleep 600 >/dev/null 2>&1 &
PIDS+=($!)
disown 2>/dev/null || true
sleep 4   # the session itself; nothing to poll for, and `share` reports if it is missing

out=$("$H5I" box share "$BOX" --port "$STRANGER_PORT" --expire 2m 2>&1)
if grep -q 'h5i1_' <<<"$out"; then
  fail "a port held by a stranger was SHARED — this is the defect the route exists to prevent"
fi
if ! grep -q 'not part of this box' <<<"$out"; then
  fail "refused, but not for the right reason: $(head -1 <<<"$out")"
fi
pass "a stranger's port is refused, not shared"
note "$(grep -o 'held by [^,]*' <<<"$out" | head -1)"

# ── 2. nothing is listening ──────────────────────────────────────────────────
#
# A warning, not a refusal: an agent about to start its dev server is a
# perfectly good reason to share a port that is not up yet. Getting this wrong
# in the safe direction is still getting it wrong.

FREE=$(free_port)
"$H5I" box share "$BOX" --port "$FREE" --expire 30s >"$WORK/empty.log" 2>&1 &
sleep 12
if ! grep -qa 'h5i1_' "$WORK/empty.log"; then
  fail "an empty port was refused; it should warn and start: $(grep -am1 Error "$WORK/empty.log")"
fi
if ! grep -qa 'nothing is listening' "$WORK/empty.log"; then
  fail "an empty port started without saying so"
fi
pass "an empty port warns and still starts"
pkill -f "box share $BOX" 2>/dev/null
sleep 2
# The dev server has to *be* the session from here on: the box holds one at a
# time, and the `sleep` above is still holding it.
stop_session

# ── 3. the box binds the v4 wildcard, under a stranger on v4 loopback ────────
#
# Refused, and this one is worth staging because the answer is counter-
# intuitive: the box *is* listening on the port, and h5i still declines.
#
# The kernel hands a connection to the most specific matching socket, so a
# stranger bound to `127.0.0.1:P` takes every loopback connection from a box
# bound to `0.0.0.0:P`. The box is listening and is simply not reachable — not
# on v4, where the stranger wins, and not on v6, where it is not bound at all.
# Sharing it would hand the visitor the stranger, so there is nothing to share.
#
# This case was written the wrong way round first, expecting a share, and the
# script is the reason the expectation got corrected rather than the code.

cat > "$WORK/serve4.py" <<'PY'
import http.server, socketserver, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        s.send_response(200); s.end_headers(); s.wfile.write(b'THE BOX')
    def log_message(*a): pass
class S(socketserver.TCPServer):
    allow_reuse_address = True          # 0.0.0.0, the v4 wildcard
S(('0.0.0.0', int(sys.argv[1])), H).serve_forever()
PY
cp "$WORK/serve4.py" ".git/.h5i/env/human/$BOX/work/serve4.py"
"$H5I" box run "$BOX" -- python3 serve4.py "$STRANGER_PORT" >"$WORK/serve4.log" 2>&1 &
PIDS+=($!)
disown 2>/dev/null || true
# `*:PORT` on an IPv4 row is how lsof prints 0.0.0.0 — the box's socket, as
# distinct from the stranger's `127.0.0.1:PORT`.
wait_listen "$STRANGER_PORT" "IPv4.*\*:$STRANGER_PORT" "the box's v4-wildcard server" "$WORK/serve4.log"

out=$("$H5I" box share "$BOX" --port "$STRANGER_PORT" --expire 2m 2>&1)
if grep -q 'h5i1_' <<<"$out"; then
  fail "a box the stranger shadows was shared — a visitor would have reached the stranger"
fi
pass "a box shadowed by a more specific stranger is refused, though it is listening"
stop_session

# ── 4. the box binds dual-stack, under the same stranger ─────────────────────
#
# The real thing, and the shape found in the wild: the box's server on `::`
# (which `python3 -m http.server` and most dev servers produce) with an
# unrelated process on `127.0.0.1`. Both answer to "port $STRANGER_PORT". h5i
# must notice that the box wins on `::1` and dial it *there* — the whole point
# of resolving an address rather than connecting to "loopback".

cat > "$WORK/serve6.py" <<'PY'
import http.server, socket, socketserver, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        s.send_response(200); s.end_headers(); s.wfile.write(b'THE BOX')
    def log_message(*a): pass
class S(socketserver.TCPServer):
    address_family = socket.AF_INET6
    allow_reuse_address = True
    def server_bind(self):
        # v6-only, explicitly, and the reason is the scenario itself: with
        # `IPV6_V6ONLY` off, binding `::` also claims every v4 address, so this
        # bind would collide with the stranger already on `127.0.0.1` and the
        # box's server would never start. A dev server and an unrelated v4
        # listener coexisting on one port number is precisely the case found in
        # the wild, and this is the socket shape that produces it.
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        socketserver.TCPServer.server_bind(self)
S(('::', int(sys.argv[1])), H).serve_forever()
PY
cp "$WORK/serve6.py" ".git/.h5i/env/human/$BOX/work/serve6.py"
"$H5I" box run "$BOX" -- python3 serve6.py "$STRANGER_PORT" >"$WORK/serve6.log" 2>&1 &
PIDS+=($!)
disown 2>/dev/null || true
wait_listen "$STRANGER_PORT" "IPv6" "the box's v6 server" "$WORK/serve6.log"

"$H5I" box share "$BOX" --port "$STRANGER_PORT" --expire 5m >"$WORK/share.log" 2>&1 &
sleep 15
ticket=$(grep -ao 'h5i1_[A-Za-z0-9_-]*' "$WORK/share.log" | head -1)
[ -n "$ticket" ] || fail "the box's own port was not shared: $(grep -am1 Error "$WORK/share.log")"
pass "the box's port is shared even with a stranger on the same port number"

JOIN=$(free_port)
# `--shared-jar` because this script is macOS-only and macOS gives `lo0` one
# address. The join therefore always lands on `127.0.0.1`, always shares its
# cookie jar with every other local service, and is always refused unless it is
# asked for — so without the flag every run of this harness stops here, on a
# refusal about the joiner's machine rather than anything about attribution.
# The visit below fetches from nothing but the share under test.
"$H5I" join "$ticket" --port "$JOIN" --shared-jar >"$WORK/join.log" 2>&1 &
sleep 12
url=$(grep -ao "http://127\.[0-9.]*:$JOIN/?h5i=[a-f0-9]*" "$WORK/join.log" | head -1)
[ -n "$url" ] || fail "the joiner never came up: $(tail -2 "$WORK/join.log")"

body=$(curl -s -L -c "$WORK/jar" -b "$WORK/jar" -m 20 "$url")
case "$body" in
  *"THE BOX"*)
    pass "the visitor reached the box, not the stranger" ;;
  *STRANGER*)
    fail "THE VISITOR REACHED THE STRANGER — attribution picked the wrong socket" ;;
  *)
    fail "the visitor got neither: $(head -c 120 <<<"$body")" ;;
esac

# ── 5. the box's server dies mid-share and a stranger takes the port ─────────
#
# The one that cannot happen on Linux, and so the one only this platform can
# get wrong. There, the port lives in the box's namespace and a stranger cannot
# reach it at any price. Here the port is on the host's loopback and becomes
# available the moment the dev server lets go of it — while the share is up,
# authorized, and carrying a ticket somebody already holds.
#
# What must happen is that the visitor gets an error. What must not happen is
# that they get the stranger's bytes, which is the whole exposure the
# attribution route exists to prevent, arriving after the share started rather
# than before.
#
# The session and the dev server are deliberately different processes here
# (`sh` starts the server and outlives it), so that killing the server does not
# also end the session and end the share for an unrelated reason.

stop_session
kill_port_holders

cat > "$WORK/serve_child.sh" <<PY
python3 serve4.py $STRANGER_PORT &
sleep 300
PY
cp "$WORK/serve_child.sh" ".git/.h5i/env/human/$BOX/work/serve_child.sh"
"$H5I" box run "$BOX" -- sh serve_child.sh >"$WORK/serve5.log" 2>&1 &
PIDS+=($!)
disown 2>/dev/null || true
wait_listen "$STRANGER_PORT" "IPv4.*\*:$STRANGER_PORT" "the box's server" "$WORK/serve5.log"

"$H5I" box share "$BOX" --port "$STRANGER_PORT" --expire 5m >"$WORK/share5.log" 2>&1 &
disown 2>/dev/null || true
sleep 15
ticket=$(grep -ao 'h5i1_[A-Za-z0-9_-]*' "$WORK/share5.log" | head -1)
[ -n "$ticket" ] || fail "could not share the box's own port: $(grep -am1 Error "$WORK/share5.log")"

JOIN2=$(free_port)
"$H5I" join "$ticket" --port "$JOIN2" --shared-jar >"$WORK/join5.log" 2>&1 &   # as above
disown 2>/dev/null || true
sleep 12
url=$(grep -ao "http://127\.[0-9.]*:$JOIN2/?h5i=[a-f0-9]*" "$WORK/join5.log" | head -1)
[ -n "$url" ] || fail "the joiner never came up: $(tail -2 "$WORK/join5.log")"
body=$(curl -s -L -c "$WORK/jar5" -b "$WORK/jar5" -m 20 "$url")
case "$body" in
  *"THE BOX"*) : ;;
  *) fail "the share was not serving the box before the handover: $(head -c 80 <<<"$body")" ;;
esac

# The dev server lets go, and something else claims the port. The session — and
# so the share — is still up.
pkill -f "python3 serve4.py" 2>/dev/null
for _ in $(seq 1 40); do
  lsof -nP -iTCP:"$STRANGER_PORT" -sTCP:LISTEN 2>/dev/null | grep -q . || break
  sleep 0.5
done
python3 -c "
import http.server, socketserver
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        s.send_response(200); s.end_headers(); s.wfile.write(b'STRANGER')
    def log_message(*a): pass
class S(socketserver.TCPServer):
    allow_reuse_address = True
S(('127.0.0.1', $STRANGER_PORT), H).serve_forever()
" &
PIDS+=($!)
disown 2>/dev/null || true
wait_listen "$STRANGER_PORT" "127.0.0.1:$STRANGER_PORT" "the stranger taking over"

after=$(curl -s -L -c "$WORK/jar5" -b "$WORK/jar5" -m 20 "$url")
case "$after" in
  *STRANGER*)
    fail "A LIVE SHARE HANDED THE VISITOR A STRANGER — the port changed hands and the share \
followed it" ;;
  *"THE BOX"*)
    fail "the share served the box's page after its server had gone (a cache, or the wrong \
process answered)" ;;
  *)
    pass "a port that changes hands mid-share stops serving rather than serving the stranger" ;;
esac

echo
echo "all five outcomes hold. Boxes left behind: $BOX (in $REPO)"
