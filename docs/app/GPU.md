# GPU backends: Rust-CUDA and cuda-oxide

Rust-CUDA (`backend-nvvm`) is the active experimental backend. cuda-oxide
(`backend-oxide`) remains a compile-probe backend for later completion.
Both are outside the extraction workspace. No production GPU executor is claimed.

## Rust-CUDA setup

Pinned upstream [Rust-GPU/rust-cuda](https://github.com/Rust-GPU/rust-cuda/tree/3a82c35a7196731fb60eebdac2a40d2b1bf0ba95),
revision `3a82c35a7196731fb60eebdac2a40d2b1bf0ba95`, nightly-2026-04-02
with rustc-dev, rust-src and llvm-tools-preview. Device: RTX 3090 (sm_86),
CUDA toolkit/libNVVM 12.6, driver 560.35.03, GPU 0 only.
The documented prebuilt route (`USE_PREBUILT_LLVM=1`) downloaded LLVM 7.1.0
from upstream's rustc_codegen_nvvm-llvm release. No root or Docker was needed.

Reproduction scripts: `crates/h5i-app-gpu/backend-nvvm/setup.sh` and
`build-device.sh`. Everything builds into `/var/tmp/h5i-gpu-target`; sources,
Cargo/Rustup homes, downloaded packages and temporary files stay in `/var/tmp`.
Ubuntu packages libssl-dev/libssl3t64, libncurses-dev/libtinfo6 and
libxml2-dev/libxml2 were unpacked with dpkg-deb into
`/var/tmp/h5i-gpu-system-libs`. They supply OpenSSL headers plus terminal/XML
link libraries. GCC 13's builtin include path is supplied to bindgen;
OpenSSL's multiarch include path is supplied through CFLAGS.
The lockfile pins cuda_std's glam to 0.30.10: its unbounded `>=0.22`
constraint otherwise selected 0.34.1, whose missing root UVec2/UVec3 exports
broke cuda_std. No upstream compiler source patch was needed.

Both natural slice-based decision wrappers compiled to PTX and ran on GPU 0.
They import the exact CPU/Aeneas decision source, excluding only schema host
adapters with target_arch cfg. They do not allocate or return Result; no
fixed-array fallback or alternate implementation was needed.

## Trust boundary

Lean checks the existing extracted search/push properties and schema laws.
It does not prove GPU execution or CPU/GPU equivalence. rustc, the NVVM
codegen backend, LLVM 7, libNVVM, PTX JIT, CUDA driver and hardware are trusted,
as are the pointer/length ABI, packing, launch geometry, bounds/error shell,
and timing harness. Differential tests provide evidence for tested inputs only.
The output columns are u32 values and per-thread errors (0 success, 1 missing
input row or row beyond the million-row capacity). Semantic query rejection
remains a zero decision with error 0, matching the shared Rust function.

## GPU differential checks and measurements

The backend-generic suite compares every u32 output/error slot to the Rust CPU
oracle. It covers deterministic randomized text/tags/permissions/members,
AND/OR/NOT, valid and rejected queries, wildcards including 64-byte patterns,
missing columns, 0/1/127/128/129/1025 rows, exact million-row capacity and
one row beyond it, split-batch independence, and schema Bookmark/Event SoA round trips
with non-sorted ids, plus a schema Rule driving a GPU push batch. Existing host layout tests cover 512/513-byte text,
embedded separators and rejected trailing zero. No KeyHash implementation is
replaced or sorted by the GPU; output indices preserve input order.
These are empirical tests, not a GPU theorem or exhaustive absence-of-panic proof.

Commands: run setup.sh, build-device.sh, then measure.sh in backend-nvvm.
Detach long runs and poll logs. `measure PTX --check-only` runs just conformance.
All builds/tests/measurements used nice 19 and CPUs 64–127; only GPU 0 was used.
CPU baseline and GPU measurements were sequential, with our proofs/builds paused.
One warm-up is excluded, followed by three repetitions; table reports medians.
Module creation/JIT, context setup, CPU fixture generation and SoA packing are
excluded. Kernel time uses CUDA events; total uses wall clock and includes
allocation, all H2D input copies, output initialization, launch, synchronization,
and D2H copies of both output columns. Allocations are fresh per run, using
ordinary pageable host buffers. It excludes final destruction of the text/output
device buffers. Inputs are 512 bytes/row plus per-row scalar columns; push also
has 64-byte pattern and display columns. Production packing/DB work is additional.

| Workload | Rows | Kernel ms | Total ms | GPU total rows/s | CPU 1 thread ms | Total speedup |
|---|---:|---:|---:|---:|---:|---:|
| Search | 10,000 | 6.243 | 6.860 | 1,457,746 | 809.614 | 118.02× |
| Search | 100,000 | 233.764 | 239.408 | 417,698 | 8112.959 | 33.89× |
| Search | 1,000,000 | 2932.031 | 3022.202 | 330,885 | 81206.593 | 26.87× |
| Push | 10,000 | 0.337 | 1.096 | 9,125,153 | 19.773 | 18.04× |
| Push | 100,000 | 5.929 | 13.874 | 7,207,690 | 207.097 | 14.93× |
| Push | 1,000,000 | 70.183 | 133.406 | 7,495,890 | 2075.482 | 15.56× |

Device build and conformance logs: [PTX build](gpu/nvvm-device-build.txt),
[differential checks](gpu/nvvm-differential.txt).

Raw repetitions/checksums: [parallel-nvvm.csv](measurements/parallel-nvvm.csv).
CPU is the same decision source on all padded rows, with output/error allocation.
The earlier cache-hot repeated-row [CPU baseline](measurements/parallel-cpu.csv)
used 1 and 16 threads; its million-row search was 78.142/4.948 seconds,
push 2.545/0.176 seconds. Relative to its 16-thread results, the new GPU total
at 10k/100k/1M is search **10.60× / 2.11× / 1.64×**, push
**3.98× / 1.26× / 1.32×**. These are cross-run comparisons with slightly different
host iteration/layout overhead and compiler versions, not a controlled CPU
parallel scaling study. No application request speedup is inferred from them.

This establishes a useful prototype path for large compute-heavy batches,
especially search. It does not establish a Conduit speedup: its decision work
was too small, and SQL, cloning, routing and transfer/packing may dominate real
requests. Push has much less headroom against parallel CPU than the single-core
comparison suggests. For h5i-app-bench, only workloads that can expose enough
independent search/rule work per request (or accept cross-request batching) are
candidates; a real integration needs its own complete request measurements.

## Framework verification

`cargo app-verify --full`: **42 passed, 0 failed, 0 skipped**, including all
extraction freshness, Lean/axiom and mutation gates. All five mutants were
caught and every unchanged baseline passed. The shared GPU CPU tests and
preserved oxide calculator test pass separately. See the
[validation record](EXECUTOR-VALIDATION.md) and
[full gate output](measurements/verify-nvvm-full.txt).
No new axiom, sorry, theorem weakening or manually edited generated Lean was used.

## Historical cuda-oxide feasibility report

The remaining sections describe the earlier cuda-oxide experiment, which
stopped before device execution. Paths to its probes now start at
`crates/h5i-app-gpu/backend-oxide/`. These failures do not describe Rust-CUDA.

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

## Earlier calculator / Result failures

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

## Search and push-rule prototypes

The redirected experiment uses the small, separately proved
[`parallel` example](../../examples/app/parallel/kernel/src/lib.rs), rather
than importing a real application. Both decision functions allocate nothing
and return `bool` / `u32`:

* Bookmark search: ASCII case folding, up to eight terms with AND/OR/NOT,
  substring matching over zero-separated title/description/url/notes bytes,
  and a 64-tag membership mask.
* Push rules: byte globs (`*`, `?`), display-name substring, inclusive room
  member bounds, and sender permission; one flattened (event, rule) per row.
  This is a deliberately small model, without Matrix JSON lookup, Unicode
  word boundaries, actions or full protocol semantics.

Aeneas extracts these actual Rust functions. The checked theorems establish
that disabled rules and insufficient permission deny, a missing required tag
denies, and excessive query term counts fail closed. These are meaningful
safety properties, not a proof of complete search/glob semantics or GPU code.
The schema fixture is generated by `schema!`; its Lean laws build too.

The thin `search_batch` / `push_batch` wrappers are behind the `gpu` feature
(and their respective probe features), outside Charon's workspace. They call
those same Rust decision functions, with a million-row bound, fixed-width
512-byte text columns, 64-byte push pattern/display columns, and per-thread
output/error slots. The host search SoA adapter is type-checked against the
schema's `Bookmark` and tests row capacity, 512/513-byte boundaries, round trip,
input order and independent CPU batches. Its padding format rejects trailing
zero bytes so round trip is unambiguous. These are **CPU layout tests**, not a
GPU differential suite or a proof of KeyHash ordering.

Both new device compilations fail before producing PTX, at creation of a
bounded byte-slice view:

```
Unsupported construct: Aggregate kind RawPtr(Ty { ...
  RigidTy(Slice(... Uint(U8))) ... }, Not) not yet supported
```

The backtrace names `core/src/slice/index.rs` and the corresponding batch
wrapper. [`examples/slice_probe.rs`](../../crates/h5i-app-gpu/backend-oxide/examples/slice_probe.rs)
reduces this to `decision(&bytes[0..16])`, returning a boolean, with the same
failure. This does not establish that scalar booleans are unsupported: the
observed boundary is slice-pointer aggregate lowering in this compiler revision.
See [search](gpu/search-failure.txt), [push](gpu/push-failure.txt) and
[minimal slice diagnostic](gpu/slice-failure.txt). No upstream changes, alternate backend or handwritten twin kernel were used
in that earlier experiment.

Reproduce with `bash crates/h5i-app-gpu/reproduce.sh search` (or `push`, `slice`,
`result`, `calculator`), detached for long runs as required by this workspace.
The script installs the pinned nightly and builds the backend in isolated
`/var/tmp` directories. Preserved oxide CPU tests use
`cargo test --release --manifest-path crates/h5i-app-gpu/backend-oxide/Cargo.toml`.

## Historical cuda-oxide performance and verification boundary

Because neither cuda-oxide target compiles, that backend has **no GPU throughput, kernel-only
timing, H2D-plus-launch timing, GPU panic validation, or executable CPU/GPU
differential test** at 10k, 100k or 1M rows. The stopping point is the requested
minimal compiler reproduction. A successful future device compiler must pass
those differential checks, capacity limits, batch independence, layout and
hash-order checks before any equivalence claim. rustc, cuda-oxide, its driver
and layout/launch shell would remain trusted; Lean checks alone do not prove
GPU execution equivalent.

CPU reference measurements follow below. They measure just repeated decision
calls, not HTTP, database work, allocation, SoA packing or transfers. For each
N, identical representative rows are reused (a cache-hot compute baseline);
16 workers distribute independent rows. Search uses eight nonmatching
12-byte terms against a 512-byte row; push exercises a matching glob and display
condition. Results include checksums. This is a shared EPYC 7502 host, CPUs
64–127 at nice 19; no claim of isolated-machine precision. The CPU baseline
is not evidence of an end-to-end linkding or Matrix speedup. These byte-by-byte
toy algorithms are not an optimized SIMD/search-library CPU baseline.

For h5i-app-bench, the cuda-oxide backend provides **zero usable GPU scenarios
and no measured GPU speedup**. Rust-CUDA results are reported above. These workloads expose substantial compute compared with the
calculator, but a speedup remains unestablished until the compiler works and
transfer-inclusive tests run. Step 1's measured HTTP results, including write
regressions after cache maintenance, are in [MEMORY.md](MEMORY.md).

### CPU reference results (2026-10-16)

Historical CPU throughput in rows/s; cuda-oxide GPU timings were unavailable
because PTX generation failed. Rust-CUDA timings are in the table above.

| N | Search, 1 CPU thread | Search, 16 threads | Push, 1 CPU thread | Push, 16 threads |
|---:|---:|---:|---:|---:|
| 10,000 | 12,821 | 137,569 | 270,897 | 2,295,015 |
| 100,000 | 12,750 | 198,224 | 379,241 | 5,734,905 |
| 1,000,000 | 12,797 | 202,115 | 392,975 | 5,694,935 |

[Raw CPU timings and checksums](measurements/parallel-cpu.csv).
At 1M rows, search takes 78.14 s on one thread and 4.95 s on 16; push takes
2.54 s and 0.176 s respectively. CPU parallelism itself helps these models;
no GPU advantage follows from that observation.

## linkding search (real predicate)

The NVVM backend now has a separate `linkding_batch` path. The parser and an
independent host oracle are copied from h5i-app-bench's
`performance/linkding/rust-h5i/kernel/src/search.rs` (read-only reference
`/var/tmp/h5iab-ld-engine`, source revision
`0d0f736935496770510b7a07e89c5cb6e278bd18`). The original parser runs once per query on the host.
It serializes postfix nodes into kind/text-offset/text-length/left/right words,
concatenated text bytes, root, and empty/failed flags. The allocation-free
[`linkding.rs`](../../examples/app/parallel/kernel/src/linkding.rs) is the same
plain Rust source compiled for CPU and device. Only the device wrapper has
`#[kernel]`; it is outside the Charon input. No new Lean theorem or GPU/compiler
correctness proof is claimed.

This preserves terms, exact case-insensitive tags, lax term-as-tag matching,
unknown keywords (true), unread/untagged flags, quotes, implicit AND, AND/OR/NOT,
empty queries (true), and parse failures (false). Like the original, every AST
node is evaluated; Boolean operators do not skip evaluation of their subtrees.
There are four separate bounded text fields per row, with explicit lengths;
terms cannot match across a field boundary. Embedded NUL bytes are preserved.
The column buffers contain text slots (four 128-byte subslots per row), four
lengths, eight tag hashes, and unread/untagged/tag-count metadata. Capacity is
256 AST nodes, 128 bytes per text field, eight tags, and one million rows.
Over-capacity inputs and malformed layouts return value=0/error=1 per row;
a legitimate negative match returns value=0/error=0. Packing marks oversized
bookmarks invalid rather than silently truncating their search semantics.

Tags use ASCII-folded FNV-1a u64 hashes, computed on host for bookmarks and by
the shared evaluator for query leaves. This deliberately replaces exact tag-byte
comparison with hash equality: collisions can produce false positives, including
for lax terms. Tag order does not affect membership. There is no collision
resolution; applications requiring adversarial collision safety need an exact
byte comparison or a collision-free host interning protocol before deployment.
These bounds also exclude some otherwise valid linkding bookmarks. This is a
bounded scan prototype, not a replacement for the full linkding HTTP service.

Reproduce with `backend-nvvm/build-device.sh`, then
`backend-nvvm/measure-linkding.sh` from `crates/h5i-app-gpu`. The latter runs
independent-original/CPU/GPU comparisons before timing. Checks cover randomized
Boolean queries and bookmarks, tags and lax mode, quoted terms, keywords,
parse errors, empty queries, 128/129-row splitting, field separation,
embedded zeros, 128/129-byte fields, tag/node/row capacities, malformed ASTs and
truncated column buffers. CPU unit tests also check exact-width packing and
case-folded hash/tag-order behavior. All outputs and error slots are compared.
[Differential and CPU test log](gpu/linkding-differential.txt);
[device build log](gpu/linkding-device-build.txt).

Measurements below are medians of three runs on GPU 0 (RTX 3090), with CPU
work pinned to CPUs 64–127 at nice 19. Rows use the exact searchable fields,
flags and tag names from linkding `bench/seed.sql`: title `alpha i` every 50th
row, otherwise `bookmark i`, empty description/notes, URL
`https://example.com/b/i`, two `tagN` names, unread every seventh row. The same
seed pattern is extended to 1M. Queries are strict (lax=false): `alpha`, and the
eight-term query `(alpha or beta) and not (missing or absent) and (gamma or
delta) and not (unknown and zeta)`. The heavy query deliberately evaluates all
eight leaves, even though the seeded rows produce no final matches.

| Query | Rows | CPU 1 ms | CPU 16 ms | GPU kernel ms | GPU resident total ms | GPU upload each call ms | Resident M rows/s |
|---|---:|---:|---:|---:|---:|---:|---:|
| alpha | 10k | 4.732 | 1.676 | 0.033 | 0.118 | 0.783 | 84.6 |
| alpha | 100k | 40.334 | 5.347 | 0.167 | 0.391 | 10.052 | 255.5 |
| alpha | 1M | 349.505 | 35.392 | 1.539 | 7.205 | 75.252 | 138.8 |
| eight terms | 10k | 24.745 | 4.590 | 0.181 | 0.289 | 1.107 | 34.6 |
| eight terms | 100k | 268.364 | 28.226 | 5.432 | 5.700 | 17.122 | 17.5 |
| eight terms | 1M | 2757.438 | 188.831 | 65.819 | 69.965 | 133.420 | 14.3 |

[Raw measurements](measurements/linkding-nvvm.csv). The one-time row upload and
output-buffer allocation took 0.905 / 6.409 / 73.670 ms at 10k / 100k / 1M.
Resident total includes AST/text H2D, launch, synchronization, and full value
and error arrays D2H (8 bytes/row), with rows and device output buffers reused.
Upload-each-call includes a fresh upload and output-buffer allocation as well.
Upload-each-call and CPU-16 measurements also include a host output-equality
check against the expected vector.
Context initialization/module JIT, host parsing/packing, and final resource
teardown are excluded. CPU timings use the same bounded evaluator and include
output allocation, thread creation/join and partitioning into 1 or 16 chunks.
These are scans, not HTTP latency or end-to-end request throughput.

The supplied same-host historical 100k `alpha` baselines are PostgreSQL ILIKE
across four columns **213 ms**, and the original resident predicate **89 ms**
(1 thread), **10.6 ms** (16), **7.1 ms** (32). These historical values were
provided with this task, not remeasured here. The bounded CPU implementation
has different allocation/layout costs, so use the new same-data CPU/GPU table
for the direct comparison. Against the supplied Postgres baseline, residency
already explains about **20–30×** with the original parallel CPU predicate;
the new bounded CPU scan reaches **40×**. The resident GPU scan is about
**545×** that historical Postgres time, and adds **13.7×** over the new
16-thread CPU scan at 100k. These ratios do not predict a 545× API speedup.

In this measured range resident GPU wins for both query weights: versus the
same 16-thread CPU path, alpha is **14.2× / 13.7× / 4.9×** faster at
10k/100k/1M, and eight terms **15.9× / 5.0× / 2.7×**. Uploading every call
loses to the parallel CPU for alpha at 100k and 1M; the heavy query still wins
there by about **1.65× / 1.42×**. Thus keeping rows resident is the main
architectural requirement, with a further measurable GPU advantage for these
bounded scans. Update/upload maintenance, snapshot consistency, tenant
isolation, pagination, serialization, concurrent request scheduling and the
full API remain unmeasured. Rust-CUDA compiled this predicate without a
construct failure or handwritten alternate algorithm; cuda-oxide remains the
separate deferred backend.
