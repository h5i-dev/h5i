# Reaching a dev server inside a microVM guest

`h5i box share` refuses the `microvm` tier: the shared port lives in the
guest's network stack, and h5i finds a box's dev server by identifying the host
process holding the port — a VM has no such process. This measures whether the
obvious fix is viable, finds that it is not, and measures the alternative,
which is.

**Result: an `msb exec --stream` tunnel reaches a guest dev server in 20.5 ms
per connection and moves 34–59 MiB/s, and it works on a box with no network at
all.** That last property is the reason to prefer it: the data path is h5i's
own exec channel rather than the guest's netstack, so sharing a box opens no
ingress hole in the boundary the tier exists to provide.

These are one Apple Silicon host and `msb` 0.6.8. Nothing here is implemented;
this is the measurement that decides a design.

## Why not publish a port

`msb` has `-p, --port <BIND_ADDR:HOST:GUEST>`, which is exactly the mechanism a
port share wants, and it is **create-time only** — `msb modify` has no `--port`.

That collides with how warm guests are named. A guest's name is a SHA-256 of
its create argv (M13 step 2), so adding a forward changes the name, which
creates a *new* guest and reaps the old one — killing the dev server that was
to be shared. Sharing would destroy its own subject. The alternative, opening a
port on every box at creation against the possibility of a later share, costs
every box an ingress hole for a feature most will never use, and requires
knowing the port before the service that binds it exists.

## The exec tunnel

`msb exec --stream` gives bidirectional byte-faithful stdio with no PTY (no
echo, no CRLF translation). One exec per accepted connection, splicing the TCP
stream to its stdin/stdout, needs no create-time change and therefore rotates
no guest.

### The channel itself

`msb exec --stream <guest> -- cat`, echoing a payload back. This isolates the
channel from any forwarder.

| Payload | Median | Throughput |
|---|---:|---:|
| 0 B (setup only) | 8.64 ms | — |
| 1 KiB | 8.62 ms | — |
| 64 KiB | 9.71 ms | 6.4 MiB/s |
| 1 MiB | 25.74 ms | 38.9 MiB/s |
| 8 MiB | 135.94 ms | 58.8 MiB/s |

Setup is ~8.6 ms and dominates anything under about 64 KiB; past that the
channel settles around 59 MiB/s (~470 Mbit/s).

### End to end, through real HTTP

A host-side bridge listening on loopback, one `msb exec --stream` per
connection, an in-guest forwarder connecting to `127.0.0.1:8000`, and Python's
`http.server` in the guest:

| Request | Median | |
|---|---:|---|
| Small (267 B directory listing) | **20.5 ms** | |
| 1 MiB body | **29.5 ms** | ~34 MiB/s |

The 20.5 ms decomposes as ~8.6 ms exec setup, ~5.5 ms Python interpreter
startup in the forwarder, and the remainder in the guest's own TCP connect and
response.

**This cost is per connection, not per request.** With HTTP keep-alive a
browser holds connections open across a page load, so a page pays it a handful
of times rather than once per asset.

### The forwarder's interpreter tax

| In-guest command | Median |
|---|---:|
| `cat` (no interpreter) | 8.64 ms |
| `python3 -c pass` | 14.17 ms |

~5.5 ms of the end-to-end figure is Python starting up. A small static
forwarder binary would remove it, taking a small request to roughly 15 ms — but
speed is the lesser reason to want one (see below).

## The property that decides it

On a guest created with `--no-net`:

- Outbound is genuinely gone — `1.1.1.1:443` fails with `ConnectionRefusedError`.
- A dev server on the guest's loopback still works — `200`.
- **The exec tunnel still reaches it** — `HTTP/1.0 200 OK`.

So a box with *no network access whatsoever* can still be shared. Loopback is
not the network, and the tunnel is not either: it rides h5i's exec channel.
Port publishing cannot make this claim, because it works by opening the
netstack.

## Services survive a warm guest

Started with `setsid`, a dev server in the guest answered `200` immediately and
again after unrelated `msb exec` calls in between. This is a consequence of
M13 step 2 rather than of anything here: before warm guests, the VM died with
the command that started it, so there was never a dev server to share.

## What this does not settle

- **The in-guest forwarder is an unsolved dependency.** `nc` and `socat` are
  absent from slim images and `/dev/tcp` is a bash builtin, not POSIX `sh`.
  Python was used here because the test image has it; depending on an
  interpreter being present makes the feature fail differently per image. The
  likely answer is a small static binary shipped with h5i and staged into a
  mounted directory, which is a real piece of work and the first thing to
  decide.
- **One exec per connection is not pooled.** At 8.6 ms it does not need to be
  yet; it would become worth doing before anything latency-sensitive.
- **`box service` does not exist at this tier**, so the dev server above was
  started by hand with `setsid`. Sharing has nothing to share until that lands.
- **`share.rs`'s discovery path assumes a host process holds the port** and
  must be replaced rather than adapted. The port has to come from h5i's own
  records: a joiner must never influence what the exec runs.

## How to reproduce

Requires `msb` (see `docs/benchmarks/microvm-boot.md` for install) and a
pre-pulled image.

```bash
msb create --pull never --name tun --memory 1024M python:3.12-slim

# the channel, without a forwarder
head -c 1048576 /dev/zero | msb exec --stream tun -- cat | wc -c

# a dev server that outlives the exec that started it
msb exec tun -- sh -c 'cd /tmp && setsid nohup python3 -m http.server 8000 >/dev/null 2>&1 &'
msb exec --stream tun -- python3 -c "
import socket,sys
s=socket.create_connection(('127.0.0.1',8000))
s.sendall(b'GET / HTTP/1.0\r\nHost: x\r\n\r\n')
d=b''
while True:
    c=s.recv(65536)
    if not c: break
    d+=c
print(d.decode(errors='replace').splitlines()[0])"

msb rm --force tun
```

Repeat the last two steps against a guest created with `--no-net` to reproduce
the isolation result.

## See also

- `docs/benchmarks/microvm-boot.md` — the boot tax, and the warm-guest work
  that made a persistent dev server possible at all.
- `docs/roadmap-history.md` M14 — the `box service` design this measurement feeds.
