#!/usr/bin/env python3
"""Does the engine crash, panic, or hang on a large corpus of real pages?

roadmap-history.md §B19.4, item 7. This is the one instrument in that section
with no substitute here, and the reason is a matter of scale rather than of kind:

  * `tests/corpus.rs` holds hand-written reductions of things the network
    corpus already found, so by construction it can only contain regressions of
    bugs we have had. It cannot find a new one.
  * `corpus/run.py` reads thirty-five pages and reports what they *asked for*.
    It does not sweep, it does not classify, and a panic in it looks like a
    page that failed.
  * `wpt/` measures conformance against a specification. A page can be
    perfectly conformant and still be the page that wedges a worker.

A crash on the nine-hundredth real page is not reachable by any of those. It is
reachable by running nine hundred real pages and sorting the outcomes into
classes that are named, which is §B8's own doctrine — an instrument that cannot
name what is missing is fixed before anything it failed to name — pointed at
*crashing* rather than at missing features.

Two phases:

  discover  one-level crawl from the seed list, pooling links, to build a
            corpus that is diverse rather than curated. The seeds are pages we
            chose; their links are pages we did not, which is the point.
  sweep     run every URL and classify the outcome from the exit status,
            stderr, and what came back.

The classes, and why each exists as its own bucket rather than as "failed":

  CRASH      killed by a signal. A non-recoverable engine bug, and the only
             outcome here that is unambiguously ours.
  PANIC      a Rust panic reached stderr. An engine bug even when caught: the
             two `catch_unwind` sites degrade a panic to an error return, and a
             degraded panic is still a panic that should not have happened.
  HANG       the harness deadline fired. The engine has a navigation budget and
             a script-phase budget; both firing is normal, neither firing while
             the process sits there is not.
  HANG_HARD  even SIGTERM did not end it. The worst case, and distinguished
             from HANG because they have different causes: one is a budget that
             was too generous, the other is a process that stopped listening.
  REFUSED    the *main document* was denied by policy. **This class is ours and
             is not in Obscura's sweep**, because for them a refusal is a bug
             and for us it is the engine working. Counting it as a failure
             would make a run under a narrow allowlist look like an unstable
             engine, which is the exact confusion §B19.5 found in the corpus.
  THIN       exit 0, page reachable, almost nothing read. Usually an anti-bot
             wall or a client-rendered page we could not settle; a finding
             worth having and not an engine bug.
  OK         read real content.

The bug classes are CRASH, PANIC, HANG and HANG_HARD. Everything else is
information about the web.

    python3 reliability/sweep.py --max-urls 300
    python3 reliability/sweep.py --seeds my-urls.txt --concurrency 4 --json-out r.json

A discovery pass is cached in `reliability/corpus.txt` so a re-run sweeps the
same corpus. Delete it, or pass `--rediscover`, to build a new one: a sweep
whose corpus changes underneath it cannot be compared with the previous sweep,
which is most of what a reliability number is for.
"""

import argparse
import collections
import json
import re
import subprocess
import sys
import threading
import time
import urllib.parse
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
sys.path.insert(0, str(CRATE))
import harness  # noqa: E402

CORPUS_CACHE = HERE / "corpus.txt"

# The seeds are the corpus lists this crate already maintains, read from the
# harness that owns them rather than copied. Two lists that have to agree about
# what a representative page is are two lists that stop agreeing.
def seed_urls():
    # `corpus/run.py` calls `main()` at import time, so it cannot be imported
    # for its constants. They are read out of the source instead: ugly, and
    # better than a second copy of the seed list that drifts out of step with
    # the one the corpus actually uses.
    source = (CRATE / "corpus" / "run.py").read_text()
    urls = []
    for name in ("DOCUMENTS", "APPLICATIONS", "INTERNATIONAL", "STRUCTURES"):
        match = re.search(rf"^{name} = \[(.*?)^\]", source, re.S | re.M)
        if not match:
            continue
        urls += re.findall(r'"(https?://[^"]+)"', match.group(1))
    if not urls:
        sys.exit(
            "no seeds found in corpus/run.py — its lists were renamed. "
            "Pass --seeds <file> instead."
        )
    return urls


# Per-URL wall-clock ceilings. The engine has its own budgets (a navigation
# deadline and a script-phase budget), so the first of these should never fire
# on a healthy engine — which is exactly what makes it a signal.
SOFT_DEADLINE = 60

PANIC = re.compile(r"panicked at|RUST_BACKTRACE|internal error: entered unreachable")
# A refusal names the allowlist, and the engine writes it the same way every
# time (`policy.rs`), so this matches a message rather than guessing at prose.
REFUSAL = re.compile(r"denied by policy|is not in the allowlist")

# Below this many outline lines a page has not really been read.
THIN_LINES = 3


def classify(done, timed_out, killed_hard):
    """Sort one outcome into a named class. Order matters: the bug classes are
    tested before the informational ones, so a crash that also produced a thin
    page is reported as a crash."""
    if killed_hard:
        return "HANG_HARD", "SIGTERM did not end it"
    if timed_out:
        return "HANG", f"no exit within {SOFT_DEADLINE}s"

    stderr = done.stderr or ""
    if done.returncode is not None and done.returncode < 0:
        return "CRASH", f"signal {-done.returncode}"
    if PANIC.search(stderr):
        line = next((l for l in stderr.splitlines() if "panicked at" in l), "")
        return "PANIC", line.strip()[:200]

    if done.returncode != 0:
        if REFUSAL.search(stderr):
            return "REFUSED", "the main document was denied by policy"
        note = next(
            (l for l in stderr.strip().splitlines() if l.strip() and "ICU4X" not
            in l), "",
        )
        return "THIN", note.strip()[:200] or f"exit {done.returncode}"

    try:
        payload = json.loads(done.stdout)
    except Exception as e:
        # Exit 0 and unreadable output is an engine bug of its own kind: it
        # promised JSON and did not produce it.
        return "PANIC", f"exit 0 but unparseable output: {e}"

    lines = len((payload.get("snapshot") or {}).get("lines") or [])
    if lines < THIN_LINES:
        return "THIN", f"{lines} outline line(s)"
    return "OK", f"{lines} outline line(s)"


def run_one(binary, url, script=True):
    cmd = harness.instrument_argv(
        binary, "open", url, "--json", "--max-snapshot-lines", "200"
    )
    if script:
        cmd.append("--script")
    started = time.time()
    timed_out = killed_hard = False
    try:
        done = subprocess.run(
            cmd, capture_output=True, text=True, timeout=SOFT_DEADLINE
        )
    except subprocess.TimeoutExpired as e:
        timed_out = True
        # Distinguish a process that overran its budget from one that stopped
        # responding at all. They have different causes and only the second is
        # the worst case.
        killed_hard = _outlived_sigterm(cmd)
        done = subprocess.CompletedProcess(
            cmd, None, stdout=e.stdout or "", stderr=(e.stderr or "")
        )
    elapsed = time.time() - started
    kind, detail = classify(done, timed_out, killed_hard)
    return {"url": url, "class": kind, "detail": detail, "seconds": round(elapsed, 1)}


def _outlived_sigterm(cmd):
    """Whether a second run of the same URL also refuses to die politely.

    `subprocess.run`'s timeout kills the child for us, so by the time the
    exception arrives the distinction has been lost. Re-running once with an
    explicit SIGTERM is how the two are told apart, and it happens only for the
    small number of URLs that hung at all.
    """
    try:
        child = subprocess.Popen(
            cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
    except Exception:
        return False
    try:
        child.wait(timeout=SOFT_DEADLINE)
        return False
    except subprocess.TimeoutExpired:
        child.terminate()
        try:
            child.wait(timeout=10)
            return False
        except subprocess.TimeoutExpired:
            child.kill()
            return True


def discover(binary, seeds, max_urls, per_seed, concurrency):
    """One level out from the seeds, pooling the links each page carries.

    The seeds are pages somebody chose; their links are pages nobody did, and
    that is the whole reason to crawl rather than to curate. A page that breaks
    the engine is not likely to be one we would have thought to add.
    """
    pool, seen = list(seeds), set(seeds)
    lock = threading.Lock()
    work = collections.deque(seeds)

    def worker():
        while True:
            with lock:
                if not work or len(pool) >= max_urls:
                    return
                url = work.popleft()
            for link in links_of(binary, url)[:per_seed]:
                with lock:
                    if len(pool) >= max_urls:
                        return
                    if link not in seen:
                        seen.add(link)
                        pool.append(link)

    threads = [threading.Thread(target=worker) for _ in range(concurrency)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return pool[:max_urls]


def links_of(binary, url):
    """The http(s) links on a page, from the snapshot the engine already makes.

    Deliberately not a second HTML parser: the crawl and the sweep must agree
    about what a page is, and the engine is the definition of that here.
    """
    cmd = harness.instrument_argv(binary, "open", url, "--json", "--script")
    try:
        done = subprocess.run(cmd, capture_output=True, text=True,
        timeout=SOFT_DEADLINE) payload = json.loads(done.stdout)
    except Exception:
        return []
    out, seen = [], set()
    for ref in (payload.get("snapshot") or {}).get("refs") or []:
        href = ref.get("href") or ""
        if not href.startswith(("http://", "https://")):
            continue
        # Drop the fragment: twenty anchors into one page is one page.
        href = urllib.parse.urldefrag(href)[0]
        if href not in seen:
            seen.add(href)
            out.append(href)
    return out


def sweep(binary, urls, concurrency):
    results, lock = [], threading.Lock()
    work = collections.deque(urls)
    total = len(urls)

    def worker():
        while True:
            with lock:
                if not work:
                    return
                url = work.popleft()
            row = run_one(binary, url)
            with lock:
                results.append(row)
                done = len(results)
                # Bug classes are printed as they happen. Waiting for the
                # summary to learn the engine crashed forty minutes ago is the
                # wrong shape for a sweep this long.
                if row["class"] in ("CRASH", "PANIC", "HANG", "HANG_HARD"):
                    print(
                        f"  [{done}/{total}] {row['class']:<10} {row['url']}\n"
                        f"               {row['detail']}",
                        flush=True,
                    )
                elif done % 25 == 0:
                    print(f"  [{done}/{total}] ...", flush=True)

    threads = [threading.Thread(target=worker) for _ in range(concurrency)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return results


def report(results):
    counts = collections.Counter(r["class"] for r in results)
    total = len(results)
    print(f"\n=== {total} URLs ===\n")
    # Fixed order, so two runs are comparable line by line.
    for kind in ("OK", "THIN", "REFUSED", "HANG", "HANG_HARD", "PANIC", "CRASH"):
        n = counts.get(kind, 0)
        share = f"{100 * n / total:.1f}%" if total else "—"
        print(f"  {kind:<10} {n:>5}  {share:>6}")

    bugs = [
        r for r in results if r["class"] in ("CRASH", "PANIC", "HANG", "HANG_HARD")
    ]
    print(f"\n  engine bugs: {len(bugs)}")
    for row in bugs:
        print(f"    {row['class']:<10} {row['url']}\n               {row['detail']}")

    if counts.get("REFUSED"):
        # Said explicitly, because a REFUSED count above zero in a run that
        # passed the instrument grant means something is wrong with the *run*,
        # not with the engine.
        print(
            f"\n  note: {counts['REFUSED']} main document(s) were denied by policy.
            " f"With {harness.ENGINE_GRANT} that should be zero — check the grant "
            "reached the engine."
        )
    return 1 if bugs else 0


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--binary", default=None)
    parser.add_argument("--seeds", help="a file of seed URLs, one per line")
    parser.add_argument("--max-urls", type=int, default=300)
    parser.add_argument("--per-seed", type=int, default=12)
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--rediscover", action="store_true")
    parser.add_argument("--json-out")
    args = parser.parse_args()

    binary = harness.engine_binary(args.binary)
    harness.check_engine(binary)
    print(f"engine: {binary} (grant: {harness.ENGINE_GRANT})")

    if args.seeds:
        seeds = [
            l.strip()
            for l in Path(args.seeds).read_text().splitlines()
            if l.strip() and not l.startswith("#")
        ]
    else:
        seeds = seed_urls()

    if CORPUS_CACHE.exists() and not args.rediscover:
        urls = [
            l.strip() for l in CORPUS_CACHE.read_text().splitlines() if l.strip()
        ][: args.max_urls]
        print(f"corpus: {len(urls)} URLs from {CORPUS_CACHE.name} (--rediscover to rebuild)")
    else:
        print(f"discovering from {len(seeds)} seed(s)...", flush=True)
        urls = discover(
            binary, seeds, args.max_urls, args.per_seed, args.concurrency
        )
        CORPUS_CACHE.write_text("\n".join(urls) + "\n")
        print(f"corpus: {len(urls)} URLs, cached in {CORPUS_CACHE.name}")

    results = sweep(binary, urls, args.concurrency)
    status = report(results)

    if args.json_out:
        Path(args.json_out).write_text(
            json.dumps(
                {
                    "engine": binary,
                    "grant": harness.ENGINE_GRANT,
                    "urls": len(results),
                    "counts": dict(collections.Counter(r["class"] for r in
                    results)), "results": results,
                },
                indent=2,
            )
        )
    sys.exit(status)


if __name__ == "__main__":
    main()
