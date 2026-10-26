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
| `impl Policy<A, S, W, D>` | `permits(actor, s, w)`: may the actor make write `w` in state `s`; `visible(actor, s, d)`: may it see row `d` | the policy |
| `impl Disclose<D> for Reply` | the rows a reply carries | complete: a row left out is not checked |
| `impl View<A, S, C>` (optional) | `read_only(cmd)`, and `view(actor, s)`: the part of the state the actor may see | the read side of the policy |
| `impl Transition<A, S, C, W, R, E>` | delegates `kernel_transition` and `kernel_apply` to the kernel's `transition` and `apply` | wiring |
| `guarded_transition` | `guard::<…>` or `guard_viewed::<…>` of the impls | wiring |

In the server, `Kernel::transition` calls `guarded_transition`, and
`Kernel::Error` is `Guarded<Error>`: `Guarded::Denied` maps to 403. In
`h5i-app.toml`:

```toml
[extract]
start-from = ["guarded_transition", "read_only"]
include = ["h5i_app_sql", "h5i_app_std"]
std-specs = true

[authz]
guarded = ["guarded_transition"]
```

Handlers need no authorization checks of their own. In the forum, they check
input and existence only; who may do what is the `Policy` impl alone.

## What is proven

All theorems are about the extracted code, quantified over every actor, state
and command, and over every `Transition`, `Policy`, `Disclose` and `View`
instance. All use only `propext`, `Classical.choice` and `Quot.sound`
(`crates/h5i-app-std/proofs/Check.lean`). Partial correctness: each is about a
run that returned, so no app function needs a termination proof.

| Theorem | States | Does not state |
|---|---|---|
| `guard_authorized` | a committed command is the kernel's own result (`kernel_transition` returned it); write `i` is permitted in `kernel_apply(s, ws[..i])`; every disclosed row is visible in `s` or in `kernel_apply(s, ws)` (`WritesPermitted`, `ReadsVisible`) | that the policy says what was meant; anything about values a reply derives from rows (counts, flags) |
| `guard_app_error`, `guard_only_denies` | the guard passes the kernel's refusals through, and changes nothing but adding `Denied` | — |
| `guard_viewed_authorized` | with `guard_viewed`: a command that writes is `guard_authorized`'s case; a committed read-only command made no writes and disclosed only rows visible in the actor's view (`AuthorizedViewed`) | that `view` hides what `visible` hides: `view` is part of the reviewed policy |
| `guard_viewed_noninterference` | for read-only commands, two states with the same view give the same result, refusals and denials included (`ViewNoninterference`): nothing outside the view reaches the reply, derived values included | anything about commands that write |
| `read_only_sound_of_viewed` | `H5iAppLib.ReadOnly.Sound` for the view's `read_only`: the engine may run those commands in a read-only transaction | — |
| `transition_authorized_of_never_denied` | if the app also proves `NeverDenied` (the guard never fires), the policy holds of the kernel's own `transition`: the static guarantee | — |

For each `[authz] guarded` function `g`, `h5i app extract` writes
`proofs/generated/Authz.lean`:

```lean
import AuthzSpecs
open Aeneas Aeneas.Std Result H5iAppLib forum_kernel
h5i_derive_all
h5i_authz forum_kernel.guarded_transition
```

`h5i_authz` reads the instances off `g`'s extracted body and adds
`g.authorized : AuthorizedViewed g … ∧ ViewNoninterference g …` (or
`Authorized g …` for `guard`) and, for `guard_viewed`, `g.read_only_sound`. It
prints both statements. `h5i app check` gates them by name.

## What stays trusted

Nothing new about the code path: the theorem rests on A4 to A9 of
[`ROADMAP.md`](ROADMAP.md) like every other h5i-app theorem. The new trust is
in what the app writes, and it is small enough to review:

- **The policy means what was intended.** The Rust `Policy` impl *is* the
  specification. There is no code generator between it and the theorem, so
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

Three things the guard does not hide: whether a write command was refused by
the kernel (`NotFound`) or by the policy (`Denied`) can tell an actor that a
row exists; the guard checks after `transition`, so a kernel that panics
or loops on some state does so behind the guard too; and an app that does not
list a function in `[authz]` gets nothing from this page.

## Cost

The guard runs `kernel_apply` once per write (on the prefix) and once more for
the state after the command, and `guard_viewed` builds the view for read-only
commands: a few snapshot copies per command, linear in the tenant's size. The
engine already loads and decodes the whole tenant from PostgreSQL per
command. Measurements: [`NUMBERS.md`](NUMBERS.md#framework-proven-authorization).

## Not built

- A Cedar front end (design Z7): the policy is Rust.
- A Lean-side differential test of the forum's extracted policy. In the pinned
  Aeneas `Result` is coinductive, so the extracted code does not `#eval`, and
  scenario proofs (`h5i_eval`) need per-app loop specs, which is the
  hand-written Lean this page avoids. Aeneas's faithfulness is A7, backed by the
  document service's 55k-case differential test.
