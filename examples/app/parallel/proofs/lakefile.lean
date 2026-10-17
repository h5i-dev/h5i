import Lake
open Lake DSL
require aeneas from git "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"
require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"
package parallel_proofs
lean_lib Generated where
  srcDir := "generated"
  roots := #[`ParallelKernel, `Schema]
@[default_target] lean_lib Proofs where
  roots := #[`Proofs]
