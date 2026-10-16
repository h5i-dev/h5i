# RAM read executor

`h5i-app-mem::MemoryPgEngine` is an opt-in single-process, single-owner
executor. Existing PostgreSQL applications keep their existing engine.
All writes for each cached tenant **must** go through this one instance;
external SQL, other server instances and migrations require stopping it and
recreating its cache. There is no cross-process cache coherence protocol.

Writes and keyed requests use the existing PostgreSQL engine, including its
SERIALIZABLE transactions, scoped idempotency records, locks and retries.
Successful replies follow COMMIT. PostgreSQL durability settings still apply.
While holding the tenant write lock, the cache is disabled before awaiting
PostgreSQL. The PG engine returns the write set from the **actual committed
attempt**, after COMMIT, rather than recomputing the transition or its time.
For a warm cache, the shell applies that set with `K::apply` before releasing
the lock or replying. Refusals preserve the snapshot. A cold cache remains
lazy. A keyed replay invalidates the snapshot and refills on the next read:
an internal retry after an ambiguous COMMIT can itself observe a replay, so
assuming that a replay never changed cached state would be unsound.

Cancellation or any executor-level error (including key conflicts) permanently
disables RAM reads for that tenant in this executor: a possibly in-flight
COMMIT could otherwise race a refill. Such reads use PostgreSQL directly.
Later successful writes cannot re-enable the cache. Recreate the executor only
after outstanding database transactions have drained. A panic in `apply`
also leaves the cache disabled. Cache loss needs no recovery beyond reloading
PG in a deferrable transaction.
A lost response after commit has the same ambiguity as the PostgreSQL engine;
use an idempotency key when replay matters.

A tenant read/write lock allows concurrent reads and orders them with writes. Read-only commands run the same
kernel transition on the cached snapshot, discard the proved-empty write set,
and stamp the actor with the local system clock. This implementation supports
system-clock, non-monotonic deployments only; database/fixed/manual clocks and
monotonic time need an extended cache clock protocol before use. Full snapshots
can cost more CPU than a proved `Store::load_for` projection. Tenants remain in
RAM until the executor is dropped; memory capacity is an operator obligation.

The existing kernel proofs are unchanged. The read-only classification requires a soundness proof such as
`ReadOnly.Sound`. The calculator adapter delegates to its extracted
`read_only` and now carries this proof too.
The storage proof must relate the SQL writes to `K::apply` on the full snapshot;
this is the same obligation required of the underlying PostgreSQL Store. Cache ownership, locking, commit metadata, application of writes, time, Rust codegen
and the PG storage contract are trusted shell obligations, not Lean theorems.

Applications can opt in with `H5iApp<K, S, MemoryPgEngine<K, S>>`, using the
two-argument `with_executor` constructor and their existing `Store`/`Api`. The default
`H5iApp<K, S>` remains the PostgreSQL engine. `Executor<K>` also supports the
reference `MemoryEngine<K>`; its keyed reply cache is volatile and is not a
durable deployment alternative.

## Original Conduit measurement (2026-10-16, invalidate/refill version)

Rust HTTP load test preserved on [app-gpu-conduit](https://github.com/h5i-dev/h5i/tree/app-gpu-conduit/examples/conduit),
using the h5i-app-bench `performance/realworld/rust-h5i` kernel, API, indexes and PG
configuration for both executors. Each cell is one five-second closed-loop
run on a shared host, including the first cold-cache request; no confidence
interval or isolated-machine claim. Server: 16 Tokio workers; clients: 8;
PG pool: 32; CPUs 64–127, nice 19 (including the disposable PostgreSQL server).
Proof builds were paused during measurement. PostgreSQL used its default
container durability settings, without disabling synchronous commits or WAL.
Host: dual AMD EPYC 7502 (128 logical CPUs). PostgreSQL 16.15;
`synchronous_commit`, `fsync` and `full_page_writes` all on.
Data: 50 users, 10,000 articles, 20,000 tags, 5,000 favorites and 500 follows.
Scenarios are separate, not a mixed workload. Authentication uses pre-issued
JWTs; password hashing is outside these scenarios. Both modes use localhost
TCP HTTP; payload encoding and reply enrichment are included.

| Command | Concurrency | PG req/s | RAM req/s | RAM / PG |
|---|---:|---:|---:|---:|
| get-article | 1 | 486.0 | 8840.1 | 18.19× |
| get-article | 16 | 4748.7 | 44950.0 | 9.47× |
| get-article | 64 | 7987.0 | 105946.9 | 13.26× |
| list | 1 | 114.4 | 318.0 | 2.78× |
| list | 16 | 2053.7 | 2454.9 | 1.20× |
| list | 64 | 4498.0 | 3309.7 | 0.74× |
| feed | 1 | 86.8 | 200.3 | 2.31× |
| feed | 16 | 1622.0 | 2750.1 | 1.70× |
| feed | 64 | 5071.2 | 3120.8 | 0.62× |
| favorite | 1 | 196.7 | 178.1 | 0.91× |
| favorite | 16 | 1588.7 | 248.6 | 0.16× |
| favorite | 64 | 2537.0 | 247.1 | 0.10× |

All 24 cells had zero non-2xx replies. Full p50/p99 latency, attempt, retry
and commit counts: [PG](measurements/conduit-pg.txt),
[RAM](measurements/conduit-ram.txt). For get-article at c=1, p50 fell from
2.03 ms to 0.11 ms; at c=64, from 6.50 ms to 0.51 ms. RAM read scenarios had
zero PG command attempts (initial snapshot loads are not counted by that
counter). Favorite still commits every accepted request to PG.

This is a meaningful speedup for repeated article reads: 9.5–18.2× here.
Lists and feeds benefit at c=1 and c=16, but lose at c=64: running the existing
kernel on a whole tenant, counting and scanning/cloning rows consumes server
CPU, whereas PG uses selective reads. Favorites lose, especially at c=16/64:
this implementation serializes tenant writes and API enrichment clones full
tenant snapshots. It is not a write-throughput optimization. In this original version, mixed writes
invalidated the cache and could erase read gains; mixed traffic, multiple tenants,
RAM footprint and longer steady-state runs remain unmeasured.

For h5i-app-bench, an explicitly separate `rust-h5i-mem` row can therefore
show useful read-heavy speedups under this single-owner durability model.
These numbers do not justify replacing its general PG-backed implementation.
The [GPU experiment](GPU.md) now measures compute-heavy search and push-rule
toys through Rust-CUDA. It does not establish a Conduit request speedup;
cuda-oxide remains blocked. Transfer-inclusive million-row gains against the
earlier 16-thread CPU baseline are 1.64× search and 1.32× push rules.

## Conduit remeasurement after applying committed writes

The full-tenant reload after every accepted write was present in the original
implementation. The new implementation instead retains the actual committed
write set and applies it to a warm snapshot. The preserved Conduit archive
(`/var/tmp/h5i-gpu-conduit-archive`, sourced from `app-gpu-conduit`) was rebuilt
against the updated framework and remeasured using the same data and harness.
Its unchanged kernel/API remain outside this branch. The archive's 1,500-command
PG/reference and RAM/reference comparisons and cancellation/replay tests pass.
No builds or proofs from this experiment ran during this measurement. Other
host users were not isolated. These separate short runs cannot attribute changes
in PG throughput versus the earlier session to the framework change.

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

All cells returned zero non-2xx replies. Raw p50/p99 and engine counters:
[PG](measurements/conduit-committed-pg.txt),
[RAM](measurements/conduit-committed-ram.txt).

This fixes cache maintenance, but **does not fix Conduit write throughput**.
The generated `K::apply` clones the tenant's vectors (including article text)
and finds/updates affected rows. Favorite now pays that CPU cost under the tenant
write lock; HTTP reply enrichment also clones the full snapshot. Avoiding a
PG reload exchanges database work for expensive allocation/copying and serialized
CPU work. Favorites reach only 87–107 req/s here, versus PG's 228–4,736 req/s;
the original invalidate/refill RAM version reached 178–249 req/s in its earlier
session. An in-place or structurally shared cache needs a separate implementation
and equivalence argument; this experiment retains the kernel's actual `apply`.

Repeated get-article requests retain a meaningful 6.6–13.8× HTTP speedup.
List/feed gain at low concurrency, approach parity at c=16, and lose at c=64.
No end-to-end mixed-workload speedup is established. For h5i-app-bench, a separate
RAM row is useful for read-heavy scenarios under the documented single-owner
model, while write-heavy Conduit traffic should retain the PG executor.

## Calculator fixture (no real application in this branch)

`examples/app/calculator/server/examples/executor_load.rs` uses the existing
small calculator kernel and its proved Get classification. The direct Executor
load test runs three seconds per cell, 16 Tokio workers, PG pool 32, 64 users per
tenant, concurrency 1/16/64, CPUs 64–127 at nice 19. Each executor owns a separate
tenant; no baseline writes touch the RAM-owned tenant. Snapshots are warmed
before timing. Set includes `snapshot_for` enrichment, and RAM/PG snapshots are
compared after each RAM cell. No HTTP, JWT parsing or encoding is included.

| Command | Concurrency | PG calls/s | RAM calls/s | RAM / PG |
|---|---:|---:|---:|---:|
| get | 1 | 999.5 | 5,492,774.6 | 5495.52× |
| get | 16 | 9,862.0 | 1,038,500.8 | 105.30× |
| get | 64 | 17,954.0 | 1,027,234.3 | 57.21× |
| set+snapshot | 1 | 230.3 | 299.2 | 1.30× |
| set+snapshot | 16 | 378.2 | 315.7 | 0.83× |
| set+snapshot | 64 | 369.3 | 303.2 | 0.82× |

[Raw counts and snapshot-load counters](measurements/calculator-executor.txt).
RAM Get performs no PG loads. PG Set+snapshot loads twice per request; RAM
loads once for the transaction and updates its warm snapshot without refilling.
The integration test independently checks that invariant across 100 writes.

These tiny in-process reads mostly expose database avoidance and lock overhead;
the million-call rates must not be presented as HTTP throughput. Writes still
commit synchronously and serialize per tenant: avoiding one small snapshot load
gives 1.30× at c=1 but 0.83×/0.82× at c=16/64 here. Small toy state and large
Conduit state have very different cloning costs. Both measurements argue for
selective use of this executor, not a universal throughput improvement.
