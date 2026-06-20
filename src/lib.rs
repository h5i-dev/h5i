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
pub mod error;
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
pub mod sandbox;
pub mod session_log;
pub mod storage;
pub mod structured;
pub mod token_filter;
pub mod ui;
pub mod vibe;
#[cfg(feature = "web")]
pub mod server;

// ─── crate-internal machinery ───────────────────────────────────────────────
// Implementation detail not used outside this crate. `pub(crate)` so it cannot
// be depended on externally and stays free to refactor; types that need a
// public path (e.g. the sandbox policy vocabulary) are re-exported `pub` from a
// public module like `sandbox`.
pub(crate) mod cgroup;
pub(crate) mod container;
pub(crate) mod idents;
pub(crate) mod sandbox_policy;
pub(crate) mod seccomp_notify;
pub(crate) mod secrets;
pub(crate) mod secrets_broker;
pub(crate) mod supervisor;
/// Risk classification for the web dashboard only — its sole consumer is the
/// feature-gated `server`, so it is gated too (and absent from a lean CLI build).
#[cfg(feature = "web")]
pub(crate) mod risk;

pub use repository::H5iRepository;
