#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export CARGO_HOME=/var/tmp/h5i-gpu-cargo-home CARGO_TARGET_DIR=/var/tmp/h5i-gpu-target RUSTUP_HOME=/var/tmp/h5i-gpu-rustup
export CUDA_PATH=/usr/local/cuda-12.6 CUDA_VISIBLE_DEVICES=0 TMPDIR=/var/tmp
export PATH=/usr/local/cuda/bin:$PATH
export LD_LIBRARY_PATH=/usr/local/cuda-12.6/nvvm/lib64:/usr/local/cuda-12.6/lib64:$RUSTUP_HOME/toolchains/nightly-2026-04-02-x86_64-unknown-linux-gnu/lib
export BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/13/include
export RUSTFLAGS="-Zcodegen-backend=$CARGO_TARGET_DIR/release/librustc_codegen_nvvm.so -Zunstable-options -Zcrate-attr=feature(register_tool) -Zcrate-attr=register_tool(nvvm_internal) -Zcrate-attr=no_std -Zsaturating_float_casts=false -Cpanic=immediate-abort -Cllvm-args=-arch=compute_86"
exec nice -n 19 taskset -c 64-127 cargo +nightly-2026-04-02 build --release --locked -p discount-gpu-device --features gpu --target nvptx64-nvidia-cuda -Zbuild-std=core,alloc -j 8
