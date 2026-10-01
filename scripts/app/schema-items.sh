#!/usr/bin/env bash
# Charon start points for everything `schema!` generates in kernel crate $1,
# so each lemma in the generated Schema.lean has its function extracted.
k=$1
for f in apply sql_writes decode; do echo "--start-from-if-exists $k::$f"; done
for m in put del del_where sql_put sql_del sql_del_where from_one from_rows; do echo "--start-from-if-exists $k::_::$m"; done
