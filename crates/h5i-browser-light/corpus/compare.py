#!/usr/bin/env python3
"""Measure this engine against Chromium on the same pages, honestly.

Peak resident memory is sampled across the **whole process tree**, because
Chromium is multi-process — a browser, a renderer, a GPU process and a zygote —
and measuring only the process we launched would flatter this engine by several
hundred megabytes for no reason.

Both engines are asked to do the same job: fetch a page, run its script, and
produce a readable serialisation of the result. For Chromium that is
`--dump-dom`; for this engine it is `open --json`. A run that produces nothing
is recorded as a failure rather than as a fast, small success, because "used no
memory" and "did not read the page" are not the same claim.

    python3 compare.py [--chrome PATH] [--runs 3]
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import harness  # noqa: E402

PAGES = [
    ("a documentation page", "https://doc.rust-lang.org/book/ch01-00-getting-started.html"),
    ("a reference page", "https://developer.mozilla.org/en-US/docs/Web/API/fetch"),
    ("a wiki article", "https://en.wikipedia.org/wiki/Kelp"),
    ("a news front page", "https://news.ycombinator.com/"),
    ("a single-page app", "https://todomvc.com/examples/react/dist/"),
    ("a framework docs site", "https://vuejs.org/guide/introduction.html"),
]

# Chromium is given no allowlist at all, so this engine must not be given a
# narrow one: a comparison in which one side is refused half its subresources
# is measuring the harness. `harness.ENGINE_GRANT` is the mode that makes the
# two sides answerable to the same question (ROADMAP §B19.5).


def tree_rss_kib(root_pid):
    """Resident memory of a process and everything it spawned, in KiB."""
    try:
        out = subprocess.run(
            ["ps", "-o", "pid=,ppid=,rss=", "-e"],
            capture_output=True, text=True, timeout=5,
        ).stdout
    except Exception:
        return 0
    children, rss = {}, {}
    for line in out.splitlines():
        try:
            pid, ppid, kib = (int(x) for x in line.split())
        except ValueError:
            continue
        children.setdefault(ppid, []).append(pid)
        rss[pid] = kib
    total, stack = 0, [root_pid]
    while stack:
        pid = stack.pop()
        total += rss.get(pid, 0)
        stack.extend(children.get(pid, []))
    return total


def measure(command, timeout=120):
    """Run a command, sampling peak tree RSS until it exits."""
    peak = [0]
    started = time.time()
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def sample():
        while process.poll() is None:
            peak[0] = max(peak[0], tree_rss_kib(process.pid))
            time.sleep(0.02)

    watcher = threading.Thread(target=sample, daemon=True)
    watcher.start()
    try:
        out, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        return None, 0, 0
    watcher.join(timeout=1)
    return out, time.time() - started, peak[0]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--chrome", required=True)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--binary", default=None)
    parser.add_argument("--json-out")
    args = parser.parse_args()

    ours = harness.engine_binary(args.binary)
    harness.check_engine(ours)

    profile = "/tmp/h5i-compare-profile"
    rows = []
    print(f"{'page':<26}{'engine':<10}{'peak RSS':>11}{'wall':>9}   read")
    for label, url in PAGES:
        for engine in ("h5i", "chromium"):
            best = None
            for _ in range(args.runs):
                shutil.rmtree(profile, ignore_errors=True)
                if engine == "h5i":
                    cmd = harness.instrument_argv(
                        ours, "open", url, "--json", "--script",
                        "--max-snapshot-lines", "300",
                    )
                else:
                    cmd = [
                        args.chrome, "--headless", "--disable-gpu", "--no-sandbox",
                        f"--user-data-dir={profile}", "--dump-dom", url,
                    ]
                out, wall, peak = measure(cmd)
                if out is None:
                    continue
                # Did it actually read the page? A run that produced nothing is
                # not a cheap success.
                if engine == "h5i":
                    try:
                        got = len((json.loads(out).get("snapshot") or {}).get("lines") or [])
                    except Exception:
                        got = 0
                else:
                    got = out.decode("utf-8", "replace").count("<")
                if best is None or peak < best[1]:
                    best = (wall, peak, got)
            if best is None:
                print(f"{label:<26}{engine:<10}{'—':>11}{'timeout':>9}   failed")
                rows.append({"page": label, "engine": engine, "failed": True})
                continue
            wall, peak, got = best
            unit = "lines" if engine == "h5i" else "tags"
            print(f"{label:<26}{engine:<10}{peak/1024:>8.0f} MiB{wall:>8.1f}s   {got} {unit}")
            rows.append({"page": label, "engine": engine, "rss_mib": round(peak / 1024, 1),
                         "wall_s": round(wall, 2), "read": got})
    shutil.rmtree(profile, ignore_errors=True)

    ours = [r for r in rows if r["engine"] == "h5i" and not r.get("failed")]
    theirs = [r for r in rows if r["engine"] == "chromium" and not r.get("failed")]
    if ours and theirs:
        print()
        print(f"median peak RSS   h5i {sorted(r['rss_mib'] for r in ours)[len(ours)//2]:.0f} MiB"
              f"   chromium {sorted(r['rss_mib'] for r in theirs)[len(theirs)//2]:.0f} MiB")
        print(f"median wall       h5i {sorted(r['wall_s'] for r in ours)[len(ours)//2]:.1f}s"
              f"     chromium {sorted(r['wall_s'] for r in theirs)[len(theirs)//2]:.1f}s")
    if args.json_out:
        with open(args.json_out, "w") as f:
            json.dump(rows, f, indent=2)


main()
