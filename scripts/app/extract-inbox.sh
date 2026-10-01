#!/usr/bin/env bash
# Extract tutorial 4's kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/tutorials/inbox/kernel" && charon cargo --preset=aeneas \
  --start-from inbox_kernel::transition $(bash "$root/scripts/app/schema-items.sh" inbox_kernel) \
  --include h5i_app_sql \
  --dest-file "$tmp/inbox_kernel.llbc")
aeneas -backend lean "$tmp/inbox_kernel.llbc" -dest "$root/examples/app/tutorials/inbox/proofs/generated"
for f in "$root/examples/app/tutorials/inbox/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
