// ─── public surface ─────────────────────────────────────────────────────────
// The domain: an environment is a confined worktree with a pinned policy, and
// a receipt is the record of what actually ran inside it. (`error` stays public
// because `H5iError` appears in the signatures of most of them.)
pub mod cache;
pub mod env;
pub mod export;
pub mod receipt;
pub mod redact;
pub mod refstore;
pub mod skill;
pub mod source;
pub mod storage;
pub mod ui;
// The error type lives in its own leaf crate (`h5i-error`) so extracted crates
// can depend on it without depending on `h5i-core`. Re-exported as
// `crate::error` so every existing `crate::error::*` path resolves unchanged.
pub use h5i_error as error;

// The confinement layer lives in its own crate (`h5i-sandbox`). Re-exported so
// every `crate::sandbox::*` / `crate::container::*` / … path resolves inside
// this crate unchanged.
pub use h5i_sandbox::{
    auth_proxy, cgroup, container, sandbox, sandbox_policy, secrets, secrets_broker, supervisor,
};
// `seccomp_notify` is Linux+x86_64/aarch64 only (its whole module is cfg'd out
// elsewhere), so the re-export must carry the same gate or it fails to resolve
// on macOS/other targets in the cross-check job.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub use h5i_sandbox::seccomp_notify;
