#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME=/var/tmp/h5i-gpu-cargo-home CARGO_TARGET_DIR=/var/tmp/h5i-gpu-target RUSTUP_HOME=/var/tmp/h5i-gpu-rustup TMPDIR=/var/tmp CUDA_VISIBLE_DEVICES=0
export CUDA_PATH=/usr/local/cuda-12.6 PATH=/usr/local/cuda/bin:$PATH
export BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/13/include
nice -n 19 taskset -c 64-127 cargo +nightly-2026-04-02 build --release -p h5i-app-gpu-backend-nvvm --example measure --locked -j 8
export LD_LIBRARY_PATH=/usr/local/cuda-12.6/lib64
exec nice -n 19 taskset -c 64-127 "$CARGO_TARGET_DIR/release/examples/measure" "$CARGO_TARGET_DIR/nvptx64-nvidia-cuda/release/h5i_app_gpu_nvvm_device.ptx"
