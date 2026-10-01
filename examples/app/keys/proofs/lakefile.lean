import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/KeysKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package keys_proofs

-- Generated Lean: the extracted Rust (scripts/app/extract-keys.sh). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`KeysKernel]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Spec, `Lemmas, `Theorems, `Counterexample, `Apply]
