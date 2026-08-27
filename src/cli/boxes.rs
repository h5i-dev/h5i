//! `h5i box` — the box lifecycle: create, run, shell, export, remove.
//!
//! `h5i env <verb>` is the same enum under the old noun, kept hidden for one
//! release so existing scripts and muscle memory keep working.
use clap::Subcommand;
use console::style;

use h5i_core::ui::{LOOKING, SUCCESS};

#[derive(Subcommand)]
// `Create` carries every create-time flag and dwarfs the other verbs; boxing
// it would break clap's derive, and the enum is constructed once per process.
// Same trade as `Commands` in main.rs.
#[allow(clippy::large_enum_variant)]
pub enum BoxCommands {
    /// Create a box explicitly. `h5i box [SOURCE]` is the short form and
    /// takes the same flags; this is what it dispatches to.
    ///
    /// The box is a git worktree under .git/.h5i/env/ on its own code branch,
    /// confined by a pinned, fail-closed policy. The base revision is frozen
    /// at creation.
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
        /// Copy an external repository into the box instead of taking a
        /// worktree of this one. The box is **detached**: this repository is
        /// never touched, and `h5i box export` is the only way out.
        #[arg(long, conflicts_with_all = ["from", "pr"], value_name = "URL")]
        clone: Option<String>,
        /// Start from an empty box (a fresh repository with one empty commit),
        /// for letting an agent build something from nothing. Also detached.
        #[arg(long, conflicts_with_all = ["from", "pr", "clone"])]
        new: bool,
        /// Remote to fetch the PR head from (with --pr).
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Policy profile from .h5i/env.toml. Built-ins need no file: `agent`
        /// (agent-in-box, scoped to $H5I_AGENT's runtime), `agent-claude` /
        /// `agent-codex` (pin one runtime: only that agent's HOME state + API
        /// egress), `browser` (the agent profile plus headless Chrome and the
        /// agent-browser daemon, so the agent can drive a real browser against
        /// the dev server it just started), and `default` (fail-closed
        /// build/test confinement). Unset auto-picks the creating runtime's
        /// agent profile when this host can enforce it, else `default`.
        #[arg(long)]
        profile: Option<String>,
        /// Isolation: auto (default) | workspace | process | supervised | container | microvm.
        /// `auto` (or unset) picks the strongest tier the host can run; an explicit
        /// tier fails closed if the host cannot satisfy it (never silently downgrades).
        /// `microvm` boots a hardware-isolated guest via microsandbox (`msb`) and is
        /// the only tier whose net.egress allowlist is enforced by address rather
        /// than by an HTTP proxy; it needs an image and host virtualization.
        #[arg(long)]
        isolation: Option<String>,
        /// Base image for isolation=container and isolation=microvm (pre-pulled;
        /// runs never pull). Overrides the profile's `container.image` and the
        /// repo-level `[container] image` default in .h5i/env.toml. Passing it
        /// makes both image-backed tiers candidates for the isolation auto-pick.
        #[arg(long)]
        image: Option<String>,

        /// Browser engine for the `browser` profile: chromium (default),
        /// lightpanda, or h5i. Overrides the profile's `engine`. Pinned
        /// in the policy digest, and never falls back: a box whose engine
        /// cannot serve a page fails and names the recreate, because switching
        /// engine changes what a page is able to do.
        #[arg(long)]
        engine: Option<String>,
        /// Run this box on a paired runner instead of this machine.
        ///
        /// The source is copied across as a git bundle and the box is built
        /// there; the repository, the policy and the credentials stay here.
        /// The box is bound to that machine's host key, not to its name, so a
        /// name re-pointed at other hardware never silently moves a box.
        ///
        /// `h5i runner pair` sets one up and `h5i runner probe` says what it
        /// can do. A tier the runner does not offer is refused with the
        /// capability named, never quietly swapped for a weaker one.
        #[cfg(feature = "runner")]
        #[arg(long, value_name = "NAME")]
        runner: Option<String>,
        /// Workspace backend (auto|worktree)
        #[arg(long, default_value = "auto")]
        backend: String,
        /// Audit capture mode for wrapped in-env commands: signal (default) | all.
        /// `all` records every wrapped command, including small successful output.
        #[arg(long, default_value = "signal")]
        audit: String,
        /// Emit the created box's manifest as JSON (the `status --json` shape,
        /// plus the workspace path) instead of the human summary. Notes still
        /// go to stderr, so stdout stays parseable.
        #[arg(long)]
        json: bool,
    },

    /// Run one command inside a box, policy-enforced: the exit code passes
    /// through and the run is recorded as a receipt tagged with the box id and
    /// the policy digest that was actually enforced.
    Run {
        /// Environment name (slug, `agent/slug`, or full `env/agent/slug`)
        name: String,
        /// Emit a JSON envelope on stdout (receipt, exit code, timing, the
        /// recorded output) instead of streaming the raw output. Put the flag
        /// before the name: everything after `--` belongs to the command. The
        /// exit code still passes through.
        #[arg(long)]
        json: bool,
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
    /// into every container- or microvm-tier env whose profile already sets
    /// net.egress (a deny-all profile is never widened). Stored host-side under
    /// ~/.config/h5i/, outside every box-granted path; takes effect at the
    /// next `env run`/`env shell`. With no rule, lists the current entries.
    Allow {
        /// Host rule: exact `api.example.com`, wildcard `.example.com` /
        /// `*.example.com`, optionally with a `:port` suffix.
        rule: Option<String>,
        /// Remove the rule instead of adding it.
        #[arg(long)]
        remove: bool,
        /// Emit the current allowlist as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Probe what isolation this host can actually provide (Landlock, user
    /// namespaces, seccomp) and which claims are satisfiable.
    Probe,

    /// Runtime detection: what the kernel saw inside a box.
    ///
    /// A second observer of the same run, in a lane the box cannot reach: an
    /// eBPF collector reports `execve`, `connect` and `openat` whether or not
    /// anything in the box wanted them reported. Observation only — nothing
    /// here denies anything — and off unless a profile's `[detect]` section
    /// turns it on. `h5i box detect probe` says whether this machine can.
    Detect {
        #[command(subcommand)]
        action: crate::cli::detect::DetectCommands,
    },

    /// Machine-readable host enforcement report: isolation tier, egress-enforced
    /// yes/no, resource-limit support, and per-claim satisfiable/runnable — so a
    /// product can adapt to the real host without scraping `env probe` text.
    Capabilities {
        /// Emit the structured report as JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },

    /// List environments on this clone (the fleet view)
    #[command(visible_alias = "ls")]
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

    /// Watch this box's browser from the host, and take over when you want to.
    ///
    /// Starts a loopback-only forward that h5i owns: the box's stream port is
    /// never published, every connection carries the box's own token,
    /// cross-origin handshakes are refused, and your input reaches the page only
    /// while you hold the control lock (`h5i browser take`). Runs until Ctrl-C.
    ///
    /// With `--term`, the page is drawn in this terminal instead, and nothing is
    /// bound or served at all: no port, no token, and no host browser. Needs a
    /// terminal that speaks the Kitty graphics protocol.
    View {
        name: String,
        /// Loopback port to bind. 0 picks a free one and prints it.
        #[arg(long, default_value_t = 7331)]
        port: u16,
        /// Draw the page in this terminal instead of serving it to a browser
        #[arg(long)]
        term: bool,
        /// Frame-rate ceiling asked of the box, for `--term`
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=60))]
        fps: u32,
        /// Skip the graphics probe and render anyway, for `--term`. Frames are
        /// then sent uncompressed (about six times the bytes), because the
        /// probe is also what asks whether this terminal accepts deflated ones
        #[arg(long)]
        assume_graphics: bool,
    },

    /// Let one other person try this box's web app, from their own machine.
    ///
    // The middle sentence is the one that differs, because the guarantee
    // differs. Saying "h5i enters the box's network namespace" on a platform
    // with no namespaces is not a small inaccuracy: it is the sentence a reader
    // uses to decide how exposed the port is, and on macOS it claims an
    // isolation that is exactly what macOS does not have. The short summary
    // above — the only part that reaches the man page and the published manual,
    // both generated on Linux — is deliberately left alone.
    #[cfg_attr(
        not(target_os = "macos"),
        doc = "The box's port is never published. h5i enters the box's network namespace, dials \
               the dev server from inside, and carries the traffic"
    )]
    #[cfg_attr(
        target_os = "macos",
        doc = "h5i never publishes the box's port — but on macOS a box has no network of its \
               own, so its dev server binds this machine's loopback and anything local can \
               already reach it. What h5i promises here is that it shares *that box's* server \
               and nobody else's: it asks which process holds the port, refuses unless that \
               process belongs to this box, and re-checks on every connection. It carries the \
               traffic"
    )]
    /// either peer to peer (they run `h5i join`, end-to-end encrypted) or
    /// through a Cloudflare quick tunnel (`--tunnel`: any browser, no h5i, but
    /// Cloudflare terminates TLS). Either way the invite is a capability with an
    /// expiry that you can revoke, and the session lands in the box's receipt.
    ///
    /// `h5i box share <name>` starts one; the verbs manage it. Runs until Ctrl-C.
    #[cfg(feature = "share-tunnel")]
    Share(crate::cli::share::ShareArgs),

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
        /// Show only the newest N events (0 shows the full log)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Emit the event records as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },

    /// Stream this box's policy decisions, one line each, as they happen
    ///
    /// The tail of the receipt, not a viewer: no viewport, no panes, no
    /// control lock. Every row names the lane that observed it and the grade
    /// of that evidence, because a row that does not say who saw it asserts
    /// more than h5i knows. Runs until Ctrl-C.
    Watch {
        name: String,
        /// Print only refusals: a denied request, the verdict that refused it,
        /// and any agent action the mediator turned away
        #[arg(long)]
        deny_only: bool,
        /// Emit one JSON object per line instead of the human view
        #[arg(long)]
        json: bool,
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
        /// Capture id (from `h5i box status`/`log`)
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

    /// The output gate: freeze the box and write an inspectable bundle
    /// (patch.diff, report.md, receipt.json) for a human to review and apply.
    /// Nothing reaches the host repository without that human step.
    Export {
        name: String,
        /// Where to write the bundle (default: ./h5i-export/<name>).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Replace a non-empty output directory.
        #[arg(long)]
        force: bool,
        /// Emit the export summary as JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },

    /// Snapshot the worktree (mediated commit, path-allowlist enforced) and
    /// mark the box proposed — produces a review brief. Never writes the
    /// parent branch. `export` runs this as its freeze step.
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

    /// Warm dependency caches: one per project and ecosystem, keyed by that
    /// ecosystem's lockfiles, mounted read-only into a box.
    Cache {
        #[command(subcommand)]
        action: CacheCommands,
    },

    /// Permanently remove one or more environments from this clone: prune
    /// their worktrees, delete their code + reasoning branches, and erase
    /// their manifests. Destroys local provenance (only the `removed` event
    /// in refs/h5i/env survives). A still-live env (created/running/proposed)
    /// needs --force. Multiple names are processed in order; a failure for one
    /// env is reported but does not abort the rest.
    Rm {
        /// One or more environment names (slug, `agent/slug`, or full `env/agent/slug`)
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Remove even if the env is still live (not applied/aborted).
        /// Applies to every listed env.
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

/// Who is creating this env. `$H5I_AGENT` is injected per host (Claude Code
/// sets `claude`, Codex sets `codex`); a human on a bare shell gets `human`.
/// The identity scopes the env's branch namespace and the agent-in-box profile.
///
/// Unset is `human` and says nothing — that is the normal bare-shell case. A
/// value that is present but unusable warns instead of falling through in
/// silence, so a typo'd identity does not quietly scatter a run's envs into the
/// `human` namespace, where the agent then fails to find them by their own name.
fn agent_identity() -> String {
    match std::env::var("H5I_AGENT") {
        Err(std::env::VarError::NotPresent) => "human".to_string(),
        Ok(value) => {
            let identity = value.trim();
            if !identity.is_empty()
                && identity.len() <= 64
                && identity
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                identity.to_string()
            } else {
                warn_invalid_agent_identity()
            }
        }
        Err(std::env::VarError::NotUnicode(_)) => warn_invalid_agent_identity(),
    }
}

/// The stderr line for a rejected `$H5I_AGENT`, returning the `human` fallback
/// it announces. The offending value is deliberately not echoed: it is ambient
/// input of unknown provenance, and a raw `\e[` in it would repaint the
/// terminal. The rule is stated instead, which is what the reader needs anyway.
fn warn_invalid_agent_identity() -> String {
    eprintln!(
        "{} invalid H5I_AGENT; expected 1-64 ASCII letters, digits, hyphens, or underscores; using 'human'",
        style("warning:").yellow()
    );
    "human".to_string()
}

/// Is `source` a pull request spec (a number, `#number`, or a GitHub PR URL)?
/// Returns the spec unchanged so the create path resolves it once, in core.
pub fn pr_spec(source: &str) -> Option<String> {
    h5i_core::source::parse_pr_spec(source)
        .ok()
        .map(|_| source.to_string())
}

/// Does `source` look like a repository URL we should clone into a box?
/// Deliberately narrow: a scheme we recognize, or `user@host:path` (scp-style).
/// Anything else is refused rather than guessed at.
pub fn looks_like_repo_url(source: &str) -> bool {
    const SCHEMES: &[&str] = &[
        "https://",
        "http://",
        "git://",
        "ssh://",
        "git+ssh://",
        // `file://` is how you point a box at a local repository without
        // mounting anything: git copies the objects in, the box gets its own.
        "file://",
    ];
    if SCHEMES.iter().any(|p| source.starts_with(p)) {
        return true;
    }
    // scp-style `git@github.com:owner/repo.git`, but not a Windows path or a
    // bare `foo:bar` word.
    match source.split_once(':') {
        Some((host, path)) => {
            host.contains('@') && host.contains('.') && !path.is_empty() && !path.starts_with('/')
        }
        None => false,
    }
}

/// What the box may reach, short enough for the terminal viewer's status line.
///
/// An empty allowlist reads as "localhost", not as "none": loopback is always
/// open — it is how the dev server is reachable at all — so a box with no
/// egress entries can still talk to itself, and saying "none" would overstate
/// the confinement in the one place a human is looking for a quick answer.
pub fn egress_summary(hosts: &[String]) -> String {
    match hosts {
        [] => "localhost".into(),
        // Two fit; beyond that the count is more useful than a truncated list,
        // and `h5i box status` prints them in full.
        [one] => one.clone(),
        [a, b] => format!("{a},{b}"),
        many => format!("{} hosts", many.len()),
    }
}

/// Slugify a repository URL into a box name: the last path segment, minus
/// `.git`.
pub fn name_from_url(url: &str) -> anyhow::Result<String> {
    let tail = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url);
    let base = slugify(tail.strip_suffix(".git").unwrap_or(tail));
    free_box_name(if base.is_empty() { "clone" } else { &base })
}

/// Name for a box created from the current repository: the checked-out branch,
/// slugified, with a numeric suffix when that name is taken. Deterministic
/// enough to predict, unique enough to keep two boxes off one name.
pub fn default_box_name() -> anyhow::Result<String> {
    let repo = super::discover_repo("h5i box")?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_owned))
        .unwrap_or_else(|| "box".into());
    free_box_name(&slugify(&branch))
}

fn slugify(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    out.trim_matches('-').to_string()
}

/// `base`, or `base-2`, `base-3`, … — the first name no box has taken.
pub fn free_box_name(base: &str) -> anyhow::Result<String> {
    let repo = super::discover_repo("h5i box")?;
    let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;
    let base = if base.is_empty() { "box" } else { base };
    let taken: std::collections::HashSet<String> = h5i_core::env::list(&h5i_root)
        .into_iter()
        .map(|m| m.slug)
        .collect();
    if !taken.contains(base) {
        return Ok(base.to_string());
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not derive a free box name from '{base}' — pass --name")
}

#[derive(Subcommand)]
pub enum CacheCommands {
    /// List the caches on this clone.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },

    /// Show which caches a box created right now would be given, and where
    /// they would appear inside it.
    Mounts {
        #[arg(long)]
        json: bool,
    },

    /// Populate a cache by fetching this project's dependencies.
    Refresh {
        /// Ecosystem to refresh (cargo, npm, uv, go).
        eco: String,
    },

    /// Remove a cache. Without --key, removes every cache for the ecosystem.
    Rm {
        eco: String,
        #[arg(long)]
        key: Option<String>,
    },
}

fn run_cache(
    action: CacheCommands,
    repo_handle: &git2::Repository,
    h5i_root: &std::path::Path,
    workdir: &std::path::Path,
) -> anyhow::Result<()> {
    match action {
        CacheCommands::List { json } => {
            let entries = h5i_core::cache::list(h5i_root, workdir);
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
                return Ok(());
            }
            if entries.is_empty() {
                println!("no caches yet — `h5i box cache refresh <ecosystem>` builds one");
                return Ok(());
            }
            for e in entries {
                println!(
                    "  {:<8} {:<18} {:>10}  {}",
                    e.ecosystem,
                    e.key,
                    h5i_core::cache::human_bytes(e.bytes),
                    if e.current {
                        style("current").green().to_string()
                    } else {
                        style("stale (lockfiles moved on)").yellow().to_string()
                    }
                );
            }
        }
        CacheCommands::Mounts { json } => {
            let mounts = h5i_core::cache::mounts_for(h5i_root, workdir);
            if json {
                let rows: Vec<serde_json::Value> = mounts
                    .iter()
                    .map(|(host, target)| {
                        serde_json::json!({
                            "host": host.display().to_string(),
                            "box": target,
                            "mode": "ro",
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            if mounts.is_empty() {
                println!("no cache matches this project's lockfiles — a box would start cold");
                return Ok(());
            }
            for (host, target) in mounts {
                println!("  {} → {} (read-only)", host.display(), target);
            }
        }
        CacheCommands::Refresh { eco } => {
            let eco = h5i_core::cache::ecosystem(&eco).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown ecosystem '{eco}' — known: {}",
                    h5i_core::cache::ECOSYSTEMS
                        .iter()
                        .map(|e| e.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            let (dir, exit) = h5i_core::cache::refresh(repo_handle, h5i_root, workdir, eco)?;
            if exit == 0 {
                println!(
                    "{} refreshed the {} cache → {}",
                    SUCCESS,
                    eco.name,
                    style(dir.display()).cyan()
                );
            } else {
                anyhow::bail!(
                    "`{}` exited {exit} in the refresh box — the cache at {} may be incomplete. \
                     A half-populated cache that claims success is worse than a cold one, so \
                     this is a failure.",
                    eco.fetch.join(" "),
                    dir.display()
                );
            }
        }

        CacheCommands::Rm { eco, key } => {
            let eco = h5i_core::cache::ecosystem(&eco)
                .ok_or_else(|| anyhow::anyhow!("unknown ecosystem '{eco}'"))?;
            let n = h5i_core::cache::remove(h5i_root, eco, key.as_deref())?;
            println!("{} removed {} cache(s) for {}", SUCCESS, n, eco.name);
        }
    }
    Ok(())
}

pub fn run(action: BoxCommands) -> anyhow::Result<()> {
    {
            let git_repo = super::discover_repo("h5i box")?;
            let h5i_root = h5i_core::storage::h5i_root_for_repo(&git_repo)?;
            h5i_core::storage::ensure_layout(&h5i_root)?;
            let git = &git_repo;
            let workdir = git
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("h5i box requires a non-bare repository"))?
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
            let lazy_materialize_env_ref = matches!(&action, BoxCommands::Shell { .. });
            if !in_env_box && !lazy_materialize_env_ref
                && let Err(e) = h5i_core::env::materialize_from_ref(git, &h5i_root)
            {
                eprintln!(
                    "{} could not sync shared env manifests: {e}",
                    style("warning:").yellow()
                );
            }

            match action {
                BoxCommands::Create {
                    name,
                    from,
                    pr,
                    engine,
                    clone,
                    new,
                    remote,
                    profile,
                    isolation,
                    image,
                    backend,
                    audit,
                    json,
                    #[cfg(feature = "runner")]
                    runner,
                } => {
                    let agent = agent_identity();
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
                        Some(spec) => Some(h5i_core::source::resolve_pr_base(&workdir, spec, &remote)?),
                        None => None,
                    };
                    let source = match (&clone, new) {
                        (Some(url), _) => h5i_core::env::BoxSource::Clone { url: url.clone() },
                        (None, true) => h5i_core::env::BoxSource::New,
                        (None, false) => h5i_core::env::BoxSource::Repo,
                    };
                    let engine = match &engine {
                        Some(name) => Some(
                            h5i_core::sandbox::BrowserEngine::parse(name)
                                .map_err(h5i_core::error::H5iError::Metadata)?,
                        ),
                        None => None,
                    };
                    let opts = h5i_core::env::CreateOpts {
                        source,
                        from: pr_base.as_ref().map(|b| b.oid.clone()).or(from),
                        profile,
                        isolation,
                        image,
                        engine,
                        backend,
                        audit_capture: h5i_core::sandbox::AuditCapture::parse(&audit)?,
                        parent_branch: pr_base.as_ref().map(|b| b.local_branch.clone()),
                        pr: pr_base.as_ref().map(|b| b.number),
                        pr_head_ref: pr_base.as_ref().and_then(|b| b.head_ref.clone()),
                        #[cfg(feature = "runner")]
                        runner: runner.clone(),
                        #[cfg(not(feature = "runner"))]
                        runner: None,
                    };

                    // Opened before the create so a bad runner name fails
                    // before a branch exists, and held for the call so the
                    // bundle it builds outlives the request that streams it.
                    #[cfg(feature = "runner")]
                    let placement = match &runner {
                        Some(name) => {
                            let paired = super::placement::PairedRunner::open(name)?;
                            // Only an explicit tier can be checked ahead of
                            // time: `auto` is resolved against the runner's own
                            // host, so what it picks is the runner's to say.
                            if let Some(h5i_core::sandbox::IsolationRequest::Claim(c)) =
                                &opts.isolation
                            {
                                paired.check_supports(c.as_str())?;
                            }
                            Some(paired)
                        }
                        None => None,
                    };
                    #[cfg(feature = "runner")]
                    let m = h5i_core::env::create_with_remote(
                        git,
                        &h5i_root,
                        &workdir,
                        &agent,
                        &name,
                        opts,
                        placement
                            .as_ref()
                            .map(|p| p as &dyn h5i_core::placement::RemoteRunner),
                    )?;
                    #[cfg(not(feature = "runner"))]
                    let m = h5i_core::env::create(git, &h5i_root, &workdir, &agent, &name, opts)?;
                    if json {
                        // The manifest is the contract (same shape as
                        // `status --json`); the workspace path is the one fact
                        // it cannot carry, so it is injected like `list` does
                        // with drift.
                        let mut v = serde_json::to_value(&m)?;
                        if let serde_json::Value::Object(ref mut map) = v {
                            map.insert(
                                "work_dir".into(),
                                serde_json::Value::String(
                                    m.work_dir(&h5i_root).display().to_string(),
                                ),
                            );
                        }
                        println!("{}", serde_json::to_string_pretty(&v)?);
                        if profile_auto && m.profile == "default" {
                            eprintln!(
                                "note: this host cannot enforce the built-in 'agent' profile, so \
                                 the fail-closed 'default' was used — coding agents won't run in \
                                 this box"
                            );
                        }
                        return Ok(());
                    }
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
                        h5i_core::env::short(&m.base_commit, 12),
                        m.parent_branch
                    );
                    println!("   branch   {}", m.branch);
                    // A runner box has no workspace on this machine, and
                    // printing the path one *would* have had is how somebody
                    // ends up `cd`-ing into a directory that was never created.
                    match (&m.runner, &m.runner_id) {
                        (Some(runner), Some(id)) => {
                            println!(
                                "   on       {} ({})",
                                style(runner).cyan(),
                                h5i_core::env::short(id, 12)
                            );
                            println!(
                                "   work     on {runner} — this machine keeps the repository, \
                                 the policy and the credentials"
                            );
                        }
                        _ => println!("   work     {}", m.work_dir(&h5i_root).display()),
                    }
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
                             rootless Podman, which isn't installed — install it, then pass --image \
                             or set `[container] image` in .h5i/env.toml. See: h5i box probe",
                            style("tip").yellow()
                        );
                    }
                    // Suggesting verbs that cannot work yet is worse than
                    // suggesting nothing: it reads as a bug in the box rather
                    // than as a milestone that has not landed.
                    if h5i_core::env::is_remote(&m) {
                        println!(
                            "   next     h5i box run {} -- <cmd>   ·   h5i box propose {}   ·   \
                             h5i box apply {}",
                            m.slug, m.slug, m.slug
                        );
                        println!(
                            "   {}      `box shell` needs a terminal on the runner, which is \
                             the next milestone; everything else works",
                            style("note").yellow()
                        );
                    } else {
                        println!(
                            "   next     h5i box run {} -- <cmd>   ·   h5i box shell {}   ·   \
                             h5i box export {}",
                            m.slug, m.slug, m.slug
                        );
                    }
                }

                BoxCommands::Run { name, json, command } => {
                    if command.is_empty() {
                        anyhow::bail!("usage: h5i box run [--json] <name> -- <command> [args…]");
                    }
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    // A box on a runner runs there, which is the whole point of
                    // having put it there.
                    #[cfg(feature = "runner")]
                    let outcome = match (&m.runner, h5i_core::env::is_remote(&m)) {
                        (Some(runner_name), true) => {
                            let paired = super::placement::PairedRunner::open(runner_name)?;
                            paired.check_identity(&m)?;
                            h5i_core::env::run_remote(git, &h5i_root, &mut m, &command, &paired)?
                        }
                        _ => h5i_core::env::run(git, &h5i_root, &mut m, &command)?,
                    };
                    #[cfg(not(feature = "runner"))]
                    let outcome = h5i_core::env::run(git, &h5i_root, &mut m, &command)?;
                    // The command's own output, as recorded (secret-redacted).
                    let dir = h5i_core::env::env_dir(&h5i_root, &m.agent, &m.slug);
                    if json {
                        let raw = h5i_core::receipt::raw_bytes(&dir, &outcome.receipt.id)
                            .unwrap_or_default();
                        let envelope = serde_json::json!({
                            "box": m.id,
                            "policy_digest": m.policy_digest,
                            "capture": outcome.capture_id,
                            "exit_code": outcome.exit_code,
                            "timed_out": outcome.timed_out,
                            "wall_ms": outcome.wall_ms as u64,
                            "cpu_ms": outcome.cpu_ms as u64,
                            "max_rss_kb": outcome.max_rss_kb,
                            "output": String::from_utf8_lossy(&raw),
                            "record": outcome.receipt,
                        });
                        println!("{}", serde_json::to_string_pretty(&envelope)?);
                    } else {
                        if let Ok(raw) = h5i_core::receipt::raw_bytes(&dir, &outcome.receipt.id) {
                            print!("{}", String::from_utf8_lossy(&raw));
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
                            "{} receipt {} (box {}, policy {}) · exit {} · wall {}ms, cpu {}ms{}",
                            LOOKING,
                            style(&outcome.capture_id).magenta(),
                            m.id,
                            h5i_core::env::short(&m.policy_digest, 12),
                            outcome
                                .exit_code
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "signal".into()),
                            outcome.wall_ms,
                            outcome.cpu_ms,
                            rss
                        );
                    }
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

                BoxCommands::Shell {
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

                BoxCommands::Allow { rule, remove, json } => {
                    if json && (rule.is_some() || remove) {
                        anyhow::bail!("--json can only be used when listing the allowlist");
                    }
                    match rule {
                        None => {
                            let rules = h5i_core::env::user_allow_list();
                            let path = h5i_core::env::user_allow_path();
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "path": path.map(|path| path.display().to_string()),
                                        "rules": rules,
                                    }))?
                                );
                            } else {
                                match path {
                                    Some(path) => println!("── user egress allowlist ({}) ──", path.display()),
                                    None => println!("── user egress allowlist ──"),
                                }
                                if rules.is_empty() {
                                    println!("  (empty — add one with `h5i box allow <host>`)");
                                }
                                for r in &rules {
                                    println!("  {r}");
                                }
                                println!(
                                    "  applies to container-tier envs whose profile sets net.egress; \
                                     takes effect at the next env run/shell"
                                );
                            }
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
                    }
                }

                BoxCommands::Detect { action } => {
                    return crate::cli::detect::run(action);
                }

                BoxCommands::Probe => {
                    // Diagnostics must report the live truth, not last run's
                    // verdict — bypass the per-boot podman probe cache.
                    let caps = h5i_core::sandbox::probe_host_fresh();
                    println!("── Host isolation capabilities ──");
                    println!("  os           = {}", caps.os);
                    println!("  mechanism    = {}", caps.confinement_mechanism());
                    // Report the primitives this OS actually has. Printing
                    // `landlock_abi = none` on a Mac reads as a broken host when
                    // the truth is that macOS confines with something else.
                    if caps.os == "macos" {
                        println!("  seatbelt     = {}", caps.seatbelt);
                        println!(
                            "  {}",
                            style(
                                "note: no syscall filter and no memory cap at the macOS kernel \
                                 tiers; use isolation=container for a memory cap"
                            )
                            .dim()
                        );
                    } else {
                        println!(
                            "  landlock_abi = {}",
                            caps.landlock_abi
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "none".into())
                        );
                        println!("  userns       = {}", caps.userns);
                        println!("  seccomp      = {}", caps.seccomp);
                    }
                    // An interactive box shell shares the operator's terminal
                    // (that is what makes job control work), so whether the box
                    // can type into it is part of what this host enforces — and
                    // it is not h5i's to assert: on Linux it is the kernel's
                    // setting, the same at every tier; on macOS it is the
                    // Seatbelt profile, so it holds only where one is applied.
                    // Report both, because they can disagree on the same host.
                    let confined = h5i_core::sandbox::tty_input_injection(
                        &caps,
                        h5i_core::sandbox::IsolationClaim::Process,
                    );
                    let unconfined = h5i_core::sandbox::tty_input_injection(
                        &caps,
                        h5i_core::sandbox::IsolationClaim::Workspace,
                    );
                    println!(
                        "  tty-injection= {}",
                        match (confined, unconfined) {
                            (false, false) => style("blocked at every tier".to_string()).green(),
                            (false, true) => style(
                                "blocked at the kernel tiers, possible at isolation=workspace"
                                    .to_string()
                            )
                            .yellow(),
                            // Nothing subtracts it anywhere: a box shell can type
                            // at the terminal you launched it from.
                            (true, _) => style("possible at every tier".to_string()).red(),
                        }
                    );
                    if confined {
                        println!(
                            "    {}",
                            style(match caps.os.as_str() {
                                "linux" =>
                                    "set dev.tty.legacy_tiocsti=0 (or build with \
                                     CONFIG_LEGACY_TIOCSTI=n) to close it; upstream defaults \
                                     it open",
                                "macos" =>
                                    "no Seatbelt profile is applied on this host, so nothing \
                                     subtracts the ioctl",
                                _ => "this host has no confinement backend, so nothing \
                                      subtracts the ioctl",
                            })
                            .dim()
                        );
                    }
                    println!(
                        "  container    = {}",
                        caps.container_runtime.as_deref().unwrap_or("none")
                    );
                    println!(
                        "  microvm      = {}",
                        caps.microvm_runtime.as_deref().unwrap_or("none")
                    );
                    {
                        // Deliberately last, and deliberately labelled. Every
                        // line above is about what the host can *stop*; this
                        // one is about what it can *see*, and mixing the two
                        // readings is how an observability feature ends up
                        // quoted as a containment claim.
                        let d = h5i_core::bpf::probe_host();
                        println!(
                            "  detect       = {} {}",
                            if d.usable { "yes" } else { "no" },
                            style(
                                "(observation only — `h5i box detect probe` for the detail)"
                            )
                            .dim()
                        );
                        if !d.usable && let Some(why) = &d.detail {
                            println!("                 {}", style(why).dim());
                        }
                    }
                    println!();
                    // Every kernel tier, including `supervised`. It was missing
                    // here while `sandbox::probe` enumerated it correctly, so
                    // the command whose whole job is "what can this host
                    // enforce" left out the tier the code calls the security
                    // keystone — and anyone choosing a tier from this output
                    // would conclude it was unavailable.
                    for (claim, profile_net_deny) in [
                        (h5i_core::sandbox::IsolationClaim::Workspace, false),
                        (h5i_core::sandbox::IsolationClaim::Process, true),
                        (h5i_core::sandbox::IsolationClaim::Supervised, true),
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
                    // Image-backed claims: each needs its own runtime plus a
                    // profile image; show whether the runtime half is there.
                    let container_ok = caps.container_runtime.is_some();
                    println!(
                        "  claim {:<10} satisfiable = {} (needs rootless Podman + profile container.image)",
                        "container",
                        if container_ok { style("yes").green() } else { style("no").red() }
                    );
                    let microvm_ok = caps.microvm_runtime.is_some();
                    // The prerequisite list is a hint for a reader who cannot
                    // run this tier; printing it next to "yes" would read as an
                    // unmet requirement on a host that already meets them all.
                    println!(
                        "  claim {:<10} satisfiable = {}{}",
                        "microvm",
                        if microvm_ok { style("yes").green() } else { style("no").red() },
                        if microvm_ok {
                            " (profile needs container.image)"
                        } else {
                            " (needs microsandbox `msb` + host virtualization + profile container.image)"
                        }
                    );
                    if !microvm_ok {
                        // Which half is missing decides what the reader does
                        // next: install a package, or turn on nested virt.
                        println!(
                            "    {}",
                            style(h5i_core::sandbox::microvm_unavailable_detail()).dim()
                        );
                    }
                    println!(
                        "  claim hardened-container: external backend (not in this build)"
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

                BoxCommands::Capabilities { json } => {
                    // Same as Probe: a capability report is a diagnostic —
                    // never serve it from the probe cache.
                    let report = h5i_core::sandbox::capabilities_report_fresh();
                    // The runtime-detection lane is grafted on here rather than
                    // added to `CapabilitiesReport` itself: that type lives in
                    // `h5i-sandbox`, which is the *confinement* layer, and the
                    // detector sits beside it rather than under it. A field
                    // there would put an observability crate below the thing it
                    // observes, for the sake of one JSON key.
                    let detect = h5i_core::bpf::probe_host();
                    if json {
                        let mut v = serde_json::to_value(&report)?;
                        if let serde_json::Value::Object(ref mut map) = v {
                            map.insert("runtime_detection".into(), serde_json::to_value(&detect)?);
                        }
                        println!("{}", serde_json::to_string_pretty(&v)?);
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
                        println!("  mechanism        = {}", report.mechanism);
                        if report.os == "macos" {
                            println!("  seatbelt         = {}", yn(report.seatbelt));
                        } else {
                            println!(
                                "  landlock_abi     = {}",
                                report
                                    .landlock_abi
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "none".into())
                            );
                            println!("  userns           = {}", yn(report.userns));
                            println!("  seccomp          = {}", yn(report.seccomp));
                        }
                        println!(
                            "  container        = {}",
                            report.container_runtime.as_deref().unwrap_or("none")
                        );
                        println!(
                            "  microvm          = {}",
                            report.microvm_runtime.as_deref().unwrap_or("none")
                        );
                        println!("  egress_enforced  = {}", yn(report.egress_enforced));
                        // The distinction that matters for untrusted code: an
                        // L7 proxy stops `curl`, an L3 filter stops a raw socket.
                        println!(
                            "  egress_l3        = {} {}",
                            yn(report.egress_enforced_l3),
                            style(if report.egress_enforced_l3 {
                                "(by address, microvm netstack)"
                            } else if report.egress_enforced {
                                "(allowlist is L7 proxy only)"
                            } else {
                                ""
                            })
                            .dim()
                        );
                        println!("  syscall_filter   = {}", yn(report.syscall_filter));
                        println!("  resource_limits  = {}", yn(report.resource_limits));
                        println!("  memory_limit     = {}", yn(report.memory_limit));
                        println!(
                            "  strongest_tier   = {}",
                            style(report.strongest_tier).cyan().bold()
                        );
                        // Observation, not confinement, and printed as such so
                        // nobody reads a `yes` here as a boundary.
                        println!(
                            "  runtime_detect   = {} {}",
                            yn(detect.usable),
                            style(if detect.usable {
                                "(observation only; `h5i box detect probe` for detail)".to_string()
                            } else {
                                format!(
                                    "({})",
                                    detect.detail.as_deref().unwrap_or("unavailable")
                                )
                            })
                            .dim()
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

                BoxCommands::List { json } => {
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
                            println!("No environments. Create one: h5i box create <name>");
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
                            // A pulled manifest is peer data: `h5i pull`
                            // materialises one from `refs/h5i/env`, and only
                            // its identity fields and object ids are pinned.
                            // These land in a terminal, so they are cleaned
                            // the way every other box-supplied string is.
                            let clean = h5i_core::redact::sanitize_display;
                            // Where it runs, when that is not here. Rendered
                            // rather than stored as a column so a local box's
                            // line is byte-for-byte what it always was.
                            let placement = match &m.runner {
                                Some(name) => format!(" on={}", clean(name)),
                                None => String::new(),
                            };
                            println!(
                                "{:<28} {:<9} isolation={:<10} base={} captures={}{}{}{}",
                                style(clean(&m.id)).magenta(),
                                clean(&m.status),
                                clean(&m.isolation_claim),
                                h5i_core::env::short(&m.base_commit, 12),
                                m.captures.len(),
                                style(&placement).cyan(),
                                style(drift_mark).yellow(),
                                style(&live_mark).green()
                            );
                        }
                    }
                }

                BoxCommands::Status { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&m)?);
                    } else {
                        print!("{}", h5i_core::env::status_report(git, &h5i_root, &m));
                    }
                }

                BoxCommands::View {
                    name,
                    port,
                    term,
                    fps,
                    assume_graphics,
                } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let dir = h5i_core::env::env_dir(&h5i_root, &m.agent, &m.slug);
                    if term {
                        // The status line reports what the box may reach. It is
                        // read from the *enforced* policy, not from the profile
                        // as written, so it describes the box that is running.
                        let enforced = h5i_core::env::load_policy(&h5i_root, &m);
                        let egress = enforced
                            .as_ref()
                            .map(|p| egress_summary(&p.profile.net_egress))
                            .unwrap_or_default();
                        // Same source as the egress line, for the same reason:
                        // what is running, not what was written.
                        let engine = enforced
                            .as_ref()
                            .ok()
                            .and_then(|p| p.profile.engine)
                            .map(|e| e.as_str().to_string());
                        h5i_core::termview::run(h5i_core::termview::Options {
                            env_dir: dir,
                            env_id: m.id.clone(),
                            policy_digest: m.policy_digest.clone(),
                            egress,
                            engine,
                            max_fps: fps,
                            assume_graphics,
                        })?;
                    } else {
                        let forward =
                            h5i_core::view::Forward::bind(&dir, &m.id, &m.policy_digest, port)?;
                        let holder = h5i_core::control::read(&dir).holder;
                        println!("{} viewer for {}", SUCCESS, m.id);
                        println!("   open     {}", forward.url()?);
                        println!("   control  {holder:?} — `h5i browser take {name}` to drive");
                        println!("   stop     Ctrl-C");
                        forward.serve()?;
                    }
                }

                #[cfg(feature = "share-tunnel")]
                BoxCommands::Share(args) => crate::cli::share::run(args)?,

                BoxCommands::Doctor { name, json } => {
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

                BoxCommands::Secrets { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let policy = h5i_core::env::load_policy(&h5i_root, &m)?;
                    let rows = h5i_core::env::secrets_status(&h5i_root, &policy);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        print!("{}", h5i_core::env::render_secrets(&m.id, &rows));
                    }
                }

                BoxCommands::Service { action } => match action {
                    EnvServiceCommands::Start { env, service } => {
                        let m = h5i_core::env::find(&h5i_root, &env)?;
                        let rec = h5i_core::env::service_start(git, &h5i_root, &m, &service)?;
                        // A guest service has no *injected* host port — its box
                        // owns a network stack, so it binds the port it declared
                        // and nothing had to be allocated. Reading only
                        // `dynamic_port` here left the tier that just gained
                        // services as the one whose start said nothing about it.
                        let port = match (rec.dynamic_port, rec.port, &rec.runtime) {
                            (Some(p), _, _) => format!(
                                " (injected PORT={p}; reachable at http://127.0.0.1:{p} if it binds the port)"
                            ),
                            (None, Some(p), h5i_core::env::ServiceRuntime::Guest { .. }) => {
                                format!(" (PORT={p}, inside the box's network — see `h5i box share`)")
                            }
                            _ => String::new(),
                        };
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

                BoxCommands::Ports { name, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let rows = h5i_core::env::service_status(&h5i_root, &m);
                    if json {
                        let ports: Vec<_> = rows
                            .iter()
                            .filter(|s| s.record.dynamic_port.is_some() || s.record.port.is_some())
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&ports)?);
                    } else {
                        print!("{}", h5i_core::env::render_ports(&m.id, &rows));
                    }
                }

                BoxCommands::Rebase { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let msg_out = h5i_core::env::rebase(git, &h5i_root, &mut m)?;
                    println!("{} {}", SUCCESS, msg_out);
                }

                BoxCommands::Log { name, limit, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    let mut events = h5i_core::env::read_events(git, Some(&m.id));
                    if limit > 0 && events.len() > limit {
                        events.drain(..events.len() - limit);
                    }
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

                BoxCommands::Watch {
                    name,
                    deny_only,
                    json,
                } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    super::watch::run(&h5i_root, &m, deny_only, json)?;
                }

                BoxCommands::Diff { name, stat, json } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        let report = h5i_core::env::diffstat_report(git, &h5i_root, &m)?;
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print!("{}", h5i_core::env::diff(git, &h5i_root, &m, stat)?);
                        // On stderr, so `h5i box diff > x.patch` still yields a
                        // clean patch while a human at a terminal is told what
                        // was held back.
                        let hidden = h5i_core::env::private_paths_excluded(&h5i_root, &m);
                        if !hidden.is_empty() {
                            eprintln!(
                                "{}",
                                style(format!(
                                    "note: {} path(s) excluded as private ({})",
                                    hidden.len(),
                                    hidden.join(", ")
                                ))
                                .dim()
                            );
                        }
                    }
                }

                BoxCommands::Inspect {
                    name,
                    capture,
                    json,
                } => {
                    let m = h5i_core::env::find(&h5i_root, &name)?;
                    if json {
                        let rec = h5i_core::env::inspect_manifest(&h5i_root, &m, &capture)?;
                        println!("{}", serde_json::to_string_pretty(&rec)?);
                    } else {
                        print!("{}", h5i_core::env::inspect(&h5i_root, &m, &capture)?);
                    }
                }

                BoxCommands::Compare { names, json } => {
                    let rows = h5i_core::env::compare(git, &h5i_root, &names)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        print!("{}", h5i_core::env::render_compare(&rows));
                    }
                }

                BoxCommands::Cache { action } => run_cache(action, git, &h5i_root, &workdir)?,

                BoxCommands::Export { name, out, force, json } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let out = out.unwrap_or_else(|| {
                        workdir.join("h5i-export").join(&m.slug)
                    });
                    #[cfg(feature = "runner")]
                    let paired = match (&m.runner, h5i_core::env::is_remote(&m)) {
                        (Some(runner_name), true) => {
                            let p = super::placement::PairedRunner::open(runner_name)?;
                            p.check_identity(&m)?;
                            Some(p)
                        }
                        _ => None,
                    };
                    #[cfg(feature = "runner")]
                    let s = h5i_core::export::export_with_remote(
                        git,
                        &h5i_root,
                        &mut m,
                        &out,
                        force,
                        paired
                            .as_ref()
                            .map(|p| p as &dyn h5i_core::placement::RemoteRunner),
                    )?;
                    #[cfg(not(feature = "runner"))]
                    let s = h5i_core::export::export(git, &h5i_root, &mut m, &out, force)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&s)?);
                        return Ok(());
                    }
                    println!(
                        "{} exported {} → {}",
                        SUCCESS,
                        s.env_id,
                        style(s.dir.display()).cyan()
                    );
                    println!(
                        "   changes  {} file(s), +{} -{} ({} bytes of patch)",
                        s.files_changed, s.insertions, s.deletions, s.patch_bytes
                    );
                    println!("   receipts {} recorded", s.receipts);
                    if s.egress_denied > 0 {
                        println!(
                            "   {}     {} denied egress attempt(s) — read report.md before applying",
                            style("egress").yellow(),
                            s.egress_denied
                        );
                    }
                    if !s.redactions.is_empty() {
                        println!("   redacted {}", s.redactions.join(", "));
                    }
                    println!(
                        "   next     review {}/patch.diff, then `git apply --3way` it where you want it",
                        s.dir.display()
                    );
                }

                BoxCommands::Propose { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    // A runner box freezes on the machine it lives on, and the
                    // work comes home through a quarantine.
                    #[cfg(feature = "runner")]
                    let brief = match (&m.runner, h5i_core::env::is_remote(&m)) {
                        (Some(runner_name), true) => {
                            let paired = super::placement::PairedRunner::open(runner_name)?;
                            paired.check_identity(&m)?;
                            h5i_core::env::propose_remote(git, &h5i_root, &mut m, &paired)?
                        }
                        _ => h5i_core::env::propose(git, &h5i_root, &mut m)?,
                    };
                    #[cfg(not(feature = "runner"))]
                    let brief = h5i_core::env::propose(git, &h5i_root, &mut m)?;
                    println!("{brief}");
                }

                BoxCommands::Apply { name, patch } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    let msg_out = h5i_core::env::apply(git, &h5i_root, &mut m, patch)?;
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

                BoxCommands::Abort { name } => {
                    let mut m = h5i_core::env::find(&h5i_root, &name)?;
                    h5i_core::env::abort(git, &h5i_root, &mut m)?;
                    println!(
                        "{} {} aborted (manifest preserved for forensics)",
                        SUCCESS, m.id
                    );
                    // "Aborted" is a stronger word than the state this produces
                    // for a runner box: the box keeps existing over there until
                    // its lease expires or it is removed. Saying so is the
                    // difference between a status and a promise.
                    #[cfg(feature = "runner")]
                    if h5i_core::env::is_remote(&m) {
                        println!(
                            "   {}      the box itself is still on `{}` until its lease \
                             expires — `h5i box rm {}` removes it now",
                            style("note").yellow(),
                            m.runner.as_deref().unwrap_or("the runner"),
                            m.slug
                        );
                    }
                }

                BoxCommands::Rm { names, force } => {
                    let mut any_failed = false;
                    for name in &names {
                        match h5i_core::env::find(&h5i_root, name) {
                            Ok(m) => {
                            match h5i_core::env::rm(git, &h5i_root, &m, force) {
                                Ok(()) => {
                                    // The far side only after this side agreed
                                    // to let the box go. The other order looks
                                    // tidier and is wrong: `rm` refuses a live
                                    // box, and destroying the runner's copy
                                    // first would leave a local record pointing
                                    // at nothing while the user was told the
                                    // removal had failed.
                                    //
                                    // The remaining failure — this side gone,
                                    // the runner unreachable — leaves an orphan
                                    // there, which is exactly what the lease is
                                    // for: it expires and the next sweep takes
                                    // it. Best effort, and said out loud.
                                    #[cfg(feature = "runner")]
                                    if h5i_core::env::is_remote(&m)
                                        && let Some(problem) =
                                            super::placement::destroy_remote(&m)
                                    {
                                        eprintln!(
                                            "{} {} — it will be reaped there when its lease \
                                             expires",
                                            style("warning:").yellow().bold(),
                                            problem
                                        );
                                    }
                                    // A browser session placed in this box has
                                    // just lost its engine. Recording the cause
                                    // here is the difference between a record
                                    // that says "evicted, the box was removed"
                                    // and one that says "died" — both true, one
                                    // of them knowable only on this side of the
                                    // boundary and only at this moment.
                                    if let Ok(root) = h5i_core::browser_session::root()
                                        && let Ok(n) =
                                            h5i_core::browser_session::evict_box(&root, name)
                                        && n > 0
                                    {
                                        println!(
                                            "  {} browser session{} in this box {} ended \
                                             (`h5i browser list --all` keeps the record)",
                                            n,
                                            if n == 1 { "" } else { "s" },
                                            if n == 1 { "was" } else { "were" }
                                        );
                                    }
                                    println!(
                                        "{} {} removed (workspace, branches, and manifest erased)",
                                        SUCCESS, m.id
                                    )
                                }
                                Err(e) => {
                                    eprintln!(
                                        "{} failed to remove {}: {e}",
                                        style("error:").red().bold(),
                                        name
                                    );
                                    any_failed = true;
                                }
                            }
                            }
                            Err(e) => {
                                eprintln!(
                                    "{} env '{}' not found: {e}",
                                    style("error:").red().bold(),
                                    name
                                );
                                any_failed = true;
                            }
                        }
                    }
                    if any_failed {
                        std::process::exit(1);
                    }
                }

                BoxCommands::Gc => {
                    // Runner boxes are not reclaimable from here, and a `gc`
                    // that silently omitted the one kind of box still consuming
                    // something read as "nothing to do".
                    #[cfg(feature = "runner")]
                    {
                        let mut names: Vec<String> = h5i_core::env::list(&h5i_root)
                            .into_iter()
                            .filter(h5i_core::env::is_remote)
                            .filter_map(|m| m.runner)
                            .collect();
                        names.sort();
                        names.dedup();
                        if !names.is_empty() {
                            println!(
                                "{} box(es) live on runners and are not reclaimed from here — \
                                 `h5i runner gc <name>` reaps what has expired on {}.",
                                style("note:").yellow(),
                                names.join(", ")
                            );
                        }
                    }
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
