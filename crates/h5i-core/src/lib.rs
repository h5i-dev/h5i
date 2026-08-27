// ─── public surface ─────────────────────────────────────────────────────────
// The domain: an environment is a confined worktree with a pinned policy, and
// a receipt is the record of what actually ran inside it. (`error` stays public
// because `H5iError` appears in the signatures of most of them.)
/// Mediated collaboration between boxed agents: agents share information
/// through a host-owned forum, never permissions.
pub mod forum;
/// The authority ceiling a box must be under to join a forum thread: a subset
/// check over the confinement it actually runs with, refused rather than
/// downgraded.
pub mod forum_authority;
/// The only path between a box and the forum: a read-only inbox in, a spooled
/// record out, and a host-side pass that decides authority at ingest.
/// The forum's one way in and out: an ordinary git remote, used the same way
/// whether the other participants are on this machine or another one.
pub mod forum_identity;
pub mod forum_sync;
pub mod forum_tender;
pub mod browser;
pub mod browser_events;
pub mod browser_frames;
pub mod browser_session;
pub mod browser_proxy;
pub mod cache;
pub mod control;
pub mod env;
pub mod export;
/// Where a box runs: the trait the lifecycle engine uses to place one on
/// another machine, with no transport in it (ROADMAP.md R1, R7).
pub mod placement;
/// Taking a tree from a machine we agreed might be compromised (ROADMAP.md R9).
pub mod quarantine;
pub mod receipt;
pub mod redact;
pub mod refstore;
// The box console (`h5i ui`) — a read-only HTTP view of the fleet. Optional:
// `--no-default-features` drops it, and with it axum, tokio, the embedded
// bundle, and the build script's dependency on Node.
#[cfg(feature = "web")]
pub mod server;
// Reading `share.json` for everything below `h5i-share`. See the module note:
// there were three hand-rolled probes here and they did not agree with the
// crate that writes the file.
pub mod share_record;
pub mod skill;
pub mod source;
pub mod storage;
pub mod termview;
pub mod token;
pub mod ui;
pub mod view;
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
/// The kernel tiers' effective-config dump (ROADMAP.md §V2) — Linux only.
#[cfg(target_os = "linux")]
pub use h5i_sandbox::effective;
/// The filesystem-authority validator (ROADMAP.md §VF.4) — the Rust port of
/// the Lean `H5iFs.validate`.
pub use h5i_sandbox::fs_authority;
/// The macOS Seatbelt backend — compiled on every Unix target so its pure
/// SBPL generator is testable (and differentially tested, ROADMAP §V3)
/// from the Linux job.
#[cfg(unix)]
pub use h5i_sandbox::seatbelt;
/// The runtime-detection lane (ROADMAP.md D1–D14). Re-exported unconditionally
/// — every build has to be able to read a receipt written by one that had the
/// collector — while the collector itself is behind this crate's `bpf`
/// feature.
pub use h5i_bpf as bpf;
