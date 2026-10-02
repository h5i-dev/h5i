#!/usr/bin/env bash
# Check that theorems use only Lean's standard axioms.
# Usage: ci-axioms.sh DIR MODULE... -- THEOREM...
# Run after `lake build` in DIR. With H5I_AXIOMS_LOG set, appends one
# tab-separated line per theorem: dir, theorem, axioms, ok|bad|missing.
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
    [ -n "${H5I_AXIOMS_LOG:-}" ] && printf '%s\t%s\t\tmissing\n' "$dir" "$t" >> "$H5I_AXIOMS_LOG"
    fail=1
    continue
  fi
  used=$(grep -o '\[.*\]' <<<"$line" | tr -d '[] ')
  extra=$(tr ',' '\n' <<<"$used" \
    | grep -v -x -e propext -e Classical.choice -e Quot.sound -e '' || true)
  verdict=ok
  if [ -n "$extra" ]; then
    echo "error: $t uses non-standard axioms: $(echo $extra)"
    verdict=bad
    fail=1
  fi
  [ -n "${H5I_AXIOMS_LOG:-}" ] && printf '%s\t%s\t%s\t%s\n' "$dir" "$t" "$used" "$verdict" >> "$H5I_AXIOMS_LOG"
done
exit $fail
