#!/usr/bin/env bash
# Boot the thermostat firmware in Renode and talk HTTP to it over its UART.
#
#   scripts/embedded/install-renode.sh      # once
#   scripts/embedded/renode-smoke.sh        # builds the firmware, then checks
#
# Checks a GET and PUTs, the heater interlock, pipelined requests, the
# smuggling rejections (CL+TE, duplicate CL, obs-fold, space before the
# colon, an oversized head, a bad chunk size, ...), and that the device still
# answers afterwards: the rejections neither crashed nor wedged it.
#
# The UART is framed (h5i_app_embedded::link); clients reach it through
# uart-gateway.py on PORT, which talks to Renode's UART socket on PORT+1.
# Also checks the shared-line attacks: a partial request left by one client
# is never completed by the next one's bytes, a silent client is cut off
# within the device's bound, and no client receives another's response.
#
# Environment: RENODE (default ~/.local/opt/renode/renode), PORT (18080),
# FIRMWARE (a prebuilt ELF; skips the build), CARGO_TARGET_DIR.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
renode="${RENODE:-$HOME/.local/opt/renode/renode}"
port="${PORT:-18080}"
uart_port=$((port + 1))
fw_dir="$root/examples/embedded/thermostat/firmware"

if [ -z "${FIRMWARE:-}" ]; then
    (cd "$fw_dir" && cargo build --release --locked)
    target="${CARGO_TARGET_DIR:-$fw_dir/target}"
    FIRMWARE="$target/thumbv7em-none-eabihf/release/thermostat-firmware"
fi
[ -x "$renode" ] || { echo "no Renode at $renode: run scripts/embedded/install-renode.sh" >&2; exit 2; }
[ -f "$FIRMWARE" ] || { echo "no firmware at $FIRMWARE" >&2; exit 2; }

log="$(mktemp)"
# Renode's console reads stdin; an open, silent one keeps it running.
sleep infinity | "$renode" --disable-xwt --console \
    -e "\$elf=@$FIRMWARE; \$port=$uart_port; include @$root/scripts/embedded/thermostat.resc" >"$log" 2>&1 &
renode_pid=$!
python3 "$root/scripts/embedded/uart-gateway.py" "$port" "$uart_port" 6 >>"$log" 2>&1 &
gateway_pid=$!
cleanup() {
    kill "$gateway_pid" 2>/dev/null || true
    pkill -P "$renode_pid" 2>/dev/null || true
    kill "$renode_pid" 2>/dev/null || true
    pkill -f "renode.*$uart_port" 2>/dev/null || true
}
trap cleanup EXIT

PORT="$port" LOG="$log" python3 - <<'EOF'
import os, socket, sys, time

PORT = int(os.environ["PORT"])
KEY = "dev-key"

def wait_up():
    for _ in range(120):
        try:
            socket.create_connection(("127.0.0.1", PORT), timeout=1).close()
            return
        except OSError:
            time.sleep(0.5)
    print(open(os.environ["LOG"]).read())
    sys.exit("renode did not open the UART socket")

def responses(data, count, timeout=20.0):
    """Send raw bytes, read `count` responses framed by Content-Length."""
    s = socket.create_connection(("127.0.0.1", PORT), timeout=timeout)
    s.sendall(data)
    buf = b""
    out = []
    deadline = time.time() + timeout
    while len(out) < count and time.time() < deadline:
        while b"\r\n\r\n" not in buf:
            chunk = s.recv(4096)
            if not chunk:
                raise RuntimeError("closed: %r" % buf)
            buf += chunk
        head, rest = buf.split(b"\r\n\r\n", 1)
        n = 0
        for line in head.split(b"\r\n")[1:]:
            k, _, v = line.partition(b":")
            if k.strip().lower() == b"content-length":
                n = int(v.strip())
        while len(rest) < n:
            chunk = s.recv(4096)
            if not chunk:
                raise RuntimeError("closed in body")
            rest += chunk
        status = int(head.split(b" ")[1])
        out.append((status, head.decode(), rest[:n].decode(errors="replace")))
        buf = rest[n:]
    s.close()
    if len(out) < count:
        raise RuntimeError("timed out after %d responses" % len(out))
    return out

def one(data):
    return responses(data, 1)[0]

def req(method, path, body=b"", auth=True, extra=b""):
    h = b"%s %s HTTP/1.1\r\nHost: dev\r\n" % (method, path)
    if auth:
        h += b"Authorization: Bearer " + KEY.encode() + b"\r\n"
    if body or method in (b"PUT", b"POST"):
        h += b"Content-Length: %d\r\n" % len(body)
    return h + extra + b"\r\n" + body

failures = []
def check(name, cond, detail=""):
    print(("ok    " if cond else "FAIL  ") + name + ("" if cond else "  " + detail))
    if not cond:
        failures.append(name)

wait_up()
time.sleep(1)

st, _, body = one(req(b"GET", b"/state", auth=False))
check("GET /state", st == 200 and '"mode":"off"' in body, body)
st, _, body = one(req(b"PUT", b"/mode", b"heat"))
check("PUT /mode heat turns the heater on", st == 200 and '"heater":true' in body, body)
st, _, body = one(req(b"POST", b"/temp", b"360"))
check("interlock: 36.0 C turns it off", st == 200 and '"heater":false' in body, body)
st, _, body = one(req(b"PUT", b"/relay/2", b"on"))
check("PUT /relay/2", st == 200 and '"relays":[false,false,true,false]' in body, body)
st, _, body = one(req(b"PUT", b"/relay/7", b"on"))
check("relay 7 refused by the kernel", st == 404 and "no_such_relay" in body, body)
st, _, _ = one(req(b"PUT", b"/setpoint", b"999"))
check("setpoint out of range refused", st == 422)
st, _, _ = one(req(b"PUT", b"/mode", b"auto", auth=False))
check("write without the key is 401", st == 401)

rs = responses(req(b"POST", b"/temp", b"150") + req(b"GET", b"/state", auth=False)
               + req(b"PUT", b"/setpoint", b"215"), 3)
check("pipelined: three responses", [r[0] for r in rs] == [200, 200, 200], str([r[0] for r in rs]))
check("pipelined: in order", '"temp_dc":150' in rs[1][2] and '"setpoint_dc":215' in rs[2][2], rs[2][2])

chunked = (b"POST /temp HTTP/1.1\r\nHost: dev\r\nAuthorization: Bearer dev-key\r\n"
           b"Transfer-Encoding: chunked\r\n\r\n1\r\n2\r\n2\r\n05\r\n0\r\n\r\n")
st, _, body = one(chunked)
check("chunked body", st == 200 and '"temp_dc":205' in body, body)

bad = [
    ("CL + TE", b"POST /temp HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
    ("duplicate CL", b"POST /temp HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n100", 400),
    ("obs-fold", b"GET /state HTTP/1.1\r\nHost: d\r\nX: a\r\n b\r\n\r\n", 400),
    ("space before colon", b"POST /temp HTTP/1.1\r\nHost: d\r\nContent-Length : 3\r\n\r\n100", 400),
    ("TE space before colon", b"POST /temp HTTP/1.1\r\nHost: d\r\nTransfer-Encoding : chunked\r\n\r\n0\r\n\r\n", 400),
    ("bare LF", b"GET /state HTTP/1.1\nHost: d\n\n", 400),
    ("TE not chunked", b"POST /temp HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: gzip, chunked\r\n\r\n", 501),
    ("bad chunk size", b"POST /temp HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 400),
    ("chunk extension", b"POST /temp HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n3;x\r\n100\r\n0\r\n\r\n", 400),
    ("no Host", b"GET /state HTTP/1.1\r\n\r\n", 400),
    ("body too large", b"POST /temp HTTP/1.1\r\nHost: d\r\nContent-Length: 5000\r\n\r\n", 413),
    ("oversized head", b"GET /state HTTP/1.1\r\nHost: d\r\nX: " + b"a" * 1200 + b"\r\n\r\n", 431),
    ("oversized target", b"GET /" + b"a" * 1200 + b" HTTP/1.1\r\nHost: d\r\n\r\n", 414),
]
for name, data, want in bad:
    try:
        st, head, _ = one(data)
        check("reject %s -> %d" % (name, want), st == want and "Connection: close" in head, "%d %r" % (st, head))
    except Exception as e:
        check("reject %s -> %d" % (name, want), False, repr(e))
    time.sleep(0.2)

st, _, body = one(req(b"GET", b"/state", auth=False))
check("still serving after the rejections", st == 200 and '"temp_dc":205' in body, body)

# --- The shared line ---------------------------------------------------------
def conn(t=20):
    return socket.create_connection(("127.0.0.1", PORT), timeout=t)

# 1. Hijack: an attacker leaves a partial head and goes; the victim follows.
a = conn(); a.sendall(b"PUT /relay/3 HTTP/1.1\r\nX-Pad: "); a.close()
st, _, body = one(req(b"PUT", b"/relay/0", b"on"))
check("hijack: the victim's own request ran", st == 200 and '"relays":[true,false,true,false]' in body, body)
st, _, body = one(req(b"GET", b"/state", auth=False))
check("hijack: relay 3 untouched", '"relays":[true,false,true,false]' in body, body)

# 2. A silent client (request line, then nothing, socket open) holds the line
#    no longer than the device's bound; the next client is then served.
a = conn(); a.sendall(b"GET /state HTTP/1.1\r\n")
time.sleep(0.2)
t0 = time.time()
try:
    st, _, body = one(req(b"GET", b"/state", auth=False))
except Exception as e:
    st, body = 0, repr(e)
waited = time.time() - t0
check("silent client: next one served (%.1fs)" % waited, st == 200 and waited < 12, body)
a.settimeout(1)
try:
    rest = a.recv(4096)
except OSError:
    rest = b""
check("silent client: got nothing of the other's", b"setpoint_dc" not in rest, repr(rest))
a.close()

# 3. Stale responses: pipelined requests, then gone before reading.
a = conn(); a.sendall(req(b"POST", b"/temp", b"111") + req(b"GET", b"/state", auth=False)); a.close()
rs = responses(req(b"GET", b"/state", auth=False), 1)
check("stale: exactly the next client's response", rs[0][0] == 200 and rs[0][2].startswith("{"), str(rs))
s2 = conn(5); s2.sendall(req(b"GET", b"/state", auth=False))
time.sleep(3)
s2.settimeout(1)
data = b""
try:
    while True:
        d = s2.recv(4096)
        if not d:
            break
        data += d
except OSError:
    pass
s2.close()
check("stale: one response per request", data.count(b"HTTP/1.1 ") == 1, repr(data[:200]))
st, _, body = one(req(b"PUT", b"/mode", b"heat"))
check("still accepting writes", st == 200 and '"mode":"heat"' in body, body)

print("\n%d failed" % len(failures))
sys.exit(1 if failures else 0)
EOF
status=$?
if grep -q -i "fatal\|CPU abort\|HardFault" "$log"; then
    echo "renode log reports a fault:" >&2
    grep -i "fatal\|abort\|fault" "$log" >&2
    exit 1
fi
exit $status
