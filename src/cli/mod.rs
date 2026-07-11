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
pub mod policy;
