#!/usr/bin/env bash
# Extract tutorial 3's kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/tutorials/ledger/kernel" && charon cargo --preset=aeneas \
  --start-from ledger_kernel::transition $(bash "$root/scripts/app/schema-items.sh" ledger_kernel) \
  --include h5i_app_sql \
  --dest-file "$tmp/ledger_kernel.llbc")
aeneas -backend lean "$tmp/ledger_kernel.llbc" -dest "$root/examples/app/tutorials/ledger/proofs/generated"
for f in "$root/examples/app/tutorials/ledger/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
