
## Phase V: Verification (h5i)

<!--
  ACTION REQUIRED: Generate one proof task and one mutant task per row of
  the plan's Proof Map, and one test task per shell-test row. Replace the
  samples below. These tasks are not optional for h5i-app features: the
  feature is not done until they are.
-->

**Purpose**: Prove each INV-*/AUTH-* rule about the extracted kernel and show
the proofs catch the matching defect.

- [ ] TV01 Run `h5i app doctor` and `h5i app extract`; commit `proofs/generated/`
- [ ] TV02 [P] Write the Lean spec for INV-01 in `proofs/Spec.lean`
- [ ] TV03 Prove INV-01 as `[theorem]` in `proofs/Theorems.lean`; add it to `[check].theorems`
- [ ] TV04 [P] Add `[[mutant]] inv_01_...` to `h5i-app.toml`
- [ ] TV05 Prove AUTH-01 as `[theorem]`; add it to `[check].theorems`
- [ ] TV06 [P] Add `[[mutant]] auth_01_...` to `h5i-app.toml`
- [ ] TV07 [P] Write `.h5i-tests/tests/[name].yaml` for each shell-enforced rule
- [ ] TV08 Write `h5i-app.ui.json`: one guarantee node per ID, linked by `proved by` to its theorem node; boundary and assumption nodes for the Trust Boundary; exclusions for anything not proved
- [ ] TV09 Run `h5i app prove` and `h5i app mutate`; fix code, spec or proof until every mutant is caught
- [ ] TV10 Run the h5i trace command (`/speckit.h5i.trace`) and resolve every gap

**Checkpoint**: Every requirement ID has a passing theorem and a caught mutant,
or a stated exclusion.
