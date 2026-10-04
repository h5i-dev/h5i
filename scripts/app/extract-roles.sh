#!/usr/bin/env bash
# Extract the roles kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas),
# with the h5i-app-std functions it calls, then copy h5i-app-std's specs.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
gen="$root/examples/app/roles/proofs/generated"
(cd "$root/examples/app/roles/kernel" && charon cargo --preset=aeneas \
  --start-from roles_kernel::transition --include h5i_app_std \
  --dest-file "$tmp/roles_kernel.llbc")
aeneas -backend lean "$tmp/roles_kernel.llbc" -dest "$gen"
bash "$(dirname "$0")/normalize-sources.sh" "$gen/RolesKernel.lean" "$root"
bash "$(dirname "$0")/std-specs.sh" RolesKernel roles_kernel "$gen"
