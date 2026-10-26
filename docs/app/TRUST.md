# What h5i-app proves, and what it assumes

## Proven in Lean

Charon and Aeneas translate the Rust to Lean; every theorem is about the
extracted code. Covered: the kernel's `transition` and `apply` (example:
`examples/app/calculator/proofs/Spec.lean`, theorems in `Proofs.lean`), the token
parser and encoder, the JSON writer, and the storage path:

| Code | Proven | Where |
|---|---|---|
| `sql_writes`, generated table operations | store what `apply` computes | `Storage.lean`, `H5iAppLib.Store` |
| `h5i_app_sql::plan` | computes `planA` | `crates/h5i-app-sql/proofs` |
| `h5i_app_pgsql::valid`, `create`, `select`, `compile`, `render` | compute `Pg.createA`, `Pg.selectA`, `Pg.compileA` and print `Pg.render`; a quoted name reads back as itself (`Pg.lexName_quote`). Under `H5iAppLib.Pg`, a compiled statement does to the tenant's rows what `H5iAppLib.Sql.exec` says and touches no other tenant or table (`Pg.compile_sound`); a compiled `SELECT` returns the tenant's matching rows once each (`Pg.select_sound`, giving `Lists` and `Sel`) | `crates/h5i-app-pgsql/proofs` |
| `H5iAppLib.Query` (declarative reads) | meaning of `h5i_app_sql::Read`/`Pred`: filters (column equality, `IN` a column of another read, `and`, `or`) and a key-ordered window (`offset`, `limit`, ascending or descending), over the tenant's tables in key order; lemmas for frame theorems (`find?_slice`, `any_slice`, `filter_slice`, `rows_enc`, `page_of_newest`). Not proved: that `h5i_app_pgsql::compile_read` means the same (differential-tested, below) | `crates/h5i-app-core/proofs/H5iAppLib/Query.lean` |
| generated `PgServed`, `pg_loaded_inv` | an invariant kept by accepted write sets holds in every state of a multi-tenant database and every snapshot loaded from it | `generated/Schema.lean` |
| `h5i_app_std::authz::guard`, `guard_viewed` | for every kernel and policy: a committed command's writes are permitted, each in the state the writes before it leave, and its disclosed rows stored as disclosed and visible; read-only commands under `guard_viewed` are noninterfering with what the view hides, and `ReadOnly.Sound`. An app opts in with `[authz]`; its theorem is generated ([`AUTHZ.md`](AUTHZ.md)) | `crates/h5i-app-std/proofs/AuthzSpecs.lean`, `generated/Authz.lean` |

An app with `[authz]` gets this link generated (`Committed.pg_stored` in
`generated/Authz.lean`: the kernel's `sql_writes` and `decode` store and
load what its `apply` computes), with no invariant beyond it; that its
server's `Store` calls them, with the default full-tenant `load_for`, is
checked by `h5i app lint`, not proven. Each server app with hand-written proofs applies
`pg_loaded_inv` to its extracted `transition`,
`sql_writes` and `decode` as `db_inv` (docs: `Database.db_inv`, which also
covers scoped loads). It says: for the server's schema and any tenant, every
database reached by loading with the compiled `SELECT`s, running a successful
`transition` and storing its compiled writes, among other tenants' compiled
statements, holds a state satisfying `Inv`, and every later load decodes to a
snapshot satisfying `Inv`.

### Declarative reads: what is tested, not proved

`h5i_app_pgsql::compile_read` turns a `Read` into one `SELECT` and is not
extracted. Its meaning is checked in two steps against
`h5i_app_sql::eval`, a reference evaluator that follows the Lean
definitions clause by clause:

- `crates/h5i-app-pg/src/reads_test.rs` runs 3,200 random reads (nested
  `IN`, windows in both directions, `NULL`s, a second tenant's rows as
  noise) over random tables on PostgreSQL through `load_read` and
  `count_read`, and compares rows and counts with the evaluator;
- `crates/h5i-app-sql/tests/query_cases.rs` writes 200 random reads and
  the evaluator's answers as `#guard`s on `H5iAppLib.Query.Read.rows`
  (`H5iAppLibTests/QueryCases.lean`), which the Lean build checks.

In opt-in `ScalarWhenUnique` mode, membership whose subquery fixes every primary-key column uses scalar `=`
instead of `IN`. Tenant scoping plus the primary key guarantees at most one
row, including after a window. An empty scalar lookup yields SQL UNKNOWN;
in this positive `AND`/`OR` language it selects the same rows as FALSE.
This reasoning would need review before adding negation. A further 120 cases per mode
on PostgreSQL compare rows and counts for this rewrite, covering empty
lookups, compound keys, duplicate projections, windows and tenant scoping.
This remains a differential-tested renderer optimization, not a new proof.

`compile_read` rejects what the Lean does not describe: `IN` on a column
that may be `NULL` (SQL's three-valued `IN`), and windows on a table whose
key has a text or bytes column (collation-dependent order). Results come
back sorted by key, matching the key order the Lean meaning assumes and
`load` already relies on. Trusted: PostgreSQL evaluates the compiled
`SELECT`, `IN`, scalar subqueries, `ORDER BY ... LIMIT ... OFFSET` as documented, and the
tests' coverage. `create_index` only adds indexes (`CREATE INDEX IF NOT
EXISTS` on `tenant_id` and given columns), which change no result.

## Enforced by structure

| Property | How |
|---|---|
| Handlers go through the kernel | App code gets `H5iApp::respond` / `Engine::execute`, never a DB handle. A `Store` gets an opaque `h5i_app_pg::Tx` with only `load_table`, `load_read`, `count_read` and `store_writes` (SQL from `h5i_app_pgsql`); `h5i_app_pg::Pool` is opaque. `cargo deny check bans` (`deny.toml`) rejects database crates outside `h5i-app-pg`. At runtime, `h5i_app_pg::lockdown` gives the tables to a NOLOGIN owner and row access only to the engine's role. A superuser login bypasses all of this. |
| Tenant isolation | Every compiled statement filters on or writes its tenant, and `tenant_id` is in every primary key. `Pg.compile_sound` proves other tenants' rows stay as they were. Kernel rows have no tenant id, so the kernel cannot name another tenant. The engine and each `Store` must pass `K::tenant(actor)` consistently. |
| Every column is persisted | `table!` must list every field to compile. |

## Assumed

| Component | Assumption | Mitigation |
|---|---|---|
| `Authenticator` | Returns the principal that sent the request. | Proven: an accepted token's signed payload is exactly `enc(tenant, user, exp)`. HMAC-SHA256 from libcrux 0.0.8 (HACL*-verified, pre-1.0). Trusted: the key and its storage, the expiry clock, `ct_eq`. |
| JSON codec | Decodes the command the client meant. | `deny_unknown_fields`; tagged enum. |
| Shell inputs | Principal values are true: slugs (Wastebin), GitHub teams and download counts (crates.io), password checks (Atuin, Wastebin, Conduit), time in Conduit's commands. | Theorems hold for every value; a wrong one only feeds the kernel wrong facts. Each app's README lists them. |
| Clock | `Clock::System` (default) or `Clock::Database` (`transaction_timestamp()`; booking, Wastebin, crates.io) is right. `HmacAuth` expiry uses the system clock. | A wrong time only leads to decisions on that time. `Clock::Database` gives a database's servers one clock. With `monotonic`, the engine contract keeps commit times from decreasing per tenant (`h5i_clock`); `H5iAppLib.ReachableT` proofs hold for database runs only under that. |
| Authorization policy (apps with `[authz]`) | The `Policy` impl says what was meant, and its `stored` compares a disclosed row with the stored one; `Disclose` lists every row a reply carries; `View` hides what `visible` hides; a write command's handler does not copy hidden rows into its writes or derived reply values; replayed idempotent replies were shown to the same user before. | They are the specification, in Rust, extracted with the kernel: no generator between them and the theorem. Read-only commands under `guard_viewed` do not rely on `Disclose`. [`AUTHZ.md`](AUTHZ.md#what-stays-trusted). |
| Reply rendering | Shows the kernel's reply. | Bytes come from the proven `h5i-app-json` writer (exact output, strings cannot inject JSON). Trusted: the app's reply-to-`Value` mapping and `Value`-to-tokens flattening (tested against serde_json). |
| PostgreSQL statement semantics (A4) | PostgreSQL runs the rendered subset as `H5iAppLib.Pg` says: `CREATE TABLE IF NOT EXISTS`, `INSERT ... ON CONFLICT (key) DO UPDATE SET` / `DO NOTHING`, `DELETE ... WHERE`, `SELECT ... WHERE`. `=` holds only for equal non-`NULL` values; `IS NOT DISTINCT FROM` also for `NULL`. A statement fails or has the modeled effect; a `SELECT` returns the modeled rows in some order. Text compares byte by byte (deterministic collation). The driver converts `Val` losslessly, typed per `valKind`. Existing tables came from the same schema (`CREATE TABLE IF NOT EXISTS` keeps an old table as it is). | The path up to this model is proven (above). Each server's `tests/postgres.rs` runs it against PostgreSQL. |
| Schema description | `schema_spec()`, generated from the `table!` mappings, lists table names and column names, kinds and nullability in `TABLE` order. | Every compile and load checks `h5i_app_pgsql::valid`; the generated `schema_is_valid` test checks numbering and key lengths against `TABLE` and `KEY_LEN`. Names come from the `schema!` declaration. |
| PostgreSQL | SERIALIZABLE commits equal some serial order. | Documented guarantee. Retries restart from the snapshot read. Theorems over `H5iAppLib.Run` cover every serial order, so they cover concurrent requests only through this. |
| Engine protocol | `crates/h5i-app-pg` meets the contract below. | App code cannot reach its pool or transaction. Integration tests cover concurrency, retries, idempotency, connection loss, scoped reads and the outbox. |
| Deployment | The engine is not a superuser; no other service has its credentials. | h5i-app cannot stop a superuser. |
| Charon / Aeneas / Lean | Faithful translation; sound checker. | Upstream tools. |
| axum, hyper, tokio | Deliver requests and responses intact. | Widely used. |

## Deployment

1. As admin: `CREATE ROLE app_engine LOGIN PASSWORD '...'` (not a superuser, not the table owner).
2. As admin: `Engine::install_schema`, then `h5i_app_pg::lockdown::<App, Store>(admin_url, "h5i_owner", "app_engine")`. Idempotent; rerun after adding tables. With `h5i_app_pg::with_schema`, use the schema in both URLs.
3. Run the server with `DATABASE_URL` for `app_engine`. Other roles get `permission denied` (`tests/lockdown.rs`).

## Trusted engine contract

- One attempt loads one tenant, calls `transition`, writes and commits in one SERIALIZABLE transaction. Its statements are the compiled text, prepared once per connection and reused (at most `EngineConfig::statement_cache` per connection, least recently used closed first); the reads of a load are pipelined on the transaction's connection. Neither changes the text, the parameters or the snapshot a statement runs on.
- A command `Kernel::read_only` declares runs in one `SERIALIZABLE READ ONLY DEFERRABLE` transaction, without advisory locks and without an idempotency key (a keyed request takes the write path, which stores its reply). It is retried only when the connection drops; a deferrable read-only transaction is never cancelled for serialization. Its write set is passed to `Store::write` inside that transaction, so a non-empty one fails the request (PostgreSQL refuses writes in a READ ONLY transaction) instead of being dropped.
- Each retry opens a new transaction, reloads, reads a new time and reruns `transition`.
- A refused transition rolls back and writes no app or framework rows.
- App writes, the idempotency reply, `h5i_clock` and outbox rows commit in the same transaction.
- Keys are stored per `(tenant, ReplyCodec::scope(actor), key)`; reuse with another fingerprint is rejected.
- A connection lost before COMMIT aborts or retries from BEGIN. If lost during COMMIT, a keyed request retries; an unkeyed one returns `DbError::CommitUnknown`, and the caller must assume neither outcome.
- Session advisory locks (`Store::lock_for`: the tenant's, or the tenant's shared plus per-key ones) are taken before BEGIN, in one global order (tenant, then keys ascending), and released before the connection returns to the pool. They only queue writes that would otherwise retry; no proof or invariant depends on them.
- Outbox deliveries may repeat, keep a stable key, and go only through the destination registry.

Also trusted: the `ReplyCodec` fingerprint and scope, the clock source, the
registry, pool cancellation, and the code implementing the contract.

If these assumptions hold, each tenant's committed state comes from a serial
sequence of successful transitions, so `db_inv` applies to it and to every
later load. Migrations are outside this (see below).

### Read-only commands

A kernel that overrides `Kernel::read_only` proves `H5iAppLib.ReadOnly.Sound`
for its own extracted `read_only`: for every actor and state, an accepted
read-only command returns an empty write set ([Conduit](https://github.com/h5i-dev/h5i-app-bench/tree/realworld-bench/realworld/rust-h5i-app)
in h5i-app-bench: `ReadOnly.read_only_sound`; `h5i app lint` fails a server that overrides
`read_only` without such a proof; a kernel guarded with `guard_viewed` gets it
from the framework as `<fn>.read_only_sound`, as the forum does). With that, a read-only request changes
nothing, so its transaction can sit anywhere in the serial order whose
prefix its snapshot shows. PostgreSQL guarantees that a `SERIALIZABLE READ
ONLY DEFERRABLE` transaction sees such a prefix: it waits for a snapshot no
concurrent read-write transaction can make inconsistent. The reply is then
`transition` of a state the committed requests reached, which is what the
reply and authorization theorems are about: they are stated for every
snapshot (and every reachable one), not for a particular locking scheme.
Trusted: PostgreSQL's safe-snapshot guarantee, and that `Kernel::read_only`
delegates to the proven function (the lint checks that a proof exists, not
the delegation).

## Not covered yet

- Refusals are not stored under idempotency keys; a retried refusal runs again on the new state.
- Migrations are checked, not proven, and do not establish reachability: `Engine::migrate` commits only if every tenant passes the proven-exact checker. Cost grows with data; large tenants need batching.
- Effects arrive at least once; receivers dedupe by delivery key. The dispatcher (integration-tested), its registry (id to endpoint) and `Deliver` are trusted; endpoint address checks (no private ranges, no redirects) belong there.

## Optional RAM and CUDA experiments

[The RAM executor](MEMORY.md) retains PostgreSQL commits for writes and trusts
single-owner cache coherence, read-only classification, clock restrictions and
cancellation fallback. It adds no kernel theorem. The [GPU experiment](GPU.md)
now runs shared search/push decision source through Rust-CUDA. rustc, the NVVM
backend, LLVM/libNVVM, PTX JIT, driver, hardware and the launch/layout/bounds
shell are trusted. Lean establishes properties of extracted Rust decisions;
it says nothing about GPU execution. CPU/GPU differential checks are empirical,
not an equivalence proof. cuda-oxide remains a separate, currently blocked probe. The real linkding GPU
benchmark and its trust notes live in h5i-app-bench (performance/linkding/rust-h5i-gpu).

### Selecting read SQL

Declarative reads and exact counts default to `ReadSqlMode::Membership`,
which retains `IN (SELECT ...)`. An application can opt into the alternate
renderer before creating/serving its engine:

```rust,ignore
let pool = h5i_app_pg::pool(&database_url, 32)?;
pool.set_read_sql_mode(h5i_app_pg::ReadSqlMode::ScalarWhenUnique);
let engine = h5i_app_pg::Engine::<App, AppStore>::new(pool, config);
```

The setting is shared by clones of the same pool, affects nested predicates
in both row and count queries, and can be inspected with `read_sql_mode()`.
It changes neither the application read/count plan nor isolation, locks,
schema or results. Configure it before serving requests so a request uses a
consistent strategy. SQL text keys the existing bounded prepared cache.
Neither strategy is promised faster for all workloads. Both are checked
against the same reference evaluator on PostgreSQL, including exact counts,
empty lookups, windows and compound keys. This adds no new formal guarantee:
the SQL read renderer remains outside extraction. Existing `compile_read`
callers keep their signature and use Membership; direct renderer users can
call `compile_read_with_mode`.
