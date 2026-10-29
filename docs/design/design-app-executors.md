# h5i-app executors: RAM and CUDA

Two optional executors sit beside the default PostgreSQL engine. The RAM
executor serves read-only commands from an in-memory snapshot and writes
through to PostgreSQL. The CUDA executor runs pure Rust batch decisions on a
GPU. Neither adds a Lean theorem; both rest on tests and stated trust
obligations. Neither establishes a complete application request speedup.

## RAM executor

`h5i_app_mem::MemoryPgEngine` (`crates/h5i-app-mem`) is an opt-in,
single-process, single-owner executor over `h5i_app_pg::Engine`.

### Use

Opt in with `H5iApp<K, S, MemoryPgEngine<K, S>>`, using the two-argument
`with_executor` constructor and the app's existing `Store` and `Api`. The
default `H5iApp<K, S>` stays on the PostgreSQL engine. `Executor<K>` also
accepts the reference `MemoryEngine<K>`, whose keyed reply cache is volatile
and is not a durable deployment option.

All writes for a cached tenant must go through this one instance. External
SQL, other server instances and migrations require stopping it and recreating
its cache; there is no cross-process coherence protocol. Tenants stay in RAM
until the executor is dropped, so memory capacity is the operator's
responsibility.

### How it works

- Writes and keyed requests go to the PostgreSQL engine, with its SERIALIZABLE
  transactions, scoped idempotency records, locks and retries. Replies follow
  COMMIT, and PostgreSQL durability settings apply.
- A per-tenant write gate serializes a tenant's writes. Reads do not take it
  and are not blocked by a commit in flight; they see the snapshot as it was
  before that write.
- After COMMIT, the engine returns the write set of the attempt that actually
  committed. If the snapshot is warm, the executor applies it with `K::apply`
  under a brief snapshot write lock before replying. A cold snapshot stays
  empty and loads on the next read. Refusals leave the snapshot unchanged.
- A keyed replay drops the snapshot, which refills on the next read. An
  internal retry after an ambiguous COMMIT can itself observe a replay, so the
  executor does not assume a replay left the state unchanged.
- If a commit is cancelled or returns any executor-level error (key conflicts
  included), a drop guard poisons the tenant: its reads go to PostgreSQL from
  then on, and later successful writes do not re-enable the cache. Recreate
  the executor only after outstanding database transactions have drained.
- Read-only commands run the kernel transition on the cached snapshot, discard
  the proved-empty write set, and stamp the actor with the local system clock.

A lost response after commit has the same ambiguity as with the PostgreSQL
engine; use an idempotency key when replay matters.

### Trust boundary

The kernel proofs are unchanged. The read-only classification needs a
soundness proof such as `H5iAppLib.ReadOnly.Sound`; the calculator adapter
delegates to its extracted `read_only` and carries that proof. The storage
proof must relate the SQL writes to `K::apply` on the full snapshot, the same
obligation the PostgreSQL `Store` already has. Cache ownership, locking,
commit metadata, application of writes, time, Rust codegen and the PostgreSQL
storage contract are trusted shell obligations, not Lean theorems.

Limits:

- Only system-clock, non-monotonic deployments are supported. Database, fixed
  or manual clocks and monotonic time need a cache clock protocol first.
- A full snapshot can cost more CPU than a proved `Store::load_for`
  projection.
- Writes still serialize per tenant and pay `K::apply`, which clones the
  tenant's vectors. This is not a write-throughput optimization.

### Tests

Run from the `examples/app` workspace with `H5I_APP_TEST_DATABASE_URL` set to
a disposable database. The calculator `ram` integration test compares 100
committed writes and reads against `MemoryEngine`, counts store loads (one per
write, no refill for reads or enrichment), and covers refusals, keyed replay,
and a blocked-then-cancelled write that must leave the tenant on PostgreSQL.
A Conduit archive (branch `app-gpu-conduit`) passed 1,500-command
PG-versus-reference and RAM-versus-reference comparisons.

### Results

These were measured with an earlier locking scheme in which the tenant write
lock was held across the commit; the current write gate has not been
re-measured.

Conduit over HTTP (h5i-app-bench `performance/realworld/rust-h5i` kernel,
API, indexes and data: 50 users, 10,000 articles). One five-second
closed-loop run per cell on a shared dual EPYC 7502, CPUs 64-127 at nice 19,
16 Tokio workers, 8 clients, pool 32, PostgreSQL 16.15 with
`synchronous_commit`, `fsync` and `full_page_writes` on. Zero non-2xx replies.

| Command | Concurrency | PG req/s | RAM req/s | RAM / PG |
|---|---:|---:|---:|---:|
| get-article | 1 | 655.0 | 9065.5 | 13.84× |
| get-article | 16 | 6922.5 | 45823.1 | 6.62× |
| get-article | 64 | 14574.6 | 115314.4 | 7.91× |
| list | 1 | 132.1 | 326.6 | 2.47× |
| list | 16 | 2320.2 | 2553.0 | 1.10× |
| list | 64 | 6176.2 | 3053.8 | 0.49× |
| feed | 1 | 157.5 | 212.0 | 1.35× |
| feed | 16 | 2402.2 | 2845.2 | 1.18× |
| feed | 64 | 5421.3 | 3350.0 | 0.62× |
| favorite | 1 | 227.9 | 107.2 | 0.47× |
| favorite | 16 | 2360.4 | 92.5 | 0.04× |
| favorite | 64 | 4735.6 | 87.2 | 0.02× |

Repeated article reads gain 6.6-13.8×. Lists and feeds gain at low
concurrency and lose at 64, because running the kernel over the whole tenant
costs server CPU where PostgreSQL reads selectively. Favorites lose badly:
`K::apply` clones the tenant's vectors (article text included) under the write
lock, and reply enrichment clones the full snapshot. Mixed workloads, multiple
tenants, RAM footprint and long runs are unmeasured. For write-heavy traffic,
keep the PostgreSQL executor.

The calculator fixture (`examples/app/calculator/server/examples/executor_load.rs`)
calls the `Executor` directly, with no HTTP or authentication: 3 s per cell,
64 users per tenant, warm snapshots, separate tenants per executor.

| Command | Concurrency | PG calls/s | RAM calls/s | RAM / PG |
|---|---:|---:|---:|---:|
| get | 1 | 999.5 | 5,492,774.6 | 5495.52× |
| get | 16 | 9,862.0 | 1,038,500.8 | 105.30× |
| get | 64 | 17,954.0 | 1,027,234.3 | 57.21× |
| set+snapshot | 1 | 230.3 | 299.2 | 1.30× |
| set+snapshot | 16 | 378.2 | 315.7 | 0.83× |
| set+snapshot | 64 | 369.3 | 303.2 | 0.82× |

These rates measure database avoidance and lock overhead on tiny state; they
are not HTTP throughput.

## CUDA executor

The Rust-CUDA backend (`crates/h5i-app-gpu/backend-nvvm`) is experimental. It
is outside the extraction workspace, and no production GPU executor is
claimed. `crates/h5i-app-gpu/backend-oxide` is a cuda-oxide compile probe that
fails in upstream device codegen before producing PTX
(`crates/h5i-app-gpu/reproduce.sh`); it has no GPU results.

### API

- `batch::Backend<Input, Operation, Response>` is the execution interface,
  with no default application types. `batch::Cpu` wraps an app's pure Rust
  function.
- `runtime::execute_batch` selects CPU, required GPU, or an Auto policy with
  an explicit minimum batch size; Auto uses CPU when the GPU is absent or the
  batch is small. Errors after a backend is chosen propagate without CPU
  replay, and output count must equal input count.
- `repr::DeviceRepr` gives optional, fallible boundary conversions.
- `differential::compare` checks a candidate backend against a reference under
  an app-supplied comparison; `check_batch` checks cardinality, repeated
  execution and partitions.
- `backend-nvvm::Nvvm` owns the CUDA context, PTX module and stream. Its
  unsafe `map` handles transfer, launch, synchronization and readback for the
  `(input pointer, output pointer, usize row count)` ABI; apps wrap that
  contract in typed adapters. Multi-buffer or resident workloads can use
  `module()` and `stream()` with cust buffers directly. There is no scheduler
  or persistent-state abstraction.

GPU computations must not commit application state. Apps validate row-level
errors and invariants before the normal transition and commit.

Two example workloads:

- [`examples/app/parallel/gpu`](../../examples/app/parallel/gpu): bookmark
  search (ASCII case folding, up to eight AND/OR/NOT terms, substring match
  over 512-byte text, 64-tag mask) and push rules (byte globs, display-name
  substring, member bounds, sender permission). The device wrappers import the
  same decision source that Aeneas extracts from
  [`examples/app/parallel/kernel`](../../examples/app/parallel/kernel/src/lib.rs).
  Lean proves that disabled rules and insufficient permission deny, a missing
  required tag denies, and too many query terms fail closed; it does not prove
  full search or glob semantics.
- [`examples/app/discount-gpu`](../../examples/app/discount-gpu): checked
  integer pricing, `[u64; 3]` in and `[u64; 2]` out through `Nvvm::map`. It
  has no Lean proof.

The real linkding search benchmark lives in h5i-app-bench at
`performance/linkding/rust-h5i-gpu`.

### Setup

Pinned [Rust-GPU/rust-cuda](https://github.com/Rust-GPU/rust-cuda/tree/3a82c35a7196731fb60eebdac2a40d2b1bf0ba95)
at `3a82c35a7196731fb60eebdac2a40d2b1bf0ba95`, nightly-2026-04-02 with
rustc-dev, rust-src and llvm-tools-preview, prebuilt LLVM 7.1.0
(`USE_PREBUILT_LLVM=1`). Tested on an RTX 3090 (sm_86), CUDA toolkit and
libNVVM 12.6, driver 560.35.03. No root or Docker is needed.
`backend-nvvm/setup.sh` builds into `/var/tmp/h5i-gpu-target` and keeps
sources, toolchains and downloads under `/var/tmp`. It unpacks the OpenSSL,
ncurses and libxml2 dev packages into `/var/tmp/h5i-gpu-system-libs` and
passes GCC's builtin include path to bindgen. The lockfile pins `cuda_std`'s
glam to 0.30.10, since its unbounded `>=0.22` constraint otherwise picks a
release that breaks `cuda_std`.

CPU-only checks, no CUDA toolkit needed:

```sh
cargo test --release --manifest-path crates/h5i-app-gpu/Cargo.toml
cargo test --release --manifest-path examples/app/discount-gpu/Cargo.toml -p discount-gpu-example
```

With the pinned environment, from the repository root:

```sh
bash crates/h5i-app-gpu/backend-nvvm/setup.sh
bash examples/app/parallel/gpu/build-device.sh
bash examples/app/parallel/gpu/measure.sh --check-only   # conformance only; drop the flag to time
bash examples/app/discount-gpu/build-device.sh
bash examples/app/discount-gpu/check.sh
```

### Trust boundary

Lean checks the extracted search and push properties and the schema laws. It
does not prove GPU execution or CPU/GPU equivalence. Trusted: rustc, the NVVM
codegen backend, LLVM 7, libNVVM, PTX JIT, the CUDA driver and hardware, the
pointer/length ABI, packing, launch geometry, the bounds and error shell, and
the timing harness. Differential tests are evidence for the tested inputs
only.

Outputs are u32 decisions and per-thread errors (0 success, 1 missing input
row or row beyond the million-row capacity). A semantically rejected query is
a zero decision with error 0, as in the shared Rust function. The GPU does not
replace or sort any `KeyHash`; output order is input order.

The differential suite compares every output and error slot to the Rust CPU
oracle over randomized text, tags, permissions and members; AND/OR/NOT; valid
and rejected queries; wildcards up to 64-byte patterns; missing columns;
0/1/127/128/129/1025 rows; exactly the million-row capacity and one row past
it; split-batch independence; and schema `Bookmark`/`Event` SoA round trips
with non-sorted ids, plus a schema `Rule` driving a push batch. Host layout
tests cover 512/513-byte text, embedded separators and rejected trailing zero.
The discount example checks output and status equality, empty batches, block
boundaries, partition independence and determinism against a u128 CPU oracle.

### Results

Medians of three runs after one warm-up, GPU 0, CPUs 64-127 at nice 19.
Kernel time uses CUDA events. Total is wall clock including allocation, all
host-to-device copies, output initialization, launch, synchronization and
copies of both output columns back, with fresh pageable buffers per run. It
excludes module creation and JIT, context setup, fixture generation and SoA
packing. Rows are 512 bytes plus scalar columns. The CPU column is the same
decision source on one thread.

| Workload | Rows | Kernel ms | Total ms | GPU total rows/s | CPU 1 thread ms | Total speedup |
|---|---:|---:|---:|---:|---:|---:|
| Search | 10,000 | 6.243 | 6.860 | 1,457,746 | 809.614 | 118.02× |
| Search | 100,000 | 233.764 | 239.408 | 417,698 | 8112.959 | 33.89× |
| Search | 1,000,000 | 2932.031 | 3022.202 | 330,885 | 81206.593 | 26.87× |
| Push | 10,000 | 0.337 | 1.096 | 9,125,153 | 19.773 | 18.04× |
| Push | 100,000 | 5.929 | 13.874 | 7,207,690 | 207.097 | 14.93× |
| Push | 1,000,000 | 70.183 | 133.406 | 7,495,890 | 2075.482 | 15.56× |

Against an earlier, separate 16-thread CPU run, the GPU total is
10.60×/2.11×/1.64× for search and 3.98×/1.26×/1.32× for push at
10k/100k/1M rows. That is a cross-run comparison, not a controlled scaling
study. Push has much less headroom against parallel CPU than the single-core
numbers suggest, and these byte-by-byte toys are not an optimized CPU
baseline. Conduit's decision work is too small to benefit; in real requests
SQL, cloning, routing and transfer may dominate. Only workloads with enough
independent search or rule work per request, or that accept cross-request
batching, are candidates, and each needs its own end-to-end measurement.

With the backend added, `cargo app-verify --full` passed 42 of 42 gates
(project lints, Rust tests with PostgreSQL, dependency bans, eleven
extraction freshness checks, twelve Lean and axiom checks, five mutation
gates with every mutant caught).
