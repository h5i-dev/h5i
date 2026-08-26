#!/usr/bin/env bash
# Build the agent core as a wasm32 guest module: crates/h5i-wasm-harness/build/h5i-agent.wasm
#
# This is the *guest*, not the CLI: no_std, no I/O, just the loop behind the
# seven-export ABI. A host (a browser page, a WASI runtime) performs its effects.
#
# The library is #![no_std] + alloc with zero dependencies, so the stock
# wasm32-unknown-unknown target (prebuilt core + alloc) is all that's needed:
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

# --lib so the native h5i-agent binary (which needs std) is not dragged into the wasm
# build; --crate-type cdylib to emit a loadable module rather than an rlib.
( cd "$ROOT" && cargo rustc -p h5i-wasm-harness --release --lib \
    --target "$TARGET" --crate-type cdylib )

# Cargo names the artifact after the lib (h5i_wasm_harness); publish it under
# the agent's name so the browser/WASI host loads `h5i-agent.wasm`.
BUILT="$TARGET_DIR/$TARGET/release/h5i_wasm_harness.wasm"
cp "$BUILT" "$OUT/h5i-agent.wasm"
ls -la "$OUT/h5i-agent.wasm"
echo "exports: memory, alloc, dealloc, agent_init, agent_step, agent_resume, agent_dump (no imports)"
