"""Would compiling the prelude during the navigation's fetch pay for itself?

roadmap-history.md §B15.12b. The engine compiles the browser prelude while the
document it is navigating to is still on the wire, which is worth ~67 ms on the
first page a process loads. It has to decide *before* the document exists, so it
cannot ask the question that would settle it -- whether the page has any script
-- and a page with none builds no realm and would have paid the compile for nothing.

This forecasts the trade over the corpus, so `worth_warming` in `engine.rs` is
gated on a measurement rather than on a guess. Per page, with C the compile and
F the main-document fetch:

    scripted    saved = min(F, C)
    scriptless  lost  = max(0, C - F)

The realm's other costs cancel out of both arms, so only F, C and whether the
page is scripted matter.

    python3 perf/overlap.py                 # the whole corpus
    python3 perf/overlap.py --compile-ms 90 # what a bigger prelude would cost

Fetch time is measured with curl rather than through the engine's own broker:
the question is how long the *network* takes, and putting the engine in the loop
would add its policy and proxy path to a number meant to be about the wire. The
engine's window is the same or larger, so this is the conservative direction.

**Two traps, both of which caught this instrument once.**

Bot-challenge pages are not the page. They come back ~3 KB, they carry their own
script, and they arrive fast, so left in the sample they count as scripted *and*
land in the fast-fetch region that decides the whole question. Non-200 responses
are dropped rather than guessed at, and the browser-shaped headers below exist
to provoke fewer of them.

Everything here is remote. A loopback dev server answers in about a millisecond,
so a scriptless local page would pay the whole compile as added latency. That
case is not in this evidence, which is exactly why `worth_warming` excludes
loopback and the non-network schemes in code instead of assuming they behave
like the rest.
"""

import argparse
import ast
import concurrent.futures
import json
import pathlib
import re
import statistics
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
CRATE = HERE.parent

# Read the site lists out of the corpus rather than copying them, so this cannot
# drift from what the project actually measures. Statically, because importing
# `corpus/run.py` runs its argument parser.
CORPUS_PY = CRATE / "corpus" / "run.py"
WANTED = ("DOCUMENTS", "INTERNATIONAL", "STRUCTURES", "APPLICATIONS")

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
HEADERS = [
    "-H", "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,"
          "image/avif,image/webp,*/*;q=0.8",
    "-H", "Accept-Language: en-US,en;q=0.9",
    "-H", "Sec-Fetch-Dest: document",
    "-H", "Sec-Fetch-Mode: navigate",
    "-H", "Sec-Fetch-Site: none",
    "-H", "Upgrade-Insecure-Requests: 1",
]

# What `Page::run_scripts` actually decides on: any <script> that is not an
# import map -- a map declares imports that never happen and is not code -- or an
# inline event-handler attribute.
SCRIPT_TAG = re.compile(rb"<script\b([^>]*)>", re.I)
TYPE_ATTR = re.compile(rb"""type\s*=\s*["']?([^"'\s>]+)""", re.I)
INLINE_HANDLER = re.compile(rb"""<[^>]+\son[a-z]+\s*=\s*["']""", re.I)
NON_CODE_TYPES = {b"importmap", b"application/json", b"application/ld+json",
                  b"text/template", b"text/html"}


def corpora():
    found = {}
    for node in ast.parse(CORPUS_PY.read_text()).body:
        if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
            if name in WANTED:
                found[name.lower()] = ast.literal_eval(node.value)
    missing = [w for w in WANTED if w.lower() not in found]
    if missing:
        sys.exit(f"{CORPUS_PY} no longer defines {missing}")
    return found


def has_code(body):
    """Whether this document would make the engine build a realm."""
    for m in SCRIPT_TAG.finditer(body):
        t = TYPE_ATTR.search(m.group(1))
        if t is None or t.group(1).lower() not in NON_CODE_TYPES:
            return True
    return bool(INLINE_HANDLER.search(body))


def fetch(url, bodies):
    body_path = bodies / (re.sub(r"\W+", "_", url)[:80] + ".html")
    fmt = "%{time_total} %{size_download} %{http_code}"
    try:
        done = subprocess.run(
            ["curl", "-sL", "--compressed", "-A", UA, *HEADERS,
             "--max-time", "45", "-o", str(body_path), "-w", fmt, url],
            capture_output=True, text=True, timeout=60,
        )
    except subprocess.TimeoutExpired:
        return {"url": url, "error": "timeout"}
    if done.returncode != 0:
        return {"url": url, "error": f"curl exit {done.returncode}"}
    try:
        total, size, code = done.stdout.split()
    except ValueError:
        return {"url": url, "error": f"unparseable: {done.stdout[:60]}"}
    return {
        "url": url,
        "fetch_ms": float(total) * 1000.0,
        "bytes": int(size),
        "status": int(code),
        "scripted": has_code(body_path.read_bytes()),
    }


def usable(row):
    return "error" not in row and row.get("status") == 200 and row.get("bytes", 0) > 0


def delta(row, compile_ms):
    """Milliseconds saved (positive) or lost (negative) by speculating."""
    if row["scripted"]:
        return min(row["fetch_ms"], compile_ms)
    return -max(0.0, compile_ms - row["fetch_ms"])


def report(rows, compile_ms):
    ok = [r for r in rows if usable(r)]
    dropped = [r for r in rows if not usable(r)]
    if not ok:
        sys.exit("no usable pages; is there a network?")

    print(f"{len(ok)} pages measured, {len(dropped)} unusable")
    for r in dropped:
        print(f"   dropped: {r['url'][:62]} ({r.get('error') or 'HTTP ' + str(r.get('status'))})")

    scripted = [r for r in ok if r["scripted"]]
    plain = [r for r in ok if not r["scripted"]]
    print(f"\nscripted {len(scripted)}  scriptless {len(plain)} "
          f"({100 * len(scripted) / len(ok):.0f}% scripted)")
    for name, group in (("scripted", scripted), ("scriptless", plain)):
        if group:
            f = sorted(r["fetch_ms"] for r in group)
            print(f"  {name:<11} fetch ms  median {statistics.median(f):6.0f}"
                  f"  min {f[0]:6.0f}  max {f[-1]:7.0f}")

    # The scriptless minimum is the whole safety margin: below it, speculating
    # starts costing a page that was never going to use the realm.
    if plain:
        floor = min(r["fetch_ms"] for r in plain)
        print(f"\nthe compile could grow to {floor:.0f} ms before one page here "
              f"regressed (currently {compile_ms:.0f} ms)")

    print(f"\n{'compile':>8} {'net(ms)':>10} {'per page':>9} {'won':>5} {'lost':>5}
    {'worst':>8}") for c in sorted({40.0, 63.0, compile_ms, 82.0, 100.0}):
        ds = [delta(r, c) for r in ok]
        print(f"{c:>8.0f} {sum(ds):>10.0f} {sum(ds) / len(ds):>9.1f} "
              f"{sum(1 for d in ds if d > 0.5):>5} {sum(1 for d in ds if d < -0.5):>5}
              " f"{min(ds):>8.1f}")

    ds = [delta(r, compile_ms) for r in ok]
    verdict = ("speculating pays" if min(ds) > -0.5 and sum(ds) > 0
               else "speculating costs some pages; tighten the guard")
    print(f"\nat {compile_ms:.0f} ms: {verdict}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--compile-ms", type=float, default=67.0,
                        help="what the prelude costs to compile (perf example, §B8.9)")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--out", default=None, help="write the rows here as
    JSON") parser.add_argument("--bodies", default=None,
                        help="keep the fetched documents here (default: a temp dir)")
    opts = parser.parse_args()

    import tempfile
    keep = opts.bodies or tempfile.mkdtemp(prefix="h5i-overlap-")
    bodies = pathlib.Path(keep)
    bodies.mkdir(parents=True, exist_ok=True)

    rows = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=opts.jobs) as pool:
        futures = {}
        for name, urls in corpora().items():
            for url in urls:
                futures[pool.submit(fetch, url, bodies)] = name
        for fut in concurrent.futures.as_completed(futures):
            row = fut.result()
            row["corpus"] = futures[fut]
            rows.append(row)

    if opts.out:
        pathlib.Path(opts.out).write_text(json.dumps(rows, indent=1))
    report(rows, opts.compile_ms)


if __name__ == "__main__":
    main()
