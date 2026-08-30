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
