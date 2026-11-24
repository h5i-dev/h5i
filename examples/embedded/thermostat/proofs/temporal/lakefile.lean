import Lake
open Lake DSL

-- Optional Veil temporal verification package for `examples/embedded/thermostat`.
-- Kept independent from `../lakefile.lean` (Aeneas / `v4.31.0`) so normal
-- `h5i app check` / `prove` builds never require Veil.
require h5i_app_veil from "../../../../../crates/h5i-app-veil/proofs"

package thermostat_temporal

@[default_target] lean_lib ThermostatTemporal where
  roots := #[`ThermostatTemporal]
