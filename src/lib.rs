// The MCP `tool_definitions()` builds a large `json!` literal; the default
// macro recursion limit (128) is not enough to expand it.
#![recursion_limit = "512"]

pub mod ast;
pub mod codex;
pub mod injection;
pub mod compliance;
pub mod policy;
pub mod blame;
pub mod claims;
pub mod mcp;
pub mod ctx;
/// Deprecated alias — use `ctx` instead.
pub use ctx as gcc;
pub mod cgroup;
pub mod claude;
pub mod container;
pub mod env;
pub mod error;
pub mod filter_rules;
pub mod hooks;
pub mod lfs;
pub mod memory;
pub mod metadata;
pub mod msg;
pub mod objects;
pub mod pr;
pub mod prompt_score;
pub mod radio;
pub mod recap;
pub mod sandbox;
pub mod seccomp_notify;
pub mod secrets;
pub mod secrets_broker;
pub mod supervisor;
pub mod session_log;
pub mod storage;
pub mod structured;
pub mod token_filter;
pub mod repository;
pub mod resume;
pub mod review;
pub mod risk;
pub mod rules;
pub mod server;
pub mod ui;
pub mod vibe;

pub use repository::H5iRepository;
