import Lake
open Lake DSL

-- Same Aeneas pin as the example proofs.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

package h5i_app_lib

-- Reusable lemmas and tactics for Aeneas-extracted h5i-app kernels.
@[default_target] lean_lib H5iAppLib where
  roots := #[`H5iAppLib]
