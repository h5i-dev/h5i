import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/RolesKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package roles_proofs

-- Generated Lean (`h5i app extract`). Do not edit: the extracted
-- kernel, and h5i-app-std's specs copied against it.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`RolesKernel, `StdSpecs]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Spec, `Theorems]
