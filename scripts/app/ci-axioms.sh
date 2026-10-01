#!/usr/bin/env bash
# Check that theorems use only Lean's standard axioms.
# Usage: ci-axioms.sh DIR MODULE... -- THEOREM...
# Run after `lake build` in DIR.
set -uo pipefail
dir=$1; shift
mods=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do mods+=("$1"); shift; done
shift
cd "$dir"
mkdir -p .lake/ci
{
  for m in "${mods[@]}"; do echo "import $m"; done
  for t in "$@"; do echo "#print axioms $t"; done
} > .lake/ci/Axioms.lean
out=$(lake env lean .lake/ci/Axioms.lean 2>&1)
status=$?
echo "$out"
fail=0
if [ $status -ne 0 ]; then
  echo "error: axiom check did not compile"
  fail=1
fi
for t in "$@"; do
  line=$(grep -F "'$t'" <<<"$out" || true)
  if [ -z "$line" ]; then
    echo "error: no axiom report for $t"
    fail=1
    continue
  fi
  extra=$(grep -o '\[.*\]' <<<"$line" | tr -d '[] ' | tr ',' '\n' \
    | grep -v -x -e propext -e Classical.choice -e Quot.sound -e '' || true)
  if [ -n "$extra" ]; then
    echo "error: $t uses non-standard axioms: $(echo $extra)"
    fail=1
  fi
done
exit $fail
