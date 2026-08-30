#!/usr/bin/env bash
# Run WPT one top-level directory at a time, saving each result as it lands.
#
# Chunked rather than one `--all` run for a reason learned the hard way: a
# single process holding two hours of results loses all of them when something
# kills it, and something did. Each directory writes its own file, so a sweep
# that dies at hour two keeps hour one, and re-running skips what is already on
# disk.
#
#   wpt/sweep.sh            # every directory, biggest first
#   wpt/sweep.sh css html   # just these
set -uo pipefail
cd "$(dirname "$0")/.."

JOBS="${JOBS:-4}"
# Ninety seconds, not thirty, and the reason is a measurement rather than a
# preference.
#
# `html/dom/idlharness.https.html` took **28.8s** under a 30s deadline on one
# sweep and timed out on the next. That file is 6,408 subtests — 5.2% of the
# core denominator — and it passes about 60%, so *losing* it moved the headline
# from 74.8% up to 75.6% while the engine had strictly improved. A percentage
# that swings five points on a 1.2-second margin cannot measure a campaign.
#
# It is not one file. 7,325 subtests (6.3% of the denominator) sit in files that
# finish within ten seconds of a 30s deadline — the two CSSOM `idlharness` files
# at 20.5s and 20.2s are next in line.
#
# The cost is small, and that is a measurement too: only **14 files** in a whole
# core sweep reach `engine_timeout`. The 271 `fetch` files that time out are
# `harness_timeout` — testharness reports internally and they score inside the
# deadline — so they do not pay for a longer one. Fourteen files times sixty
# extra seconds over the job pool is about three minutes on a twenty-five minute
# sweep.
#
# §B12.5 says a pass count is only a floor if the corpus is fixed. This is its
# sibling: it is only a floor if the deadline is generous enough that the
# largest file's outcome is not a coin toss.
TIMEOUT="${TIMEOUT:-90}"
MEM_MB="${MEM_MB:-1500}"
OUT=wpt/results
mkdir -p "$OUT"

if [ $# -gt 0 ]; then
  dirs=("$@")
else
  mapfile -t dirs < <(python3 - <<'PY'
import sys
sys.path.insert(0, "wpt")
import run as R
from pathlib import Path
root = Path(R.os.path.expanduser("~/Dev/wpt"))
rows = []
for path in sorted(root.iterdir()):
    if not path.is_dir() or path.name.startswith(".") or path.name in R.SKIP_DIRS:
        continue
    tests, _, _ = R.find_tests(root, [path.name])
    if tests:
        rows.append((len(tests), path.name))
for _, name in sorted(rows, reverse=True):
    print(name)
PY
  )
fi

for dir in "${dirs[@]}"; do
  target="$OUT/${dir//\//_}.json"
  if [ -s "$target" ]; then
    echo "== $dir: already have $target, skipping"
    continue
  fi
  echo "== $dir"
  timeout 7200 python3 wpt/run.py --dirs "$dir" --jobs "$JOBS" --timeout "$TIMEOUT" \
    --mem-mb "$MEM_MB" --out "$target" 2>&1 | grep -E "^(subtests passing|files:|[0-9]+ testharness)"
done

echo
python3 wpt/merge.py
