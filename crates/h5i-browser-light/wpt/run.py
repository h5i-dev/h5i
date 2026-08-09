#!/usr/bin/env python3
"""Run the Web Platform Tests against this engine and report what happened.

Usage:
    python3 wpt/run.py --dirs dom css/cssom --jobs 8
    python3 wpt/run.py --all --out wpt/results/baseline.json

Counting, and why it is done this way
-------------------------------------
A WPT file contains many `test()` calls, each one a *subtest*. The number
vendors quote is subtests, and that is what this reports as the headline.

The thing this instrument refuses to do is let an unmeasured file look like a
measured one. A file can end in six distinguishable ways and they are kept
apart, because §8.3 of the roadmap was written after an instrument that could
not tell "nothing is wrong" from "I cannot see":

  ok             the harness ran and reported. Its subtests are real data.
  harness_error  the harness ran, reported, and said the file itself errored.
  harness_timeout the harness ran, reported, and said it timed out internally.
  no_report      the engine exited cleanly and the harness never reported.
                 This is *unmeasured*, not zero passes.
  engine_timeout the engine did not exit. Unmeasured.
  engine_crash   the engine died. Unmeasured.

Only the first three contribute subtests. `no_report` is the interesting
bucket: it is where an engine gap stops a file before it can even say what it
failed. Chasing that bucket down is how the pass count goes up in steps rather
than in ones.

What this cannot reach, stated up front
---------------------------------------
WPT generates a large share of its endpoints at serve time: `x.any.js` becomes
`x.any.html`, `x.any.worker.html` and more, none of which exist on disk. A
static server cannot serve them, so they are outside this run entirely. The
summary prints how many such files were skipped so the denominator is never
mistaken for "all of WPT".
"""

import argparse
import concurrent.futures
import json
import os
import re
import subprocess
import sys

try:
    import resource
except ImportError:  # not POSIX
    resource = None
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import serve  # noqa: E402

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
REPO = CRATE.parent.parent
BINARY = REPO / "target" / "release" / "h5i-browser-light"

# Directories that hold machinery rather than tests. Running them produces
# noise that looks like failure and is not.
SKIP_DIRS = {
    "resources", "support", "tools", "common", "conformance-checkers",
    "docs", "interfaces", "fonts", "images", "media", "css/reference",
    "infrastructure", ".git", ".github", "webdriver", "wasm",
}

# Files that are not tests a harness can score: reference renderings for
# reftests, and tests that need a human.
SKIP_FILE = re.compile(r"(-ref|-notref|-manual|\.tentative\.tentative)\.x?html?$")

MARKER = serve.MARKER

# testharness.js status codes.
SUBTEST_STATUS = {0: "PASS", 1: "FAIL", 2: "TIMEOUT", 3: "NOTRUN", 4: "PRECONDITION_FAILED"}
HARNESS_STATUS = {0: "OK", 1: "ERROR", 2: "TIMEOUT", 3: "PRECONDITION_FAILED"}


def find_tests(root: Path, dirs, limit=None):
    """Every on-disk HTML test under `dirs`, plus counts of what was left out.

    Returns (tests, generated, unscoreable). A file that never loads
    testharness.js cannot report a result no matter how well the engine runs it
    — reftests compare renderings, crashtests only have to not crash — so
    counting them as engine failures would inflate the unmeasured bucket with
    files that were never ours to pass. They are counted and named instead.
    """
    tests, generated, unscoreable = [], 0, 0
    roots = [root / d for d in dirs] if dirs else [root]
    for base in roots:
        if not base.exists():
            print(f"warning: {base} does not exist, skipping", file=sys.stderr)
            continue
        for path in sorted(base.rglob("*")):
            rel = path.relative_to(root)
            parts = set(rel.parts[:-1])
            if parts & SKIP_DIRS or any(p.startswith(".") for p in rel.parts):
                continue
            name = path.name
            if name.endswith((".any.js", ".window.js", ".worker.js", ".sharedworker.js")):
                generated += 1
                continue
            if not name.endswith((".html", ".xht", ".xhtml")):
                continue
            if SKIP_FILE.search(name):
                continue
            try:
                body = path.read_text(encoding="utf8", errors="replace")
            except OSError:
                continue
            if "testharness.js" not in body:
                unscoreable += 1
                continue
            tests.append(str(rel))
    if limit:
        tests = tests[:limit]
    return tests, generated, unscoreable


def memory_cap(megabytes):
    """Limit a child's address space, or None where that cannot be done.

    A WPT file is allowed to be hostile — several exist precisely to allocate
    until something gives — and without this the kernel picks the victim, which
    on a 8 GiB development box has been the whole session rather than the test.
    A capped child dies alone and is recorded as one crash.
    """
    if resource is None:
        return None

    def apply():
        limit = megabytes * 1024 * 1024
        resource.setrlimit(resource.RLIMIT_AS, (limit, limit))

    return apply


def run_one(args):
    """Run one test file. Returns a dict that always names its own outcome."""
    rel, port, timeout, mem_mb = args
    url = f"http://127.0.0.1:{port}/{rel}"
    started = time.monotonic()
    try:
        proc = subprocess.run(
            [str(BINARY), "open", "--script", "--json", "--max-snapshot-lines", "1", url],
            capture_output=True, timeout=timeout,
            preexec_fn=memory_cap(mem_mb),
        )
    except subprocess.TimeoutExpired:
        return {"test": rel, "outcome": "engine_timeout", "elapsed": time.monotonic() - started}

    elapsed = time.monotonic() - started
    if proc.returncode != 0 and not proc.stdout:
        tail = proc.stderr.decode("utf8", "replace").strip().splitlines()
        return {
            "test": rel, "outcome": "engine_crash", "elapsed": elapsed,
            "detail": tail[-1] if tail else f"exit {proc.returncode}",
        }

    try:
        payload = json.loads(proc.stdout.decode("utf8", "replace"))
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        return {"test": rel, "outcome": "engine_crash", "elapsed": elapsed, "detail": str(exc)}

    unsupported = {u["api"]: u["calls"] for u in payload.get("unsupported", [])}

    report = None
    for line in payload.get("console", []):
        text = line.get("text", "")
        index = text.find(MARKER)
        if index != -1:
            try:
                report = json.loads(text[index + len(MARKER):])
            except json.JSONDecodeError:
                pass
            break

    if report is None:
        errors = [
            line.get("text", "")
            for line in payload.get("console", [])
            if line.get("level") == "error"
        ]
        return {
            "test": rel, "outcome": "no_report", "elapsed": elapsed,
            "unsupported": unsupported,
            "detail": errors[0][:300] if errors else "",
        }

    counts = {}
    failures = []
    for sub in report.get("tests", []):
        label = SUBTEST_STATUS.get(sub.get("status"), "UNKNOWN")
        counts[label] = counts.get(label, 0) + 1
        if label not in ("PASS",) and len(failures) < 5:
            failures.append({"name": sub.get("name", "")[:200],
                             "status": label,
                             "message": (sub.get("message") or "")[:300]})

    harness = HARNESS_STATUS.get(report.get("status"), "UNKNOWN")
    outcome = {"OK": "ok", "ERROR": "harness_error",
               "TIMEOUT": "harness_timeout"}.get(harness, "harness_error")
    return {
        "test": rel, "outcome": outcome, "elapsed": elapsed,
        "harness": harness, "subtests": counts, "failures": failures,
        "unsupported": unsupported,
        "detail": (report.get("message") or "")[:300],
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wpt", default=os.environ.get("WPT_ROOT", os.path.expanduser("~/Dev/wpt")))
    parser.add_argument("--dirs", nargs="*", default=["dom"])
    parser.add_argument("--all", action="store_true", help="every directory")
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--limit", type=int, default=None)
    parser.add_argument("--out", default=None)
    parser.add_argument("--mem-mb", type=int, default=1200,
                        help="address-space cap per test process")
    opts = parser.parse_args()

    if not BINARY.exists():
        sys.exit(f"no binary at {BINARY}; cargo build --release -p h5i-browser-light")

    root = Path(opts.wpt).expanduser()
    serve.WPT_ROOT = str(root)
    dirs = None if opts.all else opts.dirs
    tests, generated, unscoreable = find_tests(root, dirs, opts.limit)
    if not tests:
        sys.exit("no tests found")

    httpd, port = serve.start()
    print(f"{len(tests)} testharness files, {opts.jobs} jobs | skipped: "
          f"{generated} generated endpoints, {unscoreable} files that load no testharness",
          flush=True)

    results = []
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=opts.jobs) as pool:
        work = [(t, port, opts.timeout, opts.mem_mb) for t in tests]
        for i, result in enumerate(pool.map(run_one, work), 1):
            results.append(result)
            if i % 50 == 0 or i == len(tests):
                passed = sum(r.get("subtests", {}).get("PASS", 0) for r in results)
                rate = i / (time.monotonic() - started)
                print(f"  {i}/{len(tests)}  {passed} subtests passing  {rate:.1f} files/s",
                      flush=True)
    httpd.shutdown()

    summary = summarise(results, generated, unscoreable, time.monotonic() - started)
    report(summary, results)
    if opts.out:
        Path(opts.out).parent.mkdir(parents=True, exist_ok=True)
        Path(opts.out).write_text(json.dumps(
            {"summary": summary, "results": results}, indent=1))
        print(f"\nwrote {opts.out}")
    return 0


def summarise(results, generated, unscoreable, elapsed):
    outcomes, subtests, unsupported = {}, {}, {}
    for r in results:
        outcomes[r["outcome"]] = outcomes.get(r["outcome"], 0) + 1
        for label, n in r.get("subtests", {}).items():
            subtests[label] = subtests.get(label, 0) + n
        for api, n in r.get("unsupported", {}).items():
            unsupported[api] = unsupported.get(api, 0) + n
    measured = sum(outcomes.get(k, 0) for k in ("ok", "harness_error", "harness_timeout"))
    return {
        "files": len(results),
        "files_measured": measured,
        "files_unmeasured": len(results) - measured,
        "generated_endpoints_skipped": generated,
        "unscoreable_files_skipped": unscoreable,
        "outcomes": outcomes,
        "subtests": subtests,
        "subtests_total": sum(subtests.values()),
        "subtests_passing": subtests.get("PASS", 0),
        "unsupported": dict(sorted(unsupported.items(), key=lambda kv: -kv[1])),
        "elapsed_s": round(elapsed, 1),
    }


def report(summary, results):
    passing = summary["subtests_passing"]
    total = summary["subtests_total"]
    pct = (100.0 * passing / total) if total else 0.0
    print(f"\n{'=' * 62}\nsubtests passing: {passing} of {total} scored ({pct:.1f}%)")
    print(f"files: {summary['files']}  measured {summary['files_measured']}  "
          f"unmeasured {summary['files_unmeasured']}")
    print(f"\noutcomes:")
    for name, n in sorted(summary["outcomes"].items(), key=lambda kv: -kv[1]):
        print(f"  {n:6d}  {name}")
    if summary["subtests"]:
        print(f"\nsubtest results:")
        for name, n in sorted(summary["subtests"].items(), key=lambda kv: -kv[1]):
            print(f"  {n:6d}  {name}")

    missing = summary["unsupported"]
    if missing:
        print(f"\nAPIs the tests asked for and this engine does not have"
              f" ({len(missing)} distinct, top 25):")
        for api, n in list(missing.items())[:25]:
            print(f"  {n:6d}  {api}")

    stuck = [r for r in results if r["outcome"] == "no_report"]
    if stuck:
        print(f"\n{len(stuck)} files where the harness never reported. Top errors:")
        tally = {}
        for r in stuck:
            key = re.sub(r"\d+", "N", (r.get("detail") or "(silent)"))[:120]
            tally[key] = tally.get(key, 0) + 1
        for detail, n in sorted(tally.items(), key=lambda kv: -kv[1])[:15]:
            print(f"  {n:6d}  {detail}")
    print(f"\n{summary['elapsed_s']}s")


if __name__ == "__main__":
    sys.exit(main())
