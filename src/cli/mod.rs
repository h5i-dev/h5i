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
// `h5i runner`. Gated with the `runner` feature it drives, so a build without
// it has no `runner` verb rather than a broken one — and, since the worker end
// of the protocol is this same binary, no ability to *be* a runner either.
// The bridge between h5i-core's placement trait and the runner protocol. Same
// gate as `runner`, because it is the half of that feature the box lifecycle
// reaches for.
#[cfg(feature = "runner")]
pub mod placement;
#[cfg(feature = "runner")]
pub mod runner;
pub mod skill;
// The box console. Gated with the `web` feature it drives, so a
// `--no-default-features` binary has no `ui` verb rather than a broken one.
#[cfg(feature = "web")]
pub mod ui;

/// `Repository::discover(".")` with an actionable failure. Every verb here
/// stores its state relative to the enclosing repository, so outside one there
/// is nothing to act on — and libgit2's raw `class=…; code=…` error names
/// neither that precondition nor what to do about it.
pub fn discover_repo(verb: &str) -> anyhow::Result<git2::Repository> {
    git2::Repository::discover(".").map_err(|e| {
        if e.code() == git2::ErrorCode::NotFound {
            anyhow::anyhow!(
                "`{verb}` needs to run inside a git repository — cd into your project, \
                 or create one with `git init`"
            )
        } else {
            e.into()
        }
    })
}
