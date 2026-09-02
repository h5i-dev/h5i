#!/usr/bin/env python3
"""Measure what an agent's *second* step costs, and every step after it.

The engine comparison in `docs/design/design-browser.md` measures a cold read: launch,
load a page, snapshot once, exit. That is the right shape for "how heavy is
this browser", and it is the wrong shape for the thing agents actually do,
which is open a page once and then read it, act on it, and read it again.
Chromium's cost there is dominated by process launch, which is paid once; h5i's
advantage on that table is mostly the same fact from the other side. Neither
number says anything about the tenth step.

This measures the tenth step.

Method, and why each part is here:

- **The session stays resident for both engines.** `h5i browser open` leaves a
  session holding the page, and the verbs that follow act on it. Playwright
  holds a connection to a running Chromium. Measuring either engine's startup
  again per step would be measuring the cold read a second time.
- **Four lanes, not one.** `snapshot` reads the whole outline; `snapshot
  --delta` reads only what changed; `click` acts; and `status` pays the same
  process launch and session lookup while asking the engine to read nothing.
  The last one is the floor. Subtract it and what is left is an upper bound on
  the engine's own work, which is the only part a change to the reading path
  can move. An upper bound rather than the figure itself, because `status`
  does not prove it takes the identical path into the session that a read
  does.
- **A page that actually changes.** The fixture's button appends a list item,
  so a `--delta` after a click reports one added line rather than nothing. A
  loop over an inert page measures the unchanged path only, which is the
  cheapest one and would flatter every engine.
- **Order rotation and warm-up.** Each repetition discards a warm-up step and
  runs the lanes in a rotated order, so a cold page cache or a thermal ramp
  cannot land entirely on whichever lane went first.
- **Refusals are results.** An engine this host cannot run is recorded with the
  reason, not dropped. A benchmark that silently omits what it could not
  measure reads as a complete sweep.
- **Memory is sampled, not asked for once.** Peak RSS is summed across the
  resident session's process tree every 5 ms, the same way the cold-read table
  does it, because `/usr/bin/time -v` reports the largest single process and
  undercounts a multi-process browser badly.

What this deliberately does not claim: that the two engines do the same work
per step. They do not, and the difference is the finding rather than a caveat.
h5i answers `--delta` with what changed, so its per-step cost tracks the size
of the change. CDP has no delta, so an agent re-reads the whole accessibility
tree every step and pays for the page. The `snapshot` lane is the like-for-like
comparison; the `--delta` lane is the one h5i exists to win, and it is only
meaningful *because* the two are reported separately.

The other asymmetry, stated because it flatters nobody: h5i spends a process
launch per verb by design (one process per command, see `ask` in `stream.rs`),
while a Playwright client holds its connection open. The `status` floor is
reported so that cost is visible and subtractable rather than smuggled into the
engine's number.

Usage:

    scripts/bench_agent_loop.py --bin target/release/h5i
    scripts/bench_agent_loop.py --steps 20 --repeat 5 --json loop.json
    scripts/bench_agent_loop.py --engines h5i        # skip Chromium entirely

Exit status is 0 when every requested engine was measured, and 1 when one was
asked for by name and could not run, so CI can tell "not measured" from
"measured and fine".
"""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

# Two pages, because one cannot answer the question.
#
# A step's cost is a fixed part (launch, IPC, logging, the session lock) plus a
# part that tracks how much page there is to read. Measured on a small page
# alone, any change to the reading path is invisible and one would conclude it
# does not matter; measured on a large page alone, the fixed part hides. The
# pair is what says which half a step actually spends its time in, and so
# whether making the read faster is worth anything at this level at all.
SMALL_FIXTURE = """<html><head><title>Agent loop fixture</title></head><body>
<h1>Task list</h1>
<p>A page an agent reads, acts on, and reads again.</p>
<ul id="items"><li>first item</li><li>second item</li></ul>
<button id="add">Add an item</button>
<script>
  var n = 0;
  document.getElementById('add').addEventListener('click', function () {
    var li = document.createElement('li');
    li.textContent = 'added ' + (++n);
    document.getElementById('items').appendChild(li);
  });
</script>
</body></html>
"""


def large_fixture(sections: int = 120) -> str:
    """The same loop, on a page that fills the snapshot line budget.

    Built to the same shape as the fixture in `benches/read.rs`, so a reader
    comparing the two is comparing the same page.
    """
    body = "".join(
        f"<section><h2>Section {n}</h2>"
        f"<p>Paragraph one of section {n}, with <a href='/link/{n}'>a link</a> inside it.</p>"
        f"<p>Paragraph two of section {n}, longer, so there is real text to collapse.</p>"
        f"<ul><li>first item</li><li>second item</li><li>third item</li></ul>"
        f"</section>"
        for n in range(sections)
    )
    # The button comes before the sections deliberately. This page is longer
    # than the snapshot line budget, so a control at the end is truncated away
    # and never gets a ref; an agent could not click it, and neither can this.
    return (
        "<html><head><title>Agent loop fixture</title></head><body>"
        "<h1>Task list</h1>"
        '<button id="add">Add an item</button>'
        f'<ul id="items"><li>first item</li></ul>{body}'
        "<script>var n = 0;"
        "document.getElementById('add').addEventListener('click', function () {"
        "var li = document.createElement('li');"
        "li.textContent = 'added ' + (++n);"
        "document.getElementById('items').appendChild(li); });"
        "</script></body></html>"
    )


# How often the memory sampler looks, in seconds. Matches the cold-read table.
RSS_INTERVAL = 0.005


class Refused(Exception):
    """An engine could not be measured here, with the reason why."""


# ----------------------------------------------------------------- memory


def _rss_kib(pid: int) -> int:
    """Resident set of one process, or zero if it has gone."""
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (OSError, ValueError, IndexError):
        return 0
    return 0


def _descendants(pid: int) -> list[int]:
    """A process and everything under it, best effort."""
    found = [pid]
    frontier = [pid]
    while frontier:
        parent = frontier.pop()
        try:
            children = Path(f"/proc/{parent}/task").iterdir()
        except OSError:
            continue
        for task in children:
            try:
                kids = (task / "children").read_text(encoding="utf-8").split()
            except OSError:
                continue
            for kid in kids:
                child = int(kid)
                if child not in found:
                    found.append(child)
                    frontier.append(child)
    return found


class PeakRss:
    """Peak summed RSS across a process tree, sampled on a thread."""

    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.peak_kib = 0
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def __enter__(self) -> "PeakRss":
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=2)

    def _run(self) -> None:
        while not self._stop.is_set():
            total = sum(_rss_kib(p) for p in _descendants(self.pid))
            self.peak_kib = max(self.peak_kib, total)
            self._stop.wait(RSS_INTERVAL)


# -------------------------------------------------------------------- h5i


class H5iSession:
    """A resident h5i browser session, driven by its CLI verbs."""

    def __init__(self, binary: str, url: str, name: str) -> None:
        self.binary = binary
        self.url = url
        self.name = name
        self.pid: int | None = None

    def _run(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        done = subprocess.run(
            [self.binary, "browser", *args, "--session", self.name],
            capture_output=True,
            text=True,
            timeout=180,
        )
        if check and done.returncode != 0:
            # A lane that cannot run is a refusal with a reason, the way an
            # engine that cannot run is. A traceback here would lose which verb
            # failed and what it said.
            raise Refused(
                f"`h5i browser {' '.join(args)}` failed: "
                f"{(done.stderr or done.stdout).strip()[:300]}"
            )
        return done

    def open(self) -> float:
        """Make the session. Returns the wall time the open cost."""
        start = time.perf_counter()
        done = subprocess.run(
            [
                self.binary, "browser", "open", self.url,
                "--script", "--session", self.name,
            ],
            capture_output=True,
            text=True,
            timeout=300,
        )
        elapsed = time.perf_counter() - start
        if done.returncode != 0:
            raise Refused(f"`h5i browser open` failed: {done.stderr.strip()[:300]}")
        status = json.loads(self._run("status", "--json").stdout)
        self.pid = status.get("control", {}).get("pid")
        if not self.pid:
            raise Refused("the session reported no control pid to sample memory from")
        return elapsed

    def close(self) -> None:
        self._run("close", check=False)

    # One measured step per lane.

    def snapshot(self) -> None:
        self._run("snapshot")

    def delta(self) -> None:
        self._run("snapshot", "--delta")

    def click(self) -> None:
        self._run("click", "@e1")

    def floor(self) -> None:
        self._run("status")


def measure_h5i(binary: str, url: str, steps: int, repeat: int, page: str) -> dict:
    if not Path(binary).exists():
        raise Refused(f"no h5i binary at {binary}; build one with `cargo build --release`")

    lanes: dict[str, list[float]] = {
        "snapshot": [], "delta": [], "click": [], "floor (status)": []
    }
    opens: list[float] = []
    peak_kib = 0

    for repetition in range(repeat):
        session = H5iSession(binary, url, f"benchloop-{page}-{repetition}")
        try:
            opens.append(session.open())
            assert session.pid is not None
            with PeakRss(session.pid) as sampler:
                # A discarded warm-up, so the first snapshot's page-cache luck
                # does not become the median.
                session.snapshot()

                order = ["snapshot", "click", "delta", "floor (status)"]
                # Rotated per repetition, so no lane is always first.
                order = order[repetition % len(order):] + order[: repetition % len(order)]
                verbs = {
                    "snapshot": session.snapshot,
                    "delta": session.delta,
                    "click": session.click,
                    "floor (status)": session.floor,
                }
                for _ in range(steps):
                    for lane in order:
                        start = time.perf_counter()
                        verbs[lane]()
                        lanes[lane].append(time.perf_counter() - start)
            peak_kib = max(peak_kib, sampler.peak_kib)
        finally:
            session.close()

    return {
        "engine": "h5i-browser",
        "page": page,
        "open_s": opens,
        "lanes": lanes,
        "peak_rss_kib": peak_kib,
    }


# --------------------------------------------------------------- chromium

# The Chromium half, as a Node script.
#
# Playwright is resolved the way `docs/demo/render.mjs` resolves it: a local
# node_modules, a global one, or an npx cache, because this repo has never
# required a browser automation stack to be installed to build or test.
CHROMIUM_DRIVER = r"""
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import { existsSync, readdirSync } from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);

// Every Playwright this machine has, not the first one found.
//
// A cached copy whose browser download is missing is worse than no copy at
// all: it resolves, then fails at launch with a path to a Chromium nobody
// installed. So the candidates are collected and each is *tried*, and the one
// that can actually start a browser wins.
function candidates() {
  const found = [];
  for (const name of ['playwright', 'playwright-core']) {
    try { found.push({ where: name, mod: require(name) }); } catch {}
  }
  const npx = path.join(homedir(), '.npm', '_npx');
  if (existsSync(npx)) {
    for (const dir of readdirSync(npx)) {
      const candidate = path.join(npx, dir, 'node_modules', 'playwright');
      if (!existsSync(candidate)) continue;
      try { found.push({ where: candidate, mod: require(candidate) }); } catch {}
    }
  }
  return found;
}

const found = candidates();
if (found.length === 0) {
  console.log(JSON.stringify({ refused: 'playwright is not installed (npm i -D playwright)' }));
  process.exit(0);
}

const [, , url, stepsRaw] = process.argv;
const steps = Number(stepsRaw);

// The clock starts before the browser does.
//
// `open` has to mean the same thing on both sides or the column is a lie:
// h5i's `browser open` creates the session and loads the page, so this must
// cover the launch and the navigation, not just the navigation onto a browser
// somebody already started.
const openStart = performance.now();
let browser = null;
let version = null;
const failures = [];
for (const { where, mod } of found) {
  for (const launch of [() => mod.chromium.launch({ channel: 'chromium' }), () => mod.chromium.launch()]) {
    try { browser = await launch(); version = where; break; } catch (e) { failures.push(e.message.split('\n')[0]); }
  }
  if (browser) break;
}
if (!browser) {
  console.log(JSON.stringify({
    refused: `no installed playwright could launch chromium: ${failures[0] ?? 'unknown'}`,
  }));
  process.exit(0);
}

const page = await browser.newPage();
await page.goto(url);
const openMs = performance.now() - openStart;

// The closest analogue to h5i's outline is an accessibility snapshot: a tree
// of roles and names, which is what an agent driving Chromium reads. Which
// call provides it has moved between Playwright versions, so the first one
// this build has wins. There is no delta to ask for on this side, so that
// lane is deliberately absent rather than faked.
let readPage;
let readVia;
if (typeof page.locator('body').ariaSnapshot === 'function') {
  readVia = 'locator.ariaSnapshot';
  readPage = () => page.locator('body').ariaSnapshot();
} else if (page.accessibility) {
  readVia = 'page.accessibility.snapshot';
  readPage = () => page.accessibility.snapshot();
} else {
  const cdp = await page.context().newCDPSession(page);
  readVia = 'cdp Accessibility.getFullAXTree';
  readPage = () => cdp.send('Accessibility.getFullAXTree');
}

const lanes = { snapshot: [], click: [] };
await readPage();  // warm-up, discarded

for (let i = 0; i < steps; i++) {
  let start = performance.now();
  await readPage();
  lanes.snapshot.push((performance.now() - start) / 1000);

  start = performance.now();
  await page.click('#add');
  lanes.click.push((performance.now() - start) / 1000);
}

console.log(JSON.stringify({ open_s: openMs / 1000, lanes, playwright: version, read_via: readVia }));
await browser.close();
"""


def measure_chromium(url: str, steps: int, repeat: int, workdir: Path, page: str) -> dict:
    node = shutil.which("node")
    if not node:
        raise Refused("node is not on PATH, so the Playwright driver cannot run")

    driver = workdir / "chromium_loop.mjs"
    driver.write_text(CHROMIUM_DRIVER, encoding="utf-8")

    lanes: dict[str, list[float]] = {"snapshot": [], "click": []}
    opens: list[float] = []
    peak_kib = 0

    for _ in range(repeat):
        done = subprocess.run(
            [node, str(driver), url, str(steps)],
            capture_output=True, text=True, timeout=600,
        )
        if done.returncode != 0:
            raise Refused(f"the Playwright driver failed: {done.stderr.strip()[:300]}")
        try:
            payload = json.loads(done.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            raise Refused(f"the Playwright driver printed no result: {done.stdout[:200]}")
        if "refused" in payload:
            raise Refused(payload["refused"])
        opens.append(payload["open_s"])
        for lane, samples in payload["lanes"].items():
            lanes[lane].extend(samples)

    return {
        "engine": "chromium (playwright)",
        "page": page,
        "open_s": opens,
        "lanes": lanes,
        # Sampled in-process by the driver would need a second thread there;
        # Chromium's resident footprint is already in the cold-read table and
        # is not what this benchmark exists to re-measure.
        "peak_rss_kib": peak_kib or None,
    }


# ------------------------------------------------------------------ report


def ms(values: list[float]) -> str:
    return f"{statistics.median(values) * 1000:.1f}" if values else "-"


def report(rows: list[dict], refusals: dict[str, str], steps: int, repeat: int) -> None:
    print("\nh5i agent loop: what a step costs once the page is open")
    print(f"median of {steps} steps x {repeat} repetitions, after a discarded warm-up\n")

    lanes = ["snapshot", "delta", "click", "floor (status)"]
    label = lambda row: f"{row['engine']} / {row['page']}"
    width = max([len(label(row)) for row in rows] + [22])

    header = f"{'engine / page':<{width}} {'open':>9}"
    for lane in lanes:
        header += f" {lane:>15}"
    print(header)
    for row in rows:
        line = f"{label(row):<{width}} {ms(row['open_s']):>9}"
        for lane in lanes:
            line += f" {ms(row['lanes'].get(lane, [])):>15}"
        print(line)
    print("\nall times in milliseconds; `-` is a lane the engine does not have")

    for row in rows:
        peak = row.get("peak_rss_kib")
        if peak:
            print(f"{label(row)}: peak resident set across the session tree {peak / 1024:.1f} MB")

    # The decomposition the whole benchmark exists for: how much of a step is
    # paid before the engine reads anything, and how much tracks the page.
    for row in rows:
        if row["engine"] != "h5i-browser":
            continue
        floor_samples = row["lanes"].get("floor (status)", [])
        floor = statistics.median(floor_samples) if floor_samples else 0.0
        for lane in ("snapshot", "delta"):
            if row["lanes"].get(lane):
                total = statistics.median(row["lanes"][lane])
                print(
                    f"{label(row)} {lane}: {total * 1000:.1f} ms per step, of which "
                    f"{floor * 1000:.1f} ms is the floor a verb pays before it reads "
                    f"anything, leaving at most {max(total - floor, 0) * 1000:.1f} ms "
                    f"of engine work"
                )

    for name, reason in refusals.items():
        print(f"{name}: not measured. {reason}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", default="target/release/h5i", help="the h5i binary to drive")
    parser.add_argument("--steps", type=int, default=15, help="steps per repetition")
    parser.add_argument("--repeat", type=int, default=3, help="repetitions")
    parser.add_argument(
        "--engines",
        default="h5i,chromium",
        help="comma-separated: h5i, chromium. Naming one makes it required.",
    )
    parser.add_argument("--json", type=Path, help="write every sample here")
    args = parser.parse_args()

    wanted = [name.strip() for name in args.engines.split(",") if name.strip()]
    explicit = args.engines != parser.get_default("engines")

    workdir = Path(tempfile.mkdtemp(prefix="h5i-agent-loop-"))
    pages: dict[str, str] = {}
    for name, markup in (("small", SMALL_FIXTURE), ("large", large_fixture())):
        fixture = workdir / f"loop-{name}.html"
        fixture.write_text(markup, encoding="utf-8")
        pages[name] = fixture.as_uri()

    rows: list[dict] = []
    refusals: dict[str, str] = {}

    try:
        for page, url in pages.items():
            if "h5i" in wanted:
                try:
                    rows.append(measure_h5i(args.bin, url, args.steps, args.repeat, page))
                except Refused as refusal:
                    refusals[f"h5i-browser / {page}"] = str(refusal)
            if "chromium" in wanted:
                try:
                    rows.append(
                        measure_chromium(url, args.steps, args.repeat, workdir, page)
                    )
                except Refused as refusal:
                    refusals[f"chromium (playwright) / {page}"] = str(refusal)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    report(rows, refusals, args.steps, args.repeat)

    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "steps": args.steps,
                    "repeat": args.repeat,
                    "results": rows,
                    "refusals": refusals,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        print(f"\nsamples written to {args.json}")

    # A refusal is only a failure when the engine was asked for by name.
    return 1 if refusals and explicit else 0


if __name__ == "__main__":
    sys.exit(main())
