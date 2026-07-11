//! `h5i team` — CLI handlers (migrated from main.rs).
use crate::*;

#[derive(Subcommand)]
pub enum TeamCommands {
    /// Create a team run over existing h5i environments
    Create {
        /// Team id (ref-safe slug)
        name: String,
        /// Base revision shared by all candidates
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Maximum improvement rounds planned for this run
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        /// Human-readable label; defaults to the team id
        #[arg(long)]
        title: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Bootstrap a two-agent (claude + codex) team in one shot: create an
    /// agent-claude env and an agent-codex env, create the team, and enroll
    /// both. Equivalent to running `env create`, `team create`, and two
    /// `team add-env` calls by hand.
    AutoCreate {
        /// Team id (ref-safe slug); also used to name the per-agent envs
        /// (`<name>-claude`, `<name>-codex`)
        name: String,
        /// Base revision shared by all candidates
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Maximum improvement rounds planned for this run
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        /// Human-readable label; defaults to the team id
        #[arg(long)]
        title: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// List team runs
    List {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Print the standing bootstrap prompt for a boxed team agent
    Bootstrap,
    /// Drive a full hands-off team cycle over the orchestra eDSL: dispatch →
    /// independent attempts → freeze → mutual review → revise → neutral verify
    /// → verdict (→ gated apply). Replaces scripts/team-run.sh; unlike the
    /// script it is journal-backed — re-running it resumes the cycle instead
    /// of starting over.
    Run {
        /// Team id (default: the current team)
        team: Option<String>,
        /// Task text to dispatch
        #[arg(long)]
        task: Option<String>,
        /// Read the task from a file
        #[arg(long = "task-file", conflicts_with = "task")]
        task_file: Option<std::path::PathBuf>,
        /// TOML manifest parameterizing the run (task/rounds/verify/gate +
        /// optional agent enrollment). Flags below override manifest values.
        #[arg(long)]
        manifest: Option<std::path::PathBuf>,
        /// Maximum review→revise cycles (early exit on full approval)
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        /// Neutral verifier command, e.g. --verify-cmd "cargo test -q"
        #[arg(long = "verify-cmd")]
        verify_cmd: Option<String>,
        /// Isolation tier for the verifier (workspace|process|container|…)
        #[arg(long)]
        isolation: Option<String>,
        /// Apply the winner after an auto-applicable verdict
        #[arg(long)]
        apply: bool,
        /// Ask a durable gate question before applying (implies apply on
        /// approval; the reply may arrive after this process exits — re-run
        /// to resume at the gate)
        #[arg(long, conflicts_with = "apply")]
        gate: bool,
        /// Spawn resident agent sessions in tmux (default: attach to
        /// already-running sessions, e.g. from team-launch.sh)
        #[arg(long = "launch-resident")]
        launch_resident: bool,
        /// Poll interval while waiting on turns, seconds
        #[arg(long, default_value_t = 15)]
        poll: u64,
        /// Per-turn wait budget, seconds
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Emit the outcome as JSON
        #[arg(long)]
        json: bool,
    },
    /// Render the recorded orchestration trace (journaled steps + phases)
    Trace {
        /// Team id (default: the current team)
        team: Option<String>,
        /// Emit Graphviz dot instead of text
        #[arg(long)]
        dot: bool,
    },
    /// Show or set the current team (omit NAME to show; --clear to unset)
    Use {
        /// Team id to make current; omit to print the current team
        name: Option<String>,
        /// Clear the current-team pointer
        #[arg(long)]
        clear: bool,
    },
    /// Add an existing env to a team roster
    AddEnv {
        /// Team id
        team: String,
        /// Env name (`slug`, `agent/slug`, or `env/agent/slug`)
        env: String,
        /// Ref-safe agent key for this team (default: a generated name)
        #[arg(long = "as")]
        as_agent: Option<String>,
        /// Runtime adapter (`claude`, `codex`, etc.)
        #[arg(long)]
        runtime: Option<String>,
        /// Model label
        #[arg(long)]
        model: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a team run
    Status {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Freeze one agent's candidate as an immutable submission
    Submit {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Team agent id
        #[arg(long)]
        agent: String,
        /// Commit to submit; defaults to the env branch tip
        #[arg(long)]
        commit: Option<String>,
        /// Summary text file
        #[arg(long = "summary-file")]
        summary_file: Option<std::path::PathBuf>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Move a draft team into sealed_submit after required submissions exist
    Freeze {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Permit a partial freeze and record missing submissions
        #[arg(long)]
        allow_missing: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Compare team candidates side by side
    Compare {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Show one teammate's submission by artifact id (read-only; safe in a box).
    /// Defaults to the diff; `--summary`/`--tests` show the other granted views.
    Artifact {
        #[command(subcommand)]
        action: TeamArtifactCommands,
    },
    /// Ingest agents' staged submissions/reviews now, without exiting their
    /// boxes (live counterpart to the at-exit ingest). Lets a run advance while
    /// the team Stop hook keeps boxes alive — no relaunch needed.
    Sync {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Run neutral verifier evidence for one submitted candidate
    Verify {
        /// Team id
        team: String,
        /// Team agent id whose latest submission should be verified
        #[arg(long)]
        agent: String,
        /// Isolation tier for the sandboxed verifier (workspace|process|
        /// supervised|container); default auto-picks the strongest the host can
        /// enforce, falling back to workspace
        #[arg(long)]
        isolation: Option<String>,
        /// Verifier command and args (everything after `--`), e.g. `-- cargo test`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        cmd: Vec<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Evaluate the conservative verifier-based finalization policy
    Finalize {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Apply the selected winner into the current branch
    Apply {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Submission id; defaults to the finalized winner
        #[arg(long)]
        winner: Option<String>,
        /// Pick a team agent's latest submission directly, skipping
        /// verify/finalize. This is an explicit human override, so it implies
        /// the verifier-verdict gate is bypassed (no `--force` needed).
        #[arg(long, conflicts_with = "winner")]
        agent: Option<String>,
        /// Override verifier verdict / auto-apply gate
        #[arg(long)]
        force: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Optional automation worker: lease runs and finalize verifier-ready teams
    Worker {
        /// Run one finalize pass and exit (mutually exclusive with --watch)
        #[arg(long)]
        once: bool,
        /// Opt-in convenience loop: repeat the one-shot pass every --interval
        /// seconds until interrupted. Still finalize-only; never auto-applies.
        /// For production prefer an external scheduler (cron/systemd/CI) driving
        /// `--once` — crash-resilient and needs no long-lived process.
        #[arg(long, conflicts_with = "once")]
        watch: bool,
        /// Seconds to sleep between passes in --watch mode
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// Worker id (ref-safe)
        #[arg(long, default_value = "team-worker")]
        id: String,
        /// Lease TTL in seconds
        #[arg(long, default_value_t = 300)]
        lease_ttl: i64,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Dispatch a prompt to every team agent through h5i msg
    ///
    /// The task prompt is read from stdin by default
    /// (`h5i team dispatch qsort-demo < TASK.md`); pass `--prompt-file` to read
    /// it from a named file instead.
    Dispatch {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Read the task prompt from this file instead of stdin
        #[arg(long = "prompt-file")]
        prompt_file: Option<std::path::PathBuf>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Send a logged, influence-tracked post-submit discussion message
    Discuss {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Sender team agent id
        #[arg(long)]
        from: String,
        /// Comma-separated recipient team agent ids
        #[arg(long)]
        to: String,
        /// Message body file
        #[arg(long)]
        file: std::path::PathBuf,
        /// Comma-separated artifact ids referenced by this message
        #[arg(long, default_value = "")]
        artifacts: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Grant one team agent review access to another agent's submitted artifacts
    GrantReview {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Reviewing team agent id
        #[arg(long)]
        reviewer: String,
        /// Target team agent id
        #[arg(long)]
        target: String,
        /// Comma-separated artifact kinds (diff,summary,tests,test-status)
        #[arg(long, default_value = "diff,summary,tests")]
        artifacts: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Record a review body for a target candidate
    Review {
        #[command(subcommand)]
        action: TeamReviewCommands,
    },
    /// Scoped commands for a team agent running from a bound env
    Agent {
        #[command(subcommand)]
        action: TeamAgentCommands,
    },
}

#[derive(Subcommand)]
pub enum TeamArtifactCommands {
    /// Show one teammate's submission by artifact id (read-only; safe in a box)
    Show {
        /// Submission artifact id, e.g. `sub-hana-r1-4ea2333c040f`
        id: String,
        /// Team id (defaults to the current team / $H5I_TEAM)
        #[arg(long)]
        team: Option<String>,
        /// Show the unified diff against the team base (the default)
        #[arg(long)]
        diff: bool,
        /// Show the submission's summary
        #[arg(long)]
        summary: bool,
        /// Show the captured test evidence (capture ids + change stats)
        #[arg(long)]
        tests: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum TeamReviewCommands {
    /// Submit a review body
    Submit {
        /// Team id (defaults to the current team — see `team use`)
        team: Option<String>,
        /// Reviewing team agent id
        #[arg(long)]
        reviewer: String,
        /// Target team agent id
        #[arg(long)]
        target: String,
        /// Review text file
        #[arg(long)]
        file: std::path::PathBuf,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum TeamAgentCommands {
    /// Read this team persona's inbox when running host-side
    Inbox {
        /// Team id; defaults to $H5I_TEAM or the current team
        team: Option<String>,
        /// Show without advancing the cursor
        #[arg(long)]
        peek: bool,
        /// Block until a team message is waiting, then print it and exit
        /// (peek-only — does not consume). Use after submitting to await a
        /// review request without ending the session.
        #[arg(long)]
        wait: bool,
        /// Poll interval in seconds while --wait is set.
        #[arg(long, default_value_t = 10)]
        interval: u64,
        /// Give up after this many seconds while --wait is set (0 = forever).
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop-hook: keep this agent working while its team round is unfinished
    /// and deliver pending review requests between turns. Registered by
    /// `h5i hook setup --team`. No-ops outside a team and inside a sealed box.
    Hook {
        /// Team id; defaults to $H5I_TEAM or the current team
        team: Option<String>,
        /// Block the stop (Claude Code) and feed pending messages back so the
        /// agent keeps working; without it, just surface them.
        #[arg(long)]
        block: bool,
        /// Print plain text with no JSON wrapper (Codex Stop hook / manual).
        #[arg(long)]
        quiet: bool,
        /// While --block, wait up to this many seconds for the next message
        /// before allowing the stop (0 = check once, don't wait). Lets a team
        /// agent stay alive between turns until a review arrives or the round
        /// ends, instead of relying on it to run `inbox --wait` itself.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Poll interval in seconds while waiting.
        #[arg(long, default_value_t = 10)]
        interval: u64,
    },
    /// Submit this env persona's candidate; boxed envs stage for host ingest
    Submit {
        /// Commit to submit; defaults to the env branch tip
        #[arg(long)]
        commit: Option<String>,
        /// Summary text file
        #[arg(long = "summary-file")]
        summary_file: Option<std::path::PathBuf>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Reply with data (text/JSON) to the host — the return channel of an
    /// orchestra `ask` turn; boxed envs stage for host ingest
    Reply {
        /// Reply body (or use --file)
        text: Option<String>,
        /// Read the reply body from a file
        #[arg(long, conflicts_with = "text")]
        file: Option<std::path::PathBuf>,
        /// Team id (host-side only; default: the current team)
        #[arg(long)]
        team: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(action: TeamCommands) -> anyhow::Result<()> {
    {
            if let TeamCommands::Agent {
                action:
                    TeamAgentCommands::Submit {
                        commit,
                        summary_file,
                        json,
                    },
            } = &action
            {
                let env_spool =
                    std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR).map(PathBuf::from);
                let in_box = env_spool.is_some()
                    && std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok()
                    && std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_ok();
                if let (true, Some(spool)) = (in_box, env_spool) {
                    let summary = match summary_file {
                        Some(path) => Some(std::fs::read_to_string(path).map_err(|e| {
                            anyhow::anyhow!(
                                "failed to read summary file {}: {e}",
                                path.display()
                            )
                        })?),
                        None => None,
                    };
                    let request = h5i_core::env::TeamSubmitSpool {
                        commit: commit.clone(),
                        summary,
                    };
                    let staged = h5i_core::env::write_team_submit_spool(&spool, &request)?;
                    if *json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "staged": staged,
                                "host_ingest": "env-shell-exit"
                            }))?
                        );
                    } else {
                        println!(
                            "{} team submit staged for host ingest ({})",
                            style("▢").cyan().dim(),
                            staged
                        );
                    }
                    return Ok(());
                }
            }
            let repo = H5iRepository::open(".")?;
            let h5i_root = repo.h5i_root.clone();
            let git = repo.git();
            // Sync the shared env roster to disk — but never in a sealed box,
            // where the host-owned env manifests are read-only (the write only
            // fails with EACCES and spams a warning). The box already has its
            // own env materialized; the shared roster is the host's concern.
            if std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_err() {
                if let Err(e) = h5i_core::env::materialize_from_ref(git, &h5i_root) {
                    eprintln!(
                        "{} could not sync shared env manifests: {e}",
                        style("warning:").yellow()
                    );
                }
            }
            let actor = std::env::var("H5I_AGENT").unwrap_or_else(|_| "human".to_string());

            match action {
                TeamCommands::Create { name, base, rounds, title, json } => {
                    let run = h5i_core::team::create(
                        git,
                        &name,
                        title.as_deref().unwrap_or(&name),
                        &base,
                        rounds,
                        &actor,
                    )?;
                    let _ = h5i_core::team::set_current(&h5i_root, &run.id);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&run)?);
                    } else {
                        let status = h5i_core::team::status(git, &run.id)?;
                        print!("{}", h5i_core::team::render_status(&status));
                    }
                }
                TeamCommands::AutoCreate {
                    name,
                    base,
                    rounds,
                    title,
                    json,
                } => {
                    let workdir = git.workdir().ok_or_else(|| {
                        anyhow::anyhow!("h5i team auto-create requires a non-bare repository")
                    })?;
                    // Envs are created under the human's identity, exactly like
                    // `h5i env create` (env id = env/<agent>/<slug>).
                    let env_agent = msg::resolve_identity(&h5i_root, None)
                        .unwrap_or_else(|_| "human".to_string());

                    // Fixed two-agent claude + codex roster; each member pins its
                    // runtime-scoped agent-in-box profile and a team-derived env
                    // slug (so auto-created teams never collide on env names).
                    let roster = h5i_core::team::auto_create_roster(&name);

                    let mut created = Vec::new();
                    for member in &roster {
                        let opts = h5i_core::env::CreateOpts {
                            profile: Some(member.profile.to_string()),
                            audit_capture: h5i_core::sandbox::AuditCapture::parse("signal")?,
                            ..Default::default()
                        };
                        let m = h5i_core::env::create(
                            git, &h5i_root, workdir, &env_agent, &member.env_slug, opts,
                        )?;
                        eprintln!(
                            "{} created env {} (profile {})",
                            STEP,
                            style(&m.id).magenta().bold(),
                            m.profile
                        );
                        created.push(m);
                    }

                    // Create the team run and make it current, like `team create`.
                    let run = h5i_core::team::create(
                        git,
                        &name,
                        title.as_deref().unwrap_or(&name),
                        &base,
                        rounds,
                        &actor,
                    )?;
                    let _ = h5i_core::team::set_current(&h5i_root, &run.id);

                    // Enroll each env under a generated persona key (like manual
                    // add-env) — distinct from the runtime, which is recorded
                    // separately. Accumulate assigned ids so the two never clash.
                    let mut assigned: Vec<String> = Vec::new();
                    for (member, m) in roster.iter().zip(created.iter()) {
                        let agent_id = h5i_core::team::gen_agent_id(&assigned);
                        h5i_core::team::add_env(
                            git,
                            &h5i_root,
                            &name,
                            &m.id,
                            &agent_id,
                            Some(member.runtime.to_string()),
                            None,
                            &actor,
                        )?;
                        eprintln!(
                            "{} enrolled {} ({}) → {}",
                            STEP,
                            style(&agent_id).green().bold(),
                            member.runtime,
                            m.id
                        );
                        assigned.push(agent_id);
                    }

                    let status = h5i_core::team::status(git, &name)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        print!("{}", h5i_core::team::render_status(&status));
                    }
                }
                TeamCommands::List { json } => {
                    let runs = h5i_core::team::list(git)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&runs)?);
                    } else {
                        print!("{}", h5i_core::team::render_list(&runs));
                    }
                }
                TeamCommands::Bootstrap => {
                    println!("{}", h5i_core::team::AGENT_BOOTSTRAP);
                }
                TeamCommands::Run {
                    team,
                    task,
                    task_file,
                    manifest,
                    rounds,
                    verify_cmd,
                    isolation,
                    apply,
                    gate,
                    launch_resident,
                    poll,
                    timeout,
                    json,
                } => {
                    use h5i_orchestra::{self as orchestra, manifest::TeamManifest, patterns};
                    let run_id = h5i_core::team::resolve_run(&h5i_root, team)?;

                    // A manifest supplies parameters (never control flow);
                    // explicit flags override it. `clap`'s defaults are used
                    // as the override signal for the numeric/bool ones.
                    let manifest_data = match &manifest {
                        Some(path) => {
                            let src = std::fs::read_to_string(path).map_err(|e| {
                                anyhow::anyhow!(
                                    "failed to read manifest {}: {e}",
                                    path.display()
                                )
                            })?;
                            Some(TeamManifest::parse(&src)?)
                        }
                        None => None,
                    };
                    let base_dir = manifest
                        .as_ref()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));

                    let flag_task = match (task, task_file) {
                        (Some(t), _) => Some(t),
                        (None, Some(f)) => Some(std::fs::read_to_string(&f).map_err(|e| {
                            anyhow::anyhow!("failed to read task file {}: {e}", f.display())
                        })?),
                        (None, None) => None,
                    };
                    let task_text = match &manifest_data {
                        Some(m) => m.resolve_task(&base_dir, flag_task)?,
                        None => flag_task
                            .ok_or_else(|| anyhow::anyhow!("pass the task via --task, --task-file, or --manifest"))?,
                    };
                    // Merge parameters: flag if non-default, else manifest, else default.
                    let rounds = if rounds != 1 {
                        rounds
                    } else {
                        manifest_data.as_ref().map(|m| m.rounds).unwrap_or(1)
                    };
                    let verify_cmd = verify_cmd
                        .or_else(|| manifest_data.as_ref().and_then(|m| m.verify_cmd.clone()));
                    let isolation = isolation
                        .or_else(|| manifest_data.as_ref().and_then(|m| m.isolation.clone()));
                    let apply = apply || manifest_data.as_ref().map(|m| m.apply).unwrap_or(false);
                    let gate = gate || manifest_data.as_ref().map(|m| m.gate).unwrap_or(false);

                    // Optional roster enrollment from the manifest, when the
                    // team has none yet (idempotent: skip already-present ids).
                    if let Some(m) = &manifest_data {
                        if !m.agents.is_empty() {
                            let existing = h5i_core::team::status(git, &run_id)?.run;
                            let workdir = git.workdir().ok_or_else(|| {
                                anyhow::anyhow!("team run requires a non-bare repository")
                            })?;
                            let env_agent = msg::resolve_identity(&h5i_root, None)
                                .unwrap_or_else(|_| "human".into());
                            for a in &m.agents {
                                if existing.agents.iter().any(|e| e.agent_id == a.name) {
                                    continue;
                                }
                                let env_id = match &a.env {
                                    Some(id) => h5i_core::env::find(&h5i_root, id)?.id,
                                    None => {
                                        let owner = a
                                            .runtime
                                            .clone()
                                            .unwrap_or_else(|| env_agent.clone());
                                        let slug = format!("{run_id}-{}", a.name);
                                        h5i_core::env::create(
                                            git,
                                            &h5i_root,
                                            workdir,
                                            &owner,
                                            &slug,
                                            h5i_core::env::CreateOpts {
                                                profile: a.profile.clone(),
                                                ..Default::default()
                                            },
                                        )?
                                        .id
                                    }
                                };
                                h5i_core::team::add_env(
                                    git,
                                    &h5i_root,
                                    &run_id,
                                    &env_id,
                                    &a.name,
                                    a.runtime.clone(),
                                    a.model.clone(),
                                    &actor,
                                )?;
                                eprintln!(
                                    "{} enrolled {} → {}",
                                    STEP,
                                    style(&a.name).green().bold(),
                                    env_id
                                );
                            }
                        }
                    }
                    let launcher: std::sync::Arc<dyn orchestra::RuntimeLauncher> =
                        if launch_resident {
                            std::sync::Arc::new(orchestra::LaunchResident)
                        } else {
                            eprintln!(
                                "{} attach mode: agent sessions must already be running \
                                 (team-launch.sh, or pass --launch-resident for tmux)",
                                STEP
                            );
                            std::sync::Arc::new(orchestra::Attach)
                        };
                    let rt = tokio::runtime::Runtime::new()?;
                    let (outcome, applied) = rt.block_on(async {
                        let c = h5i_orchestra::Conductor::builder(".", &run_id)
                            .launcher(launcher)
                            .poll_interval(std::time::Duration::from_secs(poll.max(1)))
                            .turn_timeout(std::time::Duration::from_secs(timeout.max(1)))
                            .launch()?;
                        let agents = c.roster().await?;
                        if agents.len() < 2 {
                            return Err(h5i_core::error::H5iError::Metadata(format!(
                                "team '{run_id}' has {} enrolled agent(s) — team run needs \
                                 at least two (enroll with `h5i team add-env` or \
                                 `h5i team auto-create`)",
                                agents.len()
                            )));
                        }
                        eprintln!(
                            "{} driving team '{run_id}': {} agents, {rounds} review round(s)",
                            STEP,
                            agents.len()
                        );
                        // Fail the predictable ways now, not at minute 30.
                        let mut preflight = c.preflight();
                        if !launch_resident {
                            preflight = preflight.require_live(&agents);
                        }
                        if apply || gate {
                            preflight = preflight.require_clean_worktree();
                        }
                        preflight.run().await?;
                        let mut ensemble =
                            patterns::ensemble(&c, &task_text).agents(agents).rounds(rounds);
                        if let Some(cmd) = &verify_cmd {
                            ensemble = ensemble.verify(cmd.split_whitespace());
                        }
                        if let Some(tier) = &isolation {
                            ensemble = ensemble.isolation(tier.clone());
                        }
                        let outcome = ensemble.run().await?;

                        // Apply, directly or behind a durable gate.
                        let mut applied = None;
                        let winner = outcome.verdict.as_ref().and_then(|v| {
                            v.selected_submission
                                .as_ref()
                                .and_then(|id| outcome.artifacts.iter().find(|a| &a.id == id))
                        });
                        if let Some(winner) = winner {
                            let approved = if gate {
                                eprintln!(
                                    "{} gate: awaiting approval to apply {} (reply with \
                                     `h5i msg reply <n> APPROVE`; re-run this command to \
                                     resume waiting)",
                                    STEP, winner.id
                                );
                                c.gate(format!(
                                    "apply team '{run_id}' winner {} ({} by {})?",
                                    winner.id,
                                    &winner.commit_oid[..12.min(winner.commit_oid.len())],
                                    winner.owner_agent
                                ))
                                .approve()
                                .await?
                            } else {
                                apply
                            };
                            if approved {
                                applied = Some(c.apply(winner).await?);
                            }
                        }
                        Ok((outcome, applied))
                    })?;

                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "artifacts": outcome.artifacts,
                                "reviews": outcome.reviews,
                                "verdict": outcome.verdict,
                                "rounds_run": outcome.rounds_run,
                                "applied": applied,
                            }))?
                        );
                    } else {
                        eprintln!(
                            "{} cycle complete: {} artifacts, {} reviews, {} round(s)",
                            SUCCESS,
                            outcome.artifacts.len(),
                            outcome.reviews.len(),
                            outcome.rounds_run
                        );
                        match (&outcome.verdict, &applied) {
                            (Some(v), Some(a)) => println!(
                                "verdict: {} — applied as {}",
                                v.selected_submission.as_deref().unwrap_or("(none)"),
                                &a.target_commit_oid[..12.min(a.target_commit_oid.len())]
                            ),
                            (Some(v), None) => println!(
                                "verdict: {} — apply with `h5i team apply {run_id}`",
                                v.selected_submission.as_deref().unwrap_or("(none)")
                            ),
                            (None, _) => println!(
                                "no verdict recorded (pass --verify-cmd to judge) — inspect \
                                 with `h5i team compare {run_id}`"
                            ),
                        }
                    }
                }
                TeamCommands::Trace { team, dot } => {
                    let run = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let events = h5i_core::team::read_events(git, &run)?;
                    if dot {
                        print!("{}", h5i_orchestra::trace::render_trace_dot(&run, &events));
                    } else {
                        print!("{}", h5i_orchestra::trace::render_trace(&run, &events));
                    }
                }
                TeamCommands::Use { name, clear } => {
                    if clear {
                        h5i_core::team::clear_current(&h5i_root)?;
                        println!("cleared current team");
                    } else if let Some(name) = name {
                        h5i_core::team::status(git, &name)?; // validate it exists before pinning
                        h5i_core::team::set_current(&h5i_root, &name)?;
                        println!("current team \u{2192} {name}");
                    } else {
                        match h5i_core::team::get_current(&h5i_root) {
                            Some(c) => println!("{c}"),
                            None => println!("(no current team — set one: h5i team use <name>)"),
                        }
                    }
                }
                TeamCommands::AddEnv {
                    team,
                    env,
                    as_agent,
                    runtime,
                    model,
                    json,
                } => {
                    // Default the agent key to a generated name so the user
                    // doesn't have to invent a ref-safe key; `--as` overrides.
                    let (agent_id, generated) = match as_agent {
                        Some(a) => (a, false),
                        None => {
                            let existing: Vec<String> = h5i_core::team::status(git, &team)?
                                .run
                                .agents
                                .into_iter()
                                .map(|a| a.agent_id)
                                .collect();
                            (h5i_core::team::gen_agent_id(&existing), true)
                        }
                    };
                    let run = h5i_core::team::add_env(
                        git, &h5i_root, &team, &env, &agent_id, runtime, model, &actor,
                    )?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&run)?);
                    } else {
                        if generated {
                            eprintln!(
                                "{} assigned agent key {} (override with --as)",
                                STEP,
                                style(&agent_id).green().bold()
                            );
                        }
                        let status = h5i_core::team::status(git, &team)?;
                        print!("{}", h5i_core::team::render_status(&status));
                    }
                }
                TeamCommands::Status { team, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let status = h5i_core::team::status(git, &team)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        print!("{}", h5i_core::team::render_status(&status));
                    }
                }
                TeamCommands::Submit {
                    team,
                    agent,
                    commit,
                    summary_file,
                    json,
                } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let summary = match summary_file {
                        Some(path) => Some(std::fs::read_to_string(&path).map_err(|e| {
                            anyhow::anyhow!("failed to read summary file {}: {e}", path.display())
                        })?),
                        None => None,
                    };
                    let artifact = h5i_core::team::submit(
                        git,
                        &h5i_root,
                        &team,
                        &agent,
                        commit.as_deref(),
                        summary,
                        &actor,
                    )?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&artifact)?);
                    } else {
                        println!(
                            "{} submitted {} for {} at {}",
                            SUCCESS,
                            artifact.id,
                            artifact.owner_agent,
                            &artifact.commit_oid[..12.min(artifact.commit_oid.len())]
                        );
                    }
                }
                TeamCommands::Freeze { team, allow_missing, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let run = h5i_core::team::freeze(git, &team, allow_missing, &actor)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&run)?);
                    } else {
                        let status = h5i_core::team::status(git, &team)?;
                        print!("{}", h5i_core::team::render_status(&status));
                    }
                }
                TeamCommands::Compare { team, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let rows = h5i_core::team::compare(git, &h5i_root, &team)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    } else {
                        print!("{}", h5i_core::team::render_compare(&rows));
                    }
                }
                TeamCommands::Artifact {
                    action: TeamArtifactCommands::Show { id, team, diff, summary, tests, json },
                } => {
                    // Resolve the team like the hook does: explicit arg, then the
                    // host-injected $H5I_TEAM (set inside a team box), then the
                    // current-team pointer.
                    let team = match team {
                        Some(t) => t,
                        None => std::env::var(h5i_core::env::H5I_TEAM_VAR)
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                            .map(Ok)
                            .unwrap_or_else(|| h5i_core::team::resolve_run(&h5i_root, None))?,
                    };
                    let (art, base) = h5i_core::team::find_submission(git, &team, &id)?;
                    // The diff is the default view unless another is requested.
                    let want_diff = diff || (!summary && !tests);
                    if json {
                        let mut obj = serde_json::json!({
                            "id": art.id,
                            "owner_agent": art.owner_agent,
                            "round": art.round,
                            "commit_oid": art.commit_oid,
                            "base_oid": base,
                            "files_changed": art.files_changed,
                            "insertions": art.insertions,
                            "deletions": art.deletions,
                            "capture_ids": art.capture_ids,
                            "summary": art.summary,
                        });
                        if want_diff {
                            obj["diff"] = serde_json::Value::String(
                                h5i_core::team::submission_diff(git, &base, &art.commit_oid)?,
                            );
                        }
                        println!("{}", serde_json::to_string_pretty(&obj)?);
                    } else {
                        println!(
                            "{} {} · {} · round {} · {}",
                            style("submission").bold(),
                            style(&art.id).magenta(),
                            art.owner_agent,
                            art.round,
                            style(format!(
                                "{} file(s) +{} -{}",
                                art.files_changed, art.insertions, art.deletions
                            ))
                            .dim(),
                        );
                        if summary {
                            match art.summary.as_deref() {
                                Some(s) if !s.trim().is_empty() => println!("\n{s}"),
                                _ => println!("  {}", style("(no summary)").dim()),
                            }
                        }
                        if tests {
                            if art.capture_ids.is_empty() {
                                println!("  {}", style("(no captured test evidence)").dim());
                            } else {
                                println!("\ncaptured evidence ({}):", art.capture_ids.len());
                                for c in &art.capture_ids {
                                    println!("  {} {c}  ·  h5i recall object {c}", STEP);
                                }
                            }
                        }
                        if want_diff {
                            let d = h5i_core::team::submission_diff(git, &base, &art.commit_oid)?;
                            if d.trim().is_empty() {
                                println!("  {}", style("(empty diff)").dim());
                            } else {
                                print!("\n{d}");
                            }
                        }
                    }
                }
                TeamCommands::Sync { team, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let drained = h5i_core::team::sync_outbound(git, &h5i_root, &team)?;
                    let total: usize = drained.iter().map(|(_, n)| n).sum();
                    if json {
                        let rows: Vec<_> = drained
                            .iter()
                            .map(|(a, n)| serde_json::json!({ "agent": a, "applied": n }))
                            .collect();
                        println!(
                            "{}",
                            serde_json::to_string_pretty(
                                &serde_json::json!({ "total": total, "agents": rows })
                            )?
                        );
                    } else if total == 0 {
                        println!("{} nothing staged to ingest", SUCCESS);
                    } else {
                        println!("{} ingested {total} staged record(s):", SUCCESS);
                        for (a, n) in drained.iter().filter(|(_, n)| *n > 0) {
                            println!("  {} {a}: {n}", STEP);
                        }
                    }
                }
                TeamCommands::Verify { team, agent, isolation, cmd, json } => {
                    let verification = h5i_core::team::verify(
                        git,
                        &h5i_root,
                        &team,
                        &agent,
                        cmd,
                        isolation.as_deref(),
                        &actor,
                    )?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&verification)?);
                    } else {
                        println!(
                            "{} verifier {} for {} [tier: {}]: applies_cleanly={} tests_passed={}",
                            SUCCESS,
                            verification.id,
                            verification.owner_agent,
                            verification.isolation,
                            verification.applies_cleanly,
                            verification.tests_passed
                        );
                    }
                }
                TeamCommands::Finalize { team, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let verdict = h5i_core::team::finalize(git, &team, &actor)?;
                    // Round decided → release any agents waiting in the team hook.
                    fan_out_team_done(git, &h5i_root, &team, &actor);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&verdict)?);
                    } else if let Some(winner) = &verdict.selected_submission {
                        println!(
                            "{} verdict selected {} (auto_apply={})",
                            SUCCESS, winner, verdict.can_auto_apply
                        );
                        for r in &verdict.reasons {
                            println!("  - {r}");
                        }
                    } else {
                        println!("{} no verdict", style("NOTE").yellow().bold());
                        for r in &verdict.reasons {
                            println!("  - {r}");
                        }
                    }
                }
                TeamCommands::Apply { team, winner, agent, force, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    // `--agent` is an explicit human pick: resolve it to that
                    // agent's latest submission and bypass the verifier-verdict
                    // gate (so you can apply without verify/finalize).
                    let result = match &agent {
                        Some(agent_id) => h5i_core::team::apply_agent(
                            git, &h5i_root, &team, agent_id, &actor,
                        )?,
                        None => h5i_core::team::apply_winner(
                            git,
                            &h5i_root,
                            &team,
                            winner.as_deref(),
                            force,
                            &actor,
                        )?,
                    };
                    // Round applied → release any agents waiting in the team hook.
                    fan_out_team_done(git, &h5i_root, &team, &actor);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    } else {
                        println!(
                            "{} applied {} as commit {}",
                            SUCCESS,
                            result.submission_id,
                            &result.target_commit_oid[..12.min(result.target_commit_oid.len())]
                        );
                    }
                }
                TeamCommands::Worker { once, watch, interval, id, lease_ttl, json } => {
                    if !once && !watch {
                        anyhow::bail!("team worker needs --once (single pass) or --watch (loop)");
                    }
                    let do_pass = || -> anyhow::Result<()> {
                        let report = h5i_core::team::worker_once(git, &id, lease_ttl, &actor)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&report)?);
                        } else {
                            println!(
                                "{} worker {} inspected {} team{}; finalized {}",
                                SUCCESS,
                                report.worker_id,
                                report.inspected,
                                if report.inspected == 1 { "" } else { "s" },
                                report.finalized.len()
                            );
                            for s in &report.skipped {
                                println!("  - {s}");
                            }
                        }
                        Ok(())
                    };
                    if watch {
                        eprintln!(
                            "{} team worker watching every {interval}s (finalize-only, never auto-applies); Ctrl-C to stop",
                            LOOKING
                        );
                        loop {
                            do_pass()?;
                            std::thread::sleep(std::time::Duration::from_secs(interval));
                        }
                    } else {
                        do_pass()?;
                    }
                }
                TeamCommands::Dispatch { team, prompt_file, json } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let prompt = match prompt_file {
                        Some(path) => std::fs::read_to_string(&path).map_err(|e| {
                            anyhow::anyhow!("failed to read prompt file {}: {e}", path.display())
                        })?,
                        None => {
                            use std::io::{IsTerminal, Read};
                            if std::io::stdin().is_terminal() {
                                anyhow::bail!(
                                    "no task prompt given — pipe one in (e.g. `h5i team dispatch {team} < TASK.md`) or pass --prompt-file <path>"
                                );
                            }
                            let mut s = String::new();
                            std::io::stdin().read_to_string(&mut s).map_err(|e| {
                                anyhow::anyhow!("failed to read prompt from stdin: {e}")
                            })?;
                            s
                        }
                    };
                    if prompt.trim().is_empty() {
                        anyhow::bail!("task prompt is empty");
                    }
                    let messages =
                        h5i_core::team::dispatch(git, &h5i_root, &team, &prompt, &actor)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&messages)?);
                    } else {
                        println!(
                            "{} dispatched to {} agent{}",
                            SUCCESS,
                            messages.len(),
                            if messages.len() == 1 { "" } else { "s" }
                        );
                    }
                }
                TeamCommands::Discuss {
                    team,
                    from,
                    to,
                    file,
                    artifacts,
                    json,
                } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let body = std::fs::read_to_string(&file).map_err(|e| {
                        anyhow::anyhow!("failed to read discussion file {}: {e}", file.display())
                    })?;
                    let recipients: Vec<String> = to
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    let artifact_ids: Vec<String> = artifacts
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    let discussion = h5i_core::team::discuss(
                        git,
                        &h5i_root,
                        &team,
                        &from,
                        recipients,
                        body,
                        artifact_ids,
                        &actor,
                    )?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&discussion)?);
                    } else {
                        println!(
                            "{} discussion {} -> {}",
                            SUCCESS,
                            discussion.sender,
                            discussion.recipients.join(", ")
                        );
                    }
                }
                TeamCommands::GrantReview {
                    team,
                    reviewer,
                    target,
                    artifacts,
                    json,
                } => {
                    let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                    let kinds: Vec<String> = artifacts
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    let grant = h5i_core::team::grant_review(
                        git, &h5i_root, &team, &reviewer, &target, kinds, &actor,
                    )?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&grant)?);
                    } else {
                        println!(
                            "{} granted {} review access to {} ({})",
                            SUCCESS,
                            reviewer,
                            target,
                            grant.artifact_ids.join(", ")
                        );
                    }
                }
                TeamCommands::Review { action } => match action {
                    TeamReviewCommands::Submit { team, reviewer, target, file, json } => {
                        let body = std::fs::read_to_string(&file).map_err(|e| {
                            anyhow::anyhow!("failed to read review file {}: {e}", file.display())
                        })?;
                        // In a confined box the team store is host-only: stage the
                        // review for ingest at session end (the host records it under
                        // the identity-validated env binding, so the box-supplied
                        // `--reviewer` is advisory only).
                        let in_env_spool = std::env::var_os(
                            h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR,
                        )
                        .map(PathBuf::from);
                        let in_box = in_env_spool.is_some()
                            && std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok()
                            && std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_ok();
                        if let (true, Some(spool)) = (in_box, in_env_spool) {
                            let staged = h5i_core::env::write_team_review_spool(
                                &spool,
                                &h5i_core::env::TeamReviewSpool {
                                    target: target.clone(),
                                    body,
                                },
                            )?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "staged": staged,
                                        "host_ingest": "env-shell-exit"
                                    }))?
                                );
                            } else {
                                println!(
                                    "{} team review staged for host ingest ({})",
                                    style("▢").cyan().dim(),
                                    staged
                                );
                            }
                        } else {
                            let team = h5i_core::team::resolve_run(&h5i_root, team)?;
                            let review = h5i_core::team::submit_review(
                                git, &h5i_root, &team, &reviewer, &target, body, &actor,
                            )?;
                            if json {
                                println!("{}", serde_json::to_string_pretty(&review)?);
                            } else {
                                println!(
                                    "{} recorded review {} -> {}",
                                    SUCCESS, review.reviewer, review.target
                                );
                            }
                        }
                    }
                },
                TeamCommands::Agent { action } => match action {
                    TeamAgentCommands::Inbox {
                        team,
                        peek,
                        wait,
                        interval,
                        timeout,
                        json,
                    } => {
                        // Confined box: read the host-fanned per-env read-only inbox
                        // ($H5I_ENV_INBOX); the shared msg store and team refs are
                        // sealed here. (Workspace tier has no inbox and falls through
                        // to the host path, which it can reach unconfined.)
                        if std::env::var_os(h5i_core::env::H5I_ENV_INBOX_VAR).is_some() {
                            use std::io::Write as _;
                            let agent = std::env::var("H5I_AGENT").unwrap_or_default();
                            let render = |unread: &[msg::Message]| -> anyhow::Result<()> {
                                if json {
                                    println!("{}", serde_json::to_string_pretty(unread)?);
                                } else if unread.is_empty() {
                                    println!(
                                        "{} No new team messages for {}.",
                                        SUCCESS,
                                        style(&agent).green().bold()
                                    );
                                } else {
                                    print_messages_numbered(unread, &agent, false);
                                }
                                Ok(())
                            };
                            if wait {
                                let interval = interval.max(1);
                                let mut waited = 0u64;
                                loop {
                                    let unread = box_team_inbox(false).unwrap_or_default();
                                    if !unread.is_empty() {
                                        render(&unread)?;
                                        let _ = std::io::stdout().flush();
                                        break;
                                    }
                                    if timeout != 0 && waited >= timeout {
                                        break;
                                    }
                                    std::thread::sleep(std::time::Duration::from_secs(interval));
                                    waited += interval;
                                }
                                return Ok(());
                            }
                            // Non-wait: consume unless --peek (advances the cursor).
                            let unread = box_team_inbox(!peek).unwrap_or_default();
                            render(&unread)?;
                            return Ok(());
                        }
                        let agent = msg::resolve_identity(&h5i_root, None)?;
                        let team = match team {
                            Some(t) => t,
                            None => std::env::var(h5i_core::env::H5I_TEAM_VAR)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| {
                                    h5i_core::team::resolve_run(&h5i_root, None)
                                        .unwrap_or_default()
                                }),
                        };
                        if team.is_empty() {
                            anyhow::bail!(
                                "no team set — run from `h5i env shell` for a team env, pass TEAM, or run `h5i team use <name>`"
                            );
                        }
                        let status = h5i_core::team::status(git, &team)?;
                        if !status.run.agents.iter().any(|a| a.agent_id == agent) {
                            anyhow::bail!("team '{team}' has no agent '{agent}'");
                        }
                        if wait {
                            use std::io::Write as _;
                            // Host-side (or unconfined workspace tier): poll the
                            // shared store directly. Confined boxes were handled by
                            // the env-inbox branch above.
                            let interval = interval.max(1);
                            let mut waited = 0u64;
                            loop {
                                // Re-open per poll so a concurrent host write is seen.
                                let repo = H5iRepository::open(".")?;
                                // Peek (never consume): the woken agent runs
                                // `h5i team agent inbox` to consume + number for reply.
                                let unread =
                                    msg::inbox(repo.git(), &repo.h5i_root, &agent, false)?;
                                if !unread.is_empty() {
                                    if json {
                                        println!("{}", serde_json::to_string_pretty(&unread)?);
                                    } else {
                                        print_messages_numbered(&unread, &agent, false);
                                    }
                                    let _ = std::io::stdout().flush();
                                    break; // first delivery is the wake signal
                                }
                                if timeout != 0 && waited >= timeout {
                                    break; // give up quietly (exit 0, no output)
                                }
                                std::thread::sleep(std::time::Duration::from_secs(interval));
                                waited += interval;
                            }
                            return Ok(());
                        }
                        let unread = msg::inbox(git, &h5i_root, &agent, !peek)?;
                        let ids: Vec<String> = unread.iter().map(|m| m.id.clone()).collect();
                        msg::write_last_view(&h5i_root, &agent, &ids)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&unread)?);
                        } else if unread.is_empty() {
                            println!(
                                "{} No new team messages for {}.",
                                SUCCESS,
                                style(&agent).green().bold()
                            );
                        } else {
                            println!(
                                "{} {} new team message{} for {}{}\n",
                                STEP,
                                style(unread.len()).cyan().bold(),
                                if unread.len() == 1 { "" } else { "s" },
                                style(&agent).green().bold(),
                                if peek {
                                    style(" (peek)").dim().to_string()
                                } else {
                                    String::new()
                                },
                            );
                            print_messages_numbered(&unread, &agent, false);
                        }
                    }
                    TeamAgentCommands::Hook {
                        team,
                        block,
                        quiet,
                        timeout,
                        interval,
                    } => {
                        let interval = interval.max(1);
                        // The framed messages plus a standing instruction; the hook
                        // keeps waiting between turns, so the agent need not run
                        // `inbox --wait` itself.
                        let block_reason = |text: &str| -> String {
                            format!(
                                "{text}\n\n[h5i team] Handle the request(s) above — post a review \
                                 with `h5i team review submit` and/or improve and re-submit with \
                                 `h5i team agent submit`. Submitting marks you done for this round \
                                 and releases you until the next round opens — no need to poll."
                            )
                        };
                        let is_done =
                            |m: &msg::Message| m.kind.as_deref() == Some(h5i_core::team::TEAM_DONE_KIND);

                        // Confined box: deliver from the host-fanned per-env inbox
                        // (the shared store and team refs are sealed here). Consume
                        // so a message isn't re-delivered on the next stop.
                        if std::env::var_os(h5i_core::env::H5I_ENV_INBOX_VAR).is_some() {
                            let agent = std::env::var("H5I_AGENT").unwrap_or_default();
                            // "Submit == done for this round": once we've submitted
                            // for round R, drop round-≤R review messages so the host
                            // re-fanning the same standing requests can't loop us.
                            // Newer rounds (higher round) and non-team messages (no
                            // round) still break through. The round we last submitted
                            // for is recorded box-side by `team agent submit`.
                            let submitted_round =
                                std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR)
                                    .map(PathBuf::from)
                                    .as_deref()
                                    .and_then(h5i_core::env::read_submitted_round);
                            let still_pending = |msgs: Vec<msg::Message>| -> Vec<msg::Message> {
                                msgs.into_iter()
                                    .filter(|m| match (submitted_round, msg_round(m)) {
                                        (Some(s), Some(r)) => r > s,
                                        _ => true,
                                    })
                                    .collect()
                            };
                            if !block {
                                // Codex / one-shot: surface pending mail, never wait.
                                let unread =
                                    still_pending(box_team_inbox(true).unwrap_or_default());
                                if !unread.is_empty() {
                                    let text = frame_unread(&agent, &unread);
                                    if quiet {
                                        println!("{text}");
                                    } else {
                                        let out = serde_json::json!({ "systemMessage": text });
                                        println!("{}", serde_json::to_string(&out)?);
                                    }
                                }
                                return Ok(());
                            }
                            // Claude / --block: wait for the next message, then block
                            // the stop and feed it back. A round-done signal or the
                            // wait elapsing releases the agent so it can stop.
                            let mut waited = 0u64;
                            loop {
                                let raw = box_team_inbox(true).unwrap_or_default();
                                if raw.iter().any(&is_done) {
                                    return Ok(());
                                }
                                let unread = still_pending(raw);
                                if !unread.is_empty() {
                                    let reason = block_reason(&frame_unread(&agent, &unread));
                                    let out =
                                        serde_json::json!({ "decision": "block", "reason": reason });
                                    println!("{}", serde_json::to_string(&out)?);
                                    return Ok(());
                                }
                                if timeout == 0 || waited >= timeout {
                                    return Ok(());
                                }
                                std::thread::sleep(std::time::Duration::from_secs(interval));
                                waited += interval;
                            }
                        }

                        // Host-side (or unconfined workspace tier): read the shared
                        // store directly; the run phase tells us when to release.
                        let Ok(agent) = msg::resolve_identity(&h5i_root, None) else {
                            return Ok(());
                        };
                        let team = match team {
                            Some(t) => t,
                            None => std::env::var(h5i_core::env::H5I_TEAM_VAR)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| {
                                    h5i_core::team::resolve_run(&h5i_root, None)
                                        .unwrap_or_default()
                                }),
                        };
                        if team.is_empty() {
                            return Ok(());
                        }
                        let mut waited = 0u64;
                        loop {
                            let repo = H5iRepository::open(".")?;
                            let Ok(status) = h5i_core::team::status(repo.git(), &team) else {
                                return Ok(());
                            };
                            // Not on this team, or the run is decided → let it stop.
                            if !status.run.agents.iter().any(|a| a.agent_id == agent) {
                                return Ok(());
                            }
                            if matches!(status.run.phase.as_str(), "applied" | "no_verdict") {
                                return Ok(());
                            }
                            let unread = msg::inbox(repo.git(), &repo.h5i_root, &agent, false)?;
                            if !unread.is_empty() {
                                let ids: Vec<String> =
                                    unread.iter().map(|m| m.id.clone()).collect();
                                msg::write_last_view(&repo.h5i_root, &agent, &ids)?;
                                let text = frame_unread(&agent, &unread);
                                if block {
                                    let reason = block_reason(&text);
                                    let out = serde_json::json!({ "decision": "block", "reason": reason });
                                    println!("{}", serde_json::to_string(&out)?);
                                } else if quiet {
                                    println!("{text}");
                                } else {
                                    let out = serde_json::json!({ "systemMessage": text });
                                    println!("{}", serde_json::to_string(&out)?);
                                }
                                msg::mark_seen(&repo.h5i_root, &agent, &ids)?;
                                return Ok(());
                            }
                            // Nothing pending. One-shot (non-block) → stop now;
                            // --block → wait until a message arrives or the timeout.
                            if !block || timeout == 0 || waited >= timeout {
                                return Ok(());
                            }
                            std::thread::sleep(std::time::Duration::from_secs(interval));
                            waited += interval;
                        }
                    }
                    TeamAgentCommands::Submit { commit, summary_file, json } => {
                        let summary = match summary_file {
                            Some(path) => Some(std::fs::read_to_string(&path).map_err(|e| {
                                anyhow::anyhow!(
                                    "failed to read summary file {}: {e}",
                                    path.display()
                                )
                            })?),
                            None => None,
                        };
                        let in_env_spool =
                            std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR)
                                .map(PathBuf::from);
                        let in_box = in_env_spool.is_some()
                            && std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok()
                            && std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_ok();
                        if let (true, Some(spool)) = (in_box, in_env_spool) {
                            // Snapshot the worktree onto the env branch *in-box*,
                            // before staging, unless the caller pinned an explicit
                            // commit. The host can't snapshot a live box (it holds
                            // the run lock all session), so the box commits its own
                            // edits here — otherwise an agent that wrote files but
                            // never `git commit`ed would stage a no-op that the
                            // host refuses ("identical to the team base").
                            if commit.is_none() {
                                match h5i_core::env::commit_box_worktree() {
                                    Ok(Some(oid)) => eprintln!(
                                        "{} snapshotted worktree in-box at {}",
                                        style("▢").cyan().dim(),
                                        &oid.to_string()[..12]
                                    ),
                                    Ok(None) => {}
                                    // Surface the failure but don't block the
                                    // submit — the host freezes the branch tip.
                                    // A silent failure here is what makes work
                                    // vanish into a "no changes to review" no-op.
                                    Err(e) => eprintln!(
                                        "warning: in-box worktree snapshot failed \
                                         (submit will use the branch tip — commit \
                                         your work in-box if this persists): {e}"
                                    ),
                                }
                            }
                            let request = h5i_core::env::TeamSubmitSpool { commit, summary };
                            let staged =
                                h5i_core::env::write_team_submit_spool(&spool, &request)?;
                            // Submit == "done for this round": record the round so
                            // the Stop hook stops re-surfacing this round's standing
                            // review messages. The box can't read team state, but
                            // the round rides in the inbox messages' i5h links.
                            if let Some(inbox) =
                                std::env::var_os(h5i_core::env::H5I_ENV_INBOX_VAR).map(PathBuf::from)
                            {
                                if let Some(round) = h5i_core::env::read_env_inbox(&inbox)
                                    .iter()
                                    .filter_map(msg_round)
                                    .max()
                                {
                                    let _ = h5i_core::env::write_submitted_round(&spool, round);
                                }
                            }
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "staged": staged,
                                        "host_ingest": "env-shell-exit"
                                    }))?
                                );
                            } else {
                                println!(
                                    "{} team submit staged for host ingest ({})",
                                    style("▢").cyan().dim(),
                                    staged
                                );
                            }
                        } else {
                            let team = std::env::var(h5i_core::env::H5I_TEAM_VAR)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .map(Ok)
                                .unwrap_or_else(|| h5i_core::team::resolve_run(&h5i_root, None))?;
                            let agent = msg::resolve_identity(&h5i_root, None)?;
                            // Auto-commit the worktree before submitting, mirroring
                            // the in-box (process+/container) path — otherwise a
                            // workspace-tier agent that made changes but didn't
                            // `git commit` would submit a no-op (branch tip == base)
                            // and lose its work. Only when boxed (H5I_TEAM set) and
                            // no explicit commit was pinned, so a human running
                            // `team agent submit` from the main repo is untouched.
                            let boxed = std::env::var_os(h5i_core::env::H5I_TEAM_VAR).is_some();
                            if boxed && commit.is_none() {
                                match h5i_core::env::commit_box_worktree() {
                                    Ok(Some(oid)) => eprintln!(
                                        "{} snapshotted worktree at {}",
                                        style("▢").cyan().dim(),
                                        &oid.to_string()[..12]
                                    ),
                                    Ok(None) => {}
                                    Err(e) => eprintln!(
                                        "warning: worktree snapshot failed (submit will use \
                                         the branch tip — commit your work if this persists): {e}"
                                    ),
                                }
                            }
                            let artifact = h5i_core::team::submit(
                                git,
                                &h5i_root,
                                &team,
                                &agent,
                                commit.as_deref(),
                                summary,
                                &agent,
                            )?;
                            if json {
                                println!("{}", serde_json::to_string_pretty(&artifact)?);
                            } else {
                                println!(
                                    "{} submitted {} for {} at {}",
                                    SUCCESS,
                                    artifact.id,
                                    artifact.owner_agent,
                                    &artifact.commit_oid[..12.min(artifact.commit_oid.len())]
                                );
                            }
                        }
                    }
                    TeamAgentCommands::Reply { text, file, team, json } => {
                        let body = match (text, file) {
                            (Some(t), _) => t,
                            (None, Some(path)) => {
                                std::fs::read_to_string(&path).map_err(|e| {
                                    anyhow::anyhow!(
                                        "failed to read reply file {}: {e}",
                                        path.display()
                                    )
                                })?
                            }
                            (None, None) => {
                                anyhow::bail!("team agent reply needs a body (text or --file)")
                            }
                        };
                        let in_env_spool =
                            std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR)
                                .map(PathBuf::from);
                        let in_box = in_env_spool.is_some()
                            && std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok()
                            && std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_ok();
                        if let (true, Some(spool)) = (in_box, in_env_spool) {
                            let request = h5i_core::env::TeamReplySpool { body };
                            let staged =
                                h5i_core::env::write_team_reply_spool(&spool, &request)?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "staged": staged,
                                        "host_ingest": "env-shell-exit"
                                    }))?
                                );
                            } else {
                                println!(
                                    "{} team reply staged for host ingest ({})",
                                    style("▢").cyan().dim(),
                                    staged
                                );
                            }
                        } else {
                            let team = std::env::var(h5i_core::env::H5I_TEAM_VAR)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .map(Ok)
                                .unwrap_or_else(|| {
                                    h5i_core::team::resolve_run(&h5i_root, team)
                                })?;
                            let agent = msg::resolve_identity(&h5i_root, None)?;
                            h5i_core::team::record_agent_reply(git, &team, &agent, body)?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "recorded": true, "team": team, "agent": agent
                                    }))?
                                );
                            } else {
                                println!("{} reply recorded for {agent} on {team}", SUCCESS);
                            }
                        }
                    }
                },
            }
        }
    Ok(())
}
