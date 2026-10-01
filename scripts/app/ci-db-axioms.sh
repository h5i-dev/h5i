#!/usr/bin/env bash
# The SQL compiler's and each server app's database theorems use only Lean's
# standard axioms. Run after building those proof projects.
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
fail=0
check() { scripts/app/ci-axioms.sh "$@" > /dev/null || { scripts/app/ci-axioms.sh "$@" | grep error; fail=1; }; }
check crates/h5i-app-pgsql/proofs Sound -- h5i_app_pgsql.Comp.valid_spec h5i_app_pgsql.Comp.create_spec \
  h5i_app_pgsql.Comp.select_spec h5i_app_pgsql.Comp.compile_spec h5i_app_pgsql.render_spec \
  h5i_app_pgsql.Sound.write_sound "h5i_app_pgsql.Sound.select_sound'" "h5i_app_pgsql.Sound.create_sound'"
check examples/app/tutorials/calculator/proofs Storage -- calculator_kernel.Storage.pg_stored
for app in board:tutorials/board ledger:tutorials/ledger inbox:tutorials/inbox booking:tutorials/booking \
    wastebin:wastebin conduit:conduit cratesio:cratesio; do
  check "examples/app/${app#*:}/proofs" Storage -- "${app%%:*}_kernel.Storage.db_inv"
done
[ $fail -eq 0 ] && echo "database axioms: ok"
exit $fail
