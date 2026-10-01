#!/usr/bin/env bash
# Fail on sorry/native_decide in hand-written proofs, and on any axiom beyond
# Lean's standard three in the main theorems. Run after `lake build`.
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
proofs="$root/examples/app/docs/proofs"
cd "$proofs"
fail=0

# Generated files live in generated/; these are the hand-written ones.
files=$(ls *.lean | grep -v -x -e lakefile.lean)
if hits=$(grep -n -w -e sorry -e native_decide $files); then
  echo "error: sorry or native_decide in proofs:"
  echo "$hits"
  fail=1
fi

theorems="Theorems.allows_eq Theorems.transition_total Theorems.apply_eq Theorems.authorized
  Theorems.reply_confined Theorems.inv_preserved Theorems.reachable_inv Theorems.noninterference
  Frame.transition_frame Check.check_inv_spec Storage.encode_applyAll Storage.stored
  Storage.sql_writes_spec Storage.sql_writes_stored Load.fresh Load.decode_spec Load.load_sound
  Load.store_sound Load.sql_writes_storedC Scoped.scoped_sound Scoped.scoped_command Scoped.served_inv
  Theorems.emit_publishes Scenarios.authorized_reachable Scenarios.noninterference_reachable
  Scenarios.transition_frame_reachable Scenarios.published_reachable Database.pg_served Database.db_inv"
library_theorems="H5iAppLib.Store.App.served_holds H5iAppLib.Store.App.served_lists H5iAppLib.Pg.compile_sound
  H5iAppLib.Pg.compileAll_sound H5iAppLib.Pg.select_sound H5iAppLib.Pg.lists_of_selects H5iAppLib.Pg.create_fresh
  H5iAppLib.Pg.create_kept H5iAppLib.Pg.lexName_quote"
full_theorems="$library_theorems"
for t in $theorems; do full_theorems="$full_theorems docs_kernel.$t"; done
# Modules holding the main theorems.
"$root/scripts/app/ci-axioms.sh" "$proofs" Theorems Invariants Noninterference Frame Check Storage Load Scoped \
  Scenarios Database -- $full_theorems || fail=1

[ $fail -eq 0 ] && echo "lean gate: ok"
exit $fail
