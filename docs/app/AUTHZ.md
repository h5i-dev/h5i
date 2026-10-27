# Framework-proven authorization

An h5i-app server can get a proven authorization theorem without writing Lean.
The app writes its policy in Rust, runs its kernel behind the framework's
guard, and lists the guarded function in `h5i-app.toml`. The theorem is proven
once, in `crates/h5i-app-std/proofs/AuthzSpecs.lean`, for every kernel;
`h5i app extract` instantiates it for the app in a generated one-line file.

The worked example is [`examples/app/forum`](../../examples/app/forum/), a port of
Nyxis's bulletin board (drafts, admins, self-registration). It has no
hand-written Lean. The design and its alternatives are in
[`docs/design/design-authz.md`](../design/design-authz.md).

## What an app writes

In the kernel crate, in the Rust subset Aeneas translates:

| Item | Says | Reviewed as |
|---|---|---|
| `impl Policy<A, S, W, D>` | `permits(actor, s, w)`: may the actor make write `w` in state `s`; `visible(actor, s, d)`: may it see row `d`; `stored(s, d)`: is `d` a row of `s`, exactly as stored (look it up by key and compare) | the policy |
| `impl Disclose<D> for Reply` | the rows a reply carries | complete: a row left out is not checked |
| `impl View<A, S, C>` | `read_only(cmd)`, and `view(actor, s)`: the part of the state the actor may see | the read side of the policy |
| `impl Transition<A, S, C, W, R, E>` | delegates `kernel_transition` and `kernel_apply` to the kernel's `transition` and `apply` | wiring, checked: `h5i_authz` proves `kernel_apply` is the kernel's `apply` |
| `h5i_app_std::authz_kernel! { Forum { … } }` | the types, and the tenant of a principal | the types; the tenant |

`authz_kernel!` writes, from one list of types, `guarded_transition` (the
function the generated proof is about), `read_only`, and `App`: the kernel
behind the guard, as the server runs it. The kernel crate depends on
`h5i-app-core` for `App`; none of it is extracted.

The server writes no `Kernel` and no `Store`. It runs `forum_kernel::App` on
the store `schema!` generates, and maps `Guarded::Denied` to 403:

```rust
pub type Forum = forum_kernel::App;
forum_kernel::forum_tables!(Forum, store ForumStore);
// …
Engine::<Forum, ForumStore>::new(pool, EngineConfig::default())
```

In `h5i-app.toml`:

```toml
[extract]
start-from = ["guarded_transition", "read_only"]
include = ["h5i_app_sql", "h5i_app_std"]
std-specs = true

[authz]
guarded = ["guarded_transition"]
```

## What the server runs

`App` is `h5i_app_core::Authz<Forum>`. `h5i-app-core` has one `Kernel` impl
for `Authz<T>`, and coherence allows no other, so its methods are what the
engine calls:

- `transition` is `h5i_app_std::authz::guard_viewed` of `T`'s `Transition`,
  `Policy` and `View` and the reply's `Disclose`. The guard's theorems hold
  for *every* `Transition` instance, so whatever the handlers do, what
  commits is guarded. No per-app wiring decides this.
- The types come from `T`'s `AuthzKernel` impl, which only the kernel crate
  can write (orphan rule). `authz_kernel!` writes it and `guarded_transition`
  from the same types, so the guard instantiation the engine runs is the one
  `guarded_transition.authorized` is about.
- `apply` is `T`'s `kernel_apply` (`kernel_apply_eq`: the kernel's `apply`)
  and `read_only` is the view's (`read_only_sound`).

The store the mapping macro generates (`<mapping>!(App, store Name)`) loads
every table with `schema_load` and stores with `schema_store`, the code
`Committed.pg_stored` is about, and keeps the default full-tenant `load_for`.
A partial load would have the guard judge the policy on a partial state (in
the forum, registering as admin while `users` is not loaded).

Two facts stay outside the proofs:

- the kernel builds `App` with `authz_kernel!`, once. A kernel that wrote
  `guarded_transition` by hand and an `App` over another type would have its
  theorems about one instantiation and its engine running another. With the
  macro, coherence and name resolution leave no room: a second `App` at the
  crate root is a duplicate name, and a second `AuthzKernel` impl for the
  same type a conflict;
- the server builds its engine from `forum_kernel::App` and the generated
  store, one line of `main.rs`, rather than from a kernel or store of its
  own.

`h5i app lint` checks the plain form of both: the kernel invokes
`h5i_app_std::authz_kernel!` (by that path) exactly once, and no crate of the
project hand-writes an `impl Kernel`, `impl Store` or `impl AuthzKernel`.
That is hygiene for the plain mistake, not a guarantee: it reads the source
with `syn`, so what only a macro or `include!` produces is not seen.

Two things are checked, not trusted:

- **The theorems are about `guarded_transition`.** `[authz] guarded` accepts
  only `["guarded_transition"]`, the function `authz_kernel!` writes, so the
  proofs cannot be about another function with a stricter policy while `App`
  runs `Forum`'s.
- **The framework is the framework.** `authz_kernel!`, `Authz<T>` and the
  generated store name `::h5i_app_core`, `::h5i_app_std`, `::h5i_app_pg` and
  `::h5i_app_sql`, which each crate's `Cargo.toml` resolves. `lint` and
  `check` read Cargo's resolution (`cargo metadata`) and fail unless every
  `h5i_app_*` dependency of the project's crates is the package of that name
  (no `package =` rename) and comes from the one place `h5i-app-core` comes
  from (no `[patch]` or path to a look-alike). Otherwise a stand-in
  `h5i_app_core` could supply an `Authz<T>` that skips the guard, with the
  Rust and the extracted Lean unchanged. A fork of the whole framework is out
  of scope, like a modified compiler.

The store form needs `writes` and a snapshot in `schema!`; without them it
does not compile, since its `schema_store` and `schema_load` would otherwise
resolve to functions the server writes.

The generated store names the kernel's `decode` and `sql_writes` by absolute
path (`::forum_kernel::…`), so a server module that happens to be called
`forum_kernel` cannot stand in for them.

Handlers need no checks of *who may write what*: in the forum they check
input and existence only, and the `Policy` impl alone decides. A handler
builds the reply, so the guard never trusts a row's own fields either: a
disclosed row counts only if `stored` finds it, exactly as disclosed, in the
state before or after the command.

What a handler of a command that *writes* is still trusted with is
confidentiality: it runs on the full state, and the guard does not see where
a write's content or a derived reply value came from
([below](#what-is-not-covered)). Read-only commands under `guard_viewed` are
confined to the caller's view and carry no such trust.

## What is proven

All theorems are about the extracted code, quantified over every actor, state
and command, and over every `Transition`, `Policy`, `Disclose` and `View`
instance. All use only `propext`, `Classical.choice` and `Quot.sound`
(`crates/h5i-app-std/proofs/Check.lean`). Partial correctness: each is about a
run that returned, so no app function needs a termination proof.

| Theorem | States | Does not state |
|---|---|---|
| `guard_authorized` | a committed command is the kernel's own result (`kernel_transition` returned it); write `i` is permitted in `kernel_apply(s, ws[..i])`; every disclosed row is stored as disclosed and visible, in `s` or in `kernel_apply(s, ws)` (`WritesPermitted`, `ReadsVisible`, `Shown`) | that the policy says what was meant; anything about values a reply derives from rows (counts, flags) |
| `guard_app_error`, `guard_only_denies` | the guard passes the kernel's refusals through, and changes nothing but adding `Denied` | — |
| `guard_viewed_authorized` | with `guard_viewed`: a command that writes is `guard_authorized`'s case; a committed read-only command made no writes and disclosed only rows stored and visible in the actor's view (`AuthorizedViewed`) | that `view` hides what `visible` hides: `view` is part of the reviewed policy |
| `guard_viewed_noninterference` | for read-only commands, two states with the same view give the same result, refusals and denials included (`ViewNoninterference`): nothing outside the view reaches the reply, derived values included | anything about commands that write |
| `read_only_sound_of_viewed` | `H5iAppLib.ReadOnly.Sound` for the view's `read_only`: the engine may run those commands in a read-only transaction | — |
| `transition_authorized_of_never_denied` | if the app also proves `NeverDenied` (the guard never fires), the policy holds of the kernel's own `transition`: the static guarantee | — |

For each `[authz] guarded` function `g`, `h5i app extract` writes
`proofs/generated/Authz.lean`:

```lean
import AuthzSpecs
import Schema
open Aeneas Aeneas.Std Result H5iAppLib forum_kernel
h5i_derive_all
h5i_authz forum_kernel.guarded_transition
-- then the storage section, below
```

`h5i_authz` reads the instances off `g`'s extracted body. It refuses a `g`
that does not pass its own actor, state and command to the guard unchanged,
and an instance whose `kernel_apply` is not the kernel's `apply`; then it adds
`g.kernel_apply_eq`, `g.authorized : AuthorizedViewed g … ∧
ViewNoninterference g …` and `g.read_only_sound`. It prints the statements.
(`h5i_authz` also accepts a `g` built on plain `guard`, proving `Authorized
g …` only, but `Authz<T>` runs `guard_viewed`, so an `[authz]` app uses
`authz_kernel!`'s `guarded_transition`.)

The storage section proves, for this app, that the store commits what the
kernel's `apply` computes (A4), with no hand-written model:

| Theorem | States |
|---|---|
| `Committed.sql_fits` | `sql_write` emits the table writes `sqlA` lists (read off the extracted `sql_write`; a wrong reading fails here) |
| `Committed.apply_write_enc` | `apply_write` changes `Snapshot.enc` exactly as those table writes do |
| `Committed.apply_enc` | committing a write set leaves the encoding of the kernel's `apply` |
| `Committed.pg_stored` | on PostgreSQL, interleaved with other tenants' statements, the tenant's tables hold that state and every load decodes to it, up to row order |

With `kernel_apply_eq`, the states the policy judges writes in are the states
the database holds. `extract` generates the section when the kernel uses
`schema!` with `writes` and every arm of `sql_write` only pushes table
writes (the forum, board and booking all do). Otherwise `extract` fails,
unless the app proves A4 by hand (as board's `Storage.lean` does) and names
that theorem as `[authz] storage = "<theorem>"`; `check` then requires it
instead. The storage requirement never silently disappears, and the coverage
line says which one holds (`storage: generated` or the theorem's name).

`pg_stored` keeps the schema's own hypotheses open, as board's `db_inv`
does: `Pg.Valid ts`, `Pg.klOf ts = kl` and `ts.length = n` for the server's
`schema_spec()`. They are checked at runtime (every compile and load checks
`h5i_app_pgsql::valid`) and by the generated `schema_is_valid` test, not
proven (TRUST.md, "Schema description").

`h5i app check` builds the `Authz` module itself, so it never gates a stale
`.olean`, and gates `g.authorized`, `g.kernel_apply_eq`, `g.read_only_sound`
(`App` declares the view's read-only commands) and the storage theorems (or
`[authz] storage`). `Authz`, `AuthzSpecs` and `Schema` must be in some
`lean_lib`'s roots: otherwise lake has no `Authz` target and `check` fails
with "unknown target `Authz`" and says to add it. Each run records the
theorems it required before it builds anything, and the web view lists the
latest run's.

`[authz] storage = "<theorem>"` must name a theorem in the kernel's
namespace; `check` prints its statement. What that statement says is not
checked: review it (it should be the shape of `pg_stored`).

## What stays trusted

On the code path, the theorem rests on A5 to A9 of [`ROADMAP.md`](ROADMAP.md)
like every other h5i-app theorem, and on A4's trusted half (PostgreSQL runs
statements as `H5iAppLib.Pg` says); the app's half of A4 is the generated
storage section above. The new trust is in what the app writes, and it is
small enough to review:

- **The policy means what was intended.** The Rust `Policy` impl *is* the
  specification, `stored` included: one that accepts a row without comparing
  it to the stored one lets a handler relabel a hidden row. There is no code generator between it and the theorem, so
  nothing translates it wrongly, but nothing checks it against intent either.
- **`Disclose` lists every row.** A reply field holding a row that `disclosed`
  omits is not checked. Read-only commands under `guard_viewed` do not depend
  on this: noninterference covers their whole reply.
- **`View` hides what `visible` hides.** Noninterference is relative to the
  view. In the forum, `view` keeps exactly the rows `post_visible` and
  `user_visible` accept, and drops the counter (the next id would count drafts).
- **The handler bugs the guard turns into denials still ship.** An
  authorization bug in a handler is a 403 at runtime, not a failed proof.
  `NeverDenied`, or the forum's `handler_bugs_are_denied` test, is how an app
  learns about it before users do.

- **The kernel builds `App` with `authz_kernel!`, and the server runs it on
  the generated store.** One macro invocation and one line of `main.rs`
  ([above](#what-the-server-runs)); lint checks their plain form.
- **A hand-named storage theorem says what it should.** Only with
  `[authz] storage`; `check` prints it.
- **The framework's `Authz<T>`, `authz_kernel!` and the generated store**,
  like the rest of h5i-app's shell (A8): each is a few lines, reviewed once
  for every app.

Not trusted any more: that the server calls the guard (`Authz<T>` does);
that `kernel_apply` is the kernel's `apply` (`g.kernel_apply_eq`); that the
kernel's `sql_writes` and `decode` store and load what that `apply` computes
(`Committed.pg_stored`), which the generated store calls. `pg_stored` holds
for any write sets the engine commits (its `Step` is arbitrary), so it needs
no link to the guard; the guard's theorem says which write sets those are.

## What is not covered

The guarantee for a command that writes is integrity (only permitted writes
commit) and the disclosure of stored rows. It is not confidentiality of what
the handler read:

- **Write content.** A handler sees the full state and can copy a row the
  caller cannot see into a write the caller may make: an edit of one's own
  post whose body is another user's draft. Each write is permitted, no row is
  disclosed, and the guard commits it. The forum pins this down in
  `writes_can_carry_what_the_caller_cannot_see`. Nyxis rules it out by
  construction (its handlers only see rows the caller may read); here, review
  a write command's handler for it, or keep the command read-only.
- **Derived values in a write command's reply.** A count or flag computed
  from hidden rows is not a row, so `Disclose` cannot list it. Under
  `guard_viewed`, a read-only command's whole reply is covered by
  noninterference; a write command's is not. In the forum, the view hides the
  counter because the next id would count drafts, yet `CreatePost` replies
  `Created(id)`, which tells its author that number.
- **Replayed replies.** A request repeated with the same idempotency key
  gets its stored reply back without running `transition` again
  (`crates/h5i-app-pg`, `Attempt::Replayed`), so without the guard. Keys are
  scoped per user (`ReplyCodec::scope`), so a user only gets back what was
  shown to them, but after losing a right they can still replay a reply that
  needed it: stale access, not escalation. Expire or purge stored replies
  when rights change if that matters.
- **Existence.** Whether a write command was refused by the kernel
  (`NotFound`) or by the policy (`Denied`) can tell an actor that a row exists.
- **Failures.** The guard checks after `transition`, so a kernel that panics
  or loops on some state does so behind the guard too.
- **Apps without `[authz]`** get nothing from this page.

## Cost

The guard runs `kernel_apply` once per write (on the prefix) and once more for
the state after the command, and `guard_viewed` builds the view for read-only
commands: a few snapshot copies per command, linear in the tenant's size. The
engine already loads and decodes the whole tenant from PostgreSQL per
command. `stored` is one lookup per disclosed row; on `Vec` snapshots a
listing is quadratic in its length (18.6 ms for 10,000 posts in the forum). Measurements: [`NUMBERS.md`](NUMBERS.md#framework-proven-authorization).

## Not built

- A Cedar front end (design Z7): the policy is Rust.
- A Lean-side differential test of the forum's extracted policy. In the pinned
  Aeneas `Result` is coinductive, so the extracted code does not `#eval`, and
  scenario proofs (`h5i_eval`) need per-app loop specs, which is the
  hand-written Lean this page avoids. Aeneas's faithfulness is A7, backed by the
  document service's 55k-case differential test.
