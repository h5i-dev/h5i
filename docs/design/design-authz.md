# Design: framework-proven authorization for h5i-app

How an h5i-app server gets a proven authorization theorem without its author
writing Lean. The theorem is proven once, for every kernel; the author writes
a policy in Rust, and a generated one-line file instantiates the theorem.
Z0 to Z6 are **built** (2026-10-26); Z7 is **not built**. What the theorem
states and what stays trusted is [`docs/app/AUTHZ.md`](../app/AUTHZ.md); this
page records the design and why it went the way it did.

## In one screen

- Before: each app wrote its own `allowed` in Lean and proved
  `WritesAuthorized` by case analysis over its kernel (board:
  `examples/app/board/proofs/Spec.lean`, `Theorems.lean` `authorized` and
  `writes_of`). The cost grows with the kernel.
- Z1: `h5i_app_std::authz::guard` runs `transition` and checks every write
  and every disclosed row against the policy. Its theorem never opens
  `transition`, so it holds for any kernel, including generated ones.
- Z3: the policy is ordinary Rust (`impl Policy`, `impl Disclose`, `impl
  View`), extracted with the kernel. No DSL, so no generator between the
  policy and the theorem.
- Z4: `guard_viewed` runs read-only commands on the caller's view; their
  replies are noninterfering with everything outside it, derived values
  included, and `ReadOnly.Sound` comes for free.
- Z5: `[authz] guarded` in `h5i-app.toml`; `extract` writes `Authz.lean`,
  `check` gates its theorems, `lint` checks the wiring, the report records it.
- Z6: [`examples/app/forum`](../../examples/app/forum/) ports Nyxis's bulletin
  board with no hand-written Lean.
- An authorization bug in a handler becomes a runtime `Denied`, not a failed
  proof; `NeverDenied` gets the static guarantee back.
- Opt-in and additive: apps without `[authz]` build, extract and verify
  unchanged (Z0).

Part of the h5i design set. The h5i-app trust ledger is
[`docs/app/ROADMAP.md`](../app/ROADMAP.md); this work adds A11 (the app's
policy says what was meant) and nothing on the code path.

---

## Why

h5i-app already had the structure a framework-wide theorem needs. Handlers
cannot reach the database except through the engine (A8), the running code is
the extracted code (A9), the store matches `apply` (A4), and every transaction
is SERIALIZABLE (A5). `H5iAppLib/Authz.lean` already stated `WritesAuthorized`
and `ReadsAuthorized` over every actor, state and command.

What was missing was the generic part: `allowed` and `visible` were per-app
Lean, and so were the proofs (docs: about 865 Lean lines for its spec,
authorization, replies and invariants). Nyxis (NyxFoundation/nyxis) is the
other point in the design space: one theorem, `Runtime.authorization_safety`,
over every app, with the developer writing only a Cedar schema and policy.
Nyxis gets there by restricting handlers to authorization-layer actions in a
Lean DSL. h5i keeps arbitrary Rust kernels and checks where the kernel's output
leaves.

## Z0. Compatibility requirements

Requirements on the implementation, all met:

1. The `Kernel` trait in `crates/h5i-app-core/src/lib.rs` is unchanged.
2. `Guarded<E>` appears only in the error type of apps that opt in. Existing
   `Kernel::Error` types, their HTTP mapping and `ReplyCodec` are untouched.
3. New lint checks never fail an app without `[authz]`: `guarded_wiring` is a
   no-op for them, and `read_only_proof` accepts what it accepted before.
4. `Run.authorization` is `#[serde(default)]`, so earlier run records still
   read; the web view shows it only when present.
5. `guard_viewed` is opt-in. The default `load_for` is unchanged.
6. Hand-written `WritesAuthorized` proofs keep working and keep being reported
   (board and inbox: `h5i app lint` says what it said before).
7. Extraction of an app that does not call the guard is byte-identical
   (board re-extracts unchanged); `AuthzSpecs.lean` is copied only next to a
   kernel that calls the guard, and removed otherwise.

Opting in takes: `Transition`, `Policy` and `Disclose` impls (and `View` for
`guard_viewed`), a `guarded_transition` defined as the guard, `[authz]` in
`h5i-app.toml`, and a server `Kernel` whose `transition` calls it and whose
`Error` is `Guarded<Error>`.

## Z1. The guard

`crates/h5i-app-std/src/authz.rs`:

```rust
pub fn guard<A, S, C, W: Clone, R: Disclose<D>, E, D, T: Transition<…>, P: Policy<…>>(
    actor: &A, s: &S, cmd: &C,
) -> Result<(Vec<W>, R), Guarded<E>> {
    match T::kernel_transition(actor, s, cmd) {
        Err(e) => Err(Guarded::App(e)),
        Ok(out) => {
            if !check_writes::<…>(actor, s, &out.0) { return Err(Guarded::Denied); }
            let after = T::kernel_apply(s, &out.0);
            if !check_reads::<…>(actor, s, &after, &out.1.disclosed()) { return Err(Guarded::Denied); }
            Ok(out)
        }
    }
}
```

- Write `i` is checked in `kernel_apply(s, ws[..i])`, the kernel's own commit
  semantics (the `apply` A4 links to the database), not in a `step` the author
  writes. A command may create a row and then act on it. A `DelWhere` is one
  write; the policy sees it as the kernel's `Write` value, before expansion.
- A disclosed row must be `Shown` before the command or after it: stored
  there exactly as disclosed (`Policy::stored`), and visible. A deleted row the
  actor could read is fine to return, and so is a row the command legitimately
  created. `stored` was added after review: with `visible` alone, judged on
  the reply's copy of a row, a handler could relabel a hidden draft as
  published and have the copy accepted (the forum's `ForgeOnUnpublish`).
  Making it a required trait method means an author cannot forget it.
- Fail closed: one refused write or row refuses the command; nothing commits.
- `NeverDenied` (optional, per app) turns the runtime check back into a
  static guarantee (`transition_authorized_of_never_denied`).

Changed in implementation: the trait methods are `kernel_transition` and
`kernel_apply`. An impl delegates to the kernel's `transition`, and in the
extracted Lean a method named like the function it calls resolves to itself
(Lean reports non-termination). The guard projects `out.0` and `out.1` instead
of destructuring `Ok((ws, r))`: with concrete types, Aeneas rejects the moved
tuple ("There should be no bottoms in the value").

## Z2. The library

- A module of `h5i-app-std`, not a new crate. A kernel that includes
  `h5i_app_std` already gets the crate's functions extracted with it and its
  specs copied next to its Lean (`std-specs = true`); a second crate would
  have needed a second copy of both mechanisms.
- `proofs/AuthzSpecs.lean`, 412 lines without comments, proven over arbitrary `Transition`,
  `Policy`, `Disclose` and `View` instances: `guard_authorized`,
  `guard_app_error`, `guard_only_denies`, `guard_viewed_authorized`,
  `guard_viewed_noninterference`, `read_only_sound_of_viewed`,
  `transition_authorized_of_never_denied`, and the loop lemmas under them.
  Partial correctness (`loop_ok`, `bind_eq_ok`): about runs that returned, so
  the app's functions need no termination proofs. `Check.lean` prints every
  theorem's axioms.
- `h5i_authz g` (in the same file) reads the instances off `g`'s extracted
  body. Before adding anything it checks that `g` passes its own arguments to
  the guard unchanged (otherwise Lean would try, and run out of memory, to
  unfold the guard in a definitional-equality check) and that the instance's
  `kernel_apply` is the kernel's `apply` by `rfl`, so a policy cannot be
  checked against a made-up commit semantics. Then it adds
  `g.kernel_apply_eq`, `g.authorized` (by `authorized_of_eq` or
  `authorized_viewed_of_eq`) and, for `guard_viewed`, `g.read_only_sound`.
- The existing `H5iAppLib.Authz` definitions are unchanged. `WritesPermitted`
  is a new definition: it checks each write in the state after the earlier
  ones, where `WritesAuthorized` checks all against the pre-state.

## Z3. The policy

Designed as a `policy!` macro with insert/update/delete/read rules per table,
generating Rust, a readable `Policy.lean` and the theorem. Built instead as
plain trait impls in the kernel crate:

```rust
impl Policy<Principal, Snapshot, Write, Row> for Forum {
    fn permits(actor: &Principal, s: &Snapshot, w: &Write) -> bool {
        match w {
            Write::PutPost(p) => match find_post(&s.posts, p.id) {
                None => p.author == actor.user && p.id == s.counter.next_id,
                Some(old) => (old.author == actor.user || is_admin(s, actor.user)) && p.author == old.author,
            },
            …
        }
    }
    fn visible(actor: &Principal, s: &Snapshot, d: &Row) -> bool { … }
}
```

Why: the impls are extracted with the kernel, so the theorem is stated over
the policy's own code. A macro would have put a code generator between what
the reviewer reads and what the theorem is about, and the design's plan was
to either prove each generated file against `Policy.lean` or list the
generator as trusted. Neither is needed now. `schema!` is `macro_rules`, with
no proc-macro crate to host a parser, which pointed the same way. Insert
versus update is the policy's own `find_*` lookup, as in the forum.

## Z4. Reads

- `Disclose` lists the rows a reply carries; `check_reads` denies the command
  if any is not stored as disclosed and visible, before or after. The app writes it (there is no derive: no proc-macro
  crate), and its completeness is trusted (A11).
- `guard_viewed` (opt-in): a command `View::read_only` declares runs on
  `View::view(actor, s)`, is denied if it returns writes, and has its rows
  checked against the view. `ViewNoninterference`: two states with the same
  view give the same result, denials included. Visibility is judged on the
  view, not on `s`, because a decision on `s` would leak one bit of what the
  view hides. This covers derived values, which `Disclose` cannot, and makes
  `Disclose` irrelevant for read-only commands.
- `read_only_sound_of_viewed`: the engine may run those commands in a
  `READ ONLY DEFERRABLE` transaction; no hand-written `ReadOnly.Sound` proof.
- Not built: a policy-derived read plan for `load_for`. The view is computed
  from the full tenant load.

## Z5. Wiring and tooling

- `[authz] guarded = ["guarded_transition"]` in `h5i-app.toml`.
- `h5i app extract` copies `AuthzSpecs.lean` when the kernel's Lean calls the
  guard, and writes `generated/Authz.lean` (`import AuthzSpecs`,
  `h5i_derive_all`, one `h5i_authz` per function). It fails if `[authz]` names
  functions but the kernel never calls the guard, or `std-specs` is off.
- `extract` also writes a storage section into `Authz.lean` (A4 for the
  app, below) when `sql_write` only pushes `schema!` table writes, and
  fails otherwise unless `[authz] storage` names the app's own theorem.
- `h5i app check` runs `lake build Authz` itself, so it never gates a stale
  `.olean` (a lakefile with no `Authz` target fails with a hint); gates the
  module like a hand-written one; requires `<kernel>.<fn>.authorized`,
  `.kernel_apply_eq`, the storage theorems (or `[authz] storage`), and
  `.read_only_sound` when the server overrides `Kernel::read_only` without a
  proof stated in code (a comment does not count); and records the list it
  required in the run, which the web view shows instead of recomputing it.
- `h5i app lint` reads the Rust with `syn` (`crates/h5i-app-cli/src/rust.rs`,
  following each crate's module tree as rustc does: `[lib]`/`[[bin]]`
  paths, `#[path]`, cfg'd modules, `#[cfg(test)]` and `cfg(all(test, …))`
  items left out, rename chains). With `[authz]` it only flags a
  hand-written `impl Kernel`, `impl Store` or `impl AuthzKernel`: hygiene,
  not a guarantee (see "Wiring" below).
- The check record gets the coverage line, with the storage status, only
  once the gate has passed.

### The storage proof (A4 for the app)

`Kernel::apply` is not on the commit path: the PostgreSQL engine writes
through `Store::write` (`schema_store`). So `kernel_apply_eq` ties the policy
to the kernel's `apply`, and a separate proof must tie that `apply` to the
store. Board does this by hand (`Storage.lean`, over its Lean model of the
state). The generated section needs no model: its state is the table encoding
itself (`trunc` keeps the schema's tables), so `enc_step` holds by
construction and the generic `Schema.stored`/`pg_served` apply. The app's
content is two lemmas checked against the extracted code: `sql_fits`
(`sql_write` emits `sqlA`, which is read off the extracted `sql_write` with
anchored regexes) and `apply_write_enc` (`apply_write` changes
`Snapshot.enc` exactly as `sqlA` does). From them, `apply_enc` and
`pg_stored`. A wrong reading of `sql_write`, or an `apply_write` that
disagrees with it, fails the build. The same generated text proves storage
for board and booking too, checked against their builds.

The `Store` that calls `schema_store` and `schema_load` (which call the
extracted `sql_writes` and `decode`) is generated by the mapping macro, not
proven; that the server runs it is the one trusted line (below). `pg_stored` is stated for an arbitrary `Step`, so
it holds for whatever write sets the engine commits; it keeps the schema
hypotheses (`Pg.Valid ts` and the table count) open, as board's `db_inv`
does.
- The check record carries the coverage line (`Run.authorization`), shown in
  the web view's check evidence.
- CI needs no change: it runs `--all crates examples/app`, which finds the
  forum by its `h5i-app.toml`.

### Wiring: from lint to the type system

Through five reviews, the server's `Kernel` and `Store` impls were hand
written and lint was what tied them to the proofs: first by text, then with
`syn`. Every review found a new way to pass it while running something else
(a decoy impl, a test module above the real one, a guarded call whose result
was thrown away, an impl from a macro or `include!`, a `#[path]` or Cargo
target lint did not follow). A syntactic check of what Rust compiles cannot
be made complete, so the wiring was moved out of the app:

- `h5i_app_core::Authz<T>` is the one `Kernel` impl for `Authz<_>`
  (coherence), and its `transition` is `guard_viewed` of `T`'s instances,
  called by framework code. The guard's theorems hold for every handler, so
  what the engine runs is guarded whatever `T`'s handlers do.
- `h5i_app_std::authz_kernel!` writes, from one list of types,
  `guarded_transition` (the extraction anchor the theorems are stated for)
  and `impl AuthzKernel for T` (the types `Authz<T>` uses; only the kernel
  crate may write it, by the orphan rule). The two instantiations are equal
  by construction.
- The mapping macro's `store` form generates the `Store`, with the default
  `load_for`; it is its own `Table` tag, since the kernel type is no longer
  local to the server.

The first plan was to extract an `impl Kernel` from the kernel crate and
prove `Kernel::transition = guarded_transition` by `rfl`. Charon translates
the impl, but the pinned Aeneas drops `h5i_app_core::Kernel` and its impl
from the Lean without a message (its associated types and `Send + Sync +
'static` bounds are the likely cause), and Charon refuses a generic trait
impl as a start point (`--start-from does not support trait generics`).

What stays outside the proofs is one line: that the server builds its engine
from `App` and the generated store. Lint flags the plain alternative.

## Z6. Demonstration

Built on a new example rather than board, so tutorial 2 stays as written (Z0).

- `examples/app/forum`: Nyxis's bulletin board (published and draft posts,
  self-registration, granting and revoking admin). Handlers check input and
  existence only. No hand-written Lean; `h5i app check` passes with
  `guarded_transition.authorized` and `.read_only_sound`.
- `handler_bugs_are_denied`: four handlers with typical bugs (taking over a
  post on edit, disclosing drafts on publish, admin on registration, a hidden
  draft relabelled as published in a reply) run behind the same policy and are
  denied. The forum's policy also gives each new post the counter's next id
  and lets the counter move only by one.
- `tests/postgres.rs`: 600 random commands from four users on PostgreSQL and
  `MemoryEngine` agree, denials included.
- `guard_overhead`: measured in [`NUMBERS.md`](../app/NUMBERS.md#framework-proven-authorization).
- `docs/app/AUTHZ.md`: the theorem catalog, in the form of Nyxis's
  `docs/proofs.md`.

Changed: `h5i app mutate` is not adapted. Its mutants must break a proof, and
under the guard neither a handler bug (a denial) nor a policy change (a new
specification) does; `handler_bugs_are_denied` is the check that fits. Not
built: a Lean-side differential test of the forum's policy. In the pinned
Aeneas `Result` is coinductive, so extracted code does not `#eval`, and
scenario proofs (`h5i_eval`) need per-app loop specs, the hand-written Lean
this avoids. Aeneas's faithfulness stays A7.

## Z7. Cedar front end (not built)

Translate a Cedar subset into Rust `Policy` impls, so policies written for
Cedar tooling can be reused. With Z3 as plain Rust, this would be a generator
again, and its output would be what the theorem is about. Linking to
cedar-lean's soundness theorems (`validate_with_level_is_sound_wf`, as Nyxis
does) is the stretch goal.

## The guarantee

States, for every actor, state and command of a kernel run through `guard`:
a committed command is the kernel's own result, each write is permitted in
the state the writes before it leave (by the kernel's own `apply`), and each
disclosed row is stored as disclosed and visible, before or after. Through
`guard_viewed`, additionally: a committed read-only command made no writes and
disclosed only rows stored and visible in the view, and its
result depends on nothing outside the view.

The third review (an independent reviewer) found that `check` could pass
on a stale `Authz.olean`, that the forum had no storage proof while the docs
said otherwise, that lint accepted a wired decoy impl, and smaller gaps; all
are fixed above. Does not state confidentiality for commands that write: their handlers read
the full state and can copy hidden rows into permitted writes or derived reply
values (the forum's `writes_can_carry_what_the_caller_cannot_see`); replayed
idempotent replies skip the guard. Found in the second review; the fix that
closes the first, running write commands on the view as well (noninterference
up to the one bit of a denial), is not built: it needs the view to hold what
writes depend on, which the forum's counter shows can itself disclose. Does
not state that the policy (its `stored` included), `Disclose` or `View` say
what was meant (A11);
anything about values derived from rows by a command that writes; anything A4
to A8 do not already cover.

## Risks, as they turned out

- Aeneas extracts the generic guard and its concrete uses, after the two
  changes in Z1. Trait instances become structure parameters, so one spec
  file serves every kernel.
- The guard copies the snapshot per write prefix, once after, and once for
  the view: linear in the tenant, about 20 µs at 100 posts and 2 to 3 ms at
  10,000. The engine's full-tenant load is of the same order. `stored` adds a
  lookup per disclosed row, so on `Vec` snapshots a listing is quadratic:
  18.6 ms for 10,000 posts. Keyed lookups would remove that.
- Runtime denial changes the failure mode, as expected. `NeverDenied` and
  bug tests like the forum's are the answer.
