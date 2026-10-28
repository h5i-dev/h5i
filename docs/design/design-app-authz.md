# Framework-proven authorization

An h5i-app server can get a proven authorization theorem without writing Lean.
The app writes its policy in Rust, runs its kernel behind the framework's
guard, and lists the guarded function in `h5i-app.toml`. The theorem is proven
once, for every kernel, in `crates/h5i-app-std/proofs/AuthzSpecs.lean`;
`h5i app extract` instantiates it for the app in a generated file.

The worked example is [`examples/app/forum`](../../examples/app/forum/), a port
of Nyxis's bulletin board (drafts, admins, self-registration) with no
hand-written Lean. The trust ledger entry is A11 in
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
namespace). `check` prints that theorem but does not check what it says. The
coverage line says which holds (`storage: generated` or the name).

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
   then writes the coverage line (`Run.authorization`).

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
- A hand-named `[authz] storage` theorem says what it should.
- `Authz<T>`, `authz_kernel!` and the generated store, reviewed once for every
  app like the rest of the shell.

No longer trusted: that the server calls the guard, that the theorems are about
what `App` runs, that `kernel_apply` is `apply`, and that `sql_writes` and
`decode` store and load what `apply` computes.

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
agree, denials included. `h5i app mutate` is not adapted: under the guard
neither a handler bug nor a policy change breaks a proof.

The guard copies the snapshot per write prefix, once after, and once for the
view: linear in tenant size, the same order as the engine's full-tenant load.
`stored` is a lookup per disclosed row, quadratic for a listing on `Vec`
snapshots (18.6 ms for 10,000 posts). See
[`design-app-numbers.md`](design-app-numbers.md).

## Not built

- A Cedar front end generating `Policy` impls. The generator would become
  trusted; linking to cedar-lean's soundness theorems is the stretch goal.
- A Lean-side differential test of the forum's policy: in the pinned Aeneas
  `Result` is coinductive, so extracted code does not `#eval`. Aeneas's
  faithfulness stays A7, backed by the document service's 55k-case test.
- An engine handle that accepts only an authenticated `Actor`.
