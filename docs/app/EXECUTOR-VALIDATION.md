# Executor experiment validation

All commands ran in the `app-gpu` checkout with release Cargo artifacts and
isolated caches under `/var/tmp`, at nice 19 on CPUs 64–127. Database tests
used only the disposable `h5i-gpu-pg` PostgreSQL instance on port 5465.

The original (pre-redirect) `cargo app-verify --full` finished with
**39 passed, 0 failed, 0 skipped**.
This includes Rust tests, dependency bans, lint, all ten extraction freshness
checks, all twelve existing Lean project checks and axiom gates, and all five
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
calculator arithmetic, including its error cases. Device compilation failed;
it is not a CPU/GPU differential test. See [GPU.md](GPU.md) for the exact
compiler revisions, diagnostics and the unimplemented GPU validation work.

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
