#!/usr/bin/env bash
# Extract tutorial 5's kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/tutorials/booking/kernel" && charon cargo --preset=aeneas \
  --start-from booking_kernel::transition $(bash "$root/scripts/app/schema-items.sh" booking_kernel) \
  --include h5i_app_sql \
  --dest-file "$tmp/booking_kernel.llbc")
aeneas -backend lean "$tmp/booking_kernel.llbc" -dest "$root/examples/app/tutorials/booking/proofs/generated"
for f in "$root/examples/app/tutorials/booking/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
