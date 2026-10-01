#!/usr/bin/env bash
# Extract h5i-app-json's writer to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/crates/h5i-app-json" && charon cargo --preset=aeneas \
  --start-from h5i_app_json::write \
  --dest-file "$tmp/h5i_app_json.llbc")
aeneas -backend lean "$tmp/h5i_app_json.llbc" -dest "$root/crates/h5i-app-json/proofs/generated"
