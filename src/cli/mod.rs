//! Per-noun CLI handler modules.
//!
//! `main.rs` owns the top-level `Cli`/`Commands` parse and dispatch; each noun's
//! clap subcommand enum and its handlers live in a module here, so `main.rs`
//! stays a thin router. A handler is
//! `pub fn run(action: <Noun>Commands) -> anyhow::Result<()>`.

pub mod boxes;
// `h5i browser`. Gated with the `browser` feature it drives, so a build without
// the rendering engine linked in has no `browser` verb rather than one that
// starts a subprocess that cannot render — the same rule `ui`, `share` and
// `runner` follow.
#[cfg(feature = "browser")]
pub mod browser;
// The helper lane behind `h5i browser transcript --via yt-dlp`: an outside
// program, run deliberately, recorded as one.
//
// Its own feature *and* its own flag, and both are deliberate. The feature
// means a build can be made that has no path to exec anything at all — the
// stronger promise, and the one a hardened deployment wants to be able to make
// about a binary rather than about how it is invoked. The flag means the lane
// never fires by default and never fires as a fallback from the engine's own
// read, because a fetch that left the engine's request log without anyone
// asking is exactly the silence this product is arranged against.
//
// It implies `browser`: the two lanes answer with one shape and share one
// WebVTT parser, which is what stops them disagreeing about a timestamp.
#[cfg(feature = "ytdlp")]
pub mod helper;
pub mod completion;
// `h5i box detect`. Not feature-gated, deliberately: the verbs are how a user
// finds out *why* a build cannot watch a box, and gating them behind the
// feature that provides the collector would hide that answer from exactly the
// builds that need it.
pub mod detect;
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
// `h5i box watch`. Not feature-gated: `browser_events` is exported from
// h5i-core unconditionally, and the verb is how someone finds out what a box
// is doing without opening a browser — which is exactly the build that has no
// console to open.
pub mod watch;
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
