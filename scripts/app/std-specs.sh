#!/usr/bin/env bash
# Copy h5i-app-std's specs next to a kernel's extracted Lean:
#   std-specs.sh KernelModule kernel_crate dest-dir
# The copy imports the kernel's extraction (generated/KernelModule.lean) and
# opens its namespace (the crate name), where Aeneas puts the
# `h5i_app_std.*` functions the kernel calls. Their bodies are the crate's,
# so the proofs go through unchanged; `h5i_when` skips the specs of
# functions the kernel does not call. Add `StdSpecs` to the roots of the
# kernel's generated library.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
module=$1 ns=$2 dest=$3
{
  echo "-- Copied from crates/h5i-app-std/proofs/StdSpecs.lean by scripts/app/std-specs.sh. Do not edit."
  sed -e "s/^import H5iAppStd$/import $module/" \
      -e "s/^open Aeneas Aeneas.Std Result H5iAppLib$/& $ns/" \
      "$root/crates/h5i-app-std/proofs/StdSpecs.lean"
} > "$dest/StdSpecs.lean"
grep -q "^import $module$" "$dest/StdSpecs.lean" && grep -q "^open .* $ns$" "$dest/StdSpecs.lean" || {
  echo "std-specs.sh: StdSpecs.lean header changed; update the patterns" >&2; exit 1; }
