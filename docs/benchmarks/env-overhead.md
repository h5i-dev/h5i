# `h5i env run` isolation overhead

`h5i env run` adds little steady-state overhead on this host. The `process`
tier was within 1% of bare execution for a 338 ms CPU workload. Short
`workspace` runs exposed a separate cost: the current completion loop polls at
25 ms intervals, so a 4 ms command was reported as 30 ms at the median.

These results are one Apple Silicon VM sample, not a cross-platform claim.

## Results

Each cell is the median wall-clock time from five measured runs after one
warm-up. Bare commands were timed around `subprocess.run()` with
`time.perf_counter_ns()`. Sandboxed times came from the `wall` field printed by
`h5i env run` and stored in the env exec event.

| Workload | Isolation | Median wall | Delta vs bare |
|---|---:|---:|---:|
| Python startup | bare | 4.4 ms | baseline |
| Python startup | `workspace` | 30.0 ms | +25.6 ms (+579.1%) |
| Python startup | `process` | 5.0 ms | +0.6 ms (+13.2%) |
| SHA-256 loop | bare | 338.0 ms | baseline |
| SHA-256 loop | `workspace` | 357.0 ms | +19.0 ms (+5.6%) |
| SHA-256 loop | `process` | 340.0 ms | +2.0 ms (+0.6%) |

The short workload was:

```bash
python3 -c 'print(1)'
```

The long workload hashed the same 1 MiB block 1,000 times, processing about
1 GiB through SHA-256 without filesystem writes:

```bash
python3 -c 'import hashlib; d=b"x"*1048576; [hashlib.sha256(d).digest() for _ in range(1000)]'
```

### Raw wall-clock samples

| Workload | Isolation | Five samples (ms) |
|---|---|---|
| Python startup | bare | 4.19, 5.58, 4.42, 4.09, 5.62 |
| Python startup | `workspace` | 30, 35, 26, 32, 25 |
| Python startup | `process` | 5, 5, 7, 5, 5 |
| SHA-256 loop | bare | 330.29, 339.34, 335.88, 347.37, 337.96 |
| SHA-256 loop | `workspace` | 351, 357, 367, 351, 361 |
| SHA-256 loop | `process` | 342, 337, 337, 343, 340 |

The order rotated between bare, `workspace`, and `process` on each repetition
to reduce systematic warm-cache or thermal bias.

## Host

- Date: 2026-07-19
- h5i: 0.2.9 at commit `07b24b1ded5d7f6078e68c0ab01ec27536809205`
- Host: MacBook Air (Mac16,12), Apple M4, 24 GiB RAM
- Virtualization: OrbStack 2.2.1, Ubuntu 24.04.4 LTS
- Guest kernel: Linux 7.0.11-orbstack, aarch64
- Guest allocation visible to the workload: 4 vCPUs, 7.8 GiB RAM, btrfs
- Rust: 1.97.1
- Python: 3.12.3
- Git: 2.43.0

`h5i env probe` reported:

```text
os           = linux
landlock_abi = 8
userns       = true
seccomp      = true
container    = none

claim workspace  satisfiable = yes
claim process    satisfiable = yes
claim container  satisfiable = no
process tier runnable = yes
```

`h5i env capabilities --json` also reported `supervised` as unavailable.
Rootless Podman was not installed, so the `container` tier was not benchmarked.

## Interpretation

The `process` result does not mean that kernel confinement makes commands
faster. Its 0.6 ms long-workload delta is below the run-to-run spread and should
be read as no measurable overhead on this sample.

The `workspace` short-command result includes the runner's 25 ms completion
polling cadence. That is user-visible `h5i env run` latency, but it is not
isolation work. The same cadence explains most of the 19 ms delta on the longer
workload. Direct `git status --short` checks in both generated worktrees were
within 0.01 ms of each other at the median, ruling out a material worktree
layout difference.

## How to reproduce

Build the exact revision and inspect the supported tiers:

```bash
git clone https://github.com/h5i-dev/h5i.git
cd h5i
git checkout 07b24b1ded5d7f6078e68c0ab01ec27536809205

H5I_SKIP_WEB_BUILD=1 cargo build --release --no-default-features
BIN="$PWD/target/release/h5i"

"$BIN" env probe
"$BIN" env capabilities --json
"$BIN" env create bench-workspace --isolation workspace
"$BIN" env create bench-process --isolation process
```

Run this harness from the repository root. It warms every combination once,
rotates execution order, and then prints the raw samples and medians:

```bash
python3 - "$BIN" <<'PY'
import re
import statistics
import subprocess
import sys
import time

h5i = sys.argv[1]
workloads = {
    "short": ["python3", "-c", "print(1)"],
    "long": [
        "python3",
        "-c",
        'import hashlib; d=b"x"*1048576; '
        '[hashlib.sha256(d).digest() for _ in range(1000)]',
    ],
}
envs = {"workspace": "bench-workspace", "process": "bench-process"}
orders = [
    ["bare", "workspace", "process"],
    ["process", "bare", "workspace"],
    ["workspace", "process", "bare"],
    ["bare", "process", "workspace"],
    ["process", "workspace", "bare"],
]
wall_re = re.compile(r"wall (\d+)ms")

def bare(argv):
    start = time.perf_counter_ns()
    subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    return (time.perf_counter_ns() - start) / 1_000_000

def sandbox(env, argv):
    result = subprocess.run(
        [h5i, "env", "run", env, "--", *argv],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    return int(wall_re.search(result.stderr).group(1))

for name, argv in workloads.items():
    bare(argv)
    for env in envs.values():
        sandbox(env, argv)

    samples = {"bare": [], "workspace": [], "process": []}
    for order in orders:
        for tier in order:
            value = bare(argv) if tier == "bare" else sandbox(envs[tier], argv)
            samples[tier].append(value)

    print(name)
    for tier, values in samples.items():
        print(f"  {tier:9} median={statistics.median(values):.2f}ms samples={values}")
PY
```

Clean up the benchmark environments:

```bash
"$BIN" env rm bench-workspace
"$BIN" env rm bench-process
```
