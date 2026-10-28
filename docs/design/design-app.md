# h5i-app design

See also [what is proven and trusted](design-app-trust.md), the
[roadmap](design-app-roadmap.md) and [proof sizes and build times](design-app-numbers.md).

## Architecture

The kernel, `transition(actor, snapshot, command) -> (writes, reply)`, holds
every permission check and business rule, in the Rust subset Aeneas
translates to Lean. The shell (axum routes, authentication, PostgreSQL) loads
the caller's tenant, calls the kernel and commits the writes in one
SERIALIZABLE transaction. App code gets no database connection. That alone
does not prove the server refines the kernel; that also needs the
assumptions in [design-app-trust.md](design-app-trust.md).

```
HTTP (axum) ──► Actor<K> ──► H5iApp::respond ──► Engine: BEGIN, load, transition, write, COMMIT
                                                              │
                                                  kernel (Rust) ══ Aeneas ══► Lean proofs
```

| Path | Contents |
|---|---|
| `crates/h5i-app-core` | the `Kernel` trait, an in-memory reference engine, the Lean library `H5iAppLib` |
| `crates/h5i-app-pg` | the PostgreSQL engine: tenant snapshots, retries, idempotency, role lockdown |
| `crates/h5i-app-http` | axum integration: the `Actor` extractor, `H5iApp::respond`, `rpc_router` |
| `crates/h5i-app-schema` | `schema!`, which declares kernel rows once |
| `crates/h5i-app-sql`, `-pgsql`, `-token`, `-json` | extracted and proven shell parts: planner, SQL compiler and printer, bearer tokens, JSON output |
| `crates/h5i-app-std` | extracted and proven kernel helpers: byte strings, sets, maps, reachability, expiry |
| `crates/h5i-app-cli` | `h5i app` |
| `crates/h5i-app-xtask` | `cargo app-verify`, CI's checks over every project |
| `examples/app` | tutorials, a second workspace depending on `crates/*` by path |

In each proof project (`*/proofs`), `generated/` holds extracted Rust and
`schema!` output as a separate Lean library; hand-written proofs sit at the
top level. The [tutorials](../../examples/app/README.md) cover functional
correctness, permissions and invariants, conservation of a sum,
noninterference, and interval invariants with effects. Ports of real
applications (Kellnr, Atuin, Wastebin, Conduit, crates.io) live in the
h5i-web-app repository; each port's property fails on the code before a
known upstream fix and holds after it.

## From transition to stored rows

```
Rust transition --Aeneas--> Lean: Inv(s) and Accept(ws) give Inv(apply s ws)
      |
      v
write set --sql_writes--> table writes --plan--> statements
      --h5i_app_pgsql::compile--> SQL subset + parameters --render--> text
      --PostgreSQL model (H5iAppLib.Pg)--> rows
      --h5i_app_pgsql::select, decode--> next snapshot
```

Every arrow except the PostgreSQL model is extracted Rust. Each server app
proves `db_inv`: the invariant holds in every database its requests produce
and in every snapshot loaded from it. Filters use `IS NOT DISTINCT FROM`,
and `=` only for non-`NULL` key values, because `=` is unknown on `NULL`.
The compiler addresses columns by the index `schema!` emits and tables by
number, and every compile checks `h5i_app_pgsql::valid`, which rejects names
PostgreSQL could read differently (empty, over 63 bytes, NUL, duplicates, a
column `tenant_id`, a table `h5i_...`). `h5i_app_pg::with_schema(url, "app")`
gives each app its own PostgreSQL schema, so two apps in one database do not
share idempotency keys or the outbox.

## Inputs from the shell

`transition` is pure, so time, random slugs and other services' answers come
from the shell. Put them in the principal, not the client-chosen command: a
client that picks the time can book the past. Theorems hold for any value
but cannot say it is true, so each app's README lists these inputs as
trusted.

Each attempt reads the engine's `Clock` in its transaction and
`Kernel::stamp` copies it into the principal (`Timestamp`, microseconds
since the epoch). A retry reads the clock again; idempotent replay matches
on a fingerprint that omits the time. With `EngineConfig::monotonic`, the
engine never uses a time earlier than the tenant's last commit, and
`H5iAppLib.ReachableT` proves properties under that trusted assumption
(`started_stays` in the booking tutorial).

## Authorization defaults

- `Actor<K>` can only be built by its extractor, which runs the
  `Authenticator`. `assume_authenticated` is the one greppable bypass.
- `h5i app lint` rejects an identity or privilege field (`owner`, `role`,
  `is_admin`) in a `Command` unless marked `h5i-allow: privileged-field`,
  and a `post`/`put`/`delete`/`patch` handler without an `Actor` unless
  marked `h5i-allow: no-actor`.
- `H5iAppLib.WritesAuthorized`, quantified over every actor, state and
  command, makes a forgotten check on any route fail the proof.
  `H5iAppLib.ReadsAuthorized` requires every row a reply discloses
  (`readsOf`) to be `visible` to the actor; `Authorized` states both. Apps
  with `[authz]` get this generated ([design-app-authz.md](design-app-authz.md)).

## Writing kernels in the Aeneas subset

| Instead of | Write | Prove with |
|---|---|---|
| `String`, `&str`, `str` methods | `Vec<u8>`, `&[u8]`, `h5i_app_std::bytes` | `H5iAppLib.Bytes`; list specs (`<+:`, `trimB`, `splitB`) |
| `HashSet`, `BTreeSet` | `Vec<T>` and `h5i_app_std::set` | `∈`, `⊆`, `SetEq` |
| `HashMap`, `BTreeMap` | `Vec<(K, V)>` and `h5i_app_std::map`, or `h5i_app_std::hashmap::HashMap` (keys implement `KeyHash`) | `mapGet`, `mapInsert`, `mapRemove`; `hashmap.lookup` |
| recursion over a graph | `h5i_app_std::graph::reachable` | exactly the `Reach` set |
| `exp < now - leeway` | `h5i_app_std::time`, which cannot overflow | specs over `Nat` |
| bitflags `contains` | `held & required == required` | `Covers`, `covers_iff_testBit` |
| iterator adapters, closures | `for x in v.iter()` with `return` or `push` | `iter_any`, `iter_find`, `iter_filter_map`, `iter_fold` |
| a parser with early exits | a `for` loop over bytes with a state enum | `iter_loop` |
| `v.is_empty()` | `v.len() == 0` | `usize_ofNatCore_eq_zero` |

Loop specs state facts about the whole list (`∀ e ∈ out, visible e`). A
one-loop function takes one line:

```lean
@[step] theorem find_key_spec (keys : Slice Key) (sec : alloc.vec.Vec U8) :
    find_key keys sec ⦃ o => findKey keys.val sec = o ⦄ := by
  h5i_for find_key using (iter_find keys (fun k => k.secret = sec) _ ?_) [findKey]
```

To use `h5i-app-std`, set `include = ["h5i_app_std"]` and `std-specs = true`
under `[extract]`; `h5i app extract` copies the specs from
`crates/h5i-app-std/proofs/StdSpecs.lean`, each guarded so specs for
functions Aeneas dropped are skipped. `hashmap::HashMap` is written in the
subset, and its specs ask only that `key_hash` never fail, not that it avoid
collisions. Generic helpers need `EqLaw`/`CloneLaw` for the element type,
which `h5i_derive_eq` and `h5i_derive_clone` derive for kernel types.

Other tactics: `h5i_steps` (through `let x = if ...` and `match`),
`h5i_invert` and `loop_ok` (reason about a successful run without proving
callees total), `h5i_simp`, and `usize_lt_max (by decide)` for the bound
`step*` leaves after a `push`. State postconditions as `model = extracted`
so `simp_all` rewrites, avoid `==`-based models (`List.lookup`) that pick up
Aeneas's scalar `BEq`, and move a branch into a helper when Aeneas reports
"Could not match the contexts".

## Proof patterns

1. Specify each table loop with lists (`find?`, `any`, `filter`, `upsert`).
2. Give each command one lemma: what a successful run writes and why. Only
   these touch extracted code.
3. Combine them into the write-set shapes a command produces, as a
   disjunction or an inductive `Effect` relation (scales better).
4. Prove each theorem by cases on that. Theorems needing the invariant
   assume reachability, never the invariant.
5. `Apply.lean`: the kernel's `apply` computes the spec's `applyAll`.
6. Scenario theorems run the extracted kernel on small states, showing the
   guarded behavior happens and reachable-state hypotheses can hold.

A past bug is a second transition function refuted by a scenario theorem.
Confidentiality is noninterference over a `view` (`examples/app/inbox`).
Reply correctness is equality with a spec function.

Properties across requests use `H5iAppLib.Run`, every serial order of
requests. Since each request commits in one SERIALIZABLE transaction, every
interleaving is such an order (A5), so a check-then-use across requests (a
revocation against every later request) is provable. `Run.inv`, `Run.after`
and `Run.fired` carry invariants and "from then on" facts. Liveness is not
expressible: runs are finite.

## Reading less than the tenant

Measured on [Conduit](https://github.com/h5i-dev/h5i-app-bench/tree/realworld-bench/realworld/rust-h5i-app)
in [design-app-numbers.md](design-app-numbers.md).

- `Kernel::read_only` commands (obligation `H5iAppLib.ReadOnly.Sound`) run
  in `SERIALIZABLE READ ONLY DEFERRABLE` transactions, without locks or
  serialization retries.
- `Store::lock_for` names a write's advisory locks, so writes queue on what
  they change. Only an optimization; SERIALIZABLE decides correctness.
- `Store::load_for` may load a declarative read per table
  (`h5i_app_sql::Read`: column equality, `IN` another read's column, `and`,
  `or`, a key-ordered window), defined in `H5iAppLib.Query`. Its SQL is
  differential-tested, not proven.

An app that loads by plan proves a frame theorem: on what the plan loads,
`transition` gives its whole-tenant result. `schema!` generates the decoding
(`Snapshot.load`, `decode_load`, `holds_in_<t>`), `H5iAppLib.Query` gives
the slice lemmas (`find?_slice`, `any_slice`, `filter_slice`,
`page_of_newest`), and the app proves `Plan.reads_abs` and `Frame.lean`.
Conduit's [`Frame.lean`](https://github.com/h5i-dev/h5i-app-bench/blob/realworld-bench/realworld/rust-h5i-app/proofs/Frame.lean)
is the worked example.

## Checks

Each proof project's `h5i-app.toml` says what to extract (`[extract]`),
which theorems must exist (`[check]`), and which bugs the proofs must reject
(`[[mutant]]`).

| Verb | Does |
|---|---|
| `h5i app new <dir>` | a kernel, a Lake project that proves it, and its `h5i-app.toml` |
| `h5i app extract` | Charon and Aeneas into `proofs/generated/`, plus the Lean `schema!` renders; `--check` fails if the committed Lean differs |
| `h5i app check` | `lake build`, then the gate: no `sorry` or `native_decide` in hand-written Lean, no `axiom` anywhere, every hand-written theorem on `propext`, `Classical.choice` and `Quot.sound` only |
| `h5i app prove` | `extract`, then `check` |
| `h5i app lint` | the authorization defaults above |
| `h5i app mutate` | each mutant in a copy of the project, caught when the proofs fail; `--auto` adds syntax-generated mutants |
| `h5i app doctor` | tools and project against the pins in `crates/h5i-app-cli/src/pins.rs` |

`cargo app-verify` runs `lint`, the Rust tests (PostgreSQL tests when
`H5I_APP_TEST_DATABASE_URL` is set), `cargo deny` (only `h5i-app-pg` uses a
database driver), and `extract --check` and `check` on every project.
`--full` adds mutants and the Rust/Lean differential test. Missing tools are
reported as skipped, not passed.

Lake packages are shared under `$XDG_CACHE_HOME/h5i/lake/<key>/packages`
(default `~/.cache/h5i/lake`), keyed by `lean-toolchain` and the lakefile's
`require`s; `H5I_LAKE_CACHE=<dir>` moves it and `off` disables sharing.
Toolchain: stable Rust, Lean v4.31.0 via elan, Charon and Aeneas at the
commit in `pins.rs`, which a test checks every lakefile against.
