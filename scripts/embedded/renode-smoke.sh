#!/usr/bin/env bash
# Boot the thermostat firmware in Renode and talk HTTP to it over its UART.
#
#   scripts/embedded/install-renode.sh      # once
#   scripts/embedded/renode-smoke.sh        # builds the firmware, then checks
#
# Checks reads and signed writes (a missing or replayed signature is 401),
# that the temperature comes only from the TMP108 on I2C (set through the
# Renode monitor) and drives the heater interlock, that the GPIO outputs
# follow the committed state, that the state survives a reset (flash) and
# the boot id changes, pipelining, a chunked body, the smuggling rejections
# (CL+TE, duplicate CL, obs-fold, space before the colon, an oversized head,
# a bad chunk size, ...), and that the device still answers afterwards.
#
# The UART is framed (h5i_app_embedded::link); clients reach it through
# uart-gateway.py on PORT, which talks to Renode's UART socket on PORT+1.
# Also checks the shared-line attacks: a partial request left by one client
# is never completed by the next one's bytes, a silent client is cut off
# within the device's bound, and no client receives another's response.
#
# Environment: RENODE (default ~/.local/opt/renode/renode), PORT (18080; the
# monitor is on PORT+2), FIRMWARE (a prebuilt ELF; skips the build),
# CARGO_TARGET_DIR.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
renode="${RENODE:-$HOME/.local/opt/renode/renode}"
port="${PORT:-18080}"
uart_port=$((port + 1))
mon_port=$((port + 2))
fw_dir="$root/examples/embedded/thermostat/firmware"

if [ -z "${FIRMWARE:-}" ]; then
    (cd "$fw_dir" && cargo build --release --locked)
    target="${CARGO_TARGET_DIR:-$fw_dir/target}"
    FIRMWARE="$target/thumbv7em-none-eabihf/release/thermostat-firmware"
fi
[ -x "$renode" ] || { echo "no Renode at $renode: run scripts/embedded/install-renode.sh" >&2; exit 2; }
[ -f "$FIRMWARE" ] || { echo "no firmware at $FIRMWARE" >&2; exit 2; }

log="$(mktemp)"
# The monitor on a telnet port: the checks set the sensor and read the pins.
"$renode" --disable-xwt --port "$mon_port" \
    -e "\$elf=@$FIRMWARE; \$port=$uart_port; include @$root/scripts/embedded/thermostat.resc" >"$log" 2>&1 </dev/null &
renode_pid=$!
python3 "$root/scripts/embedded/uart-gateway.py" "$port" "$uart_port" 6 >>"$log" 2>&1 </dev/null &
gateway_pid=$!
cleanup() {
    kill "$gateway_pid" 2>/dev/null || true
    pkill -P "$renode_pid" 2>/dev/null || true
    kill "$renode_pid" 2>/dev/null || true
}
trap cleanup EXIT

PORT="$port" MON="$mon_port" LOG="$log" SCRIPTS="$root/scripts/embedded" python3 - <<'EOF'
import os, re, socket, sys, time
sys.path.insert(0, os.environ["SCRIPTS"])
import importlib.util
spec = importlib.util.spec_from_file_location("h5i_hmac", os.path.join(os.environ["SCRIPTS"], "h5i-hmac.py"))
h5i_hmac = importlib.util.module_from_spec(spec); spec.loader.exec_module(h5i_hmac)

PORT = int(os.environ["PORT"])
MON = int(os.environ["MON"])
KEY = b"dev-key"
ODR_D = 0x40020C14
HEATER, RELAY = 1 << 12, [1 << 13, 1 << 14, 1 << 15, 1 << 11]

def wait_up():
    for _ in range(120):
        try:
            socket.create_connection(("127.0.0.1", PORT), timeout=1).close()
            return
        except OSError:
            time.sleep(0.5)
    print(open(os.environ["LOG"]).read())
    sys.exit("renode did not open the UART socket")

# --- The Renode monitor ------------------------------------------------------
mon_sock = None
def mon(cmd):
    """Run a monitor command; its output without the prompt."""
    global mon_sock
    if mon_sock is None:
        mon_sock = socket.create_connection(("127.0.0.1", MON), timeout=10)
        time.sleep(0.5)
        mon_sock.settimeout(0.5)
        try:
            while mon_sock.recv(65536):
                pass
        except OSError:
            pass
    mon_sock.sendall(cmd.encode() + b"\n")
    out = b""
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            d = mon_sock.recv(65536)
        except OSError:
            if b"(thermostat)" in out:
                break
            continue
        out += d
    text = re.sub(r"\x1b\[[0-9;]*m", "", out.decode(errors="replace"))
    return text.replace(cmd, "").replace("(thermostat)", "").strip()

def set_temp(c):
    mon("i2c1.tmp108 Temperature %d" % c)

def pins():
    return int(re.search(r"0x[0-9a-fA-F]+", mon("sysbus ReadDoubleWord 0x%x" % ODR_D)).group(0), 16)

def wait_pins(pred, timeout=10):
    """GPIOD's output register once `pred` holds of it (or at the timeout)."""
    deadline = time.time() + timeout
    p = pins()
    while not pred(p) and time.time() < deadline:
        time.sleep(0.3)
        p = pins()
    return p

def reset():
    """Reset the board; the ELF stays in flash, as does the store."""
    sp = re.search(r"0x[0-9a-fA-F]+", mon("sysbus ReadDoubleWord 0x08000000")).group(0)
    pc = re.search(r"0x[0-9a-fA-F]+", mon("sysbus ReadDoubleWord 0x08000004")).group(0)
    for c in ["pause", "machine Reset", "sysbus.cpu VectorTableOffset 0x08000000",
              "sysbus.cpu SP " + sp, "sysbus.cpu PC " + pc, "start"]:
        mon(c)

# --- HTTP --------------------------------------------------------------------
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

def req(method, path, body=b"", auth=None, extra=b""):
    h = b"%s %s HTTP/1.1\r\nHost: dev\r\n" % (method, path)
    if auth:
        h += b"Authorization: " + auth + b"\r\n"
    if body or method in (b"PUT", b"POST"):
        h += b"Content-Length: %d\r\n" % len(body)
    return h + extra + b"\r\n" + body

class Signer:
    def __init__(self):
        self.refresh()
    def refresh(self):
        st, head, _ = one(req(b"PUT", b"/mode", b"off"))
        m = re.search(r"boot=([0-9a-f]{8}),last=([0-9]+)", head)
        self.boot, self.counter = m.group(1).encode(), int(m.group(2))
        return st, head
    def auth(self, method, path, body):
        self.counter += 1
        return h5i_hmac.authorization(KEY, self.boot, self.counter, method, path, body)
    def req(self, method, path, body=b"", extra=b""):
        return req(method, path, body, self.auth(method, path, body), extra)

def state():
    return one(req(b"GET", b"/state"))[2]

def wait_for(pred, timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = state()
        if pred(s):
            return s
        time.sleep(0.5)
    return state()

failures = []
def check(name, cond, detail=""):
    print(("ok    " if cond else "FAIL  ") + name + ("" if cond else "  " + detail))
    if not cond:
        failures.append(name)

wait_up()
time.sleep(1)
set_temp(20)

st, _, body = one(req(b"GET", b"/state"))
check("GET /state", st == 200 and '"mode":"off"' in body, body)
sig = Signer()
st, head = sig.refresh()
check("unsigned write is 401 with a challenge", st == 401 and "WWW-Authenticate: H5i-HMAC boot=" in head, head)
st, _, _ = one(req(b"PUT", b"/mode", b"heat", b"Bearer dev-key"))
check("the key itself is not a credential", st == 401)

body = wait_for(lambda s: '"temp_dc":200' in s)
check("the sensor's 20 C reaches the kernel", '"temp_dc":200' in body, body)
st, _, body = one(sig.req(b"PUT", b"/mode", b"heat"))
check("signed PUT /mode heat turns the heater on", st == 200 and '"heater":true' in body, body)
p = wait_pins(lambda p: p & HEATER)
check("heater pin follows", (p & HEATER) != 0, hex(p))

set_temp(36)
body = wait_for(lambda s: '"heater":false' in s)
check("interlock: the sensor's 36 C turns the heater off", '"temp_dc":36' in body and '"heater":false' in body, body)
p = wait_pins(lambda p: not p & HEATER)
check("heater pin off", (p & HEATER) == 0, hex(p))
st, _, _ = one(sig.req(b"POST", b"/temp", b"100"))
check("no temperature over HTTP, even signed", st == 404)
set_temp(20)
wait_for(lambda s: '"heater":true' in s)

st, _, body = one(sig.req(b"PUT", b"/relay/2", b"on"))
check("PUT /relay/2", st == 200 and '"relays":[false,false,true,false]' in body, body)
p = wait_pins(lambda p: p & RELAY[2])
check("relay 2 pin follows", (p & RELAY[2]) != 0 and (p & RELAY[0]) == 0, hex(p))
st, _, body = one(sig.req(b"PUT", b"/relay/7", b"on"))
check("relay 7 refused by the kernel", st == 404 and "no_such_relay" in body, body)
st, _, _ = one(sig.req(b"PUT", b"/setpoint", b"999"))
check("setpoint out of range refused", st == 422)
captured = sig.req(b"PUT", b"/setpoint", b"210")
st, _, _ = one(captured)
check("signed setpoint", st == 200)
st, _, _ = one(captured)
check("a captured request replayed is 401", st == 401)

rs = responses(sig.req(b"PUT", b"/setpoint", b"150") + req(b"GET", b"/state")
               + sig.req(b"PUT", b"/setpoint", b"215"), 3)
check("pipelined: three responses", [r[0] for r in rs] == [200, 200, 200], str([r[0] for r in rs]))
check("pipelined: in order", '"setpoint_dc":150' in rs[1][2] and '"setpoint_dc":215' in rs[2][2], rs[2][2])

chunked = (b"PUT /setpoint HTTP/1.1\r\nHost: dev\r\nAuthorization: " + sig.auth(b"PUT", b"/setpoint", b"205")
           + b"\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n2\r\n2\r\n05\r\n0\r\n\r\n")
st, _, body = one(chunked)
check("chunked body (signed as decoded)", st == 200 and '"setpoint_dc":205' in body, body)

bad = [
    ("CL + TE", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
    ("duplicate CL", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n100", 400),
    ("obs-fold", b"GET /state HTTP/1.1\r\nHost: d\r\nX: a\r\n b\r\n\r\n", 400),
    ("space before colon", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nContent-Length : 3\r\n\r\n100", 400),
    ("TE space before colon", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nTransfer-Encoding : chunked\r\n\r\n0\r\n\r\n", 400),
    ("bare LF", b"GET /state HTTP/1.1\nHost: d\n\n", 400),
    ("TE not chunked", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: gzip, chunked\r\n\r\n", 501),
    ("bad chunk size", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 400),
    ("chunk extension", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nTransfer-Encoding: chunked\r\n\r\n3;x\r\n100\r\n0\r\n\r\n", 400),
    ("no Host", b"GET /state HTTP/1.1\r\n\r\n", 400),
    ("body too large", b"PUT /setpoint HTTP/1.1\r\nHost: d\r\nContent-Length: 5000\r\n\r\n", 413),
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

st, _, body = one(req(b"GET", b"/state"))
check("still serving after the rejections", st == 200 and '"setpoint_dc":205' in body, body)

# --- The shared line ---------------------------------------------------------
def conn(t=20):
    return socket.create_connection(("127.0.0.1", PORT), timeout=t)

# 1. Hijack: an attacker leaves a partial head and goes; the victim follows.
a = conn(); a.sendall(b"PUT /relay/3 HTTP/1.1\r\nX-Pad: "); a.close()
st, _, body = one(sig.req(b"PUT", b"/relay/0", b"on"))
check("hijack: the victim's own request ran", st == 200 and '"relays":[true,false,true,false]' in body, body)
st, _, body = one(req(b"GET", b"/state"))
check("hijack: relay 3 untouched", '"relays":[true,false,true,false]' in body, body)

# 2. A silent client (request line, then nothing, socket open) holds the line
#    no longer than the device's bound; the next client is then served.
a = conn(); a.sendall(b"GET /state HTTP/1.1\r\n")
time.sleep(0.2)
t0 = time.time()
try:
    st, _, body = one(req(b"GET", b"/state"))
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
a = conn(); a.sendall(req(b"GET", b"/state") + req(b"GET", b"/state")); a.close()
rs = responses(req(b"GET", b"/state"), 1)
check("stale: exactly the next client's response", rs[0][0] == 200 and rs[0][2].startswith("{"), str(rs))
s2 = conn(5); s2.sendall(req(b"GET", b"/state"))
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

# --- Reset ---------------------------------------------------------------------
st, _, _ = one(sig.req(b"PUT", b"/mode", b"auto"))
st2, _, body = one(sig.req(b"PUT", b"/setpoint", b"230"))
check("writes before the reset", st == 200 and st2 == 200, body)
old_boot = sig.boot
stale = sig.req(b"PUT", b"/setpoint", b"100")
reset()
wait_up()
body = wait_for(lambda s: '"setpoint_dc"' in s, 30)
check("after a reset: the committed state is back", '"setpoint_dc":230' in body and '"mode":"auto"' in body
      and '"relays":[true,false,true,false]' in body, body)
sig.refresh()
check("after a reset: a new boot id", sig.boot != old_boot, "%r" % sig.boot)
st, _, _ = one(stale)
check("after a reset: a request signed before it is 401", st == 401)
st, _, body = one(sig.req(b"PUT", b"/mode", b"heat"))
check("after a reset: still accepting writes", st == 200 and '"mode":"heat"' in body, body)

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
