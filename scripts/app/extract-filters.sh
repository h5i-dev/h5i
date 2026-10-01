#!/usr/bin/env bash
# Extract the saved-filters kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/filters/kernel" && charon cargo --preset=aeneas \
  --start-from filters_kernel::transition --start-from filters_kernel::transition_pre \
  --dest-file "$tmp/filters_kernel.llbc")
aeneas -backend lean "$tmp/filters_kernel.llbc" -dest "$root/examples/app/filters/proofs/generated"
for f in "$root/examples/app/filters/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
