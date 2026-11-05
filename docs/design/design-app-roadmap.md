# h5i-app roadmap

Each step should remove or shrink a trusted assumption; new properties come
second. What is proven and trusted today is in
[design-app-trust.md](design-app-trust.md); proof sizes per port are in
[design-app-numbers.md](design-app-numbers.md).

## Assumption ledger

Code and other docs cite these IDs; keep them stable.

| # | Assumption | Today | Target |
|---|---|---|---|
| A1 | Authenticator returns the real sender | Token parser and encoder extracted and proven, with a round trip (`crates/h5i-app-token/proofs`). HMAC from libcrux (HACL*). | Key management stays trusted |
| A2 | JSON decoder is faithful | Not needed for security: theorems cover all commands | Needed for correctness only |
| A3 | Reply encoder adds nothing | JSON writer proven (`crates/h5i-app-json/proofs`): output equals the spec printer; string bytes cannot close a string early. Mapping replies to JSON is trusted, per app. | Parser round trip |
| A4 | Postgres store matches `apply` | Proven up to the model `H5iAppLib.Pg`: planner, SQL compiler and printer, `schema!`-generated table code, `apply`, `sql_writes`, `decode`, `PgServed`/`pg_loaded_inv`. Server apps prove `db_inv`; `[authz]` apps get `Committed.pg_stored` generated. Trusted: PostgreSQL matches `H5iAppLib.Pg`, driver value conversion, schema description. | Test `H5iAppLib.Pg` against PostgreSQL with generated statements |
| A5 | SERIALIZABLE equals a serial order | Trusted; theorems over `H5iAppLib.Run` rest on it | Stays trusted (PostgreSQL guarantee) |
| A6 | Engine protocol (retry, idempotency, lock, clock, outbox) is correct | Trusted contract in design-app-trust.md; a fault test per bullet in CI (killed backends, lost COMMITs, forced 40001/40P01, cancellation, 64 concurrent clients, a server restart). No refinement proof from `h5i-app-pg`. | Keep the engine small; extend tests with the contract |
| A7 | Charon, Aeneas, Lean are sound | Trusted. Axiom gate; Rust-vs-Lean differential test (the document service in h5i-web-app, 55k cases, no mismatch) | Stays trusted |
| A8 | No handler bypasses the engine | Opaque `Tx` and pool; `cargo deny check bans` keeps DB crates in `h5i-app-pg`; `h5i_app_pg::lockdown` role separation. In CI. | Superusers out of scope |
| A9 | Running code is the extracted code | CI re-extracts every kernel and extracted crate with pinned tools and fails on a diff | Done |
| A10 | The spec says what we meant | Human review; `[[mutant]]`s per project run by `h5i app mutate` (`--auto` for generated ones, `--security` for authorization bugs, naming the proofs each breaks); scenario theorems in every app; one upstream-bug counterexample per port. Framework-defined obligations (`H5iAppLib.Obligation`) fix the statement of read-only soundness, storage, panic freedom and tenant isolation, so an app's theorem for one cannot say less | Add mutants for new failure modes |
| A11 | An `[authz]` app's `Policy` (with `stored`), `Disclose` and `View` say what we meant | The framework proves the guard enforces them for every kernel, in the states the kernel's own `apply` gives (`kernel_apply_eq`). Human review of a small, fixed surface ([design-app-authz.md](design-app-authz.md)); the rest of the app may be wrong. A policy may instead be declared in `schema!`, whose generated Rust is proven to compute a spec rendered from the declaration (`examples/app/notes`); then the declaration is what is reviewed. Read-only commands under `guard_viewed` do not rely on `Disclose`. Write commands' handlers stay trusted for confidentiality (they read the full state). | Derive `Disclose`; an opt-in mode running write commands on the view too |

## Open work

Trust:

- Test `H5iAppLib.Pg` against real PostgreSQL: nulls, numeric encodings,
  conflicts, scoped reads (A4).
- Bind the tenant into the transaction API, so a `Store` cannot touch
  another tenant. `schema!` already generates the store for `[authz]` apps.
- Engine faults not yet tested (A6): `Clock::Database` and `monotonic`
  under concurrency or faults, `Engine::migrate` under faults, TLS
  connections under faults, server or host crashes (only a clean restart is
  tested), network partitions and slow links, a server down for longer than
  a restart, and SERIALIZABLE's conflict pool overflowing on CI's
  `max_connections`. The tested part is listed in design-app-trust.md.
- Per-actor loads: the plan-to-SQL compilation (`compile_read`) is
  differential-tested, not proven against `H5iAppLib.Pg`, and commands
  without a read plan still load the whole tenant.

Proof effort:

- Proof automation. Target: no hand-written Lean for a typical command,
  under 20 lines for a business invariant.
- LLM-written proofs, with humans reviewing the policy table and
  invariants.

Known limits found while porting:

- Iterator adapters and closures (`.iter().any(..)`, `.filter().collect()`)
  extract to opaque functions in the pinned Aeneas, and `String` has no
  model; kernels use `for` loops and `Vec<u8>`.
- Liveness ("eventually") has no statement form; `H5iAppLib.Run` covers
  safety over every finite run.
- Anonymous callers share one idempotency scope; `ReplyCodec::scope`
  should refuse keys.
- `u64` columns are `BIGINT`; values of 2^63 or more fail at runtime.
- Conduit still puts time in its commands rather than the principal.

Adoption:

- Publish the numbers; run a pilot.
- Port candidates not yet done (axum confirmed from each `Cargo.toml`):

| App | Size | Authorization | Past bug | Fit |
|---|---|---|---|---|
| [Svix](https://github.com/svix/svix-webhooks) | ~6.6k LOC | org / application access levels | none found | medium |
| [Windmill](https://github.com/windmill-labs/windmill) | ~56k LOC API, 312 routes | workspaces, groups, folders, path-scoped tokens, Postgres RLS | GHSA-2ppx-66jv-wpw5 (token scope bypass), GHSA-x2wf-f962-7frq (cross-workspace resume) | hard; one slice medium |

Not suitable: Lemmy (actix-web), the axum examples (no authorization),
Rauthy and Kanidm (identity-protocol logic).

## Irreducible trust

Lean, Aeneas, rustc; PostgreSQL serializability and statement semantics; the
network stack; key management; the spec, which can only be kept small and
tested by mutation.
