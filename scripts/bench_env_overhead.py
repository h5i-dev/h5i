#!/usr/bin/env python3
"""Measure what each h5i isolation tier costs per command.

The number this exists to produce is **fixed cost**: the part of a run that is
paid before the workload starts and again on the next command. For the kernel
tiers it is process setup; for `microvm` it is a guest boot, and it is the
number that decides whether keeping one guest warm across a box's commands is
worth building (roadmap-history.md M13).

Method, and why each part is here:

- **Two workloads.** A near-empty one (`short`) whose runtime is almost all
  fixed cost, and a CPU-bound one (`long`) whose delta over bare execution is
  steady-state overhead. Fixed cost is read off the short workload; the long
  one is the control that says whether the tier also slows work down once it
  is running.
- **Two clocks.** `e2e` is wall-clock around the whole `h5i box run` process,
  which is what a caller actually waits for. `reported` is h5i's own `wall_ms`
  from `--json`, which brackets the sandboxed command alone (for `microvm`
  that bracket opens before `msb` is spawned, so it contains the guest boot).
  The gap between them is h5i's own CLI overhead, and separating the two keeps
  a slow binary from being reported as a slow tier.
- **Order rotation and warm-up.** Every repetition runs the tiers in a
  different order after one discarded warm-up per combination, so a warm page
  cache or a thermal ramp cannot land entirely on whichever tier went first.
- **Refusals are results.** A tier this host cannot run is recorded with the
  reason the capability probe gave, not dropped from the output. A benchmark
  that silently omits what it could not measure reads as a complete sweep.

Every sample is kept in the JSON artifact; the medians in the table are a view
of it, not a replacement for it.

Usage:

    scripts/bench_env_overhead.py --bin target/release/h5i
    scripts/bench_env_overhead.py --tiers workspace,process,microvm \
        --image ghcr.io/h5i-dev/agent-claude:latest --json bench.json

Exit status is 0 when every requested tier was measured, and 1 when one was
requested explicitly but could not run, so CI can tell "not measured" from
"measured and fine".
"""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass, field
from typing import Iterable

ALL_TIERS = ["workspace", "process", "supervised", "container", "microvm"]

def _true_binary() -> str:
    """A real `true` executable, not the shell builtin.

    The no-op probe has to be a binary the tier actually has to set up and
    execute; `true` as a builtin would measure the shell, not the tier.
    """
    for candidate in ("/usr/bin/true", "/bin/true"):
        if os.path.exists(candidate):
            return candidate
    found = shutil.which("true")
    if not found:
        raise SystemExit("no `true` binary found; the no-op probe needs one")
    return found


# The no-op probe runs in two different filesystems. `/usr/bin/true` exists on
# macOS but not in every Linux image; `/bin/true` exists in Debian- and
# Alpine-based images alike but not on macOS. Resolving one path on the host and
# then executing it *inside a guest* is how this script used to abort the whole
# sweep — for `microvm`, the tier it exists to measure.
GUEST_TRUE = "/bin/true"

WORKLOADS: dict[str, list[str]] = {
    # Fixed-cost probe. The workload does nothing, so what is left is the cost
    # of setting the tier up and tearing it down: the per-command tax, and the
    # only one of the three a warm-guest design can remove.
    "noop": [_true_binary()],
    # A realistic short command. It is *not* the fixed-cost probe: interpreter
    # startup opens hundreds of files, so a tier that checks every file access
    # shows up here and not in `noop`. Keeping the two apart is the point —
    # measured together they read as one number and hide which cost is which.
    "short": ["python3", "-c", "print(1)"],
    # Steady-state probe: ~1 GiB through SHA-256, no filesystem writes and
    # almost no syscalls, so this isolates CPU-path overhead from both above.
    "long": [
        "python3",
        "-c",
        'import hashlib; d=b"x"*1048576; '
        "[hashlib.sha256(d).digest() for _ in range(1000)]",
    ],
}


@dataclass
class Series:
    """Samples for one (workload, tier) cell, in both clocks."""

    e2e: list[float] = field(default_factory=list)
    reported: list[float] = field(default_factory=list)

    def summary(self, values: list[float]) -> dict[str, float | None]:
        if not values:
            return {"n": 0, "median": None, "p90": None, "min": None, "max": None}
        ordered = sorted(values)
        # Nearest-rank p90: with 5 samples this is the 5th, i.e. the worst.
        # Stated plainly because a p90 over few samples is a range, not a tail.
        # `ceil`, not `round` — Python rounds halves to even, so `round(4.5)`
        # is 4 and the default 5-sample run would report the 4th value as p90,
        # understating the tail in every published table.
        rank = max(0, min(len(ordered) - 1, math.ceil(0.9 * len(ordered)) - 1))
        return {
            "n": len(ordered),
            "median": statistics.median(ordered),
            "p90": ordered[rank],
            "min": ordered[0],
            "max": ordered[-1],
        }

    def as_dict(self) -> dict:
        return {
            "e2e_ms": {"samples": self.e2e, **self.summary(self.e2e)},
            "reported_ms": {"samples": self.reported, **self.summary(self.reported)},
        }


def run_out(argv: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(argv, capture_output=True, text=True)


def probe_capabilities(binary: str) -> dict:
    """Ask h5i what this host can enforce. Fresh, not the memoised view."""
    proc = run_out([binary, "env", "capabilities", "--json"])
    if proc.returncode != 0:
        raise SystemExit(
            f"`{binary} env capabilities --json` failed ({proc.returncode}):\n{proc.stderr}"
        )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"capabilities output was not JSON: {exc}\n{proc.stdout[:400]}")


def tier_blocker(caps: dict, tier: str, image: str | None) -> str | None:
    """Return why `tier` cannot be measured here, or None if it can.

    The reason is h5i's own `note` from the capability probe rather than a
    story this script invents, so what the benchmark records as the blocker is
    the same sentence `box create` would refuse with.
    """
    claim = next((c for c in caps.get("claims", []) if c.get("claim") == tier),
    None) if claim is None:
        return f"host probe reported no `{tier}` claim"
    note = claim.get("note")

    if tier in ("container", "microvm"):
        # The probe answers for the host alone, so an image-backed tier reports
        # `runnable: null` whenever no profile names an image — including when
        # this run is about to supply one. `satisfiable` is the host's half of
        # the answer, and `--image` is ours.
        if not claim.get("satisfiable"):
            return note or f"host cannot provide `{tier}`"
        if not image:
            return "needs --image (image-backed tier; runs never pull)"
        return None

    if not claim.get("runnable", False):
        state = "satisfiable but not runnable" if claim.get("satisfiable") else
        "unsatisfiable" return note or f"host reports `{tier}` {state}"
    return None


def create_box(binary: str, name: str, tier: str, image: str | None) -> None:
    argv = [binary, "env", "create", name, "--isolation", tier, "--new"]
    if image:
        argv += ["--image", image]
    proc = run_out(argv)
    if proc.returncode != 0:
        raise SystemExit(
            f"could not create `{name}` at isolation={tier}:\n{proc.stderr.strip()}"
        )


def remove_box(binary: str, name: str) -> None:
    run_out([binary, "env", "rm", name, "--force"])


def measure_bare(argv: list[str]) -> tuple[float, float | None]:
    start = time.perf_counter_ns()
    proc = subprocess.run(argv, capture_output=True)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    if proc.returncode != 0:
        raise SystemExit(f"bare workload failed: {argv}\n{proc.stderr.decode()[:400]}")
    return elapsed, None


def guest_argv(argv: list[str]) -> list[str]:
    """The same workload, spelled for the box's filesystem rather than the host's.

    Only the no-op probe differs today: it is an absolute path, and the two
    filesystems do not agree on it. `python3` is left as a bare name so `PATH`
    resolves it wherever it lives.
    """
    if argv and argv[0].endswith("/true"):
        return [GUEST_TRUE, *argv[1:]]
    return argv


def measure_boxed(binary: str, box: str, argv: list[str]) -> tuple[float, float | None]:
    """One `box run`, timed from outside and read from h5i's own envelope."""
    full = [binary, "env", "run", "--json", box, "--", *guest_argv(argv)]
    start = time.perf_counter_ns()
    proc = subprocess.run(full, capture_output=True, text=True)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    if proc.returncode != 0:
        raise SystemExit(
            f"`box run` failed in `{box}` ({proc.returncode}):\n{proc.stderr.strip()[:800]}"
        )
    reported: float | None = None
    try:
        envelope = json.loads(proc.stdout)
        reported = float(envelope.get("wall_ms"))
        if envelope.get("exit_code") not in (0, None):
            raise SystemExit(
                f"workload exited {envelope['exit_code']} in `{box}`; "
                "the tier ran but the command did not succeed"
            )
    except (json.JSONDecodeError, TypeError, ValueError):
        # A tier that cannot emit the envelope still gives us the outer clock;
        # losing the inner one is worth recording, not worth aborting over.
        pass
    return elapsed, reported


def rotations(items: list[str], count: int) -> Iterable[list[str]]:
    for i in range(count):
        yield items[i % len(items) :] + items[: i % len(items)]


def host_facts(binary: str) -> dict:
    version = run_out([binary, "--version"]).stdout.strip()
    commit = run_out(["git", "rev-parse", "HEAD"]).stdout.strip() or None
    dirty = bool(run_out(["git", "status", "--porcelain"]).stdout.strip())
    return {
        "h5i": version,
        "commit": commit,
        "worktree_dirty": dirty,
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Measure per-command overhead of each h5i isolation tier.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--bin", default="target/release/h5i", help="h5i binary to
    measure") parser.add_argument(
        "--tiers",
        default="workspace,process",
        help=f"comma-separated tiers to measure (any of: {', '.join(ALL_TIERS)})",
    )
    parser.add_argument(
        "--image",
        default=None,
        help="pre-pulled OCI image for the image-backed tiers (container, microvm)",
    )
    parser.add_argument("--reps", type=int, default=5, help="measured repetitions (default
    5)") parser.add_argument("--prefix", default="bench", help="box name prefix (default `bench`)")
    parser.add_argument("--json", dest="json_out", default=None, help="write the artifact
    here") parser.add_argument("--keep", action="store_true", help="leave the benchmark boxes behind")
    args = parser.parse_args()

    requested = [t.strip() for t in args.tiers.split(",") if t.strip()]
    unknown = [t for t in requested if t not in ALL_TIERS]
    if unknown:
        raise SystemExit(f"unknown tier(s): {', '.join(unknown)}")

    caps = probe_capabilities(args.bin)
    skipped: dict[str, str] = {}
    runnable: list[str] = []
    for tier in requested:
        blocker = tier_blocker(caps, tier, args.image)
        if blocker:
            skipped[tier] = blocker
        else:
            runnable.append(tier)

    for tier, reason in skipped.items():
        print(f"skipping `{tier}`: {reason}", file=sys.stderr)
    if not runnable:
        print("no requested tier is runnable on this host; nothing measured", file=sys.stderr)

    boxes = {tier: f"{args.prefix}-{tier}" for tier in runnable}
    for tier, name in boxes.items():
        remove_box(args.bin, name)  # a leftover from a failed run would be
        reused create_box(args.bin, name, tier, args.image)

    results: dict[str, dict[str, Series]] = {}
    try:
        for workload, argv in WORKLOADS.items():
            series = {tier: Series() for tier in ["bare", *runnable]}

            # Warm-up: one of each, discarded. The first run of anything pays
            # for a cold page cache and, at the image tiers, a cold image.
            measure_bare(argv)
            for tier in runnable:
                measure_boxed(args.bin, boxes[tier], argv)

            for order in rotations(["bare", *runnable], args.reps):
                for tier in order:
                    if tier == "bare":
                        e2e, reported = measure_bare(argv)
                    else:
                        e2e, reported = measure_boxed(args.bin, boxes[tier], argv)
                    series[tier].e2e.append(e2e)
                    if reported is not None:
                        series[tier].reported.append(reported)
            results[workload] = series
    finally:
        if not args.keep:
            for name in boxes.values():
                remove_box(args.bin, name)

    facts = host_facts(args.bin)
    print()
    print(f"{facts['h5i']} · {facts['machine']} · {facts['platform']}")
    if facts["worktree_dirty"]:
        print("note: worktree is dirty; this measures uncommitted code")
    print()

    for workload, series in results.items():
        bare_median = statistics.median(series["bare"].e2e)
        print(f"## {workload}  ({' '.join(WORKLOADS[workload])})")
        print(f"{'tier':<12} {'e2e median':>12} {'vs bare':>12}
        {'reported':>12}") print(f"{'bare':<12} {bare_median:>11.1f}ms {'baseline':>12} {'—':>12}")
        for tier in results[workload]:
            if tier == "bare":
                continue
            cell = series[tier]
            median = statistics.median(cell.e2e)
            reported = (
                f"{statistics.median(cell.reported):.1f}ms" if cell.reported else "—"
            )
            print(
                f"{tier:<12} {median:>11.1f}ms {median - bare_median:>+10.1f}ms {reported:>12}"
            )
        print()

    if "noop" in results:
        print("Fixed cost per command (no-op median over bare) — setup and")
        print("teardown only, and the one cost a warm-guest design removes:")
        bare_noop = statistics.median(results["noop"]["bare"].e2e)
        for tier in results["noop"]:
            if tier == "bare":
                continue
            fixed = statistics.median(results["noop"][tier].e2e) - bare_noop
            print(f"  {tier:<12} {fixed:>8.1f}ms")
        print()
        if "short" in results:
            print("Enforcement cost on a syscall-heavy command (short minus
            noop,") print("over the same difference for bare) — what reuse does NOT remove:")
            bare_delta = statistics.median(results["short"]["bare"].e2e) -
            bare_noop for tier in results["short"]:
                if tier == "bare":
                    continue
                tier_delta = (
                    statistics.median(results["short"][tier].e2e)
                    - statistics.median(results["noop"][tier].e2e)
                )
                print(f"  {tier:<12} {tier_delta - bare_delta:>+8.1f}ms")
            print()

    artifact = {
        "host": facts,
        "workloads": {name: argv for name, argv in WORKLOADS.items()},
        "reps": args.reps,
        "image": args.image,
        "capabilities": caps,
        "skipped": skipped,
        "results": {
            workload: {tier: cell.as_dict() for tier, cell in series.items()}
            for workload, series in results.items()
        },
    }
    if args.json_out:
        with open(args.json_out, "w") as handle:
            json.dump(artifact, handle, indent=2)
        print(f"wrote {args.json_out}")

    return 1 if skipped else 0


if __name__ == "__main__":
    sys.exit(main())
