# Design

See also the [roadmap](ROADMAP.md), [what is proven and trusted](TRUST.md)
and [proof sizes and build times](NUMBERS.md).

## Architecture

The kernel, `transition(actor, snapshot, command) -> (writes, reply)`, holds
every permission check and business rule, in the Rust subset Aeneas
translates to Lean. The shell (axum routes, authentication, PostgreSQL) loads
the caller's tenant, calls the kernel and commits the writes in one
SERIALIZABLE transaction. App code gets no database
connection. That alone does not prove the server refines the kernel; that
also needs the assumptions in [TRUST.md](TRUST.md).

```
HTTP (axum) ──► Actor<K> ──► H5iApp::respond ──► Engine: BEGIN, load, transition, write, COMMIT
                                                              │
                                                  kernel (Rust) ══ Aeneas ══► Lean proofs
```

## Repository layout

| Path | Contents |
|---|---|
| `crates/h5i-app-core` | the `Kernel` trait, an in-memory reference engine, and the Lean library `H5iAppLib` |
| `crates/h5i-app-pg` | the PostgreSQL engine: tenant snapshots, retries, idempotency and role lockdown |
| `crates/h5i-app-http` | axum integration: the `Actor` extractor, `H5iApp::respond` and `rpc_router` |
| `crates/h5i-app-schema` | `schema!`, which declares kernel rows once |
| `crates/h5i-app-sql`, `h5i-app-pgsql`, `h5i-app-token`, `h5i-app-json` | extracted and proven shell parts: statement planner, SQL compiler and printer, bearer tokens, JSON output |
| `crates/h5i-app-std` | extracted and proven kernel helpers: byte strings, sets, maps, reachability, expiry checks |
| `examples/app` | the step-by-step tutorials: `calculator`, `board`, `ledger`, `inbox`, `booking` |
| `crates/h5i-app-cli` | `h5i app`: `new`, `extract`, `check`, `prove`, `lint`, `mutate`, `doctor` over a project's `h5i-app.toml` |
| `crates/h5i-app-xtask` | `cargo app-verify`, CI's checks over every project, composed from `h5i app` plus the repository's own (PostgreSQL tests, bans) |

The root Cargo workspace holds `crates/*`. `examples/app/` is a second
workspace whose crates depend on `crates/*` by path, as an application would.

In each proof project (`*/proofs`), `generated/` holds extracted Rust and
`schema!` output as a separate Lean library with its own `srcDir`;
hand-written specs and proofs sit at the top level.

## What the examples prove

The [tutorials](../../examples/app/README.md) each teach one kind of property:
functional correctness, permissions and invariants, conservation of a sum,
noninterference, and interval invariants with effects.

Ports of real applications, and the larger examples, live in the
h5i-web-app repository, where agents are benchmarked on them. Each port's
property fails with a counterexample on the code before a known fix and holds
after it:

| Port | Bug shown |
|---|---|
| Kellnr | read-only users could change owners (PR #1243) |
| Atuin | a session alone can delete an account (issue #3297) |
| Wastebin | link previews burned pastes before commit 632ddf2 (issue #190) |
| Conduit | upstream's `favorited` flag is wrong (issue #16); every reply of realworld-axum-sqlx is proven equal to a spec |
| crates.io | locked accounts could still sign in before PR #14760 |

The same repository has the document service (projects, members, a review
workflow, webhooks and a Rust-vs-Lean differential test) and examples of two
bug classes: a template substituter and a filter parser that disagree on
escaping, and a session that checks its key once and outlives the key's
revocation. Porting the servers found three more upstream problems, listed in
[NUMBERS.md](NUMBERS.md).

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

Every arrow except the PostgreSQL model is Rust extracted by Aeneas. Each
server app proves `db_inv`: the invariant holds in every database its
requests produce and in every snapshot loaded from it. [TRUST.md](TRUST.md)
gives the exact statement, the theorem behind each arrow and what stays
trusted.

## PostgreSQL boundary

h5i-app assumes PostgreSQL follows its documented semantics. Its own use of
PostgreSQL it must prove or test: the SQL matches the Lean model, table, key
and column mappings agree across Rust, Lean and PostgreSQL, and nulls,
integers and filtered deletes mean the same on each side. Retries, atomic
commits and connection cleanup belong to the tested engine contract in
[TRUST.md](TRUST.md).

A review found one mismatch: `DelWhere` treats `Val.Null` as equal to
`Val.Null`, but the SQL used `=`, which is unknown for `NULL`. The model
gives `=` and `IS NOT DISTINCT FROM` different meanings, so the soundness
proof fails on the old SQL. Filters now use `IS NOT DISTINCT FROM`; `=` is
used only for non-`NULL` key values.

The compiler resolves names through the schema: filters by the zero-based
column index `schema!` emits (a Rust constant and a Lean `col_*`), statements
by table number. `h5i_app_pgsql::valid`
rejects names PostgreSQL could read differently (empty, over 63 bytes,
containing NUL, duplicated, a column named `tenant_id`, a table named
`h5i_...`), and every compile checks it.

## Next steps

Proofs aim at one claim: the app later reads back the state change of the
decision the extracted kernel made. PostgreSQL, the async runtime and the
engine stay trusted. With `db_inv` done, next:

1. Test `H5iAppLib.Pg` against real PostgreSQL: nulls, numeric encodings,
   conflicts, scoped reads.
2. Generate stores from `schema!` and bind the tenant into the transaction
   API, so a `Store` cannot touch another tenant.
3. Test retries, unknown commits, cancellation, advisory-lock cleanup,
   idempotency and outbox delivery on the production path in CI, not in a
   separate model checker. CI rejects a run that skipped them.

## Inputs from the shell

`transition` is pure, so outside facts (time, a random slug, another
service's answer) come in from the shell. Put them in the principal, not the
client-chosen command: a client that picks the time can book the past.
Theorems hold for any value but cannot say it is true, so each app's README
lists these inputs as trusted. The authenticator draws a slug once per
request, so a retry sees the same one.

Each attempt reads the engine's `Clock` in its transaction, and
`Kernel::stamp` copies it into the principal before `transition`.
`Timestamp` counts microseconds since the Unix epoch (`secs()` for seconds).
A retry reads the clock again, so the committing attempt's time decides.
Idempotent replay matches on the command's fingerprint, which omits the time.
With `EngineConfig::monotonic`, the engine never uses a time earlier than the
tenant's last commit (kept in `h5i_clock`). `H5iAppLib.ReachableT` proves
properties under that trusted assumption, such as `started_stays` in the
booking tutorial.

## Authorization defaults

The kernel proves a decision correct, but apps break at the edges it trusts:
an unauthenticated route, an identity read from the body, a route that skips
the kernel. Three defaults keep those edges inside the guarantee.

`Actor<K>` holds its principal privately, so the only way to build one is the
extractor, which runs the `Authenticator`; a handler cannot fake an actor.
`assume_authenticated` is the one greppable bypass, for a server that
authenticates its own way (Wastebin's signed cookie).

Identity comes from the actor, not the command, since `transition` takes them
separately. So `h5i app lint` rejects an identity or privilege field
(`owner`, `role`, `is_admin`) in a `Command` unless the line carries
`h5i-allow: privileged-field`. This is readur's register `role` and
rust-web-app's owner reassignment.

`h5i app lint` also checks every `post`/`put`/`delete`/`patch` handler
takes an `Actor`, so a mutating route cannot dispatch without a resolved
caller; opt out with `h5i-allow: no-actor`.

For coverage, an app states one theorem, `H5iAppLib.WritesAuthorized`, quantified
over every actor, state and command, so a forgotten check on any route fails
the proof. `h5i app lint` reports whether an app's proofs state it; kellnr uses
the schema.

A leak is a reply, not a write. `H5iAppLib.ReadsAuthorized` is the
counterpart for confidentiality: every row a successful reply discloses (as
`readsOf` lists them) is `visible` to the actor, in the state the command ran
in. A cross-tenant read breaks it without any write, so a read-only endpoint
needs no placeholder write to be covered. `reads_of_filter` discharges the
usual list endpoint, and `Authorized` states both.

## One schema per app

The engine's tables (idempotency keys, outbox) have fixed names. Two apps in
one database would share them, and one app's dispatcher would claim the
other's effects. `h5i_app_pg::with_schema(url, "app")` gives an app its
own PostgreSQL schema, created by `install_schema`. Every tutorial uses one.

## Writing kernels in the Aeneas subset

Aeneas translates a subset of Rust. What falls outside it has a replacement:

| Instead of | Write | Prove with |
|---|---|---|
| `String`, `&str` | `Vec<u8>`, `&[u8]` | `==` and `!=` specs in `H5iAppLib.Bytes` |
| `str` methods (`starts_with`, `trim`, `split`, ...) | `h5i_app_std::bytes` | its specs, over lists (`<+:`, `trimB`, `splitB`) |
| `HashSet`, `BTreeSet` | a `Vec<T>` and `h5i_app_std::set` | its specs: `∈`, `⊆`, `SetEq` |
| `HashMap`, `BTreeMap` | a `Vec<(K, V)>` and `h5i_app_std::map` | its specs: `mapGet`, `mapInsert`, `mapRemove` |
| recursion over a graph (role inheritance) | `h5i_app_std::graph::reachable` | its spec: exactly the `Reach` set |
| `exp < now - leeway` | `h5i_app_std::time`, which cannot overflow | its specs, over `Nat` |
| `held.contains(required)` on bitflags | `held & required == required` | `Covers`, `covers_iff_testBit` in `H5iAppLib.Bits` |
| iterator adapters, closures (`.iter().any(..)`) | `for x in v.iter()` with `return` or `push` | `iter_any`, `iter_find`, `iter_filter_map`, `iter_fold` |
| a parser with early exits | a `for` loop over bytes with a state enum | `iter_loop`, modeled by `iterRun` |
| index loops `while i < v.len()` | `for` loops (the index loops still work) | `loop_search`, `loop_fold` |
| `v.is_empty()` | `v.len() == 0` | `usize_ofNatCore_eq_zero` |

Each `for` loop spec turns a per-element fact into a statement about the
whole list, so a property like "every returned event is visible" is
`∀ e ∈ out, visible e` about the full `Vec`, not about one representative.
A function whose body is one loop takes one line:

```lean
@[step] theorem find_key_spec (keys : Slice Key) (sec : alloc.vec.Vec U8) :
    find_key keys sec ⦃ o => findKey keys.val sec = o ⦄ := by
  h5i_for find_key using (iter_find keys (fun k => k.secret = sec) _ ?_) [findKey]
```

`h5i_for` unfolds the function and its loop, applies the spec, closes the
per-element goal with `h5i_iter` and restates the conclusion; what it cannot
close is left to the caller.

### `h5i-app-std`

`h5i-app-std` holds what each port used to write and prove itself: `str`
methods over bytes, a `Vec` as a set or a map, reachability, and expiry
checks. Every function has a `@[step]` spec in
`crates/h5i-app-std/proofs/StdSpecs.lean`, stated over lists with the models
of `H5iAppLib.Text`, `Sets` and `Graph`, so `step*` goes through a call and
the proof reasons about `<+:`, `⊆` or `Reach`, never about the loop.

A kernel uses it like `h5i-app-sql`: extract with `--include h5i_app_std`,
have `h5i app extract` copy the specs next to the extracted Lean, and add
`StdSpecs` to the generated library's roots:

```toml
[extract]
start-from = ["transition"]
include = ["h5i_app_std"]
std-specs = true
```

The copy imports the kernel's extraction and opens its namespace. Aeneas
keeps only the functions the kernel calls, so each spec is guarded by its
function (`h5i_when`) and the rest are skipped. The `set`, `map` and `graph`
functions are generic over `T: PartialEq` (and `Clone`); their specs need
`EqLaw` (and `CloneLaw`) for the instance, which `H5iAppLib.Sets` provides
for the scalars and `Vec<u8>`, and `h5i_derive_eq` (`h5i_derive_clone`) for
kernel types.

Tooling fixes for common failures:

- Derived `==` on an enum compares `read_discriminant`, which the WP tactics
  do not reduce. `h5i_derive_eq T f` derives `DecidableEq T` and a `@[step]`
  spec saying `f` decides equality, and an `EqLaw` instance for `T`'s
  `PartialEq`, so `h5i-app-std`'s generic specs apply to `Vec<T>`. Run it for
  field types first, then structs.
- `h5i_derive_clone T f` proves a derived `clone` is the identity, for `T` and
  for `Vec<T>`, so `step*` passes through clones, and adds a `CloneLaw`
  instance.
- `let x = if c { a } else { b };` binds on an `if`, where `step*` stops.
  `h5i_steps` rewrites the bind into the branches and continues, whether
  they are plain values or calls, and splits a `match` it stops at.
- To reason about a run that succeeded without proving every callee total,
  invert the equation: `h5i_invert h` on `h : f x = ok y` leaves one goal per
  successful path, with each call's equation as a hypothesis
  (`bind_tc_eq_ok`). `loop_ok` does the same for a loop, by a measure.
- `h5i_simp` normalizes `if false = true`, `id` and `ok` binds, and never
  fails for making no progress.
- `step*` leaves `n < Usize.max` after a `push` onto a literal: `scalar_tac`
  does not know `usize` has 32 bits. Close it with `usize_lt_max (by decide)`.
- A model that computes with `==` (`List.lookup`, `List.contains`) picks up
  Aeneas's own `BEq` on scalars, which is not the one `DecidableEq` gives.
  Use `decide`/`∈` or the `DecidableEq` models (`mapGet`).
- State postconditions as `model = extracted` (e.g. `findKey l k = o`), so
  `simp_all` rewrites the model into the extracted value. Add
  `-List.find?_eq_none` when a match on a `find?` result must reduce.
- If Aeneas reports "Could not match the contexts", move the branch into a
  helper function.

## Properties across requests

A kernel step is one request, but properties can span many. `H5iAppLib.Run` is
every serial order of requests with their outcomes; the engine commits each
request in one SERIALIZABLE transaction, so every interleaving of concurrent
clients is such an order (A5). A theorem over every `Run` therefore covers
every interleaving. `Run.inv` carries an invariant along a run. `Run.after`
proves "once this event, then from then on", and `Run.fired` gives the state
each event ran in. A check in one request and a use in another is an
interleaving like any other: a revocation can be proven to hold against every
later request, refuting the kernel that trusts a session's earlier check.

Within one request there is no race to prove: `transition` runs on one
snapshot and its writes commit atomically. Liveness ("eventually") is not
expressible, since runs are finite; scenario theorems show a behavior is
reachable. `ReachableT` adds monotonic time.

## Proof patterns

The examples prove their theorems in this order:

1. Specify each table loop with lists (`find?`, `any`, `filter`, `upsert`),
   proven with `iter_find`, `iter_any` or `iter_fold` for `for` loops, or
   `loop_search` or `loop_fold` for index loops, from `H5iAppLib`.
2. Give each command one lemma: what a successful run writes and why it
   succeeded. Only these touch extracted code.
3. Combine them into the few write-set shapes a successful command produces:
   a disjunction (board, ledger) or an inductive `Effect` relation with
   `cases` (larger ports; scales better).
4. Prove each theorem by cases on that: permissions against a per-write-kind
   policy, one invariant lemma per state change, induction over `Reachable`.
   Theorems needing the invariant assume reachability, never the invariant.
5. `Apply.lean`: the kernel's `apply` computes the spec's `applyAll`, one
   loop lemma per table.
6. Scenario theorems run the extracted kernel on small states, showing the
   guarded behavior happens (not a kernel that refuses everything) and that
   reachable-state hypotheses can hold.

A past bug is a second transition function differing in one command, refuted
by a scenario theorem. Confidentiality is noninterference over
a `view` of the state (`examples/app/inbox`). Reply correctness is equality
with a spec function.

## Checks

Each proof project has an `h5i-app.toml` at its root: what to extract
(`[extract]`: the crate, Charon's start points, `schema`, `include`,
`std-specs`), theorems that must exist (`[check]`), and the bugs its proofs
must reject (`[[mutant]]`). `h5i app` reads it:

| Verb | Does |
|---|---|
| `h5i app new <dir>` | a kernel, a Lake project that already proves it, and its `h5i-app.toml`; the Lean library is required from git at this h5i's tag |
| `h5i app extract` | Charon and Aeneas into `proofs/generated/`, `Source:` paths made relative to the repository. With `schema = true`, also the Lean `schema!` renders to its `lean "..."` path (its test, run with `H5I_APP_BLESS=1`). `--check` changes nothing and fails if the committed Lean is not what extraction produces |
| `h5i app check` | fetch the Lake packages if needed, `lake build`, then the gate: no `sorry` or `native_decide` in hand-written Lean, no `axiom` anywhere, and every theorem of every built hand-written module (plus the named ones) on `propext`, `Classical.choice` and `Quot.sound` only. `--refetch` discards the packages and fetches them again |
| `h5i app prove` | `extract`, then `check` |
| `h5i app lint` | every mutating route of a server crate takes an `Actor`, no `Command` field sets identity or privilege, and a report of whether the proofs state a universal authorization theorem |
| `h5i app mutate` | each mutant in a copy of the project: edit, `cargo check`, extract, `lake build`. Caught when the proofs fail. `--auto` adds mutants generated from the kernel's syntax (comparison boundaries, `==`/`!=`, `&&`/`||`, dropped `!`, forced `if` conditions) |
| `h5i app doctor` | Charon, Aeneas and Lean against the pins in `crates/h5i-app-cli/src/pins.rs`, and the project against the same pins |

`cargo app-verify` runs `lint` on every project, the Rust tests (and the
PostgreSQL tests when `H5I_APP_TEST_DATABASE_URL` is set), checks with
`cargo deny` that only `h5i-app-pg` uses a database driver, and runs
`extract --check` and `check` on every project. `--full` adds every project's mutants and the
Rust/Lean differential test. Missing tools are reported as skipped, not passed.

Lake packages (Aeneas, Mathlib, a few gigabytes) are fetched once and shared:
`proofs/.lake/packages` links to `$XDG_CACHE_HOME/h5i/lake/<key>/packages`
(default `~/.cache/h5i/lake`), the key a hash of the `lean-toolchain` and the
lakefile's `require`s, so projects that require the same Aeneas share one copy.
`H5I_LAKE_CACHE=<dir>` moves the cache, `H5I_LAKE_CACHE=off` keeps a copy in
each project. A fetch counts as done only once it finishes: one cut short is
resumed by the next `check`, and `--refetch` starts over.

Toolchain: stable Rust, elan with Lean v4.31.0, and Charon and Aeneas at the
commit pinned in `crates/h5i-app-cli/src/pins.rs`; a test there checks every
lakefile in the repository against it.
