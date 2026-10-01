#!/usr/bin/env bash
# Extract tutorial 1's kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/tutorials/calculator/kernel" && charon cargo --preset=aeneas \
  --start-from calculator_kernel::transition --start-from calculator_kernel::apply \
  --start-from calculator_kernel::sql_writes $(bash "$root/scripts/app/schema-items.sh" calculator_kernel) \
  --include h5i_app_sql \
  --dest-file "$tmp/calculator_kernel.llbc")
aeneas -backend lean "$tmp/calculator_kernel.llbc" -dest "$root/examples/app/tutorials/calculator/proofs/generated"
for f in "$root/examples/app/tutorials/calculator/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
