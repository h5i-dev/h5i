#!/usr/bin/env bash
# Extract the example kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
# Starting from transition/apply skips Debug/Default impls, which only exist
# as axioms in Aeneas.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/examples/app/docs/kernel" && charon cargo --preset=aeneas \
  --start-from docs_kernel::transition --start-from docs_kernel::apply --start-from docs_kernel::read_scope --start-from docs_kernel::check_inv --start-from docs_kernel::sql_writes --start-from docs_kernel::decode --start-from docs_kernel::scoped_project \
  $(bash "$root/scripts/app/schema-items.sh" docs_kernel) --include h5i_app_sql \
  --dest-file "$tmp/docs_kernel.llbc")
aeneas -backend lean "$tmp/docs_kernel.llbc" -dest "$root/examples/app/docs/proofs/generated"
for f in "$root/examples/app/docs/proofs/generated"/*.lean; do bash "$(dirname "$0")/normalize-sources.sh" "$f" "$root"; done
