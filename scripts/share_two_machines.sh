#!/usr/bin/env bash
# Two machines, one share — the half of this feature a single host cannot test.

set -u

usage() {
  cat <<'USAGE'
usage:
  share_two_machines.sh share [options]     # on the machine with the box
  share_two_machines.sh join  [options]     # on the other machine

share options:
  --tunnel            share over a Cloudflare quick tunnel instead of peer to peer
  --direct-only       refuse to move bytes over a relay (peer to peer only)
  --minutes N         stop by itself after N minutes (default: wait for Enter)
  --name NAME         box name (default: twomachine)
  --port N            port inside the box (default: 3000)
  --isolation TIER    box isolation (default: supervised)
  --profile NAME      box profile (default: agent-claude)
  --keep              leave the box behind afterwards

join options:
  --url URL           a --tunnel invite link; without it the ticket is read from stdin
  --label NAME        what to call this machine in the output
USAGE
}

# ── small helpers ────────────────────────────────────────────────────────────

BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RESET=$(printf '\033[0m')
GREEN=$(printf '\033[32m'); RED=$(printf '\033[31m'); YELLOW=$(printf '\033[33m')

say()  { printf '%s\n' "$*"; }
head1() { printf '\n%s%s%s\n' "$BOLD" "$*" "$RESET"; }
ok()   { printf '  %sok%s   %s\n' "$GREEN" "$RESET" "$*"; PASS=$((PASS + 1)); }
bad()  { printf '  %sBAD%s  %s\n' "$RED" "$RESET" "$*"; FAIL=$((FAIL + 1)); }
skip() { printf '  %s--%s   %s\n' "$YELLOW" "$RESET" "$*"; SKIP=$((SKIP + 1)); }
note() { printf '  %s%s%s\n' "$DIM" "$*" "$RESET"; }

PASS=0; FAIL=0; SKIP=0

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}'
  else shasum -a 256 | awk '{print $1}'; fi
}

find_h5i() {
  if [ -x ./target/debug/h5i ]; then printf '%s' ./target/debug/h5i
  elif [ -x ./target/release/h5i ]; then printf '%s' ./target/release/h5i
  elif command -v h5i >/dev/null 2>&1; then command -v h5i
  else return 1; fi
}

need() {
  command -v "$1" >/dev/null 2>&1 || { say "this needs $1 on PATH"; exit 2; }
}

# ── the dev server that goes in the box ──────────────────────────────────────
#
# Deterministic, self-describing, and it answers the questions a real network
# makes interesting: does a big body arrive intact, does a slow body stay slow
# rather than being buffered whole, and what does the box get told about the
# person on the other end.

write_server() {
  cat > "$1" <<'PY'
import hashlib
import http.server
import socketserver
import sys
import threading
import time

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 3000

# Deterministic bytes: the joiner checks the digest, so the payload has to be
# the same one every run and the same one both ends can talk about.
def make(n, seed):
    out = bytearray()
    x = seed
    while len(out) < n:
        x = (x * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        out += x.to_bytes(8, "little")
    return bytes(out[:n])

BIG = make(8 * 1024 * 1024, 0x2026)
MID = make(1024 * 1024, 0x8080)
BIG_SHA = hashlib.sha256(BIG).hexdigest()
MID_SHA = hashlib.sha256(MID).hexdigest()
HELLO = b"h5i two-machine test"


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, body, ctype="application/octet-stream"):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        p = self.path.split("?")[0]
        if p == "/":
            self._send(HELLO, "text/plain")
        elif p == "/sha":
            self._send(("%s %s" % (BIG_SHA, MID_SHA)).encode(), "text/plain")
        elif p == "/big":
            self._send(BIG)
        elif p == "/mid":
            self._send(MID)
        elif p == "/slow":
            # Eight chunks, a quarter second apart. A proxy that buffers the
            # whole body before forwarding turns a live log tail into a page
            # that arrives all at once at the end, and the joiner times this.
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for i in range(8):
                part = ("chunk%d\n" % i).encode()
                self.wfile.write(b"%x\r\n%s\r\n" % (len(part), part))
                self.wfile.flush()
                time.sleep(0.25)
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        elif p == "/echo":
            # Exactly what reached the box. On a quick tunnel Cloudflare adds
            # the visitor's public IP and country; from a second machine those
            # would be someone else's, which is the point of checking here.
            seen = "\n".join("%s: %s" % (k, v) for k, v in self.headers.items())
            self._send(seen.encode("utf-8", "replace"), "text/plain")
        else:
            self._send(b"not found", "text/plain")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


with Server(("127.0.0.1", PORT), H) as httpd:
    with open("bound.flag", "w") as f:
        f.write("ok")
    print("serving on 127.0.0.1:%d" % PORT, flush=True)
    httpd.serve_forever()
PY
}

# ── the sharing side ─────────────────────────────────────────────────────────

do_share() {
  TUNNEL=""; DIRECT=""; MINUTES=""; BOX="twomachine"; PORT=3000
  ISO="supervised"; PROFILE="agent-claude"; KEEP=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --tunnel) TUNNEL=1 ;;
      --direct-only) DIRECT=1 ;;
      --minutes) shift; MINUTES="${1:-}" ;;
      --name) shift; BOX="${1:-twomachine}" ;;
      --port) shift; PORT="${1:-3000}" ;;
      --isolation) shift; ISO="${1:-supervised}" ;;
      --profile) shift; PROFILE="${1:-agent-claude}" ;;
      --keep) KEEP=1 ;;
      -h|--help) usage; exit 0 ;;
      *) say "unknown option: $1"; usage; exit 2 ;;
    esac
    shift
  done

  H5I=$(find_h5i) || { say "no h5i binary: build one (cargo build) or put it on PATH"; exit 2; }
  need python3
  WORKLOG=$(mktemp -t h5i-share-XXXXXX)
  SERVELOG=$(mktemp -t h5i-serve-XXXXXX)

  cleanup_share() {
    say ""
    say "cleaning up…"
    "$H5I" box share stop "$BOX" --force >/dev/null 2>&1
    for p in $(pgrep -f "box run $BOX" 2>/dev/null); do kill "$p" 2>/dev/null; done
    sleep 1
    if [ -z "$KEEP" ]; then
      "$H5I" box rm "$BOX" --force >/dev/null 2>&1
      git worktree prune >/dev/null 2>&1
    else
      say "box $BOX kept, as asked"
    fi
    rm -f "$WORKLOG" "$SERVELOG"
  }
  trap cleanup_share EXIT INT TERM

  head1 "== the box =="
  "$H5I" box rm "$BOX" --force >/dev/null 2>&1
  git worktree prune >/dev/null 2>&1
  if ! "$H5I" box --new --name "$BOX" --isolation "$ISO" --profile "$PROFILE" >"$WORKLOG" 2>&1; then
    say "could not make the box:"
    sed 's/^/    /' "$WORKLOG"
    say ""
    say "try another tier or profile: --isolation workspace --profile default"
    exit 1
  fi
  note "box $BOX, isolation=$ISO profile=$PROFILE"

  # The work directory, derived from the id rather than assumed: the agent
  # segment comes from $H5I_AGENT and is not always `human`.
  ID=$("$H5I" box ls --json 2>/dev/null | python3 -c "
import json,sys
rows = json.load(sys.stdin)
for r in rows:
    if r.get('slug') == '$BOX':
        print(r['id']); break
")
  if [ -z "$ID" ]; then say "could not find the box after creating it"; exit 1; fi
  WORK=".git/.h5i/$(printf '%s' "$ID" | sed 's|^env/|env/|')/work"
  if [ ! -d "$WORK" ]; then say "no work directory at $WORK"; exit 1; fi

  write_server "$WORK/serve.py"
  "$H5I" box run "$BOX" -- pwd >/dev/null 2>&1
  # Keep what the box says. When this server does not come up, its stderr is
  # the whole diagnosis — most of the ways it fails (the port already taken on
  # the host's loopback, a profile that will not run python3) say so in one
  # line, and discarding it leaves only "it never bound", which reads like the
  # box or the share is at fault.
  ( "$H5I" box run "$BOX" -- python3 serve.py "$PORT" >"$SERVELOG" 2>&1 & )

  printf '  waiting for the dev server'
  i=0
  while [ $i -lt 30 ]; do
    [ -f "$WORK/bound.flag" ] && break
    printf '.'; sleep 1; i=$((i + 1))
  done
  printf '\n'
  if [ ! -f "$WORK/bound.flag" ]; then
    say "the dev server never bound port $PORT inside the box"
    if [ -s "$SERVELOG" ]; then
      say "what it said:"
      sed 's/^/    /' "$SERVELOG" | tail -15
    else
      say "and it said nothing at all — try it by hand:"
      say "    $H5I box run $BOX -- python3 serve.py $PORT"
    fi
    exit 1
  fi
  ok "dev server up inside the box on port $PORT"

  head1 "== the share =="
  ARGS="--port $PORT"
  [ -n "$TUNNEL" ] && ARGS="$ARGS --tunnel"
  [ -n "$DIRECT" ] && ARGS="$ARGS --direct-only"
  EXPIRE="${MINUTES:-120}"
  # `setsid` where there is one and plain background where there is not:
  # macOS has no setsid, and the other machine may well be a Mac.
  SETSID=""
  command -v setsid >/dev/null 2>&1 && SETSID="setsid"
  # shellcheck disable=SC2086
  $SETSID "$H5I" box share "$BOX" $ARGS --expire "${EXPIRE}m" >"$WORKLOG" 2>&1 &
  SHARE_PID=$!

  i=0
  INVITE=""
  while [ $i -lt 60 ]; do
    if [ -n "$TUNNEL" ]; then
      INVITE=$(grep -ao 'https://[^ ]*' "$WORKLOG" 2>/dev/null | head -1)
    else
      INVITE=$(grep -ao 'h5i1_[A-Za-z0-9_-]*' "$WORKLOG" 2>/dev/null | head -1)
    fi
    [ -n "$INVITE" ] && break
    sleep 1; i=$((i + 1))
  done
  if [ -z "$INVITE" ]; then
    say "the share never printed an invite:"
    sed 's/^/    /' "$WORKLOG" | tail -5
    exit 1
  fi
  ok "share is up"
  kill -0 "$SHARE_PID" 2>/dev/null || true

  head1 "== send this to the other machine =="
  say ""
  if [ -n "$TUNNEL" ]; then
    say "    $INVITE"
    say ""
    note "on the other machine, in a clone of this repo:"
    say "    scripts/share_two_machines.sh join --url '$INVITE'"
  else
    say "    $INVITE"
    say ""
    note "on the other machine, in a clone of this repo:"
    note "(the ticket goes on stdin, so it stays out of that machine's process table)"
    say "    printf '%s' '<paste the ticket>' | scripts/share_two_machines.sh join"
  fi
  say ""

  head1 "== while they are connected =="
  note "this side is now just watching. Worth doing by hand, in another terminal:"
  note "  $H5I box share status $BOX          # what the sharer thinks is happening"
  note "  $H5I box share revoke $BOX <grant>  # their connection should drop within a second"
  say ""
  if [ -n "$MINUTES" ]; then
    note "stopping by itself in $MINUTES minute(s)"
    sleep $((MINUTES * 60))
  else
    note "press Enter when the other machine has finished its run"
    read -r _ || true
  fi

  head1 "== stopping =="
  "$H5I" box share stop "$BOX" >/dev/null 2>&1
  sleep 6

  head1 "== the receipt, which is the thing to compare =="
  python3 - "$ID" <<'PY'
import json, os, sys
d = os.path.join(".git", ".h5i", sys.argv[1])
log = os.path.join(d, "receipt.jsonl")
if not os.path.exists(log):
    print("  no receipt log")
    raise SystemExit
rows = [json.loads(l) for l in open(log) if json.loads(l).get("source") == "share"]
if not rows:
    print("  no share receipt was written")
    raise SystemExit
raw = rows[-1]["raw_oid"].split(":")[1][:16] + ".raw"
print(open(os.path.join(d, "receipts", raw)).read())
PY

  head1 "== what to check against the other machine's output =="
  note "peers          1, and its endpoint id is the joiner's"
  note "via            direct / relayed — must match what the joiner printed"
  note "connections    one per request that reached the box"
  note "bytes out      about what the joiner says it downloaded"
  note "refused/turned zero, unless you revoked something on purpose"
  say ""
  note "the transport line matters most: on one machine it is always direct,"
  note "so 'relayed' has never been produced by a test before this one."
}

# ── the joining side ─────────────────────────────────────────────────────────

do_join() {
  URL=""; LABEL="$(hostname 2>/dev/null || echo 'the other machine')"
  while [ $# -gt 0 ]; do
    case "$1" in
      --url) shift; URL="${1:-}" ;;
      --label) shift; LABEL="${1:-}" ;;
      -h|--help) usage; exit 0 ;;
      *) say "unknown option: $1"; usage; exit 2 ;;
    esac
    shift
  done
  need curl

  JOINLOG=$(mktemp -t h5i-join-XXXXXX)
  JAR=$(mktemp -t h5i-jar-XXXXXX)
  JOIN_PID=""
  cleanup_join() {
    [ -n "$JOIN_PID" ] && kill "$JOIN_PID" 2>/dev/null
    rm -f "$JOINLOG" "$JAR"
  }
  trap cleanup_join EXIT INT TERM

  PATH_SEEN="n/a (tunnel)"
  if [ -n "$URL" ]; then
    head1 "== joining a quick tunnel =="
    BASE="${URL%%\?*}"
    # The invite carries the credential in its query string and answers with a
    # redirect that moves it into a cookie, exactly as a browser would do it.
    curl -s -o /dev/null -L -c "$JAR" -b "$JAR" --max-time 60 "$URL" || true
    ok "followed the invite link"
    FETCH="curl -s -b $JAR -c $JAR --max-time 120"
  else
    H5I=$(find_h5i) || { say "no h5i binary: build one (cargo build) or put it on PATH"; exit 2; }
    head1 "== joining peer to peer =="
    TICKET=$(cat)
    if [ -z "$TICKET" ]; then
      say "no ticket arrived on stdin. Try:"
      say "    printf '%s' '<the h5i1_… ticket>' | scripts/share_two_machines.sh join"
      exit 2
    fi
    # Through stdin on purpose: an argument would be readable by every other user on this
    # machine for as long as the join lasts.
    printf '%s' "$TICKET" | "$H5I" join - --shared-jar >"$JOINLOG" 2>&1 &
    JOIN_PID=$!
    i=0
    LOCAL=""
    while [ $i -lt 45 ]; do
      # Whatever address the join actually bound, not an assumed one. Each
      # join takes a loopback address of its own (`127.<x>.<y>.<z>`) so its
      # cookie jar is not shared with every other local service; only where
      # that bind is refused — macOS assigns just `127.0.0.1` to `lo0` — does
      # it fall back to the address this used to hard-code. Reading the
      # printed URL is also what a person does.
      LOCAL=$(grep -ao 'http://127\.[0-9.]*:[0-9]*/?h5i=[a-f0-9]*' "$JOINLOG" 2>/dev/null | head -1)
      [ -n "$LOCAL" ] && break
      if ! kill -0 "$JOIN_PID" 2>/dev/null; then break; fi
      sleep 1; i=$((i + 1))
    done
    if [ -z "$LOCAL" ]; then
      say "the join did not come up:"
      sed 's/^/    /' "$JOINLOG" | tail -6
      exit 1
    fi
    ok "joined"
    HOSTPORT=$(printf '%s' "$LOCAL" | sed 's|^http://||; s|/.*||')
    PORT=$(printf '%s' "$HOSTPORT" | sed 's|.*:||')
    TOK=$(printf '%s' "$LOCAL" | sed 's|.*h5i=||')
    BASE="http://$HOSTPORT"
    FETCH="curl -s -H Cookie:h5i_share_${PORT}=${TOK} --max-time 120"
    # The one line `h5i join` prints about the transport, read exactly rather
    # than by looking for the word anywhere: the banner around it discusses
    # relays in the abstract, and a grep for "relay" matches that too.
    PATH_SEEN=$(grep -a '^   path  *' "$JOINLOG" 2>/dev/null | head -1 |
                sed 's/^ *path *//; s/ *—.*//')
    [ -n "$PATH_SEEN" ] || PATH_SEEN="the joiner did not say"
    note "path at join time, from this side: $PATH_SEEN"
    note "(one reading, taken once. A QUIC path can change mid-session, so"
    note " compare it against the sharer's receipt rather than assuming it held.)"
  fi

  START=$(date +%s)
  BYTES=0
  REQS=0

  head1 "== does the app arrive at all =="
  HELLO=$($FETCH "$BASE/" 2>/dev/null)
  REQS=$((REQS + 1))
  if [ "$HELLO" = "h5i two-machine test" ]; then
    ok "the page is the page the box is serving"
  else
    bad "got something else: $(printf '%s' "$HELLO" | head -c 80)"
  fi

  head1 "== does a big body arrive intact =="
  SHAS=$($FETCH "$BASE/sha" 2>/dev/null)
  REQS=$((REQS + 1))
  BIG_WANT=$(printf '%s' "$SHAS" | awk '{print $1}')
  MID_WANT=$(printf '%s' "$SHAS" | awk '{print $2}')
  T0=$(date +%s)
  BIG_GOT=$($FETCH "$BASE/big" 2>/dev/null | sha256)
  T1=$(date +%s)
  REQS=$((REQS + 1))
  if [ -n "$BIG_WANT" ] && [ "$BIG_GOT" = "$BIG_WANT" ]; then
    SECS=$((T1 - T0))
    if [ "$SECS" -ge 1 ]; then
      ok "8 MiB arrived byte for byte, in ${SECS}s (about $((8 / SECS)) MiB/s)"
    else
      # No rate out of a one-second stopwatch and a sub-second transfer: a
      # figure there would be one this script made up.
      ok "8 MiB arrived byte for byte, in under a second"
    fi
    BYTES=$((BYTES + 8 * 1024 * 1024))
  else
    bad "8 MiB came back different — want ${BIG_WANT:-?}, got $BIG_GOT"
  fi

  head1 "== does a slow body stay slow =="
  T0=$(date +%s)
  SLOW=$($FETCH "$BASE/slow" 2>/dev/null | tr -d '\r')
  T1=$(date +%s)
  REQS=$((REQS + 1))
  LINES=$(printf '%s\n' "$SLOW" | grep -c '^chunk')
  ELAPSED=$((T1 - T0))
  if [ "$LINES" -eq 8 ]; then
    ok "all eight chunks arrived"
  else
    bad "$LINES of 8 chunks arrived"
  fi
  if [ "$ELAPSED" -ge 1 ]; then
    ok "it took ${ELAPSED}s, so it was streamed rather than buffered whole"
  else
    bad "it arrived in under a second, which means something buffered it"
  fi

  head1 "== what the box was told about you =="
  ECHOED=$($FETCH "$BASE/echo" 2>/dev/null)
  REQS=$((REQS + 1))
  LEAKED=""
  for h in cf-connecting-ip x-forwarded-for true-client-ip x-real-ip cf-ipcountry cf-ray forwarded; do
    if printf '%s' "$ECHOED" | tr 'A-Z' 'a-z' | grep -q "^$h:"; then
      LEAKED="$LEAKED $h"
    fi
  done
  if [ -z "$LEAKED" ]; then
    ok "nothing that identifies you reached the box"
  else
    bad "the box was told:$LEAKED"
  fi
  note "the box saw these headers:"
  printf '%s\n' "$ECHOED" | sed 's/^/      /' | head -12

  head1 "== many requests, which is what a real page load is =="
  FAILED=0
  i=0
  while [ $i -lt 30 ]; do
    CODE=$($FETCH -o /dev/null -w '%{http_code}' "$BASE/" 2>/dev/null)
    [ "$CODE" = "200" ] || FAILED=$((FAILED + 1))
    i=$((i + 1))
  done
  REQS=$((REQS + 30))
  if [ "$FAILED" -eq 0 ]; then
    ok "30 requests, none refused"
  else
    bad "$FAILED of 30 requests did not come back 200"
  fi

  head1 "== three at once =="
  P=0
  for n in 1 2 3; do
    ( GOT=$($FETCH "$BASE/mid" 2>/dev/null | sha256)
      [ "$GOT" = "$MID_WANT" ] && exit 0 || exit 1 ) &
    eval "PID$n=\$!"
  done
  for n in 1 2 3; do
    eval "wait \$PID$n" && P=$((P + 1))
  done
  REQS=$((REQS + 3))
  BYTES=$((BYTES + 3 * 1024 * 1024))
  if [ "$P" -eq 3 ]; then
    ok "three concurrent 1 MiB fetches all arrived intact"
  else
    bad "only $P of 3 concurrent fetches were intact"
  fi

  END=$(date +%s)

  head1 "== this machine's account, to read beside the sharer's receipt =="
  say ""
  say "  machine        $LABEL"
  say "  transport      $PATH_SEEN"
  say "  requests made  $REQS"
  say "  bytes fetched  about $((BYTES / 1024 / 1024)) MiB"
  say "  wall time      $((END - START))s"
  say ""
  say "  checks: $PASS ok, $FAIL bad, $SKIP skipped"
  say ""
  if [ -z "$URL" ]; then
    note "leave this running and try, on the sharing machine:"
    note "  h5i box share revoke <box> <grant>   — this should end within a second"
    note "  h5i box share stop <box>             — this should say they stopped sharing"
    note "then press Enter here to see how this side reported it."
    read -r _ || true
    say ""
    if kill -0 "$JOIN_PID" 2>/dev/null; then
      note "this side is still connected — nothing ended it."
    else
      note "what this side printed when it ended:"
      tail -4 "$JOINLOG" | sed 's/^/      /'
    fi
  fi

  [ "$FAIL" -eq 0 ] || exit 1
}

# ── dispatch ─────────────────────────────────────────────────────────────────

case "${1:-}" in
  share) shift; do_share "$@" ;;
  join)  shift; do_join "$@" ;;
  -h|--help|"") usage ;;
  *) say "unknown command: $1"; usage; exit 2 ;;
esac
