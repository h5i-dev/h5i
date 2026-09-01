#!/usr/bin/env bash
# Does a share of a box at each tier and profile actually move a byte?

set -uo pipefail

H5I="${H5I:-./target/debug/h5i}"
WORK="$(mktemp -d)"
BROKEN=0
ONLY="${1:-}"
# A fresh local port per row. The first version reused one, and a joiner from
# the previous row that had not exited yet still held it — so the next row's
# `h5i join` could not bind, curl got nothing, and the harness reported a
# perfectly good configuration as a broken share. A harness that invents
# findings is worse than no harness.
PORT=8981
# And a fresh *app* port per row. At the `workspace` tier the box shares the
# host's network, so a dev server left over from the previous row still holds
# the one it bound — which reads as "this box could not start a dev server"
# when the truth is that the last one had not let go yet.
APP=3300

TIERS=(workspace process supervised)
PROFILES=(default agent-claude agent-codex)

trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/serve.py" <<'PY'
import http.server, socketserver, pathlib

BODY = b'matrix-ok'

# The port comes from a file the harness writes into the box's work directory:
# `h5i box run` has no way to pass an environment variable in, because the
# profile decides what is passed rather than the caller.
PORT = int(pathlib.Path("port.txt").read_text().strip())


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *a):
        pass


socketserver.TCPServer.allow_reuse_address = True
srv = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), H)
# Written only once the bind has succeeded, so the harness can tell "the dev
# server never started" from "the share moved nothing". Without it the script
# reported a box where the server had not come up as a broken share, which is
# the kind of false signal that makes a harness worse than none.
pathlib.Path("bound.flag").write_text("ok")
srv.serve_forever()
PY

row() { printf '  %-11s %-14s %s\n' "$1" "$2" "$3"; }

reap() {
  for p in $(pgrep -f "box run $1"); do kill "$p" 2>/dev/null; done
  sleep 1
  "$H5I" box share stop "$1" --force >/dev/null 2>&1
  "$H5I" box rm "$1" --force >/dev/null 2>&1
  git worktree prune 2>/dev/null
}

printf '\n\033[1mtier        profile        result\033[0m\n'

for tier in "${TIERS[@]}"; do
  [ -n "$ONLY" ] && [ "$ONLY" != "$tier" ] && continue
  for prof in "${PROFILES[@]}"; do
    box="mx-$tier-$prof"
    reap "$box"

    if ! "$H5I" box --new --name "$box" --isolation "$tier" --profile "$prof" \
         >"$WORK/create.log" 2>&1; then
      # A tier this host cannot enforce fails closed on purpose, which is the
      # documented behaviour and not a finding.
      row "$tier" "$prof" "not available here ($(tail -1 "$WORK/create.log" | cut -c1-60))"
      continue
    fi

    cp "$WORK/serve.py" ".git/.h5i/env/human/$box/work/serve.py"
    "$H5I" box run "$box" -- pwd >/dev/null 2>&1
    APP=$((APP + 1))
    echo "$APP" > ".git/.h5i/env/human/$box/work/port.txt"
    ("$H5I" box run "$box" -- python3 serve.py >/dev/null 2>&1 &)
    sleep 6

    # Can the box even reach itself? A namespace with no loopback answers
    # ENETUNREACH here, which is the shape that started this script.
    self=$("$H5I" box run "$box" -- python3 -c "
import socket
s = socket.socket(); s.settimeout(3)
try:
    s.connect(('127.0.0.1', $APP)); print('SELF=reaches-itself')
except Exception as e:
    print('SELF=' + type(e).__name__ + ':' + str(getattr(e, 'errno', '')))
" 2>&1 | sed -n 's/^SELF=//p' | head -1)
    self="${self:-unknown}"
    if [ ! -e ".git/.h5i/env/human/$box/work/bound.flag" ]; then
      row "$tier" "$prof" "no dev server — the box could not bind port $APP [self=$self]"
      reap "$box"
      continue
    fi

    if ! setsid "$H5I" box share "$box" --port "$APP" --expire 5m \
         >"$WORK/share.log" 2>&1 & then :; fi
    sleep 12

    if ! grep -qa 'h5i1_' "$WORK/share.log"; then
      # Refusing is a fine answer as long as it says something usable.
      why=$(grep -am1 -o 'Error:.*' "$WORK/share.log" | cut -c1-70)
      row "$tier" "$prof" "refused — ${why:-no reason given} [self=$self]"
      reap "$box"
      continue
    fi

    ticket=$(grep -ao 'h5i1_[A-Za-z0-9_-]*' "$WORK/share.log" | head -1)
    PORT=$((PORT + 1))
    # `--shared-jar` for the same reason the other harnesses pass it: where
    # `127.0.0.1` is the only loopback address there is, `h5i join` refuses the
    # shared cookie jar unless it is asked for, and every row would read BROKEN
    # for a reason that has nothing to do with the tier under test.
    "$H5I" join "$ticket" --port "$PORT" --shared-jar >"$WORK/join.log" 2>&1 &
    jp=$!
    sleep 7
    if ! grep -q "joined" "$WORK/join.log"; then
      row "$tier" "$prof" "the joiner could not join [self=$self, $(head -1 "$WORK/join.log" | cut -c1-50)]"
      kill "$jp" 2>/dev/null
      reap "$box"
      continue
    fi
    tok=$(grep -o 'h5i=[a-f0-9]*' "$WORK/join.log" | head -1 | cut -d= -f2)
    # The address the join bound, not an assumed one: each join takes a
    # loopback address of its own so its cookie jar is not shared with every
    # other local service, and falls back to `127.0.0.1` only where that bind
    # is refused (macOS assigns just the one to `lo0`).
    jh=$(grep -ao 'http://127\.[0-9.]*:[0-9]*/' "$WORK/join.log" | head -1 |
         sed 's|^http://||; s|/$||; s|:[0-9]*$||')
    got=$(curl -s -b "h5i_share_$PORT=$tok" --max-time 25 "http://${jh:-127.0.0.1}:$PORT/" 2>/dev/null)
    kill "$jp" 2>/dev/null
    wait "$jp" 2>/dev/null

    if [ "$got" = "matrix-ok" ]; then
      row "$tier" "$prof" "serves [self=$self]"
    else
      row "$tier" "$prof" "$(printf '\033[31mBROKEN\033[0m') — shared but moved nothing [self=$self, got='${got:0:30}']"
      BROKEN=$((BROKEN + 1))
    fi
    reap "$box"
  done
done

echo
if [ "$BROKEN" = 0 ]; then
  echo "every configuration this host can run either serves or refuses with a reason"
else
  printf '\033[31m%d configuration(s) started a share that moved nothing\033[0m\n' "$BROKEN"
fi
exit "$BROKEN"
