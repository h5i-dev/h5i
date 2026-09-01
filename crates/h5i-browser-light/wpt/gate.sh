#!/usr/bin/env bash
# Run the WPT subset the regression gate is built on, then check it.
set -euo pipefail
cd "$(dirname "$0")/.."

GATE_DIRS=(dom css/cssom html/dom domparsing encoding)
JOBS="${JOBS:-4}"
# Ninety seconds, not thirty, and the reason is a measurement rather than a preference.
TIMEOUT="${TIMEOUT:-90}"
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
