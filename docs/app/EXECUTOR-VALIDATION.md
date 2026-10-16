# Executor experiment validation

All commands ran in the `app-gpu` checkout with release Cargo artifacts and
isolated caches under `/var/tmp`, at nice 19 on CPUs 64–127. Database tests
used only the disposable `h5i-gpu-pg` PostgreSQL instance on port 5465.

`cargo app-verify --full` finished with **39 passed, 0 failed, 0 skipped**.
This includes Rust tests, dependency bans, lint, all ten extraction freshness
checks, all twelve existing Lean project checks and axiom gates, and all five
configured mutation gates. Every unchanged mutation baseline passed; every
mutant was caught, with none surviving or invalid. The first pass encountered
a stale local JSON Lake configuration; removing only its compiled configuration
cache allowed the final full run to pass without source changes.

The existing framework, HTTP, PG and application example Rust tests passed.
The copied Conduit workspace's tests passed, including its schema-current
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
not new Lean proofs. The copied Conduit kernel is unchanged from the reference,
but only its generated schema fixture is included here; its separate proof
project is not duplicated or claimed as part of this checkout's proof gate.

Benchmark methodology, throughput, limitations and raw latency/counter logs
are linked from [MEMORY.md](MEMORY.md).
