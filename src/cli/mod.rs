//! Per-noun CLI handler modules.
//!
//! `main.rs` owns the top-level `Cli`/`Commands` parse and dispatch; each noun's
//! clap subcommand enum and its handlers live in a module here, so `main.rs`
//! stays a thin router instead of one 10k-line `fn main`. A handler is
//! `pub fn run(action: <Noun>Commands) -> anyhow::Result<()>` (plus any shared
//! setup it needs threaded in). Migrated incrementally, one noun at a time.

pub mod context;
pub mod memory;
pub mod notes;
pub mod pr;
pub mod objects;
pub mod env;
pub mod msg;
pub mod team;
pub mod orchestra;
pub mod commit;
pub mod log;
pub mod blame;
#[cfg(feature = "web")]
pub mod serve;
pub mod status;
pub mod resolve;
pub mod doctor;
pub mod vibe;
pub mod maturity;
pub mod compliance;
pub mod resume;
pub mod push;
pub mod pull;
pub mod init;
pub mod completion;
pub mod recall_rm;
pub mod setup_remote;
pub mod migrate_remote;
pub mod mcp;
pub mod man;
pub mod codex;
pub mod claude;
pub mod hook;
pub mod policy;
