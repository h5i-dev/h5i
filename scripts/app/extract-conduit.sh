#!/usr/bin/env bash
# Extract the Conduit kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/conduit/kernel" && charon cargo --preset=aeneas \
  --start-from conduit_kernel::transition --start-from conduit_kernel::transition_upstream \
  $(bash "$root/scripts/app/schema-items.sh" conduit_kernel) --include h5i_app_sql \
  --dest-file "$tmp/conduit_kernel.llbc")
aeneas -backend lean "$tmp/conduit_kernel.llbc" -dest "$root/examples/app/conduit/proofs/generated"
for f in "$root/examples/app/conduit/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
