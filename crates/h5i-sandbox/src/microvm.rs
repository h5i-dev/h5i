//! The `isolation=microvm` backend: run an environment's command inside a
//! **hardware-isolated microVM** via the [microsandbox](https://microsandbox.dev)
//! runtime (`msb`), and enforce the `net.egress` domain allowlist *in the VM's
//! own network stack* rather than in a host-side HTTP proxy.
//!
//! ### Why this tier exists
//!
//! `container.rs` says it plainly: its `net.egress` enforcement is **L7**. It
//! blocks the dominant exfiltration path (`curl`/`pip`/`npm` honouring
//! `HTTP(S)_PROXY`) and nothing stops a process that ignores the proxy env and
//! opens a raw socket to an arbitrary IP the rootless NAT permits. The same
//! module names the fix: *"airtight L3/L4 egress filtering is the
//! `hardened-container`/`microvm` tier"*. This module is that tier.
//!
//! Two things change relative to `container`:
//!
//! 1. **The boundary is a virtual machine.** The guest runs its own kernel on
//!    KVM (Linux) or Hypervisor.framework (Apple Silicon). A kernel exploit in
//!    the box is contained by the hypervisor rather than by the host kernel it
//!    just subverted, which is the property neither the kernel tiers nor a
//!    shared-kernel container can offer.
//! 2. **Egress is filtered by address, not by proxy etiquette.** The allowlist
//!    becomes `--net-default-egress deny` plus one `--net-rule` per allowed
//!    destination, evaluated by the VM's virtual network stack. A raw socket to
//!    an unlisted IP is dropped; there is no `HTTP_PROXY` to ignore. DNS-rebind
//!    protection is on by default and we never disable it.
//!
//! ### What it costs, stated rather than hidden
//!
//! - **The host must support virtualization.** `/dev/kvm` on Linux, Apple
//!   Silicon on macOS. No nested virtualization (a plain WSL2 kernel, most CI
//!   runners) means no microvm tier, and [`resolve`] refuses rather than
//!   downgrading.
//! - **No per-request egress tally.** The container tier's proxy sees every
//!   CONNECT and reports allow/deny counts into the capture manifest. A VM
//!   netstack filter drops packets without telling us which, so
//!   [`ExecOutcome::egress`] is `None` here. Stronger enforcement, weaker
//!   evidence — we report the tier's rules at session start instead of
//!   pretending to a tally we do not have.
//! - **No in-box tee shim.** The container tier self-mounts its own image at
//!   `/.h5i/orig` so a shadowed `/bin/sh` still has a real shell to exec. A VM
//!   has no image to self-mount, so that trick has no analogue. The *primary*
//!   in-box observation path — the read-only managed-settings mount carrying the
//!   unkillable `wrap-bash` hook — works here exactly as it does under
//!   `container`, and the capture spool is mounted the same way.
//!
//! ### Secrets never enter the host argv
//!
//! `msb` has no name-only env forwarding (`podman --env NAME` reads the value
//! from its own environment; `msb --env` takes `KEY=VALUE`). Putting a brokered
//! credential in `msb run`'s argv would publish it to every local user through
//! `/proc/<pid>/cmdline`, which is exactly the leak `container.rs` goes out of
//! its way to avoid. So this backend passes **no** environment on the command
//! line at all: it writes a `0600` preload script host-side, registers it with
//! `--script-path` (whose *contents* travel to the runtime over a config fd, not
//! argv), and runs the command through it. See [`preload_script`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::H5iError;
use crate::sandbox_policy::{ExecOutcome, InteractiveOutcome, NetMode, Profile, ResolvedPolicy};

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
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if raw.contains(',') || raw.contains('@') {
            return Err(H5iError::Metadata(format!(
                "net.egress entry '{raw}' contains ',' or '@', which the microvm tier's rule \
                 grammar reserves — refusing rather than emitting a rule that means something \
                 else (fail-closed). Split it into separate entries."
            )));
        }
        // An IPv6 literal is full of colons, and the port split below cannot tell
        // one from a `host:port`: `2001:db8::1` used to come out as
        // `allow@2001:db8::tcp:1` — a rule that means nothing anyone asked for.
        // The grammar has no unambiguous spelling for one here, so refuse it
        // rather than translate it wrong.
        if raw.matches(':').count() > 1 {
            return Err(H5iError::Metadata(format!(
                "net.egress entry '{raw}' looks like an IPv6 literal (more than one ':'), which \
                 the microvm tier's rule grammar cannot carry unambiguously — refusing rather \
                 than emitting a rule that means something else (fail-closed). Use a hostname, \
                 or an IPv4 address or CIDR."
            )));
        }
        // Only a trailing all-digit segment is a port.
        let (host_part, port) = match raw.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                // Out of range is an error, never a silent widening: `.ok()` here
                // turned `example.com:99999` into `allow@example.com`, which is
                // *every* port — the opposite of what the entry asked for.
                let port = p.parse::<u16>().map_err(|_| {
                    H5iError::Metadata(format!(
                        "net.egress entry '{raw}' has a port outside 1-65535 — refusing rather \
                         than falling back to an any-port rule (fail-closed)."
                    ))
                })?;
                (h, Some(port))
            }
            _ => (raw, None),
        };
        let lower = host_part.to_ascii_lowercase();
        let (host, wildcard) = match lower.strip_prefix("*.").or_else(|| lower.strip_prefix('.')) {
            Some(rest) => (rest.to_string(), true),
            None => (lower, false),
        };
        if host.is_empty() {
            continue;
        }
        let qualifier = match port {
            Some(p) => format!(":tcp:{p}"),
            None => String::new(),
        };
        if wildcard {
            if host.split('.').filter(|l| !l.is_empty()).count() < 2 {
                return Err(H5iError::Metadata(format!(
                    "net.egress wildcard '{raw}' covers a single label — a suffix rule must name \
                     at least two (e.g. '*.example.com', not '*.com'). Refusing (fail-closed)."
                )));
            }
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
    let mut s = String::from(
        "#!/bin/sh\n\
         # h5i microvm env preload — generated per run, never checked in.\n\
         # Values live here rather than in `msb run`'s argv, which /proc publishes.\n",
    );
    for (key, value) in env {
        s.push_str(&format!("export {key}='{}'\n", value.replace('\'', r"'\''")));
    }
    s.push_str("exec \"$@\"\n");
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
    let env_dir = work.parent().ok_or_else(|| {
        H5iError::Metadata(format!(
            "workspace '{}' has no parent env directory to stage the preload script in",
            work.display()
        ))
    })?;
    let dir = env_dir.join("microvm");
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    // pid **and** sequence, matching `SandboxGuard::new`: a pid alone repeats
    // across invocations, and two runs inside one process would share a name.
    let path = dir.join(format!(
        "preload-{}-{}.sh",
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
        f.write_all(preload_script(env).as_bytes())
            .map_err(|e| H5iError::with_path(e, &path))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, preload_script(env)).map_err(|e| H5iError::with_path(e, &path))?;
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
        // The guest rootfs is the image; only the mounts below are shared with
        // the host, and the workspace is the only one writable by default.
        "--mount-dir".into(),
        format!("{}:{WORK_MOUNT}:rw", work.display()),
        "--workdir".into(),
        WORK_MOUNT.into(),
    ];

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
    if let Some(ms) = plan.managed_settings {
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
    match plan.net {
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
        SandboxGuard {
            bin: bin.to_string(),
            name: format!(
                "h5i-{}-{}",
                std::process::id(),
                RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        }
    }

    fn remove(&self) {
        let _ = std::process::Command::new(&self.bin)
            .args(["remove", "--force", &self.name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        self.remove();
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
    let rt = runtime_or_refuse()?;
    let image = image_or_refuse(&policy.profile)?;
    check_mount_paths(policy, work)?;

    let net = net_plan(policy)?;
    let preload = write_preload(work, &guest_env(policy, injected_env))?;
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
    use std::io::Read;
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", full.join(" "))))?;

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b);
        b
    });
    let err_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b);
        b
    });

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
        std::env::set_var("H5I_MICROVM_TEST_VAR", "from-host");
        let env = guest_env(&pol, &[("H5I_MICROVM_TEST_VAR".into(), "brokered".into())]);
        std::env::remove_var("H5I_MICROVM_TEST_VAR");
        assert_eq!(
            env,
            vec![("H5I_MICROVM_TEST_VAR".to_string(), "brokered".to_string())]
        );
    }
}
