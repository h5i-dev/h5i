import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/DocsKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package docs_proofs

-- Generated Lean: the extracted Rust (scripts/app/extract.sh) and schema! output. Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`DocsKernel, `Schema]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Columns, `Spec, `Lemmas, `Apply, `Transition, `Theorems, `Invariants, `Noninterference, `Frame, `Check, `Storage, `Load, `Scoped, `Scenarios, `Database]

-- Runs the extracted kernel for differential tests (scripts/app/difftest.sh).
lean_exe difftest where
  root := `DiffTest
