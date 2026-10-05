# Roadmap

Each phase must remove or shrink a trusted assumption. New properties come
second.

## Assumption ledger

| # | Assumption | Today | Target |
|---|---|---|---|
| A1 | Authenticator returns the real sender | Token parser and encoder extracted and proven, with a round trip (`crates/h5i-app-token/proofs`, `TokenEncode.lean`). HMAC from libcrux (HACL*). | Key management stays trusted |
| A2 | JSON decoder is faithful | Not needed for security: theorems cover all commands | Needed for correctness only |
| A3 | Reply encoder adds nothing | JSON writer proven (`crates/h5i-app-json/proofs`): output equals the spec printer; string bytes cannot close a string early (`write_str_at`). Mapping replies to JSON is trusted, per app. | Parser round trip |
| A4 | Postgres store matches `apply` | Proven up to PostgreSQL: planner (`crates/h5i-app-sql`), SQL compiler and printer (`crates/h5i-app-pgsql`) against the model `H5iAppLib.Pg` (`exec`, `Lists`/`Sel`). `schema!` generates table code, `apply`, `sql_writes`, `decode`, their specs, and `PgServed`/`pg_loaded_inv`. Each server app proves `db_inv` from its extracted `transition` to every later load. Trusted: PostgreSQL matches `H5iAppLib.Pg`, driver value conversion, schema description. | Test `H5iAppLib.Pg` against PostgreSQL with generated statements |
| A5 | SERIALIZABLE equals a serial order | Trusted; theorems over `H5iAppLib.Run` (every interleaving of requests) rest on it | Stays trusted (PostgreSQL guarantee) |
| A6 | Engine protocol (retry, idempotency, lock, clock, outbox) is correct | Trusted contract in `TRUST.md`; integration and fault tests. The Lean engine model was removed: no refinement proof from `h5i-app-pg`. | Keep the engine small; extend tests with the contract |
| A7 | Charon, Aeneas, Lean are sound | Trusted. Axiom gate; Rust-vs-Lean differential test (`scripts/app/difftest.sh`, 55k cases, no mismatch) | Stays trusted |
| A8 | No handler bypasses the engine | Opaque `Tx` and pool; `cargo deny check bans` keeps DB crates in `h5i-app-pg`; `h5i_app_pg::lockdown` role separation. In CI. | Superusers out of scope |
| A9 | Running code is the extracted code | CI re-extracts every kernel and extracted crate (`h5i-app-sql`, `h5i-app-token`, `h5i-app-json`) with pinned tools and fails on diff | Done |
| A10 | The spec says what we meant | Human review; mutation suites (the `[[mutant]]`s in each `h5i-app.toml`, run by `h5i app mutate`; `--auto` for generated ones); scenario theorems in every app; one upstream-bug counterexample per port | Add mutants for new failure modes |

## Phase 0: proofs on the example app

Done: extraction check (A9); proofs on the extracted kernel of
authorization, invariants, reply confinement, noninterference with error
codes and totality, for every command (A2); axiom gate and differential test
(A7); mutation suite (A10). Metrics in `NUMBERS.md`.

## Phase 1: shrink the shell

Done:

- SQL translation proven equal to `apply` (A4): `h5i_app_sql::plan`, `schema!`,
  `H5iAppLib.Store`, `h5i_app_pgsql` with `H5iAppLib.Pg`, `db_inv` per server app.
  Docs keeps its own proofs (`Storage.lean`, `Load.lean`, `Scoped.lean`,
  `Database.lean`).
- Engine protocol as a trusted contract with PostgreSQL tests (A6).
- Token parsing extracted and proven unambiguous; libcrux HMAC (A1).
- Reply rendering extracted and proven (A3).
- `install_schema` creates the only role that writes h5i-app tables; `cargo-deny`
  bans DB crates outside the engine (A8).

## Phase 2: developer experience

- Done: `schema!` generates kernel types, table code, `apply`,
  `sql_writes`, `decode` and Lean specs, with column indices shared by Rust
  and Lean. Other server apps map their state in `Storage.lean` to use
  `H5iAppLib.Store`.
- Proof automation. Target: no hand-written Lean for a typical command,
  under 20 lines for a business invariant. `H5iAppLib` has `loop_search`,
  `loop_fold`, table writes, `walk`, `h5i_step` and `h5i_eval`, which runs
  concrete scenarios with `@[step]` loop specs. For `for` loops over slices,
  `iter_loop`, `iter_fold`, `iter_search` and their list forms, closed by
  `h5i_iter`, or `h5i_for` for a whole one-loop function; `h5i_derive_eq`
  and `h5i_derive_clone` for derived `==` and `clone`; `h5i_steps` through binds on
  `if`; `h5i_simp`.
- LLM-written proofs; humans review the policy table and invariants.
- Done: `cargo app-verify` (`crates/h5i-app-xtask`) runs tests, bans, extraction drift and
  all proofs with sorry/axiom gates. `--full` adds mutants and the
  differential test.
- Done: `h5i app` (`crates/h5i-app-cli`): `new`, `extract`, `check`, `prove`,
  `mutate` (declared and generated mutants) and `doctor`, driven by each
  project's `h5i-app.toml` instead of a script per app.

## Phase 3: remove MVP simplifications

Done for docs:

- Partial snapshots. `read_scope` names the counter and one project;
  `transition_frame` proves that slice gives the whole-tenant result;
  `DocsStore::load_for` loads it (`Scoped.lean`, `Database.lean`,
  `tests/postgres.rs`).
- Effects and outbox. `authorized` proves an effect goes only to its
  project's registered destination when a writer publishes an approved
  document. `h5i_app_pg::outbox` stores effects in the request's transaction and
  delivers at least once with a stable key to registered ids. Dispatcher and
  registry are trusted (`tests/outbox.rs`); receivers must deduplicate.
- Migrations, checked rather than proven. `Engine::migrate` runs pending SQL
  and the app's checker on every tenant in one transaction, rolling back on
  failure. `check_inv_spec` proves `check_inv` exact (`tests/migrations.rs`).

## Phase 4: real applications

Done: Kellnr (PR #1243), Atuin (issue #3297), Wastebin (issue #190), Conduit
(issue #16), crates.io (PR #14760), each under `examples/app/`. See `TARGETS.md`.
Open: publish the numbers; run a pilot.

## Found from user feedback

Done:

- Text handling: byte-string specs (`H5iAppLib.Bytes`) and parser loops
  (`iter_loop`); `examples/app/filters` proves a substitute/parse round trip for
  every name and refutes an escaping mismatch.
- Collections: `for` loop specs state properties of the whole list, not of
  one element.
- Multi-request properties: `H5iAppLib.Run`; `examples/app/keys` proves revocation
  against every interleaving and refutes a check-then-use kernel.
- Automation: `h5i_for`, `h5i_derive_eq`, `h5i_derive_clone`, `h5i_steps`,
  `h5i_simp`, and partial correctness (`h5i_invert`, `loop_ok`).

## Found while porting

Done:

- `with_schema`: one PostgreSQL schema per app, so apps sharing a database
  no longer share tables, idempotency keys or the outbox.
- `DelWhere`: cascading delete by a non-key column, one null-safe `DELETE`.
- `H5iApp::run` returns the reply unrendered, for cookies and API tokens.
- `HmacAuth` takes closures; `verify` is public; the scheme is configurable;
  a missing header can mean an anonymous principal.
- Clock: the engine passes a `Clock` (system, database or test) to
  `Kernel::stamp` per attempt. The trusted `monotonic` contract says time
  never goes back in commit order per tenant. `MemoryEngine::execute_at` and
  `H5iAppLib.ReachableT` support it. Conduit still puts time in its commands.
- `MemoryEngine::with_snapshot` starts a reference run from database state.
- The outbox sends each batch in row-id order; global order is not
  guaranteed.

Open:

- Iterator adapters and closures (`.iter().any(..)`, `.filter().collect()`)
  extract to opaque functions in the pinned Aeneas; kernels use `for` loops.
  `String` has no model; kernels use `Vec<u8>`.
- Liveness ("eventually") has no statement form; `H5iAppLib.Run` covers
  safety properties over every finite run.
- Per-actor loads. A command loads the whole tenant, O(tenant). An
  actor-aware `Store::load_for` needs a frame theorem per app.
- Anonymous callers share one idempotency scope; `ReplyCodec::scope` should
  refuse keys.
- `u64` columns are `BIGINT`; values of 2^63 or more fail at runtime.

## Irreducible trust

Lean, Aeneas, rustc; PostgreSQL serializability and statement semantics; the
network stack; key management; the spec, which can only be kept small and
tested by mutation.
