---
description: "Check that every INV/AUTH requirement has a passing theorem, a caught mutant and an h5i-app.ui.json node"
---

## User Input

```text
$ARGUMENTS
```

The input may name a feature directory under `specs/`. Otherwise use the
current feature (the branch name, or the most recently changed directory
under `specs/`).

## Goal

Report, for each requirement ID in the feature's `spec.md`, whether the h5i
evidence for it exists and is current. This command reads files and the run
records `h5i app` writes. It does not edit anything, and it does not judge
whether a theorem states the rule well; it only checks the links.

## Steps

1. **Collect IDs.** From `spec.md`, take every `INV-NN` and `AUTH-NN` that is
   defined in a list item (`- **INV-01**: ...`). From `plan.md`, read the
   Proof Map rows: ID, theorem, mutant, UI node, or "excluded" with a reason.

2. **Find the h5i-app project.** Locate `h5i-app.toml` (the plan's Kernel
   Boundary names the crate; otherwise search from the repository root). Read
   its `[check].theorems` and `[[mutant]]` names. Read `h5i-app.ui.json`
   next to it, if present.

3. **Find the latest run records.** Under `.h5i/app/runs/` next to the
   manifest, take the newest directory whose name contains `-check-` and the
   newest containing `-mutation-`. Each has a `run.json`. If either is
   missing, ask the user to run `h5i app prove` and `h5i app mutate` (or run
   them if the user agrees) and report the gap.

4. **Check each ID.** An ID passes only if all of these hold:
   - It has a Proof Map row.
   - Its theorem appears in `[check].theorems`.
   - The check record has `"status": "passed"`, `"inputs_changed": false`,
     and a `declarations` entry with that `name` and `"kind": "theorem"`
     whose `axioms` are a subset of `propext`, `Classical.choice`,
     `Quot.sound`.
   - Its mutant exists in `h5i-app.toml`, and the mutation record has a
     `trials` entry with that `name` and `"status": "proof_failed"` (caught).
     `survived` or `survived_by_design` is a gap; `invalid` or `error` means
     the mutant did not run.
   - `h5i-app.ui.json` has a node whose `id` is the ID in lower case and
     `kind` is `guarantee`, with a `proved by` edge to a `theorem` node whose
     `symbol` is the theorem.

   An ID marked excluded passes only if `h5i-app.ui.json` lists it in
   `exclusions` with the same reason.

5. **Check the other direction.** List theorems in `[check].theorems` and
   mutants in `h5i-app.toml` whose names do not appear in the Proof Map, and
   Proof Map IDs that are not in `spec.md`. These are not failures, but
   report them.

6. **Check the trust boundary.** For each row of the plan's Trust Boundary
   table, confirm the named `h5i-app.ui.json` node exists and the named
   `.h5i-tests/tests/` file exists. Do not claim the test passed unless an
   `h5i-test-results/` record from this branch shows it.

## Report

Print one table:

| ID | Theorem | Proof | Mutant | UI node | Result |
| --- | --- | --- | --- | --- | --- |

Use `passed`, `missing`, `stale` (inputs changed since the run), `survived`
or `excluded` in the cells. Then list the reverse-direction findings and
trust boundary gaps. End with one line: the number of IDs that pass out of
the total. If any ID does not pass, say the feature does not meet the plan's
Verification Gate.
