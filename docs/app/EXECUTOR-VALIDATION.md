# Executor experiment validation

All commands ran in the `app-gpu` checkout with release Cargo artifacts and
isolated caches under `/var/tmp`, at nice 19 on CPUs 64–127. Database tests
used only the disposable `h5i-gpu-pg` PostgreSQL instance on port 5465.

The original (pre-redirect) `cargo app-verify --full` finished with
**39 passed, 0 failed, 0 skipped**.
This includes Rust tests, dependency bans, lint, all ten extraction freshness
checks, all eleven existing Lean project checks and axiom gates, and all five
configured mutation gates. Every unchanged mutation baseline passed; every
mutant was caught, with none surviving or invalid. The first pass encountered
a stale local JSON Lake configuration; removing only its compiled configuration
cache allowed the final full run to pass without source changes.

The existing framework, HTTP, PG and application example Rust tests passed.
The preserved Conduit archive workspace's tests passed, including its schema-current
check. Three PostgreSQL integration tests passed: the existing 1,500-command
PG/reference comparison, a new 1,500-command RAM/reference comparison, and
RAM invalidation/idempotent replay/cancelled-write fallback coverage. The
last test blocks a write in PG, cancels its caller, and checks that subsequent
reads and later acknowledged commands cannot revive an uncertain cache.

The separate GPU workspace's default CPU test passed against the original
calculator arithmetic, including its error cases. In that first cuda-oxide experiment, device compilation failed;
it was not a CPU/GPU differential test. See [GPU.md](GPU.md) for the exact
compiler revisions, historical diagnostics and subsequent Rust-CUDA validation.

No existing theorem, axiom policy or generated Lean source was changed.
The new cache and executor shell have tests and documented trust obligations,
not new Lean proofs. The preserved Conduit kernel is unchanged from the reference,
and is now maintained on `app-gpu-conduit` and in the archive; its separate
proof project is not claimed as part of this checkout's proof gate.

Benchmark methodology, throughput, limitations and raw latency/counter logs
are linked from [MEMORY.md](MEMORY.md).

## Redirected experiment

Conduit source, harness and original numbers were preserved on the pushed
`app-gpu-conduit` branch at `61e5bb5` and copied into
`/var/tmp/h5i-gpu-conduit-archive` before removal from this branch. The archive
was rebuilt against the current framework; all its tests passed after the
committed-write cache update. No h5i-app-bench checkout was edited.

The replacement fixture uses the existing small calculator kernel. Its RAM test
compares 100 committed writes and reads against `MemoryEngine`, counts store
loads (one per write, no read/enrichment refill), tests refusals and keyed replay,
and blocks/cancels a transaction to verify permanent PG fallback. The CPU-only
GPU workspace tests capacity, schema-row round trip, input order and independent
batches. Neither these tests nor device compiler failures establish GPU
execution equivalence.

The new parallel toy has extracted search/push Rust and generated schema Lean,
with four handwritten denial properties and generated schema laws, checked
against the existing axiom allowlist. Existing proofs were not weakened and
no generated Lean was hand-edited. The calculator
read-only adapter delegates to its extracted classifier and has a new
`H5iAppLib.ReadOnly.Sound` proof; the original adapter's behavior is unchanged.
Cache ownership,
PG/cache synchronization, clocks and compilation remain shell trust obligations.

The deleted private Lake cache duplicated the pinned Aeneas/Mathlib dependency
versions in `/var/tmp/h5i-perf-lake`; subsequent checks use that shared cache.
The shared cached `h5i_app_lib` revision differed, so project dependencies remain
on this checkout's local `crates/h5i-app-core/proofs`, not that cached library.
Shared sources and other worktrees were not modified. Own artifacts occupy
approximately 8 GB, below the requested 15 GB budget.

## Final full gate

After the redirect, `cargo app-verify --full` completed with **42 passed,
0 failed, 0 skipped**: all twelve project lints, Rust tests (including real
PostgreSQL tests), dependency bans, eleven extraction freshness checks, twelve
Lean/axiom checks and five mutation gates. Every unmodified mutation baseline
passed; all five mutants were caught, with none surviving or invalid.
[Full gate output](measurements/verify-full.txt).

The separate GPU workspace's three CPU tests also pass, including the maximum
capacity acceptance, oversized capacity/text rejection, trailing-zero rejection,
schema-row round trip, input order, batch independence and calculator error tags.
At that stage cuda-oxide compilation remained blocked; no GPU differential or
timing result was claimed. The subsequent Rust-CUDA results are in GPU.md. The disposable `h5i-gpu-pg` container was stopped after these
checks; its data remains under `/var/tmp`. No other container was changed.

## Rust-CUDA backend validation (2026-10-16)

After adding the NVVM backend, `cargo app-verify --full` again completed with
**42 passed, 0 failed, 0 skipped**. All twelve project lints, Rust tests with
our PostgreSQL container, dependency bans, eleven extraction freshness checks,
twelve Lean/axiom checks and five mutation gates passed. Every unchanged
mutation baseline passed; all five mutants were caught, none survived or were
invalid. [Full gate output](measurements/verify-nvvm-full.txt).

The shared GPU crate's three CPU tests and the preserved oxide backend's
calculator test passed. Rust-CUDA compiled both slice-based wrappers to PTX.
The backend-generic GPU differential suite passed, including randomized inputs,
error columns, batch independence, million-row limits and schema Bookmark/Event
round trips with a schema Rule. [GPU report, timings and compiler setup](GPU.md).
The generated Lean change came from extraction and contains only source-line
updates; no theorem or axiom was added or weakened. Lean does not prove GPU
execution; rustc/NVVM/libNVVM, the driver and the launch/layout shell are trusted.

All new sources/caches/toolchains remain under `/var/tmp`; our files use about
11 GB, below the 15 GB budget. GPU 0 is idle after the run. Only our
`h5i-gpu-pg` container was started for these checks and stopped afterwards.

The bounded linkding NVVM path also passed `cargo app-verify --full`: **42
passed, 0 failed, 0 skipped**, with all five mutants caught. Existing extraction
freshness and Lean checks passed unchanged. Separately, all five GPU host crate
unit tests, the independent-original/CPU/GPU linkding differential suite, and
the existing search/push GPU differential suite passed. No existing theorem,
axiom set, or generated Lean file was changed.
[Full gate output](measurements/verify-linkding-full.txt),
[linkding differential output](gpu/linkding-differential.txt).
