# Implementation Plan: Bulletin board

**Branch**: `001-board` | **Date**: 2026-11-15 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/001-board/spec.md`

## Summary

A per-organization board with posts and moderators. The rules of the spec are
decided by a pure Rust kernel, extracted to Lean with Charon and Aeneas and
proved there. An Axum shell authenticates with HMAC bearer tokens and stores
the board in PostgreSQL through `h5i-app-pg`.

## Technical Context

**Language/Version**: Rust (workspace toolchain), Lean 4 v4.31.0

**Primary Dependencies**: h5i-app (`schema!`, `h5i-app-http`, `h5i-app-pg`),
Axum, Charon and Aeneas at the revisions `h5i app doctor` checks

**Storage**: PostgreSQL, one SERIALIZABLE transaction per command

**Testing**: `cargo test`, `h5i app prove`, `h5i app mutate`

**Target Platform**: Linux server

**Project Type**: web-service

**Performance Goals**: N/A (tutorial)

**Constraints**: kernel code stays in the subset Charon and Aeneas translate

**Scale/Scope**: one board per organization

## Constitution Check

| Principle | Status |
| --- | --- |
| I. Tutorial first | Pass: one container and `cargo run` |
| II. Small kernel | Pass: loops and matches only |
| K1. Decisions in the kernel | Pass: `transition` decides; the shell builds `Principal` from the token |
| K2. A theorem per rule | Pass: see Proof Map |
| K3. Proof gate | Pass: `h5i app check` gates every handwritten module |
| K4. A mutant per proof | Pass: one mutant per ID |
| K5. Trust boundary | Pass: boundary and assumption nodes in `h5i-app.ui.json` |
| K6. Meaning of "verified" | Applies to the completion report |

## Project Structure

### Documentation (this feature)

```text
specs/001-board/
├── spec.md
├── plan.md
└── tasks.md
```

### Source Code

```text
kernel/src/lib.rs        # schema!, Command, transition, apply_write, sql_write
server/src/{lib,main}.rs # Axum shell: HmacAuth, rpc_router, BoardStore
proofs/
├── generated/           # h5i app extract output, never edited
├── Spec.lean            # St, allowed, applyWrite, Inv, Reachable
├── Commands.lean        # one lemma per command
├── Theorems.lean        # writes_of, authorized, invariants, author_kept, moderator_kept
├── Apply.lean           # apply computes applyAll
├── Storage.lean         # PostgreSQL model, db_inv
└── Scenario.lean        # concrete accepted and refused runs
h5i-app.toml
h5i-app.ui.json
```

**Structure Decision**: kernel, server and proofs as separate directories, as
in every tutorial.

## Verified Kernel Design (h5i)

### Kernel Boundary

**Kernel crate**: `kernel/` (`board_kernel`)

**Entry point**: `transition(actor, snapshot, command)`

**Tables** (`schema!`):

| Table | Key | Columns | Tenant-scoped |
| --- | --- | --- | --- |
| posts | id | author, text | yes |
| moderators | user | | yes |
| counters | (none) | next_id | yes |

**Commands**:

| Command | Accepted when | Writes | Refusal |
| --- | --- | --- | --- |
| Publish { text } | text has 1–280 bytes, counter below `u64::MAX` | PutPost, SetCounter | BadText, Overflow |
| Edit { id, text } | post exists, caller is its author, text valid | PutPost (same id and author) | NotFound, Forbidden, BadText |
| Delete { id } | post exists, caller is its author or a moderator | DelPost | NotFound, Forbidden |
| List | always | nothing | |
| Promote { user } | caller is a moderator, or there is none and `user` is the caller | PutModerator | Forbidden |
| Demote { user } | caller and `user` are moderators, at least two moderators | DelModerator | Forbidden, NotFound, LastModerator |

**Shell responsibilities**: HMAC token check and `Principal` construction,
JSON decoding, status codes, idempotent retries, PostgreSQL.

### Proof Map

`allowed` in `proofs/Spec.lean` is the policy for AUTH-01 to AUTH-04: each
write of an accepted command must be allowed for the caller in the pre-state.
`Inv` holds the fields for INV-03 to INV-05.

| ID | Theorem | Statement (hypotheses ⇒ conclusion) | Mutant | UI node |
| --- | --- | --- | --- | --- |
| INV-01 | `board_kernel.Theorems.moderator_kept` | Inv s ∧ s has a moderator ∧ accepted ⇒ s' has a moderator | `inv_01_remove_last_moderator` | `inv-01` |
| INV-02 | `board_kernel.Theorems.author_kept` | Inv s ∧ accepted ⇒ a post with the same id in s and s' has the same author | `inv_02_publish_over_post_0` | `inv-02` |
| INV-03 | `board_kernel.Theorems.reachable_inv` | Reachable s ⇒ post ids distinct (`post_keys`) and below `next` (`fresh`) | `inv_03_counter_not_advanced` | `inv-03` |
| INV-04 | `board_kernel.Theorems.reachable_inv` | Reachable s ⇒ every post satisfies `textOk` (`texts`) | `inv_04_empty_text` | `inv-04` |
| INV-05 | `board_kernel.Theorems.reachable_inv` | Reachable s ⇒ moderator users distinct (`mod_keys`) | `inv_05_moderator_appended` | `inv-05` |
| AUTH-01 | `board_kernel.Theorems.authorized_reachable` | Reachable s ∧ accepted ⇒ every PutPost is a new post by the caller or the caller's own post | `auth_01_edit_any_post` | `auth-01` |
| AUTH-02 | `board_kernel.Theorems.authorized_reachable` | Reachable s ∧ accepted ⇒ every DelPost is by the author or a moderator | `auth_02_delete_any_post` | `auth-02` |
| AUTH-03 | `board_kernel.Theorems.authorized_reachable` | Reachable s ∧ accepted ⇒ every PutModerator is by a moderator, or a self-appointment on a board with none | `auth_03_self_appoint_any_time` | `auth-03` |
| AUTH-04 | `board_kernel.Theorems.authorized_reachable` | Reachable s ∧ accepted ⇒ every DelModerator is by a moderator and leaves another | `auth_04_demote_by_anyone` | `auth-04` |

"Accepted" is `transition a s c = ok (Ok (ws, r))`; s' is `applyAll s ws`.
Reachable states are those built from the empty board by accepted commands,
so rows written outside the kernel are not covered.

### Mutants

```toml
[[mutant]]
name = "inv_01_remove_last_moderator"
old = "s.moderators.len() <= 1"
new = "s.moderators.len() < 1"

[[mutant]]
name = "inv_02_publish_over_post_0"
old = "let id = s.counter.next_id;"
new = "let id = 0;"

[[mutant]]
name = "inv_03_counter_not_advanced"
old = "Counter { next_id: id + 1 }"
new = "Counter { next_id: id }"

[[mutant]]
name = "inv_04_empty_text"
old = "t.len() > 0 && t.len() <= MAX_TEXT"
new = "t.len() <= MAX_TEXT"

[[mutant]]
name = "inv_05_moderator_appended"
old = "Write::PutModerator(m) => Moderator::put(&mut s.moderators, m),"
new = "Write::PutModerator(m) => s.moderators.push(m),"

[[mutant]]
name = "auth_01_edit_any_post"
old = "if p.author != user {"
new = "if false {"

[[mutant]]
name = "auth_02_delete_any_post"
old = "if p.author == user || is_moderator(&s.moderators, user) {"
new = "if true {"

[[mutant]]
name = "auth_03_self_appoint_any_time"
old = "let bootstrap = s.moderators.len() == 0 && target == user;"
new = "let bootstrap = target == user;"

[[mutant]]
name = "auth_04_demote_by_anyone"
old = "if !is_moderator(&s.moderators, user) {"
new = "if false {"
```

INV-05's mutant edits `apply_write`, not `transition`: the spec's
`applyWrite` keeps moderator keys unique by construction, so the defect is a
Rust `apply` that disagrees with it. `apply_spec` is the theorem that catches
it.

### Trust Boundary and Shell Tests

| Spec item | h5i-app.ui.json node | HTTP test |
| --- | --- | --- |
| The bearer token identifies the caller | boundary `b-auth` | none |
| The organization in the token is the tenant | boundary `b-tenant` | none |
| JSON decoding, status codes, retries | boundaries `b-json`, `b-retry` | none |
| PostgreSQL runs the SQL as modeled; schema matches | boundary `b-engine`, assumption `a-schema` | none |
| Charon, Aeneas and Lean are faithful | boundary `b-extraction` | none |

No rule of the spec is enforced only in the shell, so there are no `h5i test`
flows. A deployment would add one per boundary row against staging.

### Verification Gate

The feature is done when, for the current kernel:

1. `h5i app lint` reports no unexplained warnings.
2. `h5i app prove` passes with every Proof Map theorem in `[check].theorems`.
3. `h5i app mutate` reports every Proof Map mutant as caught.
4. `h5i test` passes every flow in the table above against staging (none).
5. The h5i trace command (`/speckit.h5i.trace`) reports no gaps.
