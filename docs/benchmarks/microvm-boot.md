# The microVM tier's boot tax

The `microvm` tier boots a real guest, and this is the first record of it doing
so. It costs **461 ms per command** on this host, of which essentially all is
guest boot: the VM adds no measurable per-syscall tax once it is running. A
warm guest answers the same command in **8.9 ms**, so the tier is carrying a
**~50×** overhead that is structural rather than inherent, and two independent
changes remove most of it. This is the number `docs/roadmap-history.md` M13 asked for before any
optimisation was designed.

One finding runs against the obvious ordering: on macOS the `microvm` tier is
**3.4× faster than `process` and `supervised`** for a realistic short command,
because those tiers add ~1.5 s to Python startup here and the VM adds none.
The strongest tier is also the quickest of the three, which is not what a
"stronger isolation costs more" intuition predicts.

These results are one Apple Silicon host, one image, and `msb` 0.6.8. They are
not a cross-platform claim, and the Linux/KVM path is unmeasured.

## Results

Each cell is the median of five measured runs after one discarded warm-up,
with the tier order rotated between repetitions. `e2e` is wall-clock around
the whole `h5i box run` process — what a caller waits for. `reported` is
h5i's own `wall_ms`, which brackets the sandboxed command alone; for
`microvm` that bracket opens before `msb` is spawned, so it contains the
guest boot.

### Fixed cost — `/usr/bin/true`

The workload does nothing, so what is left is setup and teardown: the
per-command tax, and the only one of the three costs below that a warm-guest
design can remove.

| Isolation | e2e median | vs bare | reported |
|---|---:|---:|---:|
| bare | 2.2 ms | baseline | — |
| `workspace` | 54.7 ms | +52.5 ms | 35 ms |
| `process` | 64.4 ms | +62.2 ms | 35 ms |
| `supervised` | 101.5 ms | +99.3 ms | 34 ms |
| `microvm` | **463.2 ms** | **+461.0 ms** | 426 ms |

### A realistic short command — `python3 -c 'print(1)'`

Interpreter startup opens hundreds of files, so a tier that checks every file
access shows up here and not above. Keeping this workload separate from the
no-op is the point: measured together they read as one number and hide which
cost is which.

| Isolation | e2e median | vs bare | reported |
|---|---:|---:|---:|
| bare | 19.9 ms | baseline | — |
| `workspace` | 55.1 ms | +35.2 ms | 36 ms |
| `process` | 1603.7 ms | +1583.8 ms | 1575 ms |
| `supervised` | 1628.7 ms | +1608.8 ms | 1567 ms |
| `microvm` | 473.6 ms | +453.7 ms | 437 ms |

Subtracting each tier's own no-op cost leaves what the tier charges for the
syscalls themselves: `workspace` −17.3 ms and `microvm` −7.3 ms, both of which
are noise around zero, against `process` +1521.6 ms and `supervised`
+1509.4 ms. **The VM charges nothing per syscall; Seatbelt charges a second
and a half.** That is the reason for the ordering inversion above, and it is
the one cost on this page that warm reuse would *not* remove, because it is
not setup.

### Steady state — 1 GiB through SHA-256

| Isolation | e2e median | vs bare | reported |
|---|---:|---:|---:|
| bare | 451.7 ms | baseline | — |
| `workspace` | 495.5 ms | +43.8 ms | 475 ms |
| `process` | 2040.5 ms | +1588.7 ms | 2010 ms |
| `supervised` | 2067.3 ms | +1615.6 ms | 2002 ms |
| `microvm` | 864.0 ms | +412.3 ms | 824 ms |

The `microvm` delta here (+412 ms) is *smaller* than its own fixed cost
(+461 ms), which would mean the guest ran the hash faster than the host. It
did, and the cause is not virtualisation: the guest runs the image's Python
3.12 while bare runs this host's `/usr/bin/python3` 3.9.6. **Do not read this
row as a CPU-overhead measurement.** Comparing an image-backed tier against a
host baseline compares two interpreters, and the honest conclusion is only
that the VM does not make CPU-bound work meaningfully slower.

## Where the 461 ms goes

Measured directly against `msb`, outside h5i, by taking one h5i behaviour at a
time (8 samples each, median). These were four separate experiments, each with
its own control run, and **the plain-`msb run` control drifted between 230 ms
and 245 ms across them** — so only a difference clearly wider than that spread
means anything, and exactly one is:

| Behaviour tested | Its control | With the behaviour | Difference |
|---|---:|---:|---:|
| h5i's full 16-mount set (vs workspace mount alone) | 237 ms | 254 ms | +17 ms |
| `--script-path` preload, `--rlimit`, `--timeout` | 245 ms | 254 ms | +9 ms |
| default-deny plus three `--net-rule`s | 230 ms | 241 ms | +11 ms |
| `--memory 8192M` (vs the 512M floor) | 230 ms | **384 ms** | **+154 ms** |

The first three are at or inside the control's own drift and should be read as
"no measurable cost", not as small costs. Guest RAM is the one real effect,
and it scales cleanly: 512M → 230 ms, 2048M → 251 ms, 4096M → 284 ms,
8192M → 384 ms, roughly 20 ms per GiB above the floor, with tight spreads
(the 8192M samples ran 377–389 ms). Adding h5i's 25 ms completion-poll cadence
and its own CLI startup accounts for the rest of the 463 ms end-to-end figure.

So the tax has two removable parts, and they are independent:

1. **The profile asks for 8 GiB and pays ~154 ms for it on every command.**
   This looked like a configuration change available today. It is not — see
   the next section, which is where that idea was tested and had to be
   revised.
2. **The remaining ~230 ms is the boot itself**, which only reuse removes.

## The memory cost is recoverable, but not by editing a number

The obvious reading of the table above is "lower `mem_bytes` and take the
154 ms". That trades a ceiling for latency: the guest's RAM *is* its memory
limit at this tier, so a smaller number is a weaker cap, and how much headroom
an agent box needs is a policy question this benchmark cannot answer.

`msb` offers a way out of the trade, and it works. `--max-memory` sets a
boot-time hotpluggable ceiling independently of `--memory`, and it is free:

| Configuration | Boot median | Guest `MemTotal` |
|---|---:|---:|
| `--memory 8192M` | 378.5 ms | 8,140,356 kB |
| `--memory 512M` | 243.8 ms | 491,608 kB |
| `--memory 512M --max-memory 8G` | **237.2 ms** | 491,608 kB |

**But `--max-memory` alone does not grow anything**, and that is the first
thing worth knowing: a 512M/max-8G guest fails a 1.5 GiB allocation with
`MemoryError`, exactly like a plain 512M guest. Booting small and hoping is
not a design.

Growth needs an explicit `msb modify`, and that *does* work on a live guest:

```
$ msb modify <name> --memory 4G          # 9.2 ms
FIELD     REQUESTED    ACTUAL     ENFORCED    STATE
memory    4 GiB        512 MiB    4 GiB       converging
```

The resize is asynchronous — `MemTotal` still read 491,608 kB immediately
after — but the guest then allocated and touched 1.5 GiB successfully where
the un-modified guest had failed, and `MemTotal` settled at 4,161,624 kB.

**The ceiling survives the trick**, which is the part that had to be checked
before recommending any of this: against a 4 GiB enforced cap, allocating
6 GiB fails with `MemoryError` and `MemTotal` stays at 4 GiB. Boot-small-then-
grow is not a way to hand the guest more memory than its profile allows.

The catch is architectural. `msb modify` acts on a **named, running** sandbox,
and today's tier boots a VM, runs one command inside it, and destroys it —
there is no moment between "guest exists" and "command runs" in which h5i
could issue the modify. So this 141 ms is **not** available to the current
one-shot design at any price short of lowering the ceiling. It composes with
M13 step 2 instead, where the sequence becomes `msb create --memory 512M
--max-memory <ceiling>` once, one `msb modify` at ~9 ms, and then execs — and
where it is worth having anyway, because it also cuts the one boot per box
that reuse cannot amortise.

### What reuse is worth

`msb` supports a persistent sandbox: `msb create --name X` boots detached, and
`msb exec X -- cmd` attaches to the running guest over its agent relay socket
without booting. Measured on this host, 15 samples each:

| Path | Median | Min | p90 | Max |
|---|---:|---:|---:|---:|
| `msb run` (boot per command) | 233.9 ms | 230.8 ms | 243.2 ms | 243.3 ms |
| `msb exec` into a warm guest | **8.4 ms** | 8.0 ms | 9.0 ms | 10.8 ms |

**28× on the `msb` primitive alone**, and the warm path is independent of
guest size: exec into an 8 GiB guest measured 8.9 ms, the same as into a
512 MiB one, so reuse absorbs the memory cost too. Against h5i's current
461 ms of fixed cost, a warm guest reachable in ~9 ms is the ~50× the summary
claims. State persists across execs as expected — a file written by one exec
is readable by the next.

## Two things reuse depends on, checked

### Sandboxes do not share a writable filesystem

forkd's children share one `rootfs.ext4` host file read-write, which is how
they got ext4 corruption when three wrote concurrently. Whether `msb` has the
same shape decides if concurrent microvm boxes are safe, and it matters more
once guests are long-lived. It does not. A write to `/opt` — deliberately not
`/tmp` — in one sandbox was visible only to itself:

| Reader | Sees `/opt/marker-A`? |
|---|---|
| the writing sandbox, on a later exec | yes |
| a sibling running concurrently from the same image | **no** |
| a sandbox created *after* the write | **no** |
| a fresh one-shot `msb run` off the same image | **no** |

Each sandbox carries ~3.8 MB of its own state under
`$MSB_HOME/sandboxes/<name>/`, released on `msb rm` (the directory returns to
0 B). The exact mechanism — differencing layer, overlay, or copy — was not
established; only the observable behaviour above, which is what the design
question needed. **So the `/tmp`-only convention forkd has to enforce is not a
constraint h5i inherits**, and concurrent boxes from one image do not race for
a shared inode.

### `msb exec` auto-start is a trap, and the state machine is h5i's to own

Upstream documents that `exec` auto-starts a stopped sandbox. It does — but
not into the fast path, and this is the single most important measurement for
a reuse design:

| Guest state | `msb exec` median | State afterwards |
|---|---:|---|
| `running` | **8.5–9.3 ms** | `running` |
| `stopped` | **~236 ms** | still `stopped` |

Exec into a stopped guest is a one-shot boot wearing the fast path's name: it
boots, runs, tears down, and leaves the sandbox stopped, so the *next* exec
pays ~236 ms again and the guest never re-warms on its own. What restores it
is an explicit `msb start` (~143 ms), after which execs are 9.3 ms and the
state stays `running`.

The consequence is that **an idle timeout which stops a guest silently reverts
the tier to its current per-command cost, permanently, until something starts
it** — the worst of both designs, since it also keeps the guest's disk around.
Reuse has to track state and start explicitly rather than lean on exec's
convenience.

One reassurance from the same run: guest state survives the cycle. The `/opt`
write made before a `stop` was still readable after `start`, so stopping a
guest is a latency decision rather than a data-loss one.

## What reuse actually delivered (M13 step 2, built 2026-08-13)

The warm-guest path is implemented, and re-running the same harness against it
turns the projection above into a measurement. **Fixed cost per command fell
from 461.0 ms to 43.0 ms — 10.7×** — and the tier's position among its
neighbours inverted completely:

| Isolation | Fixed cost before | Fixed cost after | e2e noop | h5i's own `wall` |
|---|---:|---:|---:|---:|
| `workspace` | 52.5 ms | 53.0 ms | 55.2 ms | 35 ms |
| `process` | 62.2 ms | 62.9 ms | 65.1 ms | 35 ms |
| `supervised` | 99.3 ms | 98.9 ms | 101.1 ms | 34 ms |
| `microvm` | **461.0 ms** | **43.0 ms** | 45.3 ms | **10 ms** |

**The strongest tier is now the cheapest one on this host.** A microVM box pays
less per command than a `workspace` box, because what remains at every tier is
h5i's own CLI overhead — and the microvm path, having no host-side sandbox
machinery to set up, has less of it than the tiers that do.

Two implementation findings are worth recording because neither was predicted:

- **The 25 ms completion poll became the dominant cost.** `docs/benchmarks/env-overhead.md`
  flagged this cadence back in 2026-07-19, when it inflated a 4 ms command to
  30 ms; on a 230 ms boot it was invisible, and on a 9 ms exec it was most of
  the number. The first working warm path reported 35 ms for an exec `msb` does
  in ~9 ms. Replacing the flat cadence with a backoff (1 ms doubling to 25 ms)
  took h5i's reported wall to **10 ms**, which matches the runtime's own cost
  almost exactly, and the fixed cost from 65.5 ms to 43.0 ms. The one-shot path
  keeps the flat cadence, where it is still lost in the boot.
- **A pre-existing bug meant the orphan sweep had never reaped anything.** The
  marker directory was computed as `marker_path("").parent()`, and joining an
  empty component yields a trailing separator, so `parent()` walked up past the
  marker directory to the temp directory itself. The sweep scanned `/tmp` for
  names that only ever exist one level below it, matched nothing, and — being
  best-effort at every step — said nothing. It mattered little while guests
  died with their process; it matters a great deal now that they outlive it, so
  it is fixed and covered by a test.

The long workload confirms the guest is no slower to work in than before
(+36.6 ms over bare, against `workspace`'s +42.9 ms), and the syscall-heavy
short workload still shows the VM charging nothing per syscall (−7.0 ms, noise)
while `process` and `supervised` charge ~1.5 s.

## Negative and null results

Kept deliberately, because a benchmark that reports only what moved reads as
though everything was tried and everything mattered.

- **Mounts are nearly free.** h5i's 16-mount set, including a 74 MiB
  `.git/objects` with 1,957 files, cost +17 ms over mounting the workspace
  alone. The hypothesis that virtiofs setup dominated the boot was wrong, and
  it was the first thing tried.
- **Egress rules are free.** Default-deny plus three `--net-rule`s came in
  +11 ms over no network flags at all, inside the control's drift.
- **The `allow@<host>` form does not resolve DNS at boot.** h5i emits
  `allow@api.anthropic.com` where the tested alternative was
  `allow@domain=…`; the suspicion that the bare form triggered a resolution
  per rule (which would have been ~50 ms × 3) is refuted — 240.8 ms against
  230.4 ms, inside the spread.
- **The preload script is not a boot cost.** `--script-path` plus the guest's
  extra `sh` exec added ~9 ms, so the credential-safety mechanism documented
  in `microvm.rs` is not paid for in latency. That matters for M13 step 2,
  where the same mechanism has to carry over to `msb exec`.

## One anomaly, unexplained — seen twice

**`msb exec` has hung indefinitely twice**, both times killed after 600 s,
against roughly a hundred successful execs across this work. Both were run
directly from an interactive shell in the form
`msb exec <name> -- sh -c '<cmd>' 2>&1 | tail -N`. The second occurrence was
on the first exec after a `create`; the first was on a later exec into a
sandbox that had already served five.

Deliberate attempts to reproduce it have all failed:

- The same command shape, driven from a controlled harness with a fresh
  sandbox each time: **0 hangs in 6 attempts**.
- Stdin as `/dev/null`, as an open pipe, and inherited: no difference, ~9 ms
  each, across four command shapes.
- With and without `--max-memory` (the flag the second hung sandbox happened
  to carry, which made it the obvious suspect): both fine, ~10–38 ms.
- The sandbox that hung was still `running` afterwards and answered the same
  exec normally minutes later.

So: intermittent, roughly 2 in 100, not tied to any variable tested, and not
diagnosed. It is recorded rather than filed because a warm-guest design makes
`exec` the hot path, and a rare unbounded hang there is exactly the kind of
thing that is cheap to know about now and expensive to discover once it is
load-bearing. **M13 step 2 should treat an exec as something that can hang and
needs its own deadline**, which is the same lesson `wait_vm`'s existing
host-side backstop already encodes for `msb run`.

## Host

- Date: 2026-08-13
- h5i: 0.3.3 at commit `d83af8f44015c09581460e9e5c07266a7867105c`, plus the
  uncommitted harness — the binary measured is that commit's code
- Host: Apple Silicon (arm64), macOS 26.5 (build 25F71)
- microVM runtime: `msb` 0.6.8 (microsandbox), libkrun via Hypervisor.framework
- Guest image: `python:3.12-slim`, pre-pulled (`msb pull`), 1.7 s
- Host Python (bare baseline): 3.9.6 at `/usr/bin/python3`
- Profile: `agent-claude` (8 GiB cap, three-host egress allowlist)

`h5i env capabilities --json` reported `microvm` satisfiable with
`egress_enforced_l3: true` and `strongest_tier: microvm`. The `container` tier
was not measured: rootless Podman is not installed on this host.

## Interpretation

**The tier works.** Before this run the adapter had never booted a real guest
anywhere — it was unit-tested against its argv and its rule translation, and
`docs/roadmap-history.md` §9 said so. A microvm box now creates, runs a command, enforces its
allowlist in the guest netstack, and exits 0.

**Its cost is boot, not isolation.** The near-zero per-syscall and CPU deltas
say the guest is not a slow place to work; it is an expensive place to
*start*. That is precisely the cost profile that amortises, which is why M13
sequences reuse ahead of anything more exotic, and why the fork-from-warm
machinery that motivated the comparison is not needed to capture most of the
win here.

**The `process` and `supervised` result is a separate problem.** ~1.5 s added
to every Python startup under Seatbelt is user-visible on the tiers macOS
users get by default, and it is not a fixed cost that reuse can hide. It was
found by accident, while building a benchmark aimed at something else, and it
is not diagnosed here: the plausible suspects are macOS's
`/usr/bin/python3` Command Line Tools shim and SBPL evaluation across a
startup that opens hundreds of files, but which one it is has not been
established. It deserves its own investigation.

## How to reproduce

The harness is `scripts/bench_env_overhead.py`, committed so the numbers above
can be re-derived rather than trusted.

```bash
git clone https://github.com/h5i-dev/h5i.git
cd h5i && git checkout d83af8f44015c09581460e9e5c07266a7867105c
H5I_SKIP_WEB_BUILD=1 cargo build --release --no-default-features

# The microVM runtime. Apple Silicon or Linux+KVM only; the installer
# verifies a SHA-256 and installs under $HOME with no sudo.
curl -fsSL https://install.microsandbox.dev | sh
export PATH="$HOME/.local/bin:$PATH"
msb doctor
msb pull python:3.12-slim        # runs never pull, so pre-pull is required

./scripts/bench_env_overhead.py \
    --bin target/release/h5i \
    --tiers workspace,process,supervised,microvm \
    --image python:3.12-slim \
    --reps 5 --json bench.json
```

The harness records every sample in `bench.json`, prints medians as a view of
them, and reports any tier it could not run with the reason h5i's own
capability probe gave. It exits non-zero when a requested tier was skipped, so
"not measured" is distinguishable from "measured and fine".

The `msb`-level cold-versus-warm comparison is not part of the harness, since
it measures the runtime rather than h5i:

```bash
msb create --pull never --name warm python:3.12-slim
msb exec warm -- /bin/true        # warm path
msb run --quiet --pull never python:3.12-slim -- /bin/true   # cold path
msb rm --force warm
```

## See also

- `docs/benchmarks/env-overhead.md` — the earlier `workspace`/`process`
  measurement on a Linux guest, which this does not supersede: different
  host, different date, different tiers.
- `docs/roadmap-history.md` M13 — the plan this measurement was taken to size.
