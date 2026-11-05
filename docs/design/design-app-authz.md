# Framework-proven authorization

An h5i-app server can get a proven authorization theorem without writing Lean.
The app writes its policy in Rust, runs its kernel behind the framework's
guard, and lists the guarded function in `h5i-app.toml`. The theorem is proven
once, for every kernel, in `crates/h5i-app-std/proofs/AuthzSpecs.lean`;
`h5i app extract` instantiates it for the app in a generated file.

The worked example is [`examples/app/forum`](../../examples/app/forum/), a port
of Nyxis's bulletin board (drafts, admins, self-registration) with no
hand-written Lean. [`examples/app/notes`](../../examples/app/notes/) declares
its policy in `schema!` instead of writing it
([A policy from the schema](#a-policy-from-the-schema)). The trust ledger entry is A11 in
[`design-app-roadmap.md`](design-app-roadmap.md). Apps without `[authz]` build,
extract and verify unchanged, and the new lint checks never fail them.

## Who this is for, and against what

The author writes both kernel and server, often with an agent. The guarantee
is not a check on that author, who can write `permits = true`. It is for
whoever reads the policy: a reviewer, an auditor, the person who asked an agent
for the app. The claim:

> Whatever the reviewed surface says is what the running app enforces,
> however wrong the code behind the guard is, as long as the server acts only
> through `H5iApp` with each request's `Actor`.

The reviewed surface, which does not grow with the app:

- the `Policy`, `View` and `Disclose` impls, and the `Write` type with
  `apply_write` (`permits` judges a write's value; `apply_write` decides what
  it does);
- the one `authz_kernel!` invocation (which type, and the tenant);
- how a request becomes a principal: the `Authenticator` (for `HmacAuth`, the
  principal function given to `HmacAuth::new`), any `or_anonymous`, any
  `Actor::assume_authenticated`. The policy is judged for the principal it is
  given;
- `ReplyCodec::scope`, which decides whose stored replies a repeated request
  gets back;
- the kernel's and server's `Cargo.toml` dependencies on `h5i-app-*`;
- the `main.rs` line that builds the engine from `App` and the generated store,
  and any `Engine::migrate` (it writes outside commands).

Everything else may be wrong: the kernel handlers, everything generated, and
the rest of the server if it acts only through `H5iApp` with the request's
`Actor`, as `rpc_router` does. A handler that writes what the policy forbids,
or replies with a row the caller may not see, gets `Guarded::Denied` (403) and
nothing commits.

The ways around `H5iApp` are `H5iApp::engine()`, whose `execute` takes any
principal, and `Engine::snapshot`/`snapshot_for`, which read the tenant
unguarded. `h5i app lint` fails on those calls in an `[authz]` server and lists
the reviewed-surface calls. It reads source, so it is hygiene, not proof. An
engine handle that accepts only an authenticated `Actor` would be the
structural fix and is not built.

"Authorization" covers which writes commit and which stored rows a reply
discloses. A writing command can still carry hidden data in what it writes
([What is not covered](#what-is-not-covered)).

Out of scope: the author deceiving their own reviewer (a decoy policy type, a
crate renamed to stand in for the framework, code only a macro or `include!`
produces, a fork of h5i-app). `lint` and `check` reject the plain forms anyway,
because an agent working around a failing check produces them too. That is no
defence against someone set on it.

## What an app writes

In the kernel crate, in the Rust subset Aeneas translates:

| Item | Says | Reviewed as |
|---|---|---|
| `impl Policy<A, S, W, D>` | `permits(actor, s, w)`: may the actor make write `w` in `s`; `visible(actor, s, d)`: may it see row `d`; `stored(s, d)`: is `d` in `s` exactly as stored | the policy |
| `impl Disclose<D> for Reply` | the rows a reply carries | complete: an omitted row is not checked |
| `impl View<A, S, C>` | `read_only(cmd)`; `view(actor, s)`, the part of the state the actor may see | the read side of the policy |
| `impl Transition<…>` | delegates `kernel_transition`/`kernel_apply` to the kernel's `transition`/`apply` | wiring; `kernel_apply_eq` proves it |
| `h5i_app_std::authz_kernel! { Forum { … } }` | the types, and a principal's tenant | the types; the tenant |

From one list of types, `authz_kernel!` writes `guarded_transition` (what the
proof is about), `read_only`, and `App` (the kernel behind the guard). The
policy is plain Rust extracted with the kernel, so no generator sits between
what the reviewer reads and the theorem. Handlers check input and
existence only.

The server writes no `Kernel` and no `Store`:

```rust
pub type Forum = forum_kernel::App;
forum_kernel::forum_tables!(Forum, store ForumStore);
Engine::<Forum, ForumStore>::new(pool, EngineConfig::default())
```

```toml
[extract]
start-from = ["guarded_transition", "read_only"]
include = ["h5i_app_sql", "h5i_app_std"]
std-specs = true

[authz]
guarded = ["guarded_transition"]
```

## The guard

`guard` (`crates/h5i-app-std/src/authz.rs`) runs `kernel_transition`, then:

- checks write `i` with `permits` in `kernel_apply(s, ws[..i])`, the kernel's
  own commit semantics, so a command may create a row and then act on it (a
  `DelWhere` is one write, judged before expansion);
- checks that each disclosed row is `stored` exactly as disclosed and
  `visible`, before or after the command. The handler built the reply, so a
  row's own fields are never trusted;
- denies the whole command on one failure.

`guard_viewed` also runs a command `View::read_only` declares on
`view(actor, s)`, denies it if it returns writes, and checks its rows against
the view. Visibility is judged on the view, since a decision on `s` would leak
one bit. The view is computed from the full tenant load.

## What the server runs

`App` is `h5i_app_core::Authz<Forum>`, and coherence allows only one `Kernel`
impl for `Authz<T>`:

- `transition` is `guard_viewed` of `T`'s instances. The guard's theorems hold
  for every `Transition` instance, so what commits is guarded whatever the
  handlers do.
- The types come from `T`'s `AuthzKernel` impl, which only the kernel crate can
  write (orphan rule). `authz_kernel!` writes it and `guarded_transition` from
  the same types, so the engine runs the instantiation that is proven.
- `apply` is `kernel_apply` (`kernel_apply_eq`); `read_only` is the view's
  (`read_only_sound`).

The mapping macro's store (`<mapping>!(App, store Name)`) loads every table
with `schema_load`, stores with `schema_store` (what `Committed.pg_stored` is
about) and keeps the full-tenant `load_for`; a partial load would let the guard
judge a partial state. It needs `writes` and a snapshot in `schema!` or does
not compile, and names the kernel's `decode` and `sql_writes` by absolute path
so a same-named server module cannot replace them.

Outside the proofs: the kernel invokes `authz_kernel!` once at its crate root
(there, a hand-written `guarded_transition`, second `App` or second
`AuthzKernel` impl fails to compile; inside a module, a decoy root
`guarded_transition` could be the proven one), and the server builds its engine
from `App` and the generated store. `h5i app lint` checks the plain form: one
`h5i_app_std::authz_kernel!` by that path at the top level of the crate root,
not under `cfg`, and no hand-written `impl Kernel`, `impl Store` or
`impl AuthzKernel` in any project crate. It parses with `syn` and follows the
module tree as rustc does, but cannot see what a macro or `include!` produces.

Checked rather than trusted:

- `[authz] guarded` accepts only `["guarded_transition"]`, the name the macro
  writes at the root, so the theorems cannot be about a stricter function of
  another name.
- The framework is the framework. `lint` and `check` read `cargo metadata` for
  every crate under the project (a server with its own `[workspace]` resolves
  its own) and fail unless every `h5i_app_*` dependency is the package of that
  name (no `package =` rename) from the same source as `h5i-app-core` (no
  `[patch]` or path look-alike). They also reject `extern crate <other> as
  h5i_app_core;` (or `self as …`), which Cargo does not show; that part reads
  source, so a macro-produced `extern crate` is not seen. A fork of the whole
  framework is out of scope, like a modified compiler.

## What is proven

All theorems are about the extracted code, over every actor, state, command
and every `Transition`, `Policy`, `Disclose` and `View` instance, using only
`propext`, `Classical.choice` and `Quot.sound` (`proofs/Check.lean`). They are
partial correctness, so no app function needs a termination proof.

| Theorem | States | Does not state |
|---|---|---|
| `guard_authorized` | a committed command is `kernel_transition`'s result; write `i` is permitted in `kernel_apply(s, ws[..i])` (`WritesPermitted`); every disclosed row is stored as disclosed and visible in `s` or `kernel_apply(s, ws)` (`ReadsVisible`, `Shown`) | that the policy says what was meant; anything about values derived from rows |
| `guard_app_error`, `guard_only_denies` | refusals pass through; the guard only adds `Denied` | |
| `guard_viewed_authorized` | writing commands as above; a committed read-only command made no writes and disclosed only rows stored and visible in the view (`AuthorizedViewed`) | that `view` hides what `visible` hides |
| `guard_viewed_noninterference` | read-only commands give the same result on two states with the same view, denials and derived values included (`ViewNoninterference`) | anything about writing commands |
| `read_only_sound_of_viewed` | `ReadOnly.Sound`: those commands may run `READ ONLY DEFERRABLE` | |
| `transition_authorized_of_never_denied` | with an app proof of `NeverDenied`, the policy holds of the kernel's own `transition` | |

`WritesPermitted` judges each write after the earlier ones;
`H5iAppLib.WritesAuthorized`, which hand-written proofs use, judges all of them
against the pre-state.

`extract` writes `proofs/generated/Authz.lean` (`import AuthzSpecs`,
`h5i_derive_all`, `h5i_authz forum_kernel.guarded_transition`, then the
storage section). `h5i_authz` reads the instances off `g`'s extracted body,
refuses a `g` that does not pass its own arguments to the guard unchanged or
whose `kernel_apply` is not the kernel's `apply` (by `rfl`), and adds and
prints `g.kernel_apply_eq`, `g.authorized : AuthorizedViewed g … ∧
ViewNoninterference g …` and `g.read_only_sound`. `extract` fails if `[authz]`
is set but the kernel never calls the guard, or `std-specs` is off.

### Storage (A4 for the app)

The engine commits through `schema_store`, not `Kernel::apply`, so a generated
section ties `apply` to the store. Its state is the table encoding itself, so
no hand-written model is needed.

| Theorem | States |
|---|---|
| `Committed.sql_fits` | `sql_write` emits the table writes `sqlA` lists (read off the extracted code; a wrong reading fails here) |
| `Committed.apply_write_enc` | `apply_write` changes `Snapshot.enc` exactly as those writes do |
| `Committed.apply_enc` | committing a write set leaves the encoding of `apply` |
| `Committed.pg_stored` | on PostgreSQL, interleaved with other tenants, the tables hold that state and every load decodes to it, up to row order |

`pg_stored` holds for any `Step`, so for whatever write sets the engine
commits. It is generated when `sql_write` only pushes `schema!` table writes
(forum, board and booking do); otherwise `extract` fails unless the app names
its own A4 theorem as `[authz] storage = "<theorem>"` (in the kernel's
namespace). `check` requires that theorem to state
`H5iAppLib.Obligation.Storage` for the kernel ([Obligations](#obligations)) and
prints it. When every `Write` makes at most one table write (forum and notes),
the generated section also proves `Committed.storage`, that same statement,
from `pg_stored`. The coverage line says which holds (`storage: generated` or
the name).

`pg_stored` leaves the schema hypotheses open (`Pg.Valid ts`,
`Pg.klOf ts = kl`, `ts.length = n`), as board's `db_inv` does. They are checked
at runtime by `h5i_app_pgsql::valid` and by the generated `schema_is_valid`
test, not proven ([`design-app-trust.md`](design-app-trust.md), "Schema
description").

### What `h5i app check` does

1. Re-extracts and fails unless `proofs/generated/` matches (`extract --check`;
   `prove` skips it). The gate checks names and axioms, not statements, so this
   is what catches a hand-edited `Authz.lean`, `AuthzSpecs.lean` or extracted
   kernel. An `[authz]` check needs Charon and Aeneas.
2. Records the theorems it will require, before building.
3. Builds `Authz` itself, so a stale `.olean` is never gated. `Authz`,
   `AuthzSpecs` and `Schema` must be `lean_lib` roots, or it fails with
   "unknown target `Authz`" and a hint.
4. Gates `g.authorized`, `g.kernel_apply_eq`, `g.read_only_sound` and storage,
   checks each obligation's theorem against its statement
   ([Obligations](#obligations)), then writes the coverage line
   (`Run.authorization`).

## Obligations

h5i fixes the statement of each security property; the framework proves it
once where it can; an app may supply its own proof only of the same
statement. Each obligation is a `def … : Prop` in
`crates/h5i-app-core/proofs/H5iAppLib/Obligation.lean`, parameterised by the
app's extracted functions. `crates/h5i-app-cli/src/obligations.rs`
instantiates it for a project from the extraction and the schema, never from
the app's proofs, and `check`'s gate program elaborates that statement and
requires the theorem's type to be definitionally equal to it. A weaker or
different statement fails with the expected type printed:

```text
obligation `read-only`: calculator_kernel.Proofs.memory_of_spec does not state it. Expected (H5iAppLib.Obligation, for this app):
    H5iAppLib.Obligation.ReadOnlySound calculator_kernel.read_only calculator_kernel.transition ?m.8
```

| Obligation | Statement, for the app's | Discharged by |
|---|---|---|
| `ReadOnlySound` | `read_only`, `transition` (or `guarded_transition`): an accepted read-only command writes nothing | `[authz]`: `g.read_only_sound`; otherwise `[obligations] read-only` |
| `Storage` | `apply`, `sql_writes`, `decode`, `Snapshot.enc`, `Loads`: on `H5iAppLib.Pg`, every load after any accepted write sets, interleaved with other tenants, decodes to the snapshot `apply` computed, up to row order (`Reached`) | `Committed.storage` (generated), or `[authz] storage` / `[obligations] storage` |
| `PanicFree` | the function the engine runs: `∀ a s c, f a s c ⦃ _ => True ⦄` (no panic, overflow, bad index; total correctness, so no divergence either) | `[obligations] panic-free`, else automation (`step*`) |
| `TenantIsolation` | `valKind`: another tenant's compiled statements leave this tenant's rows unchanged, and a compiled `SELECT` returns only what this tenant's rows determine | `H5iAppLib.Obligation.tenant_isolation`, proven once from `Pg.compileAll_sound` and `Pg.select_sound` |
| `Authorized` | `transition`, with the app's `allowed` and `visible` | `[obligations] authorized` |
| `PolicySpec` | `policy::permits`, `visible`, `stored` compute `PolicySpec.Permits`, `Visible`, `Stored` | generated with a `schema!` policy ([below](#a-policy-from-the-schema)) |

`[obligations]` names the theorem for a slot (`read-only`, `panic-free`,
`authorized`, `storage`); `[authz] storage` is the same slot as
`[obligations] storage`. A theorem under `[check] theorems` only is gated for
its axioms and claims no obligation, so existing manifests mean what they
meant. `lint` asks a `Kernel` that overrides `read_only` to name its proof as
`[obligations] read-only`; a proof that only mentions `ReadOnly.Sound` is still
accepted, with a note that its statement is not checked.

Where the statement has a part the app chooses, it is a `_` and `check`
prints what it resolved to: the write list of a write set that is not a `Vec`
(calculator's `Option Memory`, resolved to its `ReadOnly.writesOf`), and a
hand-written policy's `allowed` and `visible`. Those parts are reviewed (A10).
Definitional equality also accepts a theorem about another function with the
same body, which is then the same function.

An obligation no theorem is named for is tried by automation in a separate
Lean run (`.lake/h5i/Auto.lean`, at most 900 s). A failed tactic leaves the
theorem with `sorryAx`, so it is reported `open`, and the check still passes.
`check` records each obligation's status (`proven`, `automation`, `open`,
`failed`) in its run, and `h5i app report` lists them per project, with the
ones not stated and why.

Group A, the statements that need no input from the app, on the examples
(`h5i app check` on 5 November 2026):

| Project | read-only | storage | panic-free | tenant isolation |
|---|---|---|---|---|
| forum | proven (guard) | proven (`Committed.storage`) | open | proven (library) |
| notes | proven (guard) | proven (`Committed.storage`) | open | proven (library) |
| calculator | proven (`ReadOnly.sound`) | not stated (no `[authz]`; `pg_stored` is the app's) | proven (automation) | proven (library) |
| board, booking, inbox, ledger | none (no read-only commands) | not stated (`db_inv` is the app's) | proven (`Theorems.panic_free`, from `transition_total`) | proven (library) |
| parallel | none | not stated | no `transition` | proven (library) |

Panic freedom of `guarded_transition` stays open: `step*` does not get through
the guard's loops, and nobody has proven a total spec of `guard_viewed`.
Tenant isolation is a statement about the compiler and the database model; it
leaves trusted that the engine and the generated store pass `K::tenant(actor)`
consistently (A6) and that `authz_kernel!`'s `tenant` is right (A11).

Not stated, and why: the `ReplyCodec` (fingerprint injectivity,
`decode (encode r) = ok r`, a scope without `/`) is implemented in each
server crate with `String` and serde, outside the extracted subset. Stating
those needs the codec moved into the kernel crate in the Aeneas subset (bytes
as `Vec<u8>`, explicit loops), then the statements fit next to the others.

## A policy from the schema

An app may declare its policy instead of writing it. A `policy` block after
`schema!`'s mapping line generates the `Policy` impl, in the Aeneas subset, and
with `lean "..."` the generated `Schema.lean` states what the block means and
proves the extracted Rust computes it. The app gets the `[authz]` theorems with
no Lean. [`examples/app/notes`](../../examples/app/notes/) uses it:

```rust
policy Notes {
    principal Principal.user;
    write Write;
    row Row;
    admin members.admin;
    table Member in members key user: u64 {
        put PutMember del DelMember row OfMember owner user
        write [admin, first] visible [owner, admin]
    }
    table Note in notes key id: u64 {
        put PutNote del DelNote row OfNote owner owner
        write [owner, admin] visible [public, owner, admin]
    }
}
```

With `u` the principal's `user` field:

| Atom | A new row `x` | Replacing or deleting the stored `old` | `visible` of `x` |
|---|---|---|---|
| `owner` | `x.owner == u` | `old.owner == u` | `x.owner == u` |
| `admin` | `u`'s row of the `admin` table has the column set | same | same |
| `first` | the table is empty | never | (not an atom) |
| `all` | | | always |
| a column name | | | the row's `bool` column |

A put creates when no row has its key, else replaces, and a replacement keeps
the owner column. A delete of a key that is not stored is refused. `stored`
looks the row up by key and compares the whole row. `view` keeps each table's
visible rows; every snapshot field must be a policy table, so nothing is
copied into the view unjudged. The `Write` and `Row` enums are the app's: a
variant without a rule does not compile. Keys are one column.

`schema!` writes `pub struct Notes`, `impl Policy for Notes` delegating to a
`policy` module (`policy::permits`, `visible`, `stored`, `view`, `is_admin`,
and per table `policy::<field>::{find, put, del, visible, stored, view}`,
each a short loop or `match`), in `crates/h5i-app-schema/src/policy.rs`.
`notes_kernel.PolicySpec` states the same rules over lists
(`Note.find = List.find?`, `Permits`, `Visible`, `Stored` as `Prop`s), rendered
by `crates/h5i-app-schema/src/lean.rs` from the block, not from the Rust; the
theorems `permits_spec`, `visible_spec` and `stored_spec` prove the extracted
functions compute it, and `check` requires them as obligations. The spec is
never derived from the code it checks: two generators read the block, and the
theorems compare them.

The reviewed surface of an app that uses it is the block in place of the
`Policy` impl. The generators (`policy.rs`, and `render_policy` for what
`PolicySpec` says) join the surface reviewed once for every app, like
`authz_kernel!`; the theorems mean a mistake in one shows as a failed proof
unless both make the same one. The `Policy` impl's delegation to
`policy::*` is generated wiring, reviewed with the generator.
`notes`'s `generated_policy_equals_the_hand_written_one` compares the
generated policy with one written in the forum's style on 1,323 states, four
actors, 35 writes and 30 rows, and `the_view_is_the_visible_rows` the view.

### Mutants

`h5i app mutate --security` adds authorization bugs and names the proofs each
one breaks (results in [`design-app-numbers.md`](design-app-numbers.md),
"Security mutants"). Under the guard a policy change breaks no proof unless
something pins the policy: the forum's hand-written policy mutants survive,
which is the expected result and says that A11's review of `Policy` is all
there is. In notes the same bugs, made to the generator, break `PolicySpec`.
Removing the guard is refused by `extract` before any proof runs (`[authz]
guarded` names a function that never calls the guard), which `mutate` counts
as invalid rather than caught; skipping the guard's write check breaks
`guard_authorized` and `guard_only_denies`. Fixing every principal's tenant to 0 breaks nothing: the
tenant function is trusted (A11), and tenant isolation is about the compiler,
not about which tenant a principal is put in. Specs no mutant here breaks: `read_only_sound` under `[authz]` (it holds for
every kernel, since the guard denies a read-only command that writes),
`TenantIsolation` (it depends on no app code), and `Committed.storage` (no
security mutant touches `apply_write` or `sql_write`; the `[[mutant]]`s and
`--auto` ones of other apps do that for their storage proofs).

### Toward filtering reads in SQL (not built)

Reads that load the whole tenant are what lets `guard_viewed` judge the full
state. A per-actor `SELECT … WHERE` would need an obligation that the compiled
read returns a superset of the rows the view keeps, so the view of the
filtered load equals the view of the full load. That obligation rests on
`compile_read`, which is differential-tested, not proven
([`design-app-trust.md`](design-app-trust.md)); it should be stated once
`compile_read` is extracted and proven against `H5iAppLib.Pg`, as `Obligation`
over the read plan and `View::view`.

## What stays trusted

On the code path the theorem rests on A5 to A9 of
[`design-app-roadmap.md`](design-app-roadmap.md) and A4's trusted half
(PostgreSQL behaves as `H5iAppLib.Pg` says). A8 is the "acts only through
`H5iApp`" condition; `engine()`, `snapshot` and `migrate` get around it, and
neither the opaque `Tx` nor `cargo deny` covers them. The app adds:

- The policy means what was intended, `stored` included: a `stored` that does
  not compare a row with the stored one lets a handler relabel a hidden row.
- `Disclose` lists every row (not needed for read-only commands under
  `guard_viewed`).
- `View` hides what `visible` hides. The forum's `view` keeps exactly the rows
  `post_visible` and `user_visible` accept and drops the counter.
- Handler bugs still ship, as 403s. `NeverDenied` or a test like
  `handler_bugs_are_denied` finds them first.
- `authz_kernel!` builds `App` and the server runs it on the generated store
  (lint checks the plain form).
- The `h5i-app-*` dependencies are the framework (`lint` and `check` reject
  renames and look-alikes).
- For a policy from `schema!`'s `policy` block: the block says what was meant
  (it replaces the `Policy` impl in the review), and `policy.rs` and
  `render_policy` are reviewed once, like `authz_kernel!`.
- `Authz<T>`, `authz_kernel!` and the generated store, reviewed once for every
  app like the rest of the shell.

No longer trusted: that the server calls the guard, that the theorems are about
what `App` runs, that `kernel_apply` is `apply`, that `sql_writes` and
`decode` store and load what `apply` computes, and that a hand-named storage
or read-only theorem says what it should (`check` holds it to the
obligation's statement).

## What is not covered

For a writing command the guarantee is integrity and the disclosure of stored
rows, not confidentiality of what the handler read.

- Write content. The handler sees the full state and can copy a hidden row
  into a permitted write (an edit of one's own post whose body is someone's
  draft); see `writes_can_carry_what_the_caller_cannot_see`. Review writing
  handlers for it, or keep the command read-only. Running writing commands on
  the view is not built: the view would have to hold what writes depend on,
  which can itself disclose.
- Derived values in a writing command's reply. A count from hidden rows is not
  a row. The forum's `CreatePost` replies `Created(id)`, revealing the counter
  the view hides.
- Replayed replies. A repeated idempotency key gets its stored reply without
  running the guard (`Attempt::Replayed` in `crates/h5i-app-pg`). With a
  per-user `ReplyCodec::scope` (the forum's `u{user}`) a user only gets back
  what they were shown, but can still replay a reply after losing the right it
  needed. A coarser scope lets one user replay another's reply, such as an admin's listing with
  drafts. Purge stored replies when rights change if that matters.
- Time. `Authz<T>` keeps the default `Kernel::stamp`; a time-dependent policy
  gets time from the principal function.
- Existence. `NotFound` versus `Denied` can reveal that a row exists.
- Failures. A kernel that panics or loops does so behind the guard too.
- Apps without `[authz]`.

## Tests and cost

`handler_bugs_are_denied` runs four buggy handlers (post takeover on edit,
drafts disclosed on publish, admin on registration, a draft relabelled as
published) behind the forum's policy; all are denied. `tests/postgres.rs` runs
600 random commands from four users on PostgreSQL and `MemoryEngine` and they
agree, denials included. Under the guard neither a handler bug nor a policy
change breaks a proof; `h5i app mutate --security` shows which specs do pin
the policy ([Mutants](#mutants)).

The guard copies the snapshot per write prefix, once after, and once for the
view: linear in tenant size, the same order as the engine's full-tenant load.
`stored` is a lookup per disclosed row, quadratic for a listing on `Vec`
snapshots (18.6 ms for 10,000 posts). See
[`design-app-numbers.md`](design-app-numbers.md).

## Not built

- A Cedar front end generating `Policy` impls. `schema!`'s `policy` block
  ([above](#a-policy-from-the-schema)) is a smaller one, whose output is
  proven against a spec rendered from the block; linking to cedar-lean's
  soundness theorems is the stretch goal.
- Panic freedom of `guarded_transition` (open on every `[authz]` example) and
  statements about the `ReplyCodec`, which is not extracted.
- A Lean-side differential test of the forum's policy: in the pinned Aeneas
  `Result` is coinductive, so extracted code does not `#eval`. Aeneas's
  faithfulness stays A7, backed by the document service's 55k-case test.
- An engine handle that accepts only an authenticated `Actor`.
