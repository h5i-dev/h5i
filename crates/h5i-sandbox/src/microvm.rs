//! The `isolation=microvm` backend: run an environment's command inside a
//! **hardware-isolated microVM** via [microsandbox](https://microsandbox.dev)
//! (`msb`), enforcing the `net.egress` allowlist in the VM's own network stack
//! rather than a host-side HTTP proxy.
//!
//! ### Why this tier exists
//!
//! `container.rs` enforces `net.egress` at **L7**. That blocks the dominant
//! exfiltration path (`curl`/`pip`/`npm` honouring `HTTP(S)_PROXY`) and nothing
//! else: a process that ignores the proxy env and opens a raw socket to any IP
//! the rootless NAT permits gets through. Two things change here:
//!
//! 1. **The boundary is a virtual machine.** The guest runs its own kernel on
//!    KVM or Hypervisor.framework, so a kernel exploit in the box is contained
//!    by the hypervisor rather than by the host kernel it just subverted.
//! 2. **Egress is filtered by address.** The allowlist becomes
//!    `--net-default-egress deny` plus one `--net-rule` per destination,
//!    evaluated by the VM's virtual network stack. A raw socket to an unlisted
//!    IP is dropped and there is no `HTTP_PROXY` to ignore. DNS-rebind
//!    protection stays on.
//!
//! ### What it costs
//!
//! - **The host must support virtualization**: `/dev/kvm` on Linux, Apple
//!   Silicon on macOS. Without it (plain WSL2, most CI runners) [`resolve`]
//!   refuses rather than downgrading.
//! - **No per-request egress tally.** The container tier's proxy sees every
//!   CONNECT and reports allow/deny counts; a VM netstack drops packets without
//!   saying which, so [`ExecOutcome::egress`] is `None`. Stronger enforcement,
//!   weaker evidence, so we report the tier's rules at session start rather
//!   than pretend to a tally we do not have.
//! - **No in-box tee shim.** The container tier self-mounts its image at
//!   `/.h5i/orig` so a shadowed `/bin/sh` still has a real shell to exec; a VM
//!   has no image to self-mount. The primary observation path, the read-only
//!   managed-settings mount carrying the unkillable `wrap-bash` hook, works
//!   here as it does under `container`, and the capture spool mounts the same.
//!
//! ### Secrets never enter the host argv
//!
//! `msb` has no name-only env forwarding (`--env` takes `KEY=VALUE`), and a
//! brokered credential in `msb run`'s argv would be published to every local
//! user through `/proc/<pid>/cmdline`. So this backend passes no environment on
//! the command line at all: it writes a `0600` preload script host-side,
//! registers it with `--script-path` (whose contents travel over a config fd,
//! not argv), and runs the command through it. See [`preload_script`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::H5iError;
use crate::sandbox_policy::{ExecOutcome, InteractiveOutcome, NetMode, Profile, ResolvedPolicy};
use h5i_error::redact::sanitize_block;

/// Minimum `msb` version this adapter targets. Below it, `--net-rule`,
/// `--net-default-egress`, `--script-path` and the explicit `--mount-dir` /
/// `--mount-file` forms are absent or spelled differently, and a run would fail
/// with a confusing clap error instead of an actionable one.
pub const MIN_MSB_VERSION: (u64, u64) = (0, 6);

/// Where the guest keeps `--script-path` scripts. `msb`'s guest agent creates
/// the directory, marks the entries executable, and puts it first on `PATH`
/// (`microsandbox_protocol::SCRIPTS_PATH`).
const GUEST_SCRIPTS_DIR: &str = "/.msb/scripts";

/// Script name for the environment preload wrapper. Registered with
/// `--script-path`, so it lands at `{GUEST_SCRIPTS_DIR}/{PRELOAD_SCRIPT_NAME}`.
const PRELOAD_SCRIPT_NAME: &str = "h5i-env";

/// The workspace mountpoint inside the guest. Same as the container tier, so a
/// profile, a hook, and a persona file all mean the same path on both.
const WORK_MOUNT: &str = "/work";

/// Capture spool for in-box `h5i capture run` — identical to the container
/// tier's `/.h5i/spool`, so the in-box side needs no per-tier knowledge.
const SPOOL_MOUNT: &str = "/.h5i/spool";

/// Per-env inbound mailbox, mounted READ-ONLY (the host fans messages in).
const INBOX_MOUNT: &str = "/.h5i/inbox";

/// Where a background service's log lands inside the guest.
///
/// **Logs only, never the service records.** The records
/// (`<env_dir>/services/<name>.json`) carry the pid the host later signals, and
/// a box able to rewrite one could set `runtime: host` with a pid of its
/// choosing and have `service_stop` `killpg` an arbitrary process group on the
/// host. So the guest gets a directory of its own that holds nothing but log
/// bytes, and the records stay where the box cannot reach them.
const SERVICES_MOUNT: &str = "/.h5i/services";

/// Where per-run material staged by the host lands inside a **warm** guest.
///
/// A reused guest cannot carry per-run secrets the way a one-shot run does:
/// `--script-path` is create-time, and the brokered credentials it would carry
/// are minted per run. `msb exec --env KEY=VALUE` is not an option either — it
/// is the `/proc/<pid>/cmdline` exposure this module exists to avoid. So the
/// warm path mounts one small host-owned directory read-write and stages the
/// same generated script into it per run, keeping values out of argv exactly as
/// before.
const RUN_MOUNT: &str = "/.h5i/run";

/// Set to `1` to force the one-shot path (a fresh guest per command) even where
/// reuse is available. Both the escape hatch for a box that wants a pristine
/// guest every time, and the way to bisect a suspected reuse bug.
pub const NO_REUSE_ENV: &str = "H5I_MICROVM_NO_REUSE";

/// How long a warm guest may sit idle before `msb` stops it. Stopping is
/// lossless (the guest's disk survives) but it is *not* free to undo: an exec
/// into a stopped guest costs a full boot and leaves it stopped again, so
/// [`ensure_guest`] starts it explicitly rather than letting exec do it. The
/// timeout exists because a guest with no bound is a guest that outlives the
/// laptop lid closing — `msb` ships no default of its own.
const GUEST_IDLE_TIMEOUT: &str = "30m";

/// `msb` accepts names well past this; the cap keeps a guest name readable in
/// `msb list` while leaving room for the digest suffix that makes it correct.
const GUEST_LABEL_MAX: usize = 40;

// ─── runtime detection ──────────────────────────────────────────────────────

/// A detected microVM runtime.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// The binary to invoke (`msb`).
    pub bin: String,
    /// Parsed `(major, minor)` of `msb --version`, checked against
    /// [`MIN_MSB_VERSION`].
    pub version: (u64, u64),
}

/// Cheap presence check: is the `msb` binary on PATH at all? Used for
/// discoverability hints that need "is microsandbox installed?" and not "can
/// this host actually boot a microVM?" — the latter is [`probe`].
pub fn msb_present() -> bool {
    msb_version().is_some()
}

/// Detect a usable microVM runtime: an `msb` new enough for this adapter's flag
/// set **and** a host that can actually run a VM. Returns `None` when either
/// half is missing — this tier is never approximated.
///
/// Memoized in-process (both halves are cheap: one `msb --version` exec and a
/// `stat`, so unlike the container tier's ~1.3s `podman info` there is nothing
/// worth a cross-invocation cache). `H5I_NO_PROBE_CACHE=1` bypasses the memo so
/// `env probe` always reports a live verdict.
pub fn probe() -> Option<Runtime> {
    if std::env::var_os("H5I_NO_PROBE_CACHE").is_some() {
        return probe_uncached();
    }
    static PROBE: std::sync::OnceLock<Option<Runtime>> = std::sync::OnceLock::new();
    PROBE.get_or_init(probe_uncached).clone()
}

/// Uncached [`probe`] — the **diagnostic** path, mirroring
/// [`crate::container::probe_fresh`]. `env probe`, `env capabilities` and the
/// console's `/api/probe` describe the host as it is *now*, so they must not be
/// served the memo an earlier caller filled in.
pub fn probe_fresh() -> Option<Runtime> {
    probe_uncached()
}

fn probe_uncached() -> Option<Runtime> {
    let version = msb_version()?;
    if version < MIN_MSB_VERSION {
        return None;
    }
    if virtualization_detail().is_some() {
        return None;
    }
    Some(Runtime {
        bin: "msb".into(),
        version,
    })
}

/// `(major, minor)` from `msb --version`, or `None` when the binary is absent
/// or its output is unparseable. Clap prints `msb <semver>`; we scan for the
/// first dotted-numeric token so a future banner change doesn't break detection.
fn msb_version() -> Option<(u64, u64)> {
    let out = std::process::Command::new("msb")
        .arg("--version")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// Pull `(major, minor)` out of a version banner. Pure, so the parsing rule is
/// testable without an `msb` on PATH.
pub fn parse_version(text: &str) -> Option<(u64, u64)> {
    for token in text.split(|c: char| c.is_whitespace() || c == 'v') {
        let mut parts = token.split('.');
        let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let (Ok(major), Ok(minor)) = (major.parse::<u64>(), minor.parse::<u64>()) {
            return Some((major, minor));
        }
    }
    None
}

/// Why this host cannot run a microVM, or `None` when it can. Separated from
/// [`probe`] so `env probe`/`doctor` can say *what* is missing instead of a bare
/// "microvm unavailable" — the difference between "install msb" and "enable
/// nested virtualization", which are very different afternoons.
pub fn virtualization_detail() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // The gate is not "does the file exist" but "can this user open it":
        // a host with KVM compiled in but the caller outside the `kvm` group
        // fails at boot, and finding that out here yields the actionable message.
        match std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm") {
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(
                "/dev/kvm is absent — this kernel has no KVM (common on WSL2 without nested \
                 virtualization, and on most cloud CI runners)"
                    .into(),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(
                "/dev/kvm exists but is not openable by this user — add yourself to the `kvm` \
                 group (`sudo usermod -aG kvm $USER`) and re-login"
                    .into(),
            ),
            Err(e) => Some(format!("/dev/kvm is not usable: {e}")),
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Hypervisor.framework is present on every supported macOS, but
        // microsandbox's guest kernel (libkrun) ships for Apple Silicon only.
        if cfg!(target_arch = "aarch64") {
            None
        } else {
            Some(
                "microsandbox's microVM guest requires Apple Silicon; this Mac is Intel \
                 (x86_64)"
                    .into(),
            )
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Some("the microvm tier supports Linux (KVM) and macOS (Apple Silicon) only".into())
    }
}

// ─── egress allowlist → netstack rules ──────────────────────────────────────

/// How networking is wired for a microVM run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetPlan {
    /// No network at all (`net.mode = deny`, no egress allowlist).
    None,
    /// Unfiltered outbound (`net.mode = host`).
    Host,
    /// Default-deny egress plus these `--net-rule` tokens — the real allowlist.
    Allow(Vec<String>),
}

/// Translate h5i's `net.egress` entries into microsandbox `--net-rule` tokens.
///
/// h5i spells an allowlist entry `host`, `host:port`, `.suffix` or `*.suffix`;
/// microsandbox spells a rule `<action>[:<direction>]@<target>[:<proto>[:<ports>]]`.
/// The mapping is deliberately conservative — an entry we cannot translate
/// *exactly* is an error, never a rule that quietly allows more (or less) than
/// the policy says:
///
/// - `example.com`      → `allow@example.com`                (any proto, any port)
/// - `example.com:443`  → `allow@example.com:tcp:443`
/// - `.example.com`     → `allow@domain=example.com` **and** `allow@suffix=example.com`
///   — h5i's wildcard matches the apex as well as subdomains, and microsandbox's
///   `suffix=` target covers only the subdomain half, so both tokens are emitted.
/// - a bare IP or CIDR passes through as its own target.
///
/// No DNS rule is emitted. microsandbox resolves names at the gateway and keeps
/// that path reachable under `--net-default-egress deny`, so a domain rule is
/// enough on its own: a box allowed `example.com` resolves and reaches it, and a
/// box denied `wikipedia.org` still cannot. An explicit `allow@dns` was emitted
/// here until it turned out `msb` 0.6.8 rejects the token outright ("the `dns`
/// target supports `tcp`, `udp`, or `any`, not `dns`"), which failed *every*
/// microvm run carrying an allowlist — the default agent profiles included.
///
/// Fail-closed rejections: an entry carrying `,` or `@` (which would split or
/// re-target the token), and a single-label wildcard such as `.com` (which
/// microsandbox refuses, and which is not an allowlist anybody meant to write).
pub fn egress_rule_tokens(egress: &[String]) -> Result<Vec<String>, H5iError> {
    let mut tokens: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut push = |token: String, tokens: &mut Vec<String>| {
        if seen.insert(token.clone()) {
            tokens.push(token);
        }
    };

    for raw in egress {
        // One grammar for every tier: `container::parse_egress_rule` is the
        // single definition, so the strictness here and the allowlist the
        // container proxy enforces cannot drift apart again.
        let Some((host, wildcard, port)) = crate::container::parse_egress_rule(raw)? else {
            continue;
        };
        let qualifier = match port {
            Some(p) => format!(":tcp:{p}"),
            None => String::new(),
        };
        if wildcard {
            push(format!("allow@domain={host}{qualifier}"), &mut tokens);
            push(format!("allow@suffix={host}{qualifier}"), &mut tokens);
        } else {
            push(format!("allow@{host}{qualifier}"), &mut tokens);
        }
    }

    Ok(tokens)
}

// ─── environment preload (keeping values out of argv) ───────────────────────

/// A generated preload script and the host path it lives at. Dropping the guard
/// removes the file — the values inside outlive neither the run nor a crash any
/// longer than they must.
pub struct PreloadScript {
    pub path: PathBuf,
}

impl Drop for PreloadScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Render the preload script: `export` one line per variable, then `exec "$@"`
/// so argv, stdin, the TTY and the exit code all pass through untouched.
///
/// Values are single-quoted with the POSIX `'\''` escape, which is total — there
/// is no byte a shell single-quoted string cannot carry — so a credential
/// containing quotes, `$`, backticks or newlines survives verbatim and cannot
/// break out into command position. Pure, so the quoting rule is unit-tested.
pub fn preload_script(env: &[(String, String)]) -> String {
    let mut s = env_exports(env);
    s.push_str("exec \"$@\"\n");
    s
}

/// The `export` half on its own, for a caller that wants the values in *its*
/// shell rather than in a command it is about to exec.
///
/// A background service needs this: it is started by a shell that must source
/// the values, delete the file, and only then detach the service, so the
/// credentials exist on disk for the length of one exec rather than for the
/// life of the service. `exec "$@"` would be wrong there — the sourcing shell
/// has more to do afterwards.
pub fn env_exports(env: &[(String, String)]) -> String {
    let mut s = String::from(
        "#!/bin/sh\n\
         # h5i microvm env preload — generated per run, never checked in.\n\
         # Values live here rather than in `msb run`'s argv, which /proc publishes.\n",
    );
    for (key, value) in env {
        s.push_str(&format!("export {key}='{}'\n", value.replace('\'', r"'\''")));
    }
    s
}

/// Write the preload script for this run under the env directory with `0600`
/// permissions.
///
/// Unlike the container tier's shim this is **not** best-effort: it carries the
/// profile's `env.pass` allowlist and every brokered secret, so a box that
/// silently ran without it would be a box missing its credentials and its
/// `H5I_ENV_*` wiring. Any failure is an error.
fn write_preload(work: &Path, env: &[(String, String)]) -> Result<PreloadScript, H5iError> {
    let dir = microvm_dir(work)?;
    write_env_script(&dir, "preload", env)
}

/// `<env_dir>/microvm` — where this tier stages host-side material for a box.
fn microvm_dir(work: &Path) -> Result<PathBuf, H5iError> {
    let env_dir = work.parent().ok_or_else(|| {
        H5iError::Metadata(format!(
            "workspace '{}' has no parent env directory to stage the preload script in",
            work.display()
        ))
    })?;
    Ok(env_dir.join("microvm"))
}

/// `<env_dir>/microvm/run` — the per-run staging directory mounted read-write
/// into a warm guest at [`RUN_MOUNT`]. Separate from [`microvm_dir`] because
/// *this* one is visible to the box, and nothing else h5i keeps under the env
/// directory should be.
fn run_stage_dir(work: &Path) -> Result<PathBuf, H5iError> {
    Ok(microvm_dir(work)?.join("run"))
}

/// Remove credential scripts a previous run left in the staging directory.
///
/// [`PreloadScript`]'s `Drop` is the normal cleanup and it cannot cover SIGKILL,
/// a `panic = "abort"` build, or an OOM. That was harmless while the directory
/// was never mounted — on the one-shot path the script reached the runtime over
/// a config fd. The warm path mounts it into a guest that now **outlives the
/// run**, so a crashed `box run` would otherwise leave its brokered credentials
/// readable by that box's long-lived services indefinitely, including after the
/// credential was rotated host-side.
///
/// Safe to do unconditionally here: every entry point that stages a script
/// holds the box's run lock, so no other run's script is live when this runs.
fn sweep_stale_env_scripts(stage: &Path) {
    let Ok(rd) = std::fs::read_dir(stage) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Only the shapes this module writes, for the same reason the marker
        // sweep is gated: this directory is writable by the box.
        let ours = (name.starts_with("env-") || name.starts_with("svc-")) && name.ends_with(".sh");
        if ours {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `<env_dir>/microvm/service-logs` — mounted read-write into the guest at
/// [`SERVICES_MOUNT`]. Under the microvm staging directory rather than beside
/// the service *records*, which the box must never be able to rewrite.
fn service_log_dir(work: &Path) -> Result<PathBuf, H5iError> {
    Ok(microvm_dir(work)?.join("service-logs"))
}

/// The host path of a service's log, for the record the host keeps.
pub fn service_log_path(work: &Path, service: &str) -> Result<PathBuf, H5iError> {
    Ok(service_log_dir(work)?.join(format!("{service}.log")))
}

/// Write one `0600` env script into `dir`, named `<prefix>-<pid>-<seq>.sh`.
fn write_env_script(
    dir: &Path,
    prefix: &str,
    env: &[(String, String)],
) -> Result<PreloadScript, H5iError> {
    write_env_script_with(dir, prefix, env, preload_script)
}

/// [`write_env_script`], with the caller choosing how the script is rendered —
/// [`preload_script`] to wrap a command, [`env_exports`] to be sourced.
fn write_env_script_with(
    dir: &Path,
    prefix: &str,
    env: &[(String, String)],
    render: fn(&[(String, String)]) -> String,
) -> Result<PreloadScript, H5iError> {
    std::fs::create_dir_all(dir).map_err(|e| H5iError::with_path(e, dir))?;
    // pid **and** sequence, matching `SandboxGuard::new`: a pid alone repeats
    // across invocations, and two runs inside one process would share a name.
    let path = dir.join(format!(
        "{prefix}-{}-{}.sh",
        std::process::id(),
        RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // Created `0600` *before* anything is written to it. `fs::write` would make
    // the file at the umask default and only then chmod it, leaving a window in
    // which any local user could read the brokered secrets this script carries —
    // the very exposure the module avoids by keeping them out of argv, so the
    // same threat model applies here.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // A leftover from a recycled pid is ours to clear; `create_new` then
        // guarantees we are the file's creator and its mode was never wider.
        let _ = std::fs::remove_file(&path);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        f.write_all(render(env).as_bytes())
            .map_err(|e| H5iError::with_path(e, &path))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, render(env)).map_err(|e| H5iError::with_path(e, &path))?;
    check_spec_path(&path, "preload script")?;
    Ok(PreloadScript { path })
}

/// microsandbox's `--mount-*` and `--script-path` specs are colon-separated, so
/// a host path containing `:` cannot be represented unambiguously. Fail closed:
/// a dropped mount is a policy the box does not actually run under.
fn check_spec_path(path: &Path, what: &str) -> Result<(), H5iError> {
    if path.display().to_string().contains(':') {
        return Err(H5iError::Metadata(format!(
            "microvm {what} path contains ':' and cannot be represented in microsandbox's \
             SOURCE:DEST spec syntax: {} — move the repo out of a colon'd path (fail-closed)",
            path.display()
        )));
    }
    Ok(())
}

// ─── argv construction ──────────────────────────────────────────────────────

/// Everything the argv builder needs beyond the policy itself. A struct rather
/// than a dozen positional parameters: this one is security-critical and read
/// far more often than it is written.
pub struct RunPlan<'a> {
    /// Resolved base image (`container.image` in the profile).
    pub image: &'a str,
    /// Unique sandbox name, used for cleanup after a wall-clock kill.
    pub name: &'a str,
    /// Networking for this run.
    pub net: &'a NetPlan,
    /// The command to run inside the guest.
    pub argv: &'a [String],
    /// Host path of the generated env preload script.
    pub preload: &'a Path,
    /// `None` → captured run (no stdin, wall-clock enforced). `Some(tty)` →
    /// interactive session, allocating a pseudo-TTY when the caller has one.
    pub tty: Option<bool>,
    /// Managed-settings.json (the unkillable `wrap-bash` hook) to mount
    /// read-only at Claude's managed-settings path. `None` → no injection.
    pub managed_settings: Option<&'a Path>,
}

/// Push every mount the box gets, in one place.
///
/// Both the one-shot ([`build_run_argv`]) and warm ([`build_create_argv`])
/// paths call this, which is the point: the mount set *is* a large part of what
/// the box may touch, and two copies of it would be two policies that drift
/// apart on the next change. The one-shot path passes the same arguments it
/// always did, so its argv is byte-identical to before this was factored out.
fn push_mount_set(
    a: &mut Vec<String>,
    policy: &ResolvedPolicy,
    work: &Path,
    managed_settings: Option<&Path>,
) {
    // The guest rootfs is the image; only the mounts below are shared with the
    // host, and the workspace is the only one writable by default.
    a.push("--mount-dir".into());
    a.push(format!("{}:{WORK_MOUNT}:rw", work.display()));
    a.push("--workdir".into());
    a.push(WORK_MOUNT.into());

    // Warm dependency caches, read-only at the package manager's own path — a
    // cache a box could write is a mutable surface shared between boxes.
    for b in &policy.ro_binds {
        a.push("--mount-dir".into());
        a.push(format!("{}:{}:ro", b.backing.display(), b.target.display()));
    }
    if let Some(b) = &policy.cache_write {
        a.push("--mount-dir".into());
        a.push(format!("{}:{}:rw", b.backing.display(), b.target.display()));
    }

    // Interactive config lockdown: the agent's own hook config, mounted read-only
    // over its place in the workspace. `$WORK` is writable, so without this the
    // in-box agent could rewrite the file that defines the observation hook and
    // then be unobserved. `ProtectedHookConfigGuard` has already made each file
    // exist host-side (an image-backed tier writes a sentinel when there is no
    // real config), so a missing source here means the guard deliberately left
    // it out, not that we should mount something else.
    for rel in crate::container::AGENT_CONFIG_RELS {
        // Resolved through the shared guard: `$WORK` is repo-supplied, so a
        // symlink here (or at any directory above it) would otherwise mount an
        // arbitrary host path into the guest.
        if let Some(source) = crate::container::agent_config_mount_source(work, rel) {
            a.push("--mount-file".into());
            a.push(format!("{}:{WORK_MOUNT}/{rel}:ro", source.display()));
        }
    }

    // In-box git plumbing: each host path mounted at its IDENTICAL guest path,
    // because the worktree's `gitdir`/`commondir` pointer files contain
    // host-absolute paths and must resolve unchanged inside the box.
    for b in &policy.box_git {
        let flag = if b.host.is_dir() { "--mount-dir" } else { "--mount-file" };
        a.push(flag.into());
        a.push(format!(
            "{p}:{p}:{mode}",
            p = b.host.display(),
            mode = if b.rw { "rw" } else { "ro" },
        ));
    }

    // Managed-settings injection: the box's own root cannot rewrite a read-only
    // mount, and Claude will not let a session disable a *managed* hook from
    // user config — so in-box observation cannot be silenced from inside.
    // microsandbox's guest init creates the parent directory and the bind
    // target, so the path need not exist in the image.
    if let Some(ms) = managed_settings {
        a.push("--mount-file".into());
        a.push(format!(
            "{}:{}:ro",
            ms.display(),
            crate::sandbox_policy::CLAUDE_MANAGED_SETTINGS_PATH
        ));
    }

    // Capture spool for in-box `h5i capture run`. Everything the box writes here
    // is untrusted; the host ingest caps and sanitizes it.
    if let Some(spool) = &policy.env_capture_spool {
        a.push("--mount-dir".into());
        a.push(format!("{}:{SPOOL_MOUNT}:rw", spool.display()));
    }
    // Inbound mailbox, read-only: the box receives cross-agent messages without
    // any write access to the shared coordination store.
    if let Some(inbox) = &policy.env_inbox {
        a.push("--mount-dir".into());
        a.push(format!("{}:{INBOX_MOUNT}:ro", inbox.display()));
    }
    // Per-env private paths: a distinct backing inode per env, mounted over the
    // workspace path inside the box so concurrent envs of one repo don't share
    // build-cache locks.
    for b in &policy.private_binds {
        a.push("--mount-dir".into());
        a.push(format!(
            "{}:{WORK_MOUNT}/{}:rw",
            b.backing.display(),
            b.rel.trim_matches('/')
        ));
    }
}

/// Push the network flags. Shared for the same reason as the mount set: the
/// allowlist is the whole reason this tier exists, and it must mean exactly the
/// same thing whether the guest is created for one command or for a box.
fn push_net(a: &mut Vec<String>, net: &NetPlan) {
    match net {
        NetPlan::None => a.push("--no-net".into()),
        NetPlan::Host => {
            a.push("--net".into());
            a.push("public".into());
        }
        NetPlan::Allow(rules) => {
            a.push("--net-default-egress".into());
            a.push("deny".into());
            a.push("--net-default-ingress".into());
            a.push("deny".into());
            for rule in rules {
                a.push("--net-rule".into());
                a.push(rule.clone());
            }
        }
    }
}

/// Build the `msb run` argv for `plan` under `policy`. Pure — no process is
/// spawned and no file is written, so the security-critical flag set is
/// unit-tested directly.
///
/// Every host path reaching a `SOURCE:DEST` spec has already been checked for
/// `:` by [`run`]/[`run_interactive`]; this function assumes that and emits the
/// mounts unconditionally, because silently dropping one would weaken the box
/// exactly where the caller believes it was hardened.
pub fn build_run_argv(rt: &Runtime, policy: &ResolvedPolicy, work: &Path, plan: &RunPlan) -> Vec<String> {
    let p = &policy.profile;
    let mut a: Vec<String> = vec![
        rt.bin.clone(),
        "run".into(),
        "--quiet".into(),
        "--name".into(),
        plan.name.into(),
        // A stale sandbox of the same name is this process's own leftover
        // (the name carries our pid); replace it rather than fail the run.
        "--replace".into(),
        // The image must already be in msb's local cache. A run is not the
        // place to discover the network is down, and an implicit pull would
        // make the box's contents depend on when it happened to boot.
        "--pull".into(),
        "never".into(),
    ];

    push_mount_set(&mut a, policy, work, plan.managed_settings);

    // Resource limits. Memory is the VM's, so it is a hard ceiling rather than
    // a cgroup the guest can pressure its way around.
    if let Some(bytes) = p.mem_bytes {
        a.push("--memory".into());
        a.push(format!("{}M", (bytes / (1024 * 1024)).max(1)));
    }
    if let Some(n) = p.max_procs {
        a.push("--rlimit".into());
        a.push(format!("nproc={n}"));
    }
    if let Some(secs) = p.cpu_secs {
        a.push("--rlimit".into());
        a.push(format!("cpu={secs}"));
    }
    // Wall clock, captured runs only — an interactive session is bounded by the
    // operator, not a timer (same rule as every other tier).
    if plan.tty.is_none() {
        a.push("--timeout".into());
        a.push(format!("{}s", p.wall().as_secs()));
    }

    // Network. The allowlist is the whole reason this tier exists: default-deny
    // in both directions, then exactly the rules the policy asked for.
    push_net(&mut a, plan.net);

    // The env preload. Its *contents* (the allowlist values and every brokered
    // secret) reach the runtime over a config fd; only this path is in argv.
    a.push("--script-path".into());
    a.push(format!("{PRELOAD_SCRIPT_NAME}:{}", plan.preload.display()));

    match plan.tty {
        Some(true) => a.push("--tty".into()),
        // A piped or CI invocation must not ask for a pseudo-TTY, and a captured
        // run never wants one.
        Some(false) | None => a.push("--no-tty".into()),
    }

    a.push(plan.image.to_string());
    a.push("--".into());
    a.push(format!("{GUEST_SCRIPTS_DIR}/{PRELOAD_SCRIPT_NAME}"));
    a.extend(plan.argv.iter().cloned());
    a
}

// ─── warm guests: one per box, reused across its commands ───────────────────

/// Everything [`build_create_argv`] needs beyond the policy. The guest created
/// from this outlives the command that caused it, so every field here is a
/// property of the *box* rather than of one run.
pub struct CreatePlan<'a> {
    /// Resolved base image (`container.image` in the profile).
    pub image: &'a str,
    /// Guest name — see [`guest_name`], which ties it to the create argv.
    pub name: &'a str,
    /// Networking for this box.
    pub net: &'a NetPlan,
    /// Host directory staged per-run env scripts land in, mounted read-write at
    /// [`RUN_MOUNT`]. This is the warm path's replacement for `--script-path`.
    pub run_stage: &'a Path,
    /// Host directory a background service's log is written into, mounted
    /// read-write at [`SERVICES_MOUNT`]. Logs only — see that constant.
    pub service_logs: &'a Path,
    /// Managed-settings.json to mount read-only, as on the one-shot path.
    ///
    /// A create-time mount cannot vary per session without splitting the box's
    /// guest in two, so the warm paths pass `None` and say so where they do.
    pub managed_settings: Option<&'a Path>,
    /// Idle bound, e.g. `30m`. `None` leaves the guest up indefinitely.
    pub idle_timeout: Option<&'a str>,
}

/// Build the `msb create` argv for a box's warm guest. Pure, like
/// [`build_run_argv`], and for the same reason: this decides what the box may
/// touch for its whole life rather than for one command, so it is the more
/// security-critical of the two and is unit-tested directly.
///
/// What is deliberately *not* here: the command, the TTY choice, the wall
/// clock, and the per-run rlimits. Those are per-command and belong to
/// [`build_exec_argv`]. Memory is here because it sizes the VM itself.
pub fn build_create_argv(
    rt: &Runtime,
    policy: &ResolvedPolicy,
    work: &Path,
    plan: &CreatePlan,
) -> Vec<String> {
    let p = &policy.profile;
    let mut a: Vec<String> = vec![
        rt.bin.clone(),
        "create".into(),
        "--quiet".into(),
        "--name".into(),
        plan.name.into(),
        // The name is a hash of this very argv, so a same-named guest is one we
        // created under an identical configuration. Replacing it is the safe
        // resolution of a half-created leftover.
        "--replace".into(),
        "--pull".into(),
        "never".into(),
    ];

    push_mount_set(&mut a, policy, work, plan.managed_settings);

    // The per-run staging directory: small, host-owned, and the only writable
    // surface the warm path adds over the one-shot one.
    a.push("--mount-dir".into());
    a.push(format!("{}:{RUN_MOUNT}:rw", plan.run_stage.display()));

    // Service logs. Mounted unconditionally, even for a box that declares no
    // service, and that is deliberate: this argv *is* the guest's identity, so
    // a mount that appeared only when a service started would give the box a
    // second guest and reap the first — killing whatever was already running
    // in it. Every entry point must build the same argv or none of them share
    // a guest.
    a.push("--mount-dir".into());
    a.push(format!("{}:{SERVICES_MOUNT}:rw", plan.service_logs.display()));

    // The VM's own memory. A hard ceiling, not a cgroup the guest can pressure
    // its way around — and paid once per box here rather than once per command.
    if let Some(bytes) = p.mem_bytes {
        a.push("--memory".into());
        a.push(format!("{}M", (bytes / (1024 * 1024)).max(1)));
    }

    push_net(&mut a, plan.net);

    // A guest with no bound outlives the work that wanted it. `msb` has no
    // default of its own, so not setting this leaks a VM per box.
    if let Some(idle) = plan.idle_timeout {
        a.push("--idle-timeout".into());
        a.push(idle.to_string());
    }

    a.push(plan.image.to_string());
    a
}

/// Everything [`build_exec_argv`] needs. All per-command: the same warm guest
/// serves many of these.
pub struct ExecPlan<'a> {
    /// The warm guest to run in.
    pub name: &'a str,
    /// The command to run inside the guest.
    pub argv: &'a [String],
    /// Guest-visible path of this run's env script, under [`RUN_MOUNT`].
    pub env_script: &'a str,
    /// `None` → captured run (wall-clock enforced). `Some(tty)` → interactive
    /// session, allocating a pseudo-TTY when the caller has one.
    pub tty: Option<bool>,
    /// Apply the profile's per-command bounds (`--timeout`, `--rlimit`).
    ///
    /// False for a service launcher. rlimits are inherited across `setsid` and
    /// `exec`, so a CPU bound meant for one command would follow the detached
    /// service and `SIGXCPU` it once it had accumulated that much CPU — a dev
    /// server dying hours later with nothing to explain it. The kernel tiers
    /// deliberately give services no wall or CPU bound either; a service is
    /// bounded by the operator, not by a per-command timer.
    pub bounded: bool,
}

/// Build the `msb exec` argv for one command in an already-running guest.
///
/// The command is run *through* this run's generated env script, exactly as the
/// one-shot path runs it through the `--script-path` preload: the script
/// `export`s each value and then `exec "$@"`, so argv, stdin, the TTY and the
/// exit code pass through untouched and no value ever appears in a host
/// command line.
pub fn build_exec_argv(rt: &Runtime, policy: &ResolvedPolicy, plan: &ExecPlan) -> Vec<String> {
    let p = &policy.profile;
    let mut a: Vec<String> = vec![rt.bin.clone(), "exec".into(), "--quiet".into()];

    // The workspace is the working directory for every command, as on the
    // one-shot path. Set per exec rather than relying on the guest's default.
    a.push("--workdir".into());
    a.push(WORK_MOUNT.into());

    // Per-process limits are per *command*: two commands in one warm guest each
    // get the profile's ceiling, which is what they would have got from two
    // one-shot runs. Skipped for a service launcher, whose limits would be
    // inherited by the service it detaches — see [`ExecPlan::bounded`].
    if plan.bounded {
        if let Some(n) = p.max_procs {
            a.push("--rlimit".into());
            a.push(format!("nproc={n}"));
        }
        if let Some(secs) = p.cpu_secs {
            a.push("--rlimit".into());
            a.push(format!("cpu={secs}"));
        }
        // Wall clock, captured runs only — an interactive session is bounded by
        // the operator, not a timer (same rule as every other tier).
        if plan.tty.is_none() {
            a.push("--timeout".into());
            a.push(format!("{}s", p.wall().as_secs()));
        }
    }

    match plan.tty {
        Some(true) => a.push("--tty".into()),
        Some(false) | None => a.push("--no-tty".into()),
    }

    a.push(plan.name.to_string());
    a.push("--".into());
    // `/bin/sh <script> <argv…>` rather than executing the script directly: the
    // staging mount is host-owned and need not carry the execute bit.
    a.push("/bin/sh".into());
    // `SHELL_DIRECT` means the caller's argv is already shell text that sources
    // whatever environment it needs — the service launcher, which must delete
    // the credential file before it detaches.
    if plan.env_script != SHELL_DIRECT {
        a.push(plan.env_script.to_string());
    }
    a.extend(plan.argv.iter().cloned());
    a
}

/// Reduce `raw` to something `msb` accepts as part of a sandbox name, for the
/// human half of [`guest_name`].
///
/// `msb` requires a name to start alphanumeric and rejects `/`, which every box
/// id contains (`env/human/slug`). Anything outside `[a-z0-9]` becomes `-`,
/// runs collapse, and the result is trimmed and capped — this half only has to
/// be recognisable in `msb list`, since the digest that follows is what makes
/// the name *correct*.
pub fn sanitize_label(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // leading dashes are dropped
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= GUEST_LABEL_MAX {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// The name of the warm guest for this box under this configuration.
///
/// `h5i-<label>-<12 hex>`, where the hex is a SHA-256 over the **create argv
/// itself**. That choice is the whole reuse-safety argument, so it is worth
/// stating plainly: the guest's mounts, image, memory and egress rules are
/// fixed when it is created, and the create argv is exactly the list of those
/// things. Hashing it means a box whose profile, allowlist, image, or mount set
/// has changed resolves to a *different name*, so it gets a new guest and can
/// never be served a stale one still enforcing the old policy. The rule is
/// structural rather than a comparison somebody has to remember to write.
///
/// It is deliberately not the pinned policy digest: that digest excludes the
/// runtime-only mounts (`box_git`, `private_binds`, the spool, the inbox) by
/// design, and those *do* change what a guest can reach.
pub fn guest_name(work: &Path, create_argv: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for arg in create_argv {
        hasher.update(arg.as_bytes());
        hasher.update([0u8]); // unambiguous separator: no arg can forge a boundary
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();

    // `<env_dir>/work` → `<slug>`, with the agent above it, for a name a human
    // can match to a box in `msb list`.
    let label = work
        .parent()
        .map(|env_dir| {
            let slug = env_dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let agent = env_dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if agent.is_empty() { slug } else { format!("{agent}-{slug}") }
        })
        .map(|raw| sanitize_label(&raw))
        .unwrap_or_default();

    if label.is_empty() {
        format!("h5i-{hex}")
    } else {
        format!("h5i-{label}-{hex}")
    }
}

/// What `msb` says about a guest we might reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestState {
    /// No such guest — create it.
    Absent,
    /// It exists but is not running. **Start it explicitly**: an `msb exec`
    /// into a stopped guest boots it, runs, and stops it again, so it costs a
    /// full boot *and* leaves the next command to pay the same. See
    /// `docs/benchmarks/microvm-boot.md`.
    Stopped,
    /// Running — exec straight into it, which is the ~9 ms path this whole
    /// milestone exists to reach.
    Running,
    /// The runtime could not be asked, or did not answer in time.
    ///
    /// Emphatically **not** [`GuestState::Absent`]. Treating "I do not know"
    /// as "there is none" leads to `msb create --replace`, which destroys a
    /// guest that may be running a dev server — so one flaky `msb list` would
    /// silently kill a service while the run that caused it carried on. The
    /// caller fails instead.
    Unknown,
}

/// Read a guest's state out of `msb list --format json`. Pure, so the state
/// machine's input can be tested without a runtime.
///
/// Anything present but not `Running` is reported [`GuestState::Stopped`] so
/// the caller starts it: for a guest we created, the reachable states are
/// running and stopped, and treating an unexpected one as "needs starting"
/// fails towards a working box with a clear error rather than towards a silent
/// exec into something that cannot serve it.
pub fn parse_guest_state(json: &str, name: &str) -> GuestState {
    // Output we cannot read is not an empty list. `msb list` printing a banner,
    // a warning, or a future `{"sandboxes": […]}` wrapper would otherwise mean
    // "no guest" — and `ensure_guest` answers that with `create --replace`,
    // destroying a live guest and every service in it, on every command.
    // Only a well-formed array that does not mention this name is `Absent`.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return GuestState::Unknown;
    };
    let Some(rows) = value.as_array() else {
        return GuestState::Unknown;
    };
    for row in rows {
        if row.get("name").and_then(|n| n.as_str()) != Some(name) {
            continue;
        }
        let status = row.get("status").and_then(|s| s.as_str()).unwrap_or_default();
        return if status.eq_ignore_ascii_case("running") {
            GuestState::Running
        } else {
            GuestState::Stopped
        };
    }
    GuestState::Absent
}

// ─── execution ──────────────────────────────────────────────────────────────

static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Why the microvm tier is unavailable here, naming the *specific* missing
/// half. "Install microsandbox", "update it", and "enable nested virtualization"
/// are three different afternoons; a tier that refuses without saying which one
/// is a tier nobody can adopt. Empty-ish callers get the generic line only when
/// every check unexpectedly passes.
pub fn unavailable_detail() -> String {
    match msb_version() {
        None => "microsandbox's `msb` is not on PATH — install it from \
                 https://install.microsandbox.dev"
            .to_string(),
        Some(v) if v < MIN_MSB_VERSION => format!(
            "msb {}.{} is older than the {}.{} this adapter targets — run `msb self update`",
            v.0, v.1, MIN_MSB_VERSION.0, MIN_MSB_VERSION.1
        ),
        Some(_) => virtualization_detail()
            .unwrap_or_else(|| "the microVM runtime is unavailable".to_string()),
    }
}

fn runtime_or_refuse() -> Result<Runtime, H5iError> {
    probe().ok_or_else(|| {
        H5iError::Metadata(format!(
            "isolation claim 'microvm' cannot be satisfied on this host — refusing (h5i never \
             silently downgrades):\n  - {}\nRe-request a weaker claim (--isolation \
             container|supervised|process), or run on a host with virtualization enabled.",
            unavailable_detail()
        ))
    })
}

fn image_or_refuse(p: &Profile) -> Result<String, H5iError> {
    p.image.clone().ok_or_else(|| {
        H5iError::Metadata(format!(
            "profile '{}' uses isolation=microvm but sets no image — pass `--image` at env \
             create, or set `container.image = \"…\"` in the profile / a repo-level \
             `[container] image` in .h5i/env.toml (the microvm tier boots the same OCI images)",
            p.name
        ))
    })
}

/// Resolve the network plan for a run: the enforced rule set is the digested
/// profile allowlist plus the host-side user extras (`h5i box allow`), and a
/// deny-all profile ignores the extras (it can never be widened from outside the
/// digested policy).
fn net_plan(policy: &ResolvedPolicy) -> Result<NetPlan, H5iError> {
    let rules = crate::container::effective_egress(
        &policy.profile.net_egress,
        &policy.user_egress_allow,
    );
    if !rules.is_empty() {
        return Ok(NetPlan::Allow(egress_rule_tokens(&rules)?));
    }
    Ok(if policy.profile.net_mode == NetMode::Host {
        NetPlan::Host
    } else {
        NetPlan::None
    })
}

/// Check every host path that will become part of a `SOURCE:DEST` spec.
fn check_mount_paths(policy: &ResolvedPolicy, work: &Path) -> Result<(), H5iError> {
    check_spec_path(work, "workspace")?;
    for b in &policy.ro_binds {
        check_spec_path(&b.backing, "cache mount")?;
    }
    if let Some(b) = &policy.cache_write {
        check_spec_path(&b.backing, "writable cache mount")?;
    }
    for b in &policy.box_git {
        check_spec_path(&b.host, "in-box git mount")?;
    }
    for b in &policy.private_binds {
        check_spec_path(&b.backing, "private-path mount")?;
    }
    if let Some(spool) = &policy.env_capture_spool {
        check_spec_path(spool, "capture spool mount")?;
    }
    if let Some(inbox) = &policy.env_inbox {
        check_spec_path(inbox, "inbox mount")?;
    }
    Ok(())
}

/// A named `msb` sandbox whose persisted state is removed when the run ends —
/// on success, on error, and on a wall-clock kill alike.
///
/// The name is what makes cleanup possible at all (it is how a hung run gets
/// force-stopped), but `msb` keeps *named* sandboxes around to be inspected,
/// while an unnamed one is ephemeral. h5i wants both properties, so it names the
/// sandbox and takes responsibility for reaping it. Best-effort by design: a
/// failed cleanup must never turn a successful run into an error, and the name
/// carries our pid so nothing else can be reaped by mistake.
struct SandboxGuard {
    bin: String,
    name: String,
}

impl SandboxGuard {
    fn new(bin: &str) -> Self {
        let g = SandboxGuard {
            bin: bin.to_string(),
            name: format!(
                "h5i-{}-{}",
                std::process::id(),
                RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        };
        // Drop alone cannot cover SIGKILL, SIGTERM, or a panic=abort build, and
        // a *named* msb sandbox survives us — unlike the container tier, which
        // has `--rm` as a backstop. Leave a marker so a later run can reap what
        // an abnormal exit left behind.
        if let Some(m) = marker_path(&g.name) {
            if let Some(d) = m.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&m, b"");
        }
        g
    }

    fn remove(&self) {
        remove_named(&self.bin, &self.name);
        if let Some(m) = marker_path(&self.name) {
            let _ = std::fs::remove_file(m);
        }
    }
}

fn remove_named(bin: &str, name: &str) {
    let _ = std::process::Command::new(bin)
        .args(["remove", "--force", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The directory holding one marker per live named sandbox, or `None` when this
/// process has no directory it can trust to hold them.
///
/// A function of its own rather than `marker_path("").parent()`, which is what
/// this was: joining an empty component yields a trailing separator, and
/// `parent()` then walks up *past* the marker directory to the temp directory
/// itself. The sweep consequently scanned `/tmp` for names that only ever exist
/// one level down, so it matched nothing and reaped nothing — silently, since
/// every step of it is best-effort.
///
/// **Per user, not per host.** These markers decide which VMs get destroyed, so
/// a directory shared between logins is the wrong place for them. On a shared
/// Linux box the old `/tmp/h5i-msb-live` belonged to whoever ran the tier
/// first: everyone else's marker writes then failed silently (so their guests
/// were never reaped), and worse, their sweeps *read the first user's markers*
/// and saw `exists() == false` for a workspace under a home directory they
/// cannot traverse — concluding that a live box was gone and removing its VM.
/// `$XDG_RUNTIME_DIR` is per-user and `0700` by definition; without one, the
/// uid keeps the fallback distinct and [`ensure_private_dir`] refuses to use a
/// path somebody else got to first.
fn marker_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(rt) => PathBuf::from(rt).join("h5i"),
        // No runtime dir: macOS always, plus cron without a session and some
        // containers. macOS's `$TMPDIR` is already a per-user `0700` directory;
        // elsewhere this lands in the shared `/tmp`, where the uid separates
        // users and the ownership check below is what actually protects us.
        None => std::env::temp_dir().join(format!("h5i-{}", current_uid())),
    };
    let dir = base.join("msb-live");
    ensure_private_dir(&dir).then_some(dir)
}

/// This process's uid, or `0` where there is no such concept. Only ever used to
/// name a directory and to compare against one's owner.
fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Create `dir` mode `0700` if absent, then confirm it is a real directory this
/// user owns and nobody else can write to.
///
/// Returning `false` costs a sweep — guests leak until the box is removed —
/// which is the safe direction: acting on another user's markers is how a live
/// VM gets destroyed by mistake. The check is `symlink_metadata`, so a symlink
/// planted at the path is rejected rather than followed.
fn ensure_private_dir(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !dir.exists() {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
        }
        let Ok(md) = std::fs::symlink_metadata(dir) else {
            return false;
        };
        // `symlink_metadata` does not traverse, so a symlink reports as a
        // symlink and fails this test rather than being followed to its target.
        if !md.is_dir() || md.uid() != current_uid() || md.permissions().mode() & 0o022 != 0 {
            warn_unusable_marker_dir(dir);
            return false;
        }
        true
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).is_ok()
    }
}

/// Say so once. A marker directory we cannot use means this box's VM will
/// outlive its policy — the one consequence of the sweep not running that an
/// operator would want to know about — and saying it per run would be noise.
///
/// Unix only: the only ownership/permission check that can reject a directory
/// lives in [`ensure_private_dir`]'s `cfg(unix)` arm, so on Windows this has no
/// caller and `-D dead-code` fails the build.
#[cfg(unix)]
fn warn_unusable_marker_dir(dir: &Path) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "h5i: {} is not a private directory owned by this user — microVM guests will not \
             be reaped until it is removed or fixed",
            dir.display()
        );
    });
}

/// Where the marker for a live named sandbox lives.
fn marker_path(name: &str) -> Option<PathBuf> {
    Some(marker_dir()?.join(name))
}

/// Does `name` have the shape h5i gives a sandbox it created?
///
/// The gate between a directory listing and `msb remove --force <name>`. Both
/// name forms this module produces are `h5i-` followed by lowercase
/// alphanumerics and dashes ([`SandboxGuard::new`] and [`guest_name`]), so
/// anything else in the marker directory is not ours to act on — and a name
/// that could be read as a flag can never reach the runtime's argv.
fn is_h5i_sandbox_name(name: &str) -> bool {
    name.strip_prefix("h5i-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// Reap named sandboxes an earlier h5i left behind.
///
/// Two kinds of marker, because there are two kinds of guest:
///
/// - **One-shot** (`h5i-<pid>-<seq>`, empty marker). Keyed on the pid in the
///   name: a marker whose process is gone can only be a leftover, because a
///   live run holds its own marker until Drop removes it.
/// - **Warm** (`h5i-<label>-<digest>`, marker holds the workspace path). These
///   outlive their process deliberately, so a pid says nothing. The box's
///   existence is the signal: once its workspace is gone, so is any reason to
///   keep a VM configured for it.
///
/// Best-effort throughout — a failed sweep must never turn a good run into an
/// error.
pub fn reap_orphaned_sandboxes(bin: &str) {
    for (name, owner) in live_markers() {
        let reap = match &owner {
            // Warm guest: reap once the box it belongs to is gone.
            Some(work) => box_is_gone(Path::new(work)),
            // One-shot guest: reap once the process that owned it is gone.
            //
            // Gated on the name matching `h5i-<digits>-<digits>` exactly, not
            // merely on the marker body being unreadable. A *warm* marker whose
            // body failed to read falls through to here, and a box label whose
            // first segment happens to be digits (an agent directory named
            // numerically → `h5i-2-web-abc123`) would parse as pid 2 — reaping
            // a live box's guest, and every service in it, if pid 2 is gone.
            None => match one_shot_pid(&name) {
                Some(pid) => pid != std::process::id() as i32 && !pid_alive(pid),
                None => false,
            },
        };
        if !reap {
            continue;
        }
        remove_named(bin, &name);
        if let Some(m) = marker_path(&name) {
            let _ = std::fs::remove_file(m);
        }
    }
}

/// Has this box's workspace really gone, or can we merely not see it?
///
/// `Path::exists()` answers `false` to both, and the difference decides whether
/// a VM is destroyed. A workspace under a home directory this process cannot
/// traverse, an unmounted network path, or a transient I/O error all report
/// "does not exist" through `exists()` while the box is very much alive. Only a
/// definite `NotFound` counts as gone; every other error leaves the guest
/// alone, so an unreadable path costs a leaked VM rather than a destroyed one.
fn box_is_gone(work: &Path) -> bool {
    match std::fs::symlink_metadata(work) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The pid in a one-shot guest's name, or `None` if this is not one.
///
/// `h5i-<pid>-<seq>` and nothing else: both segments must be digits and there
/// must be exactly two, so no warm guest's label can be read as a pid.
fn one_shot_pid(name: &str) -> Option<i32> {
    let rest = name.strip_prefix("h5i-")?;
    let mut parts = rest.split('-');
    let pid = parts.next()?;
    let seq = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !seq.bytes().all(|b| b.is_ascii_digit()) || seq.is_empty() {
        return None;
    }
    pid.parse().ok()
}

/// Is `pid` still around? `kill(pid, 0)` reports EPERM for a live process we do
/// not own, which still means "do not reap".
fn pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        if pid <= 0 {
            return true; // never interpret a bad pid as reapable
        }
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        self.remove();
    }
}

// ─── warm guest lifecycle ───────────────────────────────────────────────────

/// Ask `msb` whether `name` exists and is running. One `list` call — measured at
/// ~7.5 ms, the same order as the exec it guards, which is why the caller makes
/// exactly one of these per command rather than one per decision.
fn guest_state(bin: &str, name: &str) -> GuestState {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["list", "--quiet", "--format", "json"]);
    match run_bounded(cmd, GUEST_QUERY_TIMEOUT) {
        Some(o) if o.status.success() => {
            parse_guest_state(&String::from_utf8_lossy(&o.stdout), name)
        }
        // A runtime that cannot answer, exits non-zero, or overruns the
        // deadline has told us nothing. Reporting `Absent` here would send the
        // caller into `create --replace` and destroy a live guest — and adding
        // the deadline made that *more* reachable, not less.
        _ => GuestState::Unknown,
    }
}

/// Bring the box's guest to `Running`, creating or starting it as needed.
///
/// The `Stopped` branch is the one that matters and the one measurement
/// corrected: `msb exec` will auto-start a stopped guest, but it boots it, runs
/// the command, and stops it again — ~236 ms, and the next command pays it too.
/// An explicit `start` costs ~143 ms once and leaves the guest running for
/// every command after.
fn ensure_guest(
    rt: &Runtime,
    policy: &ResolvedPolicy,
    work: &Path,
    create_argv: &[String],
    name: &str,
) -> Result<(), H5iError> {
    match guest_state(&rt.bin, name) {
        GuestState::Running => {}
        GuestState::Unknown => {
            return Err(H5iError::Metadata(format!(
                "could not ask the microVM runtime whether this box's guest '{name}' exists \
                 (`{} list` failed or timed out) — refusing to continue, because creating a \
                 guest on a maybe would replace one that may be running this box's services",
                rt.bin
            )));
        }
        GuestState::Stopped => {
            let mut cmd = std::process::Command::new(&rt.bin);
            cmd.args(["start", name]);
            let out = run_bounded(cmd, GUEST_LIFECYCLE_TIMEOUT).ok_or_else(|| {
                H5iError::Metadata(format!(
                    "starting this box's microVM guest '{name}' did not finish in {}s",
                    GUEST_LIFECYCLE_TIMEOUT.as_secs()
                ))
            })?;
            if !out.status.success() {
                return Err(H5iError::Metadata(format!(
                    "could not start this box's microVM guest '{name}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        GuestState::Absent => {
            // A configuration change gives the box a new guest name, which
            // leaves the previous guest behind holding memory and a disk. Reap
            // it now, while we still know which box it belonged to.
            reap_stale_siblings(&rt.bin, work, name);
            let _ = policy; // policy shaped `create_argv`; kept for symmetry
            let mut cmd = std::process::Command::new(&create_argv[0]);
            cmd.args(&create_argv[1..]);
            let out = run_bounded(cmd, GUEST_LIFECYCLE_TIMEOUT).ok_or_else(|| {
                H5iError::Metadata(format!(
                    "creating this box's microVM guest did not finish in {}s",
                    GUEST_LIFECYCLE_TIMEOUT.as_secs()
                ))
            })?;
            if !out.status.success() {
                return Err(H5iError::Metadata(format!(
                    "could not create this box's microVM guest: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
    }
    write_marker(name, work);
    Ok(())
}

/// Record a live warm guest, and which box it belongs to.
///
/// The one-shot guard writes an empty marker and is reaped by the pid in its
/// name. A warm guest deliberately outlives the process that made it, so its
/// marker carries the **workspace path** instead: the box's existence, not a
/// pid, is what says whether the guest is still wanted.
fn write_marker(name: &str, work: &Path) {
    if let Some(m) = marker_path(name) {
        if let Some(d) = m.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&m, work.display().to_string().as_bytes());
    }
}

/// Remove any other warm guest belonging to this box.
///
/// Called when a box resolves to a new guest name — which happens exactly when
/// its image, mounts, memory or egress rules changed, since the name is a hash
/// of those. The old guest is not merely wasteful: it is a VM still configured
/// under the policy the box no longer has.
fn reap_stale_siblings(bin: &str, work: &Path, keep: &str) {
    for (name, owner) in live_markers() {
        if name == keep || owner.as_deref() != Some(work.display().to_string().as_str()) {
            continue;
        }
        remove_named(bin, &name);
        if let Some(m) = marker_path(&name) {
            let _ = std::fs::remove_file(m);
        }
    }
}

/// Every marker currently on disk, as `(sandbox name, owning workspace)`.
/// `None` for the owner means a one-shot marker (empty file), which the pid
/// rule in [`reap_orphaned_sandboxes`] handles instead.
fn live_markers() -> Vec<(String, Option<String>)> {
    let Some(dir) = marker_dir() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // The one choke point between a directory listing and
            // `msb remove --force`. Anything not shaped like a name we produce
            // is not ours to reap.
            if !is_h5i_sandbox_name(&name) {
                return None;
            }
            let owner = std::fs::read_to_string(e.path())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Some((name, owner))
        })
        .collect()
}

/// Remove the warm guest belonging to `work`, if any. Called when a box is torn
/// down, so its VM goes with it rather than waiting for a later sweep.
/// Best-effort: a box is still removed even if its guest cannot be.
pub fn remove_guest_for_workspace(work: &Path) {
    let Some(rt) = probe() else {
        return;
    };
    for (name, owner) in live_markers() {
        if owner.as_deref() != Some(work.display().to_string().as_str()) {
            continue;
        }
        remove_named(&rt.bin, &name);
        if let Some(m) = marker_path(&name) {
            let _ = std::fs::remove_file(m);
        }
    }
}

/// Write the managed-settings.json (carrying the unkillable `wrap-bash`
/// observation hook) under the env dir, to be mounted read-only into the guest.
/// Best-effort: `None` (injection skipped, session otherwise unaffected) on any
/// I/O failure or a path the spec syntax cannot carry — an unobserved session is
/// still a correctly *confined* session.
fn prepare_managed_settings(work: &Path, content: &str) -> Option<PathBuf> {
    let dir = work.parent()?.join("managed");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("managed-settings.json");
    std::fs::write(&path, content).ok()?;
    check_spec_path(&path, "managed settings").ok()?;
    Some(path)
}

/// Run `argv` inside a microVM under `policy`, capturing stdout/stderr and
/// enforcing the profile's wall clock.
///
/// [`ExecOutcome::egress`] is always `None`: the allowlist is enforced by the
/// VM's network stack, which drops packets rather than reporting them. See the
/// module docs — stronger enforcement, and no tally to pretend otherwise with.
pub fn run(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    if reuse_available(policy) {
        return run_warm(policy, work, argv, injected_env);
    }
    run_one_shot(policy, work, argv, injected_env)
}

/// May this run reuse a warm guest?
///
/// Reuse is the default (roadmap-history.md 9., "a box is the trust domain, not a
/// command"), with two exclusions:
///
/// - [`NO_REUSE_ENV`], the operator's escape hatch and the way to get a
///   pristine guest per command.
/// - A run carrying `cache_write`. It is the one run whose mount set differs
///   from the box's — `h5i box cache refresh` and nothing else — and letting it
///   define the box's guest would either give every later command a writable
///   cache mount it should not have, or make the two configurations evict each
///   other's guest on every alternation. A one-shot guest is both correct and
///   rare here.
fn reuse_available(policy: &ResolvedPolicy) -> bool {
    if std::env::var_os(NO_REUSE_ENV).is_some_and(|v| v != "0") {
        return false;
    }
    policy.cache_write.is_none()
}

/// The create plan every warm entry point uses. A function rather than an
/// inline literal so there is exactly one description of a box's guest.
fn warm_create_plan<'a>(
    image: &'a str,
    name: &'a str,
    net: &'a NetPlan,
    run_stage: &'a Path,
    service_logs: &'a Path,
    idle_timeout: Option<&'a str>,
) -> CreatePlan<'a> {
    CreatePlan {
        image,
        name,
        net,
        run_stage,
        service_logs,
        managed_settings: None,
        idle_timeout,
    }
}

/// Held while a box's guest is created or started, so two h5i processes cannot
/// both conclude there is no guest and both `create --replace` it.
///
/// Blocking, and best-effort: if the lock file cannot be made, the work still
/// happens — an unserialized create is worse than a refused command only in a
/// race, whereas refusing outright is worse always.
struct GuestLock {
    #[cfg(unix)]
    _file: Option<std::fs::File>,
}

impl GuestLock {
    fn acquire(work: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = microvm_dir(work).ok().and_then(|dir| {
                std::fs::create_dir_all(&dir).ok()?;
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(dir.join("guest.lock"))
                    .ok()
            });
            if let Some(f) = &file {
                unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            }
            GuestLock { _file: file }
        }
        #[cfg(not(unix))]
        {
            let _ = work;
            GuestLock {}
        }
    }
}

/// A box's warm guest, ready to exec into.
struct WarmGuest {
    rt: Runtime,
    /// The guest's `msb` name — a hash of the argv that created it.
    name: String,
    /// Host directory this run may stage material into, visible at
    /// [`RUN_MOUNT`].
    stage: PathBuf,
}

/// Resolve, create or start the box's guest, and hand back what an exec needs.
///
/// **The only place a create argv is built**, and that is the point rather than
/// tidiness: the argv *is* the guest's identity, so `box run`, `box shell` and
/// a service launch must produce a byte-identical one or they will each create
/// their own guest and reap the others' — taking any service running in them
/// with it. One construction site makes that impossible to get wrong.
fn ensure_warm_guest(policy: &ResolvedPolicy, work: &Path) -> Result<WarmGuest, H5iError> {
    let rt = runtime_or_refuse()?;
    let image = image_or_refuse(&policy.profile)?;
    check_mount_paths(policy, work)?;

    let net = net_plan(policy)?;
    let stage = run_stage_dir(work)?;
    std::fs::create_dir_all(&stage).map_err(|e| H5iError::with_path(e, &stage))?;
    check_spec_path(&stage, "per-run staging")?;
    sweep_stale_env_scripts(&stage);
    let logs = service_log_dir(work)?;
    std::fs::create_dir_all(&logs).map_err(|e| H5iError::with_path(e, &logs))?;
    check_spec_path(&logs, "service log")?;

    // Built twice: once with a placeholder to hash, once with the name that
    // hash produced. Only the `--name` value differs, so the digest still
    // describes this guest's configuration exactly.
    //
    // `managed_settings` is `None` here and not a parameter. It is a
    // create-time mount, so letting it vary per session would give one box two
    // guests that reap each other; the only caller passes `None` today anyway,
    // deliberately (see `env::shell`). Re-enabling it needs a design for warm
    // guests, not an argument here.
    // A box that runs services must not have its guest stopped for idleness:
    // `msb` measures idleness in commands, not in the traffic a dev server is
    // serving, so the bound would kill the very thing the box exists to run.
    // Such a guest is reclaimed by `box rm` and by the sweep instead.
    let idle = (!policy.hosts_services).then_some(GUEST_IDLE_TIMEOUT);
    let placeholder = warm_create_plan(&image, GUEST_NAME_PLACEHOLDER, &net, &stage, &logs, idle);
    let hashed = build_create_argv(&rt, policy, work, &placeholder);
    // A box that keeps getting a new guest is a box whose create argv is not
    // stable across entry points, and the argv is the only way to see which
    // element moved.
    if std::env::var_os("H5I_DEBUG_MICROVM_ARGV").is_some() {
        eprintln!("h5i microvm create argv:\n  {}", hashed.join("\n  "));
    }
    let name = guest_name(work, &hashed);
    let named = warm_create_plan(&image, &name, &net, &stage, &logs, idle);
    let create_argv = build_create_argv(&rt, policy, work, &named);

    // Serialize guest creation for this box, and only that.
    //
    // Two processes that both see no guest both issue `create --replace` under
    // the same name, and the loser's guest — with whatever was running in it —
    // is destroyed. This lock lives here rather than in the callers because the
    // race is here: `box run`, `box shell` and a service launch all pass
    // through, and the alternative (the box's writer lock) is held by a whole
    // interactive session and would refuse a service start for its duration.
    let _guest_lock = GuestLock::acquire(work);

    // Sweep leftovers from an h5i that died without running Drop, and guests
    // whose box has since been removed.
    reap_orphaned_sandboxes(&rt.bin);
    ensure_guest(&rt, policy, work, &create_argv, &name)?;
    Ok(WarmGuest { rt, name, stage })
}

/// The warm path: one guest per box, one `msb exec` per command.
fn run_warm(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let WarmGuest { rt, name, stage } = ensure_warm_guest(policy, work)?;

    // Per-run credentials, staged into the guest-visible directory rather than
    // passed on a command line. Dropped (and unlinked) when this run ends.
    let script = write_env_script(&stage, "env", &guest_env(policy, injected_env))?;
    let guest_script = guest_script_path(&script.path)?;

    let exec_argv = build_exec_argv(
        &rt,
        policy,
        &ExecPlan {
            name: &name,
            argv,
            env_script: &guest_script,
            tty: None,
            bounded: true,
        },
    );

    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(&exec_argv[0]);
    cmd.args(&exec_argv[1..]);
    let outcome = wait_exec(cmd, policy.profile.wall(), &exec_argv)?;
    Ok(ExecOutcome {
        wall_ms: started.elapsed().as_millis(),
        ..outcome
    })
}

// ─── background services ────────────────────────────────────────────────────

/// Start `argv` as a detached service inside the box's warm guest, returning
/// the **guest** pid of its session leader and the guest it runs in.
///
/// The returned pid is meaningless on this host — it names a process in the
/// guest's pid namespace, where a number equal to some host pid is a
/// coincidence, not a relationship. Callers must record which world it belongs
/// to and never hand it to `kill(2)`; [`service_alive`] and [`service_signal`]
/// are the only things that may interpret it.
///
/// The launcher shell sources this run's env script and **deletes it before
/// detaching**, so the credentials exist on disk for the length of one exec
/// rather than for the life of the service. `setsid` makes the service a
/// session leader, so signalling `-pid` later reaps its whole descendant tree —
/// the same property `spawn_background` gets from `setsid` + `killpg` on the
/// kernel tiers.
pub fn spawn_background(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    service: &str,
) -> Result<ServiceHandle, H5iError> {
    // A service *is* a process outliving the command that started it, so it has
    // nowhere to live without guest reuse. Refusing is the honest outcome:
    // starting one anyway would put it in a warm guest while every `box run`
    // got its own throwaway one, so the box could never reach its own service
    // and nothing would look wrong.
    if !reuse_available(policy) {
        return Err(H5iError::Metadata(format!(
            "services need a persistent microVM guest, and guest reuse is disabled here \
             ({NO_REUSE_ENV} is set) — a service started now would run in a guest that \
             `box run` never enters. Unset {NO_REUSE_ENV} to run services at this tier."
        )));
    }
    let WarmGuest { rt, name, stage } = ensure_warm_guest(policy, work)?;

    // Exports only: the launcher sources these into its own shell and then has
    // more to do, so `exec "$@"` would be wrong here.
    let script = write_env_script_with(&stage, "svc", &guest_env(policy, injected_env), env_exports)?;
    let guest_script = guest_script_path(&script.path)?;
    let guest_log = format!("{SERVICES_MOUNT}/{service}.log");

    // The command is the profile-pinned service definition, already shell text
    // by the time it reaches here (`sh -c <def.command>` on every tier), so it
    // is embedded as the argument to an inner `sh -c` exactly as the kernel
    // tiers embed it. Nothing from the guest reaches this string.
    let command = shell_join(argv);
    // No pidfile, and no fd gymnastics: `$!` is the service.
    //
    // `setsid` only forks when it is already a process-group leader, and a
    // background job in a non-interactive shell is not one — so it `setsid()`s
    // in place and `$!` names the session leader itself. That was verified on
    // this runtime rather than assumed, because an earlier reading of it was
    // wrong: a broken liveness check (exec'ing `kill`, which is a builtin) made
    // a perfectly good pid look dead and sent this down a detour through a
    // pidfile — which had to live in a mounted directory, where any process in
    // the box could win a race and choose the pid the host records.
    //
    // The session id is reported alongside and checked, so a shell that *does*
    // fork fails loudly here instead of silently recording a pid whose process
    // group is somebody else's.
    let launcher = format!(
        // Sourcing failing is fatal rather than skipped: a service that started
        // *without* its brokered credentials would look healthy and behave
        // wrongly, which is worse than not starting.
        //
        // `sed`/`cut` rather than a positional field: the `comm` field in
        // /proc/<pid>/stat may contain spaces and parentheses, so anything that
        // counts columns from the left is wrong for a process that chose an
        // awkward name.
        ". {script} || exit 97\n\
         rm -f {script}\n\
         cd {work} 2>/dev/null || cd /\n\
         setsid {command} >>{log} 2>&1 &\n\
         p=$!\n\
         printf '#h5i-pid %s\\n' \"$p\" >>{log}\n\
         printf 'boot %s\\n' \"$(cat /proc/sys/kernel/random/boot_id)\"\n\
         printf 'pid %s\\n' \"$p\"\n\
         printf 'sid %s\\n' \"$(sed -e 's/^.*) //' /proc/$p/stat 2>/dev/null | cut -d' ' -f4)\"\n",
        script = sh_quote(&guest_script),
        work = WORK_MOUNT,
        command = command,
        log = sh_quote(&guest_log),
    );

    let exec_argv = build_exec_argv(
        &rt,
        policy,
        &ExecPlan {
            name: &name,
            argv: &["-c".to_string(), launcher],
            // The launcher *is* the command here, so it is run through `sh`
            // directly rather than through the per-run env wrapper.
            env_script: SHELL_DIRECT,
            tty: None,
            bounded: false,
        },
    );
    // Bounded like every other call into the runtime. The launcher exits as
    // soon as it has the pid, so overrunning means the runtime is stuck — and
    // an unbounded wait here hangs `box service start` with no way out.
    let mut cmd = std::process::Command::new(&exec_argv[0]);
    cmd.args(&exec_argv[1..]);
    // Past this point the launcher may already have detached the service, so
    // every failure has to clean up after itself: the guest outlives the
    // command, and an unrecorded service in it is invisible to `service status`
    // and unreachable by `service stop`, with a retry starting a second copy.
    // The launcher writes its pid into the log for exactly this.
    // The pid comes out of a log the guest writes, so it is checked the same
    // way the success path below checks the one it was told: a pid that is not
    // its own session leader does not name this service's process group, and
    // signalling the group anyway is how a forged marker would aim `kill` at
    // something else in the guest.
    let reap_detached = || {
        let live = logged_service_pid(work, service)
            .filter(|p| guest_session_leader(&rt, &name, *p));
        if let Some(pid) = live {
            stop_group(&rt, &name, pid);
        }
    };
    let Some(out) = run_bounded(cmd, GUEST_LIFECYCLE_TIMEOUT) else {
        reap_detached();
        return Err(H5iError::Metadata(format!(
            "starting service '{service}' in the microVM guest did not finish in {}s",
            GUEST_LIFECYCLE_TIMEOUT.as_secs()
        )));
    };
    if !out.status.success() {
        reap_detached();
        return Err(H5iError::Metadata(format!(
            "service failed to start in the microVM guest: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // Both lines arrive on the same pipe, from the same exec, so they describe
    // the same life of the guest — read separately, a restart in between would
    // pair a new boot with a pid from the old one. Tagged rather than
    // positional because the launcher and the detached shell write
    // independently and either may land first.
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |tag: &str| {
        text.lines()
            .filter_map(|l| l.strip_prefix(tag))
            .map(|v| v.trim().to_string())
            .next()
    };
    let (Some(boot), Some(pid_text), Some(sid_text)) =
        (field("boot "), field("pid "), field("sid "))
    else {
        reap_detached();
        return Err(H5iError::Metadata(format!(
            "the microVM guest did not report a boot id, pid and session for service \
             '{service}' (got {text:?})"
        )));
    };
    let pid: u32 = match pid_text.parse() {
        Ok(p) => p,
        Err(_) => {
            reap_detached();
            return Err(H5iError::Metadata(format!(
                "the microVM guest reported an unreadable pid for service '{service}' \
                 (got {text:?})"
            )));
        }
    };
    // The recorded pid is later used as a process *group* to signal, so it has
    // to be the session leader. An empty session also lands here, which is what
    // a service that died before the launcher could look at it produces — worth
    // failing on rather than recording.
    if sid_text != pid_text {
        stop_group(&rt, &name, pid);
        return Err(H5iError::Metadata(format!(
            "service '{service}' did not become its own session leader in the microVM guest \
             (pid {pid_text}, session {sid_text:?}) — refusing to record a pid whose process \
             group is not this service's"
        )));
    }

    // A service that dies on its first breath — a port already bound, a missing
    // interpreter — would otherwise be reported as started, and the failure
    // would only surface later as a record naming a dead pid. Give it a moment,
    // then insist it is still there.
    std::thread::sleep(SERVICE_SETTLE);
    if service_pid_state(&rt, &name, pid) != Some(true) {
        let tail = tail_service_log(work, service);
        return Err(H5iError::Metadata(format!(
            "service '{service}' exited immediately after starting in the microVM guest{tail}"
        )));
    }
    Ok(ServiceHandle {
        pid,
        sandbox: name,
        boot,
    })
}

/// How long a service is given to fail before it is called started.
const SERVICE_SETTLE: Duration = Duration::from_millis(300);

/// Is guest pid `pid` its own session leader — i.e. does the process group
/// `-pid` names belong to it?
///
/// `sed`/`cut` rather than a positional field, for the reason the launcher
/// gives: the `comm` field in `/proc/<pid>/stat` may contain spaces and
/// parentheses, so counting columns from the left is wrong for a process that
/// picked an awkward name. A runtime that cannot be asked answers "no", which
/// skips a best-effort cleanup rather than signalling on a guess.
fn guest_session_leader(rt: &Runtime, sandbox: &str, pid: u32) -> bool {
    guest_sh(
        &rt.bin,
        sandbox,
        &format!(
            "test \"$(sed -e 's/^.*) //' /proc/{pid}/stat 2>/dev/null | cut -d' ' -f4)\" = {pid}"
        ),
    )
}

/// Best-effort TERM+KILL of a service's process group inside the guest.
fn stop_group(rt: &Runtime, sandbox: &str, pid: u32) {
    guest_sh(&rt.bin, sandbox, &format!("kill -TERM -{pid} 2>/dev/null"));
    guest_sh(&rt.bin, sandbox, &format!("kill -KILL -{pid} 2>/dev/null"));
}

/// The last few lines of a service's log, for an error message.
///
/// Every byte here was written *inside the box* — it is the service's own
/// stdout — and this string goes straight to the operator's terminal through
/// `Error: …`. So it is sanitised on the way out and bounded on the way in:
///
/// * A control sequence in a log line repaints the terminal it is printed on.
///   Five lines is enough for `ESC[2J ESC[1;1H` and a forged h5i banner
///   underneath it, and a service that fails to start on purpose is how a box
///   gets that printed on demand.
/// * The read is capped because nothing bounds the file. `box service start`
///   reading a log the box grew to fill the disk is a host-side OOM triggered
///   from inside a box, and only the tail is wanted anyway.
fn tail_service_log(work: &Path, service: &str) -> String {
    let Ok(path) = service_log_path(work, service) else {
        return String::new();
    };
    let text = read_tail(&path, SERVICE_LOG_TAIL_BYTES);
    let tail: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("#h5i-pid "))
        .rev()
        .take(5)
        .collect();
    if tail.is_empty() {
        return " (its log is empty)".into();
    }
    let body = tail.into_iter().rev().collect::<Vec<_>>().join("\n  ");
    format!(". Its log ends:\n  {}", sanitize_block(&body))
}

/// How much of a service log is read before it is tailed. The lines wanted are
/// at the end, and the file's size is the box's choice.
const SERVICE_LOG_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The last `cap` bytes of `path`, lossily decoded, or `""` if it cannot be
/// read. A partial leading line is dropped rather than shown truncated.
fn read_tail(path: &Path, cap: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let truncated = len > cap;
    if truncated && f.seek(SeekFrom::End(-(cap as i64))).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.take(cap).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    match text.find('\n') {
        Some(i) if truncated => text[i + 1..].to_string(),
        _ => text,
    }
}

/// The pid a launcher recorded into the service log, for cleaning up after a
/// start that failed **after** the service had already detached.
///
/// Without this, a host-side failure past the point of no return — the launcher
/// exec timing out, say — leaves the service running in a guest that outlives
/// the command, invisible to `service status` and unreachable by
/// `service stop`, with a retry starting a second copy beside it.
/// This marker is **guest-writable**, and that is the whole reason for the
/// checks on it. The log is a mounted file the service itself writes to; a
/// service that prints its own `#h5i-pid` line wins, because the last one is
/// the one read. So the number that comes back here is a hint from inside the
/// box, and it is used to send a signal to a process *group* — the same hazard
/// that kept the pid out of a pidfile a few screens up, arriving through the
/// log instead.
///
/// `-1` as a process group means "every process the caller can signal", so an
/// unchecked marker of `1` turns a failed `service start` into `kill -KILL -1`
/// as root inside the guest. Anything below 2 is refused here, and
/// `reap_detached` confirms the pid is a session leader — the launcher's own
/// invariant, which the success path in `start_service` already checks and this
/// path did not — before signalling.
///
/// What is left is that a box can *hide* the real pid by appending a marker of
/// its own, which loses the orphan this function exists to reap. That is a
/// service leaking into a guest the operator can still see and stop, not a
/// signal aimed by the box, and it cannot be closed from the host: the log is
/// the only channel a launcher that never returned left behind.
fn logged_service_pid(work: &Path, service: &str) -> Option<u32> {
    let path = service_log_path(work, service).ok()?;
    // Bounded for the same reason the tail is: the box chooses this file's
    // size, and the marker is near its end.
    let text = read_tail(&path, SERVICE_LOG_TAIL_BYTES);
    let pid: u32 = text
        .lines()
        .rev()
        .filter_map(|l| l.strip_prefix("#h5i-pid "))
        .next()
        .and_then(|v| v.trim().parse().ok())?;
    // 0 is "my own process group", 1 is init, and `-1` is everything: none of
    // them is a service, and each of them is a worse thing to signal than the
    // orphan we came for.
    (pid >= 2).then_some(pid)
}

/// A service started inside a guest: its pid, the guest, and that guest's boot
/// identity, which is what keeps the pid meaningful across a restart.
pub struct ServiceHandle {
    pub pid: u32,
    pub sandbox: String,
    pub boot: String,
}

/// Is guest pid `pid` still running inside `sandbox`?
///
/// `false` when the guest itself is gone, which is the common case after a
/// policy change: the guest that held the service was reaped and replaced, so
/// the service died with it.
pub fn service_alive(sandbox: &str, pid: u32, boot: &str) -> bool {
    service_state(sandbox, pid, boot).unwrap_or(false)
}

/// Liveness as a *tri-state*: `None` means the runtime could not be asked.
///
/// `service_alive` collapses that to "not running", which is right for a status
/// display and wrong for stopping: a stop that reads a transient failure as
/// "already dead" skips the signal, removes the record, and leaves the service
/// running in the guest with nothing on the host that knows about it.
pub fn service_state(sandbox: &str, pid: u32, boot: &str) -> Option<bool> {
    let rt = probe()?;
    match guest_state(&rt.bin, sandbox) {
        // The guest is gone or stopped, so anything inside it is too. That is
        // an answer, not an absence of one.
        GuestState::Absent | GuestState::Stopped => return Some(false),
        GuestState::Unknown => return None,
        GuestState::Running => {}
    }
    // A guest keeps its name across `stop`/`start`, and its pids restart from 1
    // when it boots again. So a record saying "pid 42" can match a *different*
    // process in the guest's next life — h5i would refuse to start a service
    // that is dead, and `service stop` would `kill -TERM -42` an unrelated
    // process group. The boot id makes the two lives distinguishable.
    match guest_boot_id(&rt, sandbox) {
        Some(now) if now == boot => {}
        // Rebooted: whatever the record names is gone, whoever holds the
        // number now.
        Some(_) => return Some(false),
        None => return None,
    }
    // Through `sh -c`, because `kill` is a **shell builtin**: a slim image has
    // no `/bin/kill`, so exec'ing it directly returns 127 and every service
    // would read as dead — and, on the signalling path below, would never
    // actually be stopped.
    //
    service_pid_state(&rt, sandbox, pid)
}

/// Just "does this pid exist in that guest" — one exec, no guest-state or
/// boot-id round trips.
///
/// For polling a service as it shuts down, where the caller has *already*
/// established that the guest is running and is the same life the record names.
/// [`service_state`] re-checks all three, and a 30-iteration wait loop calling
/// it made ninety runtime round trips out of what should be one per poll.
pub fn service_pid_running(rt: &Runtime, sandbox: &str, pid: u32) -> bool {
    // Collapsing here is deliberate and safe: the only caller is the shutdown
    // poll, where "could not ask" simply ends the wait early and the KILL that
    // follows is harmless. Anything that *decides* something — whether to
    // signal at all, whether a record may be deleted — must use
    // [`service_pid_state`] instead.
    service_pid_state(rt, sandbox, pid).unwrap_or(false)
}

/// [`service_pid_running`] as a tri-state: `None` when the runtime could not be
/// asked.
///
/// This distinction is the whole point of `service_state`, and collapsing it
/// here once already turned a hung `msb exec` into "the service is dead" —
/// which makes `service_stop` skip its signal and delete the record anyway,
/// leaving a live dev server in the guest that nothing on the host can reach.
pub fn service_pid_state(rt: &Runtime, sandbox: &str, pid: u32) -> Option<bool> {
    // `kill -0` alone is not liveness: it succeeds on a **zombie**, and a
    // service that exits inside a guest stays one until something reaps it.
    // Guest init reparents it to pid 1 and may never do so, so a finished dev
    // server would read as running forever — `service status` reporting a
    // corpse as healthy, `service start` refusing because of it, and
    // `service stop` waiting out its whole grace period before sending a
    // pointless KILL. Seen in exactly that order before this line existed.
    //
    // `/proc/<pid>/status` is read rather than `stat` because the `comm` field
    // in `stat` may contain spaces and parentheses, which makes positional
    // parsing of the state wrong for a process that chose the wrong name.
    let mut cmd = std::process::Command::new(&rt.bin);
    cmd.args([
        "exec",
        "--quiet",
        sandbox,
        "--",
        "sh",
        "-c",
        &pid_running_probe(pid),
    ]);
    run_bounded(cmd, GUEST_QUERY_TIMEOUT).map(|o| o.status.success())
}

/// The shell test for "pid `pid` is a live process in this guest". Pure, so the
/// rule survives in a test rather than only in a string.
pub fn pid_running_probe(pid: u32) -> String {
    format!("kill -0 {pid} 2>/dev/null && ! grep -qs '^State:.*Z' /proc/{pid}/status")
}

/// The detected runtime, for a caller that needs several guest round trips and
/// should not re-probe for each one.
pub fn runtime() -> Option<Runtime> {
    probe()
}

/// This guest's boot identity — the kernel's own, so it changes on every boot
/// and cannot be confused with the sandbox's name, which survives a restart.
///
/// `None` when the runtime could not be asked, which the callers propagate as
/// "unknown" rather than guessing.
pub fn guest_boot_id(rt: &Runtime, sandbox: &str) -> Option<String> {
    let mut cmd = std::process::Command::new(&rt.bin);
    cmd.args([
        "exec",
        "--quiet",
        sandbox,
        "--",
        "sh",
        "-c",
        "cat /proc/sys/kernel/random/boot_id",
    ]);
    let out = run_bounded(cmd, GUEST_QUERY_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Run one line of shell in the guest, reporting whether it succeeded.
fn guest_sh(bin: &str, sandbox: &str, script: &str) -> bool {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["exec", "--quiet", sandbox, "--", "sh", "-c", script]);
    // A hang here reads as "not running", which is the safe direction: it stops
    // a stuck runtime from stalling `box service status`, and the worst it
    // costs is a service reported dead that is not.
    run_bounded(cmd, GUEST_QUERY_TIMEOUT).is_some_and(|o| o.status.success())
}

/// How long a question to the runtime may take before we stop waiting.
const GUEST_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long creating or starting a guest may take. Generous — a cold boot is
/// hundreds of milliseconds, but a host under load is not a failure.
const GUEST_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Run `cmd` to completion or to `limit`, whichever comes first. `None` means
/// it overran and was killed.
///
/// Every call into the runtime goes through this. `msb exec` has been observed
/// to hang indefinitely — rarely, and still undiagnosed
/// (`docs/benchmarks/microvm-boot.md`) — and these calls sit behind
/// `box service status` and `box run`, so an unbounded one hangs the CLI with
/// no way out but Ctrl-C.
fn run_bounded(
    mut cmd: std::process::Command,
    limit: Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let mut out_pipe = child.stdout.take()?;
    let mut err_pipe = child.stderr.take()?;
    let out_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut out_pipe));
    let err_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut err_pipe));
    let deadline = std::time::Instant::now() + limit;
    let mut poll = Duration::from_millis(1);
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                return Some(std::process::Output {
                    status,
                    stdout: out_h.join().unwrap_or_default(),
                    stderr: err_h.join().unwrap_or_default(),
                });
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The reader threads are left to finish on their own: the
                    // kill closes the pipes, and joining a thread blocked on a
                    // pipe some grandchild still holds would reintroduce the
                    // hang this function exists to prevent.
                    return None;
                }
                std::thread::sleep(poll);
                poll = (poll * 2).min(POLL_MAX);
            }
        }
    }
}

/// Signal the service's whole process group inside the guest. `sig` is a name
/// `kill` understands (`TERM`, `KILL`). Best-effort, like its kernel-tier twin.
pub fn service_signal(sandbox: &str, pid: u32, sig: &str) {
    let Some(rt) = probe() else {
        return;
    };
    // `-{pid}` is the process *group* — the service was `setsid`'d precisely so
    // this reaps its whole tree, as `killpg` does on the kernel tiers. Via
    // `sh -c` for the same builtin reason as [`service_alive`].
    guest_sh(&rt.bin, sandbox, &format!("kill -{sig} -{pid}"));
}

/// Sentinel for [`ExecPlan::env_script`] meaning "run the command directly,
/// with no per-run env wrapper" — used by the service launcher, which sources
/// its own environment.
const SHELL_DIRECT: &str = "";

/// Single-quote one argument for a POSIX shell, with the total `'\''` escape.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Join argv into shell text, quoting each word.
fn shell_join(argv: &[String]) -> String {
    argv.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
}

/// The guest-visible path of a script staged in the per-run directory.
fn guest_script_path(host: &Path) -> Result<String, H5iError> {
    let file = host.file_name().ok_or_else(|| {
        H5iError::Metadata(format!("staged env script has no file name: {}", host.display()))
    })?;
    Ok(format!("{RUN_MOUNT}/{}", file.to_string_lossy()))
}

/// Stand-in `--name` used while hashing a create argv into the real name.
const GUEST_NAME_PLACEHOLDER: &str = "h5i-unnamed";

/// The original one-shot path: a fresh guest per command, destroyed after.
fn run_one_shot(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let rt = runtime_or_refuse()?;
    let image = image_or_refuse(&policy.profile)?;
    check_mount_paths(policy, work)?;

    let net = net_plan(policy)?;
    let preload = write_preload(work, &guest_env(policy, injected_env))?;
    // Sweep leftovers from an h5i that died without running Drop.
    reap_orphaned_sandboxes(&rt.bin);
    let sandbox = SandboxGuard::new(&rt.bin);
    let full = build_run_argv(
        &rt,
        policy,
        work,
        &RunPlan {
            image: &image,
            name: &sandbox.name,
            net: &net,
            argv,
            preload: &preload.path,
            tty: None,
            managed_settings: None,
        },
    );

    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    let outcome = wait_vm(cmd, &sandbox, policy.profile.wall(), &full)?;
    Ok(ExecOutcome {
        wall_ms: started.elapsed().as_millis(),
        ..outcome
    })
}

/// The **agent-in-box** path: run `argv` (a shell or a coding agent) inside the
/// microVM with stdio inherited — a real interactive session whose every command
/// is confined by the VM boundary and the netstack allowlist. Captures nothing
/// and applies no wall clock; the operator owns the session.
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    managed_settings_content: Option<&str>,
) -> Result<InteractiveOutcome, H5iError> {
    if reuse_available(policy) {
        return run_interactive_warm(policy, work, argv, injected_env, managed_settings_content);
    }
    run_interactive_one_shot(policy, work, argv, injected_env, managed_settings_content)
}

/// The warm agent-in-box path: exec a session into the box's own guest, so the
/// shell shares state with the box's captured runs — which is what every other
/// tier already does, and why 9. calls per-command amnesia an artifact.
fn run_interactive_warm(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    managed_settings_content: Option<&str>,
) -> Result<InteractiveOutcome, H5iError> {
    use std::io::IsTerminal;
    // Not injected on this path, and the guest's identity is why: the mount is
    // create-time, so a session that added it would resolve to a *different*
    // guest than `box run` uses, and creating it would reap the other — killing
    // any service running there. The only caller passes `None` today anyway
    // (deliberately, see `env::shell`); re-enabling it means designing it into
    // the create argv for every entry point, not just this one.
    let _ = managed_settings_content;
    let WarmGuest { rt, name, stage } = ensure_warm_guest(policy, work)?;

    let script = write_env_script(&stage, "env", &guest_env(policy, injected_env))?;
    let guest_script = guest_script_path(&script.path)?;

    // Only ask for a pseudo-TTY when we have one on both ends — msb rejects
    // `--tty` under a pipe, which would turn a CI `env shell` into a hard error.
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let exec_argv = build_exec_argv(
        &rt,
        policy,
        &ExecPlan {
            name: &name,
            argv,
            env_script: &guest_script,
            tty: Some(tty),
            bounded: true,
        },
    );

    let status = std::process::Command::new(&exec_argv[0])
        .args(&exec_argv[1..])
        .status()
        .map_err(|e| H5iError::Metadata(format!("failed to start microvm session: {e}")))?;
    Ok(InteractiveOutcome {
        exit_code: status.code().unwrap_or(130),
        egress: None,
    })
}

/// The original one-shot session: a guest booted for this session and destroyed
/// with it.
fn run_interactive_one_shot(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    managed_settings_content: Option<&str>,
) -> Result<InteractiveOutcome, H5iError> {
    use std::io::IsTerminal;
    let rt = runtime_or_refuse()?;
    let image = image_or_refuse(&policy.profile)?;
    check_mount_paths(policy, work)?;

    let net = net_plan(policy)?;
    let preload = write_preload(work, &guest_env(policy, injected_env))?;
    // Managed-settings injection is Claude's managed scope and inert for Codex,
    // whose hook hardening is separate; `default`/custom profiles may run Claude,
    // so they get it too.
    let is_codex = crate::sandbox_policy::AgentRuntime::from_profile_name(&policy.profile.name)
        == Some(crate::sandbox_policy::AgentRuntime::Codex);
    let managed_settings = match (is_codex, managed_settings_content) {
        (false, Some(content)) => prepare_managed_settings(work, content),
        _ => None,
    };
    // Only ask for a pseudo-TTY when we have one on both ends — msb rejects
    // `--tty` under a pipe, which would turn a CI `env shell` into a hard error.
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    // Sweep leftovers from an h5i that died without running Drop.
    reap_orphaned_sandboxes(&rt.bin);
    let sandbox = SandboxGuard::new(&rt.bin);
    let full = build_run_argv(
        &rt,
        policy,
        work,
        &RunPlan {
            image: &image,
            name: &sandbox.name,
            net: &net,
            argv,
            preload: &preload.path,
            tty: Some(tty),
            managed_settings: managed_settings.as_deref(),
        },
    );

    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    let status = cmd
        .status()
        .map_err(|e| H5iError::Metadata(format!("failed to start microvm session: {e}")))?;
    Ok(InteractiveOutcome {
        exit_code: status.code().unwrap_or(130),
        egress: None,
    })
}

/// The environment the guest command runs with: the profile's `env.pass`
/// allowlist resolved against *this* process's environment, then the brokered
/// grants (which win, since a broker-minted credential is the one the policy
/// actually authorized). Nothing is inherited wholesale.
fn guest_env(policy: &ResolvedPolicy, injected_env: &[(String, String)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    for key in &policy.profile.env_pass {
        if let Some(value) = std::env::var_os(key) {
            env.push((key.clone(), value.to_string_lossy().into_owned()));
        }
    }
    for (k, v) in injected_env {
        env.retain(|(existing, _)| existing != k);
        env.push((k.clone(), v.clone()));
    }
    env
}

/// Spawn `msb exec`, stream output, and enforce the wall clock — the warm-path
/// twin of [`wait_vm`], differing in exactly one way that matters.
///
/// **It never removes the sandbox.** `wait_vm` does, because there the guest
/// belongs to the one command it was booted for. Here the guest belongs to the
/// *box*: destroying it on a timeout would take out a session, a dev server, or
/// a concurrent command that has nothing to do with the run that overran.
/// `msb exec --timeout` already bounds the guest-side command, so this deadline
/// is the host-side backstop for a client that hangs — which has been observed,
/// rarely and undiagnosed, and is exactly why the backstop exists.
fn wait_exec(
    mut cmd: std::process::Command,
    wall: Duration,
    full: &[String],
) -> Result<ExecOutcome, H5iError> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", full.join(" "))))?;

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut out_pipe));
    let err_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut err_pipe));

    let deadline = std::time::Instant::now() + wall + Duration::from_secs(10);
    let mut timed_out = false;
    // Backoff rather than a flat 25 ms. On the one-shot path the poll is lost in
    // a ~230 ms boot, but a warm exec finishes in single-digit milliseconds, so
    // a flat cadence *is* the measured cost: it turned a 9 ms command into a
    // 35 ms one. Start fine and widen, so a fast command is noticed almost at
    // once while a long one still costs one wakeup every 25 ms.
    let mut poll = Duration::from_millis(1);
    let status = loop {
        match child.try_wait().map_err(H5iError::Io)? {
            Some(s) => break s,
            None => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    // Return **without joining**, for the reason `run_bounded`
                    // gives: if anything the killed client left behind still
                    // holds the pipe's write end, joining blocks forever and
                    // `box run` hangs past its own wall clock with no error —
                    // the exact failure this deadline exists to prevent.
                    return Ok(ExecOutcome {
                        stdout: Vec::new(),
                        stderr: b"(output dropped: the microVM exec overran its deadline)\n"
                            .to_vec(),
                        exit_code: None,
                        timed_out,
                        wall_ms: 0,
                        cpu_ms: 0,
                        max_rss_kb: None,
                        egress: None,
                    });
                }
                std::thread::sleep(poll);
                poll = (poll * 2).min(POLL_MAX);
            }
        }
    };

    Ok(ExecOutcome {
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
        exit_code: status.code(),
        timed_out,
        wall_ms: 0,
        cpu_ms: 0,
        max_rss_kb: None,
        egress: None,
    })
}

/// Ceiling for the completion-poll backoff, and the flat cadence the one-shot
/// path still uses.
const POLL_MAX: Duration = Duration::from_millis(25);

/// Spawn `msb`, stream output, and enforce the wall clock. On timeout, stop the
/// sandbox itself (the client dying does not stop a detached VM) then kill the
/// client. Resource accounting belongs to the guest kernel, not to `msb`, so we
/// report wall time only.
fn wait_vm(
    mut cmd: std::process::Command,
    sandbox: &SandboxGuard,
    wall: Duration,
    full: &[String],
) -> Result<ExecOutcome, H5iError> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", full.join(" "))))?;

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut out_pipe));
    let err_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut err_pipe));

    // `--timeout` already caps the guest command; this is the host-side backstop
    // for an `msb` that hangs before or after the guest ever runs. The grace
    // keeps the two from racing to report the same timeout.
    let deadline = std::time::Instant::now() + wall + Duration::from_secs(10);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(H5iError::Io)? {
            Some(s) => break s,
            None => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    // Stop the guest first: killing the client does not stop a
                    // VM it has already handed off to the host runtime.
                    sandbox.remove();
                    let _ = child.kill();
                    break child.wait().map_err(H5iError::Io)?;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };

    Ok(ExecOutcome {
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
        exit_code: status.code(),
        timed_out,
        wall_ms: 0,
        cpu_ms: 0,
        max_rss_kb: None,
        egress: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox_policy::{IsolationClaim, PrivateBind, RoBind};

    fn rt() -> Runtime {
        Runtime {
            bin: "msb".into(),
            version: (0, 6),
        }
    }

    fn policy() -> ResolvedPolicy {
        let mut p = Profile::builtin("default", IsolationClaim::Microvm);
        p.image = Some("alpine".into());
        ResolvedPolicy::new(IsolationClaim::Microvm, p)
    }

    fn argv_for(policy: &ResolvedPolicy, net: &NetPlan, tty: Option<bool>) -> Vec<String> {
        build_run_argv(
            &rt(),
            policy,
            Path::new("/h5i/envs/e1/work"),
            &RunPlan {
                image: "alpine",
                name: "h5i-1-0",
                net,
                argv: &["sh".into(), "-c".into(), "true".into()],
                preload: Path::new("/h5i/envs/e1/microvm/preload-1.sh"),
                tty,
                managed_settings: None,
            },
        )
    }

    fn window<'a>(a: &'a [String], flag: &str) -> Vec<&'a str> {
        a.iter()
            .zip(a.iter().skip(1))
            .filter(|(f, _)| f.as_str() == flag)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    // ─── version parsing ────────────────────────────────────────────────────

    /// A named msb sandbox outlives us, and Drop cannot run on SIGKILL. The
    /// sweep reaps a marker whose pid is gone and leaves a live one alone.
    #[test]
    fn orphan_sweep_only_targets_dead_pids() {
        // Our own pid is alive, so it must never be swept.
        assert!(pid_alive(std::process::id() as i32));
        // pid 1 always exists; kill(1,0) is EPERM for a normal user, which the
        // helper must read as "alive" rather than "reapable".
        assert!(pid_alive(1));
        // A bad pid is never treated as reapable.
        assert!(pid_alive(0));
        assert!(pid_alive(-1));
        // A pid that cannot exist is reapable.
        assert!(!pid_alive(0x7fff_fffe));
    }

    #[test]
    fn a_guard_leaves_a_marker_and_clears_it() {
        let g = SandboxGuard::new("true");
        let m = marker_path(&g.name).unwrap();
        assert!(m.exists(), "a live sandbox records a marker");
        assert!(g.name.starts_with(&format!("h5i-{}-", std::process::id())));
        drop(g);
        assert!(!m.exists(), "Drop clears the marker");
    }

    #[test]
    fn version_is_parsed_from_the_banner_shapes_msb_prints() {
        assert_eq!(parse_version("msb 0.6.8\n"), Some((0, 6)));
        assert_eq!(parse_version("Microsandbox CLI v1.12.0"), Some((1, 12)));
        assert_eq!(parse_version("msb"), None);
        assert_eq!(parse_version(""), None);
    }

    // ─── egress translation ─────────────────────────────────────────────────

    #[test]
    fn plain_host_becomes_an_any_port_rule() {
        let tokens = egress_rule_tokens(&["pypi.org".into()]).unwrap();
        assert_eq!(tokens, vec!["allow@pypi.org"]);
    }

    #[test]
    fn a_port_scoped_entry_narrows_to_tcp_on_that_port() {
        let tokens = egress_rule_tokens(&["github.com:443".into()]).unwrap();
        assert_eq!(tokens, vec!["allow@github.com:tcp:443"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_preload_script_is_never_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let work = td.path().join("work");
        std::fs::create_dir_all(&work).unwrap();

        let secret = "SUPER_SECRET_VALUE";
        let script = write_preload(&work, &[("TOKEN".into(), secret.into())]).unwrap();

        // The mode must be 0600 as observed, and — the actual point — it must
        // never have been anything else, so it has to be set at creation rather
        // than chmod'd afterwards.
        let mode = std::fs::metadata(&script.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "preload script is {mode:o}, not 0600");
        assert!(std::fs::read_to_string(&script.path).unwrap().contains(secret));

        // Writing again (same process, next run) must not inherit a wider mode
        // from a leftover file.
        let stale = write_preload(&work, &[("TOKEN".into(), secret.into())]).unwrap();
        std::fs::set_permissions(&stale.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let again = write_preload(&work, &[("TOKEN".into(), secret.into())]).unwrap();
        let mode = std::fs::metadata(&again.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a later run came out {mode:o}");
    }

    #[test]
    fn an_out_of_range_port_is_refused_not_widened() {
        // `.ok()` here used to drop the parse failure, leaving `port = None` and
        // emitting `allow@example.com` — every port, when the entry asked for
        // one. A fail-closed module must not resolve "cannot translate" to
        // "allow more".
        for entry in ["example.com:99999", "example.com:65536", "example.com:0x1"] {
            let out = egress_rule_tokens(&[entry.to_string()]);
            assert!(
                out.is_err() || !out.as_ref().unwrap().iter().any(|t| t == "allow@example.com"),
                "{entry} widened to an any-port rule: {out:?}"
            );
        }
        assert!(egress_rule_tokens(&["example.com:65536".into()]).is_err());
        // The boundary still works.
        assert_eq!(
            egress_rule_tokens(&["example.com:65535".into()]).unwrap(),
            vec!["allow@example.com:tcp:65535"]
        );
    }

    #[test]
    fn an_ipv6_literal_is_refused_not_mangled() {
        // `2001:db8::1` split into host `2001:db8:` + port `1` and came out as
        // `allow@2001:db8::tcp:1`, which is not the address anyone wrote.
        for entry in ["2001:db8::1", "::1", "fe80::1%eth0", "2001:db8::1:443"] {
            assert!(
                egress_rule_tokens(&[entry.to_string()]).is_err(),
                "{entry} was translated instead of refused"
            );
        }
        // A single colon is still an ordinary host:port.
        assert_eq!(
            egress_rule_tokens(&["example.com:443".into()]).unwrap(),
            vec!["allow@example.com:tcp:443"]
        );
    }

    #[test]
    fn no_dns_token_is_ever_emitted() {
        // `msb` 0.6.8 rejects every spelling of the `dns` target, so emitting
        // one failed the run before the guest booted. Name resolution goes
        // through the gateway, which stays reachable under a default-deny
        // egress policy, so the domain rules below are sufficient on their own.
        for entry in ["pypi.org", "github.com:443", "*.example.com", "10.0.0.1"] {
            let tokens = egress_rule_tokens(&[entry.into()]).unwrap();
            assert!(
                !tokens.iter().any(|t| t.contains("dns")),
                "emitted a dns rule for {entry}: {tokens:?}"
            );
        }
    }

    #[test]
    fn a_wildcard_covers_the_apex_as_well_as_subdomains() {
        // h5i's `.suffix` matches `suffix` itself, so a `suffix=` token alone
        // would silently *narrow* the policy the profile declared.
        let tokens = egress_rule_tokens(&[".githubusercontent.com".into()]).unwrap();
        assert_eq!(
            tokens,
            vec![
                "allow@domain=githubusercontent.com",
                "allow@suffix=githubusercontent.com",
            ]
        );
        assert_eq!(
            egress_rule_tokens(&["*.githubusercontent.com".into()]).unwrap(),
            tokens,
            "both wildcard spellings mean the same allowlist"
        );
    }

    #[test]
    fn an_empty_allowlist_emits_no_rules_at_all() {
        // An empty list is deny-all, and the caller turns "no rules" into
        // `--no-net` rather than a default-deny rule set.
        assert!(egress_rule_tokens(&[]).unwrap().is_empty());
        assert!(egress_rule_tokens(&["  ".into()]).unwrap().is_empty());
    }

    #[test]
    fn duplicate_entries_collapse_and_order_is_stable() {
        let tokens =
            egress_rule_tokens(&["pypi.org".into(), "PyPI.org".into(), "crates.io".into()]).unwrap();
        assert_eq!(tokens, vec!["allow@pypi.org", "allow@crates.io"]);
    }

    #[test]
    fn entries_that_would_break_the_token_grammar_are_refused() {
        for bad in ["a.com,b.com", "user@a.com"] {
            let err = egress_rule_tokens(&[bad.into()]).unwrap_err().to_string();
            assert!(err.contains("fail-closed"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_single_label_wildcard_is_refused_rather_than_allowing_a_whole_tld() {
        let err = egress_rule_tokens(&["*.com".into()]).unwrap_err().to_string();
        assert!(err.contains("at least two"), "{err}");
    }

    #[test]
    fn an_ip_entry_passes_through_as_its_own_target() {
        assert_eq!(
            egress_rule_tokens(&["198.51.100.5:8080".into()]).unwrap(),
            vec!["allow@198.51.100.5:tcp:8080"]
        );
    }

    // ─── preload script ─────────────────────────────────────────────────────

    #[test]
    fn preload_execs_the_real_command_so_argv_and_exit_code_pass_through() {
        let s = preload_script(&[("TERM".into(), "xterm".into())]);
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains("export TERM='xterm'\n"));
        assert!(s.trim_end().ends_with("exec \"$@\""));
    }

    #[test]
    fn a_value_containing_quotes_cannot_break_out_of_the_assignment() {
        // The POSIX '\'' escape is total: no byte can end the quoted string and
        // start a command. A credential with a quote in it must not become code.
        let s = preload_script(&[("T".into(), "a'; rm -rf /; echo '".into())]);
        assert!(s.contains(r"export T='a'\''; rm -rf /; echo '\'''"), "{s}");
        assert!(!s.contains("\nrm -rf"), "value escaped into command position: {s}");
    }

    #[test]
    fn newlines_and_dollar_signs_survive_verbatim() {
        let s = preload_script(&[("K".into(), "line1\nline2 $HOME `id`".into())]);
        assert!(s.contains("export K='line1\nline2 $HOME `id`'\n"), "{s}");
    }

    // ─── argv: the security-critical flag set ───────────────────────────────

    #[test]
    fn the_workspace_is_the_only_writable_mount_by_default() {
        let a = argv_for(&policy(), &NetPlan::None, None);
        let dirs = window(&a, "--mount-dir");
        assert_eq!(dirs, vec!["/h5i/envs/e1/work:/work:rw"]);
        assert!(window(&a, "--workdir").contains(&"/work"));
        assert!(a.contains(&"--no-net".to_string()));
    }

    #[test]
    fn no_environment_value_ever_reaches_argv() {
        // The whole point of the preload script. A `--env` flag here would
        // publish brokered credentials through /proc/<pid>/cmdline.
        let a = argv_for(&policy(), &NetPlan::None, None);
        assert!(!a.iter().any(|x| x == "--env" || x == "-e"), "{a:?}");
        let scripts = window(&a, "--script-path");
        assert_eq!(scripts, vec!["h5i-env:/h5i/envs/e1/microvm/preload-1.sh"]);
        // …and the command actually runs through it.
        let sep = a.iter().position(|x| x == "--").expect("`--` separator");
        assert_eq!(a[sep + 1], "/.msb/scripts/h5i-env");
        assert_eq!(&a[sep + 2..], &["sh", "-c", "true"]);
    }

    #[test]
    fn the_image_is_positional_and_never_pulled_at_run_time() {
        let a = argv_for(&policy(), &NetPlan::None, None);
        assert!(window(&a, "--pull").contains(&"never"));
        let sep = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[sep - 1], "alpine", "image sits just before the `--`");
    }

    #[test]
    fn an_allowlist_is_default_deny_in_both_directions_plus_its_rules() {
        let net = NetPlan::Allow(egress_rule_tokens(&["pypi.org".into()]).unwrap());
        let a = argv_for(&policy(), &net, None);
        assert!(window(&a, "--net-default-egress").contains(&"deny"));
        assert!(window(&a, "--net-default-ingress").contains(&"deny"));
        assert_eq!(window(&a, "--net-rule"), vec!["allow@pypi.org"]);
        // Never the container tier's proxy env — there is no proxy here, and a
        // stale HTTP_PROXY would make the box look filtered when it is not.
        assert!(!a.iter().any(|x| x.contains("HTTP_PROXY")), "{a:?}");
        // And never microsandbox's rebind-protection escape hatch.
        assert!(!a.iter().any(|x| x == "--no-dns-rebind-protection"), "{a:?}");
    }

    #[test]
    fn host_net_mode_asks_for_the_public_profile_and_no_rules() {
        let a = argv_for(&policy(), &NetPlan::Host, None);
        assert!(window(&a, "--net").contains(&"public"));
        assert!(window(&a, "--net-rule").is_empty());
        assert!(!a.contains(&"--no-net".to_string()));
    }

    #[test]
    fn resource_limits_come_from_the_profile() {
        let mut pol = policy();
        pol.profile.mem_bytes = Some(2 * 1024 * 1024 * 1024);
        pol.profile.max_procs = Some(64);
        pol.profile.cpu_secs = Some(600);
        let a = argv_for(&pol, &NetPlan::None, None);
        assert!(window(&a, "--memory").contains(&"2048M"));
        let rlimits = window(&a, "--rlimit");
        assert!(rlimits.contains(&"nproc=64"), "{rlimits:?}");
        assert!(rlimits.contains(&"cpu=600"), "{rlimits:?}");
    }

    #[test]
    fn the_wall_clock_applies_to_captured_runs_and_not_to_sessions() {
        let mut pol = policy();
        pol.profile.wall_secs = 900;
        assert!(window(&argv_for(&pol, &NetPlan::None, None), "--timeout").contains(&"900s"));
        // An interactive session is bounded by the operator, not a timer.
        assert!(window(&argv_for(&pol, &NetPlan::None, Some(true)), "--timeout").is_empty());
    }

    #[test]
    fn a_pseudo_tty_is_requested_only_for_an_interactive_session_that_has_one() {
        assert!(argv_for(&policy(), &NetPlan::None, Some(true)).contains(&"--tty".to_string()));
        for tty in [Some(false), None] {
            let a = argv_for(&policy(), &NetPlan::None, tty);
            assert!(a.contains(&"--no-tty".to_string()), "{tty:?}");
            assert!(!a.contains(&"--tty".to_string()), "{tty:?}");
        }
    }

    #[test]
    fn caches_mount_read_only_and_the_one_writable_cache_is_explicit() {
        let mut pol = policy();
        pol.ro_binds = vec![RoBind {
            backing: PathBuf::from("/host/cache/cargo"),
            target: PathBuf::from("/root/.cargo/registry"),
        }];
        pol.cache_write = Some(RoBind {
            backing: PathBuf::from("/host/cache/npm"),
            target: PathBuf::from("/root/.npm"),
        });
        let a = argv_for(&pol, &NetPlan::None, None);
        let dirs = window(&a, "--mount-dir");
        assert!(dirs.contains(&"/host/cache/cargo:/root/.cargo/registry:ro"), "{dirs:?}");
        assert!(dirs.contains(&"/host/cache/npm:/root/.npm:rw"), "{dirs:?}");
    }

    #[test]
    fn spool_is_writable_and_the_inbox_is_not() {
        let mut pol = policy();
        pol.env_capture_spool = Some(PathBuf::from("/h5i/envs/e1/spool"));
        pol.env_inbox = Some(PathBuf::from("/h5i/envs/e1/inbox"));
        let a = argv_for(&pol, &NetPlan::None, None);
        let dirs = window(&a, "--mount-dir");
        assert!(dirs.contains(&"/h5i/envs/e1/spool:/.h5i/spool:rw"), "{dirs:?}");
        assert!(dirs.contains(&"/h5i/envs/e1/inbox:/.h5i/inbox:ro"), "{dirs:?}");
    }

    #[test]
    fn private_paths_shadow_their_workspace_path_inside_the_box() {
        let mut pol = policy();
        pol.private_binds = vec![PrivateBind {
            backing: PathBuf::from("/h5i/envs/e1/private/target"),
            rel: "target".into(),
        }];
        let a = argv_for(&pol, &NetPlan::None, None);
        let dirs = window(&a, "--mount-dir");
        assert!(
            dirs.contains(&"/h5i/envs/e1/private/target:/work/target:rw"),
            "{dirs:?}"
        );
    }

    #[test]
    fn the_agents_own_hook_config_is_pinned_read_only_inside_the_writable_workspace() {
        // $WORK is rw, so without this mount an in-box agent could rewrite the
        // file that defines its observation hook and go dark.
        let dir = std::env::temp_dir().join(format!("h5i-microvm-cfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("work/.claude")).unwrap();
        // Canonicalized: the mount source is the resolved path, so the
        // expectation has to agree where the temp root is itself a symlink.
        let work = dir.canonicalize().unwrap().join("work");
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        let a = build_run_argv(
            &rt(),
            &policy(),
            &work,
            &RunPlan {
                image: "alpine",
                name: "h5i-1-0",
                net: &NetPlan::None,
                argv: &["bash".into()],
                preload: Path::new("/tmp/preload.sh"),
                tty: Some(true),
                managed_settings: None,
            },
        );
        let files = window(&a, "--mount-file");
        assert!(
            files.contains(
                &format!("{}:/work/.claude/settings.json:ro", work.join(".claude/settings.json").display())
                    .as_str()
            ),
            "{files:?}"
        );
        // The config that is not there is not invented.
        assert!(!files.iter().any(|f| f.contains(".codex/config.toml")), "{files:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn managed_settings_mount_is_read_only_so_the_hook_cannot_be_silenced() {
        let a = build_run_argv(
            &rt(),
            &policy(),
            Path::new("/h5i/envs/e1/work"),
            &RunPlan {
                image: "alpine",
                name: "h5i-1-0",
                net: &NetPlan::None,
                argv: &["bash".into()],
                preload: Path::new("/h5i/envs/e1/microvm/preload-1.sh"),
                tty: Some(true),
                managed_settings: Some(Path::new("/h5i/envs/e1/managed/managed-settings.json")),
            },
        );
        let files = window(&a, "--mount-file");
        assert!(
            files.contains(
                &"/h5i/envs/e1/managed/managed-settings.json:/etc/claude-code/managed-settings.json:ro"
            ),
            "{files:?}"
        );
    }

    // ─── fail-closed path handling ──────────────────────────────────────────

    #[test]
    fn a_colon_in_a_host_path_is_refused_rather_than_silently_dropped() {
        let mut pol = policy();
        pol.private_binds = vec![PrivateBind {
            backing: PathBuf::from("/h5i/weird:dir/target"),
            rel: "target".into(),
        }];
        let err = check_mount_paths(&pol, Path::new("/h5i/envs/e1/work"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("fail-closed"), "{err}");
    }

    #[test]
    fn a_deny_all_profile_is_never_widened_by_the_host_side_user_allowlist() {
        let mut pol = policy();
        pol.user_egress_allow = vec!["evil.example".into()];
        assert_eq!(net_plan(&pol).unwrap(), NetPlan::None);
        // …but a profile that already opted into egress does take the extras.
        pol.profile.net_egress = vec!["pypi.org".into()];
        let NetPlan::Allow(rules) = net_plan(&pol).unwrap() else {
            panic!("expected an allowlist plan");
        };
        assert!(rules.contains(&"allow@pypi.org".to_string()), "{rules:?}");
        assert!(rules.contains(&"allow@evil.example".to_string()), "{rules:?}");
    }

    #[test]
    fn brokered_grants_win_over_the_env_pass_allowlist() {
        let mut pol = policy();
        pol.profile.env_pass = vec!["H5I_MICROVM_TEST_VAR".into()];
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_MICROVM_TEST_VAR", "from-host");
        }
        let env = guest_env(&pol, &[("H5I_MICROVM_TEST_VAR".into(), "brokered".into())]);
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_MICROVM_TEST_VAR");
        }
        assert_eq!(
            env,
            vec![("H5I_MICROVM_TEST_VAR".to_string(), "brokered".to_string())]
        );
    }

    // ─── warm guests: naming ────────────────────────────────────────────────

    fn create_argv_for(policy: &ResolvedPolicy, net: &NetPlan, name: &str) -> Vec<String> {
        build_create_argv(
            &rt(),
            policy,
            Path::new("/h5i/envs/e1/work"),
            &warm_create_plan(
                "alpine",
                name,
                net,
                Path::new("/h5i/envs/e1/microvm/run"),
                Path::new("/h5i/envs/e1/microvm/service-logs"),
                Some(GUEST_IDLE_TIMEOUT),
            ),
        )
    }

    #[test]
    fn a_label_is_reduced_to_what_msb_accepts_in_a_name() {
        // `/` is the one msb rejects outright, and every box id has them.
        assert_eq!(sanitize_label("env/human/my-box"), "env-human-my-box");
        // Runs collapse, leading separators are dropped, trailing ones trimmed.
        assert_eq!(sanitize_label("///a///b///"), "a-b");
        assert_eq!(sanitize_label("Feature/ABC_123"), "feature-abc-123");
        // A name must start alphanumeric; the `h5i-` prefix guarantees it even
        // when the label itself is empty.
        assert_eq!(sanitize_label("///"), "");
        assert!(sanitize_label(&"x".repeat(200)).len() <= GUEST_LABEL_MAX);
    }

    /// The reuse-safety property, stated as a test: a guest is only ever reused
    /// for a configuration identical to the one it was created under, because
    /// the name is a hash of that configuration.
    #[test]
    fn a_changed_configuration_yields_a_different_guest_name() {
        let base = policy();
        let a = create_argv_for(&base, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        let name_a = guest_name(Path::new("/h5i/envs/e1/work"), &a);

        // Same policy, same everything → same guest.
        let again = create_argv_for(&base, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        assert_eq!(name_a, guest_name(Path::new("/h5i/envs/e1/work"), &again));

        // A widened egress allowlist must not be served the old guest, which is
        // still enforcing the narrower one.
        let net = NetPlan::Allow(vec!["allow@pypi.org".into()]);
        let b = create_argv_for(&base, &net, GUEST_NAME_PLACEHOLDER);
        assert_ne!(name_a, guest_name(Path::new("/h5i/envs/e1/work"), &b));

        // A memory change resizes the VM, so it cannot be the same VM.
        let mut heavier = policy();
        heavier.profile.mem_bytes = Some(9 * 1024 * 1024 * 1024);
        let c = create_argv_for(&heavier, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        assert_ne!(name_a, guest_name(Path::new("/h5i/envs/e1/work"), &c));

        // A new read-only mount changes what the box can reach.
        let mut mounted = policy();
        mounted.ro_binds = vec![RoBind {
            backing: "/host/cache".into(),
            target: "/root/.cache".into(),
        }];
        let d = create_argv_for(&mounted, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        assert_ne!(name_a, guest_name(Path::new("/h5i/envs/e1/work"), &d));
    }

    #[test]
    fn two_boxes_never_share_a_guest() {
        let pol = policy();
        let argv = create_argv_for(&pol, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        let one = guest_name(Path::new("/h5i/envs/e1/work"), &argv);
        let two = guest_name(Path::new("/h5i/envs/e2/work"), &argv);
        assert_ne!(one, two, "the box label is part of the name");
        assert!(one.starts_with("h5i-"), "{one}");
        assert!(one.len() <= 128, "msb rejects nothing this short: {one}");
    }

    #[test]
    fn a_guest_name_is_readable_and_carries_its_box() {
        let pol = policy();
        let argv = create_argv_for(&pol, &NetPlan::None, GUEST_NAME_PLACEHOLDER);
        let name = guest_name(Path::new("/h5i/.h5i/env/human/web-ui/work"), &argv);
        assert!(name.starts_with("h5i-human-web-ui-"), "{name}");
    }

    // ─── warm guests: create argv ───────────────────────────────────────────

    #[test]
    fn create_carries_the_mounts_memory_and_egress_but_no_command() {
        let mut pol = policy();
        pol.profile.mem_bytes = Some(2 * 1024 * 1024 * 1024);
        let net = NetPlan::Allow(vec!["allow@api.anthropic.com".into()]);
        let a = create_argv_for(&pol, &net, "h5i-e1-abc");

        assert_eq!(a[1], "create");
        assert_eq!(window(&a, "--name"), vec!["h5i-e1-abc"]);
        assert_eq!(window(&a, "--pull"), vec!["never"]);
        assert_eq!(window(&a, "--memory"), vec!["2048M"]);
        assert_eq!(window(&a, "--net-default-egress"), vec!["deny"]);
        assert_eq!(window(&a, "--net-rule"), vec!["allow@api.anthropic.com"]);
        assert_eq!(window(&a, "--idle-timeout"), vec!["30m"]);
        // The workspace and the per-run staging directory are both mounted.
        assert!(window(&a, "--mount-dir").contains(&"/h5i/envs/e1/work:/work:rw"));
        assert!(
            window(&a, "--mount-dir").contains(&"/h5i/envs/e1/microvm/run:/.h5i/run:rw"),
            "{a:?}"
        );
        // The image is last, and no command follows it: a create boots a guest
        // and nothing else.
        assert_eq!(a.last().unwrap(), "alpine");
        assert!(!a.contains(&"--".to_string()), "{a:?}");
        // Per-command concerns belong to exec, not to the guest.
        assert!(window(&a, "--timeout").is_empty(), "{a:?}");
        assert!(window(&a, "--rlimit").is_empty(), "{a:?}");
        assert!(!a.contains(&"--tty".to_string()));
        assert!(!a.contains(&"--no-tty".to_string()));
    }

    #[test]
    fn a_guest_always_gets_an_idle_bound_since_msb_has_no_default() {
        let a = create_argv_for(&policy(), &NetPlan::None, "h5i-e1-abc");
        assert_eq!(window(&a, "--idle-timeout"), vec![GUEST_IDLE_TIMEOUT]);
    }

    /// …but a box that runs services must not get one. `msb` measures idleness
    /// in commands, not in the traffic a dev server is serving, so the bound
    /// would stop the guest and kill the service. Measured before it was
    /// believed: a 20s bound stopped a guest at ~25s and its service died.
    #[test]
    fn a_box_that_declares_services_gets_no_idle_bound() {
        let net = NetPlan::None;
        let stage = Path::new("/h5i/envs/e1/microvm/run");
        let logs = Path::new("/h5i/envs/e1/microvm/service-logs");

        let serviceless = build_create_argv(
            &rt(),
            &policy(),
            Path::new("/h5i/envs/e1/work"),
            &warm_create_plan("alpine", "g", &net, stage, logs, Some(GUEST_IDLE_TIMEOUT)),
        );
        assert_eq!(window(&serviceless, "--idle-timeout"), vec![GUEST_IDLE_TIMEOUT]);

        let with_services = build_create_argv(
            &rt(),
            &policy(),
            Path::new("/h5i/envs/e1/work"),
            &warm_create_plan("alpine", "g", &net, stage, logs, None),
        );
        assert!(
            window(&with_services, "--idle-timeout").is_empty(),
            "a service-hosting guest must never be stopped for idleness: {with_services:?}"
        );
        // And the two are different guests, which is correct: whether a box may
        // be stopped is part of what its guest *is*.
        assert_ne!(
            guest_name(Path::new("/h5i/envs/e1/work"), &serviceless),
            guest_name(Path::new("/h5i/envs/e1/work"), &with_services),
        );
    }

    #[test]
    fn a_deny_all_box_gets_no_network_at_create() {
        let a = create_argv_for(&policy(), &NetPlan::None, "h5i-e1-abc");
        assert!(a.contains(&"--no-net".to_string()), "{a:?}");
    }

    /// The mount set is factored so one-shot and warm cannot drift. If it ever
    /// does, this is the test that says so.
    #[test]
    fn one_shot_and_warm_mount_the_same_things() {
        let mut pol = policy();
        pol.ro_binds = vec![RoBind {
            backing: "/host/cache".into(),
            target: "/root/.cache".into(),
        }];
        pol.private_binds = vec![PrivateBind {
            backing: "/host/private".into(),
            rel: "target".into(),
        }];
        pol.env_capture_spool = Some("/host/spool".into());
        pol.env_inbox = Some("/host/inbox".into());

        let one = argv_for(&pol, &NetPlan::None, None);
        let warm = create_argv_for(&pol, &NetPlan::None, "h5i-e1-abc");

        for m in window(&one, "--mount-dir") {
            assert!(
                window(&warm, "--mount-dir").contains(&m),
                "warm guest is missing the one-shot mount {m}"
            );
        }
        for m in window(&one, "--mount-file") {
            assert!(
                window(&warm, "--mount-file").contains(&m),
                "warm guest is missing the one-shot mount {m}"
            );
        }
    }

    // ─── warm guests: exec argv ─────────────────────────────────────────────

    fn exec_argv_for(policy: &ResolvedPolicy, tty: Option<bool>) -> Vec<String> {
        build_exec_argv(
            &rt(),
            policy,
            &ExecPlan {
                name: "h5i-e1-abc",
                argv: &["sh".into(), "-c".into(), "true".into()],
                env_script: "/.h5i/run/env-1-0.sh",
                tty,
                bounded: true,
            },
        )
    }

    #[test]
    fn exec_runs_the_command_through_this_run_s_env_script() {
        let a = exec_argv_for(&policy(), None);
        assert_eq!(a[1], "exec");
        // Flags, then the guest name, then the command after `--`.
        let sep = a.iter().position(|x| x == "--").expect("a `--` separator");
        assert_eq!(a[sep - 1], "h5i-e1-abc");
        assert_eq!(
            &a[sep + 1..],
            &[
                "/bin/sh".to_string(),
                "/.h5i/run/env-1-0.sh".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]
        );
    }

    /// The reason the staging mount exists: a credential must never reach a
    /// host command line, on either path.
    #[test]
    fn exec_never_carries_an_env_value_in_argv() {
        let a = exec_argv_for(&policy(), None);
        assert!(window(&a, "--env").is_empty(), "{a:?}");
        assert!(window(&a, "-e").is_empty(), "{a:?}");
        assert!(!a.iter().any(|x| x.contains('=') && x.contains("SECRET")), "{a:?}");
    }

    #[test]
    fn a_captured_run_is_wall_clocked_and_a_session_is_not() {
        let mut pol = policy();
        pol.profile.wall_secs = 900;
        pol.profile.max_procs = Some(512);
        pol.profile.cpu_secs = Some(600);

        let captured = exec_argv_for(&pol, None);
        assert_eq!(window(&captured, "--timeout"), vec!["900s"]);
        assert!(captured.contains(&"--no-tty".to_string()));
        assert_eq!(window(&captured, "--rlimit"), vec!["nproc=512", "cpu=600"]);
        assert_eq!(window(&captured, "--workdir"), vec![WORK_MOUNT]);

        let session = exec_argv_for(&pol, Some(true));
        assert!(window(&session, "--timeout").is_empty(), "{session:?}");
        assert!(session.contains(&"--tty".to_string()));

        // A piped session must not ask for a pseudo-TTY.
        let piped = exec_argv_for(&pol, Some(false));
        assert!(piped.contains(&"--no-tty".to_string()));
    }

    // ─── warm guests: state machine ─────────────────────────────────────────

    #[test]
    fn guest_state_is_read_from_the_list_json() {
        let json = r#"[
            {"name":"other","status":"Running","image":"alpine"},
            {"name":"h5i-e1-abc","status":"Running","image":"alpine"}
        ]"#;
        assert_eq!(parse_guest_state(json, "h5i-e1-abc"), GuestState::Running);
        assert_eq!(parse_guest_state(json, "nope"), GuestState::Absent);
    }

    /// A stopped guest must be *started*, never exec'd into: exec would boot it,
    /// run, and stop it again, paying a full boot every command forever.
    #[test]
    fn anything_not_running_is_reported_stopped_so_the_caller_starts_it() {
        for status in ["Stopped", "stopped", "Starting", "Draining", "Paused"] {
            let json = format!(r#"[{{"name":"g","status":"{status}"}}]"#);
            assert_eq!(
                parse_guest_state(&json, "g"),
                GuestState::Stopped,
                "status {status} must not be mistaken for a warm guest"
            );
        }
    }

    /// Only a well-formed list that does not mention the guest is `Absent`.
    /// Anything we could not read is `Unknown`, because `Absent` is answered
    /// with `create --replace`, which destroys a running guest.
    #[test]
    fn only_a_readable_list_can_say_a_guest_is_absent() {
        assert_eq!(parse_guest_state("[]", "g"), GuestState::Absent);
        assert_eq!(
            parse_guest_state(r#"[{"name":"other","status":"Running"}]"#, "g"),
            GuestState::Absent
        );
        // Unreadable in various ways — none of these may read as "no guest".
        assert_eq!(parse_guest_state("", "g"), GuestState::Unknown);
        assert_eq!(parse_guest_state("not json", "g"), GuestState::Unknown);
        assert_eq!(parse_guest_state(r#"{"name":"g"}"#, "g"), GuestState::Unknown);
        assert_eq!(
            parse_guest_state("warning: something\n[]", "g"),
            GuestState::Unknown,
            "a banner on stdout must not read as an empty list"
        );
    }

    /// "Could not ask" and "not running" must stay distinguishable, because
    /// `service_stop` deletes the record after deciding — and deleting it on a
    /// transient failure orphans a service that is still running.
    #[test]
    fn a_runtime_that_cannot_answer_is_not_the_same_as_a_dead_guest() {
        // The distinction is carried by `GuestState`, which `service_state`
        // maps to `Some(false)` / `None`.
        assert_eq!(parse_guest_state("[]", "g"), GuestState::Absent);
        assert_eq!(
            parse_guest_state(r#"[{"name":"g","status":"Stopped"}]"#, "g"),
            GuestState::Stopped
        );
        // Both of those are *answers* — the guest is gone or halted, so
        // anything inside it is too, and `service_state` maps them to
        // `Some(false)`. Output we could not read is not an answer.
        for text in ["", "not json", r#"{"name":"g"}"#] {
            assert_eq!(
                parse_guest_state(text, "g"),
                GuestState::Unknown,
                "unreadable output must not be mistaken for an answer"
            );
        }
    }

    /// `kill -0` succeeds on a zombie, and a service that exits inside a guest
    /// stays one until something reaps it — which guest init may never do. Left
    /// at `kill -0`, a finished dev server read as running forever: status
    /// reported a corpse as healthy, `start` refused because of it, and `stop`
    /// waited out its whole grace period before a pointless KILL.
    #[test]
    fn liveness_excludes_a_zombie() {
        let probe = pid_running_probe(4242);
        assert!(probe.contains("kill -0 4242"), "{probe}");
        assert!(
            probe.contains("/proc/4242/status"),
            "a zombie is only visible in the process's state: {probe}"
        );
        assert!(probe.contains("State:.*Z"), "{probe}");
        // The two tests are ANDed, so a zombie fails the whole probe.
        assert!(probe.contains("&&"), "{probe}");
    }

    /// A service must not inherit the per-command bounds. rlimits survive
    /// `setsid` and `exec`, so a CPU bound meant for one command would follow
    /// the detached dev server and `SIGXCPU` it hours later, unexplained.
    #[test]
    fn a_service_launcher_carries_no_per_command_bounds() {
        let mut pol = policy();
        pol.profile.cpu_secs = Some(600);
        pol.profile.max_procs = Some(512);
        pol.profile.wall_secs = 900;

        let launcher = build_exec_argv(
            &rt(),
            &pol,
            &ExecPlan {
                name: "h5i-e1-abc",
                argv: &["-c".into(), "true".into()],
                env_script: SHELL_DIRECT,
                tty: None,
                bounded: false,
            },
        );
        assert!(window(&launcher, "--rlimit").is_empty(), "{launcher:?}");
        assert!(window(&launcher, "--timeout").is_empty(), "{launcher:?}");

        // A captured run still gets all of them.
        let run = exec_argv_for(&pol, None);
        assert_eq!(window(&run, "--rlimit"), vec!["nproc=512", "cpu=600"]);
        assert_eq!(window(&run, "--timeout"), vec!["900s"]);
    }

    // ─── warm guests: reuse eligibility ─────────────────────────────────────

    #[test]
    fn a_cache_refresh_run_never_defines_the_box_s_guest() {
        let mut pol = policy();
        assert!(reuse_available(&pol), "an ordinary run reuses");
        pol.cache_write = Some(RoBind {
            backing: "/host/cache".into(),
            target: "/root/.cache".into(),
        });
        assert!(
            !reuse_available(&pol),
            "the one run whose mount set differs must stay one-shot"
        );
    }

    // ─── warm guests: marker bookkeeping ────────────────────────────────────

    /// A warm guest outlives its process on purpose, so the pid rule that reaps
    /// one-shot guests must not touch it — the box's existence is the signal.
    #[test]
    fn a_warm_marker_records_its_box_and_a_one_shot_marker_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();

        let name = format!("h5i-test-warm-{}", std::process::id());
        write_marker(&name, &work);
        let found: Vec<_> = live_markers().into_iter().filter(|(n, _)| n == &name).collect();
        assert_eq!(found.len(), 1, "the marker is on disk");
        assert_eq!(
            found[0].1.as_deref(),
            Some(work.display().to_string().as_str()),
            "a warm marker names the box it belongs to"
        );

        let g = SandboxGuard::new("true");
        let one_shot: Vec<_> = live_markers().into_iter().filter(|(n, _)| n == &g.name).collect();
        assert_eq!(one_shot[0].1, None, "a one-shot marker carries no box");

        // Cleanup: the sweep would otherwise see a live box and keep this.
        if let Some(m) = marker_path(&name) {
            let _ = std::fs::remove_file(m);
        }
    }

    // ─── background services ────────────────────────────────────────────────

    /// Write `text` as service `svc`'s log under a fresh work dir.
    fn with_service_log(text: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let path = service_log_path(&work, "svc").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        (tmp, work)
    }

    /// The service log is written inside the box, and its tail is printed to
    /// the operator's terminal through `Error: …`. A service that fails to
    /// start on purpose is how a box asks for that to happen, so the escape
    /// sequences that would repaint the terminal must not survive the trip.
    #[test]
    fn a_service_log_cannot_repaint_the_terminal_it_is_reported_on() {
        let esc = '\u{1b}';
        let (_tmp, work) = with_service_log(&format!(
            "listening on 3000\n{esc}[2J{esc}[1;1Hh5i: policy verified, egress denied\n\
             \u{202e}derotinom ton\ndone\r  spoof\n"
        ));
        let tail = tail_service_log(&work, "svc");
        assert!(!tail.contains(esc), "{tail:?}");
        assert!(!tail.contains('\u{202e}'), "{tail:?}");
        assert!(!tail.contains('\r'), "{tail:?}");
        // Sanitised, not swallowed: the operator still gets the diagnosis.
        assert!(tail.contains("listening on 3000"), "{tail:?}");
        // And the line structure survives, which is the point of the block form.
        assert!(tail.lines().count() >= 4, "{tail:?}");
    }

    /// Nothing bounds the size of that file — the box writes it. Reading all of
    /// it to print five lines is a host-side OOM a box can ask for.
    #[test]
    fn a_service_log_the_box_grew_is_read_by_the_tail_only() {
        // One line longer than the whole cap, then three short ones. A read of
        // the last `cap` bytes lands in the middle of the giant line and drops
        // that partial, so the tail is the three short lines and nothing else.
        // Reading the file whole would make the giant line a *complete* line
        // and put all of it in the five the error message prints — which is the
        // difference this asserts on, since "the read stopped early" is not
        // otherwise visible from the outside.
        let mut text = "x".repeat(SERVICE_LOG_TAIL_BYTES as usize + 4096);
        text.push_str("\nfirst\nsecond\nthe last line\n");
        let (_tmp, work) = with_service_log(&text);
        let path = service_log_path(&work, "svc").unwrap();
        assert!(
            std::fs::metadata(&path).unwrap().len() > SERVICE_LOG_TAIL_BYTES,
            "the fixture has to be bigger than the cap for this to test anything"
        );
        assert!(
            read_tail(&path, SERVICE_LOG_TAIL_BYTES).len() as u64 <= SERVICE_LOG_TAIL_BYTES,
            "the read is bounded"
        );
        let tail = tail_service_log(&work, "svc");
        assert!(tail.contains("the last line"), "the tail is still the tail");
        assert!(
            tail.len() < 1024,
            "the giant line reached the error message ({} bytes)",
            tail.len()
        );
    }

    /// The pid marker lives in the log, so the box writes it: the service's own
    /// stdout lands in the same file, and the *last* marker is the one read.
    /// `stop_group` turns that number into `kill -KILL -<pid>`, and `-1` is
    /// "every process the caller can signal" — as root, inside the guest.
    #[test]
    fn a_forged_pid_marker_cannot_aim_the_cleanup_at_init() {
        for forged in ["#h5i-pid 0", "#h5i-pid 1", "#h5i-pid -1", "#h5i-pid nonsense"] {
            let (_tmp, work) = with_service_log(&format!("#h5i-pid 4242\n{forged}\n"));
            assert_eq!(
                logged_service_pid(&work, "svc"),
                None,
                "{forged:?} was accepted as a process group to signal"
            );
        }
        // A real one still comes back, or the cleanup this exists for is gone.
        let (_tmp, work) = with_service_log("starting\n#h5i-pid 4242\nlistening\n");
        assert_eq!(logged_service_pid(&work, "svc"), Some(4242));
        // And the newest marker wins, which is what makes it forgeable at all —
        // asserted so the reap-vs-hide tradeoff in the doc stays honest.
        let (_tmp, work) = with_service_log("#h5i-pid 4242\n#h5i-pid 77\n");
        assert_eq!(logged_service_pid(&work, "svc"), Some(77));
    }

    /// Every warm entry point must build the same create argv, or each creates
    /// its own guest and reaps the others' — killing any service running there.
    #[test]
    fn every_entry_point_describes_the_same_guest() {
        let pol = policy();
        let net = NetPlan::None;
        let stage = Path::new("/h5i/envs/e1/microvm/run");
        let logs = Path::new("/h5i/envs/e1/microvm/service-logs");
        let a = build_create_argv(
            &rt(),
            &pol,
            Path::new("/h5i/envs/e1/work"),
            &warm_create_plan("alpine", GUEST_NAME_PLACEHOLDER, &net, stage, logs, Some("30m")),
        );
        // The helper is the single description; anything reconstructing a plan
        // by hand would diverge here.
        let b = build_create_argv(
            &rt(),
            &pol,
            Path::new("/h5i/envs/e1/work"),
            &warm_create_plan("alpine", GUEST_NAME_PLACEHOLDER, &net, stage, logs, Some("30m")),
        );
        assert_eq!(a, b);
        // Managed settings are never part of a warm guest: a create-time mount
        // that varied per session would split the box's guest in two.
        assert!(
            !a.iter().any(|x| x.contains("managed-settings")),
            "warm guests must not vary on managed settings: {a:?}"
        );
    }

    #[test]
    fn the_service_log_directory_is_mounted_but_the_records_are_not() {
        let a = create_argv_for(&policy(), &NetPlan::None, "h5i-e1-abc");
        let mounts = window(&a, "--mount-dir");
        assert!(
            mounts.iter().any(|m| m.ends_with(":/.h5i/services:rw")),
            "the guest needs somewhere to write a service log: {mounts:?}"
        );
        // The records carry the pid the host later signals. A box that could
        // rewrite one could name a host pid and have `service_stop` killpg it.
        assert!(
            mounts.iter().all(|m| !m.contains("/services:") || m.contains("service-logs")),
            "the service *records* directory must never be mounted: {mounts:?}"
        );
    }

    /// The launcher sources the credentials and deletes them before detaching,
    /// so they live on disk for one exec rather than for the service's life.
    #[test]
    fn a_service_launcher_removes_its_credentials_before_detaching() {
        let script = "/.h5i/run/svc-1-0.sh";
        let launcher = format!(
            ". {s} && rm -f {s}\ncd /work 2>/dev/null || cd /\nsetsid 'sh' '-c' 'npm run dev' >>'/.h5i/services/web.log' 2>&1 &\nprintf '%s\\n' \"$!\"\n",
            s = sh_quote(script)
        );
        let rm_at = launcher.find("rm -f").expect("the launcher removes the script");
        let detach_at = launcher.find("setsid").expect("the launcher detaches");
        assert!(rm_at < detach_at, "removal must precede detaching:\n{launcher}");
        assert!(launcher.contains("setsid"), "a session leader, so -pid reaps the tree");
    }

    /// `SHELL_DIRECT` means the argv is already shell text that sources its own
    /// environment — the service launcher. Anything else is wrapped.
    #[test]
    fn exec_runs_a_launcher_directly_and_a_command_through_its_env_script() {
        let direct = build_exec_argv(
            &rt(),
            &policy(),
            &ExecPlan {
                name: "h5i-e1-abc",
                argv: &["-c".into(), "echo hi".into()],
                env_script: SHELL_DIRECT,
                tty: None,
                bounded: false,
            },
        );
        let sep = direct.iter().position(|x| x == "--").unwrap();
        assert_eq!(&direct[sep + 1..], &["/bin/sh".to_string(), "-c".into(), "echo hi".into()]);

        let wrapped = exec_argv_for(&policy(), None);
        let sep = wrapped.iter().position(|x| x == "--").unwrap();
        assert_eq!(wrapped[sep + 1], "/bin/sh");
        assert_eq!(wrapped[sep + 2], "/.h5i/run/env-1-0.sh");
    }

    #[test]
    fn shell_quoting_survives_every_byte_a_command_can_carry() {
        // The same total `'\''` escape the preload uses, for the same reason.
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("a b; rm -rf /"), "'a b; rm -rf /'");
        assert_eq!(sh_quote("$(whoami)"), "'$(whoami)'");
        assert_eq!(
            shell_join(&["sh".into(), "-c".into(), "echo 'hi'".into()]),
            r"'sh' '-c' 'echo '\''hi'\'''"
        );
    }

    /// The gate between a directory listing and `msb remove --force <name>`.
    /// A marker directory is not a place we control the contents of, so only
    /// names shaped like the ones this module produces may reach the runtime.
    #[test]
    fn only_names_h5i_produces_can_reach_the_runtime() {
        // Both forms this module writes.
        assert!(is_h5i_sandbox_name("h5i-12345-0"), "one-shot");
        assert!(is_h5i_sandbox_name("h5i-human-web-ui-08839c208e2d"), "warm");
        // Not ours.
        assert!(!is_h5i_sandbox_name("some-victim-sandbox"));
        assert!(!is_h5i_sandbox_name("h5i-"), "the prefix alone is not a name");
        assert!(!is_h5i_sandbox_name(""));
        // A name that clap could read as a flag must never get through.
        assert!(!is_h5i_sandbox_name("--all"));
        assert!(!is_h5i_sandbox_name("-rf"));
        // Nor anything that could confuse a shell or a spec parser if the call
        // shape ever changed.
        assert!(!is_h5i_sandbox_name("h5i-a/b"));
        assert!(!is_h5i_sandbox_name("h5i-a b"));
        assert!(!is_h5i_sandbox_name("h5i-a:b"));
        assert!(!is_h5i_sandbox_name("h5i-A"), "uppercase is not a shape we emit");
    }

    /// `exists()` answers `false` both for "removed" and for "cannot look", and
    /// the difference is whether somebody's running VM gets destroyed.
    #[test]
    fn an_unreadable_workspace_is_not_mistaken_for_a_removed_box() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        assert!(!box_is_gone(&work), "a live box is not gone");

        let missing = tmp.path().join("never-existed");
        assert!(box_is_gone(&missing), "a removed box is gone");

        // A path whose parent denies traversal reports NotFound through
        // `exists()` but errors with PermissionDenied through metadata — the
        // case that had one user's sweep destroying another user's guests.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = tmp.path().join("locked");
            std::fs::create_dir_all(locked.join("work")).unwrap();
            let inner = locked.join("work");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root ignores the mode bits, so only assert where the denial is real.
            if std::fs::symlink_metadata(&inner)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            {
                assert!(
                    !box_is_gone(&inner),
                    "a workspace we cannot see must never be read as removed"
                );
            }
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// Markers decide which VMs are destroyed, so they must not live somewhere
    /// another login can write.
    #[test]
    fn the_marker_directory_is_private_to_this_user() {
        let Some(dir) = marker_dir() else {
            return; // no usable directory here; nothing to assert about one
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let md = std::fs::symlink_metadata(&dir).expect("marker dir exists once resolved");
            assert!(md.is_dir());
            assert_eq!(md.uid(), current_uid(), "owned by this user");
            assert_eq!(md.permissions().mode() & 0o022, 0, "not group/other-writable");
        }
        // And it is never the bare shared temp dir the first version used.
        assert_ne!(dir, std::env::temp_dir().join("h5i-msb-live"));
    }

    #[test]
    fn a_squatted_marker_directory_is_refused_rather_than_used() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            // World-writable: the shape a squatter leaves behind.
            let open = tmp.path().join("open");
            std::fs::create_dir_all(&open).unwrap();
            std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(!ensure_private_dir(&open), "a world-writable dir is refused");

            // A symlink is rejected rather than followed.
            let target = tmp.path().join("target");
            std::fs::create_dir_all(&target).unwrap();
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(!ensure_private_dir(&link), "a symlink is refused");

            // The ordinary case still works, and is created 0700.
            let fresh = tmp.path().join("fresh").join("msb-live");
            assert!(ensure_private_dir(&fresh));
            let mode = std::fs::symlink_metadata(&fresh).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "created private, mode {mode:o}");
        }
    }

    /// The reason a warm marker exists at all: once the box is gone, so is any
    /// reason to keep a VM configured for it.
    #[test]
    fn the_sweep_reaps_a_warm_guest_whose_box_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let name = format!("h5i-test-gone-{}", std::process::id());
        write_marker(&name, &work);

        // Box still present: the sweep must leave it alone. `true` stands in for
        // the runtime, so nothing is actually removed either way.
        reap_orphaned_sandboxes("true");
        assert!(marker_path(&name).unwrap().exists(), "a live box keeps its guest");

        std::fs::remove_dir_all(&work).unwrap();
        reap_orphaned_sandboxes("true");
        assert!(
            !marker_path(&name).unwrap().exists(),
            "a removed box's guest is reaped"
        );
    }
}
