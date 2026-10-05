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
| generated `PgServed`, `pg_loaded_inv` | an invariant kept by accepted write sets holds in every state of a multi-tenant database and every snapshot loaded from it | `generated/Schema.lean` |

Each server app applies `pg_loaded_inv` to its extracted `transition`,
`sql_writes` and `decode` as `db_inv` (docs: `Database.db_inv`, which also
covers scoped loads). It says: for the server's schema and any tenant, every
database reached by loading with the compiled `SELECT`s, running a successful
`transition` and storing its compiled writes, among other tenants' compiled
statements, holds a state satisfying `Inv`, and every later load decodes to a
snapshot satisfying `Inv`.

## Enforced by structure

| Property | How |
|---|---|
| Handlers go through the kernel | App code gets `H5iApp::respond` / `Engine::execute`, never a DB handle. A `Store` gets an opaque `h5i_app_pg::Tx` with only `load_table` and `store_writes` (SQL from `h5i_app_pgsql`); `h5i_app_pg::Pool` is opaque. `cargo deny check bans` (`deny.toml`) rejects database crates outside `h5i-app-pg`. At runtime, `h5i_app_pg::lockdown` gives the tables to a NOLOGIN owner and row access only to the engine's role. A superuser login bypasses all of this. |
| Tenant isolation | Every compiled statement filters on or writes its tenant, and `tenant_id` is in every primary key. `Pg.compile_sound` proves other tenants' rows stay as they were. Kernel rows have no tenant id, so the kernel cannot name another tenant. The engine and each `Store` must pass `K::tenant(actor)` consistently. |
| Every column is persisted | `table!` must list every field to compile. |

## Assumed

| Component | Assumption | Mitigation |
|---|---|---|
| `Authenticator` | Returns the principal that sent the request. | Proven: an accepted token's signed payload is exactly `enc(tenant, user, exp)`. HMAC-SHA256 from libcrux 0.0.8 (HACL*-verified, pre-1.0). Trusted: the key and its storage, the expiry clock, `ct_eq`. |
| JSON codec | Decodes the command the client meant. | `deny_unknown_fields`; tagged enum. |
| Shell inputs | Principal values are true: slugs (Wastebin), GitHub teams and download counts (crates.io), password checks (Atuin, Wastebin, Conduit), time in Conduit's commands. | Theorems hold for every value; a wrong one only feeds the kernel wrong facts. Each app's README lists them. |
| Clock | `Clock::System` (default) or `Clock::Database` (`transaction_timestamp()`; booking, Wastebin, crates.io) is right. `HmacAuth` expiry uses the system clock. | A wrong time only leads to decisions on that time. `Clock::Database` gives a database's servers one clock. With `monotonic`, the engine contract keeps commit times from decreasing per tenant (`h5i_clock`); `H5iAppLib.ReachableT` proofs hold for database runs only under that. |
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

- One attempt loads one tenant, calls `transition`, writes and commits in one SERIALIZABLE transaction.
- Each retry opens a new transaction, reloads, reads a new time and reruns `transition`.
- A refused transition rolls back and writes no app or framework rows.
- App writes, the idempotency reply, `h5i_clock` and outbox rows commit in the same transaction.
- Keys are stored per `(tenant, ReplyCodec::scope(actor), key)`; reuse with another fingerprint is rejected.
- A connection lost before COMMIT aborts or retries from BEGIN. If lost during COMMIT, a keyed request retries; an unkeyed one returns `DbError::CommitUnknown`, and the caller must assume neither outcome.
- Session advisory locks are taken before BEGIN and released before the connection returns to the pool.
- Outbox deliveries may repeat, keep a stable key, and go only through the destination registry.

Also trusted: the `ReplyCodec` fingerprint and scope, the clock source, the
registry, pool cancellation, and the code implementing the contract.

If these assumptions hold, each tenant's committed state comes from a serial
sequence of successful transitions, so `db_inv` applies to it and to every
later load. Migrations are outside this (see below).

## Not covered yet

- Refusals are not stored under idempotency keys; a retried refusal runs again on the new state.
- Migrations are checked, not proven, and do not establish reachability: `Engine::migrate` commits only if every tenant passes the proven-exact checker. Cost grows with data; large tenants need batching.
- Effects arrive at least once; receivers dedupe by delivery key. The dispatcher (integration-tested), its registry (id to endpoint) and `Deliver` are trusted; endpoint address checks (no private ranges, no redirects) belong there.
