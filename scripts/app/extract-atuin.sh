#!/usr/bin/env bash
# Extract the Atuin kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/atuin/kernel" && charon cargo --preset=aeneas \
  --start-from atuin_kernel::transition --start-from atuin_kernel::transition_current \
  $(bash "$root/scripts/app/schema-items.sh" atuin_kernel) --include h5i_app_sql \
  --dest-file "$tmp/atuin_kernel.llbc")
aeneas -backend lean "$tmp/atuin_kernel.llbc" -dest "$root/examples/app/atuin/proofs/generated"
for f in "$root/examples/app/atuin/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
