//! Per-noun CLI handler modules.
//!
//! `main.rs` owns the top-level `Cli`/`Commands` parse and dispatch; each noun's
//! clap subcommand enum and its handlers live in a module here, so `main.rs`
//! stays a thin router. A handler is
//! `pub fn run(action: <Noun>Commands) -> anyhow::Result<()>`.

pub mod boxes;
pub mod browser;
pub mod completion;
pub mod man;
// `h5i box share` / `h5i join`. Gated with `share-tunnel`, the narrower of
// the two switches, because the tunnel transport is what this module always
// has: a build with `share-tunnel` and no `share` gets `box share --tunnel`
// and no `join`. Without either, there is no `share` verb rather than a
// broken one.
#[cfg(feature = "share-tunnel")]
pub mod share;
pub mod skill;
// The box console. Gated with the `web` feature it drives, so a
// `--no-default-features` binary has no `ui` verb rather than a broken one.
#[cfg(feature = "web")]
pub mod ui;
