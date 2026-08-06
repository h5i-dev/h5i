//! Pure sandbox **policy vocabulary** — the data types that describe an
//! isolation policy, with no dependency on the confinement *machinery* or the
//! runtime backends.
//!
//! This module is a dependency leaf (it imports only [`crate::error`]). It
//! exists so backend modules like [`crate::container`]
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

/// The agent-identity env var (`$H5I_AGENT`). Kept local to the sandbox layer
/// so this crate never reaches back into core for the constant.
const AGENT_ENV: &str = "H5I_AGENT";

/// Absolute in-box path where the managed-settings.json (carrying the
/// unkillable wrap-bash observation hook) is bind-mounted for the in-box
/// Claude. It's the sandbox's bind target, so it lives here; `hooks` (host
/// side) imports it from this crate.
pub const CLAUDE_MANAGED_SETTINGS_PATH: &str = "/etc/claude-code/managed-settings.json";

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
    /// **microVM** adapter (opt-in shell-out to microsandbox's `msb`). A
    /// hardware-isolated guest with its own kernel, and the first tier whose
    /// `net.egress` allowlist is enforced by address in the VM's network stack
    /// rather than by an HTTP proxy the box could decline to use. Requires
    /// virtualization on the host (KVM / Apple Silicon); refused, never
    /// downgraded, when it is absent (`microvm.rs`).
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

    /// Does this tier run the command inside a **base image** with the workspace
    /// mounted in, rather than against the host filesystem under a kernel
    /// policy? True for `container` and `microvm`.
    ///
    /// Ask this instead of testing `== Container` wherever the reason is
    /// structural — the box needs its git plumbing mounted, its spool and inbox
    /// mounted, its shell rc coming from the image, its host paths free of the
    /// mount syntax's separator. Those facts follow from "the box is an image",
    /// not from which runtime boots it, and a tier added later should inherit
    /// them by construction rather than by someone remembering to grep.
    ///
    /// `hardened-container` is deliberately **not** included: it has no adapter
    /// in this build, so claiming its structural shape would be claiming a
    /// backend that does not exist.
    pub fn image_backed(&self) -> bool {
        matches!(self, Self::Container | Self::Microvm)
    }

    /// Does this tier enforce a `net.egress` **domain allowlist**? The kernel
    /// tiers can deny all outbound or allow all of it and nothing in between,
    /// so a non-empty allowlist there fails closed.
    pub fn enforces_egress_allowlist(&self) -> bool {
        matches!(self, Self::Container | Self::Microvm)
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
    /// `env` (default) | `file` — see [`SecretGrant::inject_or_default`], which
    /// is the authority on the default and explains why it is `env`.
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
        std::env::var(AGENT_ENV)
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
            // A `browser` box is an agent box with a browser in it, and it is
            // built from whoever is driving it — so it engages the credential
            // proxy exactly like `agent` does. Without this it would be the one
            // agent profile that quietly had no brokered credential.
            "browser" | "agent" => Some(AgentRuntime::detect()),
            _ => None,
        }
    }

    /// Read-write HOME state this runtime needs — its *own* credentials/config
    /// only. Never the other runtime's. `pub`: core's `env` reads it to seed
    /// the per-env HOME copy.
    pub fn state_write(self) -> &'static [&'static str] {
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

/// One declared private path (Idea 3): a workspace-relative directory that gets
/// its own per-env backing store, so concurrent envs of the same repo don't
/// collide on inode-level `flock`/`fcntl` locks or single-writer build caches
/// (Cargo `target/`, Next `.next/dev/lock`, Gradle, …). Mount-namespace
/// isolation alone doesn't fix this — `flock` is on the inode, not the mount —
/// so each env binds a distinct backing dir over the path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivatePath {
    /// Workspace-relative path (validated: relative, no `..`, no overlap).
    pub path: String,
    /// `cache` | `scratch` | `private`. Advisory label for reviewers; all are
    /// per-env isolated in v1 (distinct inode → no cross-env lock contention).
    pub kind: String,
    /// Keep the backing dir across runs (`true`, e.g. a warm build cache) or
    /// wipe it before each run (`false`, e.g. a stale dev-server lock dir).
    pub persist: bool,
}

/// A resolved private-path bind (runtime only — never serialized/digested): the
/// host backing dir and the workspace-relative path it shadows. Mirrors how
/// [`BoxGitPath`] carries container binds; computed at run time in
/// `env::prepare_private_paths`.
#[derive(Debug, Clone)]
pub struct PrivateBind {
    /// Host backing dir under the env's `private/` tree (distinct inode per env).
    pub backing: PathBuf,
    /// Workspace-relative path the backing dir shadows (e.g. `target`).
    pub rel: String,
}

/// A resolved HOME-state redirect bind (runtime only — never serialized/digested):
/// a per-env *copy* of the agent runtime's credential/session state (`~/.claude`,
/// `~/.claude.json`, `~/.codex`) bound over the real absolute HOME path inside the
/// box. Seeded once from the real HOME (copy-in) and persisted per-env, so every
/// in-box write — token refresh, session history — stays in the env's own copy and
/// the real `~/.claude.json` is never raced by concurrent boxes. Unlike
/// [`PrivateBind`] the target is an absolute host path (the real `$HOME/…`), not a
/// workspace-relative one. Computed at run time in `env::prepare_home_state`.
/// Authenticated egress to one host, with the credential staying on the host.
///
/// The shape is deliberately the one `auth_proxy` already implements: a reverse
/// proxy the box is pointed at, not a forward proxy that would have to
/// terminate TLS. The limit that follows is real and worth knowing before you
/// declare one — it binds clients you can point at another origin (anything
/// with a base-URL setting), and a plain `curl https://<host>` still goes
/// nowhere.
///
/// ```toml
/// [[profile.review.auth]]
/// host = "api.github.com"
/// credential_env = "GITHUB_TOKEN"   # read on the host, never in the box
/// base_url_var = "GH_HOST"          # what the client reads
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthGrant {
    /// Upstream host the proxy will ever connect to (an SSRF pin).
    pub host: String,
    /// Host-side environment variable holding the credential.
    pub credential_env: String,
    /// Environment variable the box is given, pointing at the proxy.
    pub base_url_var: String,
}

#[derive(Debug, Clone)]
pub struct RoBind {
    /// Host directory the box reads through the bind.
    pub backing: PathBuf,
    /// Absolute path inside the box the backing shadows.
    pub target: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HomeBind {
    /// Host backing copy under the env's `home/` tree (distinct inode per env).
    pub backing: PathBuf,
    /// Absolute real HOME path the backing copy shadows (e.g. `/home/u/.claude`).
    pub target: PathBuf,
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
    /// Authenticated egress grants (see [`AuthGrant`]). Part of the profile and
    /// therefore of the pinned digest: which hosts a box may authenticate
    /// against is exactly the kind of thing a reviewer must see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth: Vec<AuthGrant>,
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
    /// Base OCI image for the image-backed tiers — `isolation=container` and
    /// `isolation=microvm`, both of which require it. The command runs inside it
    /// with the workspace mounted at `/work`. Named `container.image` in
    /// `.h5i/env.toml` for compatibility; the microvm tier boots the same
    /// images.
    pub image: Option<String>,
    /// Environment-variable allowlist — the child gets *only* these (plus
    /// nothing else; secrets are never inherited wholesale).
    pub env_pass: Vec<String>,
    /// Per-env private paths (Idea 3). Empty for most policies, so their digest
    /// is unchanged (serialized only when non-empty; appended last to keep the
    /// canonical field order — and existing digests — stable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_paths: Vec<PrivatePath>,
    /// Opt-in for the secrets broker's `command:` extractor, which runs host-side
    /// code *outside* the sandbox to mint a credential. Off by default; pinned in
    /// the policy digest so it can't be enabled without a tamper-evident change.
    /// Serialized only when `true`, so existing digests are unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_command_extractors: bool,
    /// Custom bash rcfile for interactive `env shell` sessions, as a path
    /// **relative to `$WORK`** (the env worktree) — e.g. `.h5i/box.bashrc`. When
    /// unset, `env shell` launches bash with a generated *plain* rcfile instead
    /// of the host `~/.bashrc` (which, under confinement, often references tools
    /// the sandbox blocks — e.g. `~/.local/bin/powerline-shell`). Set this to
    /// opt a specific env back into a richer, version-controlled rc. Serialized
    /// only when set, so existing policy digests are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_rcfile: Option<String>,
    /// Persona source files (`[profile.X] persona = ["plugin/persona/architect.md"]`),
    /// each a path **relative to `$WORK`** (the env worktree). At `env create`
    /// their contents are concatenated, in order, into a single `PERSONA.md` at
    /// the worktree root — the agent's standing working style, loaded via
    /// `@PERSONA.md` in `CLAUDE.md` (Claude) or a read-`PERSONA.md` instruction
    /// in `AGENTS.md` (Codex). Empty for most profiles, so existing policy
    /// digests are unchanged (serialized only when non-empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persona: Vec<String>,
    /// Allow `AF_UNIX` sockets (`[profile.X.net] unix = true`). Off by default:
    /// `SCM_RIGHTS` passes file descriptors, which is authority smuggling, so
    /// the supervised tier's `socket()` gate denies the family unless a profile
    /// asks for it.
    ///
    /// What the grant does *not* open, and this is why it can be granted at all:
    ///
    /// - **Abstract-namespace sockets** (leading NUL) are scoped by the network
    ///   namespace, and a supervised box has a private one. They cannot name
    ///   anything the host is listening on.
    /// - **Filesystem-bound sockets** are scoped by Landlock, so a `connect()`
    ///   can only name a path the profile already granted. The usual host
    ///   sockets — `/tmp/.X11-unix`, `tmux-*`, an `ssh-agent` — live under
    ///   `/tmp`, which the kernel tiers redirect to a per-env scratch.
    ///
    /// What is left is a host socket sitting inside a granted path, and that is
    /// a real residual: granting this widens the box by exactly the IPC
    /// endpoints its own filesystem grants can reach. It is opt-in per profile
    /// and pinned in the digest for that reason.
    ///
    /// The `browser` profile sets it because the `agent-browser` daemon's
    /// control socket is a filesystem-bound `AF_UNIX` listener; without the
    /// grant the daemon dies at startup with `Failed to bind socket:
    /// Operation not permitted`.
    ///
    /// Appended last, and serialized only when `true`, so every existing
    /// profile's canonical serialization — and its pinned digest — is unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unix_sockets: bool,

    /// macOS only: allow `mach-register` and `iokit-open`.
    ///
    /// The base profile already grants `mach-lookup`, which is enough to *find*
    /// a service. A multi-process browser also **registers** mach ports of its
    /// own — that is how the browser process and its renderers talk — and opens
    /// IOKit even with `--headless` and `--disable-gpu`. Neither is covered by
    /// `mach-lookup`, and under `(deny default)` Chrome does not report the
    /// refusal: it dies with `SEGV_ACCERR` at a null address, several layers
    /// after the denial that caused it.
    ///
    /// Bisected against a real profile rather than copied from Chromium's own
    /// sandbox: with unrestricted reads *and* writes Chrome still crashed, and
    /// with these two operations added to h5i's ordinary narrow grants it
    /// started. Each alone is not enough; both together are.
    ///
    /// What it widens: the box may register mach service names in its own
    /// bootstrap namespace and open IOKit user clients. It is opt-in per
    /// profile, set only by `browser`, and pinned in the digest — the same
    /// treatment as [`Profile::unix_sockets`], for the same reason.
    ///
    /// Appended last, and serialized only when `true`, so every existing
    /// profile's canonical serialization — and its pinned digest — is unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mach_iokit: bool,
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
            auth: Vec::new(),
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
            private_paths: Vec::new(),
            allow_command_extractors: false,
            shell_rcfile: None,
            persona: Vec::new(),
            unix_sockets: false,
            mach_iokit: false,
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
    ///   (`~/.cache`, `~/.npm`), and `/tmp` (redirected to per-env scratch by
    ///   env launch on kernel tiers; private in containers);
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

    /// Built-in **`browser`** profile (`--profile browser`): the agent-in-box
    /// profile plus the surface a headless Chrome and the `agent-browser`
    /// daemon need, so the agent can drive a real browser against the dev
    /// server it just started.
    ///
    /// The browser runs in the **same box** as the agent. The dev server is
    /// then on `localhost:3000` for both of them with no namespace sharing, no
    /// pod and no second image: at the kernel tiers the "image" is the host
    /// filesystem under Landlock grants, so a browser box is a profile, not an
    /// architecture (roadmap 5.2).
    ///
    /// What is added, and why each piece:
    ///
    /// - **read**: the discovered Chrome and `agent-browser` binaries and their
    ///   resource trees, plus fontconfig and the system fonts — Chrome exits
    ///   rather than rendering without them.
    /// - **write**: `/dev/shm` (Chrome's shared-memory transport; without it
    ///   the renderer dies unless every caller remembers
    ///   `--disable-dev-shm-usage`).
    /// - Loopback is reachable inside the box's own network namespace, so the
    ///   dev server needs no grant at all. External egress stays whatever the
    ///   agent profile allows: the browser gets no wider reach than the agent.
    ///
    /// Chrome's *own* sandbox does not run here. Our seccomp deny-list blocks
    /// the namespace syscalls it needs, at every tier, so Chrome is launched
    /// with `--no-sandbox` and h5i's box is the boundary. That is one layer
    /// fewer than a browser on the host has, and it is stated in the roadmap's
    /// limits rather than hidden.
    pub fn builtin_browser(isolation: IsolationClaim, runtime: AgentRuntime) -> Profile {
        let mut p = Profile::builtin_agent(isolation, runtime);
        p.name = "browser".to_string();
        // Host paths are how the kernel tiers get a browser: the "image" there
        // is the host filesystem under Landlock grants. A container brings its
        // own Chrome and agent-browser, so granting host paths would be noise
        // in a digest and a lie in the policy — the box cannot see them.
        if isolation < IsolationClaim::Container {
            for path in browser_read_grants() {
                if !p.fs_read.contains(&path) {
                    p.fs_read.push(path);
                }
            }
        }
        // Chrome's shared-memory transport. Granting it is better than relying
        // on every caller to pass --disable-dev-shm-usage.
        p.fs_write.push("/dev/shm".into());
        // Chrome's browser↔renderer IPC and its IOKit use. See
        // [`Profile::mach_iokit`]; without it Chrome dies with a bare SEGV.
        p.mach_iokit = true;
        // Chrome's ProcessSingleton lock socket. It lives in the *macOS
        // per-user* temp dir, which Chrome resolves with
        // `confstr(_CS_DARWIN_USER_TEMP_DIR)` and NOT from `TMPDIR` — so the
        // per-env `/tmp` redirect does not move it, and a box that cannot write
        // there fails with "Failed to create socket directory", which reads as
        // a path problem and is a grant problem.
        //
        // This is the widest thing the browser profile asks for: that directory
        // is shared with the host user's own processes and with other browser
        // boxes, which is the cross-agent rendezvous point the kernel tiers
        // otherwise remove by redirecting `/tmp`. Scoped to this profile, and
        // stated here rather than discovered later.
        if let Some(d) = darwin_user_temp_dir() {
            p.fs_read.push(d.clone());
            p.fs_write.push(d);
        }
        // The agent-browser daemon's control socket is a filesystem-bound
        // AF_UNIX listener under `AGENT_BROWSER_SOCKET_DIR`, and the supervised
        // tier's socket() gate denies that family by default. Without this the
        // daemon exits at startup with "Failed to bind socket: Operation not
        // permitted" — and, because it redirects its own stderr to /dev/null
        // before failing, the caller sees "exited during startup with no error
        // output". See `Profile::unix_sockets` for what the grant does and does
        // not widen.
        p.unix_sockets = true;
        // A browser is heavier than a build: renderers are processes and tabs
        // are memory.
        p.mem_bytes = Some(12 * 1024 * 1024 * 1024);
        p.max_procs = Some(1024);
        p
    }

    pub fn wall(&self) -> Duration {
        Duration::from_secs(self.wall_secs)
    }
}


/// The per-user temp directory macOS hands out through
/// `confstr(_CS_DARWIN_USER_TEMP_DIR)` — `/var/folders/<xx>/<yy>/T`.
///
/// Distinct from `TMPDIR`, and that distinction is the whole reason this
/// exists: programs that ask the OS rather than the environment land here, so a
/// box's `TMPDIR` redirect does not move them. Returned without its trailing
/// slash; `seatbelt::path_aliases` adds the `/private` spelling, which Seatbelt
/// needs as well.
#[cfg(target_os = "macos")]
fn darwin_user_temp_dir() -> Option<String> {
    // `_CS_DARWIN_USER_TEMP_DIR`, which libc does not re-export.
    const CS_DARWIN_USER_TEMP_DIR: libc::c_int = 65537;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    let n = unsafe {
        libc::confstr(
            CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    // 0 is failure; > len means the buffer was too small to hold the answer.
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n.saturating_sub(1)); // drop the trailing NUL
    let s = String::from_utf8(buf).ok()?;
    let s = s.trim_end_matches('/').to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(target_os = "macos"))]
fn darwin_user_temp_dir() -> Option<String> {
    None
}

// ─── browser discovery ──────────────────────────────────────────────────────

/// Candidate Chrome/Chromium binaries, in preference order. `agent-browser
/// install` downloads Chrome for Testing under the user's cache, and a
/// Playwright install is a common existing source; a system Chrome is used when
/// neither is present.
/// Non-existent entries are filtered out by [`browser_read_grants`], so the
/// macOS and Linux locations can both live here without either host being
/// granted a path it does not have.
const CHROME_CANDIDATES: &[&str] = &[
    // Where `agent-browser install` — the command h5i's own "not there" error
    // tells you to run — actually puts the build, on every platform. The two
    // XDG-shaped paths below are an older layout and are kept for installs that
    // predate it.
    "~/.agent-browser/browsers",
    "~/.cache/agent-browser",
    "~/.local/share/agent-browser",
    "~/.cache/ms-playwright",
    "/opt/google/chrome",
    "/usr/lib/chromium",
    "/usr/lib/chromium-browser",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    // macOS. A browser installed the ordinary way is an app bundle, not a
    // binary on PATH, and the whole bundle is granted because Chrome loads its
    // framework and resources from inside it. Without these, a Mac with Chrome
    // sitting in /Applications was told to go and install a second copy.
    "/Applications/Google Chrome.app",
    "/Applications/Google Chrome for Testing.app",
    "/Applications/Chromium.app",
    "~/Applications/Google Chrome.app",
    "~/Applications/Google Chrome for Testing.app",
    "~/Applications/Chromium.app",
];

/// Candidate `agent-browser` binaries: cargo install, a user-local npm bin, a
/// system install.
const AGENT_BROWSER_CANDIDATES: &[&str] = &[
    "~/.cargo/bin/agent-browser",
    "~/.local/bin/agent-browser",
    "/usr/local/bin/agent-browser",
    "/usr/bin/agent-browser",
    // Homebrew on Apple Silicon. `/usr/local/bin` above covers the Intel
    // prefix only, so `brew install agent-browser` on every current Mac landed
    // somewhere h5i never looked.
    "/opt/homebrew/bin/agent-browser",
];

/// Font and rendering support Chrome needs to start at all.
const BROWSER_SUPPORT_PATHS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "~/.fonts",
    "~/.local/share/fonts",
    "/etc/fonts",
    "~/.config/fontconfig",
    "~/.cache/fontconfig",
];

/// Expand `~` against `$HOME`, leaving other paths untouched.
fn expand_home(path: &str) -> Option<std::path::PathBuf> {
    expand_home_in(path, std::env::var("HOME").ok().as_deref())
}

/// [`expand_home`] with the home directory supplied rather than read from the
/// environment — the pure half, so it can be tested without `set_var`. Cargo
/// runs tests as threads of one process, so a test that reassigns `HOME`
/// reassigns it for every concurrently-running test too, including the ones
/// right above that resolve real paths under the real home.
fn expand_home_in(path: &str, home: Option<&str>) -> Option<std::path::PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => home
            .filter(|h| !h.is_empty())
            .map(|h| std::path::PathBuf::from(h).join(rest)),
        None => Some(std::path::PathBuf::from(path)),
    }
}

/// Every candidate browser path that exists on this host, as profile grants.
///
/// Only existing paths are granted: a Landlock allowlist naming a path that is
/// not there is noise, and it makes the resolved policy (which is digested)
/// depend on wishful thinking rather than on what the box can actually reach.
pub fn browser_read_grants() -> Vec<String> {
    CHROME_CANDIDATES
        .iter()
        .chain(AGENT_BROWSER_CANDIDATES)
        .chain(BROWSER_SUPPORT_PATHS)
        .filter(|c| expand_home(c).map(|p| p.exists()).unwrap_or(false))
        .map(|c| c.to_string())
        .collect()
}

/// Did we find anything that could actually run a browser **on this host**?
///
/// Only meaningful for the kernel tiers, where the box reaches the host
/// filesystem. A container box gets its browser from the image, so `create`
/// does not consult this there.
/// The `agent-browser` binary on this host, if there is one.
///
/// The launch-and-attach shim has to invoke the *real* binary by absolute path:
/// it installs itself on `PATH` under the same name, so a bare `agent-browser`
/// from inside the shim would be the shim again.
pub fn agent_browser_binary() -> Option<String> {
    AGENT_BROWSER_CANDIDATES
        .iter()
        .filter_map(|c| expand_home(c))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

pub fn browser_tooling_present() -> (bool, bool) {
    let any = |list: &[&str]| {
        list.iter()
            .any(|c| expand_home(c).map(|p| p.exists()).unwrap_or(false))
    };
    (any(CHROME_CANDIDATES), any(AGENT_BROWSER_CANDIDATES))
}

#[cfg(test)]
mod browser_profile_tests {
    use super::*;

    #[test]
    fn browser_is_the_agent_surface_plus_the_browser_surface() {
        let agent = Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude);
        let browser = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);

        assert_eq!(browser.name, "browser");
        // Everything the agent could reach, the browser box can still reach …
        for r in &agent.fs_read {
            assert!(browser.fs_read.contains(r), "lost an agent read grant: {r}");
        }
        // … and the browser's own egress is exactly the agent's, never wider.
        assert_eq!(browser.net_egress, agent.net_egress);
        // Chrome's shared-memory transport.
        assert!(browser.fs_write.iter().any(|w| w == "/dev/shm"));
        // Room for renderers.
        assert!(browser.mem_bytes > agent.mem_bytes);
        assert!(browser.max_procs > agent.max_procs);
    }

    #[test]
    fn only_the_browser_profile_gets_the_chrome_host_surface() {
        let agent = Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude);
        let browser = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);

        // Chrome registers mach ports and opens IOKit; without both it dies
        // with a bare SEGV under `(deny default)`.
        assert!(browser.mach_iokit);
        assert!(!agent.mach_iokit, "an ordinary agent box gets neither");

        // Chrome's ProcessSingleton lock socket lives in the macOS per-user temp
        // dir, which it finds via confstr and not via TMPDIR — so the per-env
        // `/tmp` redirect cannot move it and the box must be granted the real
        // directory. The widest thing this profile asks for, and only it.
        if let Some(d) = darwin_user_temp_dir() {
            assert!(browser.fs_write.contains(&d), "{:?}", browser.fs_write);
            assert!(!agent.fs_write.contains(&d), "{:?}", agent.fs_write);
        }
    }

    #[test]
    fn only_the_browser_profile_gets_af_unix() {
        // The agent-browser daemon's control socket is a filesystem-bound
        // AF_UNIX listener, and the supervised tier's socket() gate denies that
        // family by default — so the daemon exits at startup without this.
        assert!(Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude).unix_sockets);
        // Nothing else asks for it. SCM_RIGHTS passes file descriptors, so the
        // grant widens a box by the IPC endpoints its filesystem grants can
        // reach, and it stays opt-in for that reason.
        assert!(!Profile::builtin_agent(IsolationClaim::Supervised, AgentRuntime::Claude).unix_sockets);
        assert!(!Profile::builtin("default", IsolationClaim::Supervised).unix_sockets);
    }

    #[test]
    fn the_af_unix_grant_is_in_the_digest_and_only_when_asked_for() {
        // It is appended last and skipped when false, so every profile that
        // does not ask for it keeps the exact serialization — and therefore the
        // pinned digest — it had before the field existed.
        let plain = Profile::builtin("default", IsolationClaim::Supervised);
        assert!(!toml::to_string(&plain).unwrap().contains("unix_sockets"));
        // And a profile that does ask for it says so where a reviewer looks.
        let browser = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);
        assert!(toml::to_string(&browser).unwrap().contains("unix_sockets = true"));
    }

    #[test]
    fn browser_stays_runtime_scoped() {
        let claude = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);
        let codex = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Codex);
        // A browser box must not become a way around the runtime scoping: a
        // Claude browser box still cannot reach Codex state or the OpenAI API.
        assert_ne!(claude.net_egress, codex.net_egress);
        assert!(!claude.fs_write.iter().any(|w| w.contains(".codex")));
        assert!(!codex.fs_write.iter().any(|w| w.contains(".claude")));
    }

    #[test]
    fn only_paths_that_exist_are_granted() {
        // The resolved policy is digested, so a grant naming a path that is not
        // there would make the digest depend on wishful thinking.
        for g in browser_read_grants() {
            let p = expand_home(&g).expect("expandable");
            assert!(p.exists(), "granted a path that does not exist: {g}");
        }
    }

    #[test]
    fn expand_home_only_touches_tilde_paths() {
        // The home dir is passed in, not assigned to the process: `set_var`
        // here would change `HOME` for every test running beside this one.
        assert_eq!(
            expand_home_in("~/.cache/x", Some("/home/example")).unwrap(),
            std::path::PathBuf::from("/home/example/.cache/x")
        );
        assert_eq!(
            expand_home_in("/usr/bin/chromium", Some("/home/example")).unwrap(),
            std::path::PathBuf::from("/usr/bin/chromium")
        );
        // No home to expand against → no grant, rather than a bare relative path.
        assert_eq!(expand_home_in("~/.cache/x", None), None);
        assert_eq!(expand_home_in("~/.cache/x", Some("")), None);
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
    /// Runtime-only per-env inbound mailbox, mounted READ-ONLY in the box. The
    /// host fans cross-agent messages (team review requests, etc.) into it at
    /// send time; the box reads them but cannot write — so it can receive
    /// securely without write access to the shared coordination store.
    #[serde(skip)]
    pub env_inbox: Option<PathBuf>,
    /// Runtime-only per-env private-path binds (Idea 3) — never serialized; the
    /// declarative intent lives in `profile.private_paths` (which *is* digested).
    /// Populated by `env::prepare_private_paths`; applied as bind mounts on the
    /// kernel tiers (`build_confined_command`) and `--mount`s on container.
    #[serde(skip)]
    pub private_binds: Vec<PrivateBind>,
    /// Runtime-only per-env HOME-state redirect binds — never serialized. Each
    /// shadows the agent runtime's real `~/.claude`/`~/.codex` with a per-env copy
    /// so concurrent agent boxes don't race on the shared real credential/session
    /// files. Populated by `env::prepare_home_state`; applied as bind mounts on the
    /// kernel tiers (`build_confined_command`). Kernel-tier only (container's
    /// read-only rootfs never mounts host HOME, so there is no race to close there).
    #[serde(skip)]
    pub home_binds: Vec<HomeBind>,
    /// Runtime-only **read-only** binds — never serialized, so they cannot move
    /// a pinned policy digest. Each shadows a path inside the box with a host
    /// directory the box may read and never write: today the warm dependency
    /// caches (roadmap 5.8). Applied as `MS_BIND` followed by a
    /// `MS_REMOUNT | MS_RDONLY`, in the same private mount namespace and before
    /// Landlock, exactly like the interactive config lockdown.
    #[serde(skip)]
    pub ro_binds: Vec<RoBind>,
    /// The one **writable** bind, and the only way a cache is ever written.
    ///
    /// Runtime-only and serde-skipped, like `ro_binds`. Deliberately an
    /// `Option` of one rather than a list: `h5i box cache refresh` is the sole
    /// producer, it populates exactly one ecosystem's cache, and a policy that
    /// cannot express "several writable binds" cannot grow one by accident.
    #[serde(skip)]
    pub cache_write: Option<RoBind>,
    /// Runtime-only: enforce the worktree (`$WORK`) as **read-only** — a
    /// read-only observer session (`env shell --readonly`). Never serialized (it
    /// is a per-invocation enforcement mode, not policy): a readonly session and
    /// a read-write one resolve to the *same* pinned policy digest, they differ
    /// only in how `$WORK` is granted. When set, `build_confined_command` grants
    /// `$WORK` read-only (Landlock ro, not rw) so the box cannot mutate the
    /// shared worktree.
    #[serde(skip)]
    pub work_readonly: bool,
    /// Runtime-only extra egress hosts from the **host-side** user allowlist
    /// (`h5i box allow`, stored under the user config dir — never inside the
    /// repo, `$WORK`, or any box-visible path). Never serialized: the pinned
    /// `policy_digest` stays reproducible; the extras are recorded per-run in
    /// the capture manifest instead. Applied only when the digested profile
    /// already declares a non-empty `net.egress` (a deny-all profile can never
    /// be widened from outside the digested policy) and only on the container
    /// tier (the only tier that enforces a domain allowlist).
    #[serde(skip)]
    pub user_egress_allow: Vec<String>,
    /// Runtime-only: the loopback port this box's Chrome listens on for CDP,
    /// and the **only** loopback port the box may dial.
    ///
    /// The browser shim drives Chrome over CDP, which is a TCP connection to
    /// `127.0.0.1`. On macOS a box shares the host's loopback, so `network_rules`
    /// refuses outbound to it wholesale — dialing it would reach host services,
    /// h5i's own proxies among them. Granting the whole interface to make one
    /// connection work would trade the boundary for a feature, so h5i picks the
    /// port, tells Chrome to use it, and grants that one — the same shape as the
    /// proxy ports the supervised tier already allows.
    ///
    /// Never serialized: it is a per-box runtime fact, so a pinned policy digest
    /// is unchanged.
    #[serde(skip)]
    pub cdp_port: Option<u16>,
}

impl ResolvedPolicy {
    pub fn new(claim: IsolationClaim, profile: Profile) -> Self {
        ResolvedPolicy {
            claim,
            profile,
            audit: AuditPolicy::default(),
            box_git: Vec::new(),
            env_capture_spool: None,
            env_inbox: None,
            private_binds: Vec::new(),
            home_binds: Vec::new(),
            ro_binds: Vec::new(),
            cache_write: None,
            work_readonly: false,
            user_egress_allow: Vec::new(),
            cdp_port: None,
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
    pub egress: Option<EgressSummary>,
}

/// Outcome of one **interactive** session (`env shell`): the child's exit code
/// plus, on the container tier, the egress proxy's allow/deny tally — so an
/// interactive session leaves the same network evidence a captured run does.
/// Nothing else is recorded (stdio is inherited, the operator owns the session).
#[derive(Debug, Clone)]
pub struct InteractiveOutcome {
    pub exit_code: i32,
    /// Egress verdicts observed for the session's lifetime. Only the
    /// `isolation=container` tier populates this; `None` elsewhere.
    pub egress: Option<EgressSummary>,
}

impl InteractiveOutcome {
    /// Wrap a bare exit code (the kernel tiers, which run no egress proxy).
    pub fn from_code(exit_code: i32) -> Self {
        InteractiveOutcome {
            exit_code,
            egress: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_policy_round_trips_through_toml() {
        let mut profile = Profile::builtin("locked", IsolationClaim::Process);
        profile.tools = vec!["cargo".into(), "rustc".into()];
        profile.net_egress = vec!["crates.io".into()];
        profile.secret_grants = vec![SecretGrant {
            name: "GITHUB_TOKEN".into(),
            source: Some("env:GITHUB_TOKEN".into()),
            inject: Some("env".into()),
            ttl: Some("10m".into()),
        }];
        profile.private_paths = vec![PrivatePath {
            path: "target".into(),
            kind: "cache".into(),
            persist: true,
        }];

        let policy = ResolvedPolicy::new(IsolationClaim::Process, profile);
        let encoded = policy.to_toml().expect("policy serializes");
        let decoded = ResolvedPolicy::from_toml(&encoded).expect("policy deserializes");

        assert_eq!(decoded.claim, IsolationClaim::Process);
        assert_eq!(decoded.profile.name, "locked");
        assert_eq!(decoded.profile.isolation, IsolationClaim::Process);
        assert_eq!(decoded.profile.tools, ["cargo", "rustc"]);
        assert_eq!(decoded.profile.net_egress, ["crates.io"]);
        assert_eq!(decoded.profile.secret_grants[0].source_or_default(), "env:GITHUB_TOKEN");
        assert_eq!(decoded.profile.private_paths[0].path, "target");
        assert_eq!(decoded.audit.capture, AuditCapture::Signal);
        assert_eq!(decoded.to_toml().expect("re-serializes"), encoded);
        assert_eq!(decoded.digest().expect("digest"), policy.digest().expect("digest"));
    }

    #[test]
    fn process_profile_defaults_are_fail_closed() {
        let profile = Profile::builtin("default", IsolationClaim::Process);

        assert_eq!(profile.net_mode, NetMode::Deny);
        assert!(profile.net_egress.is_empty());
        assert!(profile.secrets.is_empty());
        assert!(profile.secret_grants.is_empty());
        assert!(profile.private_paths.is_empty());
        assert!(!profile.allow_command_extractors);
        assert_eq!(profile.wall_secs, DEFAULT_WALL.as_secs());
        assert_eq!(AuditCapture::default(), AuditCapture::Signal);
        assert_eq!(AuditPolicy::default().capture, AuditCapture::Signal);
    }

    #[test]
    fn serde_defaults_keep_missing_optional_policy_fields_conservative() {
        let text = r#"
claim = "process"

[profile]
name = "minimal"
isolation = "process"
fs_read = ["/usr"]
fs_write = ["$WORK"]
fs_deny = ["~/.ssh"]
net_mode = "deny"
net_egress = []
secrets = []
mem_bytes = 1073741824
max_procs = 32
wall_secs = 1800
tools = []
env_pass = ["PATH"]
"#;

        let policy = ResolvedPolicy::from_toml(text).expect("missing defaulted fields deserialize");

        assert_eq!(policy.audit.capture, AuditCapture::Signal);
        assert_eq!(policy.profile.net_mode, NetMode::Deny);
        assert!(policy.profile.secret_grants.is_empty());
        assert!(policy.profile.private_paths.is_empty());
        assert!(!policy.profile.allow_command_extractors);
        assert!(policy.profile.net_egress.is_empty());
    }

    #[test]
    fn enum_toml_spellings_are_strict() {
        #[derive(Deserialize)]
        struct EnumFixture {
            isolation: IsolationClaim,
            net_mode: NetMode,
            audit_capture: AuditCapture,
        }

        let parsed: EnumFixture = toml::from_str(
            r#"
isolation = "hardened-container"
net_mode = "deny"
audit_capture = "signal"
"#,
        )
        .expect("known enum spellings deserialize");

        assert_eq!(parsed.isolation, IsolationClaim::HardenedContainer);
        assert_eq!(parsed.net_mode, NetMode::Deny);
        assert_eq!(parsed.audit_capture, AuditCapture::Signal);

        assert!(toml::from_str::<EnumFixture>(
            r#"
isolation = "hardened_container"
net_mode = "deny"
audit_capture = "signal"
"#,
        )
        .is_err());
        assert!(toml::from_str::<EnumFixture>(
            r#"
isolation = "process"
net_mode = "allow"
audit_capture = "signal"
"#,
        )
        .is_err());
        assert!(toml::from_str::<EnumFixture>(
            r#"
isolation = "process"
net_mode = "deny"
audit_capture = "verbose"
"#,
        )
        .is_err());
    }
}

// ── Egress summary (moved from objects.rs to break the sandbox↔objects crate
// cycle: egress tallies are policy/enforcement data the capture store only
// renders — objects now imports these from here). ──
/// Counts + a bounded per-host breakdown of an env's network egress decisions —
/// the manifest holds only this summary (token-reduction principle, design §8).
/// Populated by the `isolation=container` allowlist proxy (the only tier that
/// observes egress today); `None` everywhere else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressSummary {
    /// Requests the proxy permitted (on-allowlist host:port).
    pub allowed: u64,
    /// Requests the proxy refused with `403` (off-allowlist — a network
    /// boundary trip). The single highest-fidelity egress signal.
    pub denied: u64,
    /// Per `host:port` verdict counts, deduped and bounded ([`MAX_EGRESS_HOSTS`])
    /// so a probing loop can never bloat the shared `refs/h5i/objects` ref. This
    /// is what the dashboard reads directly — no raw rehydration needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<EgressHost>,
    /// True when [`MAX_EGRESS_HOSTS`] was exceeded and the tail was dropped, so a
    /// reader never mistakes a clamped list for the whole picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hosts_truncated: bool,
    /// Object id (in this store) of the full `egress.jsonl`, when captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// One destination an env's traffic was steered at, with allow/deny tallies.
/// A host with `denied > 0` is a refused boundary attempt; the dashboard's NET
/// lane surfaces these as "Boundary blocked".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressHost {
    pub host: String,
    pub port: u16,
    pub allowed: u64,
    pub denied: u64,
}

/// Cap on distinct `host:port` entries kept in an [`EgressSummary`] — keeps the
/// shared manifest bounded regardless of how many hosts a run probes.
pub const MAX_EGRESS_HOSTS: usize = 64;
