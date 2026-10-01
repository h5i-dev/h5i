#!/usr/bin/env bash
# Extract the h5i-app-sql planner to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas).
# Starting from `plan` skips Debug impls, which only exist as axioms in Aeneas.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$root/crates/h5i-app-sql" && charon cargo --preset=aeneas \
  --start-from h5i_app_sql::plan \
  --dest-file "$tmp/h5i_app_sql.llbc")
aeneas -backend lean "$tmp/h5i_app_sql.llbc" -dest "$root/crates/h5i-app-sql/proofs/generated"
