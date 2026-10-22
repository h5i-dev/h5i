# Integer pricing CUDA example

A second workload for the application-independent GPU adapter: unit price,
quantity and discount percentage become a total plus an explicit status code.
Overflow and invalid percentages are preserved as results. CPU and device
entry points compile the same `src/kernel.rs`; host tests use a u128 arithmetic
oracle. This small execution example has no Lean proof.

From the repository root, CPU-only:

```sh
cargo test --release --manifest-path examples/app/discount-gpu/Cargo.toml -p discount-gpu-example
cargo run --release --manifest-path examples/app/discount-gpu/Cargo.toml -p discount-gpu-example
```

With the pinned Rust-CUDA environment from `docs/app/GPU.md` installed:

```sh
bash examples/app/discount-gpu/build-device.sh
bash examples/app/discount-gpu/check.sh
```

The explicit GPU check fails if CUDA or PTX is unavailable. It checks complete
output/status equality, empty batches, block boundaries, repeat determinism,
split batches and missing-entry errors. `Quotes` is the application's typed
launch adapter; `Nvvm::map` supplies allocation, transfer, launch and readback.
