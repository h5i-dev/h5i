//! h5i-sandbox.

// Re-export the shared error crate as `crate::error` so every internal
// `crate::error::*` path in the moved modules resolves unchanged.
pub use h5i_error as error;

pub mod sandbox_policy;

pub mod auth_proxy;
pub mod cgroup;
pub mod container;
/// The effective configuration a kernel-tier invocation applies
/// (design-policy.md §P1). Linux only, like the mechanisms it describes.
#[cfg(target_os = "linux")]
pub mod effective;
/// The filesystem-authority validator (design-policy.md §P2): the per-run
/// translation validation of the effective config against declared intent.
pub mod fs_authority;
/// The mount-realization audit (design-policy.md §P3): diff realized
/// `/proc/<pid>/mountinfo` against the planned binds before exec.
pub mod mount_audit;
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
