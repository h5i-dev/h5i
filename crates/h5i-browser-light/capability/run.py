#!/usr/bin/env python3
"""Does the modern web work here, and how fast? Offline, deterministic.

ROADMAP §B19.4, item 12. The gap this fills is structural rather than a matter
of coverage:

  `tests/corpus.rs` holds hand-written reductions of things the *network*
  corpus found. By construction it can only contain regressions of bugs we have
  already had — it cannot answer "does Vue 3 mount", which is the question a
  user asks on day one.

  `corpus/run.py` can answer it, and needs the network, and takes minutes, and
  changes underneath itself as the sites change.

So: a small number of self-contained pages, with the frameworks vendored and
served from a local origin, each asserting something about the DOM that only a
working engine produces. Deterministic, offline, seconds.

**Nine stages, not thirty-three.** Obscura's obstacle course is a marketing
surface as well as a test and covers APIs this engine has decided not to have.
Ours tests what §B6 says we support, which makes it a companion to `wpt/
tiers.list` rather than a second corpus: the tiers file declares the scope and
this checks the scope actually works.

**Timing is reported, never gated.** Every stage is timed and the number is
printed, because §B15.12a's lesson was that three optimisations reasoned from
the shape of the code were all wrong and none of them had been measured. A
latency *gate* in CI would be a flake factory; a latency *report* is the
standing measurement that lesson asked for and never got.

    python3 capability/run.py
    python3 capability/run.py --filter react --runs 5
    python3 capability/run.py --json

Exits non-zero if any stage's assertion fails, so it doubles as a regression
gate on capability — which is the half that is safe to gate.
"""

import argparse
import http.server
import json
import re
import socketserver
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
sys.path.insert(0, str(CRATE))
import harness  # noqa: E402

MANIFEST = HERE / "manifest.json"


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    """A static server over `capability/`, silent.

    Silent because the runner's own output is the report: one access-log line
    per subresource would bury nine result lines under two hundred.
    """

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(HERE), **kwargs)

    def log_message(self, *args):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def start_server():
    httpd = Server(("127.0.0.1", 0), QuietHandler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, httpd.server_address[1]


def run_stage(binary, base, stage, runs, warmup):
    """Load one fixture `runs` times and check what came back.

    Loopback is reachable by default (it is the dev server), so no grant is
    needed and none is passed: the fixtures are entirely local, and a run that
    quietly enabled the instrument grant would not be able to tell a fixture
    that reaches the network from one that does not.
    """
    url = f"{base}fixtures/{stage['file']}"
    cmd = harness.instrument_argv(
        binary, "open", url, "--json", "--script",
        "--max-snapshot-lines", "400", grant=False,
    )

    timings, payload, failure = [], None, None
    for attempt in range(warmup + runs):
        started = time.monotonic()
        try:
            done = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        except subprocess.TimeoutExpired:
            return {"name": stage["name"], "ok": False,
                    "detail": "the engine did not finish within 60s", "ms": None}
        elapsed = (time.monotonic() - started) * 1000
        if attempt >= warmup:
            timings.append(elapsed)
        if done.returncode != 0:
            note = (done.stderr or "").strip().splitlines()
            return {"name": stage["name"], "ok": False,
                    "detail": (note[-1] if note else f"exit {done.returncode}")[:200],
                    "ms": None}
        try:
            payload = json.loads(done.stdout)
        except json.JSONDecodeError as exc:
            return {"name": stage["name"], "ok": False,
                    "detail": f"unreadable output: {exc}", "ms": None}

    ok, failure = check(payload, stage)
    return {
        "name": stage["name"],
        "ok": ok,
        "detail": failure or "",
        "ms": round(statistics.median(timings), 1) if timings else None,
        # Kept for a failure report: an engine error is a far more useful thing
        # to read than "expected 100, got 0".
        "console": [
            line.get("text", "")
            for line in (payload or {}).get("console", [])
            if line.get("level") == "error"
        ][:3],
    }


def check(payload, stage):
    """The assertion, over the page's own text after script has run.

    **Text, not a CSS selector**, and the first draft of this got it wrong in a
    way worth recording. It was written against a `selector` field, because
    that is how the session's `extract` verb works — and `open --json` has no
    selector engine on the far side. The code then quietly fell back to
    scanning text while the manifest still said `selector`, which is a harness
    lying about what it checks: exactly the defect §B19.5 found in the corpus,
    reproduced in the fixture that was meant to replace it.

    So the manifest says `match` and this matches. The fixtures are written to
    make that sufficient: a row renders as `item 0`, which is in the served
    HTML nowhere, so counting the lines that match is a claim about the tree
    the engine actually built.
    """
    text = (payload or {}).get("text")
    if text is None:
        return False, "the engine returned no page text to check"
    pattern = re.compile(stage["match"])
    matched = [line.strip() for line in text.splitlines() if pattern.search(line)]
    if len(matched) != stage["count"]:
        return False, (
            f"expected {stage['count']} line(s) matching {stage['match']!r}, "
            f"got {len(matched)}"
        )
    if stage.get("first") is not None and matched:
        if matched[0] != stage["first"]:
            return False, f"first match was {matched[0]!r}, expected {stage['first']!r}"
    return True, None


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--binary", default=None)
    parser.add_argument("--filter", help="only stages whose name contains this")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--json", action="store_true")
    opts = parser.parse_args()

    binary = harness.engine_binary(opts.binary)
    harness.check_engine(binary)

    stages = json.loads(MANIFEST.read_text())["stages"]
    if opts.filter:
        stages = [s for s in stages if opts.filter in s["name"]]
    if not stages:
        sys.exit("no stages matched")

    httpd, port = start_server()
    base = f"http://127.0.0.1:{port}/"
    try:
        results = [
            run_stage(binary, base, stage, opts.runs, opts.warmup) for stage in stages
        ]
    finally:
        httpd.shutdown()

    if opts.json:
        print(json.dumps({"engine": binary, "results": results}, indent=2))
    else:
        print(f"\nengine: {binary}\n")
        print(f"{'stage':<20}{'result':<10}{'median':>9}")
        print("-" * 42)
        for row in results:
            ms = f"{row['ms']:.0f} ms" if row["ms"] is not None else "—"
            print(f"{row['name']:<20}{'ok' if row['ok'] else 'FAIL':<10}{ms:>9}")
            if not row["ok"]:
                print(f"    {row['detail']}")
                for line in row.get("console", []):
                    print(f"    engine said: {line[:160]}")
        passed = sum(1 for r in results if r["ok"])
        print(f"\n{passed}/{len(results)} stages pass")
        # Timing is printed and never gated. See the module header.
        timed = [r["ms"] for r in results if r["ms"] is not None]
        if timed:
            print(f"median across stages: {statistics.median(timed):.0f} ms "
                  f"(reported, not gated)")

    return 0 if all(r["ok"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
