#!/usr/bin/env bash
# Differential test: Rust kernel vs the Aeneas-extracted Lean kernel.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
(cd "$root/examples/app/docs/proofs" && lake build difftest)
cd "$root/examples/app" && cargo test --release -p docs-difftest -- --ignored --nocapture "$@"
