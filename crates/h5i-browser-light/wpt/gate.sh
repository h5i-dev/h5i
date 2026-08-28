#!/usr/bin/env bash
# Run the WPT subset the regression gate is built on, then check it.
#
# A subset rather than the whole suite because the whole suite is a 2 GiB
# checkout and forty minutes, and a gate nobody can afford to run is not a gate.
# These five directories were picked for being the ones this engine's own work
# actually moves — DOM, CSSOM, HTML reflection, parsing and encoding — so a
# change that breaks the engine has to break one of them.
#
#   WPT_ROOT=~/Dev/wpt wpt/gate.sh          # check against the baseline
#   WPT_ROOT=~/Dev/wpt wpt/gate.sh --write  # record a new baseline
set -euo pipefail
cd "$(dirname "$0")/.."

GATE_DIRS=(dom css/cssom html/dom domparsing encoding)
JOBS="${JOBS:-4}"
TIMEOUT="${TIMEOUT:-30}"
OUT="${OUT:-wpt/gate-results}"

if [ ! -x "../../target/release/h5i" ]; then
  echo "no release binary; cargo build --release -p h5i" >&2
  exit 1
fi

rm -rf "$OUT"; mkdir -p "$OUT"
for dir in "${GATE_DIRS[@]}"; do
  echo "== $dir"
  python3 wpt/run.py --dirs "$dir" --jobs "$JOBS" --timeout "$TIMEOUT" \
    --out "$OUT/${dir//\//_}.json" | grep -E "^subtests passing"
done

python3 wpt/check.py --results "$OUT" "$@"
