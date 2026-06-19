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
use std::collections::BTreeMap;
use std::path::Path;
// PathBuf is only referenced from the `#[cfg(target_os = "linux")]` confinement
// paths (Landlock grants, config-lock); gate the import so non-Linux targets
// don't see it as unused under `-D warnings`.
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::time::Duration;

use crate::error::H5iError;

// The pure policy vocabulary lives in the dependency-leaf `sandbox_policy`
// module. Re-export it so `crate::sandbox::IsolationClaim` (etc.) keeps
// resolving for callers that also use the confinement machinery here, and so
// these names are in scope throughout this module.
// The pure policy *vocabulary* (types with no machinery deps) lives in the
// dependency-leaf `sandbox_policy` module. Re-exported so `crate::sandbox::X`
// keeps resolving for callers that also use the confinement machinery here,
// and so the names are in scope throughout this module. The container backend
// imports them from `sandbox_policy` directly, breaking the `sandbox →
// container → sandbox` dispatch cycle.
pub use crate::sandbox_policy::{
    AgentRuntime, AuditCapture, AuditPolicy, BoxGitPath, ExecOutcome, IsolationClaim, NetMode,
    PrivateBind, PrivatePath, Profile, ResolvedPolicy, SecretGrant, DEFAULT_WALL,
};

/// Repo-relative path of the checked-in policy file.
pub const POLICY_FILE: &str = ".h5i/env.toml";

// ─── policy vocabulary → moved to src/sandbox_policy.rs ──────────────────────
// `IsolationClaim`, `NetMode`, `Profile`, `SecretGrant`, `AgentRuntime`,
// `BoxGitPath`, `AuditCapture`, `AuditPolicy`, `ResolvedPolicy`, `ExecOutcome`
// and `DEFAULT_WALL` are re-exported above. The machinery that *operates* on
// them (resolve/validate/probe/run/confinement) stays here.

// `Profile` (impl builtin/builtin_agent/wall) and the `default_fs_read`/`default_fs_deny`
// helpers moved to src/sandbox_policy.rs.

/// Agent config paths whose mutation could disable the in-box observation hook,
/// locked **read-only** (bind + remount,ro) inside the box's mount namespace
/// for interactive agent sessions. Landlock is allowlist-only and cannot
/// subtract a writable child from a granted parent, so this mount-level lock is
/// how the kernel tiers make config immutable in-box without a managed-settings
/// tier (which they can't reach — `/etc/claude-code` can't be created from the
/// userns).
///
/// Two shapes, by scope:
/// - **Project scope (`$WORK/.claude`, `$WORK/.codex`) — the whole directory.**
///   A read-only directory blocks both editing existing config *and creating*
///   `settings.local.json` (the `disableAllHooks` create-bypass that per-file
///   pinning can't stop). Safe to lock: agents read project config but don't
///   write it at runtime.
/// - **User scope — the single settings file only** (`~/.claude/settings.json`,
///   `~/.codex/config.toml`). `~/.claude` itself must stay writable (the agent
///   stores session state there), and locking the whole dir would brick the
///   runtime. There is no `~/.claude/settings.local.json` in Claude's
///   precedence chain, and the Codex `[features] hooks=false` kill switch lives
///   only in `config.toml`, so pinning the one file closes user scope.
///
/// Only **existing** paths are returned — a bind needs an existing target. An
/// absent project config dir is a documented residual: the agent could create
/// `$WORK/.claude` and a local-scope `disableAllHooks`. Closing that would mean
/// shadowing the (possibly absent) dir, which the tee-shim floor covers instead.
#[cfg(target_os = "linux")]
fn config_lock_paths(work: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [".claude", ".codex"] {
        let p = work.join(dir);
        if p.is_dir() {
            out.push(p);
        }
    }
    if let Some(home) = home {
        for file in [".claude/settings.json", ".codex/config.toml"] {
            let p = home.join(file);
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
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
    /// Per-env private paths (Idea 3):
    /// `[profile.X.private_paths] "target" = { kind = "cache", persist = true }`.
    #[serde(default)]
    private_paths: BTreeMap<String, PrivatePathToml>,
    /// Opt-in for the secrets broker's host-side `command:` extractor.
    #[serde(default)]
    allow_command_extractors: bool,
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

#[derive(Debug, Default, Deserialize)]
struct PrivatePathToml {
    /// `cache` (default) | `scratch` | `private`.
    kind: Option<String>,
    /// Keep across runs (default `true` for `cache`, `false` otherwise).
    persist: Option<bool>,
}

/// Build the sorted `private_paths` list from the `[profile.X.private_paths]`
/// table. Deterministic order (BTreeMap) for a stable policy digest.
fn build_private_paths(raw: &BTreeMap<String, PrivatePathToml>) -> Vec<crate::sandbox_policy::PrivatePath> {
    raw.iter()
        .map(|(path, cfg)| {
            let kind = cfg.kind.clone().unwrap_or_else(|| "cache".to_string());
            // Sensible default: caches are worth keeping warm; scratch/private
            // (lock dirs, stale build output) default to wipe-per-run.
            let persist = cfg.persist.unwrap_or(kind == "cache");
            crate::sandbox_policy::PrivatePath {
                path: path.clone(),
                kind,
                persist,
            }
        })
        .collect()
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

/// The built-in profile for `name`: the agent-in-box defaults for `agent`,
/// the fail-closed build/test defaults for everything else. Used both as the
/// no-`env.toml` fallback and as the merge base under a user-defined profile
/// of the same name (so a partial `[profile.agent]` keeps the agent grants).
fn builtin_named(name: &str, isolation: IsolationClaim) -> Profile {
    match name {
        // Bare `agent` scopes to whoever is driving the box ($H5I_AGENT);
        // `agent-claude`/`agent-codex` pin the runtime explicitly.
        "agent" => Profile::builtin_agent(isolation, AgentRuntime::detect()),
        "agent-claude" => Profile::builtin_agent(isolation, AgentRuntime::Claude),
        "agent-codex" => Profile::builtin_agent(isolation, AgentRuntime::Codex),
        _ => Profile::builtin(name, isolation),
    }
}

/// Is `name` backed by a built-in profile (usable without `.h5i/env.toml`)?
fn is_builtin_name(name: &str) -> bool {
    matches!(name, "default" | "agent" | "agent-claude" | "agent-codex")
}

/// Is `name` an agent-in-box profile (the family that grants claude/codex HOME
/// state + API egress)? Used to decide whether a box can actually run an agent.
pub fn is_agent_profile(name: &str) -> bool {
    matches!(name, "agent" | "agent-claude" | "agent-codex")
}

/// Load profile `name` from `<repo>/.h5i/env.toml`, falling back to the
/// built-in when the file (or the profile entry) is absent and `name` is a
/// built-in one (`default`, `agent`).
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
            None if is_builtin_name(name) => None,
            None => {
                return Err(H5iError::Metadata(format!(
                    "profile '{name}' not found in {POLICY_FILE} (available: {})",
                    file.profile.keys().cloned().collect::<Vec<_>>().join(", ")
                )))
            }
        }
    } else if !is_builtin_name(name) {
        return Err(H5iError::Metadata(format!(
            "profile '{name}' requested but {POLICY_FILE} does not exist"
        )));
    } else {
        None
    };

    let mut profile = match raw {
        None => builtin_named(name, isolation_override.unwrap_or(IsolationClaim::Workspace)),
        Some(t) => {
            let isolation = match (&isolation_override, &t.isolation) {
                (Some(o), _) => *o,
                (None, Some(s)) => IsolationClaim::parse(s)?,
                (None, None) => IsolationClaim::Workspace,
            };
            let base = builtin_named(name, isolation);
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
                private_paths: if t.private_paths.is_empty() {
                    base.private_paths
                } else {
                    build_private_paths(&t.private_paths)
                },
                allow_command_extractors: t.allow_command_extractors
                    || base.allow_command_extractors,
            }
        }
    };
    if let Some(o) = isolation_override {
        profile.isolation = o;
    }
    validate_profile(&profile)?;
    Ok(profile)
}

/// What isolation the caller requested for `env create`: a specific claim
/// (fail-closed — refused, never downgraded, if the host can't satisfy it), or
/// `Auto` — pick the strongest tier the host can actually run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationRequest {
    Auto,
    Claim(IsolationClaim),
}

/// The isolation a profile *declares* in `.h5i/env.toml` (its `isolation =`
/// field), or `None` when it's absent or set to `auto`. Read directly so the
/// auto-picker can honor an explicit profile choice without probing the host.
fn profile_declared_isolation(repo_workdir: &Path, name: &str) -> Result<Option<IsolationClaim>, H5iError> {
    let path = repo_workdir.join(POLICY_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    let file: PolicyFileToml = toml::from_str(&text)?;
    match file.profile.get(name).and_then(|p| p.isolation.as_deref()) {
        None => Ok(None),
        Some(s) if s.eq_ignore_ascii_case("auto") => Ok(None),
        Some(s) => Ok(Some(IsolationClaim::parse(s)?)),
    }
}

/// Pick the isolation tier for `env create` when none is pinned explicitly: the
/// **strongest** tier the host can actually run for this profile
/// (`container > supervised > process > workspace`), so the default is
/// secure-by-default and always works. Each candidate is gated by the *same*
/// checks `create` applies (`resolve` + `verify_exec`), so a picked tier is
/// guaranteed runnable — never a tier that would then fail at run time.
///
/// `force_probe = false` (the CLI default, no `--isolation`) honors a tier the
/// profile explicitly declares; `force_probe = true` (`--isolation auto`)
/// re-probes regardless. Explicit `--isolation <tier>` never reaches here — it
/// stays fail-closed.
pub fn effective_auto(
    repo_workdir: &Path,
    name: &str,
    force_probe: bool,
) -> Result<IsolationClaim, H5iError> {
    if !force_probe {
        if let Some(c) = profile_declared_isolation(repo_workdir, name)? {
            return Ok(c);
        }
        // An explicit org/user default (`H5I_DEFAULT_ISOLATION`) pins the tier
        // without probing — set it to opt a whole clone into a fixed tier.
        // `--isolation auto` (force_probe) ignores it and re-probes.
        if let Ok(v) = std::env::var("H5I_DEFAULT_ISOLATION") {
            let v = v.trim();
            if !v.is_empty() && !v.eq_ignore_ascii_case("auto") {
                return IsolationClaim::parse(v);
            }
        }
    }
    let caps = probe_host();
    // Strongest first. `container` is only picked when the profile sets an
    // image (resolve refuses it otherwise), so the bare default lands on the
    // strongest *kernel* confinement instead.
    for tier in [
        IsolationClaim::Container,
        IsolationClaim::Supervised,
        IsolationClaim::Process,
    ] {
        let Ok(profile) = load_profile(repo_workdir, name, Some(tier)) else {
            continue;
        };
        let runnable = resolve(&profile, &caps).and_then(|pol| verify_exec(&pol)).is_ok();
        if runnable {
            return Ok(tier);
        }
    }
    Ok(IsolationClaim::Workspace)
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
        if !(src.starts_with("env:") || src.starts_with("file:") || src.starts_with("command:")) {
            return Err(H5iError::Metadata(format!(
                "secret grant '{}' has unsupported source '{src}' — use 'env:VAR', \
                 'file:/abs/path', or 'command:<shell>' (fail-closed)",
                g.name
            )));
        }
        // A command: source executes host-side code outside the sandbox — refuse
        // it at policy-load unless the profile explicitly opts in, so the gate is
        // pinned in the (tamper-evident) digest, not just enforced at run time.
        if src.starts_with("command:") && !p.allow_command_extractors {
            return Err(H5iError::Metadata(format!(
                "secret grant '{}' uses a command: extractor (host-side code outside the \
                 sandbox) but the profile does not set `allow_command_extractors = true` \
                 (fail-closed)",
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
    validate_private_paths(p)?;
    Ok(())
}

/// Validate `private_paths` (Idea 3), fail-closed: each path is
/// workspace-relative, free of `..` traversal, has a known `kind`, and no two
/// paths overlap (a parent would shadow the nested child's bind). Mirrors the
/// Coasts validation rules plus h5i's no-`..` requirement.
fn validate_private_paths(p: &Profile) -> Result<(), H5iError> {
    const KINDS: [&str; 3] = ["cache", "scratch", "private"];
    let norm: Vec<String> = p
        .private_paths
        .iter()
        .map(|pp| pp.path.trim_matches('/').to_string())
        .collect();
    for (i, pp) in p.private_paths.iter().enumerate() {
        let rel = &pp.path;
        if rel.is_empty() || norm[i].is_empty() {
            return Err(H5iError::Metadata(
                "private_paths entry is empty — give a workspace-relative directory".into(),
            ));
        }
        if rel.starts_with('/') {
            return Err(H5iError::Metadata(format!(
                "private_paths '{rel}' must be workspace-relative (no leading '/') (fail-closed)"
            )));
        }
        if rel.split('/').any(|c| c == "..") {
            return Err(H5iError::Metadata(format!(
                "private_paths '{rel}' must not contain '..' (fail-closed)"
            )));
        }
        // A comma cannot be carried by Podman's `--mount` syntax, so a private
        // bind with a comma in its path could not be applied at the container
        // tier. Reject it at policy load rather than silently skipping the bind
        // (an enforcement feature must fail closed, not fail open).
        if rel.contains(',') {
            return Err(H5iError::Metadata(format!(
                "private_paths '{rel}' must not contain ',' (unsupported by the container \
                 mount syntax) (fail-closed)"
            )));
        }
        if !KINDS.contains(&pp.kind.as_str()) {
            return Err(H5iError::Metadata(format!(
                "private_paths '{rel}' has unknown kind '{}' — use cache|scratch|private \
                 (shared cross-env state is not supported in v1; use an explicit fs grant) \
                 (fail-closed)",
                pp.kind
            )));
        }
    }
    // No overlap: listing both `a` and `a/b` is an error — the first bind would
    // shadow the second's mountpoint.
    for i in 0..norm.len() {
        for j in 0..norm.len() {
            if i == j {
                continue;
            }
            if norm[i] == norm[j] || norm[j].starts_with(&format!("{}/", norm[i])) {
                return Err(H5iError::Metadata(format!(
                    "private_paths '{}' overlaps '{}' — paths must not nest (fail-closed)",
                    p.private_paths[i].path, p.private_paths[j].path
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

// `BoxGitPath` moved to src/sandbox_policy.rs (re-exported below).

// `AuditCapture`, `AuditPolicy`, `ResolvedPolicy` moved to src/sandbox_policy.rs.

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
            // Rootless Podman adapter (opt-in shell-out). Require an image AND
            // the runtime — fail closed, never silently downgrade. Validate the
            // declared config (image) BEFORE probing host capability (podman):
            // a missing image is a static profile error, true regardless of the
            // host, so reporting it first keeps the error host-independent — a
            // box (or CI) without podman still gets the actionable
            // "set container.image" message rather than a podman-not-found one.
            if profile.image.is_none() {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'container' requires a base image — set `container.image = \
                     \"…\"` in profile '{}' (e.g. your toolchain image)",
                    profile.name
                )));
            }
            if caps.container_runtime.is_none() {
                return Err(H5iError::Metadata(
                    "isolation claim 'container' requires rootless Podman on PATH; Docker and \
                     rootful Podman are intentionally not accepted in this Linux/WSL backend — \
                     install/configure rootless podman, or re-request --isolation workspace/process"
                        .into(),
                ));
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
    Ok(ResolvedPolicy::new(profile.isolation, profile.clone()))
}

// ─── confined execution (Linux, `process` tier) ─────────────────────────────

// `ExecOutcome` moved to src/sandbox_policy.rs (re-exported above).

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
    let injected = augment_injected_env(policy, injected_env);
    let injected_env = injected.as_slice();
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

/// Spawn `argv` as a long-lived **background** process under the env's
/// confinement, with stdout+stderr redirected to `log` and stdin `/dev/null`.
/// Returns the child PID. Unlike [`run_with_env`] it does NOT wait or apply a
/// wall-clock kill — a service is operator-bounded (stopped explicitly). The
/// child gets its own session/process group so a later `killpg` reaps the whole
/// tree. v1 supports the workspace and process tiers; supervised/container
/// services are a documented follow-up (Idea 3.5).
pub fn spawn_background(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    log: &Path,
) -> Result<u32, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    check_tool_allowlist(policy, argv)?;
    let injected = augment_injected_env(policy, injected_env);
    let injected_env = injected.as_slice();
    let out = std::fs::File::create(log).map_err(|e| H5iError::with_path(e, log))?;
    let err = out.try_clone().map_err(H5iError::Io)?;
    match policy.claim {
        IsolationClaim::Workspace => {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .current_dir(work)
                .stdin(std::process::Stdio::null())
                .stdout(out)
                .stderr(err);
            cmd.env_clear();
            for key in &policy.profile.env_pass {
                if let Ok(v) = std::env::var(key) {
                    cmd.env(key, v);
                }
            }
            apply_injected_env(&mut cmd, injected_env);
            // Own session so a later killpg(pid) reaps the whole descendant tree.
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
            let child = cmd
                .spawn()
                .map_err(|e| H5iError::Metadata(format!("service failed to start: {e}")))?;
            Ok(child.id())
        }
        IsolationClaim::Process => spawn_background_confined(policy, work, argv, injected_env, out, err),
        claim => Err(H5iError::Metadata(format!(
            "services are not supported at isolation '{}' in v1 — use workspace or process",
            claim.as_str()
        ))),
    }
}

/// Process-tier background spawn: the shared confinement (Landlock + seccomp +
/// ns + rlimits) with no PID namespace (so the returned PID is the service
/// itself, killpg-able) and no wall-clock kill. Linux only.
#[cfg(target_os = "linux")]
fn spawn_background_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    out: std::fs::File,
    err: std::fs::File,
) -> Result<u32, H5iError> {
    let net_deny = policy.profile.net_mode == NetMode::Deny;
    // interactive=false → setsid (own pgid); pidns=false → no supervisor fork,
    // so child.id() is the service process; no cgroup/wall-clock kill.
    let mut cmd = build_confined_command(
        policy, work, argv, injected_env, net_deny, None, None, false, None, false,
    )?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    let child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("confined service failed to start: {e}")))?;
    Ok(child.id())
}

#[cfg(not(target_os = "linux"))]
fn spawn_background_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
    _out: std::fs::File,
    _err: std::fs::File,
) -> Result<u32, H5iError> {
    Err(H5iError::Metadata(
        "process-tier services require Linux".into(),
    ))
}

/// The **agent-in-box** entry point: run `argv` (a shell or a coding agent)
/// interactively under the env's confinement. stdio is **inherited** (a real
/// session, not captured), nothing is recorded per-command, and the child's exit
/// code is returned. Confinement comes from the box itself, so whatever the
/// agent spawns inside is contained by construction — the enforcement no longer
/// depends on the agent choosing to wrap each command.
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    check_tool_allowlist(policy, argv)?;
    let injected = augment_injected_env(policy, injected_env);
    let injected_env = injected.as_slice();
    match policy.claim {
        IsolationClaim::Workspace => interactive_unconfined(work, argv, injected_env),
        IsolationClaim::Process => interactive_confined(policy, work, argv, injected_env),
        IsolationClaim::Supervised => {
            crate::supervisor::run_interactive(policy, work, argv, injected_env)
        }
        IsolationClaim::Container => {
            crate::container::run_interactive(policy, work, argv, injected_env)
        }
        claim => Err(H5iError::Metadata(format!(
            "no interactive backend for isolation claim '{}'",
            claim.as_str()
        ))),
    }
}

/// Interactive workspace tier: inherited stdio, a new session so signals reach
/// the whole tree, no confinement (trusted code).
fn interactive_unconfined(
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(work);
    apply_injected_env(&mut cmd, injected_env);
    let status = cmd
        .status()
        .map_err(|e| H5iError::Metadata(format!("failed to start '{}': {e}", argv[0])))?;
    Ok(status.code().unwrap_or(130))
}

/// Interactive process tier: the shared confinement (Landlock + seccomp + ns +
/// rlimits + cgroup) with stdio inherited. The profile's wall-clock is *not*
/// applied — an interactive session is bounded by the operator, not a timer.
#[cfg(target_os = "linux")]
fn interactive_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    let p = &policy.profile;
    // Same rule as the captured path: a fresh netns only when egress is denied.
    let net_deny = p.net_mode == NetMode::Deny;
    let cg = make_run_cgroup(p.mem_bytes, p.max_procs);
    let procs = cg.as_ref().map(|c| c.procs_path());
    // Process tier interactive: confine the session to a fresh PID namespace +
    // private procfs too (pidns=true), with the supervisor joining it to cgroup.
    let mut cmd = build_confined_command(
        policy, work, argv, injected_env, net_deny, None, None, true, procs.as_deref(), true,
    )?;
    // build_confined_command leaves stdio unset → inherited (the session).
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("confined session failed to start: {e}")))?;
    if let Some(cgrp) = &cg {
        let _ = std::fs::write(cgrp.procs_path(), child.id().to_string());
    }
    let status = child.wait().map_err(H5iError::Io)?;
    Ok(status.code().unwrap_or(130))
}

#[cfg(not(target_os = "linux"))]
fn interactive_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    Err(H5iError::Metadata(
        "isolation=process requires Linux".into(),
    ))
}

/// Apply the secrets broker's injected env vars to a child command (used by each
/// tier). Applied after `env.pass`, so a grant can't be shadowed by a host var.
fn apply_injected_env(cmd: &mut std::process::Command, injected_env: &[(String, String)]) {
    for (k, v) in injected_env {
        cmd.env(k, v);
    }
}

/// For an **agent-in-box** profile, signal Claude Code that uid 0 inside the box
/// is a sandbox artifact, not real root, so `--dangerously-skip-permissions`
/// works. The egress tiers map the agent to root-*in-userns* (it needs
/// `CAP_NET_ADMIN` to survive `execve` for `nft`; see the uid_map in
/// `run_confined`/the supervisor), and Claude's guard refuses
/// `--dangerously-skip-permissions` on a bare `getuid()==0`. `IS_SANDBOX=1`
/// skips only that root check — it grants the agent **no** new capability (the
/// box already pins it to our real unprivileged host uid, with zero host
/// privilege). Scoped to agent profiles so ordinary confined runs don't inherit
/// a sandbox signal they don't need. A caller-supplied / broker `IS_SANDBOX`
/// (or any host one passed via `env.pass`) wins — we only set the default.
fn augment_injected_env(
    policy: &ResolvedPolicy,
    injected_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = injected_env.to_vec();
    if is_agent_profile(&policy.profile.name)
        && !env.iter().any(|(k, _)| k == "IS_SANDBOX")
        && !policy.profile.env_pass.iter().any(|k| k == "IS_SANDBOX")
    {
        env.push(("IS_SANDBOX".to_string(), "1".to_string()));
    }
    env
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
    let probe = ResolvedPolicy::new(policy.claim, profile);
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

/// The child-side handles for the `supervised` **egress allowlist** (increment
/// 2). When `build_confined_command` is given one, the child — while it still
/// holds `CAP_NET_ADMIN`/`CAP_SYS_ADMIN` in its own user namespace and *before*
/// Landlock/seccomp lock it down — pins DNS via a private `/etc/hosts` and
/// installs the nftables default-drop allowlist in its netns, after a host-side
/// helper (the `slirp4netns` uplink) signals readiness. Every field is built
/// pre-fork and is `Send`; the child touches them with raw syscalls only (no
/// allocation in the forked child). See `supervisor::EgressNetns`.
#[cfg(target_os = "linux")]
pub(crate) struct EgressJail {
    /// Child reads 1 byte here once `slirp4netns` has configured the uplink.
    pub ready_read_fd: std::os::unix::io::RawFd,
    /// Child writes its 4-byte pid here so the helper can target its netns.
    pub pid_write_fd: std::os::unix::io::RawFd,
    /// Absolute path to the `nft` binary (resolved on the host).
    pub nft_path: std::ffi::CString,
    /// Path to the temp file holding the nftables ruleset (`nft -f`).
    pub nft_rules_path: std::ffi::CString,
    /// Minimal `PATH=…` for the `nft` exec (the only env it gets).
    pub nft_envp: std::ffi::CString,
    /// Path to the temp file holding the pinned `/etc/hosts` content.
    pub hosts_src: std::ffi::CString,
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
/// - `egress`: when `Some`, install the netns egress allowlist (see [`EgressJail`]).
/// - `pidns`: when `true`, run the workload in a fresh PID namespace with a
///   private procfs (design §5 "PID view"), so it cannot see — or read
///   `/proc/<pid>/environ` of — host processes. Implemented by forking inside
///   `pre_exec`: the parent becomes a thin supervisor that mirrors the workload's
///   exit, the child is PID 1 of the new namespace. The `process` tier sets this;
///   `supervised` does not yet (it has its own model).
/// - `cgroup_procs`: path to the run cgroup's `cgroup.procs`. Only consulted when
///   `pidns` is set — the supervisor writes the *workload's* pid there so the
///   cgroup's `memory.max`/accounting bind the real process, not the supervisor.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)] // the security-critical setup is intentionally one audited fn
pub(crate) fn build_confined_command(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    force_netns: bool,
    notify_sock: Option<std::os::unix::io::RawFd>,
    egress: Option<EgressJail>,
    pidns: bool,
    cgroup_procs: Option<&Path>,
    interactive: bool,
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
    // The Landlock ABI is needed again *inside* the forked child to re-grant the
    // freshly-mounted procfs (the pre-fork `/proc` grant pins the host procfs
    // inode, which the new mount shadows). Captured by value (Copy).
    let ll_abi = abi;
    // The cgroup.procs path, pre-resolved to a CString so the alloc-free
    // supervisor branch can move the workload into the cgroup.
    let cgroup_procs_c: Option<std::ffi::CString> = cgroup_procs
        .and_then(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok());

    // Config-lockdown targets (interactive agent sessions only), pre-resolved to
    // CStrings so the post-fork child does no allocation when binding them. A
    // non-empty list forces a mount namespace below — supervised is pidns=false,
    // so without this there is no private mount ns and a bind would be unsafe.
    let config_lock_c: Vec<std::ffi::CString> = if interactive {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        config_lock_paths(&work, home.as_deref())
            .iter()
            .filter_map(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok())
            .collect()
    } else {
        Vec::new()
    };

    // Private-path binds (Idea 3): (backing, target) CString pairs, pre-resolved
    // so the post-fork child does no allocation. `target` is the workspace path
    // the per-env backing dir shadows. A non-empty list forces a mount namespace
    // below, exactly like config lockdown.
    let private_bind_c: Vec<(std::ffi::CString, std::ffi::CString)> = policy
        .private_binds
        .iter()
        .filter_map(|b| {
            let target = work.join(&b.rel);
            let bc = std::ffi::CString::new(b.backing.as_os_str().as_encoded_bytes()).ok()?;
            let tc = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()).ok()?;
            Some((bc, tc))
        })
        .collect();

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
            //    Interactive (agent-in-box) sessions skip this: setsid detaches
            //    the child from the controlling terminal, which breaks job
            //    control and every TUI ("cannot set terminal process group").
            //    They keep the caller's session — exactly how a nested shell
            //    runs — and have no wall-clock kill (operator-bounded), so the
            //    killpg guarantee isn't needed. (TIOCSTI keystroke injection
            //    via the shared tty is gated off by default since kernel 6.2,
            //    CONFIG_LEGACY_TIOCSTI; a PTY-proxy is the airtight follow-up.)
            if !interactive && libc::setsid() == -1 {
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
            if pidns {
                // A new PID namespace (so host processes are invisible/unsignalable)
                // plus a new mount namespace (so we can mount a private procfs over
                // /proc without touching the host). The userns in the same call
                // grants the CAP_SYS_ADMIN both need, unprivileged.
                flags |= libc::CLONE_NEWPID | libc::CLONE_NEWNS;
            }
            // Config lockdown needs a private mount namespace to ro-bind in
            // (supervised is pidns=false, so it would otherwise have none). The
            // bind is contained: a mount ns under a fresh userns reduces shared
            // mounts to slave, so it never propagates to the host.
            if !config_lock_c.is_empty() || !private_bind_c.is_empty() {
                flags |= libc::CLONE_NEWNS;
            }
            if libc::unshare(flags) != 0 {
                return Err(Error::last_os_error());
            }
            std::fs::write("/proc/self/setgroups", "deny")?;
            // The egress path execs `nft` to install the allowlist; capabilities
            // only survive execve for uid 0 in the user ns, so map the child to
            // root-in-userns there (CAP_NET_ADMIN is kept ⇒ nft can touch
            // netlink). The map still points back to our real host uid, so files
            // created in $WORK stay owned by us. The non-egress tiers keep the
            // 1:1 map (the untrusted program runs as our own uid).
            if egress.is_some() {
                std::fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;
                std::fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
            } else {
                std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
                std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
            }

            // 1b. Egress allowlist (supervised increment 2). We still hold full
            //     caps in our userns and seccomp/Landlock are not yet applied, so
            //     this is the window to: tell the host helper our pid (it spawns
            //     the slirp4netns uplink for this netns), pin DNS via a private
            //     /etc/hosts, install the nftables default-drop allowlist, and
            //     wait for the uplink before continuing. Raw syscalls only — no
            //     allocation in this forked child.
            if let Some(eg) = &egress {
                use std::ptr::null;
                // (a0) A private mount namespace for the pinned /etc/hosts —
                //      unshared *after* the user ns is fully set up (maps written).
                if libc::unshare(libc::CLONE_NEWNS) != 0 {
                    return Err(Error::other(format!("egress: unshare NEWNS: {}", Error::last_os_error())));
                }
                // (a) Hand our pid to the helper so it can target our netns.
                let pid = libc::getpid() as u32;
                let pidbuf = pid.to_ne_bytes();
                if libc::write(eg.pid_write_fd, pidbuf.as_ptr().cast(), 4) != 4 {
                    return Err(Error::other(format!("egress: write pid: {}", Error::last_os_error())));
                }
                // (b) Bind the pinned /etc/hosts over the real one. The mount ns
                //     was unshared under our user ns, so this mount cannot
                //     propagate back to the host. (A recursive MS_PRIVATE on "/"
                //     is unnecessary here and returns EINVAL under some kernels.)
                if libc::mount(eg.hosts_src.as_ptr(), c"/etc/hosts".as_ptr(), null(), libc::MS_BIND, null()) != 0 {
                    return Err(Error::other(format!("bind /etc/hosts failed: {}", Error::last_os_error())));
                }
                // (c) Apply the nftables ruleset (CAP_NET_ADMIN in our userns).
                //     Raw fork/execve so nothing allocates in this child.
                let argv: [*const libc::c_char; 4] =
                    [eg.nft_path.as_ptr(), c"-f".as_ptr(), eg.nft_rules_path.as_ptr(), null()];
                let envp: [*const libc::c_char; 2] = [eg.nft_envp.as_ptr(), null()];
                let kid = libc::fork();
                if kid == 0 {
                    libc::execve(eg.nft_path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    libc::_exit(127);
                }
                if kid < 0 {
                    return Err(Error::last_os_error());
                }
                let mut st = 0;
                if libc::waitpid(kid, &mut st, 0) < 0 {
                    return Err(Error::last_os_error());
                }
                if !(libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0) {
                    return Err(Error::other("nft egress ruleset failed to apply (fail-closed)"));
                }
                // (d) Block until slirp4netns has configured the uplink, so the
                //     program never races a not-yet-ready interface.
                let mut rb = [0u8; 1];
                if libc::read(eg.ready_read_fd, rb.as_mut_ptr().cast(), 1) != 1 {
                    return Err(Error::other("slirp4netns uplink did not become ready"));
                }
            }

            // 1c. PID-namespace jail (process tier, design §5 "PID view").
            //     CLONE_NEWPID only takes effect for the *next* child, so fork:
            //     the parent becomes a thin supervisor that mirrors the workload's
            //     fate to the h5i waiter; the child is PID 1 of the new namespace.
            //     A private procfs is mounted so the workload cannot enumerate, or
            //     read /proc/<pid>/environ of, host processes — notably this h5i
            //     process, which holds the operator's environment (defeating the
            //     env.pass allowlist). Raw syscalls + one File::open only.
            if pidns {
                let kid = libc::fork();
                if kid > 0 {
                    // Supervisor. First move the *workload* into the run cgroup
                    // (so memory.max + accounting bind it, not us — it was forked
                    // before the host-side cgroup write, which only sees us).
                    if let Some(cpath) = &cgroup_procs_c {
                        let fd = libc::open(cpath.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                        if fd >= 0 {
                            let line = format!("{kid}");
                            let _ = libc::write(fd, line.as_ptr().cast(), line.len());
                            libc::close(fd);
                        }
                    }
                    // Reap the workload and mirror its exit/signal so the waiter
                    // observes the real outcome through this supervisor.
                    let mut st: libc::c_int = 0;
                    loop {
                        let r = libc::waitpid(kid, &mut st, 0);
                        if r == kid {
                            break;
                        }
                        if r < 0 && Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        libc::_exit(125);
                    }
                    if libc::WIFEXITED(st) {
                        libc::_exit(libc::WEXITSTATUS(st));
                    }
                    if libc::WIFSIGNALED(st) {
                        // Re-raise so the waiter sees a signal death (exit_code
                        // None), matching the non-pidns path. The wall-clock
                        // SIGKILL already reaches us directly via the process group.
                        let sig = libc::WTERMSIG(st);
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                        libc::_exit(128 + sig);
                    }
                    libc::_exit(125);
                }
                if kid < 0 {
                    return Err(Error::last_os_error());
                }
                // Child = PID 1 of the new namespace. Mount a private procfs over
                // /proc so only this namespace is visible, then re-grant Landlock
                // read on the *new* procfs (the pre-fork grant pinned the host
                // procfs inode, now shadowed by this mount).
                if libc::mount(
                    c"proc".as_ptr(),
                    c"/proc".as_ptr(),
                    c"proc".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "pidns: mount private /proc failed: {}",
                        Error::last_os_error()
                    )));
                }
                use landlock::{AccessFs, PathBeneath, RulesetCreatedAttr};
                let proc_fd = std::fs::File::open("/proc")
                    .map_err(|e| Error::other(format!("pidns: open new /proc: {e}")))?;
                let rs = ruleset_slot
                    .take()
                    .ok_or_else(|| Error::other("landlock ruleset consumed before /proc re-grant"))?;
                let rs = rs
                    .add_rule(PathBeneath::new(proc_fd, AccessFs::from_read(ll_abi)))
                    .map_err(|e| Error::other(format!("pidns: landlock /proc re-grant failed: {e}")))?;
                ruleset_slot = Some(rs);
            }

            // 1d. Config lockdown (interactive agent sessions). Bind each agent
            //     config path read-only so the in-box agent cannot edit it — and,
            //     for the project-scope DIRECTORIES, cannot create new files in it
            //     (e.g. a `settings.local.json` carrying `disableAllHooks`). This
            //     runs in our private mount namespace (forced above), before
            //     Landlock/seccomp, while we still hold CAP_SYS_ADMIN in the
            //     userns; `mount`/`umount2` are on the seccomp deny-list, so the
            //     workload can neither undo nor stack over these. Fail-closed: a
            //     lock we set out to apply but couldn't is an error, never a
            //     silent run with mutable config.
            for c in &config_lock_c {
                let p = c.as_ptr();
                if libc::mount(p, p, std::ptr::null(), libc::MS_BIND, std::ptr::null()) != 0 {
                    return Err(Error::other(format!(
                        "config lock bind failed: {}",
                        Error::last_os_error()
                    )));
                }
                if libc::mount(
                    std::ptr::null(),
                    p,
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "config lock remount-ro failed: {}",
                        Error::last_os_error()
                    )));
                }
            }

            // 1e. Private-path binds (Idea 3). Bind each per-env backing dir
            //     over its workspace-relative path so concurrent envs of the
            //     same repo see distinct inodes — no cross-env `flock`/`fcntl`
            //     or single-writer-cache contention (Cargo `target/`, Next
            //     `.next/dev/lock`, …). Read-write (unlike the ro config
            //     lockdown above); same private mount ns, before Landlock. The
            //     backing dir is separately Landlock-granted host-side so access
            //     through the bind is allowed. Fail-closed.
            for (backing, target) in &private_bind_c {
                if libc::mount(
                    backing.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "private-path bind failed: {}",
                        Error::last_os_error()
                    )));
                }
            }

            // 2. Resource caps (cooperative, no cgroups needed).
            if let Some(bytes) = mem {
                // RLIMIT_DATA, not RLIMIT_AS. RLIMIT_AS caps *virtual address
                // space*, which modern runtimes over-reserve by design: V8/Node
                // maps a ~1TiB PROT_NONE heap-sandbox cage at startup, Go reserves
                // large arenas — none of it resident. An AS cap rejects those
                // reservations and the process aborts at trivial RSS ("JavaScript
                // heap out of memory" at ~100MiB). RLIMIT_DATA caps the writable
                // data segment (brk + writable-anonymous mmaps, Linux >=4.7), so
                // it bounds actual heap growth without counting PROT_NONE
                // reservations (is_data_mapping() requires VM_WRITE). This is the
                // rlimit-tier fallback; cgroup `memory.max` (when the host
                // delegates one — see cgroup.rs) is the accurate whole-subtree
                // RSS cap layered on top.
                let lim = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
                if libc::setrlimit(libc::RLIMIT_DATA, &lim) != 0 {
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
    // cgroup v2 (rootless, best-effort): real memory.max/pids.max + accurate
    // memory.peak/cpu accounting where the host delegates a writable subtree.
    // Created BEFORE the command so its `cgroup.procs` path can be handed to the
    // PID-namespace supervisor (which joins the workload to it). Unavailable →
    // `None`, and the rlimits set in the child still apply.
    let cg = make_run_cgroup(p.mem_bytes, p.max_procs);
    let procs = cg.as_ref().map(|c| c.procs_path());
    // Process tier: netns only when egress is denied; no seccomp-notify gate; the
    // workload is confined to a fresh PID namespace + private procfs (pidns=true).
    let cmd = build_confined_command(
        policy, work, argv, injected_env, false, None, None, true, procs.as_deref(), false,
    )?;

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
    } else {
        // Under the PID-namespace jail the workload runs as a grandchild of a thin
        // supervisor, so `wait4`'s rusage is the supervisor's, not the workload's.
        // Without a cgroup we cannot attribute rss/cpu — report unknown rather
        // than a misleading figure. The in-child rlimits still *enforce* the caps.
        outcome.max_rss_kb = None;
        outcome.cpu_ms = 0;
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

/// The curated set of syscall numbers the deny-list blocks (returns EPERM).
///
/// This is the security contract — kept as its own function so a unit test can
/// assert the security-critical members are present without a kernel. Every
/// entry is an administrative / introspection / namespace / fs-handle syscall
/// that a build or test workload never legitimately issues, so a blanket EPERM
/// is safe. We deliberately do NOT deny clone/clone3/fork (needed for normal
/// subprocesses); the documented clone-with-CLONE_NEWUSER gap is closed by the
/// hardened allowlist profile (a later phase), not here.
#[cfg(target_os = "linux")]
fn denied_syscalls() -> Vec<libc::c_long> {
    // libc's musl/aarch64 module omits SYS_kexec_file_load (it is present on
    // glibc and on musl/x86_64). Supply the arch syscall number ourselves so the
    // deny-list still blocks it there; everywhere else use libc's constant.
    #[cfg(all(target_env = "musl", target_arch = "aarch64"))]
    const SYS_KEXEC_FILE_LOAD: libc::c_long = 294;
    #[cfg(not(all(target_env = "musl", target_arch = "aarch64")))]
    const SYS_KEXEC_FILE_LOAD: libc::c_long = libc::SYS_kexec_file_load;

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
        SYS_KEXEC_FILE_LOAD,
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
    denied
}

/// The seccomp **deny-list** (v1, §5): dangerous administrative / introspection
/// syscalls return EPERM; everything else is allowed. A default-deny allowlist
/// is a later hardened profile. Known gap (documented, not hidden): `clone`
/// with CLONE_NEWUSER is not arg-filtered in v1 — `unshare` is denied, and
/// no_new_privs + Landlock still bound what a fresh namespace could reach.
#[cfg(target_os = "linux")]
fn seccomp_deny_program() -> Result<seccompiler::BpfProgram, H5iError> {
    use seccompiler::{SeccompAction, SeccompFilter, SeccompRule, TargetArch};

    let denied = denied_syscalls();
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

    let (exit_code, timed_out, cpu_ms, max_rss_kb) = wait_loop(&mut child, Some(wall));

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
///
/// `wall = None` disables the deadline (interactive sessions are bounded by
/// the operator, not a timer — and, having skipped `setsid`, they have no
/// dedicated process group to `killpg`).
#[cfg(unix)]
pub(crate) fn wait_loop(
    child: &mut std::process::Child,
    wall: Option<Duration>,
) -> (Option<i32>, bool, u128, Option<i64>) {
    // The child called setsid(), so its process-group id equals its pid; a
    // negative-pid SIGKILL reaps the whole tree, not just the leader.
    let pid = child.id() as libc::pid_t;
    let deadline = wall.map(|w| std::time::Instant::now() + w);
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
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
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
    wall: Option<Duration>,
) -> (Option<i32>, bool, u128, Option<i64>) {
    let deadline = wall.map(|w| std::time::Instant::now() + w);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
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

    #[cfg(target_os = "linux")]
    #[test]
    fn config_lock_paths_picks_existing_project_dirs_and_home_files() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let home = dir.path().join("home");
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        // Project scope: the .claude DIR exists; .codex does not.
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        // User scope: the settings FILE exists; codex config.toml exists.
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(home.join(".codex/config.toml"), "").unwrap();

        let locks = config_lock_paths(&work, Some(&home));
        // Project: the .claude directory itself (not the file under it).
        assert!(locks.contains(&work.join(".claude")), "project .claude dir locked: {locks:?}");
        assert!(!locks.contains(&work.join(".codex")), "absent project .codex not locked");
        // User: the single settings file (NOT the whole ~/.claude dir).
        assert!(locks.contains(&home.join(".claude/settings.json")), "home claude settings locked");
        assert!(!locks.contains(&home.join(".claude")), "home .claude dir must stay writable");
        assert!(locks.contains(&home.join(".codex/config.toml")), "home codex config locked");

        // No HOME → only project-scope locks.
        let locks = config_lock_paths(&work, None);
        assert_eq!(locks, vec![work.join(".claude")]);
    }

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
    fn private_paths_parse_with_kind_and_persist_defaults() {
        let toml_text = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"target" = { kind = "cache" }
".next" = { kind = "scratch", persist = false }
"build" = { }
"#;
        let p = load_from_str(toml_text, "dev", None).unwrap();
        // Deterministic (sorted) order for a stable digest.
        let by: std::collections::HashMap<_, _> =
            p.private_paths.iter().map(|pp| (pp.path.as_str(), pp)).collect();
        // cache defaults persist=true; scratch overrides to false; bare entry
        // defaults to cache+persist.
        assert_eq!(by["target"].kind, "cache");
        assert!(by["target"].persist);
        assert_eq!(by[".next"].kind, "scratch");
        assert!(!by[".next"].persist);
        assert!(by["build"].persist, "bare entry defaults to a persisted cache");
    }

    #[test]
    fn private_paths_reject_unsafe_and_overlapping() {
        // Absolute path.
        let abs = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"/etc" = { kind = "cache" }
"#;
        assert!(load_from_str(abs, "dev", None).is_err());
        // `..` traversal.
        let dotdot = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"../escape" = { kind = "cache" }
"#;
        assert!(load_from_str(dotdot, "dev", None).is_err());
        // Unknown kind (shared is explicitly unsupported in v1).
        let shared = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"db" = { kind = "shared" }
"#;
        assert!(load_from_str(shared, "dev", None).is_err());
        // Overlapping (parent would shadow nested child).
        let overlap = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"a" = { kind = "cache" }
"a/b" = { kind = "cache" }
"#;
        assert!(load_from_str(overlap, "dev", None).is_err());
        // Comma (unsupported by the container mount syntax) — fail closed at
        // load, not silently skipped later.
        let comma = r#"
[profile.dev]
isolation = "process"
[profile.dev.private_paths]
"a,b" = { kind = "cache" }
"#;
        assert!(load_from_str(comma, "dev", None).is_err());
    }

    #[test]
    fn empty_private_paths_keeps_policy_digest_stable() {
        // A profile that declares no private paths must serialize/digest exactly
        // as before the field existed (skip_serializing_if = empty).
        use crate::sandbox_policy::ResolvedPolicy;
        let p = load_from_str(doc_example_toml(), "default", None).unwrap();
        assert!(p.private_paths.is_empty());
        let toml = ResolvedPolicy::new(p.isolation, p).to_toml().unwrap();
        assert!(
            !toml.contains("private_paths"),
            "empty private_paths must not appear in the serialized policy"
        );
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
        let ra = ResolvedPolicy::new(a.isolation, a);
        let rb = ResolvedPolicy::new(b.isolation, b);
        assert_ne!(ra.digest().unwrap(), rb.digest().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn seccomp_deny_program_builds() {
        // The program compiles on this arch.
        assert!(seccomp_deny_program().is_ok());
    }

    /// The deny-list is a security *contract*: removing any of these syscalls
    /// silently widens the sandbox. This asserts membership directly (no kernel
    /// needed), so dropping e.g. `SYS_mount` or `SYS_ptrace` fails the build —
    /// the weak old test only checked the program compiled and that libc still
    /// exported the constants, which would NOT catch a deletion from the list.
    #[cfg(target_os = "linux")]
    #[test]
    fn seccomp_deny_list_covers_security_critical_syscalls() {
        let denied = denied_syscalls();
        let must_block: &[(&str, libc::c_long)] = &[
            // config-lockdown tamper-resistance depends on these two being denied
            ("mount", libc::SYS_mount),
            ("umount2", libc::SYS_umount2),
            // container/chroot escape
            ("pivot_root", libc::SYS_pivot_root),
            ("chroot", libc::SYS_chroot),
            // process-tracing escape vectors
            ("ptrace", libc::SYS_ptrace),
            ("process_vm_readv", libc::SYS_process_vm_readv),
            ("process_vm_writev", libc::SYS_process_vm_writev),
            // namespace entry/creation (the /proc-environ + userns-escape surface)
            ("setns", libc::SYS_setns),
            ("unshare", libc::SYS_unshare),
            // privileged kernel interfaces
            ("bpf", libc::SYS_bpf),
            ("init_module", libc::SYS_init_module),
            ("finit_module", libc::SYS_finit_module),
            // path-confinement bypass via fs handles
            ("open_by_handle_at", libc::SYS_open_by_handle_at),
            ("name_to_handle_at", libc::SYS_name_to_handle_at),
            // io_uring — large, repeatedly-exploited surface that also bypasses seccomp
            ("io_uring_setup", libc::SYS_io_uring_setup),
            ("io_uring_enter", libc::SYS_io_uring_enter),
            ("io_uring_register", libc::SYS_io_uring_register),
        ];
        for (name, nr) in must_block {
            assert!(
                denied.contains(nr),
                "seccomp deny-list no longer blocks {name} (SYS={nr}) — the sandbox was widened"
            );
        }
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
    fn builtin_passes_term_for_interactive_sessions() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        assert!(p.env_pass.iter().any(|k| k == "TERM"));
        assert!(p.env_pass.iter().any(|k| k == "COLORTERM"));
    }

    #[test]
    fn builtin_agent_profile_loads_without_policy_file() {
        // `--profile agent-claude` must work with no .h5i/env.toml, like
        // `default`. (Explicit runtime name → deterministic regardless of the
        // ambient $H5I_AGENT in the test runner.)
        let dir = tempfile::tempdir().unwrap();
        let p = load_profile(dir.path(), "agent-claude", Some(IsolationClaim::Supervised)).unwrap();
        assert_eq!(p.isolation, IsolationClaim::Supervised);
        // Narrowed binaries (not all of ~/.local) + the runtime's own share dir.
        assert!(p.fs_read.iter().any(|s| s == "~/.local/bin"));
        assert!(!p.fs_read.iter().any(|s| s == "~/.local"), "blanket ~/.local removed");
        assert!(p.fs_read.iter().any(|s| s == "~/.local/share/claude"));
        // Rustup shims under ~/.cargo/bin need read-only rustup metadata to
        // locate the active toolchain, but ~/.cargo and ~/.rustup stay ungranted.
        assert!(p.fs_read.iter().any(|s| s == "~/.cargo/bin"));
        assert!(p.fs_read.iter().any(|s| s == "~/.cargo/config"));
        assert!(p.fs_read.iter().any(|s| s == "~/.cargo/config.toml"));
        // Read-only crate caches for offline dependency resolution in-box.
        assert!(p.fs_read.iter().any(|s| s == "~/.cargo/registry"));
        assert!(p.fs_read.iter().any(|s| s == "~/.cargo/git"));
        assert!(p.fs_read.iter().any(|s| s == "~/.rustup/settings.toml"));
        assert!(p.fs_read.iter().any(|s| s == "~/.rustup/toolchains"));
        assert!(!p.fs_read.iter().any(|s| s == "~/.cargo"), "blanket ~/.cargo removed");
        // Credentials stay ungranted even though the caches are now readable.
        assert!(
            !p.fs_read.iter().any(|s| s == "~/.cargo/credentials"
                || s == "~/.cargo/credentials.toml"),
            "cargo credentials never granted"
        );
        assert!(!p.fs_write.iter().any(|s| s == "~/.cargo"), "blanket ~/.cargo write removed");
        assert!(
            !p.fs_write.iter().any(|s| s.starts_with("~/.cargo/")),
            "default agent profile does not mutate host Cargo cache"
        );
        assert!(!p.fs_read.iter().any(|s| s == "~/.rustup"), "blanket ~/.rustup removed");
        // Own state read-write; the OTHER runtime's state is NOT granted.
        assert!(p.fs_write.iter().any(|s| s == "~/.claude"));
        assert!(!p.fs_write.iter().any(|s| s == "~/.codex"), "no cross-runtime state");
        assert!(p.fs_write.iter().any(|s| s == "/tmp"));
        // Own API egress only — not OpenAI's.
        assert!(p.net_egress.iter().any(|s| s == "api.anthropic.com"));
        assert!(!p.net_egress.iter().any(|s| s == "api.openai.com"), "no cross-runtime egress");
        assert!(p.env_pass.iter().any(|k| k == "TERM"));
        assert!(p.env_pass.iter().any(|k| k == "SHELL"));
        // The default deny set survives and no grant contains a denied child
        // (validate_profile ran inside load_profile).
        assert!(p.fs_deny.iter().any(|s| s == "~/.ssh"));
    }

    #[test]
    fn agent_profile_injects_is_sandbox() {
        // Agent-in-box profiles map the agent to root-in-userns on the egress
        // tiers, so Claude's `getuid()==0` guard would refuse
        // `--dangerously-skip-permissions`. `IS_SANDBOX=1` is injected to skip
        // only that check (no new capability) — for every agent profile/runtime.
        for name in ["agent", "agent-claude", "agent-codex"] {
            let p = Profile::builtin(name, IsolationClaim::Supervised);
            let policy = ResolvedPolicy::new(p.isolation, p);
            let env = augment_injected_env(&policy, &[]);
            assert!(
                env.iter().any(|(k, v)| k == "IS_SANDBOX" && v == "1"),
                "{name}: IS_SANDBOX=1 must be injected"
            );
        }
    }

    #[test]
    fn non_agent_profile_does_not_inject_is_sandbox() {
        // Ordinary confined runs (build/test) stay non-root and must not get a
        // sandbox signal they don't need.
        let p = Profile::builtin("default", IsolationClaim::Process);
        let policy = ResolvedPolicy::new(p.isolation, p);
        let env = augment_injected_env(&policy, &[]);
        assert!(
            !env.iter().any(|(k, _)| k == "IS_SANDBOX"),
            "default profile must not inject IS_SANDBOX"
        );
    }

    #[test]
    fn injected_is_sandbox_is_not_overridden() {
        // A caller-supplied / broker IS_SANDBOX wins — we only set the default,
        // and never duplicate the key.
        let p = Profile::builtin("agent-claude", IsolationClaim::Supervised);
        let policy = ResolvedPolicy::new(p.isolation, p);
        let preset = [("IS_SANDBOX".to_string(), "0".to_string())];
        let env = augment_injected_env(&policy, &preset);
        let hits: Vec<_> = env.iter().filter(|(k, _)| k == "IS_SANDBOX").collect();
        assert_eq!(hits.len(), 1, "no duplicate IS_SANDBOX");
        assert_eq!(hits[0].1, "0", "caller value preserved");
    }

    #[test]
    fn agent_codex_profile_scopes_to_codex_only() {
        // The Codex box gets Codex state + OpenAI egress, and NOT Claude's.
        let dir = tempfile::tempdir().unwrap();
        let p = load_profile(dir.path(), "agent-codex", Some(IsolationClaim::Supervised)).unwrap();
        assert!(p.fs_write.iter().any(|s| s == "~/.codex"));
        assert!(!p.fs_write.iter().any(|s| s == "~/.claude"), "no cross-runtime state");
        assert!(!p.fs_write.iter().any(|s| s == "~/.claude.json"), "no cross-runtime state");
        assert!(p.fs_read.iter().any(|s| s == "~/.local/share/codex"));
        assert!(!p.fs_read.iter().any(|s| s == "~/.local/share/claude"));
        assert!(p.net_egress.iter().any(|s| s == "api.openai.com"));
        assert!(!p.net_egress.iter().any(|s| s == "api.anthropic.com"), "no cross-runtime egress");
    }

    #[test]
    fn agent_runtime_from_identity_maps_codex_else_claude() {
        assert_eq!(AgentRuntime::from_identity("codex"), AgentRuntime::Codex);
        assert_eq!(AgentRuntime::from_identity("Codex-2"), AgentRuntime::Codex);
        assert_eq!(AgentRuntime::from_identity("claude"), AgentRuntime::Claude);
        // Unknown identities default to Claude (never silent OpenAI egress).
        assert_eq!(AgentRuntime::from_identity("some-bot"), AgentRuntime::Claude);
        assert_eq!(AgentRuntime::from_identity(""), AgentRuntime::Claude);
    }

    #[test]
    fn agent_profile_refuses_tiers_that_cannot_enforce_egress() {
        // Fail-closed: the agent profile carries net.egress, which the static
        // process tier (and below) cannot enforce — refuse, never weaken.
        let dir = tempfile::tempdir().unwrap();
        for tier in [IsolationClaim::Workspace, IsolationClaim::Process] {
            let err = load_profile(dir.path(), "agent", Some(tier)).unwrap_err();
            assert!(err.to_string().contains("net.egress"), "{tier:?}: {err}");
        }
    }

    #[test]
    fn user_defined_agent_profile_merges_over_agent_builtin() {
        // A partial [profile.agent-claude] keeps the agent-in-box grants as its
        // base. (net.egress is NOT inherited: a user profile owns its egress.)
        let toml_text = r#"
[profile.agent-claude]
isolation = "supervised"
resources = { mem = "2G" }
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert_eq!(p.mem_bytes, Some(2 * 1024 * 1024 * 1024));
        assert!(p.fs_read.iter().any(|s| s == "~/.local/bin"), "agent base grants inherited");
        assert!(p.fs_write.iter().any(|s| s == "~/.claude"));
        assert!(p.net_egress.is_empty(), "egress is owned by the user profile");
    }

    #[test]
    fn isolation_override_wins_over_profile() {
        let p = load_from_str(doc_example_toml(), "default", Some(IsolationClaim::Workspace)).unwrap();
        assert_eq!(p.isolation, IsolationClaim::Workspace);
    }

    /// Temp repo workdir, optionally carrying a `.h5i/env.toml`.
    fn tmp_repo(toml_text: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(t) = toml_text {
            std::fs::create_dir_all(dir.path().join(".h5i")).unwrap();
            std::fs::write(dir.path().join(POLICY_FILE), t).unwrap();
        }
        dir
    }

    #[test]
    fn profile_declared_isolation_reads_the_field() {
        let dir = tmp_repo(Some("[profile.default]\nisolation = \"process\"\n"));
        assert_eq!(
            profile_declared_isolation(dir.path(), "default").unwrap(),
            Some(IsolationClaim::Process)
        );
        // `auto` is a strategy, not a declared tier → None (defer to the picker).
        let dir = tmp_repo(Some("[profile.default]\nisolation = \"auto\"\n"));
        assert_eq!(profile_declared_isolation(dir.path(), "default").unwrap(), None);
        // No isolation key → None.
        let dir = tmp_repo(Some("[profile.default]\ntools = [\"git\"]\n"));
        assert_eq!(profile_declared_isolation(dir.path(), "default").unwrap(), None);
        // No file at all → None (no error).
        let dir = tmp_repo(None);
        assert_eq!(profile_declared_isolation(dir.path(), "default").unwrap(), None);
    }

    #[test]
    fn effective_auto_honors_a_declared_tier_without_probing() {
        // A profile that explicitly declares `workspace` must resolve to exactly
        // that under the default (non-forced) path — deterministic, no host probe.
        let dir = tmp_repo(Some("[profile.default]\nisolation = \"workspace\"\n"));
        assert_eq!(
            effective_auto(dir.path(), "default", false).unwrap(),
            IsolationClaim::Workspace
        );
    }

    #[test]
    fn effective_auto_never_picks_an_unrunnable_tier() {
        // The core invariant of secure-by-default: whatever auto picks (host
        // dependent) MUST pass the very checks `create` applies — so a default
        // env never fails at run time. Forced probe, no declared tier.
        let dir = tmp_repo(None);
        let tier = effective_auto(dir.path(), "default", true).unwrap();
        // Workspace is always runnable; any stronger pick must verify-exec clean.
        if tier != IsolationClaim::Workspace {
            let p = load_profile(dir.path(), "default", Some(tier)).unwrap();
            let pol = resolve(&p, &probe_host()).expect("auto-picked tier must resolve");
            verify_exec(&pol).expect("auto-picked tier must verify-exec");
        }
        // And it is never weaker than workspace is meaningless — just assert it's
        // a real claim (the match is exhaustive, so reaching here means it's one).
        let _ = tier;
    }

    #[test]
    fn effective_auto_skips_container_without_an_image() {
        // The bare default has no container image, so auto must NOT pick
        // `container` (resolve refuses imageless container) — it lands on a
        // kernel tier or workspace instead.
        let dir = tmp_repo(None);
        let tier = effective_auto(dir.path(), "default", true).unwrap();
        assert_ne!(tier, IsolationClaim::Container, "imageless default can't be container");
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
    fn supervised_builtin_is_confined_and_net_deny() {
        // The supervised tier ranks above Process, so its built-in profile is
        // fully confined: net.mode=deny (so v1 supervised runs work airtight),
        // $WORK writable, no secrets/egress by default.
        let p = Profile::builtin("p", IsolationClaim::Supervised);
        assert_eq!(p.net_mode, NetMode::Deny);
        // $WORK plus the write-granted sinks (/dev/null, /dev/zero) — no other
        // host paths are writable.
        assert_eq!(p.fs_write, vec!["$WORK", "/dev/null", "/dev/zero"]);
        assert!(p.net_egress.is_empty());
        assert!(p.secret_grants.is_empty());
        // Supervised must rank above Process so the net.egress preflight lint
        // (which refuses egress at <= Process) doesn't reject a supervised egress.
        assert!(IsolationClaim::Supervised > IsolationClaim::Process);
        assert!(IsolationClaim::Supervised < IsolationClaim::Container);
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
        let r1 = ResolvedPolicy::new(p1.isolation, p1);
        let r2 = ResolvedPolicy::new(p2.isolation, p2);
        assert_eq!(r1.digest().unwrap(), r2.digest().unwrap());

        let mut p3 = r1.profile.clone();
        p3.net_mode = NetMode::Host;
        let r3 = ResolvedPolicy::new(p3.isolation, p3);
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

        // Neither image nor runtime → the static config error (image) takes
        // precedence over the host-capability error (podman), so the message is
        // host-independent: a box / CI without podman still gets the actionable
        // "set container.image" message, not a podman-not-found one.
        let err = resolve(&no_img, &caps_with_container(None)).unwrap_err();
        assert!(err.to_string().contains("image"), "{err}");
        assert!(!err.to_string().contains("podman"), "image error must win: {err}");

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
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
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
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
        let out = run(&policy, dir.path(), &["sleep".to_string(), "30".to_string()]).unwrap();
        assert!(out.timed_out, "expected the wall-clock kill to fire");
        assert_ne!(out.exit_code, Some(0));
    }

    #[test]
    fn run_records_resource_usage() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
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
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
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
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
        assert!(run(&policy, dir.path(), &["true".into()]).is_ok());
    }
}
