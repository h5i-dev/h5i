# CUDA prototype: blocked in device compilation

No GPU executor or GPU performance result is claimed. Work stopped at the
requested compiler feasibility boundary. No CubeCL, translated twin kernel,
new axiom, or change to existing kernel proofs was introduced.

The machine had four RTX 3090 GPUs (sm_86), driver 560.35.03 and CUDA 12.6.
GPU 0 was idle when checked; all CUDA commands selected CUDA_VISIBLE_DEVICES=0.
No device kernel launched.

## Tested compiler

The initial upstream checkout was NVIDIA/cuda-rust
`e84e5b3d74014fcca36ffcf1b8d603c197cc7c8f`; its SIMT installation requirements
had moved to CUDA 13 / driver 580. The experiment therefore used upstream
**v0.1.0**, commit `4a56e4220aab8ce5d085a411e7f806cebb647d14`, whose installation
page advertises CUDA 12.x and driver 545+. Toolchain:
`nightly-2026-04-03`, rustc-dev, rust-src, llvm-tools; LLVM llc shipped with that
nightly; sm_86; Pliron `733b54212a24fd463d81bc2cf7424bcfaf811f5d` (0.14.0).
The backend's resolved lockfile is retained with the probe.

The normal `cargo-oxide` installation first needed GCC's builtin include path
for bindgen (`stddef.h`). After that, cuda-core failed to compile against the
installed CUDA 12.6 headers:

```
error[E0425]: cannot find function `cuEventElapsedTime_v2` in crate `cuda_bindings`
```

The generated bindings contain `cuEventElapsedTime`. Upstream was not patched.
Building `crates/rustc-codegen-cuda` directly succeeded, permitting an isolated
device compilation probe without the failing host runtime.

## Device failures

`crates/h5i-app-gpu` is a separate workspace/toolchain invocation, excluded from
normal extraction and builds. Its default CPU test calls the existing
calculator kernel's `compute`, including checked arithmetic errors. The
`calculator-probe` feature adds a thin `#[kernel]` batch wrapper with separate
input, value and per-thread error columns; its arithmetic calls that same
calculator source. Compilation panicked while lowering calls from MIR to LLVM:

```
Operation with use(s) being erased
pliron/src/operation.rs:594
mir_lower::convert::ops::call::convert
```

A smaller reproduction in `examples/result_probe.rs` has a function returning
`Result<u64, Failure>`, where `Failure::Overflow(u64)` has a payload. Matching
the error payload in a kernel fails verification:

```
MirEnumPayloadOp operand must be an enum type
mir.enum_payload ... mir.struct <Failure,...>
```

See [calculator diagnostic](gpu/calculator-failure.txt) and
[minimal diagnostic](gpu/result-failure.txt). This establishes a failure for
these particular programs and revisions, not that every Result or enum is
unsupported. Vec<u8> was not independently tested after this stopping point.
A small local CudaKernel marker trait permits compile-only macro expansion;
it contains no kernel implementation and is never used to launch anything.

Reproduce on this machine with
`setsid nohup bash crates/h5i-app-gpu/reproduce.sh result > /var/tmp/h5i-gpu-repro.log 2>&1 &`
(or `calculator`). It reinstalls the isolated nightly under /var/tmp, and needs
several GB free. The nightly was removed after recording the failures to make
room for the unchanged Lean gate. Run the CPU check with
`cargo test --release --manifest-path crates/h5i-app-gpu/Cargo.toml` using the
same CARGO_HOME/CARGO_TARGET_DIR constraints as the rest of this experiment.

## Verification and performance boundary

No schema-derived SoA, fixed-capacity arena implementation, batch scheduler,
outbox protocol, CPU/GPU differential suite or device throughput measurement
was completed after the compiler failure. In particular, capacity boundaries,
panic mapping, batch independence, SoA round trips and KeyHash ordering are
**not GPU-validated**. A successful future prototype must implement and test
all of these before it can serve application requests. The CUDA code generator
and driver would be additional trusted components, like rustc; the unchanged
Lean proofs alone would not establish equivalence of GPU execution.

For h5i-app-bench, Step 2 currently gives **zero usable GPU scenarios** and no
measured speedup. Step 1's Conduit measurements are in [MEMORY.md](MEMORY.md).
They isolate database avoidance without claiming that CUDA can accelerate the
remaining work. Search/ranking/large aggregations remain hypotheses requiring
both a working single-source compiler and measurements; arithmetic-only
calculator throughput would not establish an HTTP API speedup.
