#!/usr/bin/env bash
# Build the `h5i-agent` Python wheel: crates/h5i-wasm-harness/dist/h5i_agent-*.whl
#
# The wheel bundles the wasm module and the browser page, so an installed
# `h5i-agent` needs no repo checkout and no Rust toolchain. This script builds
# the module, stages those assets into the package, and builds the wheel.
#
# Prereqs:  a Rust toolchain with the wasm32 target (build-wasm.sh handles it),
#           and Python with the `build` package (`pip install build`).
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$CRATE_DIR"

# 1. Build the wasm module (-> build/h5i-agent.wasm).
./scripts/build-wasm.sh

# 2. Stage the assets the hosts serve into the package. Regenerated every build,
#    so the tree never carries a stale copy (the dir is git-ignored).
ASSETS="h5i_agent/_assets"
rm -rf "$ASSETS"
mkdir -p "$ASSETS"
cp build/h5i-agent.wasm "$ASSETS/"
cp web/index.html web/host.mjs "$ASSETS/"

# 3. Build the wheel.
python3 -m build --wheel --outdir dist
ls -la dist/*.whl
