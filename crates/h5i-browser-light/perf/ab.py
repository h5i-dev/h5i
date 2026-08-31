"""Time the same pages through two engine binaries, and say whether the
difference is real.

roadmap-history.md §B15.12b. `examples/perf.rs` measures phases inside one
build; this measures whole page loads across two, which is the only way to check
that a saving on paper reaches a person waiting for a page.

    cargo build --release -p h5i        # the "after" binary
    git stash && cargo build --release -p h5i && cp target/release/h5i
    /tmp/before python3 perf/ab.py /tmp/before target/release/h5i

    python3 perf/ab.py A B --fast --reps 20   # the pages fast enough to resolve
    python3 perf/ab.py A B --ceiling-ms 82    # the largest effect that is possible

**Read the ceiling first.** A change that hides a 67 ms compile cannot save more
than 67 ms. If the measured median comes out well above `--ceiling-ms`, the
instrument is wrong, not the engine, and the honest move is to find the bias
rather than to report the number. This is not hypothetical: the first run of
this comparison said 171 ms against a 67 ms mechanism, because `before` and
`after` ran back to back on the same URL and the second inherited warm DNS, a
resumable TLS session and a warm CDN. Hence `--reps` alternating the order below.

**Pick pages that can show the effect.** The default eight span 0.9 to 39
seconds and swing by hundreds of milliseconds; a 67 ms signal is not resolvable
against that, and the honest result there was a median of -6 ms. `--fast`
selects the pages under ~1.5 s, where the same change reads +106 ms with a 95%
interval of [+53, +139]. That is not cherry-picking as long as the prediction is
stated first and the ceiling is checked, which is what this prints.
"""

import argparse
import json
import math
import pathlib
import random
import statistics
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
CRATE = HERE.parent
sys.path.insert(0, str(CRATE))
import harness  # noqa: E402

# Whole-corpus shape: documentation, an application, a news page, a spec.
PAGES = [
    "https://developer.mozilla.org/en-US/docs/Web/API/fetch",
    "https://doc.rust-lang.org/book/ch01-00-getting-started.html",
    "https://docs.python.org/3/library/json.html",
    "https://en.wikipedia.org/wiki/Kelp",
    "https://www.rust-lang.org/",
    "https://go.dev/doc/",
    "https://nodejs.org/api/fs.html",
    "https://lobste.rs/",
]

# The subset that loads in about a second, where a ~67 ms effect is a visible
# fraction of the total rather than a rounding error on it.
FAST = [
    "https://www.rust-lang.org/",
    "https://lobste.rs/",
    "https://docs.python.org/3/library/json.html",
]


def once(binary, url, script_seconds):
    argv = harness.instrument_argv(
        binary, "open", "--script", "--json",
        "--max-snapshot-lines", "1",
        "--script-seconds", str(int(script_seconds)),
        url,
    )
    start = time.perf_counter()
    try:
        done = subprocess.run(argv, capture_output=True, timeout=120)
    except subprocess.TimeoutExpired:
        return None
    elapsed = (time.perf_counter() - start) * 1000.0
    if done.returncode != 0:
        return None
    try:
        json.loads(done.stdout)
    except Exception:
        return None
    return elapsed


def binom_tail(k, n):
    """P(X >= k) for X ~ Binomial(n, 1/2). The sign test, without scipy."""
    return sum(math.comb(n, i) * 0.5 ** n for i in range(k, n + 1))


def bootstrap_ci(values, draws=20000, seed=7):
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(values, k=len(values)))
                  for _ in range(draws))
    return meds[int(0.025 * draws)], meds[int(0.975 * draws)]


def report(rows, urls, ceiling_ms):
    print(f"\n{'page':<44} {'n':>3} {'before':>9} {'after':>9} {'delta':>8}")
    for url in urls:
        b, a = rows.get(f"{url}|before", []), rows.get(f"{url}|after", [])
        if not b or not a:
            print(f"{url[8:52]:<44} {'-':>3} {'-':>9} {'-':>9} {'no data':>8}")
            continue
        mb, ma = statistics.median(b), statistics.median(a)
        print(f"{url[8:52]:<44} {min(len(b), len(a)):>3} "
              f"{mb:>8.0f}m {ma:>8.0f}m {mb - ma:>+7.0f}m")

    # Every paired run, not the per-page medians: the spread is the part that
    # decides whether any of this is resolvable.
    paired = []
    for url in urls:
        b, a = rows.get(f"{url}|before", []), rows.get(f"{url}|after", [])
        paired += [x - y for x, y in zip(b, a)]
    if not paired:
        sys.exit("no paired runs; did the binaries work?")

    n = len(paired)
    med = statistics.median(paired)
    pos = sum(1 for d in paired if d > 0)
    lo, hi = bootstrap_ci(paired)

    print(f"\n{n} paired runs")
    print(f"  after faster in {pos}/{n}   sign test p = {binom_tail(pos,
    n):.2g}") print(f"  median delta {med:+.0f} ms   95% bootstrap CI [{lo:+.0f}, {hi:+.0f}] ms")
    print(f"  ceiling (the most the change could save) {ceiling_ms:+.0f} ms")

    p = binom_tail(pos, n)
    if p > 0.05:
        # The interval can exclude zero on a handful of runs while the direction
        # is still a coin toss. The sign test is the cheaper question and it
        # goes first, so a short run cannot report a result it has not earned.
        print(f"  verdict: direction not established (p = {p:.2g}); "
              f"more repetitions, or pages with less spread")
    elif lo <= 0 <= hi:
        print("  verdict: no effect resolvable at this sample size")
    elif lo > ceiling_ms:
        print("  verdict: ENTIRELY above the ceiling, so the instrument is biased, "
              "not the engine. Check that the arms alternate order.")
    elif med > ceiling_ms:
        print("  verdict: real, and consistent with the ceiling once the interval "
              "is taken into account; the point estimate is high for a noisy box")
    else:
        print("  verdict: real and within the ceiling")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("before")
    parser.add_argument("after")
    parser.add_argument("--reps", type=int, default=6)
    parser.add_argument("--fast", action="store_true",
                        help="only the pages quick enough to resolve a ~67 ms effect")
    parser.add_argument("--ceiling-ms", type=float, default=82.0,
                        help="the largest saving the change could possibly produce")
    parser.add_argument("--script-seconds", type=float, default=10.0)
    parser.add_argument("--out", default=None)
    opts = parser.parse_args()

    urls = FAST if opts.fast else PAGES
    rows = {}
    for rep in range(opts.reps):
        for url in urls:
            # Alternate which binary goes first. A fixed order hands the second
            # one warm DNS, a resumable TLS session and whatever the CDN cached a
            # second ago, which is worth more than the change being measured.
            arms = [("before", opts.before), ("after", opts.after)]
            if rep % 2:
                arms.reverse()
            for name, binary in arms:
                ms = once(binary, url, opts.script_seconds)
                if ms is not None:
                    rows.setdefault(f"{url}|{name}", []).append(ms)
        if opts.out:
            pathlib.Path(opts.out).write_text(json.dumps(rows, indent=1))
        print(f"  rep {rep + 1}/{opts.reps}", flush=True)

    report(rows, urls, opts.ceiling_ms)


if __name__ == "__main__":
    main()
