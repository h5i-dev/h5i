#!/usr/bin/env bash
# Extract the crates.io kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/cratesio/kernel" && charon cargo --preset=aeneas \
  --start-from cratesio_kernel::transition --start-from cratesio_kernel::transition_pre14760 \
  $(bash "$root/scripts/app/schema-items.sh" cratesio_kernel) --include h5i_app_sql \
  --dest-file "$tmp/cratesio_kernel.llbc")
aeneas -backend lean "$tmp/cratesio_kernel.llbc" -dest "$root/examples/app/cratesio/proofs/generated"
for f in "$root/examples/app/cratesio/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
