import Lake
open Lake DSL

-- Optional Veil backend for h5i-app temporal verification.
--
-- Kept as an independent Lake package on Lean `v4.35.0` (matching both
-- `verse-lab/veil @ v4.35.0` and `AeneasVerif/aeneas @ 1238879f9ea5`),
-- directly compiling `H5iAppLib` (`Basic`, `Runs`, `Time`) so `H5iAppVeil`
-- bridges the real `H5iAppLib.Run` / `ReachableT` and Aeneas-extracted kernels
-- to Veil while keeping Veil completely optional for normal h5i builds/CI.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "1238879f9ea550512096e9bf8bd6fe7407b3eaef" / "backends/lean"

require veil from git
  "https://github.com/verse-lab/veil" @ "v4.35.0"

package h5i_app_veil

lean_lib H5iAppLib where
  srcDir := "../../h5i-app-core/proofs"
  roots := #[`H5iAppLib.Basic, `H5iAppLib.Runs, `H5iAppLib.Time]

lean_lib H5iAppEmbedded where
  srcDir := "../../h5i-app-embedded/proofs/generated"
  roots := #[`H5iAppEmbedded]

lean_lib H5iAppEmbeddedProofs where
  srcDir := "../../h5i-app-embedded/proofs"
  roots := #[`NorFlash, `FlashStoreProofs]

@[default_target] lean_lib H5iAppVeil where
  roots := #[`H5iAppVeil]
