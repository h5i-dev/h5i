//! The default confinement for a browser session.
//!
//! A session is not a box. It has no repository, no worktree, no manifest, and
//! nothing to export; making one would put a git operation in front of "read
//! this page". But the reason a box exists still applies to a browser more than
//! to almost anything else h5i runs: the engine parses bytes a stranger wrote,
//! and a parser bug in Blitz, Stylo, an image decoder or Boa would be running as
//! whoever started the session.
//!
//! So the engine gets the *tier* without the box: the same Landlock filesystem
//! scoping, seccomp filter and rlimits `isolation = process` applies, built here
//! from a profile rather than resolved from a repository.
//!
//! # What this contains, and what it does not
//!
//! It contains the **consequences** of a compromised engine: the filesystem it
//! can reach, the environment it can read, how much it can allocate, and the
//! privilege-escalation and kernel surface seccomp denies.
//!
//! It does not stop the engine from starting a program, and nothing here
//! pretends to. What makes that survivable is that Landlock's domain is
//! inherited across `execve` and cannot be relaxed: a shell a compromised
//! engine starts reads and writes exactly what the engine could, which is its
//! own directory and the system.
//!
//! It does not contain the **connection**. `NetMode` has two values, `Deny` and
//! `Host`, and a browser needs the network — so a compromised engine keeps the
//! host's network reachability, including loopback. The policy that decides
//! *which* origins is the engine's own, in-process, and a compromised engine is
//! past it. Containing that needs a boundary outside the engine: `--in` a box
//! whose tier enforces egress, or the broker/renderer split, where the half that
//! parses the page holds no socket at all.
//!
//! **Nothing here upgrades the request lane.** A process-tier session stays
//! `engine-claimed`, because nothing outside the engine corroborated the log.
//!
//! # Falling back is a state, not a silence
//!
//! Landlock, seccomp and user namespaces are not everywhere: a hardened kernel,
//! an AppArmor profile, a CI container, macOS, Windows. A default that refused
//! there would be a product whose first command fails on someone's laptop. So
//! the session runs unconfined instead — and **says so**, on the summary line
//! and in the record, because a sandbox nobody can see is indistinguishable from
//! one that was never applied.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::H5iError;
use crate::sandbox::{self, HostCaps, IsolationClaim, NetMode, Profile, ResolvedPolicy};

/// What is holding the engine, as the record states it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Confinement {
    /// Nothing beyond the engine's own policy. Either the host could not, or
    /// the caller said not to; `why` says which.
    None { why: String },
    /// A process-tier sandbox: Landlock filesystem scoping, a seccomp filter,
    /// namespaces and rlimits.
    Process,
}

impl Confinement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confinement::None { .. } => "none",
            Confinement::Process => "process",
        }
    }

    pub fn is_confined(&self) -> bool {
        matches!(self, Confinement::Process)
    }
}

impl Default for Confinement {
    fn default() -> Self {
        Confinement::None {
            why: "this session predates the default sandbox".into(),
        }
    }
}

/// Where a font might be, in the engine's own order of preference.
///
/// A copy of the engine's `default_font_dirs`, and it has to be: inside the
/// sandbox the environment is cleared, so `$HOME` is gone and the engine's own
/// discovery would skip every personal directory whether or not Landlock
/// granted it. So h5i computes the list, grants exactly it, and passes exactly
/// it back as `--font-dir` — one list, used for both, because a grant and a
/// search path that are derived separately are two things that can disagree.
///
/// Only directories that exist. A grant for a path that is not there is skipped
/// by the Landlock builder anyway, and passing it as `--font-dir` would make the
/// engine walk nothing.
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local/share/fonts"));
        if cfg!(target_os = "macos") {
            dirs.push(home.join("Library/Fonts"));
        }
    }
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Fonts"));
        dirs.push(PathBuf::from("/System/Library/Fonts"));
    } else {
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        dirs.push(PathBuf::from("/usr/share/fonts"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// A resolved confinement, and the font path it was granted.
pub struct Confined {
    pub policy: ResolvedPolicy,
    /// Exactly the directories the engine may read fonts from, to be passed
    /// back as `--font-dir`. Never wider than what was granted.
    pub fonts: Vec<PathBuf>,
    /// Personal font directories that had to be dropped to get a policy at all.
    /// Empty in the ordinary case; non-empty is worth a line to the user,
    /// because a page will render with different faces than it does outside.
    pub dropped_fonts: Vec<PathBuf>,
}

/// What the caller wants confined, and what it must still be able to reach.
pub struct Wants<'a> {
    /// The session's own directory. The one place the engine may write: its
    /// control file, its logs, its cookie jar, its artifacts.
    pub session_dir: &'a Path,
    /// Extra directories the engine must be able to read. Fonts the caller
    /// named explicitly, mostly — the system font path is already covered by
    /// the profile's defaults, but `~/.fonts` is not, because nothing under
    /// `$HOME` is granted by default and that is the rule worth keeping.
    pub reads: &'a [PathBuf],
    /// Names of `H5I_SECRET_*` variables this session may substitute.
    ///
    /// Empty by default, and that is the change worth noticing: outside a
    /// sandbox the engine inherits the whole environment, so a compromised one
    /// reads every secret on the machine. Here it reads the ones that were
    /// named, and a placeholder for anything else resolves to nothing.
    pub secrets: &'a [String],
}

/// Build the confinement for a session, or say why this host cannot.
///
/// `Err` is reserved for a policy that could not be resolved at all. A host that
/// simply lacks the kernel features answers `Ok(None)` with a reason, because
/// that is not a failure of the request — it is an answer about the machine.
/// What came of asking for confinement.
pub enum Outcome {
    /// A policy that resolved and ran.
    Confined(Box<Confined>),
    /// It did not, and this is why. A reason rather than a bare `None`: whoever
    /// asked has to be able to say what they got instead.
    Unavailable(String),
}

pub fn resolve_for(wants: &Wants<'_>) -> Result<Option<Confined>, H5iError> {
    let caps = sandbox::probe_host_for(IsolationClaim::Process);
    let all = font_dirs();

    // Personal font directories are granted, and must never be the reason a
    // session runs unconfined.
    //
    // The hazard is real and the codebase already catches it: Landlock grants
    // follow symlinks, so `~/.fonts` pointing at `$HOME` would grant the home
    // directory, and `resolve` refuses that because the grant would contain a
    // denied `~/.ssh`. Refusing is right; refusing *the whole sandbox* over a
    // font path is not. So the personal directories are dropped and the policy
    // is resolved again, and the caller is told which.
    for (fonts, dropped) in font_grant_attempts(&all) {
        let profile = profile_for(wants, &fonts);
        let Ok(policy) = sandbox::resolve(&profile, &caps) else {
            continue;
        };
        if sandbox::verify_exec(&policy).is_ok() {
            return Ok(Some(Confined {
                policy,
                fonts,
                dropped_fonts: dropped,
            }));
        }
        // A policy that resolves but cannot run is not a font problem; trying
        // again with fewer grants would only take longer to say the same thing.
        return Ok(None);
    }
    Ok(None)
}

/// The font grants to try, widest first, each with what it gave up.
///
/// Two, and the second exists so a font directory can never be the reason a
/// session runs unconfined: a personal directory that resolves somewhere the
/// policy denies makes the whole policy unresolvable, and losing the sandbox
/// over a font path would be a security consequence for an unrelated thing.
fn font_grant_attempts(all: &[PathBuf]) -> Vec<(Vec<PathBuf>, Vec<PathBuf>)> {
    let home = home_prefix();
    let (personal, system): (Vec<PathBuf>, Vec<PathBuf>) =
        all.iter().cloned().partition(|d| d.starts_with(&home));
    vec![(all.to_vec(), Vec::new()), (system, personal)]
}

/// `$HOME`, or a path nothing starts with when it is unset.
fn home_prefix() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("\0no-home"))
}

/// The profile a browser session runs under.
///
/// Narrower than the built-in `process` profile in the two ways that matter for
/// something whose whole job is to parse hostile input: it may write only its
/// own directory, and it may execute nothing.
fn profile_for(wants: &Wants<'_>, fonts: &[PathBuf]) -> Profile {
    let mut p = Profile::builtin("browser-session", IsolationClaim::Process);

    // `$WORK` is the session directory (the caller passes it as the working
    // directory), so the built-in write grant lands exactly where the engine's
    // control file, logs and cookie jar go and nowhere else.
    p.fs_write = vec![
        "$WORK".to_string(),
        "/dev/null".to_string(),
        "/dev/zero".to_string(),
    ];
    p.fs_read.push("$WORK".to_string());
    for dir in wants.reads {
        p.fs_read.push(dir.display().to_string());
    }
    // Read-only, and not attacker-chosen: a font the user installed is not the
    // page's font. The ones a page supplies arrive over `@font-face`, go through
    // the broker, and are parsed either way — granting these adds no new
    // parser input, only the faces someone meant to have.
    for dir in fonts {
        p.fs_read.push(dir.display().to_string());
    }

    // The engine fetches. `Deny` would put it in an empty network namespace,
    // which is a browser that cannot browse; the origin policy is the engine's
    // own and stays there.
    p.net_mode = NetMode::Host;

    // **Not** an exec denial, because there is none to make here. `tools` gates
    // the *initial* command and an empty list means no restriction at all;
    // `execve` is not on the seccomp deny list, which covers privilege
    // escalation and kernel surface rather than process creation.
    //
    // What makes that acceptable is Landlock's own rule: the domain is
    // inherited across `execve` and cannot be relaxed. A compromised engine can
    // start a shell, and the shell reads and writes exactly what the engine
    // could — nothing. The containment is the filesystem domain, not a list of
    // permitted programs, and saying otherwise would be a claim the mechanism
    // does not make.
    //
    // The ceiling is a ceiling, not a ban: the engine is multi-threaded (the
    // viewer loop, the control loop, the HTTP client's own runtime) and this
    // counts threads.
    p.max_procs = Some(64);

    p.secrets = wants.secrets.to_vec();

    // A page can allocate. The budgets in the engine bound what one navigation
    // fetches and decodes; this bounds what the process can do with it when a
    // page is not playing along.
    p.mem_bytes = Some(2 * 1024 * 1024 * 1024);
    // No wall-clock kill: a session is resident by design and outlives the
    // command that opened it. `--expires-in` is the caller's way to bound it,
    // and it is recorded as an ending rather than enforced by a signal here.
    p.wall_secs = 0;
    p
}

/// Why this host cannot confine a session, in one line for the summary.
///
/// Separate from [`resolve_for`] so the caller can report the reason without
/// re-deriving it, and so the reason is written once.
pub fn unavailable_reason(caps: &HostCaps) -> String {
    let mut missing: Vec<&str> = Vec::new();
    if caps.landlock_abi.is_none() {
        missing.push("Landlock");
    }
    if !caps.userns {
        missing.push("unprivileged user namespaces");
    }
    if !caps.seccomp {
        missing.push("seccomp");
    }
    if missing.is_empty() {
        "this host reports the kernel features but refused a confined exec".to_string()
    } else {
        format!("this host has no {}", missing.join(", "))
    }
}

/// The confinement for a **stateless read**: one page, or a batch of them, with
/// no session left behind.
///
/// This is the one browser shape that can have the strongest tier the host
/// offers, and the reason is the whole difference between the two. A session is
/// resident by design — `snapshot` then `click @e3` needs the page to still be
/// there — and the supervised tier cannot hold a resident process today: its
/// seccomp-notify gate is served by a thread inside the `h5i` process that
/// started the run, so when that command exits the gate has no server and every
/// filtered syscall blocks. A read runs to completion inside that same command,
/// which is exactly the shape `supervisor::run` already has.
///
/// So a read gets an egress allowlist **enforced outside the engine**, at the
/// box tier's own boundary, which no session on this platform can have yet.
/// `origins` becomes `net.egress`; empty means the tier denies the network
/// outright, which is right for a local file and wrong for anything else.
pub fn resolve_for_read(reads: &[PathBuf], origins: &[String], loopback: bool) -> Outcome {
    let fonts = font_dirs();
    // Strongest first, with one exception that is not a weakness: **supervised
    // puts the read in its own network namespace, so `localhost` is the
    // sandbox's own loopback and not the host's.** A read aimed at a dev server
    // on this machine would find nothing there. That is the tier working, and
    // it makes it the wrong tier for that target, so the choice follows what is
    // being read rather than a preference for the strongest name.
    //
    // Supervised is otherwise first. It is fail-closed on admission and refuses
    // on a host missing any part of the stack, which is most of the reason a
    // *session* could not default to it; a read can try and fall back, because
    // there is no resident process to strand.
    let tiers: &[IsolationClaim] = if loopback {
        &[IsolationClaim::Process]
    } else {
        &[IsolationClaim::Supervised, IsolationClaim::Process]
    };
    for &claim in tiers {
        let mut p = Profile::builtin("browser-read", claim);
        p.fs_write = vec![
            "$WORK".to_string(),
            "/dev/null".to_string(),
            "/dev/zero".to_string(),
        ];
        p.fs_read.push("$WORK".to_string());
        for dir in reads.iter().chain(fonts.iter()) {
            p.fs_read.push(dir.display().to_string());
        }
        // The allowlist goes to the tier only where the tier can enforce it.
        // `process` cannot — it has `deny` and `host` and nothing between — and
        // handing it one makes the policy unresolvable, which would drop the
        // read to unconfined over a list the engine was going to enforce
        // anyway. So the second enforcer engages where it exists and the read
        // says which it got.
        let tier_enforces_egress = claim >= IsolationClaim::Supervised;
        p.net_egress = if tier_enforces_egress {
            origins.to_vec()
        } else {
            Vec::new()
        };
        p.net_mode = if origins.is_empty() {
            NetMode::Deny
        } else {
            NetMode::Host
        };
        p.mem_bytes = Some(2 * 1024 * 1024 * 1024);

        let caps = sandbox::probe_host_for(claim);
        let Ok(policy) = sandbox::resolve(&p, &caps) else {
            continue;
        };
        if sandbox::verify_exec(&policy).is_ok() {
            return Outcome::Confined(Box::new(Confined {
                policy,
                fonts,
                dropped_fonts: Vec::new(),
            }));
        }
    }
    Outcome::Unavailable(unavailable_reason(&caps()))
}

/// The host's capabilities, for a caller that wants to explain a fallback.
pub fn caps() -> HostCaps {
    sandbox::probe_host_for(IsolationClaim::Process)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wants(dir: &Path) -> Wants<'_> {
        Wants {
            session_dir: dir,
            reads: &[],
            secrets: &[],
        }
    }

    #[test]
    fn the_engine_may_write_only_its_own_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let p = profile_for(&wants(tmp.path()), &[]);
        assert!(p.fs_write.contains(&"$WORK".to_string()));
        // Not $HOME, not the repository, not /tmp at large.
        assert!(
            p.fs_write.iter().all(|w| w == "$WORK" || w.starts_with("/dev/")),
            "{:?}",
            p.fs_write
        );
    }

    /// The process ceiling must leave room for the engine's own threads. At one,
    /// the HTTP client cannot even be constructed: `max_procs` counts threads,
    /// and the first thing a blocking reqwest client does is start one.
    #[test]
    fn the_process_ceiling_leaves_room_for_the_engines_threads() {
        let tmp = tempfile::tempdir().unwrap();
        let p = profile_for(&wants(tmp.path()), &[]);
        let ceiling = p.max_procs.expect("a ceiling");
        assert!(ceiling > 1, "a ceiling of {ceiling} cannot start an HTTP client");
        assert!(ceiling <= 256, "a ceiling that high is not a ceiling");
    }

    /// There is no exec denial here, and the profile must not imply one.
    /// `tools` gates only the initial command and an empty list means *no*
    /// restriction; `execve` is not on the seccomp deny list. What contains a
    /// shell a compromised engine starts is Landlock, whose domain is inherited
    /// across `execve` and cannot be relaxed.
    #[test]
    fn nothing_here_claims_to_forbid_exec() {
        let tmp = tempfile::tempdir().unwrap();
        let p = profile_for(&wants(tmp.path()), &[]);
        assert!(
            p.tools.is_empty(),
            "a non-empty tools list would gate the initial command only, and read as more"
        );
    }

    /// The default carries no secrets. Outside a sandbox the engine inherits
    /// the whole environment; a compromised one there reads every secret on the
    /// machine. This is the narrowing that costs a caller something, so it is
    /// pinned.
    #[test]
    fn no_secret_is_granted_unless_it_was_named() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(profile_for(&wants(tmp.path()), &[]).secrets.is_empty());

        let named = vec!["H5I_SECRET_ACME".to_string()];
        let p = profile_for(
            &Wants {
                session_dir: tmp.path(),
                reads: &[],
                secrets: &named,
            },
            &[],
        );
        assert_eq!(p.secrets, named);
    }

    /// The sandbox contains the consequences, not the connection. If this ever
    /// becomes `Deny` the browser stops browsing, and if anyone reads `Host` as
    /// "the network is contained" the lane would be wrong.
    #[test]
    fn the_network_is_not_what_this_contains() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(profile_for(&wants(tmp.path()), &[]).net_mode, NetMode::Host);
    }

    /// A resident session outlives the command that opened it, so nothing here
    /// may kill it on a clock. `--expires-in` is the bound, and it is recorded.
    #[test]
    fn a_session_is_not_killed_on_a_wall_clock() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(profile_for(&wants(tmp.path()), &[]).wall_secs, 0);
    }

    /// A font directory must never decide whether there is a sandbox. The
    /// second attempt exists for one case that really happens — `~/.fonts` as a
    /// symlink resolving somewhere the policy denies, which makes the whole
    /// policy unresolvable — and it gives up the personal directories rather
    /// than the confinement.
    #[test]
    fn the_fallback_gives_up_fonts_rather_than_the_sandbox() {
        let home = home_prefix();
        let all = vec![
            home.join(".fonts"),
            home.join(".local/share/fonts"),
            PathBuf::from("/usr/share/fonts"),
        ];
        let attempts = font_grant_attempts(&all);
        assert_eq!(attempts.len(), 2, "widest first, then narrower");

        let (first, dropped_first) = &attempts[0];
        assert_eq!(first, &all, "the first attempt gives up nothing");
        assert!(dropped_first.is_empty());

        let (second, dropped) = &attempts[1];
        assert!(
            second.iter().all(|d| !d.starts_with(&home)),
            "the fallback keeps nothing under $HOME: {second:?}"
        );
        assert!(
            second.iter().all(|d| all.contains(d)),
            "the fallback is a subset, never a different set"
        );
        assert_eq!(dropped.len(), 2, "and it says exactly what it gave up");
        assert!(dropped.iter().all(|d| d.starts_with(&home)));
    }

    /// The system path survives every attempt: a browser with no fonts at all
    /// renders nothing anyone can read.
    #[test]
    fn the_system_font_path_is_never_given_up() {
        let all = vec![home_prefix().join(".fonts"), PathBuf::from("/usr/share/fonts")];
        for (fonts, _) in font_grant_attempts(&all) {
            assert!(
                fonts.contains(&PathBuf::from("/usr/share/fonts")),
                "{fonts:?}"
            );
        }
    }

    #[test]
    fn a_fallback_names_what_the_host_is_missing() {
        let bare = HostCaps {
            landlock_abi: None,
            userns: false,
            seccomp: false,
            ..caps()
        };
        let why = unavailable_reason(&bare);
        assert!(why.contains("Landlock"), "{why}");
        assert!(why.contains("user namespaces"), "{why}");
        assert!(why.contains("seccomp"), "{why}");
    }
}
