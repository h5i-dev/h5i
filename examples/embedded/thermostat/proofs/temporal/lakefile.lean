import Lake
open Lake DSL

-- Optional Veil temporal verification package for `examples/embedded/thermostat`.
-- Kept independent from `../lakefile.lean` (Aeneas / `v4.31.0`) so normal
-- `h5i app check` / `prove` builds never require Veil, while directly compiling
-- the actual Aeneas-extracted `ThermostatKernel` and existing `Proofs.lean`
-- on the shared Lean `v4.35.0` toolchain (`aeneas @ 1238879f9ea5` + `veil @ v4.35.0`).
require h5i_app_veil from "../../../../../crates/h5i-app-veil/proofs"

package thermostat_temporal

lean_lib ThermostatKernel where
  srcDir := "../generated"
  roots := #[`ThermostatKernel]

lean_lib Proofs where
  srcDir := ".."
  roots := #[`Proofs]

@[default_target] lean_lib ThermostatTemporal where
  roots := #[`ThermostatTemporal]
