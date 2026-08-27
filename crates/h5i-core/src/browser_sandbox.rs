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
pub fn resolve_for(wants: &Wants<'_>) -> Result<Option<ResolvedPolicy>, H5iError> {
    let profile = profile_for(wants);
    let caps = sandbox::probe_host_for(IsolationClaim::Process);
    let Ok(policy) = sandbox::resolve(&profile, &caps) else {
        return Ok(None);
    };
    // Present is not runnable. A kernel can report Landlock, seccomp and user
    // namespaces and still refuse a confined `execve` under an AppArmor profile
    // or inside a CI container, and the only way to know is to try — which is
    // what `verify_exec` does, functionally, rather than reading capability
    // bits and hoping.
    match sandbox::verify_exec(&policy) {
        Ok(()) => Ok(Some(policy)),
        Err(_) => Ok(None),
    }
}

/// The profile a browser session runs under.
///
/// Narrower than the built-in `process` profile in the two ways that matter for
/// something whose whole job is to parse hostile input: it may write only its
/// own directory, and it may execute nothing.
fn profile_for(wants: &Wants<'_>) -> Profile {
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
        let p = profile_for(&wants(tmp.path()));
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
        let p = profile_for(&wants(tmp.path()));
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
        let p = profile_for(&wants(tmp.path()));
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
        assert!(profile_for(&wants(tmp.path())).secrets.is_empty());

        let named = vec!["H5I_SECRET_ACME".to_string()];
        let p = profile_for(&Wants {
            session_dir: tmp.path(),
            reads: &[],
            secrets: &named,
        });
        assert_eq!(p.secrets, named);
    }

    /// The sandbox contains the consequences, not the connection. If this ever
    /// becomes `Deny` the browser stops browsing, and if anyone reads `Host` as
    /// "the network is contained" the lane would be wrong.
    #[test]
    fn the_network_is_not_what_this_contains() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(profile_for(&wants(tmp.path())).net_mode, NetMode::Host);
    }

    /// A resident session outlives the command that opened it, so nothing here
    /// may kill it on a clock. `--expires-in` is the bound, and it is recorded.
    #[test]
    fn a_session_is_not_killed_on_a_wall_clock() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(profile_for(&wants(tmp.path())).wall_secs, 0);
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
