//! h5i's own confinement for the `process` isolation tier, plus the policy model shared by
//! every tier (docs/environments-design.md §5–§7).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
// Used on every target now: the private-path and scratch-dir helpers below
// return PathBuf regardless of which confinement backend (if any) this target
// has.
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::error::H5iError;

// The pure policy *vocabulary* (types with no machinery deps) lives in the
// dependency-leaf `sandbox_policy` module. Re-exported so `crate::sandbox::X`
// keeps resolving for callers that also use the confinement machinery here,
// and so the names are in scope throughout this module. The container backend
// imports them from `sandbox_policy` directly, breaking the
// `sandbox → container → sandbox` dispatch cycle.
pub use crate::sandbox_policy::{
    agent_browser_binary, browser_light_binary, browser_read_grants, browser_tooling_present,
    chrome_binary, chrome_exec_patterns, engine_tooling_missing, AgentRuntime, AuditCapture,
    BrowserEngine, BROWSER_ENGINES,
    AuditPolicy, BackgroundHandle,
    BoxGitPath, ExecOutcome, HomeBind, InteractiveOutcome, IsolationClaim, NetMode, PrivateBind,
    PrivatePath, Profile, ResolvedPolicy, RoBind, SecretGrant, DEFAULT_WALL,
};

/// The box env var naming the host-side egress proxy. Re-exported here because
/// the in-box tooling that reads it (the browser shim, generated in core) is
/// written against `sandbox`, while the proxy that sets it lives in
/// `container`.
pub use crate::container::EGRESS_PROXY_VAR;

/// Repo-relative path of the checked-in policy file.
pub const POLICY_FILE: &str = ".h5i/env.toml";

// ─── policy vocabulary → moved to src/sandbox_policy.rs ──────────────────────
// `IsolationClaim`, `NetMode`, `Profile`, `SecretGrant`, `AgentRuntime`,
// `BoxGitPath`, `AuditCapture`, `AuditPolicy`, `ResolvedPolicy`, `ExecOutcome`
// and `DEFAULT_WALL` are re-exported above. The machinery that *operates* on
// them (resolve/validate/probe/run/confinement) stays here.

// `Profile` (impl builtin/builtin_agent/wall) and the
// `default_fs_read`/`default_fs_deny` helpers moved to src/sandbox_policy.rs.

/// Agent config paths whose mutation could disable the in-box observation hook, locked
/// *read-only* (bind + remount,ro) inside the box's mount namespace for interactive agent
/// sessions.
#[cfg(unix)]
pub(crate) fn config_lock_paths(work: &Path, home: Option<&Path>) -> Vec<PathBuf> {
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

/// Apply the private `/tmp` bind last. Its backing and the agent HOME-state
/// copies can live below a repository in `/tmp`; mounting it first hides those
/// sources before their own binds run.
#[cfg(target_os = "linux")]
pub(crate) fn home_binds_in_mount_order(binds: &[HomeBind]) -> Vec<&HomeBind> {
    let mut ordered: Vec<_> = binds.iter().collect();
    ordered.sort_by_key(|bind| bind.target == Path::new("/tmp"));
    ordered
}

// ── raw TOML schema (what users write; everything optional) ────────────────

#[derive(Debug, Default, Deserialize)]
struct PolicyFileToml {
    #[serde(default)]
    profile: BTreeMap<String, ProfileToml>,
    /// Repo-level `[container] image = "…"`: the default base image for every
    /// profile that doesn't declare its own `container.image`, so one line
    /// makes the built-in agent profiles (which can't know your image) usable
    /// at the container tier. Declaring it also makes `container` a candidate
    /// for the isolation auto-pick (strongest-runnable).
    #[serde(default)]
    container: ContainerToml,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    isolation: Option<String>,
    #[serde(default)]
    fs: FsToml,
    #[serde(default)]
    net: NetToml,
    #[serde(default)]
    secrets: Vec<String>,
    /// Rich per-grant config: `[profile.X.secret.NAME] source=… inject=…
    /// ttl=…`.
    #[serde(default)]
    secret: BTreeMap<String, SecretGrantToml>,
    /// Authenticated egress: `[[profile.X.auth]] host=… credential_env=…
    /// base_url_var=…`.
    #[serde(default)]
    auth: Vec<crate::sandbox_policy::AuthGrant>,
    resources: Option<ResourcesToml>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    container: ContainerToml,
    #[serde(default)]
    env: EnvVarsToml,
    #[serde(default)]
    shell: ShellToml,
    /// Persona source files, each relative to `$WORK`: their contents are
    /// concatenated into `PERSONA.md` at `env create`. `[profile.X] persona =
    /// ["plugin/persona/architect.md", "plugin/persona/careful.md"]`.
    #[serde(default)]
    persona: Vec<String>,
    /// Per-env private paths (Idea 3): `[profile.X.private_paths] "target" = {
    /// kind = "cache", persist = true }`.
    #[serde(default)]
    private_paths: BTreeMap<String, PrivatePathToml>,
    /// Opt-in for the secrets broker's host-side `command:` extractor.
    #[serde(default)]
    allow_command_extractors: bool,
    /// Which browser engine this profile runs: `[profile.browser] engine =
    /// "h5i-light"`. Spellable because it is a decision an operator makes at
    /// create; validated fail-closed, and pinned in the digest, because
    /// changing engine changes what a page can do.
    #[serde(default)]
    engine: Option<String>,
    /// `[profile.X.browser]`: what the box may do with the browser.
    #[serde(default)]
    browser: BrowserToml,
    /// `[profile.X.detect]`: the runtime-detection lane (design-detect.md D11).
    #[serde(default)]
    detect: DetectToml,
}

/// `[profile.X.detect] enabled = true`.
///
/// Every field is `Option`, and omitting one inherits the base rather than
/// resetting it. The same rule `net.egress` and `net.unix` follow, so a
/// partial overlay can never quietly *widen* or *narrow* what it did not
/// mention.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetectToml {
    enabled: Option<bool>,
    require: Option<bool>,
    buffer_kb: Option<u32>,
    rules: Option<Vec<String>>,
}

/// `[profile.X.browser] deny = ["evaluate"]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserToml {
    #[serde(default)]
    deny: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContainerToml {
    /// Base image for `isolation=container`.
    image: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FsToml {
    // `Option`, like `net.egress`, so an author can tell h5i "nothing" and be
    // believed. Omitted (`None`) inherits the built-in base, which is what
    // makes a partial overlay usable; an explicit `read = []` means the empty
    // list. Treating `[]` as "omitted" turned a narrowing into a widening: a
    // profile written as `fs.write = []` to get a read-only box was handed
    // `$WORK`, `~/.claude`, `/tmp` and `/dev/tty` instead.
    #[serde(default)]
    read: Option<Vec<String>>,
    #[serde(default)]
    write: Option<Vec<String>>,
    #[serde(default)]
    deny: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetToml {
    mode: Option<String>,
    /// `None` (key omitted) inherits the builtin base's egress, so a partial
    /// `[profile.agent-claude]` overlay keeps its API allowlist. An explicit
    /// `egress = []` opts out (deny).
    egress: Option<Vec<String>>,
    /// `unix = true` allows `AF_UNIX` sockets past the supervised tier's
    /// `socket()` gate (see [`crate::sandbox_policy::Profile::unix_sockets`]).
    /// `None` inherits the base, so a partial overlay on `browser` keeps it.
    unix: Option<bool>,
    /// Loopback ports the box may dial, e.g. `loopback = [3000]` for a dev
    /// server it runs itself. Needed only on macOS, where loopback is the
    /// *host's* and is otherwise denied wholesale; on Linux the box has its own
    /// netns and this is redundant but harmless. Declared rather than
    /// discovered, so the grant is visible in the digested policy.
    loopback: Option<Vec<u16>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcesToml {
    mem: Option<String>,
    procs: Option<u64>,
    wall: Option<String>,
    fsize: Option<String>,
    cpu: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvVarsToml {
    /// `Option` for the same reason as [`FsToml`]: an explicit `pass = []`
    /// means an empty environment, not "inherit PATH/HOME/LANG/TERM/…".
    #[serde(default)]
    pass: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellToml {
    /// `[profile.X.shell] rcfile = ".h5i/box.bashrc"`: a custom bash rcfile for
    /// interactive `env shell`, relative to `$WORK`. Unset → generated plain
    /// rc.
    rcfile: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretGrantToml {
    source: Option<String>,
    inject: Option<String>,
    ttl: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
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
/// config; a name only in the simple list gets defaults; a rich table grants
/// its name implicitly. Deterministic order (sorted) for a stable policy
/// digest.
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
        // A browser box is an agent box plus the browser surface; it scopes to
        // the creating runtime exactly like `agent` does.
        "browser" => Profile::builtin_browser(isolation, AgentRuntime::detect()),
        _ => Profile::builtin(name, isolation),
    }
}

/// Is `name` backed by a built-in profile (usable without `.h5i/env.toml`)?
fn is_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "default" | "agent" | "agent-claude" | "agent-codex" | "browser"
    )
}

/// Is `name` an agent-in-box profile (the family that grants claude/codex HOME
/// state + API egress)? Used to decide whether a box can actually run an agent.
pub fn is_agent_profile(name: &str) -> bool {
    matches!(name, "agent" | "agent-claude" | "agent-codex" | "browser")
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
    // `file_image` is the repo-level `[container] image` default; it applies to
    // any profile (builtin or user-defined) that doesn't set its own.
    let (raw, file_image): (Option<ProfileToml>, Option<String>) = if path.is_file() {
        let text = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
        let mut file: PolicyFileToml = toml::from_str(&text)?;
        let entry = match file.profile.remove(name) {
            Some(p) => Some(p),
            None if is_builtin_name(name) => None,
            None => {
                return Err(H5iError::Metadata(format!(
                    "profile '{name}' not found in {POLICY_FILE} (available: {})",
                    file.profile.keys().cloned().collect::<Vec<_>>().join(", ")
                )))
            }
        };
        (entry, file.container.image)
    } else if !is_builtin_name(name) {
        return Err(H5iError::Metadata(format!(
            "profile '{name}' requested but {POLICY_FILE} does not exist"
        )));
    } else {
        (None, None)
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
                // Omitted → inherit the base; explicit `[]` → empty. Same rule
                // as `net.egress` below, so narrowing a profile never widens
                // it.
                fs_read: t.fs.read.unwrap_or(base.fs_read),
                fs_write: t.fs.write.unwrap_or(base.fs_write),
                fs_deny: t.fs.deny.unwrap_or(base.fs_deny),
                net_mode: match t.net.mode {
                    Some(ref s) => NetMode::parse(s)?,
                    None => base.net_mode,
                },
                // Omitted → inherit the builtin base (a partial
                // `[profile.agent-claude]` keeps its Anthropic egress instead
                // of silently bricking the agent); explicit `egress = []` opts
                // out. Note the inherited list still hits the tier lint below.
                // An agent overlay pinned to process/workspace is now *refused*
                // (fail-closed) rather than left egressless.
                net_egress: t.net.egress.unwrap_or(base.net_egress),
                secret_grants: merge_secret_grants(&t.secrets, &t.secret),
                // Omitted → inherit the built-in's (none today). An explicitly
                // declared list replaces it: authenticated egress is never
                // additive by accident.
                auth: if t.auth.is_empty() { base.auth } else { t.auth },
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
                env_pass: t.env.pass.unwrap_or(base.env_pass),
                private_paths: if t.private_paths.is_empty() {
                    base.private_paths
                } else {
                    build_private_paths(&t.private_paths)
                },
                allow_command_extractors: t.allow_command_extractors
                    || base.allow_command_extractors,
                shell_rcfile: t.shell.rcfile.or(base.shell_rcfile),
                persona: if t.persona.is_empty() { base.persona } else { t.persona },
                // Omitted → inherit the base, so a partial `[profile.browser]`
                // overlay keeps the grant its daemon cannot start without.
                // Explicit `unix = false` takes it away.
                unix_sockets: t.net.unix.unwrap_or(base.unix_sockets),
                loopback_ports: t
                    .net
                    .loopback
                    .clone()
                    .unwrap_or_else(|| base.loopback_ports.clone()),
                // Inherited from the base, never spelled in `.h5i/env.toml`:
                // it is not a policy dial an author picks, it is what the
                // `browser` base needs in order to start a browser at all.
                mach_iokit: base.mach_iokit,
                // Declared or inherited, and checked against the known action
                // vocabulary in `validate_profile` so a misspelling is refused
                // at create rather than silently denying nothing. Trimmed on
                // the way in, so what enforcement matches is what validation
                // checked: validating `entry.trim()` while storing the raw
                // string let `deny = [" evaluate "]` pass create and match no
                // action.
                browser_deny: t
                    .browser
                    .deny
                    .clone()
                    .map(|list| list.iter().map(|e| e.trim().to_string()).collect())
                    .unwrap_or_else(|| base.browser_deny.clone()),
                engine: match t.engine.as_deref() {
                    Some(name) => Some(
                        crate::sandbox_policy::BrowserEngine::parse(name)
                            .map_err(H5iError::Metadata)?,
                    ),
                    None => base.engine,
                },
                detect: crate::sandbox_policy::DetectPolicy {
                    enabled: t.detect.enabled.unwrap_or(base.detect.enabled),
                    require: t.detect.require.unwrap_or(base.detect.require),
                    buffer_kb: t.detect.buffer_kb.unwrap_or(base.detect.buffer_kb),
                    rules: t
                        .detect
                        .rules
                        .clone()
                        .map(|list| list.iter().map(|e| e.trim().to_string()).collect())
                        .unwrap_or_else(|| base.detect.rules.clone()),
                },
            }
        }
    };
    // Repo-level image default: weakest precedence (profile-level
    // `container.image` and the builtin base both win). Only consulted when
    // the profile ends up imageless, so it can never *narrow* anything.
    if profile.image.is_none() {
        profile.image = file_image;
    }
    if let Some(o) = isolation_override {
        profile.isolation = o;
    }
    validate_profile(&profile)?;
    Ok(profile)
}

/// What isolation the caller requested for `env create`: a specific claim
/// (fail-closed (refused, never downgraded, if the host can't satisfy it), or
/// `Auto`) pick the strongest tier the host can actually run.
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

/// Pick the isolation tier for `env create` when none is pinned: the *strongest* tier the host
/// can actually run for this profile (`container > supervised > process > workspace`).
pub fn effective_auto(
    repo_workdir: &Path,
    name: &str,
    force_probe: bool,
    image_override: Option<&str>,
) -> Result<IsolationClaim, H5iError> {
    if !force_probe {
        if let Some(c) = profile_declared_isolation(repo_workdir, name)? {
            return Ok(c);
        }
        // An explicit org/user default (`H5I_DEFAULT_ISOLATION`) pins the tier
        // without probing. Set it to opt a whole clone into a fixed tier.
        // `--isolation auto` (force_probe) ignores it and re-probes.
        if let Ok(v) = std::env::var("H5I_DEFAULT_ISOLATION") {
            let v = v.trim();
            if !v.is_empty() && !v.eq_ignore_ascii_case("auto") {
                return IsolationClaim::parse(v);
            }
        }
    }
    // Strongest first ([`AUTO_TIERS`]).
    for tier in AUTO_TIERS {
        let Ok(mut profile) = load_profile(repo_workdir, name, Some(tier)) else {
            continue;
        };
        if let Some(img) = image_override {
            profile.image = Some(img.to_string());
        }
        // The image-backed tiers need a declared image; without one `resolve`
        // refuses them regardless of the host, so skip the candidate before
        // paying the runtime probe that `probe_host_for` would trigger (~1s for
        // `podman info`).
        if tier.image_backed() && profile.image.is_none() {
            continue;
        }
        // Probe only what this tier consults: the image-backed tiers resolve
        // against the runtime-aware caps, every other tier against the cheap
        // kernel-only probe.
        let caps = probe_host_for(tier);
        let runnable = resolve(&profile, &caps).and_then(|pol| verify_exec(&pol)).is_ok();
        if runnable {
            return Ok(tier);
        }
    }
    Ok(IsolationClaim::Workspace)
}

/// Fail-closed policy lints (§7), rejecting *policies* before any env exists.
pub fn validate_image(image: &str) -> Result<(), H5iError> {
    let bad = |why: &str| {
        // `{:?}`, not `{}`: the value is going to a terminal and the thing
        // wrong with it may be a control character. Rust's `Debug` for `str`
        // escapes those, which is the whole requirement here, and this crate
        // sits below `h5i-core`, so `redact::sanitize_display` is not
        // reachable.
        Err(H5iError::Metadata(format!(
            "container image {image:?} {why} — an image reference is a registry path, an \
             optional `:tag` or `@sha256:…`, and nothing else (fail-closed)"
        )))
    };
    if image.is_empty() || image.len() > 512 {
        return bad("is empty or absurdly long");
    }
    if image.starts_with('-') {
        // Podman would read it as a flag, and the argument after it as the
        // image.
        return bad("starts with '-', which Podman reads as an option");
    }
    if !image
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'@' | b'+' | b'-'))
    {
        return bad("contains a character an image reference cannot hold");
    }
    Ok(())
}

/// Filesystem entries that name `~` when there is no `$HOME` to resolve it against.
fn unresolvable_tilde_entries(p: &Profile, home_set: bool) -> Vec<&String> {
    if home_set {
        return Vec::new();
    }
    p.fs_read
        .iter()
        .chain(p.fs_write.iter())
        .chain(p.fs_deny.iter())
        .filter(|s| *s == "~" || s.starts_with("~/"))
        .collect()
}

pub fn validate_profile(p: &Profile) -> Result<(), H5iError> {
    if let Some(image) = &p.image {
        validate_image(image)?;
    }
    // A deny entry that matches no action denies nothing, while the resolved
    // policy still reads as though the verb were blocked. Refuse the typo here
    // rather than let the operator discover it from an agent successfully
    // doing the thing.
    for entry in &p.browser_deny {
        crate::sandbox_policy::validate_browser_deny(entry).map_err(H5iError::Metadata)?;
    }

    // The same argument as the browser-deny lint above, for the same reason: a
    // `detect` section that is on and selects nothing watches nothing while the
    // policy reads as though it were watching. The rule *names* are the
    // collector's vocabulary and are checked there (a policy crate that knew
    // them would have to depend on the collector); what is checked here is
    // everything that can be judged without it.
    if p.detect.enabled {
        if p.detect.rules.is_empty() {
            return Err(H5iError::Metadata(format!(
                "profile '{}': [detect] is enabled with an empty `rules` list, which watches for \
                 nothing while the policy reads as though it watched. Use `rules = [\"*\"]` for \
                 everything, or drop `enabled` (fail-closed).",
                p.name
            )));
        }
        if p.detect.rules.iter().any(|r| r.is_empty()) {
            return Err(H5iError::Metadata(format!(
                "profile '{}': [detect] `rules` contains an empty entry, which selects no rule \
                 (fail-closed).",
                p.name
            )));
        }
    } else if p.detect.require {
        return Err(H5iError::Metadata(format!(
            "profile '{}': [detect] sets `require = true` with `enabled = false`. That reads as \
             \"refuse to run unwatched\" and would in fact never watch anything — set \
             `enabled = true` as well, or drop `require` (fail-closed).",
            p.name
        )));
    }

    // A profile that opts into an egress allowlist may not also re-export the
    // proxy wiring: `env.pass` is applied after it, so the host's value would
    // win and the box would route around the allowlist. Refused at load, so the
    // author is told rather than silently getting a wider box. (Harmless with
    // no `net.egress`, there is no proxy to shadow, so it is not refused
    // there.)
    if p.scopes_egress()
        && let Some(bad) = p
            .env_pass
            .iter()
            .find(|k| crate::container::is_proxy_wiring_var(k))

    {
        return Err(H5iError::Metadata(format!(
            "profile '{}': env.pass carries '{bad}', which is part of the egress proxy \
             wiring. Passing it through would replace the allowlist proxy's address with \
             the host's and let the box egress unfiltered — remove it from env.pass \
             (fail-closed).",
            p.name
        )));
    }

    // An [[auth]] grant that cannot hand the box its gate token is inert: the
    // proxy refuses every request with 403 and the agent sees a broken client
    // with no explanation. Refuse at load instead.
    for g in &p.auth {
        if g.token_var.trim().is_empty() {
            return Err(H5iError::Metadata(format!(
                "profile '{}': auth grant for '{}' has no `token_var`. The proxy gates each \
                 request on a per-run token, so the box must be given it in the variable its \
                 client already sends as a credential (e.g. token_var = \"GH_TOKEN\"). \
                 Refusing rather than running a grant that answers 403 forever (fail-closed).",
                p.name, g.host
            )));
        }
    }

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
        // A command: source executes host-side code outside the sandbox. Refuse
        // it at policy-load unless the profile explicitly opts in, so the gate
        // is pinned in the (tamper-evident) digest, not just enforced at run
        // time.
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
    // Persona sources are read from the worktree at `env create` and baked into
    // PERSONA.md. Pin them inside `$WORK` (fail-closed): no absolute paths, no
    // `..` escape. The same containment the `[shell] rcfile` gets.
    for src in &p.persona {
        let rel = Path::new(src);
        if src.is_empty() || rel.is_absolute() {
            return Err(H5iError::Metadata(format!(
                "profile '{}' persona source '{src}' must be a non-empty path relative to \
                 the worktree, not an absolute path (fail-closed)",
                p.name
            )));
        }
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(H5iError::Metadata(format!(
                "profile '{}' persona source '{src}' must not escape the worktree with '..' \
                 (fail-closed)",
                p.name
            )));
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
    // Nothing below can reason about a `~` it cannot resolve, and the lint that follows is the
    // only thing standing between a grant and a denied child inside it: Landlock has no deny
    // rules, so `fs.deny` is a preflight refusal.
    let unresolvable = unresolvable_tilde_entries(p, std::env::var_os("HOME").is_some());
    if !unresolvable.is_empty() {
        return Err(H5iError::Metadata(format!(
            "profile '{}' has '~'-relative filesystem entries {unresolvable:?} but $HOME is \
             unset, so there is nothing to resolve them against. A grant would confer nothing \
             and a deny would be silently dropped — refusing rather than enforcing something \
             other than what the profile says (fail-closed). Set HOME, or spell the paths \
             absolutely.",
            p.name
        )));
    }
    // fs.deny preflight lint: Landlock has no deny rules, so a granted parent
    // must never contain a denied child.
    // Compare on *resolved* paths, not expanded text. Landlock grants follow
    // symlinks, so a grant of `~/work-tools` on a host where that is a symlink
    // to `$HOME` really grants the whole home directory, and a textual prefix
    // check never saw `~/.ssh` underneath. Canonicalization is best-effort: a
    // path that does not exist yet falls back to the expanded text, and a
    // non-existent grant is skipped by the Landlock builder anyway.
    let resolve = |s: &str| -> String {
        let expanded = expand_tilde(s);
        std::fs::canonicalize(&expanded)
            .map(|p| p.display().to_string())
            .unwrap_or(expanded)
    };
    for grant in p.fs_read.iter().chain(p.fs_write.iter()) {
        let g = resolve(grant);
        for deny in &p.fs_deny {
            let d = resolve(deny);
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

/// How long a confined child waits for the slirp4netns uplink before failing
/// closed. The helper polls for `tap0` for 6s; this leaves room for the spawn
/// on top. Without a deadline a helper that exits without signalling wedges the
/// run forever, because the pipe's write end is held by the live `EgressNetns`
/// and the wall clock is not armed until `spawn()` returns.
#[cfg(target_os = "linux")]
pub(crate) const EGRESS_READY_TIMEOUT_MS: libc::c_int = 15_000;

/// Largest amount of one captured child stream h5i will hold in memory.
pub(crate) const MAX_CAPTURED_STREAM: usize = 64 * 1024 * 1024;

/// Drain a child's pipe to EOF, retaining at most [`MAX_CAPTURED_STREAM`]
/// bytes and saying so when there was more.
///
/// Keeps reading past the cap rather than stopping: a reader that walks away
/// leaves the child blocked on a full pipe until the wall clock reaps it,
/// turning a program that legitimately prints a lot into one that hangs.
/// Discarding the overflow bounds the host without changing what the child
/// sees.
pub(crate) fn drain_capped(mut pipe: impl std::io::Read) -> Vec<u8> {
    const MARKER: &[u8] = b"\n----- h5i: output truncated at the capture cap -----\n";
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    let mut over = false;
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < MAX_CAPTURED_STREAM {
                    let room = MAX_CAPTURED_STREAM - buf.len();
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                    if n > room {
                        over = true;
                    }
                } else {
                    over = true;
                }
            }
            Err(_) => break,
        }
    }
    if over {
        buf.extend_from_slice(MARKER);
    }
    buf
}

/// 16 hex chars of OS entropy. Enough that no other local user can guess or
/// pre-plant a scratch path before we create it.
fn random_suffix() -> Result<String, H5iError> {
    let mut raw = [0u8; 8];
    getrandom::fill(&mut raw).map_err(|e| {
        H5iError::Metadata(format!(
            "no OS entropy for a private scratch path ({e}) — refusing to fall back to a \
             guessable one (fail-closed)"
        ))
    })?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// A private scratch directory under the system temp dir.
pub fn private_scratch_dir(prefix: &str) -> Result<PathBuf, H5iError> {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", random_suffix()?));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| H5iError::with_path(e, &dir))?;
    }
    #[cfg(not(unix))]
    std::fs::DirBuilder::new()
        .create(&dir)
        .map_err(|e| H5iError::with_path(e, &dir))?;
    Ok(dir)
}

/// Create the directory chain for `rel` under `work` without ever traversing a symlink, and
/// return the joined path.
fn create_dirs_within(work: &Path, rel: &str, keep_last: bool) -> Result<PathBuf, H5iError> {
    let comps: Vec<&str> = rel
        .trim_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
        .collect();
    let stop = if keep_last {
        comps.len()
    } else {
        comps.len().saturating_sub(1)
    };
    let mut cur = work.to_path_buf();
    for (i, c) in comps.iter().enumerate() {
        cur.push(c);
        if i >= stop {
            break;
        }
        match std::fs::symlink_metadata(&cur) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(H5iError::Metadata(format!(
                    "private_paths '{rel}': '{}' is a symlink, and h5i will not follow one out \
                     of the workspace to place a private path (fail-closed). Remove it from the \
                     branch, or point the entry somewhere else.",
                    cur.display()
                )))
            }
            Ok(md) if md.is_dir() => {}
            Ok(_) => {
                return Err(H5iError::Metadata(format!(
                    "private_paths '{rel}': '{}' exists and is not a directory (fail-closed)",
                    cur.display()
                )))
            }
            Err(_) => std::fs::create_dir(&cur).map_err(|e| H5iError::with_path(e, &cur))?,
        }
    }
    Ok(cur)
}

/// Create the mountpoint for a private bind, refusing a symlinked ancestor.
pub fn create_private_mountpoint(work: &Path, rel: &str) -> Result<PathBuf, H5iError> {
    create_dirs_within(work, rel, true)
}

/// Create the *parent* directories for a private path, refusing a symlinked
/// ancestor, and return the path the caller should place there.
pub fn prepare_private_link_site(work: &Path, rel: &str) -> Result<PathBuf, H5iError> {
    create_dirs_within(work, rel, false)
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
    // No overlap: listing both `a` and `a/b` is an error. The first bind would
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
pub(crate) fn expand_tilde(path: &str) -> String {
    if (path == "~" || path.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let home = home.to_string_lossy();
        return format!("{}{}", home, &path[1..]);
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
    // `checked_mul`, because `n * mult` wraps in a release build and a cap that
    // wrapped to `0` is not a small cap: `--memory 0` is how Podman spells *no
    // limit*, while `policy.resolved.toml` and its digest record `mem_bytes = 0`
    // as though one were in force.
    num.trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .ok_or_else(|| {
            H5iError::Metadata(format!(
                "invalid resources.mem '{s}' (expected e.g. \"4G\", \"512M\")"
            ))
        })
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
    // As in [`parse_mem`]: an overflowing multiply is a refusal, never a wrap.
    num.trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            H5iError::Metadata(format!(
                "invalid resources.wall '{s}' (expected e.g. \"30m\", \"90s\")"
            ))
        })
}

// ─── capability probing (§5, mandatory) ─────────────────────────────────────

/// What this host's kernel actually supports. Probed at env creation and
/// before every confined run. Never assumed.
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
    /// macOS *Seatbelt* is present and functionally applying profiles. The
    /// kernel-tier mechanism on Darwin, where Landlock/seccomp/userns are all
    /// absent. Always `false` on Linux, so the two mechanisms are never
    /// confused for one another in a manifest or a capability report.
    #[serde(default)]
    pub seatbelt: bool,
    /// Detected rootless Podman binary for `isolation=container`; `None` when
    /// Podman is absent, broken, or rootful.
    pub container_runtime: Option<String>,
    /// Detected microVM runtime binary for `isolation=microvm` (microsandbox's
    /// `msb`); `None` when it is absent, too old, or the host cannot
    /// virtualize. Kept separate from `container_runtime` because the two
    /// answer different questions: a host can have Podman and no KVM, or KVM
    /// and no Podman.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microvm_runtime: Option<String>,
}

impl HostCaps {
    /// Does this host have *a* kernel-tier confinement mechanism, whichever one
    /// its OS provides? Callers that only need "can we confine at all" should
    /// ask this rather than testing `landlock_abi`, which is Linux's answer to
    /// a question macOS answers with Seatbelt.
    pub fn kernel_confinement(&self) -> bool {
        match self.os.as_str() {
            "linux" => self.landlock_abi.is_some() && self.seccomp,
            "macos" => self.seatbelt,
            _ => false,
        }
    }

    /// The name of the mechanism behind [`HostCaps::kernel_confinement`], for
    /// diagnostics that must not imply Landlock on a Mac.
    pub fn confinement_mechanism(&self) -> &'static str {
        match self.os.as_str() {
            "linux" => "landlock+seccomp",
            "macos" => "seatbelt",
            _ => "none",
        }
    }
}

/// Can a box push characters into the *input* queue of the terminal it shares with the operator
/// (`TIOCSTI`)?
pub fn tty_input_injection(caps: &HostCaps, claim: IsolationClaim) -> bool {
    // These tiers give the box a terminal of its own (Podman's `-t`, the
    // guest's console) rather than the operator's, so its input queue is its
    // own too.
    if matches!(
        claim,
        IsolationClaim::Container | IsolationClaim::HardenedContainer | IsolationClaim::Microvm
    ) {
        return false;
    }
    match caps.os.as_str() {
        "macos" => {
            !(caps.seatbelt
                && matches!(
                    claim,
                    IsolationClaim::Process | IsolationClaim::Supervised
                ))
        }
        "linux" => tty_injection_from_sysctl(
            std::fs::read_to_string("/proc/sys/dev/tty/legacy_tiocsti")
                .ok()
                .as_deref(),
        ),
        _ => true,
    }
}

/// The reading half of [`tty_input_injection`], split out so the fail-open rule
/// is testable on a host whose own answer we do not control (and on macOS,
/// where the file does not exist at all). `None` is an unreadable or absent
/// sysctl.
fn tty_injection_from_sysctl(v: Option<&str>) -> bool {
    match v {
        Some(s) => s.trim() != "0",
        None => true,
    }
}

/// Process-wide memoization of the host capability probe. Kernel features
/// (Landlock ABI, unprivileged userns, seccomp) and the rootless-Podman probe
/// are effectively immutable for the life of a process, yet `probe_host` is
/// called many times per `env create`/`run`: the tier auto-pick re-probes per
/// candidate and `create` resolves the policy several times. The Podman branch
/// alone spawns `podman info` (~1s+ rootless), so an uncached `env create` ran
/// ~9 probes. Never persisted, so a later process picks up a host change.
static HOST_CAPS: OnceLock<HostCaps> = OnceLock::new();
static HOST_CAPS_KERNEL: OnceLock<HostCaps> = OnceLock::new();

/// Full host probe *including* the rootless-Podman runtime. Detecting Podman
/// shells out to `podman info` (~1s on rootless), so this is reserved for paths
/// that actually consult a container tier (the container family resolve arm,
/// the `env probe`/doctor diagnostics, the MCP capability report). Memoized.
/// See [`probe_host_kernel`] for why the common kernel-tier path must avoid it.
pub fn probe_host() -> HostCaps {
    HOST_CAPS
        .get_or_init(|| {
            let mut caps = probe_host_kernel();
            caps.container_runtime = crate::container::probe().map(|r| r.bin);
            caps.microvm_runtime = crate::microvm::probe().map(|r| r.bin);
            caps
        })
        .clone()
}

/// Uncached full probe. The *diagnostic* path.
///
/// Bypasses both the in-process memo and the per-boot Podman probe cache, so a
/// report describes the host as it is now rather than as the first caller found
/// it. Callers that just need to resolve a policy should use [`probe_host`];
/// paying ~1s of `podman info` on every run is the reason the cache exists.
pub fn probe_host_fresh() -> HostCaps {
    let mut caps = probe_host_kernel_uncached();
    caps.container_runtime = crate::container::probe_fresh().map(|r| r.bin);
    // Both image-backed runtimes, or the report contradicts what `resolve` will
    // do: [`probe_host`] fills `microvm_runtime` in, so omitting it here made
    // `env probe`, `env capabilities` and `/api/probe` all say "microvm = none"
    // on a host that boots microVMs perfectly well.
    caps.microvm_runtime = crate::microvm::probe_fresh().map(|r| r.bin);
    caps
}

/// Kernel-only probe: Landlock/userns/seccomp, but *not* the ~1s Podman
/// shell-out (`container_runtime` and `microvm_runtime` are left `None`).
/// `resolve` only reads those inside the container/microvm arms (after the
/// image check), and `supervisor::probe` only reads the kernel bits, so every
/// non-image-backed claim can probe with this and skip `podman info` entirely.
/// Memoized separately from the full probe so the two never cross-trigger.
pub fn probe_host_kernel() -> HostCaps {
    HOST_CAPS_KERNEL.get_or_init(probe_host_kernel_uncached).clone()
}

/// Cheap "is Podman installed?" check for discoverability hints. Runs only
/// `podman --version` (~tens of ms), not the full rootless `podman info` probe
/// (~1s). Use when a hint just needs binary presence, not full container-tier
/// readiness (that's [`probe_host`]'s `container_runtime`).
pub fn podman_present() -> bool {
    crate::container::podman_present()
}

/// Cheap "is microsandbox installed?" check for discoverability hints. Runs
/// only `msb --version`, not the virtualization check. Use when a hint just
/// needs binary presence, not full microvm-tier readiness (that's
/// [`probe_host`]'s `microvm_runtime`).
pub fn msb_present() -> bool {
    crate::microvm::msb_present()
}

/// Why the `microvm` tier is unavailable on this host, naming the specific
/// missing half (no `msb`, an `msb` too old, or no virtualization). For
/// diagnostics, `env doctor`/`env probe`, that must be actionable rather than
/// merely negative.
pub fn microvm_unavailable_detail() -> String {
    crate::microvm::unavailable_detail()
}

/// Capability probe scoped to what `claim` actually needs: the image-backed
/// family gets the full (runtime-aware) probe; every other claim gets the cheap
/// kernel-only probe. This is the choke point that keeps a default supervised/
/// process `env create` from ever shelling out to `podman info`.
pub fn probe_host_for(claim: IsolationClaim) -> HostCaps {
    match claim {
        IsolationClaim::Container
        | IsolationClaim::HardenedContainer
        | IsolationClaim::Microvm => probe_host(),
        _ => probe_host_kernel(),
    }
}

/// The tiers `--isolation auto` will consider, strongest first.
/// `microvm` leads: when a host can virtualize and the profile names an image,
/// it is the strongest boundary h5i can build, and its egress allowlist is the
/// only one enforced by address rather than by proxy etiquette. Both
/// image-backed tiers are skipped without an image, so a bare default still
/// lands on the strongest *kernel* confinement rather than refusing.
const AUTO_TIERS: [IsolationClaim; 4] = [
    IsolationClaim::Microvm,
    IsolationClaim::Container,
    IsolationClaim::Supervised,
    IsolationClaim::Process,
];

/// Support for one isolation claim on this host: can its policy be *resolved*
/// (`satisfiable`), and, for the kernel tiers, does a confined command
/// actually *exec* here (`runnable`, the functional `verify_exec` self-test)?
#[derive(Debug, Clone, Serialize)]
pub struct ClaimSupport {
    pub claim: &'static str,
    pub satisfiable: bool,
    /// Functional exec self-test for the kernel tiers (`Some`); `None` for
    /// tiers not exec-tested here (container and microvm both need a profile
    /// image, so their readiness is a runtime check rather than a boot;
    /// hardened-container has no adapter in this build).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runnable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

/// Machine-readable answer to "what can h5i actually enforce here?". The
/// structured form of `h5i box probe`, so a downstream product adapts to the
/// real host instead of regex-scraping a log line. Backs `env capabilities
/// [--json]`.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesReport {
    pub os: String,
    pub landlock_abi: Option<i32>,
    pub userns: bool,
    pub seccomp: bool,
    /// macOS Seatbelt is present and functionally applying profiles.
    #[serde(default)]
    pub seatbelt: bool,
    /// Which mechanism backs the kernel tiers here: `landlock+seccomp`,
    /// `seatbelt`, or `none`. Read this rather than inferring from
    /// `landlock_abi`: a Mac confines with neither Landlock nor seccomp.
    pub mechanism: &'static str,
    /// A syscall-level deny-list is enforced (Linux seccomp-bpf). macOS has no
    /// equivalent, so the tiers there rest on the filesystem/network policy
    /// alone; a caller reasoning about untrusted *native* code needs to know.
    pub syscall_filter: bool,
    /// A *memory* cap is enforceable. Distinguished from `resource_limits`
    /// because Darwin enforces cpu/fsize/procs rlimits but no memory cap at all
    /// (see `seatbelt::RESOURCE_NOTE`).
    pub memory_limit: bool,
    pub container_runtime: Option<String>,
    /// Detected microVM runtime (microsandbox's `msb`) for `isolation=microvm`;
    /// `None` when it is absent, too old, or the host cannot virtualize.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microvm_runtime: Option<String>,
    /// A *domain allowlist* for egress can be *enforced* here. The container
    /// tier's DNS-pinned proxy enforces it at L7 and the microvm tier's
    /// netstack rules enforce it by address; the kernel tiers can deny-all but
    /// never allowlist, so this tracks whether either of those runtimes is
    /// present.
    pub egress_enforced: bool,
    /// The allowlist above is enforced *by address* (L3/L4), not by a proxy the
    /// box could decline to use. True only for the microvm tier. Distinguished
    /// from `egress_enforced` because it is exactly the difference a caller
    /// running genuinely untrusted code needs to know about: an L7 proxy stops
    /// `curl` and does not stop a raw socket.
    pub egress_enforced_l3: bool,
    /// Resource limits (mem / procs / wall / cpu) can be enforced. True when
    /// any confined tier beyond `workspace` runs here (kernel rlimits or the
    /// container runtime's cgroup limits).
    pub resource_limits: bool,
    pub claims: Vec<ClaimSupport>,
    /// The strongest tier that actually runs here (`workspace` is the floor).
    pub strongest_tier: &'static str,
}

/// Probe the host and evaluate every isolation claim, mirroring the auto-pick
/// (`resolve` + functional `verify_exec`) used by `env create`. Shells out to
/// `podman info` once (via [`probe_host`]). Reserve for diagnostic paths.
pub fn capabilities_report() -> CapabilitiesReport {
    capabilities_report_from(probe_host())
}

/// [`capabilities_report`] against a freshly-probed host, bypassing both the
/// in-process memo and the per-boot Podman cache. This is what the diagnostics
/// (`box probe`, `box capabilities`, the console's `/api/probe`) want: a report
/// that is stale is a report that misleads about what the host can enforce.
pub fn capabilities_report_fresh() -> CapabilitiesReport {
    capabilities_report_from(probe_host_fresh())
}

fn capabilities_report_from(caps: HostCaps) -> CapabilitiesReport {
    let mut claims: Vec<ClaimSupport> = Vec::new();
    let mut strongest = IsolationClaim::Workspace;

    // Kernel tiers: resolve the built-in `probe` profile, then run the
    // functional exec self-test (`verify_exec` is a no-op for every tier except
    // Process, so Workspace/Supervised are gated by `resolve` alone: exactly as
    // auto-pick).
    for claim in [
        IsolationClaim::Workspace,
        IsolationClaim::Process,
        IsolationClaim::Supervised,
    ] {
        let mut p = Profile::builtin("probe", claim);
        // Workspace applies no net confinement, so probe it with host net; the
        // confined tiers keep the built-in deny default.
        if claim == IsolationClaim::Workspace {
            p.net_mode = NetMode::Host;
        }
        let satisfiable = resolve(&p, &caps).is_ok();
        let runnable = resolve(&p, &caps)
            .and_then(|pol| verify_exec(&pol))
            .is_ok();
        if runnable && claim > strongest {
            strongest = claim;
        }
        claims.push(ClaimSupport {
            claim: claim.as_str(),
            satisfiable,
            runnable: Some(runnable),
            // Naming the mechanism matters most where the tier name is the same
            // but the guarantee is not: a supervised Mac box has a real egress
            // allowlist and no syscall filter.
            note: match (caps.os.as_str(), claim) {
                ("macos", IsolationClaim::Process) => {
                    Some("Seatbelt (no syscall filter, no memory cap)")
                }
                ("macos", IsolationClaim::Supervised) => {
                    Some("Seatbelt + host allowlist proxy (no syscall filter)")
                }
                _ => None,
            },
        });
    }

    // Container tier: gated by a rootless-Podman runtime; a concrete run also
    // needs a profile image, so it isn't exec-tested here.
    let container_ok = caps.container_runtime.is_some();
    if container_ok && IsolationClaim::Container > strongest {
        strongest = IsolationClaim::Container;
    }
    claims.push(ClaimSupport {
        claim: IsolationClaim::Container.as_str(),
        satisfiable: container_ok,
        runnable: None,
        note: Some("needs rootless Podman + profile container.image"),
    });
    claims.push(ClaimSupport {
        claim: IsolationClaim::HardenedContainer.as_str(),
        satisfiable: false,
        runnable: None,
        note: Some("external backend (not in this build)"),
    });
    // microVM tier: gated by microsandbox's `msb` *and* host virtualization;
    // like container, a concrete run also needs a profile image, so it isn't
    // exec-tested here.
    let microvm_ok = caps.microvm_runtime.is_some();
    if microvm_ok && IsolationClaim::Microvm > strongest {
        strongest = IsolationClaim::Microvm;
    }
    claims.push(ClaimSupport {
        claim: IsolationClaim::Microvm.as_str(),
        satisfiable: microvm_ok,
        runnable: None,
        // The unmet-prerequisite list belongs to a host that cannot run the
        // tier; once it can, the only thing still outstanding is the profile
        // image, and saying otherwise reads as a blocker that isn't there.
        note: Some(if microvm_ok {
            "profile needs container.image"
        } else {
            "needs microsandbox `msb` + host virtualization + profile container.image"
        }),
    });

    let confined_tier_runs = claims
        .iter()
        .any(|c| c.claim != "workspace" && c.runnable == Some(true));
    let resource_limits = container_ok || microvm_ok || confined_tier_runs;
    // Darwin has no cgroups and does not enforce RLIMIT_AS against mmap, so a
    // memory cap at the kernel tiers there would be a limit in name only. Say
    // so rather than let `resource_limits` imply it. A microVM's memory is the
    // guest's whole address space, so it is a hard cap on every host.
    let memory_limit = container_ok || microvm_ok || (caps.os != "macos" && confined_tier_runs);
    // A domain allowlist is enforced by the container tier's DNS-pinned proxy,
    // by the microvm tier's netstack rules, and, on macOS, by the supervised
    // tier, whose Seatbelt profile leaves the box no outbound route except that
    // same proxy on loopback.
    let supervised_runs = claims
        .iter()
        .any(|c| c.claim == "supervised" && c.runnable == Some(true));
    let egress_enforced = container_ok || microvm_ok || (caps.os == "macos" && supervised_runs);

    CapabilitiesReport {
        mechanism: caps.confinement_mechanism(),
        syscall_filter: caps.os == "linux" && caps.seccomp,
        os: caps.os,
        landlock_abi: caps.landlock_abi,
        userns: caps.userns,
        seccomp: caps.seccomp,
        seatbelt: caps.seatbelt,
        memory_limit,
        container_runtime: caps.container_runtime,
        microvm_runtime: caps.microvm_runtime,
        egress_enforced,
        egress_enforced_l3: microvm_ok,
        resource_limits,
        claims,
        strongest_tier: strongest.as_str(),
    }
}

/// Which of a profile's declared resource caps a claim actually applies here.
///
/// `h5i box status` prints the profile's `mem`/`procs` numbers under an
/// `enforce:` label, which is a claim about the host, not about the file the
/// numbers came from. Darwin has no cgroups, does not enforce `RLIMIT_AS`
/// against the mmap'd heap every modern runtime uses, and scopes
/// `RLIMIT_NPROC` to the whole uid, so [`crate::seatbelt`] applies neither and
/// printing them unqualified would state a property the host does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitSupport {
    /// Is `mem_bytes` a real ceiling on this host?
    pub mem: bool,
    /// Is `max_procs` a real ceiling on this host?
    pub procs: bool,
}

impl LimitSupport {
    /// True when neither cap is applied, so a caller can say so once instead of
    /// annotating each number.
    pub fn none(&self) -> bool {
        !self.mem && !self.procs
    }
}

/// See [`LimitSupport`].
pub fn limit_support(claim: IsolationClaim) -> LimitSupport {
    let both = |b| LimitSupport { mem: b, procs: b };
    match claim {
        // No confinement at all: `run_unconfined` sets a wall-clock deadline
        // and applies nothing else.
        IsolationClaim::Workspace => both(false),
        // Image-backed tiers cap both in the runtime itself: `--memory` +
        // `--pids-limit` for Podman, `--memory` + `--rlimit nproc` for `msb`,
        // where the memory figure is the guest's entire address space.
        IsolationClaim::Container | IsolationClaim::HardenedContainer | IsolationClaim::Microvm => {
            both(true)
        }
        // Kernel tiers: a per-run cgroup on Linux, and only when cgroup v2 is
        // delegated to this user; nothing usable on Darwin.
        // Asked per limit, not once for both: cgroup delegation is per
        // controller, `Delegate=memory` without `pids` is a real systemd
        // configuration, and answering `procs` with the memory probe's result
        // printed a process cap as enforced on a host that had not applied it.
        IsolationClaim::Process | IsolationClaim::Supervised => {
            if !cfg!(target_os = "linux") {
                return both(false);
            }
            let caps = crate::cgroup::probe();
            LimitSupport {
                mem: caps.usable,
                procs: caps.usable && caps.procs_enforceable,
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_host_kernel_uncached() -> HostCaps {
    HostCaps {
        os: "linux".into(),
        landlock_abi: probe_landlock_abi(),
        userns: probe_userns(),
        seccomp: probe_seccomp(),
        seatbelt: false,
        container_runtime: None,
        microvm_runtime: None,
    }
}

/// macOS: none of the Linux primitives exist, and none are faked. The kernel
/// tier rests entirely on Seatbelt, whose probe is functional (it runs a
/// command under a deny-default profile) rather than a feature bit.
#[cfg(target_os = "macos")]
fn probe_host_kernel_uncached() -> HostCaps {
    HostCaps {
        os: "macos".into(),
        landlock_abi: None,
        userns: false,
        seccomp: false,
        seatbelt: crate::seatbelt::probe().usable(),
        container_runtime: None,
        microvm_runtime: None,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_host_kernel_uncached() -> HostCaps {
    HostCaps {
        os: std::env::consts::OS.to_string(),
        landlock_abi: None,
        userns: false,
        seccomp: false,
        seatbelt: false,
        container_runtime: None,
        microvm_runtime: None,
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

// `AuditCapture`, `AuditPolicy`, `ResolvedPolicy` moved to
// src/sandbox_policy.rs.

/// Why Seatbelt is unusable, for the macOS refusal message.
///
/// `resolve` is platform-independent code that branches on `caps.os` at
/// *runtime*, so it is compiled for every target, including ones where the
/// `seatbelt` module does not exist (it is `cfg(unix)`, since it needs
/// `std::os::unix`). This wrapper is what keeps that runtime branch from
/// becoming a compile-time dependency on a Unix-only module.
#[cfg(unix)]
fn seatbelt_detail() -> Option<String> {
    crate::seatbelt::probe().detail
}

#[cfg(not(unix))]
fn seatbelt_detail() -> Option<String> {
    None
}

/// Resolve `profile` against what `caps` says the host supports. Refuses,
/// never silently downgrades, when the requested minimum claim cannot be
/// satisfied (§5 "Capability probing + fail-closed").
pub fn resolve(profile: &Profile, caps: &HostCaps) -> Result<ResolvedPolicy, H5iError> {
    validate_profile(profile)?;
    match profile.isolation {
        IsolationClaim::Workspace => {}
        IsolationClaim::Process => {
            let mut missing: Vec<String> = Vec::new();
            if caps.os == "macos" {
                // Darwin's kernel tier is Seatbelt. It is a different mechanism
                // with a different residual set (no syscall filter, no memory
                // cap: see `seatbelt::RESOURCE_NOTE`), which `env probe`
                // reports; what it must not be is a silent downgrade, so an
                // unusable Seatbelt refuses here exactly as a missing Landlock
                // does on Linux.
                if !caps.seatbelt {
                    let detail =
                        seatbelt_detail().unwrap_or_else(|| "Seatbelt unavailable".into());
                    missing.push(format!("macOS Seatbelt is not usable: {detail}"));
                }
                // `net.mode = deny` needs no namespace here: the profile simply
                // grants no outbound rule, which the kernel enforces.
            } else if caps.os != "linux" {
                missing.push(format!(
                    "isolation=process needs Linux (Landlock+seccomp) or macOS (Seatbelt); \
                     this host is {}",
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
            // the runtime. Fail closed, never silently downgrade. Validate the
            // declared config (image) BEFORE probing host capability (podman):
            // a missing image is a static profile error, true regardless of the
            // host, so reporting it first keeps the error host-independent. A
            // box (or CI) without podman still gets the actionable
            // "set container.image" message rather than a podman-not-found one.
            if profile.image.is_none() {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'container' requires a base image — pass `--image <img>`, \
                     set a repo default `[container] image = \"…\"` in .h5i/env.toml, or set \
                     `container.image` in profile '{}' (e.g. your toolchain image)",
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
        IsolationClaim::Microvm => {
            // Same ordering rule as the container arm: validate the declared
            // config (image) BEFORE probing host capability, so the error a box
            // or a CI runner sees is the host-independent one it can act on.
            if profile.image.is_none() {
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'microvm' requires a base image — pass `--image <img>`, \
                     set a repo default `[container] image = \"…\"` in .h5i/env.toml, or set \
                     `container.image` in profile '{}' (the microvm tier boots the same OCI \
                     images the container tier runs)",
                    profile.name
                )));
            }
            if caps.microvm_runtime.is_none() {
                // Say which half is missing. "Install microsandbox" and "enable
                // nested virtualization in your hypervisor" are very different
                // afternoons, and a tier that refuses without saying which one
                // is a tier nobody can adopt.
                let detail = crate::microvm::unavailable_detail();
                return Err(H5iError::Metadata(format!(
                    "isolation claim 'microvm' cannot be satisfied on this host — refusing \
                     (h5i never silently downgrades):\n  - {detail}\nRe-request a weaker claim \
                     (--isolation container|supervised|process), or run on a host with \
                     virtualization enabled."
                )));
            }
            // Reject an untranslatable allowlist here, at policy-resolve time,
            // rather than at first run: a `net.egress` entry the rule grammar
            // cannot carry exactly is a policy this tier cannot enforce, and
            // the place to find that out is `env create`.
            crate::microvm::egress_rule_tokens(&profile.net_egress)?;
            // Authenticated egress (5.5) hands the box a base URL pointing at a
            // proxy on the *host's* loopback. A container reaches that through
            // a known slirp gateway; a microVM's guest has its own loopback and
            // its own per-sandbox subnet, so the same URL resolves to nothing
            // inside it. Refuse the combination rather than hand the box an
            // origin it cannot dial. A grant that silently fails to
            // authenticate is worse than one that never started.
            if !profile.auth.is_empty() {
                return Err(H5iError::Metadata(format!(
                    "profile '{}' declares authenticated-egress grants ({}), which the microvm \
                     tier cannot route yet: the credential proxy listens on the host's loopback \
                     and a microVM has its own. Use --isolation container|supervised for this \
                     profile, or drop the `[[profile.{}.auth]]` grants (fail-closed).",
                    profile.name,
                    profile
                        .auth
                        .iter()
                        .map(|g| g.host.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    profile.name
                )));
            }
        }
        IsolationClaim::Supervised => {
            // The keystone safety property: refuse unless the ENTIRE mediation
            // stack probes green on this host. Never downgrade to a weaker
            // tier. An unsatisfiable supervised claim is an *impossible* claim,
            // not a degraded pass (docs/supervisor-design.md).
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
/// non-empty, the command's program (argv[0], by basename) MUST be listed.
/// Defense in depth so a profile can pin exactly which executables an
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
/// part of the policy and never serialized. They only reach the child process.
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
        IsolationClaim::Microvm => crate::microvm::run(policy, work, argv, injected_env),
        claim => Err(H5iError::Metadata(format!(
            "no backend for isolation claim '{}'",
            claim.as_str()
        ))),
    }
}

/// Spawn `argv` as a long-lived *background* process under the env's
/// confinement, with stdout+stderr redirected to `log` and stdin `/dev/null`.
/// Returns the child PID. Unlike [`run_with_env`] it does NOT wait or apply a
/// wall-clock kill. A service is operator-bounded (stopped explicitly). The
/// child gets its own session/process group so a later `killpg` reaps the whole
/// tree. v1 supports the workspace and process tiers; supervised/container
/// services are a documented follow-up (Idea 3.5).
pub fn spawn_background(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    log: &Path,
    service: &str,
) -> Result<BackgroundHandle, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    check_tool_allowlist(policy, argv)?;
    let injected = augment_injected_env(policy, injected_env);
    let injected_env = injected.as_slice();

    // The microVM tier runs the service inside the box's warm guest, so it
    // neither wants a host log fd (the guest cannot write one) nor produces a
    // host pid. Handled before the fd is opened for that reason.
    if policy.claim == IsolationClaim::Microvm {
        let h = crate::microvm::spawn_background(policy, work, argv, injected_env, service)?;
        return Ok(BackgroundHandle {
            pid: h.pid,
            sandbox: Some(h.sandbox),
            boot: Some(h.boot),
        });
    }

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
            apply_env_allowlist(&mut cmd, &policy.profile, injected_env);
            // Own session so a later killpg(pid) reaps the whole descendant
            // tree.
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
            Ok(BackgroundHandle::host(child.id()))
        }
        IsolationClaim::Process => spawn_background_confined(policy, work, argv, injected_env, out, err)
            .map(BackgroundHandle::host),
        claim => Err(H5iError::Metadata(format!(
            "services are not supported at isolation '{}' — use workspace, process, or microvm",
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

/// macOS process-tier background spawn. `sandbox-exec` `execve`s the workload
/// rather than forking it, so the returned pid *is* the service and stays
/// `killpg`-able. The same guarantee the Linux no-pidns path gives.
#[cfg(target_os = "macos")]
fn spawn_background_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    out: std::fs::File,
    err: std::fs::File,
) -> Result<u32, H5iError> {
    crate::seatbelt::spawn_background(
        policy,
        work,
        argv,
        injected_env,
        out,
        err,
        &seatbelt_opts(false, &[]),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_background_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
    _out: std::fs::File,
    _err: std::fs::File,
) -> Result<u32, H5iError> {
    Err(H5iError::Metadata(
        "process-tier services require Linux or macOS".into(),
    ))
}

/// The *agent-in-box* entry point: run `argv` (a shell or a coding agent)
/// interactively under the env's confinement. stdio is *inherited* (a real
/// session, not captured), nothing is recorded per-command, and the child's
/// exit code is returned. Confinement comes from the box itself, so whatever
/// the agent spawns inside is contained by construction. The enforcement no
/// longer depends on the agent choosing to wrap each command.
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    // Pre-generated managed-settings.json content (the wrap-bash observation
    // hook), built host-side by `hooks` and injected at the container tier.
    // Threaded through so this crate never depends on core to generate it.
    managed_settings_content: Option<&str>,
) -> Result<InteractiveOutcome, H5iError> {
    if argv.is_empty() {
        return Err(H5iError::Metadata("empty command".into()));
    }
    check_tool_allowlist(policy, argv)?;
    let injected = augment_injected_env(policy, injected_env);
    let injected_env = injected.as_slice();
    match policy.claim {
        IsolationClaim::Workspace => interactive_unconfined(policy, work, argv, injected_env)
            .map(InteractiveOutcome::from_code),
        IsolationClaim::Process => {
            interactive_confined(policy, work, argv, injected_env)
                .map(InteractiveOutcome::from_code)
        }
        IsolationClaim::Supervised => {
            crate::supervisor::run_interactive(
                policy,
                work,
                argv,
                injected_env,
                managed_settings_content,
            )
            .map(InteractiveOutcome::from_code)
        }
        IsolationClaim::Container => {
            crate::container::run_interactive(
                policy,
                work,
                argv,
                injected_env,
                managed_settings_content,
            )
        }
        IsolationClaim::Microvm => {
            crate::microvm::run_interactive(
                policy,
                work,
                argv,
                injected_env,
                managed_settings_content,
            )
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
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(work);
    apply_env_allowlist(&mut cmd, &policy.profile, injected_env);
    let status = cmd
        .status()
        .map_err(|e| H5iError::Metadata(format!("failed to start '{}': {e}", argv[0])))?;
    Ok(status.code().unwrap_or(130))
}

/// Interactive process tier: the shared confinement (Landlock + seccomp + ns +
/// rlimits + cgroup) with stdio inherited. The profile's wall-clock is *not*
/// applied. An interactive session is bounded by the operator, not a timer.
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
    // private procfs too (pidns=true), with the supervisor joining it to
    // cgroup.
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

/// macOS interactive process tier. `interactive = true` adds the agent-config
/// write lockdown and the pty grants, and keeps the caller's session so job
/// control works. The same two adjustments the Linux path makes.
#[cfg(target_os = "macos")]
fn interactive_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    crate::seatbelt::run_interactive(policy, work, argv, injected_env, &seatbelt_opts(true, &[]))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn interactive_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    Err(H5iError::Metadata(
        "isolation=process requires Linux or macOS".into(),
    ))
}

/// Apply the secrets broker's injected env vars to a child command (used by
/// each tier). Applied after `env.pass`, so a grant can't be shadowed by a host
/// var.
fn apply_injected_env(cmd: &mut std::process::Command, injected_env: &[(String, String)]) {
    for (k, v) in injected_env {
        cmd.env(k, v);
    }
}

/// Give `cmd` exactly the environment the profile allows: nothing inherited wholesale, the
/// `env.pass` names forwarded from the host, then the brokered secrets layered on top so a
/// grant is never shadowed by a passed-through host var.
fn apply_env_allowlist(
    cmd: &mut std::process::Command,
    profile: &Profile,
    injected_env: &[(String, String)],
) {
    cmd.env_clear();
    for key in &profile.env_pass {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    apply_injected_env(cmd, injected_env);
}

/// For an *agent-in-box* profile, signal Claude Code that uid 0 inside the box
/// is a sandbox artifact, not real root, so `--dangerously-skip-permissions`
/// works. The egress tiers map the agent to root-*in-userns* (it needs
/// `CAP_NET_ADMIN` to survive `execve` for `nft`), and Claude's guard refuses
/// the flag on a bare `getuid()==0`. `IS_SANDBOX=1` skips only that root check
/// and grants *no* new capability: the box already pins the agent to our real
/// unprivileged host uid. Scoped to agent profiles, and a caller-supplied or
/// brokered `IS_SANDBOX` wins; we only set the default.
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

/// Monotonic counter so concurrent runs get distinct per-run cgroup names.
/// cgroups are Linux-only, and since the exec probe stopped using this for its
/// scratch path the cgroup builder is its sole consumer.
#[cfg(target_os = "linux")]
static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static VERIFIED_EXEC_POLICIES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Functionally verify the resolved policy can actually *execute* a command on this host.
pub fn verify_exec(policy: &ResolvedPolicy) -> Result<(), H5iError> {
    if policy.claim != IsolationClaim::Process {
        return Ok(());
    }
    let cache_key = policy.digest().ok();
    if let Some(key) = cache_key.as_deref() {
        let cache = VERIFIED_EXEC_POLICIES.get_or_init(|| Mutex::new(HashSet::new()));
        if cache.lock().map(|c| c.contains(key)).unwrap_or(false) {
            return Ok(());
        }
    }
    let dir = private_scratch_dir("h5i-exec-probe")?;
    // Clone the profile but clear the tools allowlist so our internal probe
    // command isn't rejected by a user-pinned list that omits `true`.
    let mut profile = policy.profile.clone();
    profile.tools.clear();
    let probe = ResolvedPolicy::new(policy.claim, profile);
    let result = run(&probe, &dir, &["true".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(o) if o.exit_code == Some(0) => {
            if let Some(key) = cache_key {
                let cache = VERIFIED_EXEC_POLICIES.get_or_init(|| Mutex::new(HashSet::new()));
                if let Ok(mut cache) = cache.lock() {
                    cache.insert(key);
                }
            }
            Ok(())
        },
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

/// `workspace` tier: no kernel confinement (trusted), but still scoped. Runs
/// in the env worktree with the wall-clock limit applied so a hung command
/// cannot wedge `h5i box run` forever.
fn run_unconfined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(work);
    apply_env_allowlist(&mut cmd, &policy.profile, injected_env);
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

/// The child-side handles for the `supervised` egress allowlist (increment 2).
/// Given one, the child, while it still holds `CAP_NET_ADMIN`/`CAP_SYS_ADMIN`
/// in its own user namespace and *before* Landlock and seccomp lock it down,
/// pins DNS via a private `/etc/hosts` and installs the nftables default-drop
/// allowlist in its netns, after a host-side `slirp4netns` helper signals
/// readiness. Every field is built pre-fork and is `Send`; the child touches
/// them with raw syscalls only. See `supervisor::EgressNetns`.
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

/// Close every descriptor above stdio in the calling process.
#[cfg(target_os = "linux")]
unsafe fn close_inherited_fds() {
    // Safety: discharged by this function's own contract. The caller promises
    // no other user of these descriptors exists in the forked child.
    unsafe {
        // close_range(2) is Linux 5.9+; Landlock needs 5.13+, so on any host
        // that reaches this code it is present and this is the only branch that
        // runs. `syscall` is variadic, so each argument is read as a `long`.
        // Passing a 32-bit `c_uint` happens to work on x86-64 and aarch64
        // because the ABI zero-extends, but the widths should agree by
        // construction rather than by the platform being forgiving.
        if libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_long,
            libc::c_uint::MAX as libc::c_long,
            0 as libc::c_long,
        ) == 0
        {
            return;
        }
        // Fallback for a kernel (or seccomp policy) without close_range: walk
        // the descriptor table. Bounded so a huge RLIMIT_NOFILE cannot stall
        // the fork.
        let mut lim: libc::rlimit = std::mem::zeroed();
        let max = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur > 3 {
            lim.rlim_cur.min(65536) as i32
        } else {
            4096
        };
        for fd in 3..max {
            libc::close(fd);
        }
    }
}

/// Decimal-render `v` into `buf`, returning the written slice.
///
/// An allocation-free stand-in for `format!` on the post-fork path: everything
/// between `fork` and `execve` runs in a child that inherited the parent's
/// malloc state, and h5i's parents are multithreaded (the stdio drain threads,
/// the egress helper, the notify serve loop), so a heap allocation there can
/// deadlock on a lock whose owning thread does not exist in the child.
#[cfg(target_os = "linux")]
fn fmt_u32(mut v: u32, buf: &mut [u8; 24]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    &buf[i..]
}

/// `open`/`write`/`close` a small procfs control file with raw syscalls.
///
/// The allocation-free equivalent of `std::fs::write`, which converts the path
/// to a `CString` and so allocates. See [`fmt_u32`] for why that matters here.
///
/// # Safety
/// `path` must be a valid NUL-terminated C string.
#[cfg(target_os = "linux")]
unsafe fn write_proc_file(path: *const libc::c_char, bytes: &[u8]) -> Result<(), std::io::Error> {
    // Safety: discharged by this function's own contract. The caller promises
    // `path` is a valid NUL-terminated C string.
    let fd = unsafe { libc::open(path, libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Safety: `fd` is open and owned here, and the buffer is valid for `len`.
    let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    // `close` can clobber errno, so capture the write's error first.
    let err = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if n != bytes.len() as isize {
        return Err(err);
    }
    Ok(())
}

/// Build a fully-confined `std::process::Command` for `argv`: the shared confinement core used
/// by both the `process` tier ([`run_confined`]) and the `supervised` tier
/// ([`crate::supervisor::run`]), so the security-critical setup (Landlock, seccomp deny-list,
/// namespaces, rlimits, no-new-privs, uid/gid maps) has one audited implementation.
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
    // fail closed before spawning anything. This path is only used by the
    // kernel tiers, so avoid the full container-aware probe (`podman info`) on
    // the hot path.
    let caps = probe_host_for(policy.claim);
    resolve(p, &caps)?;

    // The effective configuration (design-policy.md §P1). Computed once by
    // `effective::compute_effective` and consumed below for the Landlock path
    // sets and the bind lists, so the serialized dump and the enforcement are
    // the same values by construction. The semantics live in that function:
    // change enforcement there, never beside it.
    // The effective-config layer serializes paths as UTF-8. A path this host
    // cannot represent as UTF-8 would round-trip through the dump mangled and
    // then fail closed confusingly, as a silently skipped worktree grant or a
    // bind aimed at a path that does not exist. Refuse explicitly instead.
    if work.to_str().is_none() {
        return Err(H5iError::Metadata(format!(
            "workspace path {} is not valid UTF-8 — the kernel tiers cannot \
             represent it in the effective config (fail-closed)",
            work.display()
        )));
    }
    if let Some(bad) = policy
        .private_binds
        .iter()
        .map(|b| &b.backing)
        .chain(policy.home_binds.iter().flat_map(|b| [&b.backing, &b.target]))
        .chain(policy.ro_binds.iter().flat_map(|b| [&b.backing, &b.target]))
        .chain(policy.cache_write.iter().flat_map(|b| [&b.backing, &b.target]))
        .find(|p| p.to_str().is_none())
    {
        return Err(H5iError::Metadata(format!(
            "bind path {} is not valid UTF-8 — refusing the run (fail-closed)",
            bad.display()
        )));
    }
    let abi_int = caps.landlock_abi.unwrap_or(1);
    let abi = landlock_abi_for(abi_int);
    let shape = crate::effective::RunShape {
        force_netns,
        notify: notify_sock.is_some(),
        egress: egress.is_some(),
        pidns,
        interactive,
    };
    let eff = crate::effective::compute_effective(policy, &work, abi_int, &shape);
    // The filesystem-authority gate (design-policy.md §P2), fully opt-in: it
    // does not run (no host measurement, no cost, no behavior change) unless
    // `H5I_FS_AUTHORITY_ENFORCE=1`. When on, re-check the effective config
    // against the declared policy at this single spawn chokepoint (so a run
    // cannot bypass it) and fail closed on a violation; `confined()` is an
    // invariant a legitimate config always passes.
    if crate::fs_authority::enforce_enabled() {
        let verdict = crate::effective::validate_effective(policy, &work, &eff);
        if !verdict.confined() {
            return Err(H5iError::Metadata(format!(
                "filesystem-authority validator refused the effective config \
                 (design-policy.md §P2): fs_subset={} writes_confined={} cache_readonly={} — the \
                 resolved grants are not a subset of the declared policy; refusing the run \
                 (fail-closed)",
                verdict.fs_subset, verdict.writes_confined, verdict.cache_readonly
            )));
        }
        if verdict.symlink_clean == Some(false) {
            eprintln!(
                "h5i: warning: a granted path resolves outside the worktree through a \
                 symlink (design-policy.md §P3) — the run continues, but this is a boundary signal"
            );
        }
    }
    // Persist at the apply seam, before anything is spawned: the file records
    // what this invocation is about to enforce. Fail-closed. An env that
    // asked for the record does not run without it.
    if let Some(out) = &policy.effective_out {
        eff.write_to(out)?;
    }
    let rw_paths: Vec<PathBuf> = eff.landlock.rw.iter().map(PathBuf::from).collect();
    let ro_paths: Vec<PathBuf> = eff.landlock.ro.iter().map(PathBuf::from).collect();

    let ruleset = {
        use landlock::{
            path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
            RulesetCreatedAttr,
        };
        Ruleset::default()
            // Fail closed: if the kernel can't enforce what we handle, error.
            // Never a silent partial sandbox.
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(&ro_paths, AccessFs::from_read(abi))))
            .and_then(|r| r.add_rules(path_beneath_rules(&rw_paths, AccessFs::from_all(abi))))
            .map_err(|e| H5iError::Metadata(format!("landlock ruleset construction failed: {e}")))?
    };

    // ── seccomp deny-list program (compiled pre-fork) ──
    let bpf = seccomp_deny_program()?;

    // The netns decision also comes from the effective config. One formula,
    // two readers, exactly like the path sets and bind lists above. A second
    // spelling of `net_mode == Deny || force_netns` here would be the drift
    // the apply-seam rule exists to prevent.
    let want_netns = eff.namespaces.net;
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let mem = p.mem_bytes;
    let nproc = p.max_procs;
    let fsize = p.fsize_bytes;
    let cpu = p.cpu_secs;
    // The Landlock ABI is needed again *inside* the forked child to re-grant
    // the freshly-mounted procfs (the pre-fork `/proc` grant pins the host
    // procfs inode, which the new mount shadows). Captured by value (Copy).
    let ll_abi = abi;
    // The cgroup.procs path, pre-resolved to a CString so the alloc-free
    // supervisor branch can move the workload into the cgroup.
    let cgroup_procs_c: Option<std::ffi::CString> = cgroup_procs
        .and_then(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok());

    // Bind lists, from the effective config, pre-resolved to CStrings so the
    // post-fork child does no allocation when mounting them. `eff.binds` is in
    // apply order (config-lock, private, home-state, cache-ro, cache-rw: the
    // child's steps 1d–1h) and carries the semantics each kind documents in
    // `effective.rs`; a non-empty list forces a mount namespace below.
    let bind_pairs = |kind: crate::effective::BindKind| -> Vec<(std::ffi::CString, std::ffi::CString)> {
        eff.binds
            .iter()
            .filter(|b| b.kind == kind)
            .filter_map(|b| {
                let sc = std::ffi::CString::new(b.source.as_bytes()).ok()?;
                let tc = std::ffi::CString::new(b.target.as_bytes()).ok()?;
                Some((sc, tc))
            })
            .collect()
    };
    // Config lockdown binds a path read-only over itself, so only the target is
    // kept.
    let config_lock_c: Vec<std::ffi::CString> = bind_pairs(crate::effective::BindKind::ConfigLock)
        .into_iter()
        .map(|(_, tc)| tc)
        .collect();
    let private_bind_c = bind_pairs(crate::effective::BindKind::Private);
    let home_bind_c = bind_pairs(crate::effective::BindKind::HomeState);
    let ro_bind_c = bind_pairs(crate::effective::BindKind::CacheRo);
    let cache_write_c: Option<(std::ffi::CString, std::ffi::CString)> =
        bind_pairs(crate::effective::BindKind::CacheRw).into_iter().next();

    // uid/gid map contents, rendered HERE rather than post-fork. The egress
    // path execs `nft` and so needs root-in-userns (capabilities only survive
    // execve for uid 0); every other tier keeps the 1:1 map. Both forms are
    // known pre-fork, so the child writes bytes and allocates nothing. See
    // [`fmt_u32`] for why that matters.
    let (uid_map, gid_map) = if egress.is_some() {
        (format!("0 {uid} 1"), format!("0 {gid} 1"))
    } else {
        (format!("{uid} {uid} 1"), format!("{gid} {gid} 1"))
    };

    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&work);

    // Environment allowlist, nothing inherited wholesale (§7), plus the
    // brokered secrets layered on top.
    apply_env_allowlist(&mut cmd, p, injected_env);

    let mut ruleset_slot = Some(ruleset);
    unsafe {
        cmd.pre_exec(move || {
            use std::io::Error;

            // 0.
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
                // A new mount namespace, so we can mount a private procfs over /proc without
                // touching the host.
                flags |= libc::CLONE_NEWNS;
            }
            // Config lockdown needs a private mount namespace to ro-bind in
            // (supervised is pidns=false, so it would otherwise have none). The
            // bind is contained: a mount ns under a fresh userns reduces shared
            // mounts to slave, so it never propagates to the host.
            if !config_lock_c.is_empty()
                || !private_bind_c.is_empty()
                || !home_bind_c.is_empty()
                || !ro_bind_c.is_empty()
                || cache_write_c.is_some()
            {
                flags |= libc::CLONE_NEWNS;
            }
            if libc::unshare(flags) != 0 {
                return Err(Error::last_os_error());
            }
            // The maps were rendered pre-fork (see `uid_map`/`gid_map` above);
            // raw writes here keep this path allocation-free. `setgroups=deny`
            // must land before `gid_map`, and `gid_map` before `uid_map`. The
            // kernel refuses the group map otherwise. The map points back at
            // our real host uid either way, so files created in $WORK stay
            // ours.
            write_proc_file(c"/proc/self/setgroups".as_ptr(), b"deny")?;
            write_proc_file(c"/proc/self/gid_map".as_ptr(), gid_map.as_bytes())?;
            write_proc_file(c"/proc/self/uid_map".as_ptr(), uid_map.as_bytes())?;

            // 1b. Egress allowlist (supervised increment 2). We still hold full
            //     caps in our userns and seccomp/Landlock are not yet applied, so
            //     this is the window to: tell the host helper our pid (it spawns
            //     the slirp4netns uplink for this netns), pin DNS via a private
            //     /etc/hosts, install the nftables default-drop allowlist, and
            //     wait for the uplink before continuing. Raw syscalls only, no
            //     allocation in this forked child.
            if let Some(eg) = &egress {
                use std::ptr::null;
                // (a0) A private mount namespace for the pinned /etc/hosts.
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
                // (d) Wait for slirp4netns to configure the uplink, so the program never
                //     races a not-yet-ready interface, but with a DEADLINE. A bare read
                //     hung forever whenever the helper exited without signalling (slirp
                //     failed to spawn, or tap0 never appeared): the write end lives on the
                //     parent's live `EgressNetns`, so no EOF ever arrives, and `spawn()`
                //     blocks the caller with it, before the wall clock has been armed.
                //
                //     `poll` is async-signal-safe and allocates nothing. The budget is the
                //     helper's own 6s of tap0 polling plus slack for the spawn.
                let mut pfd = libc::pollfd {
                    fd: eg.ready_read_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = libc::poll(&mut pfd, 1, EGRESS_READY_TIMEOUT_MS);
                if ready != 1 {
                    return Err(Error::other(
                        "slirp4netns uplink did not become ready in time (fail-closed)",
                    ));
                }
                let mut rb = [0u8; 1];
                if libc::read(eg.ready_read_fd, rb.as_mut_ptr().cast(), 1) != 1 {
                    return Err(Error::other("slirp4netns uplink did not become ready"));
                }
            }

            // 1c. PID-namespace jail (process tier, design §5). CLONE_NEWPID only
            //     takes effect for the *next* child, so fork: the parent becomes a
            //     thin supervisor mirroring the workload's fate, the child is PID 1 of
            //     the new namespace. A private procfs is mounted so the workload
            //     cannot read /proc/<pid>/environ of host processes, notably this h5i
            //     process, which holds the operator's environment and would defeat the
            //     env.pass allowlist. Raw syscalls plus one File::open only.
            if pidns {
                // Claim the PID namespace now, after the egress helper has come
                // and gone (see the CLONE_NEWNS note in step 1), and
                // immediately before the fork that becomes its init.
                if libc::unshare(libc::CLONE_NEWPID) != 0 {
                    return Err(Error::last_os_error());
                }
                let kid = libc::fork();
                if kid > 0 {
                    // Supervisor. Drop every inherited descriptor FIRST:
                    // holding std's spawn-status pipe would keep `spawn()`
                    // blocked until the workload exits, deadlocking any command
                    // that fills the stdout pipe and disarming the wall clock.
                    // See `close_inherited_fds`. Nothing below needs an
                    // inherited fd (the cgroup handle is opened fresh).
                    close_inherited_fds();
                    // Move the *workload* into the run cgroup (so memory.max +
                    // accounting bind it, not us: it was forked before the
                    // host-side cgroup write, which only sees us).
                    if let Some(cpath) = &cgroup_procs_c {
                        let fd = libc::open(cpath.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                        if fd >= 0 {
                            // Format the pid on the stack: `format!` here would
                            // allocate in a forked child (malloc-lock deadlock
                            // risk when the parent is multithreaded).
                            let mut buf = [0u8; 24];
                            let line = fmt_u32(kid as u32, &mut buf);
                            let _ = libc::write(fd, line.as_ptr().cast(), line.len());
                            libc::close(fd);
                        }
                    }
                    // Reap the workload and mirror its exit/signal so the
                    // waiter observes the real outcome through this supervisor.
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
                        // SIGKILL already reaches us directly via the process
                        // group.
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
                // Child = PID 1 of the new namespace. Mount a private procfs
                // over /proc so only this namespace is visible, then re-grant
                // Landlock read on the *new* procfs (the pre-fork grant pinned
                // the host procfs inode, now shadowed by this mount).
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

            // 1d. Config lockdown (interactive agent sessions). Bind each agent config
            //     path read-only so the in-box agent cannot edit it and, for the
            //     project-scope directories, cannot create a `settings.local.json`
            //     carrying `disableAllHooks`. Runs in our private mount namespace,
            //     before Landlock and seccomp, while we still hold CAP_SYS_ADMIN in
            //     the userns; `mount`/`umount2` are on the deny-list, so the workload
            //     can neither undo nor stack over these. Fail-closed.
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

            // 1e. Private-path binds (Idea 3). Bind each per-env backing dir over its
            //     workspace-relative path so concurrent envs of the same repo see
            //     distinct inodes, with no cross-env `flock`/`fcntl` or
            //     single-writer-cache contention. Read-write, unlike the config
            //     lockdown above; same private mount ns, before Landlock. The backing
            //     dir is separately Landlock-granted host-side. Fail-closed.
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

            // 1f. HOME-state redirect binds (#1). Bind each per-env credential copy
            //     over the agent runtime's real `~/.claude`/`~/.claude.json`/`~/.codex`
            //     so in-box writes (session history, refreshed tokens) land in the
            //     env's own copy. Concurrent agent boxes never race the shared real
            //     files, and the real HOME is never written. Same private mount ns,
            //     before Landlock; the backing copy is separately Landlock-granted
            //     rw host-side (it was pushed onto fs_write). Fail-closed.
            for (backing, target) in &home_bind_c {
                if libc::mount(
                    backing.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "home-state bind failed: {}",
                        Error::last_os_error()
                    )));
                }
            }

            // 1g. Read-only cache binds. A cache the box could write is a
            //     mutable surface shared between boxes, which is exactly what
            //     the design refuses; bind it, then remount read-only while we
            //     still hold CAP_SYS_ADMIN in the userns. Fail-closed: a cache
            //     we could bind but not seal is not offered at all.
            for (backing, target) in &ro_bind_c {
                if libc::mount(
                    backing.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "cache bind failed: {}",
                        Error::last_os_error()
                    )));
                }
                if libc::mount(
                    std::ptr::null(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(Error::other(format!(
                        "cache remount-ro failed: {}",
                        Error::last_os_error()
                    )));
                }
            }

            // 1h. The writable cache bind (refresh only). Its target is created
            //     by the caller host-side; a failure here is fatal rather than
            //     silently producing an empty cache.
            if let Some((backing, target)) = &cache_write_c
                && libc::mount(
                    backing.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0

            {
                return Err(Error::other(format!(
                    "cache write bind failed: {}",
                    Error::last_os_error()
                )));
            }

            // 2. Resource caps (cooperative, no cgroups needed).
            if let Some(bytes) = mem {
                // RLIMIT_DATA, not RLIMIT_AS.
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
                // Cap any single file the command writes. A disk-bomb backstop.
                let lim = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            if let Some(secs) = cpu {
                // Hard CPU-time cap (SIGKILL at the hard limit). A kernel
                // backstop to the host-side wall-clock kill.
                let lim = libc::rlimit { rlim_cur: secs, rlim_max: secs };
                if libc::setrlimit(libc::RLIMIT_CPU, &lim) != 0 {
                    return Err(Error::last_os_error());
                }
            }
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &core);

            // 3. No new privileges: required by Landlock, and blocks setuid
            //    escalation on its own.
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(Error::last_os_error());
            }

            // 4.
            let rs = ruleset_slot
                .take()
                .ok_or_else(|| Error::other("landlock ruleset consumed twice"))?;
            let status = rs
                .restrict_self()
                .map_err(|e| Error::other(format!("landlock restrict_self: {e}")))?;
            if status.ruleset != landlock::RulesetStatus::FullyEnforced {
                return Err(Error::other(
                    "landlock is not fully enforced (fail-closed): the kernel accepted only \
                     part of the filesystem allowlist, so the confinement would not be the \
                     one this tier claims",
                ));
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
    // Created BEFORE the command so its `cgroup.procs` path can be handed to
    // the PID-namespace supervisor (which joins the workload to it).
    // Unavailable → `None`, and the rlimits set in the child still apply.
    let cg = make_run_cgroup(p.mem_bytes, p.max_procs);
    let procs = cg.as_ref().map(|c| c.procs_path());
    // Process tier: netns only when egress is denied; no seccomp-notify gate;
    // the workload is confined to a fresh PID namespace + private procfs
    // (pidns=true).
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
        // Under the PID-namespace jail the workload runs as a grandchild of a
        // thin supervisor, so `wait4`'s rusage is the supervisor's, not the
        // workload's. Without a cgroup we cannot attribute rss/cpu. Report
        // unknown rather than a misleading figure. The in-child rlimits still
        // *enforce* the caps.
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

/// macOS `process` tier: the same contract (confined filesystem, no egress,
/// wall clock, rlimits) delivered by Seatbelt instead of Landlock+seccomp. The
/// process tier never proxies egress, so no loopback port is opened.
#[cfg(target_os = "macos")]
fn run_confined(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    crate::seatbelt::run(policy, work, argv, injected_env, &seatbelt_opts(false, &[]))
}

/// Build the Seatbelt options for a kernel-tier run on macOS.
#[cfg(target_os = "macos")]
pub(crate) fn seatbelt_opts(
    interactive: bool,
    proxy_ports: &[u16],
) -> crate::seatbelt::SeatbeltOptions {
    crate::seatbelt::SeatbeltOptions {
        proxy_ports: proxy_ports.to_vec(),
        interactive,
        home: std::env::var_os("HOME").map(PathBuf::from),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run_confined(
    _policy: &ResolvedPolicy,
    _work: &Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    Err(H5iError::Metadata(
        "isolation=process needs Linux (Landlock+seccomp) or macOS (Seatbelt) — this build has \
         neither on this target (fail-closed)"
            .into(),
    ))
}

/// Map a probed Landlock ABI version to the highest version this crate knows.
#[cfg(target_os = "linux")]
/// The `landlock` crate's `ABI` for a probed kernel ABI level.
///
/// The `_` arm caps at `V5`, the newest this crate knows: the conservative
/// direction for *this* ruleset, and a residual worth naming, since a kernel
/// newer than the crate is asked for V5's access rights. It costs nothing
/// today (`AccessFs` has not grown since ABI 5's `IOCTL_DEV`; ABI 6 and later
/// added scopes and TCP network rights, neither used here) but the arm
/// silently absorbs a future `AccessFs` bit, so a `landlock` upgrade is a
/// reason to come back here.
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
/// This is the security contract, kept as its own function so a unit test can
/// assert the security-critical members are present without a kernel. Every
/// entry is an administrative, introspection, namespace or fs-handle syscall
/// that a build or test workload never legitimately issues. We deliberately
/// do NOT deny clone/clone3/fork, needed for normal subprocesses; the
/// documented clone-with-CLONE_NEWUSER gap is closed by the hardened
/// allowlist profile, not here.
#[cfg(target_os = "linux")]
fn denied_syscalls() -> Vec<libc::c_long> {
    // libc's musl/aarch64 module omits SYS_kexec_file_load (it is present on
    // glibc and on musl/x86_64). Supply the arch syscall number ourselves so
    // the deny-list still blocks it there; everywhere else use libc's constant.
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
        // io_uring. A large, repeatedly-exploited kernel attack surface that
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

/// The seccomp *deny-list* (v1, §5): dangerous administrative / introspection
/// syscalls return EPERM; everything else is allowed. A default-deny allowlist
/// is a later hardened profile. Known gap (documented, not hidden): `clone`
/// with CLONE_NEWUSER is not arg-filtered in v1: `unshare` is denied, and
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
    let program: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| {
            H5iError::Metadata(format!("seccomp compile failed: {e}"))
        })?;
    Ok(prepend_x32_guard(program))
}

/// Refuse the x32 ABI before the deny-list runs.
#[cfg(target_os = "linux")]
fn prepend_x32_guard(program: seccompiler::BpfProgram) -> seccompiler::BpfProgram {
    use seccompiler::sock_filter;
    // Spelled as literals: the classic-BPF opcode components (BPF_LD|BPF_W|
    // BPF_ABS, BPF_JMP|BPF_JGE|BPF_K, BPF_RET|BPF_K) include zero-valued terms,
    // and clippy rightly objects to `x | 0`.
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JGE_K: u16 = 0x35;
    const BPF_RET_K: u16 = 0x06;
    const OFF_NR: u32 = 0;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

    let mut out: seccompiler::BpfProgram = vec![
        // A = nr
        sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: OFF_NR },
        // nr >= X32 bit → fall through to the kill; otherwise skip it.
        sock_filter { code: BPF_JMP_JGE_K, jt: 0, jf: 1, k: X32_SYSCALL_BIT },
        sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_KILL_PROCESS },
    ];
    out.extend(program);
    out
}

/// Spawn `cmd`, stream stdout/stderr off-thread, and enforce `wall` as a hard
/// deadline (SIGKILL). stdin is closed. Env runs are non-interactive by
/// construction so a confined process can't block on a prompt forever.
pub(crate) fn wait_with_deadline(
    mut cmd: std::process::Command,
    wall: Duration,
    argv: &[String],
    cgroup_procs: Option<&Path>,
) -> Result<ExecOutcome, H5iError> {
    use std::process::Stdio;

    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", argv.join(" "))))?;

    // Move the child into its cgroup as early as possible (best-effort): write
    // its pid to the cgroup's `cgroup.procs`. There's a sub-millisecond window
    // between spawn and this write where the child is not yet limited. Accepted
    // for v1 (CLONE_INTO_CGROUP would close it but isn't exposed by std).
    if let Some(procs) = cgroup_procs {
        let _ = std::fs::write(procs, child.id().to_string());
    }

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || drain_capped(&mut out_pipe));
    let err_h = std::thread::spawn(move || drain_capped(&mut err_pipe));

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
/// the operator, not a timer, and, having skipped `setsid`, they have no
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
            return (exit_code, timed_out, cpu_ms(&usage), Some(maxrss_kb(&usage)));
        }
        if r == -1 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // Lost the child (e.g. ECHILD). Fall back to std's bookkeeping.
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

/// Peak RSS in KiB, normalising the one `rusage` field whose unit POSIX never
/// fixed: Linux (and the BSDs) report `ru_maxrss` in kilobytes, Darwin reports
/// it in bytes. Taking the raw value there over-reported every receipt by
/// 1024×, so a `pwd` claimed 1.1 GiB and a 512 MiB allocation claimed a
/// terabyte. Numbers that then travelled into export reports.
#[cfg(unix)]
fn maxrss_kb(u: &libc::rusage) -> i64 {
    if cfg!(target_vendor = "apple") {
        u.ru_maxrss / 1024
    } else {
        u.ru_maxrss
    }
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
    fn home_bind_mount_order_places_private_tmp_last() {
        let binds = vec![
            HomeBind {
                backing: PathBuf::from("/tmp/repo/env/tmp"),
                target: PathBuf::from("/tmp"),
            },
            HomeBind {
                backing: PathBuf::from("/tmp/repo/env/home/claude"),
                target: PathBuf::from("/home/test/.claude"),
            },
            HomeBind {
                backing: PathBuf::from("/tmp/repo/env/home/claude.json"),
                target: PathBuf::from("/home/test/.claude.json"),
            },
        ];

        let ordered = home_binds_in_mount_order(&binds);
        assert_eq!(ordered[0].target, Path::new("/home/test/.claude"));
        assert_eq!(ordered[1].target, Path::new("/home/test/.claude.json"));
        assert_eq!(ordered[2].target, Path::new("/tmp"));
    }

    /// Functional, real-kernel proof that `policy.home_binds` shadows the
    /// target path inside the confined child (the in-box half of per-env
    /// credential isolation #1). A backing file is bound over a target file;
    /// the confined `cat target` must read the BACKING bytes, never the
    /// target's own. Skips where process-tier confinement can't run
    /// (CI/AppArmor), like the rest of the kernel-tier suite. Net-deny so it
    /// needs no egress stack.
    #[cfg(target_os = "linux")]
    #[test]
    fn home_bind_shadows_target_inside_confined_child() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        // Both live under $WORK (implicitly rw-granted) so the test exercises
        // the bind, not Landlock. `target` stands in for the real
        // `~/.claude.json`.
        std::fs::write(work.join("target"), "REAL-HOST-CREDS").unwrap();
        std::fs::write(work.join("backing"), "PER-ENV-COPY").unwrap();

        let mut pol = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        pol.home_binds.push(HomeBind {
            backing: work.join("backing"),
            target: work.join("target"),
        });

        if verify_exec(&pol).is_err() {
            eprintln!("SKIP home_bind_shadows_target_inside_confined_child: process tier not runnable");
            return;
        }
        let out = run_with_env(
            &pol,
            &work,
            &["/bin/sh".into(), "-c".into(), "cat target".into()],
            &[],
        )
        .expect("confined run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("PER-ENV-COPY"),
            "the bind must redirect `target` to the backing copy; got: {stdout:?} / {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("REAL-HOST-CREDS"),
            "the real target bytes must be shadowed by the bind"
        );
    }

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
    fn shell_rcfile_parses_and_defaults_none() {
        // Unset → None (so the generated plain rc is used, and the policy
        // digest is unchanged: skip_serializing_if keeps it out of the
        // serialization).
        let base = load_from_str(doc_example_toml(), "default", None).unwrap();
        assert_eq!(base.shell_rcfile, None);
        assert!(
            !toml::to_string(&ResolvedPolicy::new(base.isolation, base.clone()))
                .unwrap()
                .contains("shell_rcfile"),
            "an unset shell_rcfile must not appear in the serialized (digested) policy"
        );

        // Set → carried onto the profile.
        let with_rc = r#"
[profile.dev]
isolation = "process"
[profile.dev.shell]
rcfile = ".h5i/box.bashrc"
"#;
        let p = load_from_str(with_rc, "dev", None).unwrap();
        assert_eq!(p.shell_rcfile.as_deref(), Some(".h5i/box.bashrc"));
    }

    #[test]
    fn persona_sources_parse_and_validate() {
        // Unset → empty, and kept out of the digested serialization.
        let base = load_from_str(doc_example_toml(), "default", None).unwrap();
        assert!(base.persona.is_empty());
        assert!(
            !toml::to_string(&ResolvedPolicy::new(base.isolation, base.clone()))
                .unwrap()
                .contains("persona"),
            "an empty persona list must not appear in the serialized (digested) policy"
        );

        // Set → carried in declared order.
        let with_persona = r#"
[profile.architect]
isolation = "process"
persona = ["plugin/persona/architect.md", "plugin/persona/careful.md"]
"#;
        let p = load_from_str(with_persona, "architect", None).unwrap();
        assert_eq!(
            p.persona,
            vec!["plugin/persona/architect.md", "plugin/persona/careful.md"]
        );

        // Fail-closed: absolute path and `..` escape are both refused at load.
        for bad in ["/etc/passwd", "../secrets.md", "a/../../b.md"] {
            let doc = format!("[profile.x]\nisolation = \"process\"\npersona = [\"{bad}\"]\n");
            assert!(
                load_from_str(&doc, "x", None).is_err(),
                "persona source '{bad}' must be refused (fail-closed)"
            );
        }
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
        // Comma (unsupported by the container mount syntax). Fail closed at
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
        // A profile that declares no private paths must serialize/digest
        // exactly as before the field existed (skip_serializing_if = empty).
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

    /// The deny-list is a security *contract*: removing any of these syscalls silently widens
    /// the sandbox.
    #[cfg(target_os = "linux")]
    #[test]
    fn landlock_enforces_fully_or_not_at_all() {
        let Some(probed) = probe_landlock_abi() else {
            return; // no Landlock on this kernel; `resolve` already refuses the tier
        };
        let abi = landlock_abi_for(probed);

        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            use landlock::{
                path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset,
                RulesetAttr, RulesetCreatedAttr, RulesetStatus,
            };
            // The same construction the real path uses, with `/` standing in
            // for the grants. What is under test is the enforcement status,
            // not which paths were granted.
            let code = (|| -> Option<i32> {
                let status = Ruleset::default()
                    .set_compatibility(CompatLevel::HardRequirement)
                    .handle_access(AccessFs::from_all(abi))
                    .and_then(|r| r.create())
                    .and_then(|r| {
                        r.add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi)))
                    })
                    .and_then(|r| r.restrict_self())
                    .ok()?;
                Some(match status.ruleset {
                    RulesetStatus::FullyEnforced => 0,
                    RulesetStatus::PartiallyEnforced => 1,
                    RulesetStatus::NotEnforced => 2,
                })
            })()
            .unwrap_or(3);
            unsafe { libc::_exit(code) };
        }
        let mut wstatus = 0;
        unsafe { libc::waitpid(child, &mut wstatus, 0) };
        let code = libc::WEXITSTATUS(wstatus);
        assert_eq!(
            code, 0,
            "landlock ABI {probed} reported {} — the tier's confinement is only as \
             good as this, and `restrict_self` is checked against it",
            match code {
                1 => "PartiallyEnforced: some requested access rights are not in force",
                2 => "NotEnforced",
                _ => "an error while building a ruleset HardRequirement should have accepted",
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn seccomp_deny_list_covers_security_critical_syscalls() {
        let denied = denied_syscalls();
        let must_block: &[(&str, libc::c_long)] = &[
            // config-lockdown tamper-resistance depends on these two being
            // denied
            ("mount", libc::SYS_mount),
            ("umount2", libc::SYS_umount2),
            // container/chroot escape
            ("pivot_root", libc::SYS_pivot_root),
            ("chroot", libc::SYS_chroot),
            // process-tracing escape vectors
            ("ptrace", libc::SYS_ptrace),
            ("process_vm_readv", libc::SYS_process_vm_readv),
            ("process_vm_writev", libc::SYS_process_vm_writev),
            // namespace entry/creation (the /proc-environ + userns-escape
            // surface)
            ("setns", libc::SYS_setns),
            ("unshare", libc::SYS_unshare),
            // privileged kernel interfaces
            ("bpf", libc::SYS_bpf),
            ("init_module", libc::SYS_init_module),
            ("finit_module", libc::SYS_finit_module),
            // path-confinement bypass via fs handles
            ("open_by_handle_at", libc::SYS_open_by_handle_at),
            ("name_to_handle_at", libc::SYS_name_to_handle_at),
            // io_uring. Large, repeatedly-exploited surface that also bypasses
            // seccomp
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

    /// A `net.mode = "host"` box that cannot resolve a name is a box that looks
    /// like a host with no network, and the cause is one symlink: `/etc` is
    /// granted, `/etc/resolv.conf` points outside it, and Landlock follows the
    /// link. The grant is the same on every host whether or not the file is
    /// there, because a digest that varied with `/etc` could not promise that
    /// two boxes with one digest were allowed the same things.
    #[test]
    fn the_resolver_config_is_granted_wherever_it_actually_lives() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        // WSL, which is where this was found.
        assert!(p.fs_read.iter().any(|s| s == "/mnt/wsl/resolv.conf"), "{:?}", p.fs_read);
        // systemd-resolved, the common case everywhere else.
        assert!(
            p.fs_read.iter().any(|s| s == "/run/systemd/resolve/stub-resolv.conf"),
            "{:?}",
            p.fs_read
        );
        // One file each. `/run` itself is not a grant, and neither is the
        // directory holding the stub.
        assert!(!p.fs_read.iter().any(|s| s == "/run"), "{:?}", p.fs_read);
        assert!(!p.fs_read.iter().any(|s| s == "/mnt/wsl"), "{:?}", p.fs_read);

        // The same list on an unconfined profile's absence of one: nothing is
        // granted there at all, so the entries cannot leak in through it.
        let open = Profile::builtin("default", IsolationClaim::Workspace);
        assert!(open.fs_read.is_empty(), "{:?}", open.fs_read);
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
        // Narrowed binaries (not all of ~/.local) + the runtime's own share
        // dir.
        assert!(p.fs_read.iter().any(|s| s == "~/.local/bin"));
        assert!(!p.fs_read.iter().any(|s| s == "~/.local"), "blanket ~/.local removed");
        assert!(p.fs_read.iter().any(|s| s == "~/.local/share/claude"));
        // Rustup shims under ~/.cargo/bin need read-only rustup metadata to
        // locate the active toolchain, but ~/.cargo and ~/.rustup stay
        // ungranted.
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
        // Own API egress only, not OpenAI's.
        assert!(p.net_egress.iter().any(|s| s == "api.anthropic.com"));
        assert!(!p.net_egress.iter().any(|s| s == "api.openai.com"), "no cross-runtime egress");
        assert!(p.env_pass.iter().any(|k| k == "TERM"));
        assert!(p.env_pass.iter().any(|k| k == "SHELL"));
        // The default deny set survives and no grant contains a denied child
        // (validate_profile ran inside load_profile).
        assert!(p.fs_deny.iter().any(|s| s == "~/.ssh"));
    }

    /// Landlock grants follow symlinks, so the lint has to as well. A grant of
    /// a symlink into `$HOME` really grants the home directory, and a textual
    /// prefix check never saw the denied child underneath it.
    #[test]
    #[cfg(unix)]
    fn fs_deny_lint_sees_through_a_symlinked_grant() {
        let td = tempfile::tempdir().unwrap();
        let home = td.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        // The grant is a symlink to the directory that holds the denied child.
        let tools = td.path().join("work-tools");
        std::os::unix::fs::symlink(&home, &tools).unwrap();

        let mut p = Profile::builtin("default", IsolationClaim::Process);
        p.fs_read = vec![tools.display().to_string()];
        p.fs_deny = vec![home.join(".ssh").display().to_string()];
        let err = validate_profile(&p).map(|_| ()).unwrap_err();
        assert!(err.to_string().contains("denied child"), "{err}");

        // An unrelated grant still loads.
        p.fs_read = vec![td.path().join("elsewhere").display().to_string()];
        assert!(validate_profile(&p).is_ok());
    }

    /// Host-side scratch must not be pre-plantable. The nftables ruleset lands
    /// in one of these and is then executed by a child with CAP_NET_ADMIN, so a
    /// guessable path let another local user pick the box's egress policy.
    #[test]
    fn private_scratch_is_unguessable_and_owner_only() {
        let a = private_scratch_dir("h5i-test-scratch").unwrap();
        let b = private_scratch_dir("h5i-test-scratch").unwrap();
        assert_ne!(a, b, "two scratch dirs must not collide");
        // Not derived from the pid: that is what made the old name guessable.
        assert!(!a.to_string_lossy().contains(&std::process::id().to_string()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "scratch dir must be 0700, got {mode:o}");
        }
        // Never reuses an existing directory.
        assert!(std::fs::DirBuilder::new().create(&a).is_err());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn agent_profile_injects_is_sandbox() {
        // Agent-in-box profiles map the agent to root-in-userns on the egress
        // tiers, so Claude's `getuid()==0` guard would refuse
        // `--dangerously-skip-permissions`. `IS_SANDBOX=1` is injected to skip
        // only that check (no new capability). For every agent profile/runtime.
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
        // A caller-supplied / broker IS_SANDBOX wins. We only set the default,
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
    fn agent_profiles_reach_their_own_auth_host() {
        // Without the auth host an agent box is a one-session box: the seeded
        // credential copy expires, the silent refresh has nowhere to go, and
        // the in-box login that should repopulate it cannot complete either
        // (the paste comes back "invalid token"). Regression guard. The API
        // host alone looks sufficient right up until a session outlives its
        // token.
        let dir = tempfile::tempdir().unwrap();
        for (name, auth_host, foreign) in [
            ("agent-claude", "platform.claude.com", "auth.openai.com"),
            ("agent-codex", "auth.openai.com", "platform.claude.com"),
        ] {
            let p = load_profile(dir.path(), name, Some(IsolationClaim::Supervised)).unwrap();
            assert!(
                p.net_egress.iter().any(|s| s == auth_host),
                "{name} cannot refresh or log in without {auth_host}: {:?}",
                p.net_egress
            );
            assert!(
                !p.net_egress.iter().any(|s| s == foreign),
                "{name} must not reach the other runtime's auth host"
            );
        }
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
        // process tier (and below) cannot enforce. Refuse, never weaken.
        let dir = tempfile::tempdir().unwrap();
        for tier in [IsolationClaim::Workspace, IsolationClaim::Process] {
            let err = load_profile(dir.path(), "agent", Some(tier)).unwrap_err();
            assert!(err.to_string().contains("net.egress"), "{tier:?}: {err}");
        }
    }

    #[test]
    fn user_defined_agent_profile_merges_over_agent_builtin() {
        // A partial [profile.agent-claude] keeps the agent-in-box grants as its
        // base, including net.egress when the overlay omits it (an agent box
        // that silently lost its API allowlist would be bricked, not safer).
        let toml_text = r#"
[profile.agent-claude]
isolation = "supervised"
resources = { mem = "2G" }
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert_eq!(p.mem_bytes, Some(2 * 1024 * 1024 * 1024));
        assert!(p.fs_read.iter().any(|s| s == "~/.local/bin"), "agent base grants inherited");
        assert!(p.fs_write.iter().any(|s| s == "~/.claude"));
        assert!(
            p.net_egress.iter().any(|s| s == "api.anthropic.com"),
            "omitted net.egress inherits the builtin agent allowlist"
        );
    }

    #[test]
    fn explicit_empty_egress_opts_out_of_the_builtin_allowlist() {
        // `egress = []` is a deliberate opt-out. Kept empty, never re-widened.
        let toml_text = r#"
[profile.agent-claude]
isolation = "supervised"
net.egress = []
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert!(p.net_egress.is_empty(), "explicit [] must stay empty");
    }

    /// The same opt-out rule for the filesystem and environment lists. Treating
    /// `[]` as "omitted" inverted a narrowing into a widening: an author asking
    /// for a read-only box with `fs.write = []` was handed the builtin base
    /// ($WORK, ~/.claude, ~/.cache, /tmp, /dev/tty) instead.
    #[test]
    fn explicit_empty_fs_and_env_lists_are_not_re_widened() {
        let toml_text = r#"
[profile.agent-claude]
isolation = "supervised"
net.egress = ["api.anthropic.com"]
fs.write = []
fs.read = []
env.pass = []
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert!(p.fs_write.is_empty(), "explicit fs.write = [] must stay empty: {:?}", p.fs_write);
        assert!(p.fs_read.is_empty(), "explicit fs.read = [] must stay empty: {:?}", p.fs_read);
        assert!(p.env_pass.is_empty(), "explicit env.pass = [] must stay empty: {:?}", p.env_pass);
    }

    /// Omitting them still inherits the base, so a partial overlay stays
    /// usable.
    #[test]
    fn omitted_fs_and_env_lists_still_inherit_the_builtin_base() {
        let toml_text = r#"
[profile.agent-claude]
isolation = "supervised"
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert!(!p.fs_write.is_empty(), "omitted fs.write inherits the base");
        assert!(!p.env_pass.is_empty(), "omitted env.pass inherits the base");
        assert!(p.env_pass.iter().any(|k| k == "PATH"));
    }

    #[test]
    fn file_level_container_image_is_the_default_for_imageless_profiles() {
        // A repo-level `[container] image` supplies the image for the builtin
        // agent profiles (which cannot know it); a profile-level
        // `container.image` still wins.
        let toml_text = r#"
[container]
image = "localhost/repo-default:1"

[profile.agent-claude]
isolation = "container"

[profile.custom]
isolation = "container"
container.image = "localhost/mine:2"
"#;
        let p = load_from_str(toml_text, "agent-claude", None).unwrap();
        assert_eq!(p.image.as_deref(), Some("localhost/repo-default:1"));
        // Builtin name with NO [profile.X] entry also picks up the default.
        let p = load_from_str(toml_text, "default", None).unwrap();
        assert_eq!(p.image.as_deref(), Some("localhost/repo-default:1"));
        let p = load_from_str(toml_text, "custom", None).unwrap();
        assert_eq!(p.image.as_deref(), Some("localhost/mine:2"));
    }

    /// The image is repo-supplied and reaches two places where its bytes are
    /// syntax: Podman's positional argument (no `--` before it, so a leading
    /// `-` is read as a flag) and `--mount type=image,source=<image>,…`, where
    /// a comma ends the value and starts an option.
    #[test]
    fn an_image_reference_that_is_really_an_argument_is_refused() {
        for good in [
            "alpine",
            "alpine:3.19",
            "docker.io/library/alpine:3.19",
            "localhost/mine:2",
            "ghcr.io/org/img@sha256:abc123",
            "registry.example.com:5000/team/img:tag",
            "oci-archive:/var/tmp/img.tar",
        ] {
            assert!(validate_image(good).is_ok(), "{good} must be accepted");
        }
        for bad in [
            "",
            "-v",
            "--privileged",
            "--security-opt=label=disable",
            "alpine,rw",
            "alpine destination=/etc",
            "alpine\u{1b}[2J",
            "alpine\n--privileged",
        ] {
            assert!(validate_image(bad).is_err(), "{bad:?} must be refused");
        }

        // And it is refused at policy load, not at run time.
        let toml_text = "[profile.custom]\nisolation = \"container\"\ncontainer.image = \"--privileged\"\n";
        let err = load_from_str(toml_text, "custom", None).unwrap_err().to_string();
        assert!(err.contains("Podman reads as an option"), "{err}");

        // The refusal shows the value without letting it reach the terminal
        // as a control sequence.
        let err = validate_image("a\u{1b}[2Jb").unwrap_err().to_string();
        assert!(!err.contains('\u{1b}'), "{err:?}");
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
        // `auto` is a strategy, not a declared tier → None (defer to the
        // picker).
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
        // A profile that explicitly declares `workspace` must resolve to
        // exactly that under the default (non-forced) path. Deterministic, no
        // host probe.
        let dir = tmp_repo(Some("[profile.default]\nisolation = \"workspace\"\n"));
        assert_eq!(
            effective_auto(dir.path(), "default", false, None).unwrap(),
            IsolationClaim::Workspace
        );
    }

    #[test]
    fn effective_auto_never_picks_an_unrunnable_tier() {
        // The core invariant of secure-by-default: whatever auto picks (host
        // dependent) MUST pass the very checks `create` applies, so a default
        // env never fails at run time. Forced probe, no declared tier.
        let dir = tmp_repo(None);
        let tier = effective_auto(dir.path(), "default", true, None).unwrap();
        // Workspace is always runnable; any stronger pick must verify-exec
        // clean.
        if tier != IsolationClaim::Workspace {
            let p = load_profile(dir.path(), "default", Some(tier)).unwrap();
            let pol = resolve(&p, &probe_host()).expect("auto-picked tier must resolve");
            verify_exec(&pol).expect("auto-picked tier must verify-exec");
        }
        // And it is never weaker than workspace is meaningless. Just assert
        // it's a real claim (the match is exhaustive, so reaching here means
        // it's one).
        let _ = tier;
    }

    #[test]
    fn effective_auto_skips_container_without_an_image() {
        // The bare default has no container image, so auto must NOT pick
        // `container` (resolve refuses imageless container). It lands on a
        // kernel tier or workspace instead.
        let dir = tmp_repo(None);
        let tier = effective_auto(dir.path(), "default", true, None).unwrap();
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
        // $WORK plus the write-granted sinks (/dev/null, /dev/zero), no other
        // host paths are writable.
        assert_eq!(p.fs_write, vec!["$WORK", "/dev/null", "/dev/zero"]);
        assert!(p.net_egress.is_empty());
        assert!(p.secret_grants.is_empty());
        // Supervised must rank above Process so the net.egress preflight lint
        // (which refuses egress at <= Process) doesn't reject a supervised
        // egress.
        assert!(IsolationClaim::Supervised > IsolationClaim::Process);
        assert!(IsolationClaim::Supervised < IsolationClaim::Container);
    }

    #[test]
    fn fs_deny_lint_rejects_granted_parent_of_denied_child() {
        // Granting $HOME while denying ~/.ssh is unenforceable under Landlock
        // (allowlist-only). The policy must be refused, not weakened.
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

    /// The lint is the only thing standing between a grant and a denied child
    /// inside it: Landlock has no deny rules, so `fs.deny` is a preflight
    /// refusal and nothing else. With `$HOME` unset it compared the literal
    /// string `~/.ssh` against `/Users/dev`, found no overlap, and let the
    /// policy load with the key material inside the grant.
    ///
    /// Driven through the helper rather than by unsetting `HOME`: cargo runs
    /// tests as threads in one process, so a `remove_var` here is a
    /// `remove_var` for every test running beside it.
    #[test]
    fn a_runaway_child_cannot_grow_the_hosts_memory_without_limit() {
        // Under the cap: byte-identical, no marker.
        let small = vec![b'x'; 1024];
        assert_eq!(drain_capped(&small[..]), small);

        // Over it: capped, and it says so rather than looking complete.
        let big = vec![b'y'; MAX_CAPTURED_STREAM + 4096];
        let out = drain_capped(&big[..]);
        assert!(
            out.len() < MAX_CAPTURED_STREAM + 1024,
            "retained {} bytes for a {} byte stream",
            out.len(),
            big.len()
        );
        assert!(out.starts_with(&[b'y'; 16]));
        assert!(
            String::from_utf8_lossy(&out).contains("output truncated at the capture cap"),
            "a silent truncation reads as a complete capture"
        );

        // Exactly at the cap is complete, not truncated.
        let exact = vec![b'z'; MAX_CAPTURED_STREAM];
        let out = drain_capped(&exact[..]);
        assert_eq!(out.len(), MAX_CAPTURED_STREAM);
        assert!(!String::from_utf8_lossy(&out).contains("truncated"));
    }

    #[test]
    fn a_tilde_path_with_no_home_to_resolve_it_is_refused() {
        let mut p = Profile::builtin("default", IsolationClaim::Process);
        p.fs_read = vec!["/Users/dev".to_string()];
        p.fs_write = Vec::new();
        p.fs_deny = vec!["~/.ssh".to_string()];

        // With a home there is nothing to complain about.
        assert!(unresolvable_tilde_entries(&p, true).is_empty());
        // Without one, the deny cannot be resolved and must not be ignored.
        assert_eq!(
            unresolvable_tilde_entries(&p, false),
            vec![&"~/.ssh".to_string()]
        );

        // A grant counts too: it would confer nothing while claiming to.
        p.fs_deny = Vec::new();
        p.fs_read = vec!["~/tools".to_string(), "/usr".to_string()];
        assert_eq!(
            unresolvable_tilde_entries(&p, false),
            vec![&"~/tools".to_string()]
        );

        // `$WORK`/`$REPO` are h5i's own tokens, resolved elsewhere.
        p.fs_read = vec!["$WORK".to_string()];
        p.fs_deny = vec!["$REPO/.env".to_string()];
        assert!(unresolvable_tilde_entries(&p, false).is_empty());
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
        // A cap that wraps to zero is not a small cap: `--memory 0` is how
        // Podman spells *no limit*, and the policy digest records the zero as
        // though a cap were in force.
        assert!(parse_mem("17179869184G").is_err());
        assert!(parse_wall("18446744073709551615h").is_err());
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
        HostCaps {
            os: "linux".into(),
            landlock_abi: landlock,
            userns,
            seccomp,
            seatbelt: false,
            container_runtime: None,
            microvm_runtime: None,
        }
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
        // `hardened-container` (gVisor/Kata) still has no adapter. `microvm`
        // does. See `resolve_microvm_requires_image_and_runtime`.
        let p = Profile::builtin("default", IsolationClaim::HardenedContainer);
        let err = resolve(&p, &caps(Some(5), true, true)).unwrap_err();
        assert!(err.to_string().contains("backend"), "{err}");
    }

    fn caps_with_container(runtime: Option<&str>) -> HostCaps {
        HostCaps {
            os: "linux".into(),
            landlock_abi: Some(3),
            userns: true,
            seccomp: true,
            seatbelt: false,
            container_runtime: runtime.map(str::to_owned),
            microvm_runtime: None,
        }
    }

    fn caps_with_microvm(runtime: Option<&str>) -> HostCaps {
        HostCaps {
            microvm_runtime: runtime.map(str::to_owned),
            ..caps_with_container(None)
        }
    }

    #[test]
    fn resolve_microvm_requires_image_and_runtime() {
        // A missing image is a *static* profile error, true on every host, so
        // it is reported before the host probe, and a box or a CI runner with
        // no virtualization still gets the message it can act on.
        let bare = Profile::builtin("default", IsolationClaim::Microvm);
        let err = resolve(&bare, &caps_with_microvm(Some("msb"))).unwrap_err();
        assert!(err.to_string().contains("requires a base image"), "{err}");

        // Image declared, no runtime → refuse, never downgrade.
        let mut imaged = bare.clone();
        imaged.image = Some("alpine".into());
        let err = resolve(&imaged, &caps_with_microvm(None)).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("never silently downgrades"), "{text}");
        assert!(text.contains("microvm"), "{text}");

        // Both halves present → satisfiable.
        let policy = resolve(&imaged, &caps_with_microvm(Some("msb"))).unwrap();
        assert_eq!(policy.claim, IsolationClaim::Microvm);
    }

    #[test]
    fn resolve_microvm_rejects_an_untranslatable_egress_entry_at_create_time() {
        // A `net.egress` entry this tier's rule grammar cannot carry exactly is
        // a policy it cannot enforce. Finding that out at `env create` beats
        // finding it out on the first run inside the box.
        let mut p = Profile::builtin("default", IsolationClaim::Microvm);
        p.image = Some("alpine".into());
        p.net_egress = vec!["*.com".into()];
        let err = resolve(&p, &caps_with_microvm(Some("msb"))).unwrap_err();
        assert!(err.to_string().contains("at least two"), "{err}");
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

    fn mac_caps(seatbelt: bool) -> HostCaps {
        HostCaps {
            os: "macos".into(),
            landlock_abi: None,
            userns: false,
            seccomp: false,
            seatbelt,
            container_runtime: None,
            microvm_runtime: None,
        }
    }

    #[test]
    fn resolve_process_on_macos_rests_on_seatbelt_not_landlock() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        // A Mac has no Landlock, no seccomp and no userns, and must not be
        // judged by them. Seatbelt is its kernel tier.
        assert!(
            resolve(&p, &mac_caps(true)).is_ok(),
            "a usable Seatbelt satisfies the process claim on macOS"
        );
        // ...and an unusable one refuses rather than downgrading.
        let err = resolve(&p, &mac_caps(false)).unwrap_err();
        assert!(err.to_string().contains("Seatbelt"), "{err}");
        assert!(!err.to_string().contains("Landlock"), "{err}");
    }

    /// An interactive box shell shares the operator's terminal, so whether a
    /// box can type into it is a disclosed limit, and on Linux it is the
    /// *host's* setting. Report it wrong in the safe direction and an operator
    /// trusts a door that is open, so the unreadable case must read as
    /// injectable.
    #[test]
    fn tty_injection_reporting_fails_open() {
        assert!(tty_injection_from_sysctl(None), "an unreadable sysctl must read as open");
        assert!(
            tty_injection_from_sysctl(Some("1\n")),
            "upstream's default (y → 1) leaves TIOCSTI available"
        );
        assert!(!tty_injection_from_sysctl(Some("0\n")), "0 means the kernel refuses TIOCSTI");
        assert!(!tty_injection_from_sysctl(Some("0")), "no trailing newline is still 0");
        assert!(
            tty_injection_from_sysctl(Some("banana")),
            "anything we cannot read as a definite 0 must read as open"
        );
    }

    /// macOS answers from the Seatbelt *profile*, so the answer is tier-shaped:
    /// the subtraction exists only where a profile is applied. Reporting one
    /// constant for the host would claim the door is shut on exactly the paths
    /// that apply no profile. The direction the fail-open rule forbids.
    #[test]
    fn tty_injection_on_macos_follows_the_profile_not_the_platform() {
        // A kernel tier on a working Seatbelt: the profile subtracts it.
        assert!(!tty_input_injection(&mac_caps(true), IsolationClaim::Process));
        assert!(!tty_input_injection(&mac_caps(true), IsolationClaim::Supervised));

        // `workspace` runs unconfined by design, no profile, no subtraction.
        assert!(
            tty_input_injection(&mac_caps(true), IsolationClaim::Workspace),
            "an unconfined session applies no profile; nothing subtracts TIOCSTI"
        );
        // A host whose Seatbelt is unusable applies no profile either. The
        // kernel tiers refuse there, but the report must not say "blocked".
        assert!(
            tty_input_injection(&mac_caps(false), IsolationClaim::Process),
            "no working Seatbelt means no profile to subtract with"
        );

        // The image-backed tiers hand the box its own terminal, so its input
        // queue is its own. True whatever the host underneath.
        for claim in [
            IsolationClaim::Container,
            IsolationClaim::HardenedContainer,
            IsolationClaim::Microvm,
        ] {
            assert!(!tty_input_injection(&mac_caps(false), claim), "{claim:?} has its own tty");
        }

        // An OS with no backend at all: nothing is known, so nothing is
        // claimed.
        let mut unknown = mac_caps(true);
        unknown.os = "windows".into();
        assert!(tty_input_injection(&unknown, IsolationClaim::Process));
    }

    #[test]
    fn resolve_process_refused_on_a_platform_with_no_backend() {
        let p = Profile::builtin("default", IsolationClaim::Process);
        let win = HostCaps {
            os: "windows".into(),
            landlock_abi: None,
            userns: false,
            seccomp: false,
            seatbelt: false,
            container_runtime: None,
            microvm_runtime: None,
        };
        let err = resolve(&p, &win).unwrap_err();
        assert!(err.to_string().contains("windows"), "{err}");
    }

    #[test]
    fn kernel_confinement_asks_the_right_question_per_os() {
        assert!(mac_caps(true).kernel_confinement());
        assert!(!mac_caps(false).kernel_confinement());
        assert_eq!(mac_caps(true).confinement_mechanism(), "seatbelt");
        assert!(caps(Some(3), true, true).kernel_confinement());
        assert!(
            !caps(None, true, true).kernel_confinement(),
            "no Landlock is no kernel confinement on Linux"
        );
        assert_eq!(caps(Some(3), true, true).confinement_mechanism(), "landlock+seccomp");
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

    /// The PID-namespace tiers fork a thin supervisor inside `pre_exec`, and
    /// that supervisor inherits std's `CLOEXEC` spawn-status pipe. Until it
    /// dropped that descriptor, `Command::spawn` did not return until the
    /// workload had already exited, so the stdout drain threads started too
    /// late and any command whose output exceeded the 64 KiB pipe buffer
    /// deadlocked forever. Well over one pipe buffer, and it must come back
    /// whole.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_confined_run_does_not_deadlock_on_output_larger_than_the_pipe_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let policy = ResolvedPolicy::new(
            IsolationClaim::Process,
            Profile::builtin("default", IsolationClaim::Process),
        );
        if verify_exec(&policy).is_err() {
            eprintln!("SKIP output-deadlock test: process tier not runnable here");
            return;
        }
        // 4096 lines × ~64 B ≈ 256 KiB, four times the pipe buffer.
        let out = run(
            &policy,
            dir.path(),
            &[
                "sh".into(),
                "-c".into(),
                "i=0; while [ $i -lt 4096 ]; do echo \
                 'padding-padding-padding-padding-padding-padding-line'; i=$((i+1)); done"
                    .into(),
            ],
        )
        .expect("a confined run must not hang on a full stdout pipe");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out, "the run must finish well inside the wall clock");
        assert_eq!(
            out.stdout.iter().filter(|b| **b == b'\n').count(),
            4096,
            "every line must survive: got {} bytes",
            out.stdout.len()
        );
    }

    /// The same root cause disarmed the deadline itself: with `spawn` blocked
    /// until the workload exited, `wait_with_deadline` only began counting
    /// after the run it was supposed to bound. A confined `sleep` must be
    /// killed.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_wall_clock_bounds_a_confined_run_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Profile::builtin("default", IsolationClaim::Process);
        p.wall_secs = 1;
        let policy = ResolvedPolicy::new(IsolationClaim::Process, p);
        if verify_exec(&policy).is_err() {
            eprintln!("SKIP confined wall-clock test: process tier not runnable here");
            return;
        }
        let started = std::time::Instant::now();
        let out = run(&policy, dir.path(), &["sleep".into(), "30".into()]).expect("run");
        assert!(out.timed_out, "the wall clock must kill a confined run");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the kill must fire near the deadline, not after the command finishes: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn run_records_resource_usage() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
        // A command that burns a little wall time so the numbers are
        // non-trivial.
        let out = run(&policy, dir.path(), &["sh".into(), "-c".into(), "sleep 0.2".into()]).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.wall_ms >= 150, "wall_ms should reflect the ~200ms sleep: {}", out.wall_ms);
        // On Linux wait4 fills ru_maxrss (KiB). A real process is > 0.
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

    /// `env_pass` is digested policy and `box status` reports it as enforced,
    /// so every tier has to honour it, including `workspace`, where nothing
    /// else is confined and the operator's shell is therefore at its most
    /// exposed.
    #[test]
    fn the_env_allowlist_is_enforced_at_the_workspace_tier_too() {
        let dir = tempfile::tempdir().unwrap();
        let policy = ResolvedPolicy::new(
            IsolationClaim::Workspace,
            Profile::builtin("default", IsolationClaim::Workspace),
        );
        // A host variable nobody put on the allowlist. Set inside the test
        // rather than assumed, so the assertion is about the allowlist and not
        // about whatever the ambient environment happens to hold. Safety:
        // single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_UNLISTED_HOST_VAR", "must-not-reach-the-child");
        }
        let out = run(
            &policy,
            dir.path(),
            &[
                "sh".into(),
                "-c".into(),
                "echo \"unlisted=[$H5I_TEST_UNLISTED_HOST_VAR]\"; echo \"path=[${PATH:+set}]\"".into(),
            ],
        )
        .expect("workspace run");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_UNLISTED_HOST_VAR");
        }
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("unlisted=[]"),
            "an unlisted host var must not reach a workspace-tier child: {text}"
        );
        // …and the allowlisted ones still do, or the tier would simply be
        // broken.
        assert!(text.contains("path=[set]"), "PATH is on the allowlist: {text}");
    }

    #[test]
    fn empty_tools_allowlist_allows_anything() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::builtin("default", IsolationClaim::Workspace);
        assert!(p.tools.is_empty());
        let policy = ResolvedPolicy::new(IsolationClaim::Workspace, p);
        assert!(run(&policy, dir.path(), &["true".into()]).is_ok());
    }

    #[test]
    fn capabilities_report_invariants() {
        // Host-independent structural invariants (the actual bits vary by host,
        // so we don't assert on landlock/podman presence).
        let r = capabilities_report();
        // Every claim is reported, weakest → strongest.
        let claims: Vec<&str> = r.claims.iter().map(|c| c.claim).collect();
        assert_eq!(
            claims,
            vec![
                "workspace",
                "process",
                "supervised",
                "container",
                "hardened-container",
                "microvm",
            ]
        );
        // Workspace is the floor: no confinement needed, so always usable.
        let ws = r.claims.iter().find(|c| c.claim == "workspace").unwrap();
        assert!(ws.satisfiable && ws.runnable == Some(true));
        // The one remaining not-in-this-build backend is always unsatisfiable.
        let hc = r.claims.iter().find(|c| c.claim == "hardened-container").unwrap();
        assert!(!hc.satisfiable && hc.runnable.is_none());
        // The image-backed tiers are never exec-tested (a run needs an image),
        // and each tracks its own runtime.
        let mv = r.claims.iter().find(|c| c.claim == "microvm").unwrap();
        assert!(mv.runnable.is_none());
        assert_eq!(mv.satisfiable, r.microvm_runtime.is_some());
        let ct = r.claims.iter().find(|c| c.claim == "container").unwrap();
        assert!(ct.runnable.is_none());
        assert_eq!(ct.satisfiable, r.container_runtime.is_some());
        // A domain allowlist is enforced by the container tier's DNS-pinned
        // proxy, by the microvm tier's netstack rules, and on macOS also by the
        // supervised tier. Whose Seatbelt profile leaves the box no outbound
        // route except that same proxy. The Linux kernel tiers can deny all but
        // never allowlist, so they do not count towards this.
        let supervised_runs = r
            .claims
            .iter()
            .any(|c| c.claim == "supervised" && c.runnable == Some(true));
        assert_eq!(
            r.egress_enforced,
            r.container_runtime.is_some()
                || r.microvm_runtime.is_some()
                || (r.os == "macos" && supervised_runs)
        );
        // L3 enforcement is the microvm tier's alone: an L7 proxy stops `curl`
        // and does not stop a raw socket, and the report must not blur the two.
        assert_eq!(r.egress_enforced_l3, r.microvm_runtime.is_some());
        assert!(!r.egress_enforced_l3 || r.egress_enforced);
        // The two honesty flags, which exist so a caller never infers a
        // guarantee from a tier name that means different things per platform.
        assert!(
            !r.syscall_filter || r.os == "linux",
            "only Linux has a syscall deny-list; macOS must never claim one"
        );
        assert!(
            !(r.memory_limit
                && r.os == "macos"
                && r.container_runtime.is_none()
                && r.microvm_runtime.is_none()),
            "Darwin cannot cap memory without an image-backed tier"
        );
        assert!(
            !r.memory_limit || r.resource_limits,
            "a memory cap implies some resource limit is enforced"
        );
        let expected_mechanism = match r.os.as_str() {
            "linux" => "landlock+seccomp",
            "macos" => "seatbelt",
            _ => "none",
        };
        assert_eq!(r.mechanism, expected_mechanism);
        // strongest_tier is one of the known claims.
        assert!(claims.contains(&r.strongest_tier));
    }
}
