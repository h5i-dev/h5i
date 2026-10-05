import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/WastebinKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package wastebin_proofs

-- Generated Lean: the extracted Rust (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`WastebinKernel, `Schema]

-- Hand-written specification and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Spec, `Commands, `Theorems, `Apply, `Storage, `Preview, `Scenarios]
