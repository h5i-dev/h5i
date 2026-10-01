#!/usr/bin/env bash
# Extract the h5i-app-pgsql compiler to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
# Starting from the public functions skips Debug impls, which only exist as
# axioms in Aeneas.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/crates/h5i-app-pgsql" && charon cargo --preset=aeneas \
  --start-from h5i_app_pgsql::compile --start-from h5i_app_pgsql::select \
  --start-from h5i_app_pgsql::create --start-from h5i_app_pgsql::render \
  --include h5i_app_sql \
  --dest-file "$tmp/h5i_app_pgsql.llbc")
aeneas -backend lean "$tmp/h5i_app_pgsql.llbc" -dest "$root/crates/h5i-app-pgsql/proofs/generated"
