//! macOS confinement: the *Seatbelt* backend for the `process` and `supervised`
//! tiers, counterpart to the Linux stack in [`crate::sandbox`].
//!
//! macOS has no Landlock, seccomp, namespaces, cgroups or nftables. It has
//! Seatbelt, default-deny over every operation class at once and able to
//! *subtract*, which Landlock cannot. Subtraction earns its keep in one place,
//! the agent-config lockdown: `$WORK` read-write with `$WORK/.claude` unwritable
//! is one rule here and a bind plus remount-ro in a private mount namespace on
//! Linux.
//!
//! | property                  | Linux                          | macOS (here)                                   |
//! |---------------------------|--------------------------------|------------------------------------------------|
//! | filesystem allowlist      | Landlock                       | SBPL `(deny default)` + `file-read*`/`file-write*` |
//! | subtracting from a grant  | impossible (bind + remount-ro) | one `(deny …)` rule, last match wins            |
//! | syscall deny-list         | seccomp-bpf                    | *absent*                                      |
//! | egress allowlist          | netns + nftables + slirp4netns | loopback-only + host allowlist proxy            |
//! | net deny                  | empty network namespace        | `(deny network*)`                               |
//! | pid isolation             | PID namespace + private procfs | partial: `(deny process-info*)` for non-self    |
//! | per-env path redirects    | bind mounts in a mount ns      | symlinks + runtime env vars (see [`plan`])      |
//! | memory cap                | cgroup `memory.max` + rlimit   | *not enforceable* (see [`RESOURCE_NOTE`])     |
//! | cpu / fsize / nproc caps  | rlimits                        | rlimits (same)                                  |
//!
//! [`probe`] reports that right-hand column honestly, so a caller adapts to what
//! the host enforces rather than to a tier name.
//!
//! `sandbox-exec`, not `sandbox_init(3)`: the latter parses the profile, which
//! allocates, and allocating in a forked child of a tokio process is the classic
//! malloc-lock deadlock. `sandbox-exec` also `execvp`s in the same process, so
//! the pid, the exit status and the process-group SIGKILL all still work. The
//! profile goes by file, never argv, which `ps` publishes.
//!
//! The module compiles on every Unix so the generator and plan translation are
//! tested on Linux CI; only [`probe`] is platform-aware, and it fails closed.

// Compiled on all Unix targets so the pure logic is covered by the Linux test
// job; the exec paths are only *called* from the macOS dispatch arms.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::H5iError;
use crate::sandbox_policy::{NetMode, ResolvedPolicy};

/// The system Seatbelt launcher. Present on every macOS install since 10.5.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Why a memory cap is not claimed on macOS. `RLIMIT_AS`/`RLIMIT_RSS` are the
/// same resource on Darwin and the kernel does not enforce either against
/// mmap-backed allocation, which is where every modern runtime's heap lives,
/// so setting it would produce a limit that silently does nothing. A real
/// memory cap on macOS needs the container tier (the VM has cgroups).
pub const RESOURCE_NOTE: &str =
    "memory and process-count caps are not enforceable at the macOS kernel tiers: Darwin has no \
     cgroups, does not enforce RLIMIT_AS against mmap, and scopes RLIMIT_NPROC to the whole uid \
     rather than to one process tree (so applying it would cap the operator's machine, not the \
     box). cpu and fsize rlimits and the wall clock still apply. Use isolation=container for \
     memory or process limits.";

// ─── capability probe ────────────────────────────────────────────────────────

/// What Seatbelt can actually do on this host.
#[derive(Debug, Clone)]
pub struct SeatbeltCaps {
    /// `/usr/bin/sandbox-exec` exists and is executable.
    pub present: bool,
    /// A trivial command actually ran under a `(deny default)` profile. The
    /// functional check, not just the binary being on disk.
    pub functional: bool,
    /// Why it isn't usable, when it isn't.
    pub detail: Option<String>,
}

impl SeatbeltCaps {
    /// Fail-closed: usable only when both the binary is there and the
    /// functional self-test passed.
    pub fn usable(&self) -> bool {
        self.present && self.functional
    }

    fn unavailable(detail: &str) -> SeatbeltCaps {
        SeatbeltCaps {
            present: false,
            functional: false,
            detail: Some(detail.to_string()),
        }
    }
}

/// Probe Seatbelt. Memoized per process, exactly like the Linux kernel probe:
/// the answer cannot change while we run, and the functional self-test spawns a
/// process we don't want to repeat on every policy resolution.
pub fn probe() -> SeatbeltCaps {
    static CAPS: std::sync::OnceLock<SeatbeltCaps> = std::sync::OnceLock::new();
    CAPS.get_or_init(probe_uncached).clone()
}

#[cfg(target_os = "macos")]
fn probe_uncached() -> SeatbeltCaps {
    let present = Path::new(SANDBOX_EXEC).is_file();
    if !present {
        return SeatbeltCaps::unavailable(
            "/usr/bin/sandbox-exec not found (Seatbelt is required for the macOS kernel tiers)",
        );
    }
    // Functional self-test: run `true` under a minimal deny-default profile. A
    // host where Seatbelt is present but the profile is rejected (a future macOS
    // that drops SBPL, an MDM policy) must refuse the tier, not claim it.
    match std::process::Command::new(SANDBOX_EXEC)
        .arg("-p")
        .arg(SELF_TEST_PROFILE)
        .arg("/usr/bin/true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => SeatbeltCaps {
            present: true,
            functional: true,
            detail: None,
        },
        Ok(s) => SeatbeltCaps {
            present: true,
            functional: false,
            detail: Some(format!(
                "sandbox-exec self-test exited {:?} — Seatbelt is present but not applying \
                 profiles on this host",
                s.code()
            )),
        },
        Err(e) => SeatbeltCaps {
            present: true,
            functional: false,
            detail: Some(format!("sandbox-exec self-test failed to run: {e}")),
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn probe_uncached() -> SeatbeltCaps {
    SeatbeltCaps::unavailable("Seatbelt is macOS-only")
}

/// The smallest profile that still proves Seatbelt is applying rules: read-only
/// everywhere, exec allowed, everything else denied. Kept on one line because it
/// is passed with `-p` (this one has no secrets in it, unlike a real profile).
const SELF_TEST_PROFILE: &str =
    "(version 1)(deny default)(allow file-read* file-read-metadata)(allow process-exec*)\
     (allow process-fork)(allow sysctl-read)(allow mach-lookup)(allow signal (target self))";

// ─── SBPL generation ─────────────────────────────────────────────────────────

/// Knobs the caller supplies that are not part of the policy itself.
#[derive(Debug, Clone, Default)]
pub struct SeatbeltOptions {
    /// Loopback ports of h5i's own host-side proxies (the egress allowlist proxy
    /// and/or the credential-injecting auth proxy). When non-empty the box may
    /// dial exactly these `127.0.0.1` ports and nothing else. That is how the
    /// `supervised` tier gets a real domain allowlist on macOS.
    pub proxy_ports: Vec<u16>,
    /// Interactive agent session: adds the agent-config write lockdown.
    pub interactive: bool,
    /// The operator's real `HOME`, for the user-scope config lock and for
    /// resolving `~`-relative deny rules.
    pub home: Option<PathBuf>,
}

/// The macOS translation of one resolved policy: the SBPL text plus the two
/// things that stand in for Linux's bind mounts. Symlinks and runtime env
/// vars. See [`plan`].
#[derive(Debug, Clone, Default)]
pub struct SeatbeltPlan {
    /// The generated SBPL profile.
    pub profile: String,
    /// Env vars the child must get for a redirect to take effect (`TMPDIR`,
    /// `CLAUDE_CONFIG_DIR`, `CODEX_HOME`). Applied like brokered secrets: after
    /// the `env.pass` allowlist, so a host var cannot shadow them.
    pub env: Vec<(String, String)>,
    /// `(link, target)` pairs to materialize before the run: the workspace-relative
    /// private paths, which Linux gets via bind mounts.
    pub symlinks: Vec<(PathBuf, PathBuf)>,
    /// The workspace root every `symlinks` link must stay inside. Carried so
    /// `apply_symlinks` can create the parents without following a repo-planted
    /// symlink out of the box.
    pub work: PathBuf,
    /// Redirects with no macOS equivalent, reported rather than silently dropped
    /// (h5i never downgrades quietly). Surfaced by `env probe` and the run's
    /// diagnostics.
    pub unmapped: Vec<String>,
}

/// Structural read grants every macOS process needs before it can even start:
/// the dyld shared cache, the system frameworks, and the paths `execve` walks.
/// These are not policy. They are the macOS equivalent of the implicit `/proc`
/// re-grant the Linux path performs inside the PID namespace, and they carry no
/// user data.
const MACOS_SYSTEM_READ: &[&str] = &[
    "/System",
    "/usr/lib",
    "/usr/share",
    "/usr/bin",
    "/usr/sbin",
    "/bin",
    "/sbin",
    "/Library/Apple",
    "/Library/Frameworks",
    "/Library/Preferences/.GlobalPreferences.plist",
    // Where Xcode records that its licence was accepted. The `/usr/bin` tool
    // shims refuse to run without it ("You have not agreed to the Xcode license
    // agreements"), so a box that cannot read this cannot run `git` even with
    // the toolchain itself granted. A system preference file, no user data.
    "/Library/Preferences/com.apple.dt.Xcode.plist",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/private/var/select",
    "/private/etc/localtime",
    "/private/etc/protocols",
    "/private/etc/services",
    "/private/etc/ssl",
    "/private/etc/passwd",
    "/private/etc/group",
    "/private/etc/hosts",
    "/private/etc/resolv.conf",
];

/// Where `xcode-select` records the active developer directory. A symlink, so
/// the active toolchain is resolvable with a `readlink` rather than an exec.
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// Standard developer directories, used when [`XCODE_SELECT_LINK`] is absent
/// (it is only written once `xcode-select` has run).
const MACOS_DEVELOPER_DIRS: &[&str] = &[
    "/Applications/Xcode.app/Contents/Developer",
    "/Library/Developer/CommandLineTools",
];

/// Read grants for the active Xcode / Command Line Tools toolchain.
///
/// Not a convenience. On macOS the tools in `/usr/bin` (`git`, `python3`,
/// `clang`, `make`) are *shims*: each loads `libxcrun.dylib` from the active
/// developer directory and re-execs the real binary from there. Granting
/// `/usr/bin` alone therefore buys nothing, and a box denied this path could not
/// run `git` at all:
///
/// ```text
/// xcrun: error: unable to load libxcrun (… file system sandbox blocked open())
/// ```
///
/// Read-only, and the same category as [`MACOS_SYSTEM_READ`]: a system toolchain
/// carrying no user data, and the counterpart of the `/usr/bin` and `/usr/lib`
/// grants that make the equivalent tools work on Linux.
fn macos_developer_reads() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: PathBuf| {
        // A full Xcode's developer dir is `Xcode.app/Contents/Developer`, but
        // the shim also loads DVTSystemPrerequisites from the bundle's
        // `Contents/SharedFrameworks`, one level *outside* it. Granting only
        // the developer dir gets past `libxcrun` and then fails with "unable to
        // locate xcodebuild", so the grant is the bundle root when there is one.
        let p = app_bundle_root(&p).unwrap_or(p);
        let s = p.display().to_string();
        if p.is_dir() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Ok(active) = std::fs::read_link(XCODE_SELECT_LINK) {
        push(active);
    }
    for d in MACOS_DEVELOPER_DIRS {
        push(PathBuf::from(d));
    }
    out
}

/// The enclosing `.app` bundle of `path`, if it is inside one. Pure, so the
/// truncation rule is testable without an Xcode install.
fn app_bundle_root(path: &Path) -> Option<PathBuf> {
    let mut acc = PathBuf::new();
    for part in path.components() {
        acc.push(part);
        if acc
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        {
            return Some(acc);
        }
    }
    None
}

/// Device nodes every program opens. Kept apart from [`MACOS_SYSTEM_READ`]
/// because they are files, so they are emitted with `literal` rather than
/// `subpath`.
const MACOS_DEV_NODES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/dtracehelper",
    "/dev/tty",
    "/dev/fd",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/ptmx",
];

/// Character devices every shell pipeline writes to. Granted `file-write*` (not
/// just data) so `>` truncation works; they reveal nothing.
const MACOS_DEV_WRITE: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/tty",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/fd",
    "/dev/ptmx",
    // libSystem opens this for *writing* during process startup. Granting only
    // read is not enough: the box gets `deny(1) file-write-data
    // /dev/dtracehelper` and dies before it reaches main().
    "/dev/dtracehelper",
];

/// `sysctl` names that leak another process's argv and environment. `(deny
/// default)` already blocks them, but the base profile allows `sysctl-read`
/// wholesale (runtimes read `hw.*` constantly), so these are subtracted back
/// out. This is the macOS stand-in for the Linux private-procfs jail: it is what
/// stops a box from reading h5i's own environment, and therefore the operator's
/// secrets, out from under the `env.pass` allowlist.
const SYSCTL_PROCESS_LEAKS: &[&str] = &["kern.procargs", "kern.procargs2"];

/// Escape a string for an SBPL literal. SBPL string syntax is Scheme's: double
/// quotes, backslash escapes. A path we cannot represent is rejected by the
/// caller rather than silently mangled.
fn sbpl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// Every path Seatbelt might see for `path`.
///
/// This matters more on macOS than it looks. Seatbelt matches against the
/// *resolved* path, and the macOS root is full of firmlinks: `/tmp`, `/var`
/// and `/etc` all resolve under `/private`. A `(subpath "/tmp")` rule alone
/// therefore matches nothing at all for a file the process opened as
/// `/tmp/x`: the check sees `/private/tmp/x`. Emitting both forms is what
/// makes the profile actually enforce what it says.
fn path_aliases(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    for prefix in ["/tmp", "/var", "/etc"] {
        if path == prefix || path.starts_with(&format!("{prefix}/")) {
            out.push(format!("/private{path}"));
        }
    }
    // The inverse: a policy that already names /private/... also matches the
    // firmlinked spelling, which is what a shell in the box will type.
    if let Some(rest) = path.strip_prefix("/private")
        && ["/tmp", "/var", "/etc"]
            .iter()
            .any(|p| rest == *p || rest.starts_with(&format!("{p}/")))

    {
        out.push(rest.to_string());
    }
    out
}

/// Render one `(<verb> (subpath "…") …)` rule, expanding firmlink aliases.
/// Returns `None` when there is nothing to emit, so callers never write an
/// empty rule (SBPL rejects `(allow file-read*)` with an empty filter list only
/// in some macOS versions: an empty rule is also just noise).
fn rule(verb: &str, kind: &str, paths: &[String]) -> Option<String> {
    let mut seen = BTreeSet::new();
    for p in paths {
        for alias in path_aliases(p) {
            seen.insert(alias);
        }
    }
    if seen.is_empty() {
        return None;
    }
    let filters: Vec<String> = seen
        .iter()
        .map(|p| format!("({kind} \"{}\")", sbpl_escape(p)))
        .collect();
    Some(format!("({verb}\n  {}\n)", filters.join("\n  ")))
}

/// Expand a `~`-prefixed policy path against `home`. Mirrors
/// `sandbox::expand_tilde`, kept here so the generator stays pure.
fn expand_home(path: &str, home: Option<&Path>) -> Option<String> {
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|h| h.join(rest).display().to_string());
    }
    if path == "~" {
        return home.map(|h| h.display().to_string());
    }
    // `$REPO`-relative deny entries are a repo-scoped lint on every tier; they
    // are not absolute and cannot become an SBPL rule.
    if path.starts_with('$') {
        return None;
    }
    Some(path.to_string())
}

/// `fs.deny` entries this host cannot turn into an SBPL rule.
///
/// [`expand_home`] answers `None` for a `~`-relative path when `HOME` is unset,
/// and [`build_profile`] then leaves it out of the deny block. For a *grant*
/// that is fail-closed, the box losing access it was offered. For a *deny* it is
/// fail-open, and this is the one file whose header says `fs.deny` is "genuinely
/// enforced" here rather than being the lint it is on Linux: a profile granting
/// `/Users/dev` and denying `~/.ssh` handed the box the key material, and
/// `validate_profile`'s containment lint did not catch it either, because its
/// own `expand_tilde` leaves `~/.ssh` unexpanded under the same condition.
///
/// `$`-prefixed entries are excluded on purpose: `$REPO`-relative denies are a
/// repo-scoped lint on every tier and were never meant to be SBPL rules.
fn unexpandable_denies(p: &crate::sandbox_policy::Profile, home: Option<&Path>) -> Vec<String> {
    p.fs_deny
        .iter()
        .filter(|d| !d.starts_with('$'))
        .filter(|d| expand_home(d, home).is_none())
        .cloned()
        .collect()
}

/// Build the SBPL profile for `policy` running in `work`.
///
/// Rule order is load-bearing: SBPL is last match wins, so the file is laid
/// out as base allows → policy grants → network → *denies last*. That is what
/// lets `fs.deny` be genuinely enforced here when on Linux it is only a lint.
pub fn build_profile(policy: &ResolvedPolicy, work: &Path, opts: &SeatbeltOptions) -> String {
    let p = &policy.profile;
    let home = opts.home.as_deref();
    let mut s = String::new();

    s.push_str("(version 1)\n\n");
    s.push_str(";; Generated by h5i - do not edit. Profile for isolation=");
    s.push_str(policy.claim.as_str());
    s.push_str(", profile=");
    s.push_str(&p.name);
    // Denials are logged to the system log by default, which is how an operator
    // finds out *why* something in the box got EPERM. Do not add `(with
    // no-log)`: it would silence exactly the diagnostic that makes a
    // deny-default profile debuggable.
    s.push_str("\n\n(deny default)\n\n");

    // ── base: what any program needs to start at all ──────────────────────
    s.push_str(";; --- base process plumbing -------------------------------------\n");
    s.push_str("(allow process-fork)\n");
    s.push_str("(allow process-exec*)\n");
    // Signals only within our own sandbox: the box can manage its own children,
    // and cannot signal host processes. This is the reachable part of what the
    // Linux PID namespace gives for free.
    s.push_str("(allow signal (target same-sandbox))\n");
    s.push_str("(allow process-info* (target self))\n");
    s.push_str("(allow process-info-pidinfo (target same-sandbox))\n");
    // Path resolution needs metadata on every ancestor of a granted path.
    // Granting it globally leaks directory existence and timestamps, never
    // content. The same trade Chromium's renderer profile makes.
    s.push_str("(allow file-read-metadata)\n");
    s.push_str("(allow sysctl-read)\n");
    // dyld, malloc, notifyd, the bootstrap server. Restricting this to a service
    // list is brittle across macOS releases; it is a documented residual.
    s.push_str("(allow mach-lookup)\n");
    s.push_str("(allow ipc-posix-shm)\n");
    // Opt-in, `browser` only: looking a service up is not the same as
    // registering one, and a multi-process browser does both. See
    // [`Profile::mach_iokit`] for what this widens and how it was established.
    if p.mach_iokit {
        s.push_str("(allow mach-register)\n");
        s.push_str("(allow iokit-open)\n");
    }
    s.push('\n');

    // ── filesystem: reads ─────────────────────────────────────────────────
    s.push_str(";; --- filesystem: read grants -----------------------------------\n");
    let mut reads: Vec<String> = MACOS_SYSTEM_READ.iter().map(|s| s.to_string()).collect();
    reads.extend(macos_developer_reads());
    reads.extend(p.fs_read.iter().filter_map(|r| expand_home(r, home)));
    // The worktree is always readable; it is the point of the box.
    reads.push(work.display().to_string());
    if let Some(r) = rule("allow file-read*", "subpath", &reads) {
        s.push_str(&r);
        s.push('\n');
    }
    // The root directory itself, and *only* itself. Process startup reads `/`,
    // and no `(subpath "/usr")`-style grant covers it, so without this the box
    // gets `deny(1) file-read-data /` and dies before main(). Silently, since
    // Seatbelt reports denials only to the system log.
    //
    // `literal`, emphatically not `subpath`: `(subpath "/")` would grant the
    // entire filesystem and undo the whole profile. The two differ by one word
    // and by everything else.
    s.push_str("(allow file-read* (literal \"/\"))\n");
    // Device nodes are files, not directories: name them with `literal` so the
    // rule says what it means (a `subpath` of a character device is a category
    // error even where the matcher tolerates it).
    if let Some(r) = rule(
        "allow file-read*",
        "literal",
        &MACOS_DEV_NODES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    ) {
        s.push_str(&r);
        s.push('\n');
    }
    // Read-only cache offers: Linux binds them read-only, here the grant *is*
    // read-only because no write rule ever names them.
    let ro: Vec<String> = policy
        .ro_binds
        .iter()
        .map(|b| b.backing.display().to_string())
        .collect();
    if let Some(r) = rule("allow file-read*", "subpath", &ro) {
        s.push_str(";; warm dependency caches (read-only)\n");
        s.push_str(&r);
        s.push('\n');
    }
    s.push('\n');

    // ── filesystem: writes ────────────────────────────────────────────────
    s.push_str(";; --- filesystem: write grants ----------------------------------\n");
    let mut writes: Vec<String> = Vec::new();
    if !policy.work_readonly {
        writes.push(work.display().to_string());
    }
    for w in &p.fs_write {
        if w == "$WORK" {
            continue; // handled above, and honours work_readonly
        }
        if let Some(w) = expand_home(w, home) {
            writes.push(w);
        }
    }
    // The per-env backings for private paths / HOME state are already on
    // `fs_write` (core rewrites the grant to the backing), so they need no
    // special case here. Only the *redirect* does, which `plan` handles.
    if let Some(b) = &policy.cache_write {
        writes.push(b.backing.display().to_string());
    }
    if let Some(spool) = &policy.env_capture_spool {
        writes.push(spool.display().to_string());
    }
    // `file-write*` and `file-read*` are independent operation classes in SBPL:
    // granting write does NOT confer read. Landlock's `AccessFs::from_all` does
    // both at once, so a policy's `fs.write` entry means read-write everywhere
    // else in h5i, and it must mean the same here. Otherwise a box could write
    // its own cache and not read it back.
    if let Some(r) = rule("allow file-write* file-read*", "subpath", &writes) {
        s.push_str(&r);
        s.push('\n');
    }
    if let Some(r) = rule(
        "allow file-write* file-read*",
        "literal",
        &MACOS_DEV_WRITE.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    ) {
        s.push_str(&r);
        s.push('\n');
    }
    // Terminals: an interactive session inherits the operator's, and a program in
    // the box may allocate its own pty pair. Neither lives under any path grant. A
    // captured run reaches none of this: it gets `setsid`, so it has no
    // controlling terminal and `/dev/tty` cannot be opened at all.
    //
    // `file-ioctl` is a *separate* SBPL operation: `file-write*` does not imply
    // it, so without this rule every terminal ioctl returns EPERM under `(deny
    // default)`. Not a cosmetic gap. An interactive zsh puts itself in its own
    // process group and calls `tcsetpgrp` to make that group the terminal's
    // foreground one; the EPERM surfaces as
    //
    //     zsh: can't set tty pgrp: operation not permitted
    //
    // and leaves the shell in a *background* process group, where reading the
    // terminal raises SIGTTIN and no line ever reaches it. The box prompt renders
    // and keystrokes echo, so the session looks alive while `ls` does nothing. The
    // same EPERM denies `tcsetattr`, so raw mode is gone too.
    //
    // The grant names exactly the nodes the two rules above it name, so the ioctl
    // reach can never exceed the path reach. `/dev/tty` is spelled out even though
    // `^/dev/tty[a-z0-9]*$` already subsumes it: it is the node a program actually
    // opens, and a reader comparing the three rules should see one node set.
    // `/dev/ptmx` is not optional, `^/dev/pty…$` does not match it (`ptm` ≠ `pty`),
    // and without it `posix_openpt`'s `TIOCPTYGRANT`/`TIOCPTYUNLK` fail.
    //
    // This is the only rule the generator emits with more than one filter clause;
    // `every_sbpl_construct_we_emit_is_accepted` hands it to the real parser.
    if opts.interactive {
        s.push_str("(allow file-write* file-read* (regex #\"^/dev/tty[a-z0-9]*$\"))\n");
        s.push_str("(allow file-write* file-read* (regex #\"^/dev/pty[a-z0-9]*$\"))\n");
        s.push_str(
            "(allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/ptmx\") \
             (regex #\"^/dev/tty[a-z0-9]*$\") (regex #\"^/dev/pty[a-z0-9]*$\"))\n",
        );
    }
    s.push('\n');

    // ── network ───────────────────────────────────────────────────────────
    s.push_str(";; --- network ---------------------------------------------------\n");
    s.push_str(&network_rules(policy, opts));
    s.push('\n');

    // ── denies (last: they must win over every grant above) ───────────────
    s.push_str(";; --- denies (last match wins) ----------------------------------\n");
    // Another process's argv/environ. See SYSCTL_PROCESS_LEAKS.
    for name in SYSCTL_PROCESS_LEAKS {
        s.push_str(&format!("(deny sysctl-read (sysctl-name \"{name}\"))\n"));
    }
    s.push_str("(deny process-info* (target others))\n");
    // TIOCSTI (`_IOW('t', 114, char)` = 0x80017472) pushes a byte into the
    // terminal's *input* queue rather than reading or writing it. An interactive
    // session shares the operator's terminal, so a box able to issue it would be
    // typing at the host shell, running a command outside the box after the
    // session ends. The tty grant above is what puts that ioctl in reach, so the
    // subtraction ships with it. (Darwin appears to refuse TIOCSTI under a
    // deny-default profile even without this rule; containment must not rest on
    // an observation of undocumented behaviour. A pty proxy, which would stop
    // handing the box the operator's terminal at all, is the airtight fix.)
    s.push_str("(deny file-ioctl (ioctl-command #x80017472))\n");

    // Defence in depth, not a new capability: `validate_profile` refuses a
    // policy whose granted parent contains a denied child, so anything left in
    // `fs.deny` is already outside every grant and `(deny default)` covers it.
    // Emitting it anyway costs nothing and means the profile states the intent
    // explicitly, so a reader of the generated SBPL sees the same list the
    // policy does.
    let denies: Vec<String> = p.fs_deny.iter().filter_map(|d| expand_home(d, home)).collect();
    if let Some(r) = rule("deny file-read* file-write*", "subpath", &denies) {
        s.push_str(";; profile fs.deny (restated; already outside every grant)\n");
        s.push_str(&r);
        s.push('\n');
    }

    // Agent-config lockdown. Linux needs a bind + remount-ro in a private mount
    // namespace to get this; here it is one rule, and it also covers the
    // create-a-settings.local.json bypass because a denied subpath cannot be
    // written into at all.
    if opts.interactive {
        let locks: Vec<String> = crate::sandbox::config_lock_paths(work, home)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        if let Some(r) = rule("deny file-write*", "subpath", &locks) {
            s.push_str(";; agent config lockdown (the in-box hook cannot be disabled)\n");
            s.push_str(&r);
            s.push('\n');
        }
    }

    // The real HOME paths a per-env copy stands in for. The copy is granted
    // above; the original must be unreachable, or the redirect would be a
    // convenience rather than an isolation boundary.
    let shadowed: Vec<String> = policy
        .home_binds
        .iter()
        .filter(|b| b.target != Path::new("/tmp"))
        .map(|b| b.target.display().to_string())
        .collect();
    if let Some(r) = rule("deny file-read* file-write*", "subpath", &shadowed) {
        s.push_str(";; real HOME state shadowed by this env's private copy\n");
        s.push_str(&r);
        s.push('\n');
    }

    s
}

/// The network section. Three shapes, matching the three things a policy can
/// ask for.
fn network_rules(policy: &ResolvedPolicy, opts: &SeatbeltOptions) -> String {
    let p = &policy.profile;
    let mut s = String::new();

    // `AF_UNIX` is a separate grant from IP egress on both platforms: it passes
    // file descriptors, which is authority smuggling. Scoped by the filesystem
    // rules above, exactly as the Linux gate documents.
    if p.unix_sockets {
        s.push_str("(allow network-outbound (remote unix-socket))\n");
        s.push_str("(allow network-bind (local unix-socket))\n");
    }

    // Serving is not egress. A dev server in the box must be able to bind
    // loopback and accept the host's connection. On Linux that happens inside
    // the box's own netns; here it is the host's loopback, so it is spelled out.
    s.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
    s.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");

    // The box's own browser, and nothing else on loopback. The arms below refuse
    // loopback wholesale because here it is the *host's*; this lets exactly the
    // one port h5i told Chrome to listen on through, and leaves every other
    // loopback service, h5i's own proxies included, as unreachable as before.
    let mut ports: Vec<u16> = policy.loopback_ports.clone();
    ports.sort_unstable();
    ports.dedup();
    for port in ports {
        s.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }

    match p.net_mode {
        NetMode::Host => {
            s.push_str("(allow network-outbound)\n");
        }
        NetMode::Deny => {
            if opts.proxy_ports.is_empty() {
                // Airtight. Note this is stricter than Linux `net.mode=deny`,
                // which gives the box a private loopback it may dial; here
                // loopback is the *host's*, so dialing it would reach host
                // services (including h5i's own proxies). Fail closed.
                s.push_str(";; no outbound at all (loopback is the host's - see module docs)\n");
            } else {
                // The supervised tier's egress allowlist: the ONLY outbound
                // destinations are h5i's own proxies, which enforce the domain
                // list and are DNS-pinned. The box needs no DNS of its own, the
                // proxy resolves, so name resolution is denied too, which is a
                // stronger position than the Linux nftables path.
                let mut ports: Vec<u16> = opts.proxy_ports.clone();
                ports.sort_unstable();
                ports.dedup();
                for port in ports {
                    s.push_str(&format!(
                        "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
                    ));
                }
            }
        }
    }
    s
}

// ─── plan: the macOS stand-in for bind mounts ────────────────────────────────

/// Translate the policy's bind-mount redirects into what macOS can actually do,
/// and say plainly what it cannot.
///
/// Linux shadows a path by bind-mounting over it in a private mount namespace.
/// macOS has no unprivileged bind mount and no mount namespace, so each redirect
/// is re-expressed as whichever of these preserves its *purpose*:
///
/// - *private paths* (`target/`, `.next/`) exist so concurrent envs of one repo
///   do not fight over a single build-cache inode. A symlink from the worktree
///   path to the per-env backing achieves that, and the backing is already the
///   granted path.
/// - *HOME state* (`~/.claude`, `~/.codex`) exists so boxes do not race on
///   shared credential files. Both runtimes honour an explicit config directory
///   (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`), which points them at the per-env copy,
///   and the real one is denied outright by [`build_profile`].
/// - *private `/tmp`* becomes `TMPDIR`, which every well-behaved macOS program
///   honours.
/// - *read-only caches* need no redirect at all: the grant is read-only because
///   nothing grants write to it.
///
/// Anything that does not fit lands in [`SeatbeltPlan::unmapped`] and is
/// reported, never dropped in silence.
pub fn plan(policy: &ResolvedPolicy, work: &Path, opts: &SeatbeltOptions) -> SeatbeltPlan {
    // NOTE: the `/usr/bin` tool shims cache their toolchain lookup in the
    // *host's* per-user temp dir, located with
    // `confstr(_CS_DARWIN_USER_TEMP_DIR)` rather than `TMPDIR`, so the box's
    // redirect cannot move it and the write is denied. Every shimmed
    // `git`/`python3` call prints "couldn't create cache file …" on stderr and
    // still succeeds. No variable suppresses it: `XCRUN_NO_CACHE=1` changes
    // nothing, and granting write into the host's shared temp dir would hand
    // every box a drop point host processes read.
    let mut env: Vec<(String, String)> = Vec::new();
    let mut symlinks: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut unmapped: Vec<String> = Vec::new();

    for b in &policy.private_binds {
        symlinks.push((work.join(&b.rel), b.backing.clone()));
    }

    for b in &policy.home_binds {
        let target = b.target.as_path();
        let backing = b.backing.display().to_string();
        if target == Path::new("/tmp") {
            env.push(("TMPDIR".into(), backing));
            continue;
        }
        match target.file_name().and_then(|n| n.to_str()) {
            Some(".claude") => env.push(("CLAUDE_CONFIG_DIR".into(), backing)),
            Some(".codex") => env.push(("CODEX_HOME".into(), backing)),
            // `~/.claude.json` lives beside `~/.claude`, and setting
            // `CLAUDE_CONFIG_DIR` is expected to move it along with the
            // directory, so no separate redirect is emitted. Not verified on a
            // real Mac. If it turns out Claude still reads the real
            // `~/.claude.json`, the failure is loud and safe rather than quiet:
            // the real path is denied by the profile (core rewrote the grant to
            // the per-env copy), so the box gets a permission error instead of
            // silently sharing host state with another box.
            Some(".claude.json") => {}
            _ => unmapped.push(format!(
                "HOME state '{}' has no macOS redirect (no bind mounts); the per-env copy at {} \
                 is granted and the real path is denied, but the program will not find it there",
                target.display(),
                b.backing.display()
            )),
        }
    }

    for b in &policy.ro_binds {
        // Same path on both sides means the grant alone does the job.
        if b.backing != b.target {
            unmapped.push(format!(
                "read-only cache is offered at its host path {} rather than {} (macOS cannot bind \
                 mount); set the tool's cache dir if it must appear at the conventional path",
                b.backing.display(),
                b.target.display()
            ));
        }
    }

    let profile = build_profile(policy, work, opts);
    SeatbeltPlan {
        profile,
        env,
        symlinks,
        unmapped,
        work: work.to_path_buf(),
    }
}

/// Materialize the plan's symlinks. Fail-closed in both directions: a redirect
/// we set out to make and could not is an error rather than a silent run against
/// the shared path, and a redirect that would *destroy* existing work is also an
/// error rather than a silent `rm -rf`.
///
/// That second guard is the one Linux does not need. A bind mount merely
/// *shadows* whatever is at the mountpoint and leaves it intact underneath; a
/// symlink has to replace it. So an empty directory is replaced without comment,
/// and anything with contents in it stops the run.
fn apply_symlinks(plan: &SeatbeltPlan) -> Result<(), H5iError> {
    for (link, target) in &plan.symlinks {
        // Already pointing where it should. Envs are re-run constantly.
        if std::fs::read_link(link).is_ok_and(|existing| existing == *target) {
            continue;
        }
        let meta = std::fs::symlink_metadata(link).ok();
        match meta {
            // A stale symlink (a previous env's backing): ours to replace.
            Some(m) if m.file_type().is_symlink() => {
                std::fs::remove_file(link).map_err(|e| H5iError::with_path(e, link))?;
            }
            Some(m) if m.is_dir() => {
                let empty = std::fs::read_dir(link)
                    .map_err(|e| H5iError::with_path(e, link))?
                    .next()
                    .is_none();
                if !empty {
                    return Err(H5iError::Metadata(format!(
                        "private_paths: '{}' already exists and is not empty. macOS has no bind \
                         mounts, so h5i redirects it with a symlink to this env's own backing \
                         ({}), which would replace what is there. Move or delete it first — \
                         h5i will not discard it for you.",
                        link.display(),
                        target.display()
                    )));
                }
                std::fs::remove_dir(link).map_err(|e| H5iError::with_path(e, link))?;
            }
            Some(_) => {
                return Err(H5iError::Metadata(format!(
                    "private_paths: '{}' exists and is a file, not a directory — refusing to \
                     replace it with the redirect to {}",
                    link.display(),
                    target.display()
                )));
            }
            None => {}
        }
        // Create the parents component by component rather than with
        // `create_dir_all`, which would happily follow a repo-planted symlink
        // at any level and plant this redirect, a box-writable path, outside
        // the workspace entirely.
        let rel = link.strip_prefix(&plan.work).map_err(|_| {
            H5iError::Metadata(format!(
                "private_paths: redirect '{}' is not inside the workspace '{}' (fail-closed)",
                link.display(),
                plan.work.display()
            ))
        })?;
        crate::sandbox::prepare_private_link_site(&plan.work, &rel.to_string_lossy())?;
        std::os::unix::fs::symlink(target, link).map_err(|e| H5iError::with_path(e, link))?;
    }
    Ok(())
}

/// Tell the operator about any redirect this platform could not express.
///
/// These are not errors: the *grant* is still correct, the per-env backing being
/// what the profile allows and the real path denied, so nothing has been
/// widened. What is lost is the program finding the backing at the conventional
/// path, a behaviour difference from Linux, and h5i's rule is that a difference
/// the operator would otherwise discover as a mystery gets said out loud.
/// Printed once per distinct message per process.
fn report_unmapped(plan: &SeatbeltPlan) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SEEN: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    for msg in &plan.unmapped {
        let fresh = seen
            .lock()
            .map(|mut s| s.insert(msg.clone()))
            .unwrap_or(true);
        if fresh {
            eprintln!("note: {msg}");
        }
    }
}

// ─── the profile file ────────────────────────────────────────────────────────

/// A generated profile on disk, removed when the run ends. The profile names
/// every path the box may touch, so it is written 0600 and never passed on the
/// command line where `ps` would show it.
pub struct ProfileFile {
    path: PathBuf,
}

impl ProfileFile {
    fn write(profile: &str) -> Result<ProfileFile, H5iError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // In a private 0700 scratch dir with an unguessable name, and created
        // with `create_new` + 0600 rather than written at the umask default and
        // narrowed afterwards. The doc above claimed 0600; `fs::write` gave it
        // 0644 first, and the predictable name meant a pre-existing file (or a
        // symlink) at that path was followed and overwritten. The profile
        // enumerates every path the box may touch, so on a shared TMPDIR that
        // window is both a disclosure and a write primitive.
        let dir = crate::sandbox::private_scratch_dir("h5i-seatbelt")?;
        let path = dir.join(format!("{seq}.sb"));
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| H5iError::with_path(e, &path))?;
            f.write_all(profile.as_bytes())
                .map_err(|e| H5iError::with_path(e, &path))?;
        }
        #[cfg(not(unix))]
        std::fs::write(&path, profile).map_err(|e| H5iError::with_path(e, &path))?;
        Ok(ProfileFile { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProfileFile {
    fn drop(&mut self) {
        // The profile now lives in its own private scratch dir, so take the
        // directory with it rather than leaving an empty one behind per run.
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

// ─── confined execution ──────────────────────────────────────────────────────

/// Build the confined `Command` for `argv`: `sandbox-exec -f <profile> -- argv`,
/// with the environment allowlist, the plan's redirect vars, and the rlimits
/// applied in `pre_exec`.
///
/// The returned [`ProfileFile`] must be held until the child has been spawned:
/// `sandbox-exec` reads it at startup, and dropping it early would delete the
/// profile out from under the exec.
pub fn build_confined_command(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    opts: &SeatbeltOptions,
) -> Result<(std::process::Command, ProfileFile, SeatbeltPlan), H5iError> {
    use std::os::unix::process::CommandExt;

    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    let caps = probe();
    if !caps.usable() {
        return Err(H5iError::Metadata(format!(
            "macOS Seatbelt confinement is not available on this host — refusing (h5i never \
             silently downgrades): {}",
            caps.detail.unwrap_or_else(|| "unknown".into())
        )));
    }
    // A deny that cannot be expressed must stop the run, not be left out of the
    // profile. See [`unexpandable_denies`].
    let dropped = unexpandable_denies(&policy.profile, opts.home.as_deref());
    if !dropped.is_empty() {
        return Err(H5iError::Metadata(format!(
            "profile fs.deny lists {dropped:?}, which is '~'-relative while $HOME is unset —              there is nothing to resolve it against, so the deny cannot become a Seatbelt rule.              Refusing rather than running with it silently dropped (fail-closed): set HOME, or              spell the path absolutely."
        )));
    }
    let work = work
        .canonicalize()
        .map_err(|e| H5iError::with_path(e, work))?;

    let plan = plan(policy, &work, opts);
    apply_symlinks(&plan)?;
    report_unmapped(&plan);
    let profile_file = ProfileFile::write(&plan.profile)?;

    let p = &policy.profile;
    let mut cmd = std::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-f").arg(profile_file.path()).arg("--");
    cmd.args(argv).current_dir(&work);

    // Environment allowlist. Nothing inherited wholesale, same rule as Linux.
    cmd.env_clear();
    for key in &p.env_pass {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // Redirect vars, then brokered secrets: neither may be shadowed by a host
    // var that happened to be on the allowlist.
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    for (k, v) in injected_env {
        cmd.env(k, v);
    }

    let nproc = p.max_procs;
    let fsize = p.fsize_bytes;
    let cpu = p.cpu_secs;
    let interactive = opts.interactive;

    // Only raw syscalls here. This runs in a forked child of a multi-threaded
    // process, so nothing may allocate or take a lock (the same discipline the
    // Linux path keeps).
    unsafe {
        cmd.pre_exec(move || {
            use std::io::Error;

            // Own session so the wall-clock kill reaps the whole tree via
            // killpg. Interactive sessions keep the caller's session or job
            // control breaks, exactly as on Linux.
            if !interactive && libc::setsid() == -1 {
                return Err(Error::last_os_error());
            }
            // Deliberately NOT setting RLIMIT_NPROC. On Darwin it caps processes
            // per *real uid*, not per process tree, so a per-box `max_procs` of
            // 256 is really a cap on everything the operator is running. On any
            // Mac with a browser open the box then cannot fork at all, and the
            // failure surfaces as `fork: Resource temporarily unavailable` from
            // the shell, a long way from its cause. A limit that constrains the
            // host instead of the box is worse than no limit, so `max_procs` is
            // reported as unenforceable here rather than applied.
            let _ = nproc;
            if let Some(bytes) = fsize {
                let lim = libc::rlimit {
                    rlim_cur: bytes,
                    rlim_max: bytes,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            if let Some(secs) = cpu {
                let lim = libc::rlimit {
                    rlim_cur: secs,
                    rlim_max: secs,
                };
                if libc::setrlimit(libc::RLIMIT_CPU, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            // No memory rlimit: see RESOURCE_NOTE. Setting one on Darwin would
            // be a limit that reads as enforced and is not.
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &core);
            Ok(())
        });
    }

    Ok((cmd, profile_file, plan))
}

/// Captured run at the macOS kernel tiers. The wall clock, the process-group
/// kill, and the `rusage` accounting are the shared ones from
/// [`crate::sandbox`]; only the confinement mechanism differs.
pub fn run(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    opts: &SeatbeltOptions,
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    let (cmd, _profile, _plan) = build_confined_command(policy, work, argv, injected_env, opts)?;
    // `_profile` is held across the spawn inside wait_with_deadline: sandbox-exec
    // reads it before it execs the workload.
    crate::sandbox::wait_with_deadline(cmd, policy.profile.wall(), argv, None)
}

/// Interactive (agent-in-box) session at the macOS kernel tiers: stdio
/// inherited, no wall clock (the operator bounds the session).
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    opts: &SeatbeltOptions,
) -> Result<i32, H5iError> {
    let (mut cmd, _profile, _plan) =
        build_confined_command(policy, work, argv, injected_env, opts)?;
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("confined session failed to start: {e}")))?;
    let status = child.wait().map_err(H5iError::Io)?;
    Ok(status.code().unwrap_or(130))
}

/// Long-lived background service at the macOS kernel tiers: no wall clock, own
/// session so a later `killpg` reaps the tree. Returns the child pid, which is
/// the workload's own, because `sandbox-exec` `execve`s rather than forking.
pub fn spawn_background(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    out: std::fs::File,
    err: std::fs::File,
    opts: &SeatbeltOptions,
) -> Result<u32, H5iError> {
    let (mut cmd, profile, _plan) = build_confined_command(policy, work, argv, injected_env, opts)?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    let child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("confined service failed to start: {e}")))?;
    let pid = child.id();
    // A captured run holds the profile file until the child exits; a service
    // outlives this call, so the file has to be released here, but not
    // immediately. `spawn` returns once `sandbox-exec` has *exec'd*; reading and
    // applying the profile happens just after that, concurrently with us. Hand
    // the guard to a detached thread that outwaits the window, so the caller
    // still returns at once and the file is never yanked mid-parse.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        drop(profile);
    });
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox_policy::{HomeBind, IsolationClaim, PrivateBind, Profile, RoBind};

    fn policy(claim: IsolationClaim) -> ResolvedPolicy {
        ResolvedPolicy::new(claim, Profile::builtin("test", claim))
    }

    fn opts() -> SeatbeltOptions {
        SeatbeltOptions {
            proxy_ports: Vec::new(),
            interactive: false,
            home: Some(PathBuf::from("/Users/dev")),
        }
    }

    #[test]
    fn profile_is_deny_default_and_grants_work() {
        let p = policy(IsolationClaim::Process);
        let s = build_profile(&p, Path::new("/Users/dev/repo/.h5i/env/a/work"), &opts());
        assert!(s.starts_with("(version 1)"), "{s}");
        assert!(s.contains("(deny default)"));
        assert!(
            s.contains("(subpath \"/Users/dev/repo/.h5i/env/a/work\")"),
            "the worktree must be granted\n{s}"
        );
    }

    #[test]
    fn firmlinked_paths_get_both_spellings() {
        // A /tmp rule that does not also name /private/tmp enforces nothing:
        // Seatbelt matches the resolved path.
        assert_eq!(path_aliases("/tmp"), vec!["/tmp", "/private/tmp"]);
        assert_eq!(
            path_aliases("/etc/ssl"),
            vec!["/etc/ssl", "/private/etc/ssl"]
        );
        assert_eq!(
            path_aliases("/private/var/x"),
            vec!["/private/var/x", "/var/x"]
        );
        assert_eq!(path_aliases("/usr/lib"), vec!["/usr/lib"]);
    }

    #[test]
    fn a_write_grant_also_grants_read() {
        // SBPL treats file-read* and file-write* as independent classes, so a
        // write-only rule would leave a box able to write its own cache and not
        // read it back. Every other tier's `fs.write` means read-write.
        let mut p = policy(IsolationClaim::Process);
        p.profile.fs_write.push("/Users/dev/.cache".to_string());
        let s = build_profile(&p, Path::new("/Users/dev/w"), &opts());
        let writes = s
            .split(";; --- filesystem: write grants")
            .nth(1)
            .unwrap()
            .split(";; --- network")
            .next()
            .unwrap();
        assert!(
            writes.contains("(allow file-write* file-read*\n"),
            "write grants must carry read too\n{writes}"
        );
        assert!(writes.contains("(subpath \"/Users/dev/.cache\")"), "{writes}");
    }

    #[test]
    fn denials_are_left_loggable() {
        // `(with no-log)` would silence the only signal an operator has for why
        // something in the box got EPERM.
        let p = policy(IsolationClaim::Process);
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(s.contains("(deny default)"));
        assert!(!s.contains("no-log"), "{s}");
    }

    #[test]
    fn work_readonly_drops_the_write_grant() {
        let mut p = policy(IsolationClaim::Process);
        p.work_readonly = true;
        let work = Path::new("/Users/dev/repo/w");
        let s = build_profile(&p, work, &opts());
        let writes = s
            .split(";; --- filesystem: write grants")
            .nth(1)
            .unwrap()
            .split(";; --- network")
            .next()
            .unwrap();
        assert!(
            !writes.contains("/Users/dev/repo/w\""),
            "a read-only session must not grant write on the worktree\n{writes}"
        );
    }

    /// The deny block is what makes `fs.deny` real on macOS rather than the
    /// lint it is on Linux, and a `~`-relative entry with no `$HOME` to resolve
    /// it against was simply left out of it. A profile granting `/Users/dev`
    /// and denying `~/.ssh` therefore handed the box the key material:
    /// `validate_profile`'s containment lint misses it for the same reason (its
    /// `expand_tilde` leaves the path unexpanded too, so the prefix test finds
    /// no overlap).
    #[test]
    fn a_deny_that_cannot_be_expressed_stops_the_run() {
        let mut p = policy(IsolationClaim::Process);
        p.profile.fs_read.push("/Users/dev".to_string());
        p.profile.fs_deny = vec!["~/.ssh".to_string()];

        // With a home, it becomes a rule and nothing is dropped.
        assert!(unexpandable_denies(&p.profile, Some(Path::new("/Users/dev"))).is_empty());
        // Without one, there is nothing to resolve it against.
        assert_eq!(
            unexpandable_denies(&p.profile, None),
            vec!["~/.ssh".to_string()]
        );

        // `$REPO`-relative entries are a repo-scoped lint on every tier and were
        // never SBPL rules, so they are not what this refuses over.
        p.profile.fs_deny = vec!["$REPO/.env".to_string()];
        assert!(unexpandable_denies(&p.profile, None).is_empty());
    }

    #[test]
    fn fs_deny_is_enforced_and_comes_last() {
        let mut p = policy(IsolationClaim::Process);
        p.profile.fs_read.push("/Users/dev".to_string());
        p.profile.fs_deny = vec!["~/.ssh".to_string()];
        let s = build_profile(&p, Path::new("/Users/dev/w"), &opts());
        let deny_at = s.find("(deny file-read* file-write*").expect("deny rule");
        let allow_at = s.find("(allow file-read*").expect("allow rule");
        assert!(
            deny_at > allow_at,
            "SBPL is last-match-wins, so the deny must follow the grant"
        );
        assert!(s.contains("(subpath \"/Users/dev/.ssh\")"), "{s}");
    }

    #[test]
    fn repo_relative_deny_entries_are_skipped_not_mangled() {
        // `$REPO/.git/hooks` is a repo-scoped lint on every tier; it has no
        // absolute form and must not become a bogus rule.
        assert_eq!(expand_home("$REPO/.git/hooks", Some(Path::new("/h"))), None);
        assert_eq!(
            expand_home("~/.aws", Some(Path::new("/h"))),
            Some("/h/.aws".to_string())
        );
    }

    #[test]
    fn net_deny_emits_no_outbound_rule() {
        let p = policy(IsolationClaim::Process);
        assert_eq!(p.profile.net_mode, NetMode::Deny);
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(!s.contains("(allow network-outbound)"), "{s}");
        assert!(!s.contains("(allow network-outbound (remote ip"), "{s}");
        // Serving still works. That is not egress.
        assert!(s.contains("(allow network-bind (local ip \"localhost:*\"))"));
    }

    #[test]
    fn egress_allows_only_the_proxy_port() {
        let p = policy(IsolationClaim::Supervised);
        let o = SeatbeltOptions {
            proxy_ports: vec![51234],
            ..opts()
        };
        let s = build_profile(&p, Path::new("/w"), &o);
        assert!(s.contains("(allow network-outbound (remote ip \"localhost:51234\"))"));
        assert!(!s.contains("(allow network-outbound)\n"), "{s}");
    }

    #[test]
    fn host_net_allows_outbound() {
        let mut p = policy(IsolationClaim::Workspace);
        p.profile.net_mode = NetMode::Host;
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(s.contains("(allow network-outbound)"));
    }

    #[test]
    fn unix_sockets_are_a_separate_opt_in() {
        let mut p = policy(IsolationClaim::Supervised);
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(!s.contains("remote unix-socket"), "off by default\n{s}");
        p.profile.unix_sockets = true;
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(s.contains("(allow network-outbound (remote unix-socket))"));
    }

    #[test]
    fn only_the_browsers_own_cdp_port_is_dialable_on_loopback() {
        let mut p = policy(IsolationClaim::Supervised);
        // Off by default: loopback is the *host's* here, so a box that could
        // dial it would reach host services, h5i's own proxies among them.
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(
            !s.contains("(allow network-outbound (remote ip \"localhost:"),
            "no loopback egress without a pinned port\n{s}"
        );

        p.loopback_ports = vec![49999, 49999, 3000];
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(s.contains("(allow network-outbound (remote ip \"localhost:3000\"))"), "{s}");
        // Deduplicated: a port declared in the profile and also live as a
        // service must not emit the rule twice.
        assert_eq!(
            s.matches("(allow network-outbound (remote ip \"localhost:49999\"))").count(),
            1,
            "duplicate port emitted twice\n{s}"
        );

        p.loopback_ports = vec![49999];
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(
            s.contains("(allow network-outbound (remote ip \"localhost:49999\"))"),
            "the box's own browser port must be dialable\n{s}"
        );
        // Exactly one port, not the interface.
        assert_eq!(
            s.matches("(allow network-outbound (remote ip \"localhost:").count(),
            1,
            "granted more than the one port\n{s}"
        );
        assert!(!s.contains("(allow network-outbound)\n"), "{s}");
    }

    #[test]
    fn mach_register_and_iokit_are_a_separate_opt_in() {
        let mut p = policy(IsolationClaim::Supervised);
        let s = build_profile(&p, Path::new("/w"), &opts());
        // `mach-lookup` is base plumbing every program needs; registering a
        // service and opening IOKit are not, and stay off unless asked for.
        assert!(s.contains("(allow mach-lookup)"), "base plumbing\n{s}");
        assert!(!s.contains("mach-register"), "off by default\n{s}");
        assert!(!s.contains("iokit-open"), "off by default\n{s}");

        p.profile.mach_iokit = true;
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(s.contains("(allow mach-register)"), "{s}");
        assert!(s.contains("(allow iokit-open)"), "{s}");
    }

    #[test]
    fn process_environ_leak_is_denied() {
        let p = policy(IsolationClaim::Process);
        let s = build_profile(&p, Path::new("/w"), &opts());
        // The macOS stand-in for the private-procfs jail: without this the box
        // reads h5i's own environment and the env.pass allowlist is moot.
        assert!(s.contains("(deny sysctl-read (sysctl-name \"kern.procargs2\"))"));
        assert!(s.contains("(deny process-info* (target others))"));
        let allow_at = s.find("(allow sysctl-read)").unwrap();
        let deny_at = s.find("kern.procargs2").unwrap();
        assert!(deny_at > allow_at, "the subtraction must come after");
    }

    /// Job control is what makes a box shell usable, and it rests on one
    /// operation the write grant does not confer. Without `file-ioctl` the
    /// session's `tcsetpgrp` fails, zsh warns "can't set tty pgrp" and is left in
    /// a background process group where no typed line ever reaches it.
    #[test]
    fn interactive_session_may_drive_its_terminal() {
        let p = policy(IsolationClaim::Supervised);
        let s = build_profile(
            &p,
            Path::new("/w"),
            &SeatbeltOptions {
                interactive: true,
                ..opts()
            },
        );
        let ioctl = s
            .find("(allow file-ioctl")
            .unwrap_or_else(|| panic!("interactive sessions need tty ioctls\n{s}"));
        // Every node the interactive session already holds read/write on, and no
        // other: an ioctl grant wider than the path grant it accompanies would
        // be reach this rule never intended to add. `/dev/ptmx` is the one that
        // needs naming: `^/dev/pty…$` does not match it (`ptm` ≠ `pty`), and it
        // is granted by literal above for the same reason.
        let rule = s[ioctl..].lines().next().unwrap();
        for f in [
            "(literal \"/dev/tty\")",
            "(literal \"/dev/ptmx\")",
            "(regex #\"^/dev/tty[a-z0-9]*$\")",
            "(regex #\"^/dev/pty[a-z0-9]*$\")",
        ] {
            assert!(rule.contains(f), "tty ioctl grant misses {f}\n{rule}");
        }
        // TIOCSTI types at the *host* shell through the shared terminal; the
        // subtraction must come after the grant, or last-match-wins re-allows it.
        let deny = s.find("(deny file-ioctl (ioctl-command #x80017472))").unwrap();
        assert!(deny > ioctl, "the TIOCSTI subtraction must come after\n{s}");

        // A captured run has no terminal to drive (it gets its own session), so
        // it gets neither the tty paths nor the ioctl.
        let s = build_profile(&p, Path::new("/w"), &opts());
        assert!(!s.contains("(allow file-ioctl"), "captured runs get no tty ioctl\n{s}");
        assert!(!s.contains("/dev/tty[a-z0-9]"), "captured runs get no tty paths\n{s}");
    }

    #[test]
    fn interactive_locks_agent_config() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path();
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        let p = policy(IsolationClaim::Supervised);
        let o = SeatbeltOptions {
            interactive: true,
            ..opts()
        };
        let s = build_profile(&p, work, &o);
        assert!(
            s.contains("(deny file-write*"),
            "the interactive session must lock agent config\n{s}"
        );
        assert!(s.contains(&format!("(subpath \"{}\")", work.join(".claude").display())));
    }

    #[test]
    fn the_root_is_granted_as_a_literal_and_never_as_a_subpath() {
        // `(allow file-read* (literal "/"))` grants the root directory entry,
        // which process startup needs. `(allow file-read* (subpath "/"))`
        // grants the entire filesystem and quietly turns the sandbox into
        // nothing. One word apart, so assert the difference rather than trust
        // it. A fix for "the box will not start" is exactly the situation in
        // which someone reaches for the wrong one.
        for claim in [IsolationClaim::Process, IsolationClaim::Supervised] {
            let s = build_profile(&policy(claim), Path::new("/Users/dev/w"), &opts());
            assert!(
                s.contains("(allow file-read* (literal \"/\"))"),
                "the root entry must be readable or nothing starts:\n{s}"
            );
            assert!(
                !s.contains("(subpath \"/\")"),
                "granting subpath / would expose the whole filesystem:\n{s}"
            );
        }
    }

    #[test]
    fn startup_device_nodes_are_writable() {
        // /dev/dtracehelper is opened for write by libSystem at startup; a
        // read-only grant is not enough and the failure is a silent death.
        let s = build_profile(&policy(IsolationClaim::Process), Path::new("/w"), &opts());
        let writes = s.split(";; --- network").next().unwrap();
        assert!(
            writes.contains("(literal \"/dev/dtracehelper\")"),
            "/dev/dtracehelper must be write-granted:\n{writes}"
        );
    }

    #[test]
    fn the_generated_profile_is_pure_ascii() {
        // Not a style rule. libsandbox's SBPL parser is C, and a non-ASCII byte
        // anywhere in the profile, including inside a comment, makes it `abort()`,
        // so `sandbox-exec` dies on SIGABRT having printed nothing at all. A
        // single em dash in the generated header was enough to make every macOS
        // run fail silently. Keep the whole document ASCII.
        //
        // Runs on every platform: this is a property of what we generate, and
        // catching it here is far cheaper than catching it on a Mac.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        for claim in [IsolationClaim::Process, IsolationClaim::Supervised] {
            for interactive in [false, true] {
                for ports in [vec![], vec![8080u16]] {
                    let mut p = policy(claim);
                    p.profile.unix_sockets = true;
                    p.home_binds.push(HomeBind {
                        backing: PathBuf::from("/envs/a/home/.claude"),
                        target: PathBuf::from("/Users/dev/.claude"),
                    });
                    let o = SeatbeltOptions {
                        proxy_ports: ports,
                        interactive,
                        home: Some(PathBuf::from("/Users/dev")),
                    };
                    let s = build_profile(&p, tmp.path(), &o);
                    assert!(
                        s.is_ascii(),
                        "non-ASCII byte in the generated profile (libsandbox aborts on these): {:?}",
                        s.chars().filter(|c| !c.is_ascii()).collect::<String>()
                    );
                }
            }
        }
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(sbpl_escape(r#"/a"b\c"#), r#"/a\"b\\c"#);
    }

    #[test]
    fn private_paths_become_symlinks() {
        let mut p = policy(IsolationClaim::Process);
        p.private_binds.push(PrivateBind {
            backing: PathBuf::from("/envs/a/private/target"),
            rel: "target".to_string(),
        });
        let plan = plan(&p, Path::new("/w"), &opts());
        assert_eq!(
            plan.symlinks,
            vec![(
                PathBuf::from("/w/target"),
                PathBuf::from("/envs/a/private/target")
            )]
        );
        assert!(plan.unmapped.is_empty(), "{:?}", plan.unmapped);
    }

    #[test]
    fn home_state_becomes_runtime_config_vars() {
        let mut p = policy(IsolationClaim::Supervised);
        p.home_binds = vec![
            HomeBind {
                backing: PathBuf::from("/envs/a/home/.claude"),
                target: PathBuf::from("/Users/dev/.claude"),
            },
            HomeBind {
                backing: PathBuf::from("/envs/a/home/.claude.json"),
                target: PathBuf::from("/Users/dev/.claude.json"),
            },
            HomeBind {
                backing: PathBuf::from("/envs/a/tmp"),
                target: PathBuf::from("/tmp"),
            },
        ];
        let plan = plan(&p, Path::new("/w"), &opts());
        assert!(plan
            .env
            .contains(&("CLAUDE_CONFIG_DIR".into(), "/envs/a/home/.claude".into())));
        assert!(plan.env.contains(&("TMPDIR".into(), "/envs/a/tmp".into())));
        assert!(plan.unmapped.is_empty(), "{:?}", plan.unmapped);
        // ...and the real HOME state is denied, so the redirect is a boundary
        // rather than a default.
        assert!(plan
            .profile
            .contains("(subpath \"/Users/dev/.claude\")"));
    }

    #[test]
    fn codex_home_state_maps_too() {
        let mut p = policy(IsolationClaim::Supervised);
        p.home_binds = vec![HomeBind {
            backing: PathBuf::from("/envs/a/home/.codex"),
            target: PathBuf::from("/Users/dev/.codex"),
        }];
        let plan = plan(&p, Path::new("/w"), &opts());
        assert!(plan
            .env
            .contains(&("CODEX_HOME".into(), "/envs/a/home/.codex".into())));
    }

    #[test]
    fn an_unmappable_redirect_is_reported_not_dropped() {
        let mut p = policy(IsolationClaim::Supervised);
        p.home_binds = vec![HomeBind {
            backing: PathBuf::from("/envs/a/home/.gnupg"),
            target: PathBuf::from("/Users/dev/.gnupg"),
        }];
        let plan = plan(&p, Path::new("/w"), &opts());
        assert_eq!(plan.unmapped.len(), 1, "{:?}", plan.unmapped);
        assert!(plan.unmapped[0].contains(".gnupg"));
    }

    #[test]
    fn ro_cache_at_a_different_path_is_reported() {
        let mut p = policy(IsolationClaim::Process);
        p.ro_binds.push(RoBind {
            backing: PathBuf::from("/h5i/cache/cargo"),
            target: PathBuf::from("/Users/dev/.cargo/registry"),
        });
        let plan = plan(&p, Path::new("/w"), &opts());
        assert_eq!(plan.unmapped.len(), 1, "{:?}", plan.unmapped);
        // The grant is still emitted, and it is read-only because no write rule
        // names it.
        assert!(plan.profile.contains("(subpath \"/h5i/cache/cargo\")"));
        let writes = plan
            .profile
            .split(";; --- filesystem: write grants")
            .nth(1)
            .unwrap();
        assert!(!writes.contains("/h5i/cache/cargo"));
    }

    /// A helper that builds a plan with one private-path redirect from `link`
    /// to a fresh backing dir.
    fn symlink_plan(work: &Path, link: &Path, backing: &Path) -> SeatbeltPlan {
        SeatbeltPlan {
            symlinks: vec![(link.to_path_buf(), backing.to_path_buf())],
            work: work.to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn redirect_replaces_the_empty_mountpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("target");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&link).unwrap();
        std::fs::create_dir_all(&backing).unwrap();
        apply_symlinks(&symlink_plan(tmp.path(), &link, &backing)).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), backing);
    }

    #[test]
    fn redirect_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("target");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&backing).unwrap();
        let plan = symlink_plan(tmp.path(), &link, &backing);
        apply_symlinks(&plan).unwrap();
        apply_symlinks(&plan).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), backing);
    }

    #[test]
    fn redirect_refuses_to_discard_existing_work() {
        // Linux binds *over* a populated mountpoint and leaves it intact; a
        // symlink cannot. Refuse rather than delete the user's build output.
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("target");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&link).unwrap();
        std::fs::create_dir_all(&backing).unwrap();
        std::fs::write(link.join("artifact.o"), b"precious").unwrap();
        let err = apply_symlinks(&symlink_plan(tmp.path(), &link, &backing)).unwrap_err();
        assert!(err.to_string().contains("not empty"), "{err}");
        assert!(link.join("artifact.o").exists(), "must not have been deleted");
    }

    #[test]
    fn redirect_replaces_a_stale_link_from_another_env() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("target");
        let old = tmp.path().join("old-env");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&backing).unwrap();
        std::os::unix::fs::symlink(&old, &link).unwrap();
        apply_symlinks(&symlink_plan(tmp.path(), &link, &backing)).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), backing);
    }

    /// `private_paths` and the worktree are both repo-supplied. A branch that
    /// ships `a` as a symlink to somewhere outside the workspace would, with
    /// `create_dir_all`, have h5i plant the redirect at the link's target. A
    /// box-writable path on the operator's filesystem, e.g. a directory on
    /// their PATH. Refuse instead of following it.
    #[test]
    fn redirect_refuses_a_symlinked_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        let outside = tmp.path().join("outside");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&backing).unwrap();
        // The repo ships `a` as a symlink pointing out of the workspace.
        std::os::unix::fs::symlink(&outside, work.join("a")).unwrap();

        let link = work.join("a/bin");
        let err = apply_symlinks(&symlink_plan(&work, &link, &backing)).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(
            !outside.join("bin").exists(),
            "must not have created the redirect outside the workspace"
        );
    }

    /// The same walk still creates honest nested parents inside the workspace.
    #[test]
    fn redirect_creates_real_nested_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        let backing = tmp.path().join("backing");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&backing).unwrap();
        let link = work.join("a/b/cache");
        apply_symlinks(&symlink_plan(&work, &link, &backing)).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), backing);
    }

    /// SBPL is Scheme: an unbalanced paren means `sandbox-exec` rejects the
    /// whole profile, and the run then fails with a parse error rather than
    /// running unconfined, but it fails for every box, so catch it here. Only
    /// parens outside string literals count (a path may legitimately contain one).
    fn parens_balanced(s: &str) -> bool {
        let mut depth: i32 = 0;
        let mut in_str = false;
        let mut escaped = false;
        for c in s.chars() {
            match c {
                _ if escaped => escaped = false,
                '\\' if in_str => escaped = true,
                '"' => in_str = !in_str,
                '(' if !in_str => depth += 1,
                ')' if !in_str => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => {}
            }
        }
        depth == 0 && !in_str
    }

    #[test]
    fn every_profile_shape_is_balanced_sbpl() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        for claim in [
            IsolationClaim::Workspace,
            IsolationClaim::Process,
            IsolationClaim::Supervised,
        ] {
            for interactive in [false, true] {
                for ports in [vec![], vec![49211u16]] {
                    for net in [NetMode::Deny, NetMode::Host] {
                        let mut p = policy(claim);
                        p.profile.net_mode = net;
                        p.profile.unix_sockets = true;
                        p.home_binds.push(HomeBind {
                            backing: PathBuf::from("/envs/a/home/.claude"),
                            target: PathBuf::from("/Users/dev/.claude"),
                        });
                        p.ro_binds.push(RoBind {
                            backing: PathBuf::from("/h5i/cache/npm"),
                            target: PathBuf::from("/Users/dev/.npm"),
                        });
                        let o = SeatbeltOptions {
                            proxy_ports: ports.clone(),
                            interactive,
                            home: Some(PathBuf::from("/Users/dev")),
                        };
                        let s = build_profile(&p, tmp.path(), &o);
                        assert!(
                            parens_balanced(&s),
                            "unbalanced SBPL for {claim:?}/{interactive}/{ports:?}/{net:?}:\n{s}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unbalanced_input_is_actually_detected() {
        // Guard the guard: a balance check that always says yes proves nothing.
        assert!(parens_balanced("(allow (foo \"a)b\"))"));
        assert!(!parens_balanced("(allow (foo)"));
        assert!(!parens_balanced("(allow))"));
    }

    // ─── functional (macOS only) ─────────────────────────────────────────────
    //
    // Everything above tests the profile *text*. These four run commands under
    // the real generated profile and check what the kernel actually did, which
    // is the only evidence that any of this confines anything. A profile that
    // SBPL rejects, or one that over-restricts until nothing execs, would pass
    // every string test in this file.
    //
    // Every denial below is paired with a positive control in the *same* run.
    // Without that, a sandbox so broken that nothing runs at all reads as a
    // clean pass.

    /// Shared setup: a worktree and the built-in `process` policy over it.
    #[cfg(target_os = "macos")]
    fn functional_env() -> (tempfile::TempDir, PathBuf, ResolvedPolicy) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            crate::sandbox_policy::Profile::builtin("seatbelt-fn", IsolationClaim::Process),
        );
        (tmp, work, pol)
    }

    /// Run `script` under the real confinement and return its stdout+stderr.
    #[cfg(target_os = "macos")]
    fn run_confined_sh(pol: &ResolvedPolicy, work: &Path, script: &str) -> (Option<i32>, String) {
        let out = crate::sandbox::run(
            pol,
            work,
            &["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        )
        .expect("confined run should not error out");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.exit_code, text)
    }

    /// Every non-trivial SBPL construct [`build_profile`] emits, checked one at a
    /// time on top of a base profile already known to work.
    ///
    /// libsandbox does not report a bad profile the way a parser normally would:
    /// it `abort()`s, so `sandbox-exec` dies on SIGABRT with nothing on stderr,
    /// and a whole-profile failure tells you only that *something* in a few
    /// hundred lines is wrong. Checking each construct in isolation names the
    /// culprit, and pins the dialect we depend on.
    #[test]
    #[cfg(target_os = "macos")]
    fn every_sbpl_construct_we_emit_is_accepted() {
        if !probe().usable() {
            eprintln!("SKIP every_sbpl_construct_we_emit_is_accepted: Seatbelt unusable");
            return;
        }
        // Sanity: the base alone must work, or every result below is noise.
        assert!(
            sbpl_accepts(""),
            "the base self-test profile itself was rejected; nothing below is meaningful"
        );

        let constructs = [
            "(allow file-read-metadata)",
            "(allow file-read* (subpath \"/usr\"))",
            "(allow file-read* (literal \"/dev/null\"))",
            "(allow file-write* file-read* (subpath \"/tmp\"))",
            "(allow file-write* file-read* (literal \"/dev/null\"))",
            "(deny file-read* file-write* (subpath \"/Users/nobody/.ssh\"))",
            "(deny file-write* (subpath \"/tmp/x/.claude\"))",
            "(allow file-write* file-read* (regex #\"^/dev/tty[a-z0-9]*$\"))",
            "(allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/ptmx\") \
             (regex #\"^/dev/tty[a-z0-9]*$\") (regex #\"^/dev/pty[a-z0-9]*$\"))",
            "(deny file-ioctl (ioctl-command #x80017472))",
            "(allow process-fork)",
            "(allow process-exec*)",
            "(allow signal (target same-sandbox))",
            "(allow process-info* (target self))",
            "(allow process-info-pidinfo (target same-sandbox))",
            "(deny process-info* (target others))",
            "(allow sysctl-read)",
            "(deny sysctl-read (sysctl-name \"kern.procargs2\"))",
            "(allow mach-lookup)",
            "(allow ipc-posix-shm)",
            "(allow network-bind (local ip \"localhost:*\"))",
            "(allow network-inbound (local ip \"localhost:*\"))",
            "(allow network-outbound (remote ip \"localhost:8080\"))",
            "(allow network-outbound (remote unix-socket))",
            "(allow network-bind (local unix-socket))",
            "(allow network-outbound)",
        ];
        let rejected: Vec<&str> = constructs
            .iter()
            .copied()
            .filter(|c| !sbpl_accepts(c))
            .collect();
        assert!(
            rejected.is_empty(),
            "libsandbox rejected {} construct(s) this crate emits:\n  {}",
            rejected.len(),
            rejected.join("\n  ")
        );
    }

    /// Split a generated profile into its top-level items: each comment line on
    /// its own, each parenthesised form whole (they span lines).
    fn split_forms(profile: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        for line in profile.lines() {
            let t = line.trim();
            if cur.is_empty() && (t.is_empty() || t.starts_with(';')) {
                if !t.is_empty() {
                    out.push(line.to_string());
                }
                continue;
            }
            if !cur.is_empty() {
                cur.push('\n');
            }
            cur.push_str(line);
            if parens_balanced(&cur) {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    /// Is `profile` a *valid* document, regardless of whether the command it
    /// wraps can run? libsandbox signals an unusable profile by aborting, so a
    /// death by signal means invalid and any ordinary exit means the profile
    /// compiled (the exit code then only says whether `true` was permitted).
    #[cfg(target_os = "macos")]
    fn sbpl_document_is_valid(profile: &str) -> bool {
        use std::os::unix::process::ExitStatusExt;
        let Ok(file) = ProfileFile::write(profile) else {
            return false;
        };
        match std::process::Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(file.path())
            .arg("/usr/bin/true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(s) => s.signal().is_none(),
            Err(_) => false,
        }
    }

    /// When a run under the real profile dies, say what would have saved it.
    ///
    /// Bisecting *forms* was the wrong tool and gave a misleading answer, for a
    /// reason worth recording: macOS *kills* a process whose exec or library
    /// mapping the profile denies, so "died on a signal" does not distinguish
    /// "the profile is invalid" from "the profile compiled and forbade something
    /// the program needed". A prefix carrying `(allow process-exec*)` but no read
    /// grants dies exactly like a malformed profile does, which is how
    /// `(allow process-exec*)` got fingered as the culprit.
    ///
    /// So probe from the other side: append a candidate permission to the real
    /// profile and see whether the run starts working.
    #[test]
    #[cfg(target_os = "macos")]
    fn locate_what_the_real_profile_is_missing() {
        if !probe().usable() {
            eprintln!("SKIP locate_what_the_real_profile_is_missing: Seatbelt unusable");
            return;
        }
        let (_tmp, work, pol) = functional_env();
        let plan = plan(&pol, &work, &SeatbeltOptions::default());
        if runs_under(&plan.profile, &work) {
            return; // healthy: nothing to locate
        }
        // Each candidate is appended to the real profile; SBPL is last-match-wins
        // so an appended allow overrides the restriction under test.
        // Guessing roots did not converge: a blanket `(allow file-read*)`
        // repairs the run but no single root does, so either several paths are
        // missing or the denied operation is not subpath-expressible. Stop
        // guessing and read the denial out of the system log. MacOS records
        // every one of them, it just does not put them on stderr, which is the
        // whole reason this has been so opaque.
        let denials = recent_sandbox_denials();
        // Control: does a path-shaped blanket behave like the unfiltered one? If
        // `(subpath "/")` repairs it too, the gap is genuinely a set of paths.
        // If only the unfiltered form works, it is an operation with no path.
        let root_subpath_repairs = runs_under(
            &format!("{}\n(allow file-read* (subpath \"/\"))\n", plan.profile),
            &work,
        );
        panic!(
            "a command cannot run under the real profile.\n\
             `(allow file-read* (subpath \"/\"))` repairs it: {root_subpath_repairs}\n\
             --- sandbox denials from the system log ---\n{denials}\n\
             --- profile ---\n{}",
            plan.profile
        );
    }

    /// Sandbox denials macOS recorded in the last minute.
    ///
    /// This is the diagnostic that was missing all along. A Seatbelt denial is
    /// reported to the unified log as `deny(1) <operation> <path>` and to
    /// nowhere else (not stderr, not an exit code) so a confined program that
    /// dies gives no clue on its own. Reading the log turns "it died silently"
    /// into the exact operation and file.
    #[cfg(target_os = "macos")]
    fn recent_sandbox_denials() -> String {
        let out = std::process::Command::new("/usr/bin/log")
            .args([
                "show",
                "--last",
                "1m",
                "--style",
                "compact",
                "--predicate",
                "eventMessage CONTAINS \"deny(\"",
            ])
            .output();
        match out {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let lines: Vec<&str> = text.lines().filter(|l| l.contains("deny(")).collect();
                if lines.is_empty() {
                    format!(
                        "(no deny( lines; log exited {:?}, stderr: {})",
                        o.status.code(),
                        String::from_utf8_lossy(&o.stderr).chars().take(300).collect::<String>()
                    )
                } else {
                    // Newest last; keep the tail, which is our run.
                    lines
                        .iter()
                        .rev()
                        .take(40)
                        .rev()
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            Err(e) => format!("(could not run `log show`: {e})"),
        }
    }

    /// Can `/usr/bin/true` actually run to a clean exit under `profile`?
    /// Deliberately not "did it die on a signal": macOS kills on a denied exec,
    /// so only a successful exit distinguishes a working profile.
    #[cfg(target_os = "macos")]
    fn runs_under(profile: &str, cwd: &Path) -> bool {
        let Ok(file) = ProfileFile::write(profile) else {
            return false;
        };
        std::process::Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(file.path())
            .arg("/usr/bin/true")
            // The real path runs in $WORK, which is granted. Matching it here
            // keeps the cwd from becoming a difference between probe and
            // reality. Otherwise a root containing the test's own cwd would
            // look like the missing grant.
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Does `sandbox-exec` accept the known-good base profile plus `extra`?
    #[cfg(target_os = "macos")]
    fn sbpl_accepts(extra: &str) -> bool {
        let profile = format!("{SELF_TEST_PROFILE}{extra}");
        std::process::Command::new(SANDBOX_EXEC)
            .arg("-p")
            .arg(&profile)
            .arg("/usr/bin/true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Isolate "SBPL rejected the profile" from "the profile was fine and the
    /// command died". Those two look identical from a failed run, and telling
    /// them apart is the difference between a syntax bug and a grant bug, so
    /// this asserts the narrower property (the parser accepts what we generate)
    /// and surfaces `sandbox-exec`'s own diagnostic when it does not.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_generated_profile_is_accepted_by_sandbox_exec() {
        if !probe().usable() {
            eprintln!("SKIP the_generated_profile_is_accepted_by_sandbox_exec: Seatbelt unusable");
            return;
        }
        let (_tmp, work, pol) = functional_env();
        // Both option sets, because they generate *different* profiles: the
        // interactive one adds the tty grants and the agent-config lockdown, and a
        // rule the parser rejects takes the whole profile with it. Checking only
        // the captured shape would leave every interactive box shell resting on
        // `String::contains`.
        //
        // The interactive lockdown comes from `config_lock_paths(work, home)`,
        // which lists only paths that *exist*, so `home: None` yields an empty
        // lockdown and the rules production actually generates never reach the
        // parser. Create both scopes.
        let home = work.join("home");
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let home = Some(home);
        for opts in [
            SeatbeltOptions {
                home: home.clone(),
                ..SeatbeltOptions::default()
            },
            SeatbeltOptions {
                interactive: true,
                home: home.clone(),
                ..SeatbeltOptions::default()
            },
        ] {
            let interactive = opts.interactive;
            let plan = plan(&pol, &work, &opts);
            if interactive {
                // Guard the setup, not the parser: a profile that quietly lost
                // the tty grant or the lockdown would still compile, and this
                // test would pass while covering less than it says it does.
                for expected in [
                    "(allow file-ioctl",
                    "(deny file-ioctl (ioctl-command #x80017472))",
                    &format!("(subpath \"{}\")", work.join(".claude").display()),
                    &format!(
                        "(subpath \"{}\")",
                        work.join("home/.claude/settings.json").display()
                    ),
                ] {
                    assert!(
                        plan.profile.contains(expected),
                        "the interactive profile handed to the parser is missing {expected}\n{}",
                        plan.profile
                    );
                }
            }
            let file = ProfileFile::write(&plan.profile).unwrap();
            let out = std::process::Command::new(SANDBOX_EXEC)
                .arg("-f")
                .arg(file.path())
                .arg("/usr/bin/true")
                .output()
                .expect("run sandbox-exec");
            assert!(
                out.status.success(),
                "sandbox-exec rejected the generated profile (interactive={interactive}).\n\
                 status: {:?}\nstderr: {}\nstdout: {}\n--- profile ---\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout),
                plan.profile
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_confined_command_actually_execs_under_the_real_profile() {
        // The load-bearing one. If SBPL rejects the generated profile, or the
        // grants miss something dyld needs, nothing runs at all and every other
        // "denied" assertion here would be true for the wrong reason.
        if !probe().usable() {
            eprintln!("SKIP a_confined_command_actually_execs: Seatbelt unusable on this host");
            return;
        }
        let (_tmp, work, pol) = functional_env();
        let out = crate::sandbox::run(
            &pol,
            &work,
            &["/bin/sh".to_string(), "-c".to_string(), "echo alive".to_string()],
        )
        .expect("confined run should not error out");
        // `exit_code: None` means it died on a signal. Report enough to tell a
        // wall-clock kill from a sandbox kill from a crash, because an empty
        // stdout+stderr on its own says almost nothing.
        assert_eq!(
            out.exit_code,
            Some(0),
            "confined /bin/sh did not run cleanly.\n\
             exit_code={:?} timed_out={} wall_ms={}\nstdout: {:?}\nstderr: {:?}\n\
             --- sandbox denials from the system log ---\n{}",
            out.exit_code,
            out.timed_out,
            out.wall_ms,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            // Seatbelt reports denials only here. Without it this failure is a
            // signal number and two empty strings, which is what made the
            // original bug take several rounds to find.
            recent_sandbox_denials(),
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("alive"),
            "no output from the confined shell"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_confined_command_can_fork_and_exec_a_child() {
        // Distinct from the exec test above, which only runs shell builtins and
        // so never forks. `max_procs` used to be applied as RLIMIT_NPROC, which
        // on Darwin is scoped to the whole uid rather than to this process
        // tree, so on a machine where the operator already had a few hundred
        // processes the box could not fork at all, and said so only as
        // `fork: Resource temporarily unavailable`. Anything real the box does
        // (a compiler, a test runner, a package manager) forks constantly.
        if !probe().usable() {
            eprintln!("SKIP a_confined_command_can_fork_and_exec_a_child: Seatbelt unusable");
            return;
        }
        let (_tmp, work, pol) = functional_env();
        assert!(
            pol.profile.max_procs.is_some(),
            "the built-in profile should carry a max_procs for this to be a real check"
        );
        let (code, text) = run_confined_sh(&pol, &work, "/usr/bin/true && echo FORKED");
        assert_eq!(code, Some(0), "forking a child failed: {text}");
        assert!(text.contains("FORKED"), "child did not run: {text}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn writes_are_confined_to_the_worktree() {
        if !probe().usable() {
            eprintln!("SKIP writes_are_confined_to_the_worktree: Seatbelt unusable");
            return;
        }
        let (tmp, work, pol) = functional_env();
        // A sibling of $WORK: no grant names it, and it is not under /tmp (macOS
        // temp dirs live in /var/folders), so nothing else can be granting it.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let (_code, text) = run_confined_sh(
            &pol,
            &work,
            &format!(
                "echo in > inside.txt && echo INSIDE-OK; \
                 echo out > '{}/outside.txt' 2>/dev/null && echo OUTSIDE-WROTE || echo OUTSIDE-DENIED",
                outside.display()
            ),
        );
        // Positive control first: the box can write where it is supposed to.
        assert!(text.contains("INSIDE-OK"), "worktree must stay writable: {text}");
        assert!(work.join("inside.txt").exists(), "the write did not land: {text}");
        // ...and cannot write where it is not.
        assert!(text.contains("OUTSIDE-DENIED"), "write outside $WORK was allowed: {text}");
        assert!(
            !outside.join("outside.txt").exists(),
            "a file appeared outside $WORK — the filesystem boundary did not hold"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn net_deny_blocks_outbound_even_on_loopback() {
        if !probe().usable() {
            eprintln!("SKIP net_deny_blocks_outbound_even_on_loopback: Seatbelt unusable");
            return;
        }
        let nc = Path::new("/usr/bin/nc");
        if !nc.is_file() {
            eprintln!("SKIP net_deny_blocks_outbound_even_on_loopback: no /usr/bin/nc");
            return;
        }
        // A listener on host loopback. Deliberately loopback rather than a real
        // host: it needs no network in CI, and it is the case that actually
        // differs from Linux (a macOS box shares the host's loopback, so the
        // profile has to deny dialing it: a Linux box gets a private one).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Positive control, unconfined: the listener really is reachable, so a
        // later refusal means the sandbox refused it and not that the test
        // pointed at a dead port.
        let reachable = std::process::Command::new(nc)
            .args(["-z", "-w", "2", "127.0.0.1", &port.to_string()])
            .status()
            .expect("run nc unconfined");
        assert!(
            reachable.success(),
            "positive control failed: the listener must be reachable when unconfined"
        );

        let (_tmp, work, pol) = functional_env();
        assert_eq!(pol.profile.net_mode, NetMode::Deny);
        let (_code, text) = run_confined_sh(
            &pol,
            &work,
            &format!(
                "echo RAN; /usr/bin/nc -z -w 2 127.0.0.1 {port} && echo CONNECTED || echo BLOCKED"
            ),
        );
        // RAN proves the shell executed, so BLOCKED means the connect was
        // refused rather than the whole command failing to start.
        assert!(text.contains("RAN"), "the confined shell did not run: {text}");
        assert!(
            text.contains("BLOCKED") && !text.contains("CONNECTED"),
            "net.mode=deny let the box dial host loopback: {text}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn config_lock_subtracts_from_the_granted_worktree() {
        // The one place Seatbelt's ability to subtract genuinely buys something
        // Landlock cannot express: $WORK is granted read-write, and
        // $WORK/.claude must still be unwritable. Linux needs a bind plus a
        // remount-ro in a private mount namespace for this; here it is a rule,
        // and this proves the rule actually binds.
        if !probe().usable() {
            eprintln!("SKIP config_lock_subtracts_from_the_granted_worktree: Seatbelt unusable");
            return;
        }
        let (_tmp, work, pol) = functional_env();
        std::fs::create_dir_all(work.join(".claude")).unwrap();

        let opts = SeatbeltOptions {
            proxy_ports: Vec::new(),
            interactive: true, // interactive sessions get the config lockdown
            home: std::env::var_os("HOME").map(PathBuf::from),
        };
        let script = "echo x > sibling.txt && echo SIBLING-OK; \
                      echo y > .claude/settings.local.json 2>/dev/null && echo LOCK-BYPASSED \
                      || echo LOCK-HELD";
        let out = run(
            &pol,
            &work,
            &["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            &[],
            &opts,
        )
        .expect("confined run should not error out");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Control: the rest of the worktree is still writable, so the lock is a
        // subtraction and not a blanket read-only worktree.
        assert!(text.contains("SIBLING-OK"), "worktree must stay writable: {text}");
        assert!(
            text.contains("LOCK-HELD"),
            "the agent could create $WORK/.claude/settings.local.json — the config lock did not \
             hold, and an in-box agent could disable the observation hook: {text}"
        );
        assert!(!work.join(".claude/settings.local.json").exists());
    }

    #[test]
    fn probe_fails_closed_off_macos() {
        // The generator is portable; the mechanism is not. Nothing may claim
        // Seatbelt on a host that has none.
        #[cfg(not(target_os = "macos"))]
        {
            let c = probe();
            assert!(!c.usable());
            assert!(c.detail.is_some());
        }
    }
}
