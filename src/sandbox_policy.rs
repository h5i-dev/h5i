//! Pure sandbox **policy vocabulary** — the data types that describe an
//! isolation policy, with no dependency on the confinement *machinery* or the
//! runtime backends.
//!
//! This module is a dependency leaf (it imports only [`crate::error`] and
//! [`crate::idents`]). It exists so backend modules like [`crate::container`]
//! and [`crate::secrets_broker`] can name these types without depending on
//! [`crate::sandbox`] — the module that *dispatches* to those very backends.
//! That dispatch edge would otherwise form a `sandbox → container → sandbox`
//! cycle. `sandbox` re-exports everything here, so `crate::sandbox::IsolationClaim`
//! and friends keep resolving for callers that legitimately use the machinery too.
//!
//! Keep this module free of backend/machinery imports.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

use crate::error::H5iError;

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
    /// Our own Landlock + seccomp + netns confinement (the `sandbox` module).
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

/// Which coding-agent runtime an `agent` box is scoped to. The built-in `agent`
/// profile is *not* one-size-fits-all: a Claude box must not get Codex's
/// credentials (or egress to OpenAI), and vice versa — granting both makes a
/// prompt-injected agent able to read the *other* runtime's API token and use
/// it against an allowlisted host. Each runtime gets only its own HOME state +
/// API endpoints.
///
/// The per-runtime grant helpers are `pub(crate)`: the type lives here (so the
/// container backend can name it), but `Profile::builtin_agent` in
/// [`crate::sandbox`] is what assembles them into a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    Claude,
    Codex,
}

impl AgentRuntime {
    /// Map an agent identity (`$H5I_AGENT`, e.g. `claude`/`codex`) to the
    /// runtime whose credentials + API the box should expose. Codex identities
    /// → Codex; everything else (claude, unknown) → Claude, the conservative
    /// default (Claude Code is the primary driver, and an unknown identity
    /// should not silently inherit OpenAI egress).
    pub fn from_identity(agent: &str) -> AgentRuntime {
        if agent.trim().to_ascii_lowercase().starts_with("codex") {
            AgentRuntime::Codex
        } else {
            AgentRuntime::Claude
        }
    }

    /// Detect the runtime from the ambient `$H5I_AGENT`, defaulting to Claude.
    /// Used when the bare `agent` profile is resolved — `env create` runs with
    /// `$H5I_AGENT` set to the creating agent, so the box is scoped to whoever
    /// built it. Explicit `agent-claude`/`agent-codex` profiles bypass this.
    pub(crate) fn detect() -> AgentRuntime {
        std::env::var(crate::idents::AGENT_ENV)
            .ok()
            .map(|s| AgentRuntime::from_identity(&s))
            .unwrap_or(AgentRuntime::Claude)
    }

    /// The built-in profile name that pins this runtime explicitly.
    pub fn profile_name(self) -> &'static str {
        match self {
            AgentRuntime::Claude => "agent-claude",
            AgentRuntime::Codex => "agent-codex",
        }
    }

    /// Recover the runtime a profile name pins, if it is one of the built-in
    /// agent profiles. `None` for `default`/custom profiles (which could run
    /// either runtime). Used to decide runtime-specific box hardening.
    pub fn from_profile_name(name: &str) -> Option<AgentRuntime> {
        match name {
            "agent-claude" => Some(AgentRuntime::Claude),
            "agent-codex" => Some(AgentRuntime::Codex),
            _ => None,
        }
    }

    /// Read-write HOME state this runtime needs — its *own* credentials/config
    /// only. Never the other runtime's.
    pub(crate) fn state_write(self) -> &'static [&'static str] {
        match self {
            AgentRuntime::Claude => &["~/.claude", "~/.claude.json"],
            AgentRuntime::Codex => &["~/.codex"],
        }
    }

    /// Read-only `~/.local/share/<runtime>` subtree holding the runtime's own
    /// installed binary (`claude` is a launcher → `~/.local/share/claude/...`).
    /// Scoped per-runtime so the box never sees unrelated `~/.local/share`
    /// state (Jupyter secrets, history DBs, …).
    pub(crate) fn share_read(self) -> &'static str {
        match self {
            AgentRuntime::Claude => "~/.local/share/claude",
            AgentRuntime::Codex => "~/.local/share/codex",
        }
    }

    /// The API endpoints this runtime is allowed to reach. Scoped per-runtime so
    /// a Claude box cannot egress to OpenAI (and so a stolen cross-runtime token
    /// would have nowhere allowlisted to go).
    pub(crate) fn egress(self) -> &'static [&'static str] {
        match self {
            AgentRuntime::Claude => &["api.anthropic.com", "statsig.anthropic.com"],
            AgentRuntime::Codex => &["api.openai.com", "auth.openai.com", "chatgpt.com"],
        }
    }
}

/// One structural in-box git path: a piece of the repo's `.git` plumbing the
/// container backend bind-mounts at its *identical host path* inside the box,
/// so the worktree's gitdir/commondir pointer files resolve. Computed at run
/// time from the env manifest (see `env::box_git_grants`).
#[derive(Debug, Clone)]
pub struct BoxGitPath {
    pub host: PathBuf,
    pub rw: bool,
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

impl Profile {
    pub fn builtin(name: &str, isolation: IsolationClaim) -> Profile {
        let confined = isolation >= IsolationClaim::Process;
        Profile {
            name: name.to_string(),
            isolation,
            fs_read: if confined { default_fs_read() } else { Vec::new() },
            // /dev/null and /dev/zero are write-granted sinks: every shell
            // pipeline redirects to them, and granting write reveals nothing.
            fs_write: if confined {
                vec!["$WORK".to_string(), "/dev/null".to_string(), "/dev/zero".to_string()]
            } else {
                Vec::new()
            },
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
            // TERM/COLORTERM so interactive sessions (`env shell`) render: a
            // TUI without TERM is garbage on screen. Harmless for captured runs.
            env_pass: vec![
                "PATH".into(),
                "HOME".into(),
                "LANG".into(),
                "TERM".into(),
                "COLORTERM".into(),
            ],
        }
    }

    /// Built-in **`agent`** profile (`--profile agent`): the agent-in-box
    /// defaults, scoped to a single `runtime` (Claude or Codex). The base
    /// built-in confines to system paths + `$WORK`, which is right for
    /// build/test workloads but bricks coding agents — `claude` / `codex` live
    /// under `$HOME`, keep state there, and need egress to their APIs. This
    /// profile adds the minimum HOME surface *that runtime* needs:
    ///
    /// - read-only (shared, non-secret): `~/.local/bin` (PATH shims),
    ///   `~/.local/lib` (user site-packages for tooling), `~/.nvm` (node
    ///   installs), shell rc files, `~/.gitconfig` + `~/.config/git` (commit
    ///   identity), and the runtime's own `~/.local/share/<runtime>` binary;
    /// - read-write: **only this runtime's** state (Claude →
    ///   `~/.claude`/`~/.claude.json`, Codex → `~/.codex`), shared caches
    ///   (`~/.cache`, `~/.npm`), and `/tmp` (host-shared at this tier; the
    ///   container tier gives a private one);
    /// - `net.egress`: **only this runtime's** API endpoints, DNS-pinned +
    ///   nftables-enforced at the supervised tier (the lint refuses egress at
    ///   tiers that cannot enforce it — `agent` is a supervised/container
    ///   profile by design);
    /// - `USER`/`SHELL` passed through; roomier mem/procs for a live agent.
    ///
    /// The default deny set (`~/.ssh`, `~/.aws`, `~/.config/gh`, hooks) still
    /// applies — none of the grants contains a denied child. Deliberate trade:
    /// the agent can read its *own* credentials (it cannot function without
    /// them), and the egress allowlist bounds where bytes can go. Crucially the
    /// box gets *neither the other runtime's credentials nor egress to its API*
    /// — a Claude box cannot read `~/.codex/auth.json` and use it against
    /// OpenAI, and the broad `~/.local` read no longer exposes unrelated
    /// `~/.local/share` secrets (Jupyter tokens, history DBs).
    pub fn builtin_agent(isolation: IsolationClaim, runtime: AgentRuntime) -> Profile {
        let mut p = Profile::builtin(runtime.profile_name(), isolation);
        p.fs_read.extend(
            [
                // Narrowed from all of `~/.local`: PATH shims + user libs only.
                // NOT `~/.local/share` wholesale (Jupyter notebook_secret, app
                // history DBs) — the runtime's own share dir is added below.
                "~/.local/bin",
                "~/.local/lib",
                "~/.nvm",
                // PATH shims + rustup toolchain metadata only — NOT ~/.cargo
                // itself (credentials.toml). The crate caches (registry/git)
                // are granted read-only below: pure download caches with no
                // secrets, so offline `cargo build/test` resolves deps in-box.
                "~/.cargo/env",
                "~/.cargo/bin",
                "~/.cargo/config",
                "~/.cargo/config.toml",
                // Read-only crate caches so an offline cargo build can resolve
                // dependencies in-box (network egress is API-only). These hold
                // the downloaded registry index + crate sources / git checkouts,
                // never credentials (`~/.cargo/credentials.toml` stays ungranted).
                "~/.cargo/registry",
                "~/.cargo/git",
                "~/.rustup/settings.toml",
                "~/.rustup/toolchains",
                "~/.bashrc",
                "~/.bash_profile",
                "~/.profile",
                "~/.inputrc",
                "~/.gitconfig",
                "~/.config/git",
            ]
            .map(String::from),
        );
        // The runtime's own installed binary (e.g. `~/.local/bin/claude` is a
        // launcher resolving into `~/.local/share/claude/versions/...`).
        p.fs_read.push(runtime.share_read().to_string());
        // Read-write: this runtime's own state only, plus shared caches.
        p.fs_write.extend(runtime.state_write().iter().map(|s| s.to_string()));
        p.fs_write.extend(["~/.cache", "~/.npm", "/tmp"].map(String::from));
        // The agent's own controlling terminal (TUIs re-open /dev/tty for raw
        // input). Deliberately NOT /dev/pts — that subtree includes the user's
        // *other* host terminals (same-uid writable → injection channel).
        p.fs_write.push("/dev/tty".into());
        // Egress: this runtime's API endpoints only.
        p.net_egress = runtime.egress().iter().map(|s| s.to_string()).collect();
        p.env_pass.extend(["USER".into(), "SHELL".into()]);
        p.mem_bytes = Some(8 * 1024 * 1024 * 1024);
        p.max_procs = Some(512);
        p
    }

    pub fn wall(&self) -> Duration {
        Duration::from_secs(self.wall_secs)
    }
}

// ─── audit + resolved policy ────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCapture {
    #[default]
    Signal,
    All,
}

impl AuditCapture {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditCapture::Signal => "signal",
            AuditCapture::All => "all",
        }
    }

    pub fn parse(s: &str) -> Result<Self, H5iError> {
        match s {
            "signal" => Ok(AuditCapture::Signal),
            "all" => Ok(AuditCapture::All),
            other => Err(H5iError::Metadata(format!(
                "audit capture mode '{other}' is not available (use signal or all)"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditPolicy {
    #[serde(default)]
    pub capture: AuditCapture,
}

impl Default for AuditPolicy {
    fn default() -> Self {
        AuditPolicy {
            capture: AuditCapture::Signal,
        }
    }
}

/// The policy as actually enforced: profile + resolved claim. Serialized as
/// `policy.resolved.toml`; its digest is pinned in the env manifest and in
/// every capture taken inside the env.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    pub claim: IsolationClaim,
    pub profile: Profile,
    #[serde(default)]
    pub audit: AuditPolicy,
    /// Runtime-only in-box git mounts for the container backend — never
    /// serialized (`policy.resolved.toml` and its pinned digest are unchanged;
    /// these are structural like the implicit `$WORK` mount, not policy).
    #[serde(skip)]
    pub box_git: Vec<BoxGitPath>,
    /// Runtime-only env capture spool. In-box `h5i capture run` writes here;
    /// the host ingests records into the real object store after the run/shell.
    #[serde(skip)]
    pub env_capture_spool: Option<PathBuf>,
}

impl ResolvedPolicy {
    pub fn new(claim: IsolationClaim, profile: Profile) -> Self {
        ResolvedPolicy {
            claim,
            profile,
            audit: AuditPolicy::default(),
            box_git: Vec::new(),
            env_capture_spool: None,
        }
    }

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
