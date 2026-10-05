# Numbers

Measured 2026-09-27. Lines exclude blanks and comments. Hand-written Lean
includes the spec and excludes generated files (extracted kernels,
`Schema.lean`). Rust kernel sizes exclude tests.

## Proof effort per app

| App | Kernel (Rust) | Spec (Lean) | Hand-written Lean | Lean per kernel line |
|---|---|---|---|---|
| `examples/app/docs` | 936 | 108 | 2,538 | 2.7 |
| `examples/app/kellnr` | 374 | 71 | 820 | 2.2 |
| `examples/app/atuin` | 459 | 51 | 527 | 1.1 |
| `examples/app/wastebin` | 275 | 51 | 720 | 2.6 |
| `examples/app/conduit` | 1,063 | 127 | 1,682 | 1.6 |
| `examples/app/cratesio` | 1,030 | 142 | 1,497 | 1.5 |
| tutorial 1, calculator | 114 | 11 | 218 | 1.9 |
| tutorial 2, board | 221 | 45 | 441 | 2.0 |
| tutorial 3, ledger | 180 | 29 | 461 | 2.6 |
| tutorial 4, inbox | 239 | 41 | 580 | 2.4 |
| tutorial 5, booking | 268 | 61 | 644 | 2.4 |
| `examples/app/filters` | 251 | 55 | 425 | 1.7 |
| `examples/app/keys` | 204 | 41 | 298 | 1.5 |

Counts include scenarios and upstream-bug counterexamples. Kellnr grew
after 2026-09-26 when its `apply` was extracted and its owner invariant
proven. Upstream Conduit has 1,077 handler lines with inline SQL, about 1.6
Lean lines per handler line.

`examples/app/filters` and `examples/app/keys` (2026-09-30) use `for` loops and
the `H5iAppLib.Iter` specs. `h5i_for` and `h5i_derive_clone` then cut their
function-spec files from 227 to 123 lines (filters `Lemmas` 120 to 71, keys
`Lemmas` 65 to 36, keys `Apply` 42 to 16); most loop specs are one line. The
library grew by 332 lines for them (`Iter`, `Bytes`, `Runs`, tactics).

Moving 37 repeated lemmas into `H5iAppLib` cut app Lean by 486 lines and grew
the library by 258.

Docs also proves noninterference, partial snapshots, migration checks, row
codecs and scoped loads. Its spec, authorization, replies and invariants
alone take 865 Lean lines, about 1.3 per line of the matching kernel code.

Docs features added after the shared library:

| Feature | Kernel | Spec | Proofs |
|---|---|---|---|
| Webhooks and effects | 82 | 17 | 101 |
| Partial snapshots (`read_scope`, `Frame.lean`) | 32 | 0 | 160 |
| Invariant checker (`check_inv`, `Check.lean`) | 167 | 0 | 292 |
| Row encoding (`sql_writes`, `Storage.lean`) | 58 | 40 | 239 |
| Row decoding (`decode`, `Load.lean`) | 61 | 26 | 411 |
| Scoped loads (`scoped_project`, `Scoped.lean`) | 17 | 20 | 211 |

Docs' generated `Schema.lean` (474 lines) includes table operations and
proofs; the app writes 31 lines of `Columns.lean` for two enums.

Shared code:

| Piece | Rust | Lean |
|---|---|---|
| `H5iAppLib` (loops, tables, lists, scalars, SQL and store semantics, tactics) | none | 1,310 |
| `h5i-app-sql` (planner) | 141 | 118 |
| `h5i-app-token` (token parser and encoder) | 197 | 963 |
| `h5i-app-json` (reply writer) | 207 | 427 |
| `h5i-app-std` (bytes, sets, maps, reachability, expiry), with its list models in `H5iAppLib` | 335 | 1,102 |
| `Schema.lean`, generated for docs | none | 474 |

## Checks

| Check | Result |
|---|---|
| `h5i app mutate examples/app/docs` | 22 of 22 kernel bugs break a proof |
| `h5i app mutate` on the other twelve kernels | 12 of 12 compiling kernel bugs break a proof |
| `scripts/app/difftest.sh` (Rust vs Lean) | 5,000 random cases per run agree (55,000 in one longer run); every outcome kind hit |
| Axioms (`h5i app check`) | only `propext`, `Classical.choice`, `Quot.sound`, for all 3,974 theorems in hand-written modules |
| Extraction drift (CI) | every kernel and extracted crate re-extracted and compared |

## Time

On a 128-core machine:

| Step | Time |
|---|---|
| Clean `lake build` of docs proofs (Mathlib cached) | 87 s |
| `cargo app-verify`, incremental | about 5 min |
| Docs mutation suite, 3 jobs | 13 min |

## Bugs found

| Where | What | How |
|---|---|---|
| h5i-app engine | idempotency keys per tenant only; one user could get another's reply | PostgreSQL test; keys now per app-defined scope |
| h5i-app engine | stale snapshot under the tenant lock | fault and concurrency tests; lock now taken before `BEGIN` |
| Kellnr (before PR #1243) | read-only session could add crate owners | read-only theorem fails; Lean gives the counterexample |
| Atuin (issue #3297, open) | a session alone can delete the account | proven for current rules; fix proven to need the password |
| h5i-app engine | apps sharing a database shared tables, keys and outbox; one dispatcher killed another's effects | example tests; one PostgreSQL schema per app |
| Wastebin (before 632ddf2, issue #190) | link preview GET burned a burn-after-reading paste | `preview_broken` counterexample; `preview_fixed` for current code |
| Wastebin (current) | `/raw`, `/dl`, `/md` still burn a paste on GET | `raw_link_burns` |
| Conduit (issue #16, open) | `favorited` means "favorited any article" | `upstream_violates_reply_spec` |
| Conduit (unreported) | `?favorited=` lists every article once the user favorited one | `favorited_filter` |
| crates.io (before PR #14760) | a locked account could sign in | `pre14760_violates_lock` |
| crates.io (current) | the emailed invitation link skips the lock check; a locked user can become owner | porting; `examples/app/cratesio/README.md` |
