import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/H5iAppStd.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../h5i-app-core/proofs"

package std_proofs

-- Generated Lean: the extracted Rust (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`H5iAppStd]

-- The specs. A kernel that includes h5i-app-std gets a copy of `StdSpecs`
-- importing its own extraction (`h5i app extract`, `std-specs = true`).
@[default_target] lean_lib Proofs where
  roots := #[`StdSpecs, `Check]
