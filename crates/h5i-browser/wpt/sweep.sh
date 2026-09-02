#!/usr/bin/env bash
# Run WPT one top-level directory at a time, saving each result as it lands.
set -uo pipefail
cd "$(dirname "$0")/.."

JOBS="${JOBS:-4}"
# Ninety seconds, not thirty, and the reason is a measurement rather than a preference.
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
