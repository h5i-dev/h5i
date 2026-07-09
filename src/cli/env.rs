//! `h5i env` — CLI handlers (migrated from main.rs).
use crate::*;

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Create an isolated environment: code branch + git worktree under
    /// .git/.h5i/env/, a forked reasoning branch, and a pinned, fail-closed
    /// policy. The base revision is frozen at creation.
    Create {
        /// Environment name (lowercase slug, e.g. `fix-auth`)
        name: String,
        /// Base revision (default: HEAD). Pinned immutably.
        #[arg(long)]
        from: Option<String>,
        /// Base the env on a GitHub pull request (number, #number, or URL):
        /// fetches refs/pull/<n>/head from the remote, pins it as the immutable
        /// base, and points the env's parent branch at a local `pr/<n>`
        /// tracking branch — so propose/apply review the PR head, and apply
        /// prints the push-back command. Needs only `git`; `gh` (optional)
        /// enriches the push-back hint with the PR's head branch name.
        #[arg(long, conflicts_with = "from", value_name = "NUMBER|URL")]
        pr: Option<String>,
        /// Remote to fetch the PR head from (with --pr).
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Policy profile from .h5i/env.toml. Built-ins need no file: `agent`
        /// (agent-in-box, scoped to $H5I_AGENT's runtime), `agent-claude` /
        /// `agent-codex` (pin one runtime: only that agent's HOME state + API
        /// egress), and `default` (fail-closed build/test confinement). Unset
        /// auto-picks the creating runtime's agent profile when this host can
        /// enforce it, else `default`.
        #[arg(long)]
        profile: Option<String>,
        /// Isolation: auto (default) | workspace | process | supervised | container | hardened-container | microvm.
        /// `auto` (or unset) picks the strongest tier the host can run; an explicit
        /// tier fails closed if the host cannot satisfy it (never silently downgrades).
        #[arg(long)]
        isolation: Option<String>,
        /// Workspace backend (auto|worktree)
        #[arg(long, default_value = "auto")]
        backend: String,
        /// Audit capture mode for wrapped in-env commands: signal (default) | all.
        /// `all` records every wrapped command, including small successful output.
        #[arg(long, default_value = "signal")]
        audit: String,
    },

    /// Run a command inside an environment, policy-enforced and
    /// capture-wrapped: exit code passes through, evidence lands in
    /// refs/h5i/objects tagged with the env id + policy digest.
    Run {
        /// Environment name (slug, `agent/slug`, or full `env/agent/slug`)
        name: String,
        /// The command to run, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Open an interactive, confined session INSIDE the environment — the
    /// "agent-in-box". stdio is inherited (a real terminal), so every command
    /// the session spawns is contained by the box, not by the agent choosing to
    /// wrap each call. Defaults to a login shell when no command is given.
    Shell {
        /// Environment name (slug, `agent/slug`, or full `env/agent/slug`)
        name: String,
        /// Attach as a READ-ONLY observer: `$WORK` is pinned read-only, the box
        /// gets an ephemeral per-session HOME/tmp, and no env state (status,
        /// captures, manifest) is touched. Multiple `--readonly` sessions can run
        /// on one env at once (they take a shared lock); they still exclude a
        /// live read-write session. Requires isolation=process or supervised.
        #[arg(long)]
        readonly: bool,
        /// Command to run inside the box (after `--`); default: an interactive shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Manage the persistent user-level egress allowlist: extra hosts merged
    /// into every container-tier env whose profile already sets net.egress
    /// (a deny-all profile is never widened). Stored host-side under
    /// ~/.config/h5i/, outside every box-granted path; takes effect at the
    /// next `env run`/`env shell`. With no rule, lists the current entries.
    Allow {
        /// Host rule: exact `api.example.com`, wildcard `.example.com` /
        /// `*.example.com`, optionally with a `:port` suffix.
        rule: Option<String>,
        /// Remove the rule instead of adding it.
        #[arg(long)]
        remove: bool,
    },

    /// Probe what isolation this host can actually provide (Landlock, user
    /// namespaces, seccomp) and which claims are satisfiable.
    Probe,

    /// Machine-readable host enforcement report: isolation tier, egress-enforced
    /// yes/no, resource-limit support, and per-claim satisfiable/runnable — so a
    /// product can adapt to the real host without scraping `env probe` text.
    Capabilities {
        /// Emit the structured report as JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },

    /// List environments on this clone (the fleet view)
    List {
        /// Emit a machine-readable JSON array (manifest + base drift per env)
        #[arg(long)]
        json: bool,
    },

    /// Show one environment's status: lifecycle, enforced policy, evidence,
    /// and base drift
    Status {
        name: String,
        /// Emit the raw manifest JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Check one environment's enforcement readiness and structural health
    /// (can it actually enforce its isolation claim here? are its refs intact?)
    Doctor {
        name: String,
        /// Emit the structured report as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// List the secret grants an environment's policy declares, with a dry-run
    /// resolution status (never the value — only a fingerprint when resolvable)
    Secrets {
        name: String,
        /// Emit the status as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Manage long-lived services declared in the env's `.h5i/env.toml`
    /// (`[service.<name>]`), confined and pid-tracked — no daemon
    Service {
        #[command(subcommand)]
        action: EnvServiceCommands,
    },

    /// Show the per-env dynamic port map (services with a declared port)
    Ports {
        name: String,
        /// Emit the port map as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Rebase the environment onto its parent branch's current tip, re-pinning
    /// the base (use when `status` reports the parent has advanced)
    Rebase { name: String },

    /// Show the event log (refs/h5i/env) for one environment
    Log {
        name: String,
        /// Emit the event records as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Show the environment's reasoning/context branch
    /// (`refs/h5i/context/env/<agent>/<slug>`) — a convenience alias for
    /// `h5i context show --branch <that-branch>`.
    Context {
        name: String,
        /// Number of recent context commits to show (window K)
        #[arg(long, default_value_t = 3)]
        window: usize,
        /// Include the full reasoning trace (equivalent to --depth 3)
        #[arg(long)]
        trace: bool,
        /// Progressive disclosure depth: 1=compact index, 2=timeline (default), 3=full trace
        #[arg(long, default_value_t = 2)]
        depth: u8,
    },

    /// Diff the environment's work against its pinned base
    Diff {
        name: String,
        /// Show a diffstat instead of the full patch
        #[arg(long)]
        stat: bool,
        /// Output a machine-readable diffstat
        #[arg(long)]
        json: bool,
    },

    /// Inspect one of an environment's evidence captures (structured findings,
    /// exit code, policy digest, redactions)
    Inspect {
        name: String,
        /// Capture id (from `h5i env status`/`log`)
        #[arg(long)]
        capture: String,
        /// Emit the stored capture manifest as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Compare environments side by side — changes + latest run results (the
    /// "arena" reviewer comparison). Best on envs sharing one base.
    Compare {
        /// Two or more environment names
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Emit JSON instead of the table
        #[arg(long)]
        json: bool,
    },

    /// Snapshot the worktree (mediated commit, path-allowlist enforced) and
    /// mark the env proposed — produces a review brief. Never writes the
    /// parent branch.
    Propose { name: String },

    /// Apply a proposed environment onto its parent branch (reviewer-selected,
    /// never automatic). Default is a merge (fast-forward when possible).
    Apply {
        name: String,
        /// Squash the env's changes into a single commit instead of merging
        #[arg(long)]
        patch: bool,
    },

    /// Abort an environment — manifest and workspace preserved for forensics
    Abort { name: String },

    /// Permanently remove an environment from this clone: prune its worktree,
    /// delete its code + reasoning branches, and erase its manifest. Destroys
    /// local provenance (only the `removed` event in refs/h5i/env survives).
    /// A still-live env (created/running/proposed) needs --force.
    Rm {
        name: String,
        /// Remove even if the env is still live (not applied/aborted)
        #[arg(long)]
        force: bool,
    },

    /// Reclaim workspaces of applied/aborted environments (worktree prune).
    /// Manifests, branches, and captures are retained.
    Gc,
}

#[derive(Subcommand)]
pub enum EnvServiceCommands {
    /// Start a declared service as a confined background process
    Start {
        /// Environment name (slug, `agent/slug`, or full `env/agent/slug`)
        env: String,
        /// Service name (a `[service.<name>]` in the env's `.h5i/env.toml`)
        service: String,
    },
    /// Stop a running service (and capture its log as evidence)
    Stop { env: String, service: String },
    /// Show every recorded service for an env and its liveness
    Status {
        env: String,
        #[arg(long)]
        json: bool,
    },
    /// Tail a running service's log
    Logs {
        env: String,
        service: String,
        #[arg(long, default_value_t = 200)]
        tail: usize,
    },
}

pub fn run(action: EnvCommands) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let h5i_root = repo.h5i_root.clone();
            let git = repo.git();
            let workdir = git
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("h5i env requires a non-bare repository"))?
                .to_path_buf();

            // Surface environments pulled from other clones: materialize any
            // manifests/policies present in refs/h5i/env but absent (or older)
            // on disk, so `list`/`status`/`diff`/`apply` see them.
            // Sync the shared env roster to disk — but never in a sealed box,
            // where the host-owned env manifests are read-only (the write only
            // fails with EACCES and spams a warning). The box already has its
            // own env materialized; the shared roster is the host's concern.
            //
            // `env shell` is on the interactive hot path and operates on a single
            // named env that is almost always already materialized locally, so it
            // skips the eager sync and materializes lazily (only on a `find` miss)
            // below — trimming a `refs/h5i/env/meta` read + disk writes off every
            // shell start.
            let in_env_box = std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok();
            let lazy_materialize_env_ref = matches!(&action, EnvCommands::Shell { .. });
            if !in_env_box && !lazy_materialize_env_ref {
                if let Err(e) = h5i_core::env::materialize_from_ref(git, &h5i_root) {
                    eprintln!(
                        "{} could not sync shared env manifests: {e}",
                        style("warning:").yellow()
                    );
                }
            }

            match action {
                EnvCommands::Create {
                    name,
                    from,
                    pr,
                    remote,
                    profile,
                    isolation,
                    backend,
                    audit,
                } => {
                    let agent = msg::resolve_identity(&h5i_root, None)
                        .unwrap_or_else(|_| "human".to_string());
                    use h5i_core::sandbox::{IsolationClaim, IsolationRequest};
                    let isolation = match isolation.as_deref() {
                        None => None,
                        Some(s) if s.eq_ignore_ascii_case("auto") => Some(IsolationRequest::Auto),
                        Some(s) => Some(IsolationRequest::Claim(IsolationClaim::parse(s)?)),
                    };
                    // Did we auto-pick (vs. the user pinning a tier)? Used below to
                    // surface the container tier when the host lacks Podman.
                    let auto_picked = matches!(isolation, None | Some(IsolationRequest::Auto));
                    let profile_auto = profile.is_none();
                    // A PR base is resolved host-side BEFORE create: fetch the PR
                    // head, pin the local pr/<n> tracking branch, then create pins
                    // the immutable base from it like any other rev.
                    let pr_base = match &pr {
                        Some(spec) => Some(h5i_core::pr::resolve_pr_base(&workdir, spec, &remote)?),
                        None => None,
                    };
                    let opts = h5i_core::env::CreateOpts {
                        from: pr_base.as_ref().map(|b| b.oid.clone()).or(from),
                        profile,
                        isolation,
                        backend,
                        audit_capture: h5i_core::sandbox::AuditCapture::parse(&audit)?,
                        parent_branch: pr_base.as_ref().map(|b| b.local_branch.clone()),
                        pr: pr_base.as_ref().map(|b| b.number),
                        pr_head_ref: pr_base.as_ref().and_then(|b| b.head_ref.clone()),
                    };
                    let m = h5i_core::env::create(git, &h5i_root, &workdir, &agent, &name, opts)?;
                    println!(
                        "{} Created environment {} (isolation: {}, profile: {})",
                        SUCCESS,
                        style(&m.id).magenta().bold(),
                        style(&m.isolation_claim).cyan(),
                        m.profile
                    );
                    if let Some(b) = &pr_base {
                        println!(
                            "   pr       #{} head {} pinned to local branch {}{}",
                            b.number,
                            &b.oid[..12.min(b.oid.len())],
                            style(&b.local_branch).cyan(),
                            match b.cross_repo {
                                Some(true) => "  (cross-repo PR — push-back needs the fork remote)",
                                _ => "",
                            }
                        );
                    }
                    if profile_auto && m.profile == "default" {
                        println!(
                            "   {}      this host cannot enforce the built-in 'agent' profile (its \
                             API egress needs the supervised or container tier), so the fail-closed \
                             'default' was used — coding agents won't run in this box",
                            style("note").yellow()
                        );
                    }
                    println!(
                        "   base     {}  (from {})",
                        &m.base_commit[..12],
                        m.parent_branch
                    );
                    println!("   branch   {}", m.branch);
                    println!("   context  {}", m.context_branch);
                    println!("   work     {}", m.work_dir(&h5i_root).display());
                    // Discoverability: when we auto-picked a kernel tier and the host
                    // has no rootless Podman, tell the user the `container` tier
                    // (the one with a network egress allowlist) exists and what it
                    // needs — otherwise they would never learn Podman unlocks it.
                    if auto_picked
                        && matches!(
                            m.isolation_claim.as_str(),
                            "workspace" | "process" | "supervised"
                        )
                        && !h5i_core::sandbox::podman_present()
                    {
                        println!(
                            "   {}      the 'container' tier (adds a network egress allowlist) needs \
                             rootless Podman, which isn't installed — install it, then set \
                             container.image in .h5i/env.toml. See: h5i env probe",
                            style("tip").yellow()
                        );
                    }
                    println!(
                        "   next     h5i env run {} -- <cmd>   ·   h5i env shell {}   ·   h5i env propose {}",
                        m.slug, m.slug, m.slug
                    );
                }

                EnvCommands::Run { name, command } => {
                    if command.is_empty() {
                        anyhow::bail!("usage: h5i env run <name> -- <command> [args…]");
                    }
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let outcome = h5i_core::env::run(git, &h5i_root, &mut m, &command)?;
                    match &outcome.manifest.structured {
                        Some(s) => println!("{}", h5i_core::structured::render_compact(s)),
                        None => println!("{}", outcome.manifest.summary),
                    }
                    if outcome.timed_out {
                        eprintln!(
                            "{} run killed by the policy wall-clock limit",
                            style("warning:").yellow().bold()
                        );
                    }
                    let rss = outcome
                        .max_rss_kb
                        .map(|kb| format!(", rss {}MiB", kb / 1024))
                        .unwrap_or_default();
                    eprintln!(
                        "{} evidence {} (env {}, policy {}) · wall {}ms, cpu {}ms{}",
                        LOOKING,
                        style(&outcome.capture_id).magenta(),
                        m.id,
                        &m.policy_digest[..12],
                        outcome.wall_ms,
                        outcome.cpu_ms,
                        rss
                    );
                    // A wall-clock kill is a failure, not success — the child
                    // was SIGKILLed so it has no exit code of its own. Use the
                    // conventional timeout code (124, as coreutils `timeout`).
                    if outcome.timed_out {
                        std::process::exit(124);
                    }
                    // Transparent wrapper: pass the child's exit code through.
                    // A None code means the child died on a signal — surface it
                    // as a generic failure rather than a silent success.
                    match outcome.exit_code {
                        Some(0) => {}
                        Some(code) => std::process::exit(code),
                        None => std::process::exit(1),
                    }
                }

                EnvCommands::Shell {
                    name,
                    readonly,
                    command,
                } => {
                    // Lazy materialize: the eager shared-roster sync is skipped
                    // for `shell` (above). The common case is a local env that
                    // `find` resolves straight away; only on a miss do we pay the
                    // shared-ref sync and retry (never inside a sealed box, whose
                    // manifests are host-owned + read-only).
                    let mut m = match h5i_core::env::find(&h5i_root, &name) {
                        Ok(m) => m,
                        Err(e) if !in_env_box => {
                            if let Err(sync_err) =
                                h5i_core::env::materialize_from_ref(git, &h5i_root)
                            {
                                eprintln!(
                                    "{} could not sync shared env manifests: {sync_err}",
                                    style("warning:").yellow()
                                );
                                return Err(e.into());
                            }
                            h5i_core::env::find(&h5i_root, &name)?
                        }
                        Err(e) => return Err(e.into()),
                    };
                    // An empty `command` means "default interactive shell";
                    // `env::shell` builds the argv (host bashrc is replaced with a
                    // generated plain rc by default — see `default_shell_argv`).
                    eprintln!(
                        "{} entering {} (isolation: {}, profile: {}{}) — confined session; exit to return",
                        LOOKING,
                        style(&m.id).magenta(),
                        style(&m.isolation_claim).cyan(),
                        style(&m.profile).cyan(),
                        if readonly {
                            style(", read-only observer").yellow().to_string()
                        } else {
                            String::new()
                        }
                    );
                    if !h5i_core::sandbox::is_agent_profile(&m.profile) {
                        eprintln!(
                            "   note: this profile has no agent grants — claude/codex won't run \
                             here (envs default to --profile agent where the host supports it)"
                        );
                    }
                    let code = h5i_core::env::shell(git, &h5i_root, &mut m, &command, readonly)?;
                    match code {
                        0 => {}
                        c => std::process::exit(c),
                    }
                }

                EnvCommands::Allow { rule, remove } => match rule {
                    None => {
                        let rules = h5i_core::env::user_allow_list();
                        match h5i_core::env::user_allow_path() {
                            Some(path) => println!("── user egress allowlist ({}) ──", path.display()),
                            None => println!("── user egress allowlist ──"),
                        }
                        if rules.is_empty() {
                            println!("  (empty — add one with `h5i env allow <host>`)");
                        }
                        for r in &rules {
                            println!("  {r}");
                        }
                        println!(
                            "  applies to container-tier envs whose profile sets net.egress; \
                             takes effect at the next env run/shell"
                        );
                    }
                    Some(raw) => {
                        if remove {
                            let (removed, path) = h5i_core::env::user_allow_remove(&raw)?;
                            if removed {
                                println!("✔  removed '{}' from {}", raw.trim(), path.display());
                            } else {
                                println!("   '{}' was not in {}", raw.trim(), path.display());
                            }
                        } else {
                            let (added, path) = h5i_core::env::user_allow_add(&raw)?;
                            if added {
                                println!("✔  allowed '{}' ({})", raw.trim(), path.display());
                                println!(
                                    "   merged into container-tier envs whose profile sets \
                                     net.egress, from the next env run/shell on"
                                );
                            } else {
                                println!("   '{}' already allowed ({})", raw.trim(), path.display());
                            }
                        }
                    }
                },

                EnvCommands::Probe => {
                    let caps = h5i_core::sandbox::probe_host();
                    println!("── Host isolation capabilities ──");
                    println!("  os           = {}", caps.os);
                    println!(
                        "  landlock_abi = {}",
                        caps.landlock_abi
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "none".into())
                    );
                    println!("  userns       = {}", caps.userns);
                    println!("  seccomp      = {}", caps.seccomp);
                    println!(
                        "  container    = {}",
                        caps.container_runtime.as_deref().unwrap_or("none")
                    );
                    println!();
                    for (claim, profile_net_deny) in [
                        (h5i_core::sandbox::IsolationClaim::Workspace, false),
                        (h5i_core::sandbox::IsolationClaim::Process, true),
                    ] {
                        let mut p = h5i_core::sandbox::Profile::builtin("probe", claim);
                        if !profile_net_deny {
                            p.net_mode = h5i_core::sandbox::NetMode::Host;
                        }
                        let ok = h5i_core::sandbox::resolve(&p, &caps).is_ok();
                        println!(
                            "  claim {:<10} satisfiable = {}",
                            claim.as_str(),
                            if ok {
                                style("yes").green()
                            } else {
                                style("no").red()
                            }
                        );
                    }
                    // Container claim: needs rootless Podman + a profile image;
                    // show whether the runtime half is satisfiable.
                    let container_ok = caps.container_runtime.is_some();
                    println!(
                        "  claim {:<10} satisfiable = {} (needs rootless Podman + profile container.image)",
                        "container",
                        if container_ok { style("yes").green() } else { style("no").red() }
                    );
                    println!(
                        "  claim hardened-container/microvm: external backends (not in this build)"
                    );
                    // Functional self-test: bits can be present while a hardened
                    // kernel still denies exec under the full confinement stack.
                    let probe = h5i_core::sandbox::Profile::builtin(
                        "probe",
                        h5i_core::sandbox::IsolationClaim::Process,
                    );
                    match h5i_core::sandbox::resolve(&probe, &caps)
                        .and_then(|pol| h5i_core::sandbox::verify_exec(&pol))
                    {
                        Ok(()) => println!("  process tier runnable = {}", style("yes").green()),
                        Err(e) => println!("  process tier runnable = {} ({e})", style("no").red()),
                    }
                }

                EnvCommands::Capabilities { json } => {
                    let report = h5i_core::sandbox::capabilities_report();
                    if json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        let yn = |b: bool| {
                            if b {
                                style("yes").green()
                            } else {
                                style("no").red()
                            }
                        };
                        println!("── h5i host capabilities ──");
                        println!("  os               = {}", report.os);
                        println!(
                            "  landlock_abi     = {}",
                            report
                                .landlock_abi
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "none".into())
                        );
                        println!("  userns           = {}", yn(report.userns));
                        println!("  seccomp          = {}", yn(report.seccomp));
                        println!(
                            "  container        = {}",
                            report.container_runtime.as_deref().unwrap_or("none")
                        );
                        println!("  egress_enforced  = {}", yn(report.egress_enforced));
                        println!("  resource_limits  = {}", yn(report.resource_limits));
                        println!(
                            "  strongest_tier   = {}",
                            style(report.strongest_tier).cyan().bold()
                        );
                        println!();
                        for c in &report.claims {
                            let runnable = match c.runnable {
                                Some(true) => format!(" runnable = {}", style("yes").green()),
                                Some(false) => format!(" runnable = {}", style("no").red()),
                                None => String::new(),
                            };
                            let note = c
                                .note
                                .map(|n| format!("  {}", style(format!("({n})")).dim()))
                                .unwrap_or_default();
                            println!(
                                "  claim {:<18} satisfiable = {}{}{}",
                                c.claim,
                                yn(c.satisfiable),
                                runnable,
                                note
                            );
                        }
                    }
                }

                EnvCommands::List { json } => {
                    let envs = h5i_core::env::list(&h5i_root);
                    if json {
                        let rows: Vec<serde_json::Value> = envs
                            .iter()
                            .map(|m| {
                                let mut v = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
                                if let serde_json::Value::Object(ref mut map) = v {
                                    let d = h5i_core::env::drift(git, m);
                                    map.insert("drift".into(), serde_json::to_value(&d).unwrap_or(serde_json::Value::Null));
                                    // Live sessions (the pid registry) — runtime
                                    // state, so injected like drift rather than
                                    // stored in the manifest.
                                    let live = h5i_core::env::live_sessions(&m.dir(&h5i_root));
                                    map.insert("live".into(), serde_json::to_value(&live).unwrap_or(serde_json::Value::Null));
                                }
                                v
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        if envs.is_empty() {
                            println!("No environments. Create one: h5i env create <name>");
                        }
                        for m in envs {
                            let d = h5i_core::env::drift(git, &m);
                            let drift_mark = if d.is_current() { "" } else { " ⚠drift" };
                            let live = h5i_core::env::live_sessions(&m.dir(&h5i_root));
                            let live_mark = match live
                                .iter()
                                .find(|s| h5i_core::env::live_is_writer(&s.kind))
                            {
                                Some(s) => format!(" ●{} pid {}", s.kind, s.pid),
                                None if m.status == "running" => " ⚠stale".to_string(),
                                None if !live.is_empty() => {
                                    format!(" ◦{} observer(s)", live.len())
                                }
                                None => String::new(),
                            };
                            println!(
                                "{:<28} {:<9} isolation={:<10} base={} captures={}{}{}",
                                style(&m.id).magenta(),
                                m.status,
                                m.isolation_claim,
                                &m.base_commit[..12],
                                m.captures.len(),
                                style(drift_mark).yellow(),
                                style(&live_mark).green()
                            );
                        }
                    }
                }

                EnvCommands::Status { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&m)?);
                    } else {
                        print!("{}", h5i_core::env::status_report(git, &h5i_root, &m));
                    }
                }

                EnvCommands::Doctor { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let report = h5i_core::env::doctor(git, &h5i_root, &m);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print!("{}", h5i_core::env::render_doctor(&report));
                    }
                    if !report.healthy {
                        std::process::exit(1);
                    }
                }

                EnvCommands::Secrets { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let policy = h5i_core::env::load_policy(&h5i_root, &m)?;
                    let rows = h5i_core::env::secrets_status(&policy);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        print!("{}", h5i_core::env::render_secrets(&m.id, &rows));
                    }
                }

                EnvCommands::Service { action } => match action {
                    EnvServiceCommands::Start { env, service } => {
                        let m = h5i_core::env::find(&h5i_root, &env)?;
                        let rec = h5i_core::env::service_start(git, &h5i_root, &m, &service)?;
                        let port = rec
                            .dynamic_port
                            .map(|p| {
                                format!(
                                    " (injected PORT={p}; reachable at http://127.0.0.1:{p} if it binds the port)"
                                )
                            })
                            .unwrap_or_default();
                        println!(
                            "{} service {} started (pid {}){}",
                            SUCCESS, rec.name, rec.pid, port
                        );
                    }
                    EnvServiceCommands::Stop { env, service } => {
                        let m = h5i_core::env::find(&h5i_root, &env)?;
                        let cap = h5i_core::env::service_stop(git, &h5i_root, &m, &service)?;
                        match cap {
                            Some(id) => println!(
                                "{} service {} stopped (log captured: {})",
                                SUCCESS, service, id
                            ),
                            None => println!("{} service {} stopped", SUCCESS, service),
                        }
                    }
                    EnvServiceCommands::Status { env, json } => {
                        let m = h5i_core::env::find(&h5i_root, &env)?;
                        let rows = h5i_core::env::service_status(&h5i_root, &m);
                        if json {
                            println!("{}", serde_json::to_string_pretty(&rows)?);
                        } else {
                            print!("{}", h5i_core::env::render_services(&m.id, &rows));
                        }
                    }
                    EnvServiceCommands::Logs { env, service, tail } => {
                        let m = h5i_core::env::find(&h5i_root, &env)?;
                        println!("{}", h5i_core::env::service_logs(&h5i_root, &m, &service, tail)?);
                    }
                },

                EnvCommands::Ports { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let rows = h5i_core::env::service_status(&h5i_root, &m);
                    if json {
                        let ports: Vec<_> = rows
                            .iter()
                            .filter(|s| s.record.dynamic_port.is_some())
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&ports)?);
                    } else {
                        print!("{}", h5i_core::env::render_ports(&m.id, &rows));
                    }
                }

                EnvCommands::Rebase { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let msg_out = h5i_core::env::rebase(git, &h5i_root, &mut m)?;
                    println!("{} {}", SUCCESS, msg_out);
                }

                EnvCommands::Log { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let events = h5i_core::env::read_events(git, Some(&m.id));
                    if json {
                        println!("{}", serde_json::to_string_pretty(&events)?);
                    } else {
                        for e in events {
                            println!(
                                "{}  {:<9} {}{}",
                                e.ts,
                                e.event,
                                e.detail.unwrap_or_default(),
                                e.capture
                                    .map(|c| format!("  [capture {c}]"))
                                    .unwrap_or_default()
                            );
                        }
                    }
                }

                EnvCommands::Context {
                    name,
                    window,
                    trace,
                    depth,
                } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    // --trace is shorthand for --depth 3 (mirrors `context show`).
                    let effective_depth = if trace { 3 } else { depth };
                    let opts = ctx::ContextOpts {
                        branch: Some(m.context_branch.clone()),
                        commit_hash: None,
                        show_log: effective_depth >= 3,
                        log_offset: 0,
                        metadata_segment: None,
                        window,
                        depth: effective_depth,
                    };
                    let snapshot = ctx::gcc_context(&workdir, &opts)?;
                    ctx::print_context_depth(&snapshot, effective_depth);
                }

                EnvCommands::Diff { name, stat, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        let report = h5i_core::env::diffstat_report(git, &h5i_root, &m)?;
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print!("{}", h5i_core::env::diff(git, &h5i_root, &m, stat)?);
                    }
                }

                EnvCommands::Inspect {
                    name,
                    capture,
                    json,
                } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        let manifest = h5i_core::env::inspect_manifest(git, &m, &capture)?;
                        println!("{}", serde_json::to_string_pretty(&manifest)?);
                    } else {
                        print!("{}", h5i_core::env::inspect(git, &m, &capture)?);
                    }
                }

                EnvCommands::Compare { names, json } => {
                    let rows = h5i_core::env::compare(git, &h5i_root, &names)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        print!("{}", h5i_core::env::render_compare(&rows));
                    }
                }

                EnvCommands::Propose { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let brief = h5i_core::env::propose(git, &h5i_root, &mut m)?;
                    println!("{brief}");
                }

                EnvCommands::Apply { name, patch } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let msg_out = h5i_core::env::apply(git, &h5i_root, &workdir, &mut m, patch)?;
                    println!("{} {}", SUCCESS, msg_out);
                    // A PR env applied onto its local pr/<n> branch: tell the
                    // reviewer exactly how to send the result back to the PR.
                    if let Some(n) = m.pr {
                        match (&m.pr_head_ref, m.parent_branch.as_str()) {
                            (Some(head), local) => println!(
                                "   push back to PR #{n}:  git push origin {local}:{head}"
                            ),
                            (None, local) => println!(
                                "   push back to PR #{n}:  git push origin {local}:<pr-head-branch> \
                                 (see `gh pr view {n} --json headRefName`; a fork PR needs the fork \
                                 remote instead of origin)"
                            ),
                        }
                    }
                }

                EnvCommands::Abort { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    h5i_core::env::abort(git, &h5i_root, &mut m)?;
                    println!(
                        "{} {} aborted (manifest preserved for forensics)",
                        SUCCESS, m.id
                    );
                }

                EnvCommands::Rm { name, force } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    h5i_core::env::rm(git, &h5i_root, &m, force)?;
                    println!(
                        "{} {} removed (workspace, branches, and manifest erased)",
                        SUCCESS, m.id
                    );
                }

                EnvCommands::Gc => {
                    let reclaimed = h5i_core::env::gc(git, &h5i_root)?;
                    if reclaimed.is_empty() {
                        println!("Nothing to reclaim (only applied/aborted envs are gc'd).");
                    } else {
                        for id in reclaimed {
                            println!("{} reclaimed workspace of {}", SUCCESS, id);
                        }
                    }
                }
            }
        }
    Ok(())
}
