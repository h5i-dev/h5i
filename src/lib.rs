// The MCP `tool_definitions()` builds a large `json!` literal; the default
// macro recursion limit (128) is not enough to expand it.
#![recursion_limit = "512"]

// ─── public surface ─────────────────────────────────────────────────────────
// Modules consumed by the `h5i` binary, the integration tests, or the MCP
// surface. These form the crate's de-facto public API. (`error` stays public
// because `H5iError` appears in the signatures of many of them.)
pub mod blame;
pub mod claude;
pub mod cli_routing;
pub mod codex;
pub mod compliance;
pub mod ctx;
/// Deprecated alias — use `ctx` instead.
pub use ctx as gcc;
pub mod env;
// The error type now lives in its own leaf crate (`h5i-error`) so extracted
// crates can depend on it without depending on `h5i-core`. Re-exported as
// `crate::error` so every existing `crate::error::*` path resolves unchanged.
pub use h5i_error as error;
pub mod filter_rules;
pub mod hooks;
pub mod injection;
pub mod lfs;
pub mod mcp;
pub mod memory;
pub mod metadata;
pub mod msg;
pub mod objects;
pub mod policy;
pub mod pr;
pub mod prompt_score;
pub mod radio;
pub mod recap;
pub mod repository;
pub mod resume;
pub mod review;
pub mod rules;
pub mod session_log;
pub mod storage;
pub mod structured;
pub mod team;
pub mod token_filter;
pub mod ui;
pub mod vibe;
#[cfg(feature = "web")]
pub mod server;

// The confinement layer now lives in its own crate (`h5i-sandbox`). Re-exported
// so every existing `crate::sandbox::*` / `crate::container::*` / … path
// resolves unchanged. `idents` stays here (a core identity constant table).
pub(crate) mod idents;
pub use h5i_sandbox::{
    auth_proxy, cgroup, container, sandbox, sandbox_policy, seccomp_notify, secrets,
    secrets_broker, supervisor,
};
/// Risk classification for the web dashboard only — its sole consumer is the
/// feature-gated `server`, so it is gated too (and absent from a lean CLI build).
#[cfg(feature = "web")]
pub(crate) mod risk;

pub use repository::H5iRepository;
