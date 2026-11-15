# Tasks: Bulletin board

**Input**: `specs/001-board/plan.md`, `specs/001-board/spec.md`

The board existed before this spec, so the implementation phases are marked
done. Phase V lists the work this spec added: the requirement IDs, eight new
mutants and the ID-named nodes in `h5i-app.ui.json`.

## Phase 1: Setup

- [X] T001 Create `kernel/`, `server/` and `proofs/` with `h5i app new`
- [X] T002 Add the board to the `examples/app` workspace

## Phase 2: Kernel

- [X] T003 Declare posts, moderators and counters with `schema!` in `kernel/src/lib.rs`
- [X] T004 [US1] Implement Publish and List
- [X] T005 [US2] Implement Edit and Delete
- [X] T006 [US3] Implement Promote and Demote
- [X] T007 Implement `apply_write` and `sql_write`

## Phase 3: Shell

- [X] T008 Serve `POST /rpc` with `HmacAuth` and `rpc_router` in `server/src/main.rs`
- [X] T009 Implement `BoardStore` on PostgreSQL in `server/src/lib.rs`

## Phase V: Verification (h5i)

- [X] TV01 Run `h5i app doctor` and `h5i app extract`; commit `proofs/generated/`
- [X] TV02 Write `allowed`, `applyWrite`, `Inv` and `Reachable` in `proofs/Spec.lean`
- [X] TV03 Prove INV-03, INV-04, INV-05 as `reachable_inv` in `proofs/Theorems.lean`
- [X] TV04 Prove INV-01 as `moderator_kept` and INV-02 as `author_kept`
- [X] TV05 Prove AUTH-01 to AUTH-04 as `authorized_reachable`
- [X] TV06 Add the Proof Map theorems to `[check].theorems` in `h5i-app.toml`
- [X] TV07 Add the nine Proof Map mutants to `h5i-app.toml`
- [X] TV08 Rename the guarantee nodes in `h5i-app.ui.json` to `inv-01` … `auth-04`
- [ ] TV09 Run `h5i app prove` and `h5i app mutate`; every mutant caught
- [ ] TV10 Run the h5i trace command (`/speckit.h5i.trace`); no gaps

**Checkpoint**: Every requirement ID has a passing theorem and a caught mutant.
