//! h5i-sandbox — multi-tier process/container confinement for h5i.
//!
//! The policy model (`sandbox_policy`) plus the confinement machinery and
//! runtime backends: kernel-tier Landlock/seccomp/namespaces (`sandbox`,
//! `supervisor`, `seccomp_notify`, `cgroup`), the rootless-Podman container
//! backend (`container`), the microsandbox microVM backend (`microvm`), the
//! egress allowlist proxy and secrets handling
//! (`auth_proxy`, `secrets`, `secrets_broker`). Extracted from `h5i-core` as an
//! internal workspace crate so it compiles independently of the domain layer
//! and could back other tools; it depends only on `h5i-error`.

// Re-export the shared error crate as `crate::error` so every internal
// `crate::error::*` path in the moved modules resolves unchanged.
pub use h5i_error as error;

pub mod sandbox_policy;

pub mod auth_proxy;
pub mod cgroup;
pub mod container;
pub mod microvm;
pub mod sandbox;
/// macOS Seatbelt backend. Compiled on every Unix target (so its profile
/// generator is typechecked and unit-tested by the Linux job), but
/// [`seatbelt::probe`] fails closed anywhere but macOS.
#[cfg(unix)]
pub mod seatbelt;
pub mod seccomp_notify;
pub mod secrets;
pub mod secrets_broker;
pub mod supervisor;
