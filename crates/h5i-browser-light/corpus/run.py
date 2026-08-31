#!/usr/bin/env python3
"""Point the engine at real sites and let them choose what to build next.

roadmap-history.md §B8. This is a **development tool, not a test**: it needs the
network, the sites change under it, and a run takes minutes. The regression gate
that CI runs is `tests/corpus.rs`, which exercises the same patterns against
local fixtures and needs nothing outside the repository.

Two corpora, because they ask for different things:

  documents     wikis, references, standards, package pages — where agents
                actually spend their time, and mostly server-rendered.
  applications  single-page apps and interactive demos. The document corpus will
                never ask for routing, storage or template cloning, because it
                contains nothing that does them.

Each URL is loaded twice, with script and without, because the interesting
number is not "did it render" but "what did script add, and what did it ask for
that we do not have".

    python3 run.py                  # both corpora
    python3 run.py --only documents
    python3 run.py --json-out results.json
"""
import argparse
import collections
import json
import os
import resource
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import harness  # noqa: E402

# Where agents actually spend their time. Plus a few script-heavy ones, so the
# corpus is not all easy and the failures are honest.
DOCUMENTS = [
    "https://example.com/",
    "https://en.wikipedia.org/wiki/Kelp",
    "https://developer.mozilla.org/en-US/docs/Web/API/fetch",
    "https://doc.rust-lang.org/book/ch01-00-getting-started.html",
    "https://docs.python.org/3/library/json.html",
    "https://news.ycombinator.com/",
    "https://lobste.rs/",
    "https://crates.io/crates/serde",
    "https://pypi.org/project/requests/",
    "https://github.com/rust-lang/rust",
    "https://stackoverflow.com/questions/tagged/rust",
    "https://www.rust-lang.org/",
    "https://blog.rust-lang.org/",
    "https://tc39.es/ecma262/#sec-intro",
    "https://html.spec.whatwg.org/multipage/introduction.html",
    "https://www.w3.org/TR/CSP3/",
    "https://json.org/",
    "https://man7.org/linux/man-pages/man2/open.2.html",
    "https://ziglang.org/documentation/master/",
    "https://go.dev/doc/",
    "https://nodejs.org/api/fs.html",
    "https://react.dev/learn",
    "https://vitejs.dev/guide/",
    "https://tailwindcss.com/docs/installation",
    "https://arxiv.org/abs/1706.03762",
    "https://curl.se/docs/manpage.html",
    "https://sqlite.org/lang_select.html",
    "https://redis.io/docs/latest/commands/get/",
]

# Writing systems this engine had never been pointed at. Text shaping, bidi and
# CJK line breaking all run through parley, and every page measured until now was
# Latin — so nothing here had ever been exercised, in an engine whose entire
# product is extracted text.
INTERNATIONAL = [
    # CJK: no spaces between words, so line breaking and text extraction differ.
    "https://ja.wikipedia.org/wiki/寿司",
    "https://zh.wikipedia.org/wiki/茶",
    "https://ko.wikipedia.org/wiki/김치",
    "https://developer.mozilla.org/ja/docs/Web/API/fetch",
    "https://www.aozora.gr.jp/cards/000148/files/789_14547.html",
    # Right-to-left, and bidirectional where Latin is embedded in it.
    "https://ar.wikipedia.org/wiki/قهوة",
    "https://he.wikipedia.org/wiki/ספר",
    "https://fa.wikipedia.org/wiki/چای",
    # Other scripts and diacritics.
    "https://ru.wikipedia.org/wiki/Чай",
    "https://el.wikipedia.org/wiki/Καφές",
    "https://hi.wikipedia.org/wiki/चाय",
    "https://th.wikipedia.org/wiki/ชา",
    # Latin, but heavily accented and hyphenated.
    "https://de.wikipedia.org/wiki/Kaffee",
    "https://vi.wikipedia.org/wiki/Trà",
]

# Shapes an agent meets constantly and the other corpora do not contain: big
# tables, forms, plain-text standards, and markup old enough to predate the
# conventions the rest of the web settled on.
STRUCTURES = [
    "https://en.wikipedia.org/wiki/List_of_countries_by_GDP_(nominal)",
    "https://www.w3.org/TR/WCAG21/",
    "https://datatracker.ietf.org/doc/html/rfc2616",
    "https://www.gnu.org/software/bash/manual/bash.html",
    "https://news.ycombinator.com/newest",
    "https://pypi.org/search/?q=requests",
    "https://www.rfc-editor.org/rfc/rfc9110.html",
    "https://caniuse.com/fetch",
]

# Applications: routing, client-side rendering, local state, custom elements.
APPLICATIONS = [
    "https://vite.dev/guide/",
    "https://svelte.dev/docs/svelte/overview",
    "https://vuejs.org/guide/introduction.html",
    "https://angular.dev/overview",
    "https://www.solidjs.com/guides/getting-started",
    "https://nextjs.org/docs",
    "https://astro.build/",
    "https://remix.run/docs/en/main",
    "https://preactjs.com/guide/v10/getting-started/",
    "https://lit.dev/docs/",
    "https://todomvc.com/examples/react/dist/",
    "https://todomvc.com/examples/vue/dist/",
    "https://excalidraw.com/",
    "https://jsonformatter.org/",
    "https://regex101.com/",
    "https://crates.io/crates/serde",
    "https://www.npmjs.com/package/react",
    "https://bundlephobia.com/package/react",
    "https://shoelace.style/components/button",
    "https://material-web.dev/components/button/",
]

# The allowlist this harness used to build by hand is gone, and the reason is
# worth keeping. It granted the site's own host, a wildcard over its registrable
# domain, and six named CDNs — which meant a page pulling from a *seventh* CDN
# came back short and looked like an engine failure. The corpus exists to see
# what pages ask for, so an instrument that pre-filters the asks is measuring
# its own configuration. `harness.ENGINE_GRANT` is that decision made once, out
# loud, and it widens the name check only (roadmap-history.md §B19.5).

# A per-child address-space ceiling. One enormous page rendered without a cap
# took the whole machine down mid-run. A child that hits this dies with a status
# the harness records, which is itself a result worth having: "too big for the
# engine as configured" is a finding, not an outage.
MEMORY_CAP = 2 * 1024 * 1024 * 1024


def cap():
    resource.setrlimit(resource.RLIMIT_AS, (MEMORY_CAP, MEMORY_CAP))


def run(binary, url, script):
    cmd = harness.instrument_argv(
        binary, "open", url, "--json", "--max-snapshot-lines", "300"
    )
    if script:
        cmd.append("--script")
    try:
        done = subprocess.run(cmd, capture_output=True, timeout=90, text=True, preexec_fn=cap)
    except subprocess.TimeoutExpired:
        return {"error": "timeout"}
    except MemoryError:
        return {"error": "out of memory"}
    if done.returncode != 0:
        lines = (done.stderr or "").strip().splitlines()
        note = next((l for l in lines if "ICU4X" not in l), "")
        return {"error": note[:160] or f"exit {done.returncode}"}
    try:
        return json.loads(done.stdout)
    except Exception as e:
        return {"error": f"unparseable: {e}"}


def summarise(payload):
    if "error" in payload:
        return None
    snap = payload.get("snapshot") or {}
    denied = sum(
        1 for r in payload.get("requests", [])
        if r.get("phase") == "request" and not r.get("allowed", True)
    )
    return {
        "lines": len(snap.get("lines") or []),
        "refs": len(snap.get("refs") or []),
        "denied": denied,
        "unsupported": {u["api"]: u["calls"] for u in payload.get("unsupported", [])},
        # Only what the *engine* said. A page calling `console.error` is the
        # site reporting its own trouble, which is information rather than a
        # failure of this engine to explain itself.
        "errors": [
            c["text"] for c in payload.get("console", [])
            if c["level"] == "error" and c.get("source", "page") == "engine"
        ],
        "page_errors": sum(
            c.get("repeats", 1) for c in payload.get("console", [])
            if c["level"] == "error" and c.get("source", "page") == "page"
        ),
        "settled": payload.get("settled"),
    }


# An error line that names nothing is the failure this instrument exists to
# prevent: it reports a problem an agent cannot act on and we cannot find.
ATTRIBUTED_PREFIXES = ("could not load", "`", "module failed", "inline script", "http")


def anonymous(text):
    kind = text.split(":")[0]
    return kind in ("TypeError", "ReferenceError", "Error", "SyntaxError", "RangeError")


def measure(binary, name, sites):
    api_calls, api_sites, asked_by = collections.Counter(),
    collections.Counter(), {} error_kinds = collections.Counter()
    rows, failures, anonymous_errors = [], [], []

    print(f"\n### {name} ({len(sites)} sites)\n")
    for url in sites:
        without = summarise(run(binary, url, script=False))
        with_js = summarise(run(binary, url, script=True))
        if without is None and with_js is None:
            failures.append(url)
            print(f"  FAIL   {url}", flush=True)
            continue
        base = without or {"lines": 0, "refs": 0, "denied": 0}
        # Substituting the no-script reading when the script run fails is how a
        # timeout disappears from a report: the row looks like a page that
        # rendered, because the *other* run of it did. Recorded instead.
        script_failed = with_js is None
        js = with_js or base

        for api, calls in js.get("unsupported", {}).items():
            api_calls[api] += calls
            api_sites[api] += 1
            asked_by.setdefault(api, []).append(url)
        for text in js.get("errors", []):
            error_kinds[text.split(":")[0][:60]] += 1
            if anonymous(text):
                anonymous_errors.append((url, text[:120]))

        rows.append({
            "url": url,
            "lines_without": base["lines"],
            "lines_with": js["lines"],
            "refs": js["refs"],
            "denied": js["denied"],
            "errors": len(js.get("errors", [])),
            "page_errors": js.get("page_errors", 0),
            "cut_off": bool(js.get("settled") and "still busy" in
            str(js["settled"])), "script_failed": script_failed,
            # Script that *loses* content is its own kind of failure, and it
            # looks like success in a column that only counts lines.
            "script_lost": js["lines"] < base["lines"] * 0.8,
        })
        print(
            f"  {js['lines']:>4} lines ({base['lines']:>4} w/o)  {js['refs']:>3} refs 
            " f"{len(js.get('errors', [])):>2} err  {js.get('page_errors', 0):>3} page  "
            f"{js['denied']:>2} denied  {url}"
            + ("   [script run FAILED — this row is the no-script reading]" if
            script_failed else "") + ("   [script LOST content]" if js["lines"] < base["lines"]
            * 0.8 else ""), flush=True,
        )

    rendered = sum(1 for r in rows if r["lines_with"] >= 5)
    gained = [r for r in rows if r["lines_with"] > r["lines_without"] * 1.2 + 2]
    print(f"\n{len(rows)}/{len(sites)} loaded; {rendered} gave a usable outline (>=5
    lines)") print(f"{len(gained)} rendered materially more *with* script")
    print(f"{sum(1 for r in rows if r['cut_off'])} did not settle within
    budget") broken = [r["url"] for r in rows if r["script_failed"]]
    lost = [r["url"] for r in rows if r["script_lost"] and not
    r["script_failed"]] if broken:
        print(f"{len(broken)} could not be read *with* script at all: {', '.join(broken)}")
    if lost:
        print(f"{len(lost)} rendered materially LESS with script: {', '.join(lost)}")
    if failures:
        print(f"failed entirely: {', '.join(failures)}")

    print("\n--- what it asked for and we lack ---")
    if not api_sites:
        print("  (nothing)")
    for api, sites_count in api_sites.most_common(40):
        print(f"  {api:<44} {sites_count:>3} sites {api_calls[api]:>4} calls   {asked_by[api][0]}")

    print("\n--- console error kinds ---")
    for kind, n in error_kinds.most_common(12):
        print(f"  {n:>3}  {kind}")

    # The number that matters more than the empty ask list: an error that names
    # neither a request nor a script is one nobody can act on.
    print(f"\n--- anonymous errors: {len(anonymous_errors)} ---")
    for url, text in anonymous_errors[:10]:
        print(f"  {url}\n      {text}")

    return {
        "rows": rows, "failures": failures, "asked_by": asked_by,
        "api_sites": dict(api_sites), "api_calls": dict(api_calls),
        "error_kinds": dict(error_kinds), "anonymous": anonymous_errors,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=None)
    parser.add_argument(
        "--only", choices=["documents", "applications", "international", "structures"]
    )
    parser.add_argument("--json-out")
    args = parser.parse_args()

    corpora = {
        "documents": DOCUMENTS,
        "applications": APPLICATIONS,
        "international": INTERNATIONAL,
        "structures": STRUCTURES,
    }
    if args.only:
        corpora = {args.only: corpora[args.only]}

    binary = harness.engine_binary(args.binary)
    harness.check_engine(binary)
    print(f"engine: {binary} (grant: {harness.ENGINE_GRANT})")

    results = {name: measure(binary, name, sites) for name, sites in
    corpora.items()} if args.json_out:
        with open(args.json_out, "w") as f:
            json.dump(results, f, indent=2)


main()
