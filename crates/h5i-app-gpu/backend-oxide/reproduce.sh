#!/usr/bin/env bash
# Expected to fail in upstream device codegen; see docs/app/GPU.md.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
export CARGO_HOME=/var/tmp/h5i-gpu-cargo-home
export CARGO_TARGET_DIR=/var/tmp/h5i-gpu-target
export RUSTUP_HOME=/var/tmp/h5i-gpu-rustup
export TMPDIR=/var/tmp CUDA_VISIBLE_DEVICES=0
export PATH=/usr/local/cuda/bin:$PATH
export BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/13/include
source_dir=/var/tmp/h5i-gpu-cuda-rust
rev=4a56e4220aab8ce5d085a411e7f806cebb647d14
if [[ ! -d "$source_dir/.git" ]]; then git clone https://github.com/NVIDIA/cuda-rust "$source_dir"; fi
git -C "$source_dir" checkout --detach "$rev"
nice -n 19 taskset -c 64-127 rustup toolchain install --no-self-update nightly-2026-04-03 --profile minimal --component rust-src,rustc-dev,llvm-tools
export LIBRARY_PATH=$RUSTUP_HOME/toolchains/nightly-2026-04-03-x86_64-unknown-linux-gnu/lib
export LD_LIBRARY_PATH=$LIBRARY_PATH:/usr/local/cuda/lib64
# Backend has a separate workspace. Pin its otherwise floating Pliron dependency.
cp "$repo/crates/h5i-app-gpu/backend-oxide/backend-Cargo.lock" "$source_dir/crates/rustc-codegen-cuda/Cargo.lock"
(cd "$source_dir/crates/rustc-codegen-cuda"; nice -n 19 taskset -c 64-127 cargo +nightly-2026-04-03 build --release --locked)
export CUDA_OXIDE_LLC=$RUSTUP_HOME/toolchains/nightly-2026-04-03-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin/llc
export CUDA_OXIDE_ARCH=sm_86 CUDA_OXIDE_TARGET=sm_86
export RUSTFLAGS="-Z codegen-backend=$CARGO_TARGET_DIR/release/librustc_codegen_cuda.so -C opt-level=3 -C debug-assertions=off -Z mir-enable-passes=-JumpThreading -Csymbol-mangling-version=v0"
case "${1:-result}" in
 slice) args=(--features gpu --example slice_probe);;
 result) args=(--features gpu --example result_probe);;
 search) args=(--features search-probe --lib);;
 push) args=(--features push-probe --lib);;
 calculator) args=(--features calculator-probe --lib);;
 *) echo 'Usage: reproduce.sh [result|calculator|search|push|slice]' >&2; exit 2;;
esac
cd "$repo"
exec nice -n 19 taskset -c 64-127 cargo +nightly-2026-04-03 build --release --manifest-path crates/h5i-app-gpu/backend-oxide/Cargo.toml "${args[@]}"
