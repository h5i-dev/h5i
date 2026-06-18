//! Neutral, dependency-free identifiers shared across subsystems.
//!
//! These live here — not in a feature module like [`crate::msg`] — so that
//! low-level layers (e.g. [`crate::sandbox`]) can reference them without taking
//! a dependency on a higher-level subsystem. Keep this module free of imports.

/// Environment variable carrying the active agent identity (e.g. `claude`,
/// `codex`). Consulted by the messaging layer to resolve "who am I", and by the
/// sandbox to scope the agent-in-box profile to whoever created the env.
pub const AGENT_ENV: &str = "H5I_AGENT";
