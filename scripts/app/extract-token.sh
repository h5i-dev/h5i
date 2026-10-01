#!/usr/bin/env bash
# Extract h5i-app-token to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/crates/h5i-app-token" && charon cargo --preset=aeneas \
  --start-from h5i_app_token::parse --start-from h5i_app_token::encode_payload --start-from h5i_app_token::join \
  --dest-file "$tmp/h5i_app_token.llbc")
aeneas -backend lean "$tmp/h5i_app_token.llbc" -dest "$root/crates/h5i-app-token/proofs/generated"
