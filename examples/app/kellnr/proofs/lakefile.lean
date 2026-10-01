import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/KellnrKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package kellnr_proofs

-- Generated Lean: the extracted Rust (scripts/app/extract-kellnr.sh). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`KellnrKernel]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Spec, `Lemmas, `Theorems, `Apply, `Invariants, `Counterexample]
