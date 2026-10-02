# Proofs for the document service

These are Lean 4 proofs about the Rust kernel in `../kernel`, as translated by
Charon and Aeneas. The files in `generated/` are produced by tools and never
edited: `DocsKernel.lean` comes from `scripts/app/extract.sh` and `Schema.lean`
from `schema!`. Everything else is written by hand.

## Files

| File | Contents |
|---|---|
| `Spec.lean` | The specification to review: the policy table, the meaning of a write set on lists, the invariants, authorization of effects, and what each user may see. |
| `Columns.lean` | The SQL encoding of the application's own column types, `Role` and `Status`. |
| `Lemmas.lean`, `Transition.lean`, `Apply.lean` | Specifications of the kernel's helpers and of `apply`, mostly by way of the loop lemmas in `H5iAppLib`. |
| `Theorems.lean` | Totality, authorization and reply confinement. |
| `Invariants.lean` | The invariants hold in every reachable state. |
| `Noninterference.lean` | A user's result depends only on what that user may see. |
| `Frame.lean` | A command's result depends only on the rows its scope names. |
| `Check.lean` | The invariant checker used by migrations is exact. |
| `Storage.lean`, `Load.lean`, `Scoped.lean` | The rows the server writes and reads back. |
| `Database.lean` | The same requests on PostgreSQL, as the SQL the server sends. |
| `Scenarios.lean` | The main theorems for reachable states, and a concrete run of the kernel that publishes a document under the four-eyes rule. |

## Theorems

The following hold for every actor and command. `authorized`, `noninterference` and
`transition_frame` assume the snapshot satisfies the invariants, which
`reachable_inv` proves for every reachable state.

| Theorem | Statement |
|---|---|
| `allows_eq` | The kernel's permission table is the one in the specification. |
| `transition_total` | The kernel never fails, so it never panics, overflows or indexes out of bounds. |
| `apply_eq` | `apply` computes the meaning of a write set that the database must implement, as long as no table overflows `usize`. |
| `authorized` | Every write of a successful command is allowed by the policy, judged against the state before the command, including the four-eyes rule, the status workflow and the counter step. An effect goes only to the destination its project registered, and only when a writer publishes an approved document (with `emit_publishes`: the same write set publishes it). |
| `reply_confined` | Replies contain only documents the caller may read. |
| `inv_preserved`, `reachable_inv` | Every reachable state has an owner for each project, valid references, unique keys and fresh ids, and it satisfies the four-eyes rule; a document names an approver exactly when it is approved or published. |
| `noninterference` | Two valid states that look the same to a user give that user the same result, including error codes. The id counter is a declared exception. |
| `transition_frame` | A command's result depends only on the counter and one project's rows, which is why the server loads only those. |
| `authorized_reachable`, `noninterference_reachable`, `transition_frame_reachable` | The three theorems above for reachable states, with no assumption on the snapshot. |
| `published_reachable` | A concrete run reaches a state with a document published under the four-eyes rule; the runs in `Scenarios.lean` also show that publishing emits an effect and that the view and the scopes hide rows. |
| `check_inv_spec` | `check_inv` returns true exactly when the invariants hold, so migrations can refuse changes that break one. |
| `sql_writes_stored`, `sql_writes_storedC` | When the tenant's rows hold a valid state, running the statements the server plans for a write set leaves exactly the rows of the new state, under the PostgreSQL meaning of upsert and delete. The second covers a fresh tenant that has no counter row yet. |
| `fresh`, `load_sound`, `store_sound` | An empty tenant holds a valid state, loading rows in any order decodes to a valid state that the rows hold, and storing the writes of any successful command keeps it that way. As a result, the invariants hold for the database itself. |
| `scoped_sound`, `scoped_command` | The server's scoped loads decode to exactly the slice that `transition_frame` talks about, so storing the resulting writes keeps the database valid as well. This assumes that each table has at most `usize::MAX` rows. |
| `served_inv` | Every database the server produces from an empty tenant, loading all rows or a scope's rows for each request, holds a valid state. `served_create` and `served_create_scoped` show the hypotheses hold together. |
| `Database.db_inv` | The same on PostgreSQL: loading with the compiled `SELECT`s (full or scoped), running a successful command and storing the compiled statements of its writes, among any other tenants' statements, keeps the tenant's rows valid, and every full load decodes to a valid snapshot. The SQL comes from the extracted `h5i_app_pgsql`; PostgreSQL's meaning of it is the model in `H5iAppLib.Pg`. |

Every theorem depends only on Lean's standard axioms (`propext`,
`Classical.choice` and `Quot.sound`), which `scripts/app/ci-lean-gate.sh` checks.

## Building the proofs

```
lake exe cache get
lake build
../../../../scripts/app/ci-lean-gate.sh      # rejects sorry and non-standard axioms
../../../../scripts/app/mutants.py           # injected bugs must break a proof
```

`scope.toml` beside these files lists what the theorems assume and leave out;
`h5i ui` reads it, together with the receipt `cargo app-verify` writes.

The generic lemmas come from `H5iAppLib` in `crates/h5i-app-core/proofs`. `loop_search`
and `loop_fold` turn a fact about one step of an extracted loop into a fact
about the whole loop, and `walk` and `h5i_step` step through extracted code.
