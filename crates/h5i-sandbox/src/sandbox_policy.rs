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
    ///
    /// Each runtime needs its **auth** host as well as its API host, or the box
    /// is a one-session box: the credential copy `prepare_home_state` seeds it
    /// with expires, the silent refresh has nowhere to go, and the in-box login
    /// the credential-proxy design falls back to cannot complete either — the
    /// paste is reported as an invalid token. Claude exchanges and refreshes at
    /// `platform.claude.com/v1/oauth/token`; Codex at `auth.openai.com`.
    ///
    /// Granting the auth host is not a new capability class: the box already
    /// holds the OAuth token, and `api.anthropic.com` already serves
    /// `/api/oauth/claude_cli/create_api_key`. Where a host-side credential
    /// exists the broker is still the better answer — `auth_proxy::engage`
    /// scrubs the box copy and locks egress to the proxy alone, so none of these
    /// hosts are reachable at all.
    pub(crate) fn egress(self) -> &'static [&'static str] {
        match self {
            AgentRuntime::Claude => {
                &["api.anthropic.com", "platform.claude.com", "statsig.anthropic.com"]
            }
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
    /// Environment variable the box is given the **per-run dummy token** in —
    /// whatever variable its client already sends as a credential (`GH_TOKEN`
    /// for `gh`, and so on).
    ///
    /// The proxy gates every request on that token, so without this the box has
    /// nothing to present and each request is refused: the grant was inert.
    /// Validated non-empty at policy load rather than defaulted, because only
    /// the profile author knows which variable their client reads.
    #[serde(default)]
    pub token_var: String,
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

/// `[profile.X.detect]` — the runtime-detection lane (ROADMAP.md D11).
///
/// Four fields, all optional, and `enabled` is `false` by default. That
/// default is a decision, not caution: the collector needs `CAP_BPF`, which an
/// ordinary install does not have, so turning it on for everybody would
/// produce a fleet of `unavailable` blocks and teach every user to skip past
/// the one part of the receipt that is hardest to forge.
///
/// This crate holds the *policy*, not the mechanism: `h5i-bpf` sits beside
/// `h5i-sandbox`, not below it, and a policy type that depended on the
/// collector would make the confinement layer unbuildable without an eBPF
/// toolchain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectPolicy {
    /// Attach the probe for runs under this profile.
    pub enabled: bool,
    /// Refuse to run at all when the probe cannot attach.
    ///
    /// The fail-closed switch, and off by default for the same reason
    /// `enabled` is: a mandatory detector on a laptop kernel is a tool that
    /// does not start. It is the right setting for "I am running somebody
    /// else's dependency tree", and that is a decision an operator makes.
    pub require: bool,
    /// Ring-buffer size in KiB, clamped by the collector to its own bounds.
    pub buffer_kb: u32,
    /// Rule selectors: ids (`net.direct-egress`), families (`net`), or `*`.
    ///
    /// A selector that matches no rule is refused at run time rather than
    /// ignored — a profile that believes it enabled a rule and silently
    /// enabled nothing is the exact failure this lane exists to catch.
    pub rules: Vec<String>,
}

impl Default for DetectPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            require: false,
            // Mirrors `h5i_bpf::DEFAULT_BUFFER_KB`. Duplicated rather than
            // imported, because importing it would put the collector below the
            // sandbox in the dependency graph for the sake of one integer.
            buffer_kb: 256,
            rules: vec!["*".to_string()],
        }
    }
}

impl DetectPolicy {
    /// Is this the default? Drives `skip_serializing_if`, so that every
    /// profile written before this section existed serializes byte for byte as
    /// it did — and its pinned policy digest is unchanged.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

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

    /// Loopback ports the box may dial (`[profile.X.net] loopback = [3000]`).
    ///
    /// macOS shares the host's loopback with the box, so it is denied wholesale
    /// — dialing it would reach host services. A box that runs its own dev
    /// server and wants to point its own browser at it names the port here, and
    /// exactly that port is granted. Declared, so it is part of the digest and a
    /// reviewer sees it; on Linux the box has a private netns and this is
    /// redundant but harmless.
    ///
    /// Appended last, and serialized only when non-empty, so every existing
    /// profile's canonical serialization — and its pinned digest — is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loopback_ports: Vec<u16>,

    /// Which browser engine a `browser` box runs (`[profile.X] engine = "..."`).
    ///
    /// Declared rather than discovered, because **an engine change is a policy
    /// change, not an optimization**: an API that is absent by construction in
    /// one engine exists in another, so a box that silently fell back to
    /// Chromium would widen its own capability surface without anyone deciding
    /// to. It is pinned in the digest for the same reason `unix_sockets` is,
    /// and there is deliberately no fallback: an engine that cannot serve a
    /// page fails and names the recreate (see `engine_tooling_present`).
    ///
    /// `None` means the profile does not run a browser at all, which is every
    /// profile but `browser`.
    ///
    /// Appended last, and serialized only when set, so every existing profile's
    /// canonical serialization — and its pinned digest — is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<BrowserEngine>,

    /// Browser actions this box may not perform
    /// (`[profile.X.browser] deny = ["evaluate", "state"]`).
    ///
    /// Names are the *action* as it goes over the daemon socket, which is not
    /// always the CLI verb: `agent-browser eval` sends `evaluate`. Entries are
    /// validated against [`BROWSER_DENYABLE_ACTIONS`] at create.
    ///
    /// Enforced by the mediator that sits on the daemon's control socket, so
    /// this is a real refusal rather than advice: the verb never reaches the
    /// browser. A bare family name denies its members, so `state` covers
    /// `state_save` and `state_load`. `evaluate` is the one most profiles want,
    /// because it is arbitrary code in the page.
    ///
    /// Appended last, and serialized only when non-empty, so every existing
    /// profile's canonical serialization — and its pinned digest — is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_deny: Vec<String>,

    /// The runtime-detection lane (`[profile.X.detect]`, ROADMAP.md D11).
    ///
    /// Appended last and skipped when default, so every existing profile's
    /// canonical serialization — and its pinned digest — is unchanged. It is
    /// *in* the digest when it is set, deliberately: whether a box was watched
    /// is part of what its policy claimed, and a reviewer comparing two
    /// receipts must not find that the difference between them was invisible.
    ///
    /// Being a table, it must stay the final field: TOML puts tables after
    /// scalars, and a scalar declared below this one would serialize into the
    /// `[profile.detect]` table instead of the profile.
    #[serde(default, skip_serializing_if = "DetectPolicy::is_default")]
    pub detect: DetectPolicy,
}

/// The browser engines a `browser` box can be pinned to.
///
/// Two of these are Chromium-shaped and one is not, and the difference is the
/// whole point of naming it in policy: `Chromium` runs the full engine through
/// agent-browser and can do everything a browser does; `H5iLight` runs our own
/// engine, which has no script, no video and no WebGL, and is therefore the
/// safer place to read the untrusted web and the wrong place to verify a React
/// app (roadmap-history.md 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserEngine {
    /// Chromium driven by agent-browser. The fidelity path; today's default.
    Chromium,
    /// Lightpanda, driven by agent-browser over CDP. Lighter than Chromium,
    /// still runs script.
    Lightpanda,
    /// h5i's own engine, part of the h5i binary. Script is opt-in, and every
    /// request is policy-checked and receipted before the wire.
    ///
    /// Spelled `h5i`. It was `h5i-light` while the engine was a second binary
    /// called `h5i-browser-light`; that binary is gone, and a value named after
    /// it was naming something that no longer exists.
    ///
    /// `alias` rather than a silent acceptance: a `policy.resolved.toml` written
    /// by an older h5i still *parses*, so the box fails on the digest check —
    /// which already says "created by a different h5i version; recreate it" —
    /// rather than on a TOML error that names a field and explains nothing.
    #[serde(rename = "h5i", alias = "h5i-light")]
    H5iLight,
}

/// Browser actions (and action *families*) a profile may name in
/// `[profile.X.browser] deny`.
///
/// A curated vocabulary rather than free text, because the deny list is
/// fail-open by nature: an entry that matches nothing denies nothing, while
/// `policy.resolved.toml` still reads as though the verb were blocked. A
/// misspelling has to be refused at create, where the author is standing
/// there, not discovered later by an agent successfully doing the thing.
///
/// Family entries (`state`, `credentials`, …) cover their `name_*` members —
/// `state` denies `state_save` and `state_load` — which is why they are listed
/// alongside the individual verbs rather than instead of them.
///
/// Adding an action upstream means adding it here. That is the cost of the
/// guarantee, and it is the same trade the Chrome candidate tables make.
pub const BROWSER_DENYABLE_ACTIONS: &[&str] = &[
    // Families.
    // The stored-login family. Distinct from `credentials` on the wire and
    // reaching the same secrets, so it is nameable in its own right — and
    // `credentials` also covers it, see `browser_proxy::DENY_ALSO_COVERS`.
    "auth",
    "cookies",
    "credentials",
    "profiler",
    "react",
    "recording",
    "state",
    "storage",
    "tab",
    "trace",
    // Individual verbs.
    //
    // The script-execution verbs are each nameable, and `evaluate` also covers
    // every one of them: a profile that denied `evaluate` to get a reading
    // browser was getting one that still ran whatever the agent wrote, because
    // `evalhandle` is `Runtime.evaluate` under another name. See
    // `browser_proxy::DENY_ALSO_COVERS`.
    "addinitscript",
    "addscript",
    "addstyle",
    "back",
    "cdp_url",
    "boundingbox",
    "check",
    "click",
    "close",
    "console",
    "content",
    "count",
    "dblclick",
    "diff_snapshot",
    "diff_url",
    "download",
    "drag",
    "evalhandle",
    "evaluate",
    "expose",
    "fill",
    "focus",
    "forward",
    "geolocation",
    "getattribute",
    "gettext",
    "headers",
    "hover",
    "innerhtml",
    "inputvalue",
    "ischecked",
    "isenabled",
    "isvisible",
    "keyboard",
    "launch",
    "locale",
    "navigate",
    "network",
    "offline",
    "open",
    "pdf",
    "permissions",
    "press",
    "read",
    "reload",
    "route",
    "screenshot",
    "scroll",
    "select",
    "setcontent",
    "snapshot",
    "styles",
    "tap",
    "timezone",
    "title",
    "type",
    "uncheck",
    "upload",
    "unroute",
    "url",
    "useragent",
    "viewport",
    "wait",
    "waitforfunction",
];

/// Check one `[profile.X.browser] deny` entry, fail-closed.
///
/// The error names a near match when there is one, because the mistake this
/// exists to catch is a plausible short spelling (`eval` for `evaluate`) that
/// would otherwise silently deny nothing.
pub fn validate_browser_deny(entry: &str) -> Result<(), String> {
    let name = entry.trim();
    if name.is_empty() {
        return Err("`[profile.X.browser] deny` contains an empty entry (fail-closed)".to_string());
    }
    // `snapshot` is the one verb that clears the stale-handle latch a human
    // takeover sets, so denying it means every mutating verb is refused with
    // "run `agent-browser snapshot`" and that snapshot is refused too — the
    // browser is unusable for the life of the box, with an error that reads as
    // a malfunction.
    if name == "snapshot" {
        return Err(
            "`snapshot` cannot be denied: it is how an agent recovers after a human takes and \
             hands back browser control, so denying it would leave the browser permanently \
             refusing every action (fail-closed)."
                .to_string(),
        );
    }
    if BROWSER_DENYABLE_ACTIONS.contains(&name) {
        return Ok(());
    }

    // A near match is almost always the intent: `eval` -> `evaluate`,
    // `cookie` -> `cookies`.
    let lowered = name.to_ascii_lowercase();
    // The *closest* near match, not the first in a list that happens to be
    // alphabetical. `eval` prefixes both `evaluate` and `evalhandle`, and the
    // one a person who typed `eval` meant is the shorter — the list order gave
    // them `evalhandle`, which is a real verb and the wrong advice.
    let hint = BROWSER_DENYABLE_ACTIONS
        .iter()
        .filter(|known| {
            **known == lowered || known.starts_with(&lowered) || lowered.starts_with(*known)
        })
        .min_by_key(|known| (known.len(), **known))
        .map(|known| format!(" — did you mean `{known}`?"))
        .unwrap_or_default();

    Err(format!(
        "unknown browser action `{entry}` in `[profile.X.browser] deny`{hint}\n           An entry that matches no action denies nothing while the policy reads as if it did, \n           so it is refused here (fail-closed). Accepted names are the agent-browser verbs and \n           the families `auth`, `cookies`, `credentials`, `state`, `storage`, `tab`, `trace`, \n           `profiler`, `react`, `recording` (a family covers its `name_*` members)."
    ))
}

/// Every engine a profile may pin, in preference order (fidelity first).
///
/// A list rather than a derived iterator so that adding a variant to
/// [`BrowserEngine`] and forgetting it here is a one-line diff to spot in
/// review: the callers that walk it are the ones that pick a *fallback* engine
/// for a host, and an engine missing from the walk is an engine the tool
/// silently claims the host cannot run.
pub const BROWSER_ENGINES: [BrowserEngine; 3] = [
    BrowserEngine::Chromium,
    BrowserEngine::Lightpanda,
    BrowserEngine::H5iLight,
];

impl BrowserEngine {
    /// The spelling used in `.h5i/env.toml` and on `--engine`.
    pub fn as_str(&self) -> &'static str {
        match self {
            BrowserEngine::Chromium => "chromium",
            BrowserEngine::Lightpanda => "lightpanda",
            BrowserEngine::H5iLight => "h5i",
        }
    }

    /// Parse a policy-declared engine name, fail-closed on anything else.
    ///
    /// The error names the accepted values rather than saying "invalid",
    /// because the caller is someone editing a profile who needs the list.
    pub fn parse(input: &str) -> Result<Self, String> {
        match input.trim() {
            "chromium" | "chrome" => Ok(BrowserEngine::Chromium),
            "lightpanda" => Ok(BrowserEngine::Lightpanda),
            // The old spellings stay accepted as *input*. A profile or a habit
            // that still says `h5i-light` costs nothing to honour, and what it
            // resolves to is written out under the current name.
            "h5i" | "h5i-light" | "h5i_light" => Ok(BrowserEngine::H5iLight),
            other => Err(format!(
                "unknown browser engine `{other}` — expected one of: chromium, lightpanda, h5i (fail-closed)"
            )),
        }
    }

    /// Whether agent-browser drives this engine.
    ///
    /// The h5i engine does not speak CDP, so agent-browser cannot drive it; h5i
    /// runs it itself. Getting this backwards would mean injecting
    /// `AGENT_BROWSER_*` variables for an engine that never reads them, which
    /// reviews as enforcement while enforcing nothing.
    pub fn driven_by_agent_browser(&self) -> bool {
        match self {
            BrowserEngine::Chromium | BrowserEngine::Lightpanda => true,
            BrowserEngine::H5iLight => false,
        }
    }

    /// The binaries this engine needs on a kernel-tier host, and the command
    /// that installs them, for the create-time refusal.
    pub fn required_tooling(&self) -> (&'static [&'static str], &'static str) {
        match self {
            BrowserEngine::Chromium => (
                &["a Chrome/Chromium build", "the `agent-browser` binary"],
                "npm install -g agent-browser && agent-browser install",
            ),
            BrowserEngine::Lightpanda => (
                &["the `lightpanda` binary", "the `agent-browser` binary"],
                "npm install -g agent-browser  # and install lightpanda from https://lightpanda.io",
            ),
            BrowserEngine::H5iLight => (
                &["the `h5i-browser-light` binary"],
                "cargo install --path crates/h5i-browser-light",
            ),
        }
    }
}

/// Read-only system paths granted by default at the `process` tier — enough to
/// exec interpreters and link against system libraries, nothing under `$HOME`.
/// Where `/etc/resolv.conf` actually lives, on the hosts where it is a symlink.
///
/// `/etc` is granted, so on a host whose resolver config is a real file inside
/// it there is nothing to do. It is a symlink on more machines than one would
/// like — WSL points it at `/mnt/wsl/resolv.conf`, systemd-resolved and
/// resolvconf at somewhere under `/run` — and a Landlock grant follows the link
/// to a path the domain never granted. What that costs is worth spelling out,
/// because it does not read as a sandbox denial: `getaddrinfo` answers
/// "Temporary failure in name resolution", every fetch fails before the wire,
/// and a `net.mode = "host"` box looks like a host with no network. It took a
/// boxed browser session and twenty minutes to find, on the box that has been
/// this project's own development machine since the beginning.
///
/// **A static list rather than `canonicalize()`**, and that is the whole
/// design. Resolving the link at runtime would grant the right path on every
/// host and give the *same profile a different digest on each of them* — the
/// one promise a policy digest makes is that two boxes with the same digest
/// were allowed the same things, and a digest that varies with `/etc` cannot
/// make it. These entries are the same everywhere, present or not: a grant for
/// a path that does not exist is skipped by the Landlock builder, so a host
/// with none of them enforces exactly what it enforced before.
///
/// Read-only, one file each, and none of them is a way out of anything: a
/// `deny` box has no network to resolve for, and a `host` box already reaches
/// everything. `/run` itself stays ungranted.
const RESOLVER_PATHS: &[&str] = &[
    // WSL2.
    "/mnt/wsl/resolv.conf",
    // systemd-resolved, stub and non-stub.
    "/run/systemd/resolve/stub-resolv.conf",
    "/run/systemd/resolve/resolv.conf",
    // Debian's resolvconf, and NetworkManager writing its own.
    "/run/resolvconf/resolv.conf",
    "/run/NetworkManager/resolv.conf",
];

fn default_fs_read() -> Vec<String> {
    ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/nix", "/opt", "/tmp", "/dev/null", "/dev/zero", "/dev/urandom", "/proc"]
        .iter()
        .chain(RESOLVER_PATHS.iter())
        .map(|s| s.to_string())
        .collect()
}

fn default_fs_deny() -> Vec<String> {
    // `~/.config/h5i` holds the runner directory: a per-runner private key and
    // the pinned host key that is the whole basis of `runner_id`. That is the
    // `~/.ssh` threat model exactly, and it belongs on the same list. It held
    // by omission before — nothing in the defaults grants anything under
    // `$HOME` — but the deny list is what survives a user-authored profile
    // granting `~/.config`, which is precisely when it matters.
    ["~/.ssh", "~/.aws", "~/.config/gh", "~/.config/h5i", "$REPO/.git/hooks"]
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
            loopback_ports: Vec::new(),
            engine: None,
            browser_deny: Vec::new(),
            detect: DetectPolicy::default(),
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
        // Pinned, and never inferred. A box that quietly changed engine would
        // change what the page can do without changing its digest.
        p.engine = Some(BrowserEngine::Chromium);
        p
    }

    /// Does this profile actually **name** an egress allowlist?
    ///
    /// Not `!net_egress.is_empty()`. A blank entry is a `Vec` element and not a
    /// rule: `parse_egress_rule` skips it, so `net.egress = [""]` builds an
    /// allowlist of zero entries — a deny-all — while the length test reports
    /// that the profile "sets net.egress".
    ///
    /// The distinction decides whether the host-side `h5i box allow` list is
    /// merged in, and SECURITY.md states the property it rests on: that list
    /// "merges into a profile that already sets `net.egress` and never widens a
    /// deny-all one". Under the length test a repo could write
    /// `net.egress = [""]` and have the operator's own global allow rules
    /// applied to a box that was meant to reach nothing.
    ///
    /// Deliberately *not* used where the length test decides whether to build
    /// the enforcement itself (the supervisor's netns, the container proxy).
    /// There a blank list means "an allowlist with no entries", which is
    /// deny-all and correct; answering "no allowlist" would hand the box the
    /// host network instead.
    pub fn scopes_egress(&self) -> bool {
        self.net_egress.iter().any(|e| !e.trim().is_empty())
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

/// The **runnable** browser under each [`CHROME_CANDIDATES`] grant, in the same
/// preference order — one `*` per segment, matched against the real filesystem
/// by [`chrome_binary`].
///
/// This is the list, and the grant list above is derived from where its entries
/// live. They used to be independent: the grants named `~/.cache/ms-playwright`
/// while the launcher only ever looked in `/usr/bin`, so a host whose only
/// Chrome was a Playwright build passed `create` (the grant's directory exists),
/// got a read grant for it, and then failed *inside the box* with "no
/// Chrome/Chromium found". Two tables that have to agree, kept apart, disagreed.
/// [`browser_discovery_tests`] now fails the build in both directions.
const CHROME_EXECUTABLES: &[&str] = &[
    // `agent-browser install`, current layout. The Linux build is a plain
    // binary; on macOS the same download is an app bundle.
    "~/.agent-browser/browsers/chrome-*/chrome-linux64/chrome",
    "~/.agent-browser/browsers/chrome-*/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "~/.cache/agent-browser/chrome-*/chrome-linux64/chrome",
    "~/.cache/agent-browser/chrome-*/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "~/.local/share/agent-browser/chrome-*/chrome-linux64/chrome",
    "~/.local/share/agent-browser/chrome-*/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    // Playwright. Deliberately not `chromium_headless_shell-*`: the shim
    // launches with `--headless=new`, which the headless shell does not accept,
    // so finding one would trade a clear "not found" for an opaque exit 1.
    "~/.cache/ms-playwright/chromium-*/chrome-linux/chrome",
    "~/.cache/ms-playwright/chromium-*/chrome-mac/Chromium.app/Contents/MacOS/Chromium",
    "/opt/google/chrome/chrome",
    // Debian's `/usr/bin/chromium` is a wrapper around the second path; both
    // are runnable and either is fine.
    "/usr/lib/chromium/chromium",
    "/usr/lib/chromium-browser/chromium-browser",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "~/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "~/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "~/Applications/Chromium.app/Contents/MacOS/Chromium",
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

/// Where `lightpanda` lands. Same shape as the agent-browser list, and for the
/// same reason: a grant nobody looks in reads like support and supports
/// nothing.
const LIGHTPANDA_CANDIDATES: &[&str] = &[
    "~/.cargo/bin/lightpanda",
    "~/.local/bin/lightpanda",
    "/usr/local/bin/lightpanda",
    "/usr/bin/lightpanda",
    "/opt/homebrew/bin/lightpanda",
];

/// Where our own engine lands.
const BROWSER_LIGHT_CANDIDATES: &[&str] = &[
    "~/.cargo/bin/h5i-browser-light",
    "~/.local/bin/h5i-browser-light",
    "/usr/local/bin/h5i-browser-light",
    "/usr/bin/h5i-browser-light",
    "/opt/homebrew/bin/h5i-browser-light",
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
        // Every engine's binaries are granted, not just the pinned one: the
        // grant list is host discovery, and narrowing it per engine would make
        // the digest depend on which engine a box happened to pick while
        // buying nothing — the engine is enforced by what h5i launches, and
        // only paths that exist are granted anyway.
        .chain(LIGHTPANDA_CANDIDATES)
        .chain(BROWSER_LIGHT_CANDIDATES)
        .chain(BROWSER_SUPPORT_PATHS)
        .filter(|c| expand_home(c).map(|p| p.exists()).unwrap_or(false))
        .map(|c| c.to_string())
        .collect()
}

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

/// The browser patterns the in-box launcher should try, in preference order.
///
/// The shim is a shell script, so it cannot call [`chrome_binary`]: the box may
/// see a different filesystem than the host that generated it (and at the
/// container tier it certainly does). It gets the *patterns* and does its own
/// `-x` test in there. Same list either way, which is the point.
pub fn chrome_exec_patterns() -> &'static [&'static str] {
    CHROME_EXECUTABLES
}

/// The first [`CHROME_EXECUTABLES`] entry that resolves to an executable file
/// **on this host**, or `None`.
///
/// Only meaningful for the kernel tiers, where the box reaches the host
/// filesystem. A container box gets its browser from the image, so `create`
/// does not consult this there.
pub fn chrome_binary() -> Option<String> {
    chrome_binary_within(None, std::env::var("HOME").ok().as_deref())
}

/// [`chrome_binary`] with the filesystem it searches supplied.
///
/// `root`, when set, is prepended to **every** pattern — the absolute ones as
/// well as the ones under `home`. A test that only redirected `home` would
/// still be searching the real `/usr/bin`, and CI runners ship a Chrome there:
/// the "this host has no browser" cases passed on a laptop and failed on the
/// runner, which is the same class of mistake this whole change is about.
fn chrome_binary_within(root: Option<&std::path::Path>, home: Option<&str>) -> Option<String> {
    CHROME_EXECUTABLES
        .iter()
        .filter_map(|p| expand_home_in(p, home))
        .map(|p| match root {
            Some(r) => r.join(p.strip_prefix("/").unwrap_or(&p)),
            None => p,
        })
        .flat_map(|p| glob_paths(&p))
        .find(|p| is_executable_file(p))
        .map(|p| p.display().to_string())
}

/// Expand a path with at most one `*` per segment against the real filesystem.
///
/// Not a general glob: no `**`, no character classes, no brace expansion. That
/// is deliberate — the only wildcard any browser layout needs is a version
/// directory (`chromium-1140`, `chrome-141.0.7390.54`), and a real glob crate
/// would be a dependency in the sandbox crate for one `*`.
///
/// Matches within a directory are returned in **reverse** name order, so
/// `chromium-1140` is tried before `chromium-1039`. That is a string sort and
/// not a version comparison: with two builds installed either one works, and
/// which is "newest" is not worth a version parser.
fn glob_paths(pattern: &std::path::Path) -> Vec<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    let mut out: Vec<PathBuf> = vec![PathBuf::new()];
    for comp in pattern.components() {
        let Component::Normal(seg) = comp else {
            for p in out.iter_mut() {
                p.push(comp.as_os_str());
            }
            continue;
        };
        let seg = seg.to_string_lossy().into_owned();
        let Some((pre, post)) = seg.split_once('*') else {
            for p in out.iter_mut() {
                p.push(&seg);
            }
            continue;
        };
        let mut next = Vec::new();
        for parent in &out {
            // An unreadable or absent directory contributes nothing, exactly as
            // a non-matching literal segment would.
            let Ok(entries) = std::fs::read_dir(parent) else { continue };
            let mut hits: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.len() >= pre.len() + post.len()
                        && name.starts_with(pre)
                        && name.ends_with(post)
                })
                .map(|e| e.path())
                .collect();
            hits.sort();
            hits.reverse();
            next.extend(hits);
        }
        out = next;
        if out.is_empty() {
            return Vec::new();
        }
    }
    out
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else { return false };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    // No exec bit to consult; being a file is as much as we can ask.
    #[cfg(not(unix))]
    {
        true
    }
}

/// `(a browser we can actually launch, the `agent-browser` binary)`.
///
/// The first half asks for a **runnable executable**, not for a directory that
/// exists. `~/.cache/ms-playwright` is there on any host that ever installed
/// Playwright for something else, and it may hold nothing but an ffmpeg build —
/// which used to be enough to pass `create` and fail in the box.
/// Which of an engine's required binaries are missing on this host.
///
/// Returns the human-readable names, so the caller's refusal can say what to
/// install rather than "unsupported". Empty means the engine can run here.
pub fn engine_tooling_missing(engine: BrowserEngine) -> Vec<&'static str> {
    let any = |list: &[&str]| {
        list.iter()
            .any(|c| expand_home(c).map(|p| p.exists()).unwrap_or(false))
    };
    let agent_browser = any(AGENT_BROWSER_CANDIDATES);

    let mut missing = Vec::new();
    match engine {
        BrowserEngine::Chromium => {
            if chrome_binary().is_none() {
                missing.push("a Chrome/Chromium build");
            }
            if !agent_browser {
                missing.push("the `agent-browser` binary");
            }
        }
        BrowserEngine::Lightpanda => {
            if !any(LIGHTPANDA_CANDIDATES) {
                missing.push("the `lightpanda` binary");
            }
            if !agent_browser {
                missing.push("the `agent-browser` binary");
            }
            // Chrome too, and not as an oversight: h5i's launch shim starts
            // Chrome and attaches agent-browser to it over `--cdp`, and that
            // shim is installed for every engine agent-browser drives. Until
            // the shim can launch lightpanda itself, a lightpanda box still
            // needs a Chrome on the host — so demand it here rather than let
            // create pass and every browser command afterwards fail.
            if chrome_binary().is_none() {
                missing.push("a Chrome/Chromium build (h5i's launch shim still starts Chrome)");
            }
        }
        // Ours drives itself: agent-browser cannot speak to it, so requiring
        // agent-browser here would refuse a box that would have worked.
        BrowserEngine::H5iLight => {
            if !any(BROWSER_LIGHT_CANDIDATES) {
                missing.push("the `h5i-browser-light` binary");
            }
        }
    }
    missing
}

/// The `h5i-browser-light` binary on this host, if there is one.
pub fn browser_light_binary() -> Option<String> {
    BROWSER_LIGHT_CANDIDATES
        .iter()
        .filter_map(|c| expand_home(c))
        .find(|p| p.exists())
        .map(|p| p.display().to_string())
}

pub fn browser_tooling_present() -> (bool, bool) {
    let any = |list: &[&str]| {
        list.iter()
            .any(|c| expand_home(c).map(|p| p.exists()).unwrap_or(false))
    };
    (chrome_binary().is_some(), any(AGENT_BROWSER_CANDIDATES))
}

/// The invariant that the two browser tables have to hold, in both directions.
///
/// Every failure this fix came from was one of these two assertions being false
/// with nothing to notice it: a grant nobody looks in reads like support and
/// supports nothing, and a launcher path nobody granted is a box that fails
/// after `create` said it was fine.
#[cfg(test)]
mod browser_discovery_tests {
    use super::*;

    #[test]
    fn a_misspelled_deny_entry_is_refused_and_the_error_suggests_the_real_name() {
        // The failure this guards: `deny = ["eval"]` looks right, matches no
        // action, and leaves arbitrary JS allowed while the resolved policy
        // reads as though it were blocked.
        let err = validate_browser_deny("eval").expect_err("must refuse");
        assert!(err.contains("eval"), "{err}");
        assert!(err.contains("did you mean `evaluate`"), "{err}");
        assert!(err.contains("fail-closed"), "{err}");

        assert!(validate_browser_deny("Evaluate").is_err(), "case matters");
        assert!(validate_browser_deny("evaluate-js").is_err());
        assert!(validate_browser_deny("  ").is_err(), "empty entries too");
    }

    #[test]
    fn real_actions_and_families_are_accepted() {
        for good in ["evaluate", "click", "state", "credentials", "cookies", "screenshot"] {
            validate_browser_deny(good)
                .unwrap_or_else(|e| panic!("`{good}` should be a valid deny entry: {e}"));
        }
        // Surrounding whitespace validates — and `load_profile` trims on the
        // way in, so what enforcement matches is what was validated. This test
        // previously stopped here, which pinned the fail-open: validation
        // trimmed, storage did not, and the padded entry denied nothing.
        assert!(validate_browser_deny(" evaluate ").is_ok());
    }

    #[test]
    fn snapshot_cannot_be_denied_because_it_is_how_the_lock_releases() {
        let err = validate_browser_deny("snapshot").expect_err("must refuse");
        assert!(err.contains("snapshot"), "{err}");
        assert!(err.contains("fail-closed"), "{err}");
    }

    #[test]
    fn every_read_only_action_the_mediator_knows_is_deniable() {
        // Drift guard. The mediator enumerates actions it treats as read-only;
        // if one of them is missing here, `validate_profile` hard-refuses a
        // profile that names it — a regression for a config that loaded
        // before — and, for `cdp_url`, removes the only way to close a
        // mediation bypass (it hands out the raw CDP endpoint).
        for action in [
            "cdp_url",
            "console",
            "network",
            "diff_snapshot",
            "diff_url",
            "read",
            "screenshot",
            "url",
            "title",
        ] {
            validate_browser_deny(action)
                .unwrap_or_else(|e| panic!("`{action}` is a real action and must be deniable: {e}"));
        }
    }

    #[test]
    fn the_engine_is_in_the_digest_and_only_when_a_profile_runs_a_browser() {
        // Same discipline as the AF_UNIX grant: appended last and skipped when
        // absent, so every existing profile's canonical TOML — and its pinned
        // digest — is byte-identical to before this field existed.
        let plain = Profile::builtin("default", IsolationClaim::Process);
        let rendered = toml::to_string(&plain).expect("serializes");
        assert!(
            !rendered.contains("engine"),
            "a profile that runs no browser must not mention an engine: {rendered}"
        );

        let browser = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);
        let rendered = toml::to_string(&browser).expect("serializes");
        assert!(
            rendered.contains("engine = \"chromium\""),
            "the browser profile must pin its engine: {rendered}"
        );
    }

    #[test]
    fn changing_engine_changes_the_digest() {
        // The whole point of declaring it: an engine switch is a policy change,
        // so two boxes that differ only by engine must not share a digest.
        let mut a = Profile::builtin_browser(IsolationClaim::Supervised, AgentRuntime::Claude);
        let mut b = a.clone();
        a.engine = Some(BrowserEngine::Chromium);
        b.engine = Some(BrowserEngine::H5iLight);
        assert_ne!(
            toml::to_string(&a).expect("a"),
            toml::to_string(&b).expect("b"),
            "engine must be part of what gets hashed"
        );
    }

    #[test]
    fn an_unknown_engine_is_refused_by_name_with_the_accepted_set() {
        let err = BrowserEngine::parse("firefox").expect_err("must refuse");
        assert!(err.contains("firefox"), "{err}");
        assert!(err.contains("chromium"), "the message must list what works: {err}");
        assert!(err.contains("fail-closed"), "{err}");

        // Spellings a person actually types.
        assert_eq!(BrowserEngine::parse("chrome").unwrap(), BrowserEngine::Chromium);
        assert_eq!(BrowserEngine::parse(" h5i ").unwrap(), BrowserEngine::H5iLight);
        // The name it had while the engine was a second binary, still accepted
        // as input so a profile written against an older h5i keeps resolving.
        assert_eq!(BrowserEngine::parse("h5i-light").unwrap(), BrowserEngine::H5iLight);
        assert_eq!(BrowserEngine::parse("h5i_light").unwrap(), BrowserEngine::H5iLight);
    }

    #[test]
    fn only_chromium_shaped_engines_are_driven_by_agent_browser() {
        // Getting this wrong means injecting AGENT_BROWSER_* variables for an
        // engine that never reads them, which reviews as enforcement while
        // enforcing nothing.
        assert!(BrowserEngine::Chromium.driven_by_agent_browser());
        assert!(BrowserEngine::Lightpanda.driven_by_agent_browser());
        assert!(!BrowserEngine::H5iLight.driven_by_agent_browser());
    }

    #[test]
    fn lightpanda_still_demands_chrome_because_the_shim_launches_it() {
        // The create-time guard used to require Chrome for every browser box.
        // Making it per-engine dropped that for lightpanda while
        // `prepare_browser_shim` kept installing the Chrome-launching shim —
        // so create passed and every browser command then failed, or worse,
        // silently ran full Chromium under a digest that said "lightpanda".
        let (_, install) = BrowserEngine::Lightpanda.required_tooling();
        assert!(!install.is_empty());
        // The check must ask about Chrome at all; whether it is present here
        // depends on the host, so assert on the question, not the answer.
        let missing = engine_tooling_missing(BrowserEngine::Lightpanda);
        if chrome_binary().is_none() {
            assert!(
                missing.iter().any(|m| m.contains("Chrome")),
                "lightpanda must not pass create on a host with no Chrome: {missing:?}"
            );
        }
    }

    #[test]
    fn our_own_engine_does_not_require_agent_browser_to_be_installed() {
        // It drives itself, so demanding agent-browser would refuse a box that
        // would have worked.
        let missing = engine_tooling_missing(BrowserEngine::H5iLight);
        assert!(
            !missing.iter().any(|m| m.contains("agent-browser")),
            "the h5i engine must not demand agent-browser: {missing:?}"
        );
    }

    /// Path-segment-aware prefix: `/usr/bin/chromium` covers
    /// `/usr/bin/chromium/…` and itself, but not `/usr/bin/chromium-browser`.
    fn covers(grant: &str, exe: &str) -> bool {
        exe == grant || exe.starts_with(&format!("{grant}/"))
    }

    #[test]
    fn every_launcher_path_is_a_path_the_box_may_read() {
        for exe in CHROME_EXECUTABLES {
            assert!(
                CHROME_CANDIDATES.iter().any(|g| covers(g, exe)),
                "the launcher would try {exe}, which no CHROME_CANDIDATES grant covers — \
                 Landlock would deny it in the box"
            );
        }
    }

    #[test]
    fn every_grant_is_a_path_the_launcher_actually_tries() {
        for grant in CHROME_CANDIDATES {
            assert!(
                CHROME_EXECUTABLES.iter().any(|e| covers(grant, e)),
                "{grant} is granted to browser boxes but the launcher never looks there — \
                 a host whose only Chrome is under it passes create and fails in the box"
            );
        }
    }

    /// The shim quotes a segment that contains a space and leaves `*` bare, so a
    /// segment needing both would have its glob quoted into a literal.
    #[test]
    fn no_pattern_segment_needs_quoting_and_globbing_at_once() {
        for exe in CHROME_EXECUTABLES {
            for seg in exe.split('/') {
                assert!(
                    !(seg.contains(' ') && seg.contains('*')),
                    "{exe}: segment {seg:?} cannot be both quoted and globbed in the shim"
                );
            }
        }
    }

    /// The home every fixture below is built under. A virtual path, not a real
    /// one: [`chrome_binary_within`] relocates it under the test root along
    /// with `/usr/bin` and everything else, so no test here can see the machine
    /// it is running on.
    const FAKE_HOME: &str = "/home/test";

    /// Place an `agent-browser`-shaped file inside a synthetic filesystem.
    /// `path` is absolute in that filesystem's terms (`/home/test/…`,
    /// `/usr/bin/…`).
    fn plant(root: &std::path::Path, path: &str, mode: u32) -> std::path::PathBuf {
        let full = root.join(path.strip_prefix('/').unwrap_or(path));
        std::fs::create_dir_all(full.parent().unwrap()).expect("mkdir");
        std::fs::write(&full, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }
        let _ = mode;
        full
    }

    fn found_in(root: &tempfile::TempDir) -> Option<String> {
        chrome_binary_within(Some(root.path()), Some(FAKE_HOME))
    }

    #[test]
    fn the_playwright_build_this_regressed_on_is_found() {
        let root = tempfile::tempdir().expect("tempdir");
        let chrome = plant(
            root.path(),
            "/home/test/.cache/ms-playwright/chromium-1140/chrome-linux/chrome",
            0o755,
        );

        assert_eq!(found_in(&root).as_deref(), Some(chrome.display().to_string().as_str()));
    }

    #[test]
    fn a_directory_that_holds_no_browser_is_not_a_browser() {
        let root = tempfile::tempdir().expect("tempdir");
        // The shape that used to pass `create`: the Playwright cache exists,
        // with an ffmpeg build in it and no Chrome anywhere.
        plant(
            root.path(),
            "/home/test/.cache/ms-playwright/ffmpeg-1011/ffmpeg",
            0o755,
        );

        assert_eq!(found_in(&root), None);
    }

    #[test]
    fn a_non_executable_chrome_does_not_count() {
        let root = tempfile::tempdir().expect("tempdir");
        plant(
            root.path(),
            "/home/test/.cache/ms-playwright/chromium-1140/chrome-linux/chrome",
            0o644,
        );

        assert_eq!(found_in(&root), None);
    }

    /// The search is over a filesystem the caller names, and `/usr/bin` is part
    /// of it. Without this the two `None` cases above are only testing that the
    /// *runner* has no Chrome, which on GitHub's runners is false.
    #[test]
    fn a_system_chrome_is_found_and_only_under_the_root_it_is_given() {
        let root = tempfile::tempdir().expect("tempdir");
        let chrome = plant(root.path(), "/usr/bin/google-chrome", 0o755);

        assert_eq!(found_in(&root).as_deref(), Some(chrome.display().to_string().as_str()));
        // The same fixture, searched from an empty root: nothing.
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(found_in(&empty), None);
    }

    /// A downloaded build beats the distro one, because it is the build
    /// `agent-browser install` put there for this.
    #[test]
    fn a_downloaded_build_wins_over_a_system_one() {
        let root = tempfile::tempdir().expect("tempdir");
        plant(root.path(), "/usr/bin/chromium", 0o755);
        let downloaded = plant(
            root.path(),
            "/home/test/.agent-browser/browsers/chrome-141.0.7390.54/chrome-linux64/chrome",
            0o755,
        );

        assert_eq!(
            found_in(&root).as_deref(),
            Some(downloaded.display().to_string().as_str())
        );
    }

    #[test]
    fn the_newest_looking_versioned_build_is_tried_first() {
        let root = tempfile::tempdir().expect("tempdir");
        for v in ["chromium-1039", "chromium-1140"] {
            plant(
                root.path(),
                &format!("/home/test/.cache/ms-playwright/{v}/chrome-linux/chrome"),
                0o755,
            );
        }

        let found = found_in(&root).expect("a build");
        assert!(found.contains("chromium-1140"), "picked {found}");
    }
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
    /// Runtime-only: does this box declare background services?
    ///
    /// Only the microvm tier reads it, and only to decide whether its guest may
    /// be stopped for idleness. A guest is stopped by `msb --idle-timeout` when
    /// no *command* has run for a while, and `msb` cannot see that a dev server
    /// inside it is busy serving — so an idle bound on a box that runs services
    /// silently kills them. Measured: a guest with a 20s bound stopped at ~25s
    /// and took its service with it.
    ///
    /// Declared rather than observed, because the bound is fixed when the guest
    /// is created and services start later; `.h5i/env.toml`'s `[service.*]` is
    /// pinned at box creation, so this is known in time.
    #[serde(skip)]
    pub hosts_services: bool,
    /// Runtime-only: the loopback ports this box may dial, and the only ones.
    ///
    /// On macOS a box shares the *host's* loopback, so `network_rules` refuses
    /// outbound to it wholesale — dialing it would reach host services, h5i's
    /// own proxies among them. But a box has legitimate business on loopback
    /// with itself: the browser's CDP endpoint, and the dev server a declared
    /// `[service]` is running. Both are ports h5i allocated for this box, so it
    /// can name them exactly instead of opening the interface.
    ///
    /// Never serialized: these are per-box runtime facts (a service's port is
    /// assigned when it starts), so a pinned policy digest is unchanged.
    #[serde(skip)]
    pub loopback_ports: Vec<u16>,
    /// Runtime-only: the loopback port the egress allowlist proxy should bind
    /// for this box, rather than whatever ephemeral port the OS hands out.
    ///
    /// It exists for one reason: on the macOS supervised tier the proxy *is*
    /// the box's only route out, and a `browser` box's Chrome deliberately
    /// outlives the `box run` that started it. Chrome takes its proxy address
    /// once, at launch, so an ephemeral port makes every navigation after that
    /// first run fail with `ERR_PROXY_CONNECTION_FAILED` — the box pointed at a
    /// port nothing is listening on any more. Pinning the port per env keeps
    /// the surviving browser pointed at the proxy that is actually there.
    ///
    /// Read only by the tier that has this problem: the macOS supervised path
    /// ([`crate::supervisor`]). The container tier's proxy stays ephemeral, and
    /// correctly so — its Chrome lives in the container and dies with the run,
    /// so nothing survives to remember an address.
    ///
    /// Allocated and persisted host-side by `env::prepare_browser_shim`,
    /// exactly like the CDP port next to it, and with the same exposure — with
    /// one addition worth stating plainly: the file it is persisted in lives
    /// under a directory the **box** is granted write on, so the *value* is
    /// box-controlled, not just squattable by a same-uid process. Neither is
    /// authority. `remembered_port` refuses a nonsense value (`0`, privileged),
    /// the number only ever reaches `bind`, and what the policy grants the box
    /// is the port the host actually bound — so a box that writes its own
    /// number gets a proxy there or an ephemeral fallback, and either way
    /// cannot reach a port the profile does not allow.
    ///
    /// That argument is about a *port*, and it does not carry to the pid file
    /// sitting next to it: a pid reaches `kill` on the host rather than `bind`.
    /// See `env::browser_pid`, which is why that one is checked to still name
    /// this box's own browser before anything is signalled.
    ///
    /// `None` → ephemeral, the previous behaviour.
    #[serde(skip)]
    pub egress_proxy_port: Option<u16>,
    /// Runtime-only: where the kernel tiers write `policy.effective.json` —
    /// the enforced state, serialized at the apply seam inside
    /// `build_confined_command` (ROADMAP.md §P1). Set by the host's `env` run
    /// paths; never serialized. Its digest is recorded per run in the capture
    /// record, not here.
    #[serde(skip)]
    pub effective_out: Option<PathBuf>,
}

impl ResolvedPolicy {
    pub fn new(claim: IsolationClaim, profile: Profile) -> Self {
        ResolvedPolicy {
            claim,
            profile,
            audit: AuditPolicy::default(),
            hosts_services: false,
            box_git: Vec::new(),
            env_capture_spool: None,
            env_inbox: None,
            private_binds: Vec::new(),
            home_binds: Vec::new(),
            ro_binds: Vec::new(),
            cache_write: None,
            work_readonly: false,
            user_egress_allow: Vec::new(),
            loopback_ports: Vec::new(),
            egress_proxy_port: None,
            effective_out: None,
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

/// A started background service, and **which world its pid belongs to**.
///
/// The distinction is not bookkeeping. A pid is only meaningful inside the pid
/// namespace that issued it, so a guest pid and a host pid are not two values
/// of one kind — they are the same integers naming unrelated processes. Handing
/// a guest pid to `kill(2)` would signal whatever host process happens to hold
/// that number, and signalling a *process group* (which is how a service is
/// stopped) would take its whole tree. So the runtime travels with the pid, and
/// every consumer dispatches on it rather than assuming.
#[derive(Debug, Clone)]
pub struct BackgroundHandle {
    /// The service's session-leader pid, in the namespace named by `sandbox`.
    pub pid: u32,
    /// `None` → a host process, signalable directly. `Some(name)` → a process
    /// inside that microVM guest, reachable only through the runtime.
    pub sandbox: Option<String>,
    /// The guest's boot identity when the service started. A guest keeps its
    /// name across `stop`/`start` but restarts its pids from 1, so without this
    /// a stale record can match an unrelated process in the guest's next life.
    pub boot: Option<String>,
}

impl BackgroundHandle {
    /// A service running as a host process (the kernel tiers).
    pub fn host(pid: u32) -> Self {
        BackgroundHandle {
            pid,
            sandbox: None,
            boot: None,
        }
    }
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

    /// SECURITY.md: the host-side allow list "merges into a profile that
    /// already sets `net.egress` and never widens a deny-all one". The test for
    /// that was `!net_egress.is_empty()`, and a blank entry is a `Vec` element
    /// and not a rule — `parse_egress_rule` skips it — so `net.egress = [""]`
    /// built an allowlist of zero entries while reporting that the profile had
    /// set one. A repo could write that and have the operator's own global
    /// allow rules applied to a box meant to reach nothing.
    #[test]
    fn a_blank_egress_entry_is_not_a_profile_opting_into_egress() {
        let mut p = Profile::builtin("default", IsolationClaim::Container);

        p.net_egress = Vec::new();
        assert!(!p.scopes_egress(), "an absent list names nothing");
        p.net_egress = vec![String::new()];
        assert!(!p.scopes_egress(), "a blank entry names nothing");
        p.net_egress = vec!["   ".into(), "\t".into()];
        assert!(!p.scopes_egress(), "nor several of them");

        p.net_egress = vec!["pypi.org".into()];
        assert!(p.scopes_egress());
        // One real rule among blanks is still a rule.
        p.net_egress = vec![String::new(), "pypi.org".into()];
        assert!(p.scopes_egress());
    }
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
    /// A default `[detect]` must not appear in the serialization at all, or
    /// every pinned policy digest in every existing box changes the day this
    /// field ships.
    #[test]
    fn a_default_detect_section_leaves_the_digest_untouched() {
        let plain = Profile::builtin("default", IsolationClaim::Process);
        let toml = toml::to_string(&plain).unwrap();
        assert!(!toml.contains("detect"), "{toml}");
    }

    #[test]
    fn an_enabled_detect_section_is_serialized_and_round_trips() {
        let mut p = Profile::builtin("default", IsolationClaim::Process);
        p.detect = DetectPolicy {
            enabled: true,
            require: true,
            buffer_kb: 512,
            rules: vec!["net".into(), "secret.read".into()],
        };
        let toml = toml::to_string(&p).unwrap();
        assert!(toml.contains("[detect]"), "{toml}");
        let back: Profile = toml::from_str(&toml).unwrap();
        assert_eq!(back.detect, p.detect);
    }

    /// The digest is what a reviewer compares two boxes by. Whether a box was
    /// watched has to be inside it.
    #[test]
    fn enabling_detection_changes_the_policy_digest() {
        let base = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        let mut watched = base.clone();
        watched.profile.detect.enabled = true;
        assert_ne!(base.digest().unwrap(), watched.digest().unwrap());
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
