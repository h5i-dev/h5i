#!/usr/bin/env bash
# Extract h5i-app-std to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/crates/h5i-app-std" && charon cargo --preset=aeneas \
  --dest-file "$tmp/h5i_app_std.llbc")
aeneas -backend lean "$tmp/h5i_app_std.llbc" -dest "$root/crates/h5i-app-std/proofs/generated"
