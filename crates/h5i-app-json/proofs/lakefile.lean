import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/H5iAppJson.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../h5i-app-core/proofs"

package json_proofs

-- Generated Lean: the extracted Rust (scripts/app/extract-json.sh). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`H5iAppJson]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`JsonSpec, `JsonProofs]
