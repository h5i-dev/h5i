#!/usr/bin/env python3
"""Total up every per-directory result file into one honest score.

Honest here means the three numbers stay separate and all three get printed:
what was scored, what could not be scored because this engine could not get the
file to report, and what was never on the table because a static server cannot
serve it. A single percentage hides which of the three moved.
"""

import collections
import json
import sys
from pathlib import Path

RESULTS = Path(__file__).resolve().parent / "results"


def main():
    files = sorted(p for p in RESULTS.glob("*.json") if p.name != "merged.json")
    if not files:
        sys.exit(f"no result files in {RESULTS}; run wpt/sweep.sh first")

    subtests = collections.Counter()
    outcomes = collections.Counter()
    unsupported = collections.Counter()
    per_dir, totals = {}, collections.Counter()

    for path in files:
        blob = json.loads(path.read_text())
        summary = blob["summary"]
        subtests.update(summary["subtests"])
        outcomes.update(summary["outcomes"])
        unsupported.update(summary["unsupported"])
        for key in ("files", "files_measured", "files_unmeasured",
                    "generated_endpoints_skipped", "unscoreable_files_skipped"):
            totals[key] += summary.get(key, 0)
        per_dir[path.stem] = (summary["subtests_passing"],
                              summary["subtests_total"],
                              summary["files"])

    passing = subtests["PASS"]
    scored = sum(subtests.values())
    print("=" * 66)
    print(f"WPT subtests passing: {passing}  of {scored} scored")
    print(f"files run {totals['files']}  "
          f"(reported {totals['files_measured']}, silent {totals['files_unmeasured']})")
    print(f"not run: {totals['generated_endpoints_skipped']} generated endpoints, "
          f"{totals['unscoreable_files_skipped']} files with no testharness")
    print()
    print("outcomes:", dict(outcomes.most_common()))
    print("subtests:", dict(subtests.most_common()))
    print()
    print("top directories by passing subtests:")
    for name, (p, t, f) in sorted(per_dir.items(), key=lambda kv: -kv[1][0])[:20]:
        print(f"  {p:7d} / {t:<7d} {f:5d} files  {name}")
    print()
    print("most-wanted missing APIs:")
    for api, n in unsupported.most_common(30):
        print(f"  {n:6d}  {api}")

    (RESULTS / "merged.json").write_text(json.dumps({
        "subtests_passing": passing,
        "subtests_scored": scored,
        "subtests": dict(subtests),
        "outcomes": dict(outcomes),
        "files": dict(totals),
        "per_dir": per_dir,
        "unsupported": dict(unsupported.most_common(200)),
    }, indent=1))
    print(f"\nwrote {RESULTS / 'merged.json'}")


if __name__ == "__main__":
    main()
