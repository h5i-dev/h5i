#!/usr/bin/env bash
# No root installation, no files under HOME. Run with nice/taskset as below.
set -euo pipefail
export CARGO_HOME=/var/tmp/h5i-gpu-cargo-home CARGO_TARGET_DIR=/var/tmp/h5i-gpu-target RUSTUP_HOME=/var/tmp/h5i-gpu-rustup TMPDIR=/var/tmp
export CUDA_PATH=/usr/local/cuda-12.6 CUDA_VISIBLE_DEVICES=0 PATH=/usr/local/cuda/bin:$PATH
export LD_LIBRARY_PATH=/usr/local/cuda-12.6/nvvm/lib64:/usr/local/cuda-12.6/lib64
export BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/13/include
source_dir=/var/tmp/h5i-gpu-rust-cuda
rev=3a82c35a7196731fb60eebdac2a40d2b1bf0ba95
if [[ ! -d $source_dir/.git ]]; then git clone https://github.com/Rust-GPU/rust-cuda "$source_dir"; fi
git -C "$source_dir" checkout --detach "$rev"
nice -n 19 taskset -c 64-127 rustup toolchain install --no-self-update nightly-2026-04-02 --profile minimal --component rust-src,rustc-dev,llvm-tools-preview
mkdir -p /var/tmp/h5i-gpu-system-libs
cd /var/tmp/h5i-gpu-system-libs
# Ubuntu 24.04 packages are extracted locally, not installed system-wide.
apt-get download libssl-dev libssl3t64 libncurses-dev libxml2-dev libxml2 libtinfo6
for package in *.deb; do dpkg-deb -x "$package" /var/tmp/h5i-gpu-system-libs; done
export OPENSSL_DIR=/var/tmp/h5i-gpu-system-libs/usr OPENSSL_LIB_DIR=/var/tmp/h5i-gpu-system-libs/usr/lib/x86_64-linux-gnu
export CFLAGS=-I/var/tmp/h5i-gpu-system-libs/usr/include/x86_64-linux-gnu
export LIBRARY_PATH=/var/tmp/h5i-gpu-system-libs/usr/lib/x86_64-linux-gnu
cd "$source_dir"
USE_PREBUILT_LLVM=1 nice -n 19 taskset -c 64-127 cargo +nightly-2026-04-02 build --release --locked -p rustc_codegen_nvvm -j 8
