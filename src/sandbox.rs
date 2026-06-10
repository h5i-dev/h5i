//! h5i's own confinement for the `process` isolation tier — plus the policy
//! model shared by every tier (docs/environments-design.md §5–§7).
//!
//! Design (mirrors a minimal, embedded Sandlock):
//!   - **Policy** comes from a checked-in `.h5i/env.toml` profile (fail-closed
//!     defaults when absent). A profile requests a *minimum* isolation claim;
//!     the resolved claim is recorded in the env manifest and every capture.
//!   - **Capability probing**: hosts vary wildly (this matters — Landlock may
//!     be compiled out, userns may be disabled). We probe what the kernel
//!     actually supports and **refuse** (never silently downgrade) when the
//!     requested claim cannot be satisfied.
//!   - **Enforcement** (Linux, `process` tier v1, static — no supervisor):
//!     Landlock filesystem allowlist (`$WORK` rw + ro system paths), a seccomp
//!     deny-list of dangerous syscalls, `unshare(CLONE_NEWUSER|CLONE_NEWNET)`
//!     for `net.mode = deny`, `PR_SET_NO_NEW_PRIVS`, and rlimits with a
//!     wall-clock kill. Domain egress allowlists (`net.egress`) need the
//!     seccomp-notify supervisor or a container backend and therefore **fail
//!     closed** under the static `process` tier.
//!
//! Cross-platform honesty: the `process` tier is Linux-only in this build;
//! macOS (Seatbelt) and Windows are explicitly not claimed (§5).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::H5iError;

/// Repo-relative path of the checked-in policy file.
pub const POLICY_FILE: &str = ".h5i/env.toml";

/// Default wall-clock limit when a profile sets none (fail-closed: a confined
/// command can never run unbounded).
pub const DEFAULT_WALL: Duration = Duration::from_secs(30 * 60);

// ─── isolation claims (§6) ──────────────────────────────────────────────────

/// Descriptive isolation *claims*, not "security tiers" — so we never
/// accidentally call Docker "secure". Ordered weakest → strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationClaim {
    /// git worktree only — file isolation, no confinement.
    Workspace,
    /// Our own Landlock + seccomp + netns confinement (this module).
    Process,
    /// Process tier + a live seccomp-notify **supervisor** and a netns+nftables
    /// L3/L4 egress guard (`docs/supervisor-design.md`). The first tier that may
    /// claim untrusted-code containment — and only when every component probes
    /// green; otherwise the claim is refused (fail-closed), never downgraded.
    Supervised,
    /// Rootless Podman adapter (opt-in shell-out).
    Container,
    /// gVisor / Kata adapter (not in this build).
    HardenedContainer,
    /// Firecracker adapter (not in this build).
    Microvm,
}

impl IsolationClaim {
    pub fn parse(s: &str) -> Result<Self, H5iError> {
        match s.trim().to_lowercase().as_str() {
            "workspace" => Ok(Self::Workspace),
            "process" => Ok(Self::Process),
            "supervised" => Ok(Self::Supervised),
            "container" => Ok(Self::Container),
            "hardened-container" | "hardened_container" => Ok(Self::HardenedContainer),
            "microvm" => Ok(Self::Microvm),
            other => Err(H5iError::Metadata(format!(
                "unknown isolation claim '{other}' (expected workspace|process|supervised|container|hardened-container|microvm)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Process => "process",
            Self::Supervised => "supervised",
            Self::Container => "container",
            Self::HardenedContainer => "hardened-container",
            Self::Microvm => "microvm",
        }
    }
}

/// `net.mode` — what the *static* `process` tier can honestly enforce (netns):
/// all-or-nothing. Domain allowlists live in `net.egress` and require a
/// supervisor or container backend (fail closed under `process`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    Deny,
    Host,
}

impl NetMode {
    pub fn parse(s: &str) -> Result<Self, H5iError> {
        match s.trim().to_lowercase().as_str() {
            "deny" => Ok(Self::Deny),
            "host" => Ok(Self::Host),
            other => Err(H5iError::Metadata(format!(
                "unknown net.mode '{other}' (process-v1 enforces deny|host only)"
            ))),
        }
    }
}

// ─── policy profile (§7) ────────────────────────────────────────────────────

/// A fully-resolved policy profile — every field explicit, suitable for
/// serializing as `policy.resolved.toml` and digesting. Field order is the
/// canonical serialization order (digest stability depends on it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub isolation: IsolationClaim,
    /// Landlock read-only GRANTS (allowlist) — system paths. `$WORK` is
    /// implicitly readable+writable and need not be listed.
    pub fs_read: Vec<String>,
    /// Landlock read-write GRANTS. `$WORK` expands to the env workspace.
    pub fs_write: Vec<String>,
    /// NOT a kernel rule (Landlock is allowlist-only): a preflight lint +
    /// secret-scrub scope. The policy is refused if any granted parent
    /// contains one of these.
    pub fs_deny: Vec<String>,
    pub net_mode: NetMode,
    /// Domain allowlist — requires supervisor/container backend; fails closed
    /// under the static `process` tier when non-empty.
    pub net_egress: Vec<String>,
    /// Secret grant **names** (simple form: `secrets = ["GITHUB_TOKEN"]`). Each
    /// name with no matching `[secret.<name>]` table gets default config.
    pub secrets: Vec<String>,
    /// Resolved secret grant **config** (names + source/inject/ttl) — the
    /// authoritative input to the broker (`docs/secrets-broker-design.md`).
    /// Config only; values never appear here (or in any digest/ref). Empty for
    /// non-secret policies, so their digest is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_grants: Vec<SecretGrant>,
    pub mem_bytes: Option<u64>,
    pub max_procs: Option<u64>,
    pub wall_secs: u64,
    /// Max single-file size (RLIMIT_FSIZE) — caps disk-bomb writes. Opt-in:
    /// `None` leaves it unbounded so legitimate large build artifacts aren't
    /// truncated with SIGXFSZ.
    pub fsize_bytes: Option<u64>,
    /// CPU-time limit in seconds (RLIMIT_CPU) — a kernel backstop to the
    /// wall clock against a CPU-spinning command. Opt-in.
    pub cpu_secs: Option<u64>,
    /// Tools allowlist — when non-empty, only these programs (argv[0] basename)
    /// may run; enforced fail-closed (see `check_tool_allowlist`).
    pub tools: Vec<String>,
    /// Container image for `isolation=container` (required at that tier). The
    /// command runs inside it with the workspace bind-mounted at `/work`.
    pub image: Option<String>,
    /// Environment-variable allowlist — the child gets *only* these (plus
    /// nothing else; secrets are never inherited wholesale).
    pub env_pass: Vec<String>,
}

/// One secret grant's configuration (never its value). Part of the resolved
/// policy, so a tampered `source` is caught by the policy digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretGrant {
    pub name: String,
    /// `env:VAR` | `file:/abs/path`; `None` ⇒ `env:H5I_SECRET_<NAME>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `file` (default) | `env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<String>,
    /// Advisory validity window for sources that mint a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

impl SecretGrant {
    /// Effective source string, applying the per-name default.
    pub fn source_or_default(&self) -> String {
        self.source
            .clone()
            .unwrap_or_else(|| format!("env:H5I_SECRET_{}", self.name))
    }
    /// Effective injection method. `env` is the universal default (works on
    /// every tier); `file` is opt-in and, in v1, supported on the `workspace`
    /// tier only (a secret file needs a Landlock grant on `process` and a
    /// bind-mount on `container` — a documented follow-up).
    pub fn inject_or_default(&self) -> &str {
        self.inject.as_deref().unwrap_or("env")
    }
}

impl Profile {
    /// Built-in profile used when no `.h5i/env.toml` exists. Fail-closed for
    /// `process`+ (deny-network, deny-home); `workspace` enforces nothing and
    /// honestly says so (`net_mode = host`, no grants).
    pub fn builtin(name: &str, isolation: IsolationClaim) -> Profile {
        let confined = isolation >= IsolationClaim::Process;
        Profile {
            name: name.to_string(),
            isolation,
            fs_read: if confined { default_fs_read() } else { Vec::new() },
            fs_write: if confined { vec!["$WORK".to_string()] } else { Vec::new() },
            fs_deny: default_fs_deny(),
            net_mode: if confined { NetMode::Deny } else { NetMode::Host },
            net_egress: Vec::new(),
            secrets: Vec::new(),
            secret_grants: Vec::new(),
            mem_bytes: Some(4 * 1024 * 1024 * 1024),
            max_procs: Some(256),
            wall_secs: DEFAULT_WALL.as_secs(),
            fsize_bytes: None,
            cpu_secs: None,
            tools: Vec::new(),
            image: None,
            env_pass: vec!["PATH".into(), "HOME".into(), "LANG".into()],
        }
    }

    pub fn wall(&self) -> Duration {
        Duration::from_secs(self.wall_secs)
    }
}

/// Read-only system paths granted by default at the `process` tier — enough to
/// exec interpreters and link against system libraries, nothing under `$HOME`.
fn default_fs_read() -> Vec<String> {
    ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/nix", "/opt", "/tmp", "/dev/null", "/dev/zero", "/dev/urandom", "/proc"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_fs_deny() -> Vec<String> {
    ["~/.ssh", "~/.aws", "~/.config/gh", "$REPO/.git/hooks"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// ── raw TOML schema (what users write; everything optional) ────────────────

#[derive(Debug, Default, Deserialize)]
struct PolicyFileToml {
    #[serde(default)]
    profile: BTreeMap<String, ProfileToml>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileToml {
    isolation: Option<String>,
    #[serde(default)]
    fs: FsToml,
    #[serde(default)]
    net: NetToml,
    #[serde(default)]
    secrets: Vec<String>,
    /// Rich per-grant config: `[profile.X.secret.NAME] source=… inject=… ttl=…`.
    #[serde(default)]
    secret: BTreeMap<String, SecretGrantToml>,
    resources: Option<ResourcesToml>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    container: ContainerToml,
    #[serde(default)]
    env: EnvVarsToml,
}

#[derive(Debug, Default, Deserialize)]
struct ContainerToml {
    /// Base image for `isolation=container`.
    image: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FsToml {
    #[serde(default)]
    read: Vec<String>,
    #[serde(default)]
    write: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct NetToml {
    mode: Option<String>,
    #[serde(default)]
    egress: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ResourcesToml {
    mem: Option<String>,
    procs: Option<u64>,
    wall: Option<String>,
    fsize: Option<String>,
    cpu: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct EnvVarsToml {
    #[serde(default)]
    pass: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SecretGrantToml {
    source: Option<String>,
    inject: Option<String>,
    ttl: Option<String>,
}

/// Merge the simple `secrets = [..]` name list with the rich `[secret.<name>]`
/// tables into the authoritative `secret_grants`. A name in both takes the rich
/// config; a name only in the simple list gets defaults; a rich table grants its
/// name implicitly. Deterministic order (sorted) for a stable policy digest.
fn merge_secret_grants(
    names: &[String],
    rich: &BTreeMap<String, SecretGrantToml>,
) -> Vec<SecretGrant> {
    let mut all: std::collections::BTreeSet<String> = names.iter().cloned().collect();
    all.extend(rich.keys().cloned());
    all.into_iter()
        .map(|name| {
            let cfg = rich.get(&name);
            SecretGrant {
                source: cfg.and_then(|c| c.source.clone()),
                inject: cfg.and_then(|c| c.inject.clone()),
                ttl: cfg.and_then(|c| c.ttl.clone()),
                name,
            }
        })
        .collect()
}

/// Load profile `name` from `<repo>/.h5i/env.toml`, falling back to the
/// built-in default when the file (or the `default` profile) is absent.
/// `isolation_override` (the CLI `--isolation` flag) replaces the profile's
/// claim. The result is validated (fail-closed lints) before being returned.
pub fn load_profile(
    repo_workdir: &Path,
    name: &str,
    isolation_override: Option<IsolationClaim>,
) -> Result<Profile, H5iError> {
    let path = repo_workdir.join(POLICY_FILE);
    let raw: Option<ProfileToml> = if path.is_file() {
        let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
        let mut file: PolicyFileToml = toml::from_str(&text)?;
        match file.profile.remove(name) {
            Some(p) => Some(p),
            None if name == "default" => None,
            None => {
                return Err(H5iError::Metadata(format!(
                    "profile '{name}' not found in {POLICY_FILE} (available: {})",
                    file.profile.keys().cloned().collect::<Vec<_>>().join(", ")
                )))
            }
        }
    } else if name != "default" {
        return Err(H5iError::Metadata(format!(
            "profile '{name}' requested but {POLICY_FILE} does not exist"
        )));
    } else {
        None
    };

    let mut profile = match raw {
        None => Profile::builtin(name, isolation_override.unwrap_or(IsolationClaim::Workspace)),
        Some(t) => {
            let isolation = match (&isolation_override, &t.isolation) {
                (Some(o), _) => *o,
                (None, Some(s)) => IsolationClaim::parse(s)?,
                (None, None) => IsolationClaim::Workspace,
            };
            let base = Profile::builtin(name, isolation);
            Profile {
                name: name.to_string(),
                isolation,
                fs_read: if t.fs.read.is_empty() { base.fs_read } else { t.fs.read },
                fs_write: if t.fs.write.is_empty() { base.fs_write } else { t.fs.write },
                fs_deny: if t.fs.deny.is_empty() { base.fs_deny } else { t.fs.deny },
                net_mode: match t.net.mode {
                    Some(ref s) => NetMode::parse(s)?,
                    None => base.net_mode,
                },
                net_egress: t.net.egress,
                secret_grants: merge_secret_grants(&t.secrets, &t.secret),
                secrets: t.secrets,
                mem_bytes: match t.resources.as_ref().and_then(|r| r.mem.as_deref()) {
                    Some(s) => Some(parse_mem(s)?),
                    None => base.mem_bytes,
                },
                max_procs: t.resources.as_ref().and_then(|r| r.procs).or(base.max_procs),
                wall_secs: match t.resources.as_ref().and_then(|r| r.wall.as_deref()) {
                    Some(s) => parse_wall(s)?.as_secs(),
                    None => base.wall_secs,
                },
                fsize_bytes: match t.resources.as_ref().and_then(|r| r.fsize.as_deref()) {
                    Some(s) => Some(parse_mem(s)?),
                    None => base.fsize_bytes,
                },
                cpu_secs: match t.resources.as_ref().and_then(|r| r.cpu.as_deref()) {
                    Some(s) => Some(parse_wall(s)?.as_secs()),
                    None => base.cpu_secs,
                },
                tools: t.tools,
                image: t.container.image.or(base.image),
                env_pass: if t.env.pass.is_empty() { base.env_pass } else { t.env.pass },
            }
        }
    };
    if let Some(o) = isolation_override {
        profile.isolation = o;
    }
    validate_profile(&profile)?;
    Ok(profile)
}

/// Fail-closed policy lints (§7). These reject *policies*, before any env is
/// created — never silently weaken them.
pub fn validate_profile(p: &Profile) -> Result<(), H5iError> {
    // Secret grants are brokered (docs/secrets-broker-design.md). Validate the
    // *config* here (names + source/inject syntax); values are resolved
    // fail-closed at run time, never at policy-load time.
    for g in &p.secret_grants {
        if g.name.is_empty() || !g.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(H5iError::Metadata(format!(
                "secret grant name '{}' is invalid — use ASCII letters, digits, '_' \
                 (it becomes an environment variable)",
                g.name
            )));
        }
        let src = g.source_or_default();
        if !(src.starts_with("env:") || src.starts_with("file:")) {
            return Err(H5iError::Metadata(format!(
                "secret grant '{}' has unsupported source '{src}' — use 'env:VAR' or \
                 'file:/abs/path' (fail-closed)",
                g.name
            )));
        }
        match g.inject_or_default() {
            "file" | "env" => {}
            other => {
                return Err(H5iError::Metadata(format!(
                    "secret grant '{}' has unknown inject '{other}' — use 'file' or 'env'",
                    g.name
                )))
            }
        }
    }
    // A domain egress allowlist cannot be honored by the static process tier
    // (netns is all-or-nothing) and is meaningless below it.
    if !p.net_egress.is_empty() && p.isolation <= IsolationClaim::Process {
        return Err(H5iError::Metadata(format!(
            "profile '{}' sets a net.egress domain allowlist, but isolation '{}' cannot \
             enforce it (process-v1 supports net.mode deny|host only) — use a \
             supervisor/container backend or drop net.egress (fail-closed)",
            p.name,
            p.isolation.as_str()
        )));
    }
    // fs.deny preflight lint: Landlock has no deny rules, so a granted parent
    // must never contain a denied child. Compare on expanded, normalized text.
    for grant in p.fs_read.iter().chain(p.fs_write.iter()) {
        let g = expand_tilde(grant);
        for deny in &p.fs_deny {
            let d = expand_tilde(deny);
            if d == g || d.starts_with(&format!("{}/", g.trim_end_matches('/'))) {
                return Err(H5iError::Metadata(format!(
                    "policy refused: granted path '{grant}' contains denied child '{deny}' \
                     (Landlock is allowlist-only and cannot subtract a child from a granted \
                     parent — narrow the grant)"
                )));
            }
        }
    }
    Ok(())
}

/// Expand a leading `~/` (or bare `~`) to `$HOME`. Symbolic placeholders like
/// `$WORK` / `$REPO` are left as-is (they expand at enforcement time).
fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            return format!("{}{}", home, &path[1..]);
        }
    }
    path.to_string()
}

/// Parse a memory size like "4G", "512M", "1024K", or plain bytes.
pub fn parse_mem(s: &str) -> Result<u64, H5iError> {
    let t = s.trim();
    let (num, mult) = match t.chars().last() {
        Some('G') | Some('g') => (&t[..t.len() - 1], 1024u64 * 1024 * 1024),
        Some('M') | Some('m') => (&t[..t.len() - 1], 1024 * 1024),
        Some('K') | Some('k') => (&t[..t.len() - 1], 1024),
        _ => (t, 1),
    };
    num.trim()
        .parse::<u64>()
        .map(|n| n * mult)
        .map_err(|_| H5iError::Metadata(format!("invalid resources.mem '{s}' (expected e.g. \"4G\", \"512M\")")))
}

/// Parse a wall-clock duration like "30m", "90s", "2h".
pub fn parse_wall(s: &str) -> Result<Duration, H5iError> {
    let t = s.trim();
    let (num, mult) = match t.chars().last() {
        Some('h') => (&t[..t.len() - 1], 3600u64),
        Some('m') => (&t[..t.len() - 1], 60),
        Some('s') => (&t[..t.len() - 1], 1),
        _ => (t, 1),
    };
    num.trim()
        .parse::<u64>()
        .map(|n| Duration::from_secs(n * mult))
        .map_err(|_| H5iError::Metadata(format!("invalid resources.wall '{s}' (expected e.g. \"30m\", \"90s\")")))
}

// ─── capability probing (§5, mandatory) ─────────────────────────────────────

/// What this host's kernel actually supports. Probed at env creation and
/// before every confined run — never assumed.
#[derive(Debug, Clone, Serialize)]
pub struct HostCaps {
    pub os: String,
    /// Landlock ABI version (≥1 means filesystem scoping works); `None` when
    /// the LSM is absent/disabled (e.g. many WSL2 kernels).
    pub landlock_abi: Option<i32>,
    /// Unprivileged user namespaces (needed for `net.mode = deny`).
    pub userns: bool,
    /// seccomp-bpf filters.
    pub seccomp: bool,
    /// Detected rootless Podman binary for `isolation=container`; `None` when
    /// Podman is absent, broken, or rootful.
    pub container_runtime: Option<String>,
}

#[cfg(target_os = "linux")]
pub fn probe_host() -> HostCaps {
    HostCaps {
        os: "linux".into(),
        landlock_abi: probe_landlock_abi(),
        userns: probe_userns(),
        seccomp: probe_seccomp(),
        container_runtime: crate::container::probe().map(|r| r.bin),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn probe_host() -> HostCaps {
    HostCaps {
        os: std::env::consts::OS.to_string(),
        landlock_abi: None,
        userns: false,
        seccomp: false,
        container_runtime: crate::container::probe().map(|r| r.bin),
    }
}

#[cfg(target_os = "linux")]
fn probe_landlock_abi() -> Option<i32> {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    // returns the highest supported ABI, or -1 (ENOSYS/EOPNOTSUPP) when the
    // LSM is unavailable. This does not create anything.
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1 << 0;
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret > 0 { Some(ret as i32) } else { None }
}

#[cfg(target_os = "linux")]
fn probe_seccomp() -> bool {
    // PR_GET_SECCOMP succeeds (0 or 2) iff the kernel has seccomp.
    unsafe { libc::prctl(libc::PR_GET_SECCOMP) >= 0 }
}

#[cfg(target_os = "linux")]
fn probe_userns() -> bool {
    // The only reliable probe is to try: unshare(CLONE_NEWUSER) in a throwaway
    // child (never in this process). `true` exits 0 iff the unshare succeeded.
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("true");
    unsafe {
        cmd.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ─── claim resolution (fail-closed, §6) ─────────────────────────────────────

/// The policy as actually enforced: profile + resolved claim. Serialized as
/// `policy.resolved.toml`; its digest is pinned in the env manifest and in
/// every capture taken inside the env.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    pub claim: IsolationClaim,
    pub profile: Profile,
}

impl ResolvedPolicy {
    pub fn to_toml(&self) -> Result<String, H5iError> {
        toml::to_string(self).map_err(|e| H5iError::Metadata(format!("policy serialization failed: {e}")))
    }

    /// Parse a stored `policy.resolved.toml` back. Callers MUST verify
    /// [`Self::digest`] against the env manifest's pinned digest afterwards —
    /// the stored file is tamper-evident, not trusted.
    pub fn from_toml(text: &str) -> Result<Self, H5iError> {
        toml::from_str(text).map_err(H5iError::TomlParse)
    }

    /// sha256 over the canonical TOML serialization.
    pub fn digest(&self) -> Result<String, H5iError> {
        let toml = self.to_toml()?;
        let mut hasher = Sha256::new();
        hasher.update(toml.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }
}

/// Resolve `profile` against what `caps` says the host supports. Refuses —
/// never silently downgrades — when the requested minimum claim cannot be
/// satisfied (§5 "Capability probing + fail-closed").
pub fn resolve(profile: &Profile, caps: &HostCaps) -> Result<ResolvedPolicy, H5iError> {
    validate_profile(profile)?;
    match profile.isolation {
        IsolationClaim::Workspace => {}
        IsolationClaim::Process => {
            let mut missing: Vec<String> = Vec::new();
            if caps.os != "linux" {
                missing.push(format!(
                    "isolation=process is Linux-only in this build (host: {})",
                    caps.os
                ));
            } else {
                if caps.landlock_abi.is_none() {
                    missing.push(
                        "Landlock LSM unavailable (kernel <5.13, or compiled out / not in the \
                         active LSM list — common on WSL2)"
                            .into(),
                    );
                }
                if !caps.seccomp {
                    missing.push("seccomp-bpf unavailable".into());
                }
                if profile.net_mode == NetMode::Deny && !caps.userns {
                    missing.push(
                        "unprivileged user namespaces unavailable (required for net.mode=deny)"
                            .into(),
                    );
                }
            }
            if !missing.is_empty() {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'process' cannot be satisfied on this host — refusing \
                     (h5i never silently downgrades):\n  - {}\nRe-request a weaker claim \
                     (--isolation workspace) or fix the host.",
                    missing.join("\n  - ")
                )));
            }
        }
        IsolationClaim::Container => {
            // Rootless Podman adapter (opt-in shell-out). Require the
            // runtime AND an image — fail closed, never silently downgrade.
            if caps.container_runtime.is_none() {
                return Err(H5iError::Metadata(
                    "isolation claim 'container' requires rootless Podman on PATH; Docker and \
                     rootful Podman are intentionally not accepted in this Linux/WSL backend — \
                     install/configure rootless podman, or re-request --isolation workspace/process"
                        .into(),
                ));
            }
            if profile.image.is_none() {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'container' requires a base image — set `container.image = \
                     \"…\"` in profile '{}' (e.g. your toolchain image)",
                    profile.name
                )));
            }
        }
        IsolationClaim::Supervised => {
            // The keystone safety property: refuse unless the ENTIRE mediation
            // stack probes green on this host. Never downgrade to a weaker tier
            // — an unsatisfiable supervised claim is an *impossible* claim, not
            // a degraded pass (docs/supervisor-design.md).
            let probe = crate::supervisor::probe();
            if !probe.usable {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'supervised' cannot be satisfied on this host — refusing \
                     (h5i never claims untrusted-code containment it cannot deliver). Missing:\n  - {}\n\
                     Re-request a weaker claim (--isolation process|workspace), or run on a host \
                     with the full stack (see docs/supervisor-design.md).",
                    probe.missing().join("\n  - ")
                )));
            }
        }
        claim => {
            return Err(H5iError::Metadata(format!(
                "isolation claim '{}' requires an external backend adapter that this build \
                 does not ship yet (rollout §11 phase 4) — use workspace, process, container, or supervised",
                claim.as_str()
            )));
        }
    }
    Ok(ResolvedPolicy {
        claim: profile.isolation,
        profile: profile.clone(),
    })
}

// ─── confined execution (Linux, `process` tier) ─────────────────────────────

/// Outcome of one (possibly confined) command execution, including resource
/// accounting (`records exit/resource/egress`, design §9).
#[derive(Debug)]
pub struct ExecOutcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Wall-clock duration of the run, milliseconds.
    pub wall_ms: u128,
    /// CPU time (user + system) consumed by the command and its children, ms.
    pub cpu_ms: u128,
    /// Peak resident set size of the command and its children, KiB
    /// (`rusage.ru_maxrss`). `None` when the platform doesn't report it.
    pub max_rss_kb: Option<i64>,
    /// Network egress verdicts observed during the run. Only the
    /// `isolation=container` tier (whose allowlist proxy sees every request)
    /// populates this; `None` for `workspace`/`process`.
    pub egress: Option<crate::objects::EgressSummary>,
}

/// Validate `argv` against the policy's `tools` allowlist. When the list is
/// non-empty, the command's program (argv[0], by basename) MUST be listed —
/// defense in depth so a profile can pin exactly which executables an
/// environment may launch. An empty list means "unrestricted" (the default).
fn check_tool_allowlist(policy: &ResolvedPolicy, argv: &[String]) -> Result<(), H5iError> {
    let tools = &policy.profile.tools;
    if tools.is_empty() {
        return Ok(());
    }
    let prog = &argv[0];
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    if tools.iter().any(|t| t == base || t == prog) {
        Ok(())
    } else {
        Err(H5iError::Metadata(format!(
            "command '{base}' is not in the profile '{}' tools allowlist ({}) — refusing (fail-closed)",
            policy.profile.name,
            tools.join(", ")
        )))
    }
}

/// Run `argv` inside `work` under `policy`. Dispatches on the resolved claim:
/// `workspace` runs unconfined (trusted; file isolation only), `process`
/// applies the kernel confinement. Anything else was already refused by
/// [`resolve`].
pub fn run(policy: &ResolvedPolicy, work: &Path, argv: &[String]) -> Result<ExecOutcome, H5iError> {
    run_with_env(policy, work, argv, &[])
}

/// Like [`run`], plus `injected_env` (the secrets broker's resolved grants)
/// applied to the child *after* the `env.pass` allowlist. The values are not
/// part of the policy and never serialized — they only reach the child process.
pub fn run_with_env(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    check_tool_allowlist(policy, argv)?;
    match policy.claim {
        IsolationClaim::Workspace => run_unconfined(policy, work, argv, injected_env),
        IsolationClaim::Process => run_confined(policy, work, argv, injected_env),
        IsolationClaim::Supervised => crate::supervisor::run(policy, work, argv, injected_env),
        IsolationClaim::Container => crate::container::run(policy, work, argv, injected_env),
        claim => Err(H5iError::Metadata(format!(
            "no backend for isolation claim '{}'",
            claim.as_str()
        ))),
    }
}

/// Apply the secrets broker's injected env vars to a child command (used by each
/// tier). Applied after `env.pass`, so a grant can't be shadowed by a host var.
fn apply_injected_env(cmd: &mut std::process::Command, injected_env: &[(String, String)]) {
    for (k, v) in injected_env {
        cmd.env(k, v);
    }
}

/// Monotonic counter so concurrent functional probes get distinct temp dirs.
static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Functionally verify the resolved policy can actually *execute* a command on
/// this host. Capability bits (Landlock + user namespaces + seccomp present)
/// are necessary but **not sufficient**: a hardened kernel can satisfy every
/// bit yet still deny `exec` under the full confinement stack — notably
/// AppArmor-restricted unprivileged user namespaces on Ubuntu 24.04 (and the
/// GitHub-Actions runners built on it), where `unshare(CLONE_NEWUSER)` succeeds
/// but the resulting namespace is too restricted to run a program.
///
/// For non-`process` claims this is a no-op. For `process`, it runs a trivial
/// `true` inside a throwaway directory under the *same* confinement the
/// environment will use (the tool allowlist is bypassed — the probe command is
/// ours, not the user's). Returning an error lets `env create` fail closed with
/// a clear message instead of letting every later `env run` die on EACCES.
pub fn verify_exec(policy: &ResolvedPolicy) -> Result<(), H5iError> {
    if policy.claim != IsolationClaim::Process {
        return Ok(());
    }
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("h5i-exec-probe-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    // Clone the profile but clear the tools allowlist so our internal probe
    // command isn't rejected by a user-pinned list that omits `true`.
    let mut profile = policy.profile.clone();
    profile.tools.clear();
    let probe = ResolvedPolicy { claim: policy.claim, profile };
    let result = run(&probe, &dir, &["true".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(o) if o.exit_code == Some(0) => Ok(()),
        Ok(o) => Err(H5iError::Metadata(format!(
            "process-tier confinement self-test exited {:?} on this host — refusing to create an \
             environment whose commands could not run (re-request --isolation workspace)",
            o.exit_code
        ))),
        Err(e) => Err(H5iError::Metadata(format!(
            "process-tier confinement is not functional on this host: {e}. The kernel reports \
             Landlock/user-namespace/seccomp support, but a confined command could not execute \
             (e.g. AppArmor-restricted unprivileged user namespaces). Re-request \
             --isolation workspace."
        ))),
    }
}

/// `workspace` tier: no kernel confinement (trusted), but still scoped — runs
/// in the env worktree with the wall-clock limit applied so a hung command
/// cannot wedge `h5i env run` forever.
fn run_unconfined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(work);
    apply_injected_env(&mut cmd, injected_env);
    // New session so the wall-clock kill reaps the whole tree (killpg), the
    // same group-kill guarantee the confined path gets.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    wait_with_deadline(cmd, policy.profile.wall(), argv, None)
}

/// Build a fully-confined `std::process::Command` for `argv` — the **shared**
/// confinement core used by both the `process` tier ([`run_confined`]) and the
/// `supervised` tier ([`crate::supervisor::run`]). Keeping this in one place
/// means the security-critical setup (Landlock + seccomp deny-list + namespaces
/// + rlimits + no-new-privs + uid/gid maps) has a single audited implementation.
///
/// - `force_netns`: always create a fresh network namespace (the `supervised`
///   tier does; the `process` tier only when `net.mode = deny`).
/// - `notify_sock`: when `Some`, the child additionally installs a
///   seccomp **user-notification** filter and hands the listener fd to this
///   `AF_UNIX` socket via `SCM_RIGHTS` (the `supervised` socket gate). The
///   notify filter stacks *after* the deny-list, and seccomp action precedence
///   (ERRNO > USER_NOTIF > ALLOW) makes them compose correctly.
#[cfg(target_os = "linux")]
pub(crate) fn build_confined_command(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    force_netns: bool,
    notify_sock: Option<std::os::unix::io::RawFd>,
) -> Result<std::process::Command, H5iError> {
    use std::os::unix::process::CommandExt;

    let p = &policy.profile;
    let work = work
        .canonicalize()
        .map_err(|e| H5iError::with_path(e, work))?;

    // Re-probe at run time (the host may have changed since `env create`) and
    // fail closed before spawning anything.
    let caps = probe_host();
    resolve(p, &caps)?;

    // ── Landlock ruleset (built pre-fork; restricted in the child) ──
    // Grants: rw on $WORK + fs.write, ro on fs.read. Paths that don't exist on
    // this host are skipped — skipping a *grant* narrows the sandbox, which is
    // the fail-closed direction.
    let abi = landlock_abi_for(caps.landlock_abi.unwrap_or(1));
    let rw_paths: Vec<PathBuf> = std::iter::once(work.clone())
        .chain(p.fs_write.iter().filter(|s| s.as_str() != "$WORK").map(|s| PathBuf::from(expand_tilde(s))))
        .filter(|p| p.exists())
        .collect();
    let ro_paths: Vec<PathBuf> = p
        .fs_read
        .iter()
        .map(|s| PathBuf::from(expand_tilde(s)))
        .filter(|p| p.exists())
        .collect();

    let ruleset = {
        use landlock::{
            path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
            RulesetCreatedAttr,
        };
        Ruleset::default()
            // Fail closed: if the kernel can't enforce what we handle, error —
            // never a silent partial sandbox.
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(&ro_paths, AccessFs::from_read(abi))))
            .and_then(|r| r.add_rules(path_beneath_rules(&rw_paths, AccessFs::from_all(abi))))
            .map_err(|e| H5iError::Metadata(format!("landlock ruleset construction failed: {e}")))?
    };

    // ── seccomp deny-list program (compiled pre-fork) ──
    let bpf = seccomp_deny_program()?;

    let want_netns = p.net_mode == NetMode::Deny || force_netns;
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let mem = p.mem_bytes;
    let nproc = p.max_procs;
    let fsize = p.fsize_bytes;
    let cpu = p.cpu_secs;

    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&work);

    // Environment allowlist — nothing inherited wholesale (§7).
    cmd.env_clear();
    for key in &p.env_pass {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // Brokered secrets, applied after the allowlist (so a grant is never
    // shadowed by a passed-through host var).
    apply_injected_env(&mut cmd, injected_env);

    let mut ruleset_slot = Some(ruleset);
    unsafe {
        cmd.pre_exec(move || {
            use std::io::Error;

            // 0. New session/process group so the wall-clock kill can reap the
            //    WHOLE tree (killpg), not just the direct child — a confined
            //    command must not be able to leave runaway descendants behind.
            if libc::setsid() == -1 {
                return Err(Error::last_os_error());
            }

            // 1. Namespaces. Always create a user namespace at the process tier
            //    (drops every host capability outside it) plus fresh IPC and
            //    UTS namespaces (no shared SysV IPC, isolated hostname); add an
            //    empty network namespace when egress is denied. CLONE_NEWUSER
            //    makes all of this unprivileged; we map our own uid/gid 1:1 so
            //    file access inside $WORK keeps working.
            let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS;
            if want_netns {
                flags |= libc::CLONE_NEWNET;
            }
            if libc::unshare(flags) != 0 {
                return Err(Error::last_os_error());
            }
            std::fs::write("/proc/self/setgroups", "deny")?;
            std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
            std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;

            // 2. Resource caps (cooperative, no cgroups needed).
            if let Some(bytes) = mem {
                let lim = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
                if libc::setrlimit(libc::RLIMIT_AS, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            if let Some(n) = nproc {
                let lim = libc::rlimit { rlim_cur: n, rlim_max: n };
                if libc::setrlimit(libc::RLIMIT_NPROC, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            if let Some(bytes) = fsize {
                // Cap any single file the command writes — a disk-bomb backstop.
                let lim = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            if let Some(secs) = cpu {
                // Hard CPU-time cap (SIGKILL at the hard limit) — a kernel
                // backstop to the host-side wall-clock kill.
                let lim = libc::rlimit { rlim_cur: secs, rlim_max: secs };
                if libc::setrlimit(libc::RLIMIT_CPU, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &core);

            // 3. No new privileges — required by Landlock, and blocks setuid
            //    escalation on its own.
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(Error::last_os_error());
            }

            // 4. Landlock filesystem allowlist. Fail closed if not fully
            //    enforced (HardRequirement should already guarantee this).
            let rs = ruleset_slot
                .take()
                .ok_or_else(|| Error::other("landlock ruleset consumed twice"))?;
            let status = rs
                .restrict_self()
                .map_err(|e| Error::other(format!("landlock restrict_self: {e}")))?;
            if status.ruleset == landlock::RulesetStatus::NotEnforced {
                return Err(Error::other("landlock not enforced (fail-closed)"));
            }

            // 5. Seccomp deny-list (everything after this call is subject to
            //    the filter).
            seccompiler::apply_filter(&bpf)
                .map_err(|e| Error::other(format!("seccomp apply: {e}")))?;

            // 6. Supervised tier only: stack a seccomp user-notification filter
            //    on top of the deny-list and hand its listener fd to the
            //    supervisor over `notify_sock`. The untrusted program must not
            //    inherit the listener, so it's CLOEXEC (the supervisor keeps its
            //    own copy received via SCM_RIGHTS).
            if let Some(sock) = notify_sock {
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                {
                    let listener = crate::seccomp_notify::install_listener()
                        .map_err(Error::from_raw_os_error)?;
                    libc::fcntl(listener, libc::F_SETFD, libc::FD_CLOEXEC);
                    crate::seccomp_notify::send_fd(sock, listener)?;
                }
                #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
                {
                    let _ = sock;
                    return Err(Error::other("seccomp user-notif unsupported on this arch"));
                }
            }
            Ok(())
        });
    }
    Ok(cmd)
}

#[cfg(target_os = "linux")]
fn run_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let p = &policy.profile;
    // Process tier: netns only when egress is denied; no seccomp-notify gate.
    let cmd = build_confined_command(policy, work, argv, injected_env, false, None)?;

    // cgroup v2 (rootless, best-effort): real memory.max/pids.max + accurate
    // memory.peak/cpu accounting where the host delegates a writable subtree.
    // Unavailable → `None`, and the rlimits set in the child still apply.
    let cg = make_run_cgroup(p.mem_bytes, p.max_procs);
    let procs = cg.as_ref().map(|c| c.procs_path());
    let mut outcome = wait_with_deadline(cmd, p.wall(), argv, procs.as_deref())?;
    if let Some(cg) = &cg {
        let u = cg.usage();
        // Prefer cgroup accounting (whole-subtree, accurate) over rusage.
        if let Some(bytes) = u.mem_peak_bytes {
            outcome.max_rss_kb = Some((bytes / 1024) as i64);
        }
        if let Some(usec) = u.cpu_usec {
            outcome.cpu_ms = (usec / 1000) as u128;
        }
    }
    Ok(outcome)
}

/// Create a best-effort run cgroup when the profile sets a memory/pid limit and
/// the host actually supports rootless cgroup management. `None` (the common
/// case on WSL2/CI) leaves the rlimit path as the sole enforcement.
#[cfg(target_os = "linux")]
pub(crate) fn make_run_cgroup(mem_bytes: Option<u64>, max_procs: Option<u64>) -> Option<crate::cgroup::ScopedCgroup> {
    if mem_bytes.is_none() && max_procs.is_none() {
        return None;
    }
    let caps = crate::cgroup::probe();
    if !caps.usable {
        return None;
    }
    let parent = caps.parent?;
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::cgroup::ScopedCgroup::create(&parent, seq, mem_bytes, max_procs).ok()
}

#[cfg(not(target_os = "linux"))]
fn run_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    Err(H5iError::Metadata(
        "isolation=process is Linux-only in this build (fail-closed)".into(),
    ))
}

/// Map a probed Landlock ABI version to the highest version this crate knows.
#[cfg(target_os = "linux")]
fn landlock_abi_for(probed: i32) -> landlock::ABI {
    match probed {
        1 => landlock::ABI::V1,
        2 => landlock::ABI::V2,
        3 => landlock::ABI::V3,
        4 => landlock::ABI::V4,
        _ => landlock::ABI::V5,
    }
}

/// The seccomp **deny-list** (v1, §5): dangerous administrative / introspection
/// syscalls return EPERM; everything else is allowed. A default-deny allowlist
/// is a later hardened profile. Known gap (documented, not hidden): `clone`
/// with CLONE_NEWUSER is not arg-filtered in v1 — `unshare` is denied, and
/// no_new_privs + Landlock still bound what a fresh namespace could reach.
#[cfg(target_os = "linux")]
fn seccomp_deny_program() -> Result<seccompiler::BpfProgram, H5iError> {
    use seccompiler::{SeccompAction, SeccompFilter, SeccompRule, TargetArch};

    // Architecture-portable set (present on x86_64 and aarch64). Every entry is
    // an administrative / introspection / namespace / fs-handle syscall that a
    // build or test workload never legitimately issues, so a blanket EPERM is
    // safe. We deliberately do NOT deny clone/clone3/fork (needed for normal
    // subprocesses); the documented clone-with-CLONE_NEWUSER gap is closed by
    // the hardened allowlist profile (a later phase), not here.
    #[allow(unused_mut)] // `mut` is used only on arches with the extend below
    let mut denied: Vec<libc::c_long> = vec![
        // mount / rootfs manipulation
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        // tracing / cross-process memory
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        // kernel keyring
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        // privileged kernel interfaces
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_userfaultfd,
        // module loading
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        // kexec
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        // filesystem handles (bypass path-based confinement / Landlock)
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        // namespace entry/creation
        libc::SYS_setns,
        libc::SYS_unshare,
        // host / time / power administration
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_settimeofday,
        libc::SYS_clock_settime,
        libc::SYS_clock_adjtime,
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        libc::SYS_quotactl,
        // NUMA memory-policy / page migration (host-visibility side effects)
        libc::SYS_move_pages,
        libc::SYS_mbind,
        libc::SYS_set_mempolicy,
        libc::SYS_migrate_pages,
        // filesystem-wide change notification
        libc::SYS_fanotify_init,
        libc::SYS_fanotify_mark,
        // io_uring — a large, repeatedly-exploited kernel attack surface that
        // also bypasses seccomp for the operations it submits; build/test
        // workloads don't need it, so deny the whole interface.
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ];
    // x86_64-only port-I/O and LDT syscalls (absent on aarch64).
    #[cfg(target_arch = "x86_64")]
    denied.extend_from_slice(&[libc::SYS_iopl, libc::SYS_ioperm, libc::SYS_modify_ldt]);

    // The cast is a no-op on 64-bit but required where c_long is i32.
    #[allow(clippy::unnecessary_cast)]
    let rules: std::collections::BTreeMap<i64, Vec<SeccompRule>> =
        denied.iter().map(|s| (*s as i64, Vec::new())).collect();
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| H5iError::Metadata(format!("unsupported seccomp arch {}", std::env::consts::ARCH)))?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                       // mismatch: allow
        SeccompAction::Errno(libc::EPERM as u32),   // match: EPERM
        arch,
    )
    .map_err(|e| H5iError::Metadata(format!("seccomp filter build failed: {e}")))?;
    filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| H5iError::Metadata(format!("seccomp compile failed: {e}")))
}

/// Spawn `cmd`, stream stdout/stderr off-thread, and enforce `wall` as a hard
/// deadline (SIGKILL). stdin is closed — env runs are non-interactive by
/// construction so a confined process can't block on a prompt forever.
fn wait_with_deadline(
    mut cmd: std::process::Command,
    wall: Duration,
    argv: &[String],
    cgroup_procs: Option<&Path>,
) -> Result<ExecOutcome, H5iError> {
    use std::io::Read;
    use std::process::Stdio;

    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", argv.join(" "))))?;

    // Move the child into its cgroup as early as possible (best-effort): write
    // its pid to the cgroup's `cgroup.procs`. There's a sub-millisecond window
    // between spawn and this write where the child is not yet limited — accepted
    // for v1 (CLONE_INTO_CGROUP would close it but isn't exposed by std).
    if let Some(procs) = cgroup_procs {
        let _ = std::fs::write(procs, child.id().to_string());
    }

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let (exit_code, timed_out, cpu_ms, max_rss_kb) = wait_loop(&mut child, wall);

    Ok(ExecOutcome {
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
        exit_code,
        timed_out,
        wall_ms: started.elapsed().as_millis(),
        cpu_ms,
        max_rss_kb,
        egress: None, // process tier doesn't proxy egress (see container tier)
    })
}

/// Poll the child to the deadline, enforcing the wall-clock with a
/// process-group SIGKILL, and reap it with `wait4` so we recover `rusage`
/// (peak RSS + CPU time). Returns `(exit_code, timed_out, cpu_ms, max_rss_kb)`.
#[cfg(unix)]
pub(crate) fn wait_loop(
    child: &mut std::process::Child,
    wall: Duration,
) -> (Option<i32>, bool, u128, Option<i64>) {
    // The child called setsid(), so its process-group id equals its pid; a
    // negative-pid SIGKILL reaps the whole tree, not just the leader.
    let pid = child.id() as libc::pid_t;
    let deadline = std::time::Instant::now() + wall;
    let mut timed_out = false;

    loop {
        let mut status: libc::c_int = 0;
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if r == pid {
            // Reaped. Decode exit/signal and resource usage. (std's Child does
            // not auto-wait on drop, so reaping here causes no double-wait.)
            let exit_code = if libc::WIFEXITED(status) {
                Some(libc::WEXITSTATUS(status))
            } else {
                None // died on a signal (incl. our SIGKILL)
            };
            return (exit_code, timed_out, cpu_ms(&usage), Some(usage.ru_maxrss));
        }
        if r == -1 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // Lost the child (e.g. ECHILD) — fall back to std's bookkeeping.
            let code = child.wait().ok().and_then(|s| s.code());
            return (code, timed_out, 0, None);
        }
        // r == 0: still running.
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            unsafe {
                if libc::kill(-pid, libc::SIGKILL) != 0 {
                    let _ = child.kill();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn cpu_ms(u: &libc::rusage) -> u128 {
    let secs = (u.ru_utime.tv_sec + u.ru_stime.tv_sec) as u128;
    let usecs = (u.ru_utime.tv_usec + u.ru_stime.tv_usec) as u128;
    secs * 1000 + usecs / 1000
}

#[cfg(not(unix))]
pub(crate) fn wait_loop(
    child: &mut std::process::Child,
    wall: Duration,
) -> (Option<i32>, bool, u128, Option<i64>) {
    let deadline = std::time::Instant::now() + wall;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().expect("wait after kill");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return (None, timed_out, 0, None),
        }
    };
    (status.code(), timed_out, 0, None)
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_example_toml() -> &'static str {
        r#"
[profile.default]
isolation = "process"
fs.read   = ["/usr", "/lib", "/nix"]
fs.write  = ["$WORK"]
fs.deny   = ["~/.ssh", "~/.aws", "~/.config/gh", "$REPO/.git/hooks"]
net.mode  = "deny"
net.egress = []
secrets   = []
resources = { mem = "4G", procs = 256, wall = "30m" }
tools     = ["python", "pytest", "cargo", "npm", "git"]
env.pass  = ["PATH", "HOME", "LANG"]
"#
    }

    fn load_from_str(toml_text: &str, name: &str, over: Option<IsolationClaim>) -> Result<Profile, H5iError> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".h5i")).unwrap();
        std::fs::write(dir.path().join(POLICY_FILE), toml_text).unwrap();
        load_profile(dir.path(), name, over)
    }

    #[test]
    fn parses_the_design_doc_example_profile() {
        let p = load_from_str(doc_example_toml(), "default", None).expect("doc example must parse");
        assert_eq!(p.isolation, IsolationClaim::Process);
        assert_eq!(p.fs_read, vec!["/usr", "/lib", "/nix"]);
        assert_eq!(p.fs_write, vec!["$WORK"]);
        assert_eq!(p.net_mode, NetMode::Deny);
        assert_eq!(p.mem_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(p.max_procs, Some(256));
        assert_eq!(p.wall_secs, 30 * 60);
        assert_eq!(p.env_pass, vec!["PATH", "HOME", "LANG"]);
        assert_eq!(p.tools.len(), 5);
    }

    #[test]
    fn resources_fsize_and_cpu_parse_and_default_off() {
        // Opt-in: absent → None (unbounded file size, no CPU cap).
        let p = load_from_str(doc_example_toml(), "default", None).unwrap();
        assert_eq!(p.fsize_bytes, None);
        assert_eq!(p.cpu_secs, None);

        let toml_text = r#"
[profile.default]
isolation = "process"
resources = { mem = "2G", fsize = "100M", cpu = "5s" }
"#;
        let p = load_from_str(toml_text, "default", None).unwrap();
        assert_eq!(p.mem_bytes, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(p.fsize_bytes, Some(100 * 1024 * 1024));
        assert_eq!(p.cpu_secs, Some(5));
    }

    #[test]
    fn fsize_changes_the_policy_digest() {
        let mut a = Profile::builtin("default", IsolationClaim::Process);
        let mut b = a.clone();
        a.fsize_bytes = None;
        b.fsize_bytes = Some(100 * 1024 * 1024);
        let ra = ResolvedPolicy { claim: a.isolation, profile: a };
        let rb = ResolvedPolicy { claim: b.isolation, profile: b };
        assert_ne!(ra.digest().unwrap(), rb.digest().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn seccomp_deny_program_builds_and_blocks_io_uring() {
        // The program compiles on this arch …
        assert!(seccomp_deny_program().is_ok());
        // … and io_uring is in the denied set (the curated list is the contract).
        // Reference the syscalls so this test fails to compile if libc drops them.
        let _ = (libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register);
    }

    #[test]
    fn missing_policy_file_yields_builtin_workspace_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = load_profile(dir.path(), "default", None).unwrap();
        assert_eq!(p.isolation, IsolationClaim::Workspace);
        // Workspace honestly claims nothing: no grants, host network.
        assert_eq!(p.net_mode, NetMode::Host);
        assert!(p.fs_write.is_empty());
    }

    #[test]
    fn missing_named_profile_is_an_error() {
        let err = load_from_str(doc_example_toml(), "fetch", None).unwrap_err();
        assert!(err.to_string().contains("profile 'fetch' not found"), "{err}");
    }

    #[test]
    fn isolation_override_wins_over_profile() {
        let p = load_from_str(doc_example_toml(), "default", Some(IsolationClaim::Workspace)).unwrap();
        assert_eq!(p.isolation, IsolationClaim::Workspace);
    }

    #[test]
    fn egress_allowlist_under_process_fails_closed() {
        let toml_text = r#"
[profile.default]
isolation = "process"
net.mode = "deny"
net.egress = ["pypi.org", "github.com:443"]
"#;
        let err = load_from_str(toml_text, "default", None).unwrap_err();
        assert!(err.to_string().contains("net.egress"), "{err}");
        assert!(err.to_string().contains("fail-closed"), "{err}");
    }

    #[test]
    fn secret_grants_are_accepted_and_normalized() {
        // Secrets are now brokered (docs/secrets-broker-design.md): a profile
        // that declares them loads, with names merged into secret_grants.
        let toml_text = r#"
[profile.default]
isolation = "process"
secrets = ["DB_URL"]

[profile.default.secret.GITHUB_TOKEN]
source = "env:GH_PAT"
inject = "env"
"#;
        let p = load_from_str(toml_text, "default", None).unwrap();
        let names: Vec<&str> = p.secret_grants.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"DB_URL"));
        assert!(names.contains(&"GITHUB_TOKEN"));
        let gh = p.secret_grants.iter().find(|g| g.name == "GITHUB_TOKEN").unwrap();
        assert_eq!(gh.source.as_deref(), Some("env:GH_PAT"));
        // DB_URL got defaults.
        let db = p.secret_grants.iter().find(|g| g.name == "DB_URL").unwrap();
        assert_eq!(db.source_or_default(), "env:H5I_SECRET_DB_URL");
    }

    #[test]
    fn secret_grant_bad_source_fails_closed() {
        let toml_text = r#"
[profile.default]
[profile.default.secret.TOK]
source = "http://evil/steal"
"#;
        let err = load_from_str(toml_text, "default", None).unwrap_err();
        assert!(err.to_string().contains("source"), "{err}");
    }

    #[test]
    fn fs_deny_lint_rejects_granted_parent_of_denied_child() {
        // Granting $HOME while denying ~/.ssh is unenforceable under Landlock
        // (allowlist-only) — the policy must be refused, not weakened.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/x".into());
        let toml_text = format!(
            r#"
[profile.default]
isolation = "process"
fs.read = ["{home}"]
fs.deny = ["~/.ssh"]
"#
        );
        let err = load_from_str(&toml_text, "default", None).unwrap_err();
        assert!(err.to_string().contains("granted path"), "{err}");
    }

    #[test]
    fn fs_deny_lint_allows_disjoint_grants() {
        let toml_text = r#"
[profile.default]
isolation = "process"
fs.read = ["/usr", "/lib"]
fs.deny = ["~/.ssh", "$REPO/.git/hooks"]
"#;
        assert!(load_from_str(toml_text, "default", None).is_ok());
    }

    #[test]
    fn parse_mem_units() {
        assert_eq!(parse_mem("4G").unwrap(), 4 * 1024 * 1024 * 1024);
        assert_eq!(parse_mem("512M").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_mem("64k").unwrap(), 64 * 1024);
        assert_eq!(parse_mem("12345").unwrap(), 12345);
        assert!(parse_mem("lots").is_err());
    }

    #[test]
    fn parse_wall_units() {
        assert_eq!(parse_wall("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_wall("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_wall("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_wall("45").unwrap(), Duration::from_secs(45));
        assert!(parse_wall("soon").is_err());
    }

    #[test]
    fn isolation_claim_parse_and_order() {
        assert_eq!(IsolationClaim::parse("workspace").unwrap(), IsolationClaim::Workspace);
        assert_eq!(
            IsolationClaim::parse("hardened-container").unwrap(),
            IsolationClaim::HardenedContainer
        );
        assert!(IsolationClaim::parse("docker").is_err());
        assert!(IsolationClaim::Workspace < IsolationClaim::Process);
        assert!(IsolationClaim::Process < IsolationClaim::Microvm);
    }

    #[test]
    fn policy_digest_is_stable_and_content_sensitive() {
        let p1 = load_from_str(doc_example_toml(), "default", None).unwrap();
        let p2 = load_from_str(doc_example_toml(), "default", None).unwrap();
        let r1 = ResolvedPolicy { claim: p1.isolation, profile: p1 };
        let r2 = ResolvedPolicy { claim: p2.isolation, profile: p2 };
        assert_eq!(r1.digest().unwrap(), r2.digest().unwrap());

        let mut p3 = r1.profile.clone();
        p3.net_mode = NetMode::Host;
        let r3 = ResolvedPolicy { claim: p3.isolation, profile: p3 };
        assert_ne!(r1.digest().unwrap(), r3.digest().unwrap());
        assert_eq!(r1.digest().unwrap().len(), 64);
    }

    fn caps(landlock: Option<i32>, userns: bool, seccomp: bool) -> HostCaps {
        HostCaps { os: "linux".into(), landlock_abi: landlock, userns, seccomp, container_runtime: None }
    }

    #[test]
    fn resolve_workspace_needs_nothing() {
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        assert!(resolve(&p, &caps(None, false, false)).is_ok());
    }

    #[test]
    fn resolve_process_requires_landlock_and_seccomp() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        // Fully capable host: ok.
        assert!(resolve(&p, &caps(Some(3), true, true)).is_ok());
        // No Landlock (the WSL2 case): refuse, mention Landlock.
        let err = resolve(&p, &caps(None, true, true)).unwrap_err();
        assert!(err.to_string().contains("Landlock"), "{err}");
        // No userns with net deny: refuse.
        let err = resolve(&p, &caps(Some(3), false, true)).unwrap_err();
        assert!(err.to_string().contains("user namespaces"), "{err}");
        // net=host doesn't need userns.
        let mut host_net = Profile::builtin("default", IsolationClaim::Process);
        host_net.net_mode = NetMode::Host;
        assert!(resolve(&host_net, &caps(Some(1), false, true)).is_ok());
    }

    #[test]
    fn resolve_refuses_unimplemented_backends() {
        for claim in [IsolationClaim::HardenedContainer, IsolationClaim::Microvm] {
            let p = Profile::builtin("default", claim);
            let err = resolve(&p, &caps(Some(5), true, true)).unwrap_err();
            assert!(err.to_string().contains("backend"), "{err}");
        }
    }

    fn caps_with_container(runtime: Option<&str>) -> HostCaps {
        HostCaps {
            os: "linux".into(),
            landlock_abi: Some(3),
            userns: true,
            seccomp: true,
            container_runtime: runtime.map(str::to_owned),
        }
    }

    #[test]
    fn resolve_container_requires_runtime_and_image() {
        // No runtime on the host → refuse, mention podman.
        let mut p = Profile::builtin("default", IsolationClaim::Container);
        p.image = Some("docker.io/library/debian:stable-slim".into());
        let err = resolve(&p, &caps_with_container(None)).unwrap_err();
        assert!(err.to_string().contains("podman"), "{err}");

        // Runtime present but no image → refuse, mention image.
        let no_img = Profile::builtin("default", IsolationClaim::Container);
        let err = resolve(&no_img, &caps_with_container(Some("podman"))).unwrap_err();
        assert!(err.to_string().contains("image"), "{err}");

        // Runtime + image → resolves.
        assert!(resolve(&p, &caps_with_container(Some("podman"))).is_ok());
    }

    #[test]
    fn net_egress_allowed_under_container_refused_under_process() {
        // Under process, a domain allowlist fails closed (validate_profile).
        let mut proc = Profile::builtin("p", IsolationClaim::Process);
        proc.net_egress = vec!["pypi.org".into()];
        assert!(validate_profile(&proc).is_err());

        // Under container, it is permitted.
        let mut cont = Profile::builtin("c", IsolationClaim::Container);
        cont.net_egress = vec!["pypi.org".into()];
        cont.image = Some("img".into());
        assert!(validate_profile(&cont).is_ok());
        assert!(resolve(&cont, &caps_with_container(Some("podman"))).is_ok());
    }

    #[test]
    fn resolve_process_refused_off_linux() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        let mac = HostCaps { os: "macos".into(), landlock_abi: None, userns: false, seccomp: false, container_runtime: None };
        let err = resolve(&p, &mac).unwrap_err();
        assert!(err.to_string().contains("Linux-only"), "{err}");
    }

    #[test]
    fn workspace_run_executes_in_workdir_with_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        let policy = ResolvedPolicy { claim: IsolationClaim::Workspace, profile: p };
        let out = run(&policy, dir.path(), &["pwd".to_string()]).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        let printed = String::from_utf8_lossy(&out.stdout);
        let canon = dir.path().canonicalize().unwrap();
        assert_eq!(printed.trim(), canon.to_string_lossy());
    }

    #[test]
    fn wall_clock_kill_fires() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Profile::builtin("default", IsolationClaim::Workspace);
        p.wall_secs = 1;
        let policy = ResolvedPolicy { claim: IsolationClaim::Workspace, profile: p };
        let out = run(&policy, dir.path(), &["sleep".to_string(), "30".to_string()]).unwrap();
        assert!(out.timed_out, "expected the wall-clock kill to fire");
        assert_ne!(out.exit_code, Some(0));
    }

    #[test]
    fn run_records_resource_usage() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        let policy = ResolvedPolicy { claim: IsolationClaim::Workspace, profile: p };
        // A command that burns a little wall time so the numbers are non-trivial.
        let out = run(&policy, dir.path(), &["sh".into(), "-c".into(), "sleep 0.2".into()]).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.wall_ms >= 150, "wall_ms should reflect the ~200ms sleep: {}", out.wall_ms);
        // On Linux wait4 fills ru_maxrss (KiB) — a real process is > 0.
        #[cfg(target_os = "linux")]
        assert!(out.max_rss_kb.unwrap_or(0) > 0, "expected a peak RSS reading");
    }

    #[test]
    fn tools_allowlist_enforced_when_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Profile::builtin("default", IsolationClaim::Workspace);
        p.tools = vec!["echo".into(), "python".into()];
        let policy = ResolvedPolicy { claim: IsolationClaim::Workspace, profile: p };
        // Listed program (by basename) runs.
        assert!(run(&policy, dir.path(), &["echo".into(), "hi".into()]).is_ok());
        // An unlisted program is refused before it ever executes.
        let err = run(&policy, dir.path(), &["sh".into(), "-c".into(), "echo no".into()]).unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
    }

    #[test]
    fn empty_tools_allowlist_allows_anything() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        assert!(p.tools.is_empty());
        let policy = ResolvedPolicy { claim: IsolationClaim::Workspace, profile: p };
        assert!(run(&policy, dir.path(), &["true".into()]).is_ok());
    }
}
