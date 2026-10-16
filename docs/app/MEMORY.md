# RAM read executor

`h5i-app-mem::MemoryPgEngine` is an opt-in single-process, single-owner
executor. Existing PostgreSQL applications keep their existing engine.
All writes for each cached tenant **must** go through this one instance;
external SQL, other server instances and migrations require stopping it and
recreating its cache. There is no cross-process cache coherence protocol.

Writes and keyed requests use the existing PostgreSQL engine, including its
SERIALIZABLE transactions, scoped idempotency records, locks and retries.
Successful replies follow COMMIT. PostgreSQL durability settings still apply.
The cache is invalidated before a write is awaited, including on refusal.
Cancellation or any executor-level error (including key conflicts) disables RAM reads for that tenant for
the lifetime of this executor: a possibly in-flight COMMIT could otherwise
race a refill. Such reads use PostgreSQL directly. Recreate the executor only
after outstanding database transactions have drained. The next read loads the whole tenant in
a deferrable transaction. Cache loss needs no recovery beyond reloading PG.
A lost response after commit has the same ambiguity as the PostgreSQL engine;
use an idempotency key when replay matters.

A tenant read/write lock allows concurrent reads and orders them with writes. Read-only commands run the same
kernel transition on the cached snapshot, discard the proved-empty write set,
and stamp the actor with the local system clock. This implementation supports
system-clock, non-monotonic deployments only; database/fixed/manual clocks and
monotonic time need an extended cache clock protocol before use. Full snapshots
can cost more CPU than a proved `Store::load_for` projection. Tenants remain in
RAM until the executor is dropped; memory capacity is an operator obligation.

The existing kernel proofs are unchanged. The read-only classification relies
on `ReadOnly.Sound`. Cache ownership, locking, invalidation, time, Rust codegen
and the PG storage contract are trusted shell obligations, not Lean theorems.

Applications can opt in with `H5iApp<K, S, MemoryPgEngine<K, S>>`, using the
two-argument `with_executor` constructor and their existing `Store`/`Api`. The default
`H5iApp<K, S>` remains the PostgreSQL engine. `Executor<K>` also supports the
reference `MemoryEngine<K>`; its keyed reply cache is volatile and is not a
durable deployment alternative.

## Conduit measurement (2026-10-16)

Rust HTTP load test in `examples/conduit`, same kernel, API, indexes and PG
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
tenant snapshots. It is not a write-throughput optimization. Mixed writes
invalidate the cache and can erase read gains; mixed traffic, multiple tenants,
RAM footprint and longer steady-state runs remain unmeasured.

For h5i-app-bench, an explicitly separate `rust-h5i-mem` row can therefore
show useful read-heavy speedups under this single-owner durability model.
These numbers do not justify replacing its general PG-backed implementation.
The [CUDA experiment](GPU.md) produced no runnable GPU kernel and hence no
measured GPU speedup. Compute-heavy search, ranking or aggregation would need
a new scenario and a working compiler before a GPU claim is possible.
