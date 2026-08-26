#!/usr/bin/env bash
# Build the agent as a wasm32 module: crates/h5i-wasm-harness/build/h5i_wasm_harness.wasm
#
# The library is #![no_std] + alloc with zero dependencies, so the stock
# wasm32-unknown-unknown target (prebuilt core + alloc) is all that's needed —
# no -Zbuild-std, no nightly, no crates.io. The cdylib crate-type is requested
# here rather than in Cargo.toml because a #![no_std] cdylib built for the host
# has no allocator/panic handler and would break `cargo build --workspace`.
#
# Prereq (one time):  rustup target add wasm32-unknown-unknown
set -euo pipefail

TARGET=wasm32-unknown-unknown
# This crate lives in a workspace, so cargo writes artifacts to the workspace
# target dir, not a per-crate one. Resolve both the workspace root and the
# crate's own dir from the script location.
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="$CRATE_DIR/build"
mkdir -p "$OUT"

if ! rustup target list --installed 2>/dev/null | grep -q "^${TARGET}$"; then
  echo "error: the $TARGET target is not installed." >&2
  echo "       run:  rustup target add $TARGET" >&2
  exit 1
fi

# --lib so the native i5h binary (which needs std) is not dragged into the wasm
# build; --crate-type cdylib to emit a loadable module rather than an rlib.
( cd "$ROOT" && cargo rustc -p h5i-wasm-harness --release --lib \
    --target "$TARGET" --crate-type cdylib )

BUILT="$TARGET_DIR/$TARGET/release/h5i_wasm_harness.wasm"
cp "$BUILT" "$OUT/h5i_wasm_harness.wasm"
ls -la "$OUT/h5i_wasm_harness.wasm"
echo "exports: memory, alloc, dealloc, agent_init, agent_step, agent_dump (no imports)"
