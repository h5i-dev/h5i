//! `h5i context` — CLI handlers (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Initialize the `.h5i-ctx/` reasoning workspace for this project
    Init {
        /// High-level project goal written to main.md
        #[arg(long, default_value = "")]
        goal: String,
    },

    /// Checkpoint the agent's current progress as a structured milestone
    /// (like `git commit` but for the reasoning workspace)
    Commit {
        /// One-line summary of what was accomplished
        summary: String,
        /// Detailed description of this commit's contribution
        #[arg(long, default_value = "")]
        detail: String,
    },

    /// Create a new isolated reasoning branch for exploring an alternative
    /// (like `git branch` but for the `.h5i-ctx/` workspace)
    Branch {
        /// Branch name (e.g. "experiment/cache-strategy")
        name: String,
        /// Why this branch exists / what hypothesis it explores
        #[arg(long, default_value = "")]
        purpose: String,
    },

    /// Switch to an existing reasoning branch
    /// (like `git checkout` but for the `.h5i-ctx/` workspace)
    Checkout {
        /// Branch name to switch to
        name: String,
    },

    /// Merge a completed reasoning branch into the current branch
    /// (like `git merge` but for the `.h5i-ctx/` workspace)
    Merge {
        /// Name of the branch to merge in
        branch: String,
    },

    /// Retrieve the current project state at multiple levels of detail
    /// (like `git show` — global roadmap, recent commits, optional trace)
    ///
    /// Three depths inspired by progressive-disclosure retrieval:
    ///   --depth 1  compact index (~800 tokens): goal, branch, milestone IDs, counts
    ///   --depth 2  timeline (default, ~2-5K tokens): adds recent commits + mini-trace
    ///   --depth 3  full trace: adds the complete OTA log
    Show {
        /// Show context for this branch (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// Return the complete record for a specific commit hash
        #[arg(long)]
        commit: Option<String>,
        /// Include recent OTA execution trace from trace.md (equivalent to --depth 3)
        #[arg(long)]
        trace: bool,
        /// Retrieve a specific metadata segment from metadata.yaml (e.g. "file_structure")
        #[arg(long)]
        metadata: Option<String>,
        /// Number of recent commits to show (context window K)
        #[arg(long, default_value_t = 3)]
        window: usize,
        /// Show only the N most recent milestones (0 = all). Long-lived
        /// workspaces accumulate hundreds; this keeps the view focused.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Scroll back N lines in the trace (sliding-window offset k)
        #[arg(long, default_value_t = 0)]
        trace_offset: usize,
        /// Progressive disclosure depth: 1=compact index, 2=timeline (default), 3=full trace
        #[arg(long, default_value_t = 2)]
        depth: u8,
    },

    /// Append an OTA (Observation–Thought–Action) step to the current branch trace
    Trace {
        /// Step type: OBSERVE, THINK, ACT, or NOTE
        #[arg(long, default_value = "NOTE")]
        kind: String,
        /// Trace entry content
        content: String,
        /// Mark this entry as ephemeral (scratch-only, cleared on next context commit,
        /// not persisted to the DAG or snapshots — like Claude Code's /btw)
        #[arg(long)]
        ephemeral: bool,
    },

    /// Show the current reasoning workspace state (branch, counts, pin status)
    /// plus a short tail of recent trace entries
    Status {
        /// Number of recent trace entries to show (0 = none). Unlike
        /// `context show --trace`, this stays a *recent* view, not the full log.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    /// Print just the current goal + pin status — a cheap, low-token check to run
    /// at the start of a task (before `context init --goal`). Warns when context
    /// is pinned to a branch other than the current git branch (a stale pin that
    /// silently misroutes new traces), and suggests `context unpin`.
    Goal {
        /// Show the full goal history for the current git branch (newest first)
        /// instead of just the current goal.
        #[arg(long)]
        log: bool,
    },

    /// Resume auto-follow: remove the per-worktree context pin so the active
    /// h5i context branch tracks the current git branch on the next write.
    Unpin,

    /// Permanently remove a context (reasoning) branch
    /// (`refs/h5i/context/<name>`) — the safe counterpart to deleting the ref
    /// by hand. Refuses `main`, an env-owned `env/…` branch, and (without
    /// `--force`) the active branch. Per-commit workspace snapshots
    /// (`context restore`/`diff`) are preserved. Local only: if the branch was
    /// shared, delete it on the remote too or `share pull` will resurrect it.
    Rm {
        /// Context branch name to remove (e.g. `improve-shell`, `scope/foo`)
        name: String,
        /// Remove even if it is the active branch (resets HEAD to main + unpins)
        #[arg(long)]
        force: bool,
    },

    /// Print a system prompt for injecting h5i context commands into an agent session
    Prompt,

    /// Scan the reasoning trace for prompt-injection patterns and report a risk score
    Scan {
        /// Branch to scan (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },

    /// Restore the context workspace to the state captured at a given git commit
    Restore {
        /// Git commit SHA whose context snapshot to restore (prefix OK)
        sha: String,
    },

    /// Show how the context workspace evolved between two git commits
    Diff {
        /// Earlier git commit SHA (prefix OK)
        from: String,
        /// Later git commit SHA (prefix OK)
        to: String,
    },

    /// Show context workspace entries relevant to a specific file
    Relevant {
        /// File path to look up (e.g. src/repository.rs)
        file: String,
    },

    /// Compact old context history using three-pass structurally-lossless trimming.
    /// Pass 1: remove OBSERVE entries subsumed by a later THINK/ACT on the same topic.
    /// Pass 2: keep all THINK, ACT, NOTE entries verbatim.
    /// Pass 3: merge consecutive OBSERVE entries mentioning the same file.
    Pack,

    /// Create a subagent-scoped sub-context for isolated delegation.
    /// Scoped branches are prefixed `scope/` and shown separately in `status`.
    /// Merge them back with `h5i context merge scope/<name>` when the subagent finishes.
    Scope {
        /// Sub-context name (will be stored as `scope/<name>`)
        name: String,
        /// Why this scope exists / what the subagent is investigating
        #[arg(long, default_value = "")]
        purpose: String,
    },

    /// Show the ephemeral scratch traces for the current branch (cleared on context commit)
    Ephemeral {
        /// Branch to inspect (default: current)
        #[arg(long)]
        branch: Option<String>,
    },

    /// Show the stable-prefix / dynamic-suffix boundary for the current trace
    /// (useful for understanding prompt-caching efficiency)
    CachedPrefix {
        /// Number of dynamic (volatile) tail lines to exclude from stable prefix
        #[arg(long, default_value_t = 40)]
        tail: usize,
    },

    /// Show all open TODO / FIXME / BLOCKED items extracted from the trace.
    /// These are NOTE and THINK entries that contain actionable keywords.
    Todo,

    /// Distill all THINK entries across every context branch into a project knowledge base.
    /// Useful for reviewing every design decision ever recorded in this workspace.
    Knowledge,

    /// Render the per-branch trace DAG as a coloured graph in the terminal.
    /// Each node shows its kind (OBSERVE/THINK/ACT/NOTE/MERGE), 8-hex ID,
    /// timestamp, and content. Merge nodes display both parent IDs.
    Dag {
        /// Branch whose DAG to display (default: current branch)
        #[arg(long)]
        branch: Option<String>,
    },

    /// Import Claude Code "Recap" (`away_summary`) entries from the active
    /// session log as context commits. Idempotent — each recap UUID is
    /// recorded and skipped on subsequent runs.
    Recap {
        /// Explicit JSONL session file to scan (default: auto-detect latest)
        #[arg(long)]
        session: Option<PathBuf>,
        /// Only import recaps with an ISO-8601 timestamp after this cutoff
        /// (e.g. `2026-04-23T00:00:00Z`)
        #[arg(long)]
        since: Option<String>,
        /// Show what would be imported without modifying the workspace
        #[arg(long)]
        dry_run: bool,
    },

    /// Search context traces and session footprints for files relevant to a query.
    /// Combines BM25-style scoring over OBSERVE/THINK/ACT entries with git
    /// co-change analysis — no AST or embeddings required.
    Search {
        /// Natural-language query (e.g. "auth token expiry" or "retry logic")
        query: String,
        /// Maximum number of results to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Enrich top results with git co-change partners (walks last N commits)
        #[arg(long, default_value_t = 200)]
        history: usize,
        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },

    /// Recall task-aware prior context for any agent or workflow.
    Smart {
        /// Current task prompt/query to rank prior context against
        #[arg(long)]
        query: String,
        /// Maximum recalled file results to show
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
}

pub fn run(action: cli::context::ContextCommands) -> anyhow::Result<()> {
    {
            let workdir = Path::new(".");
            match action {
                ContextCommands::Init { goal } => {
                    ctx::init(workdir, &goal)?;
                    println!(
                        "{} {} at {}",
                        SUCCESS,
                        style(".h5i-ctx/ workspace initialized").green().bold(),
                        style(".h5i-ctx/").dim()
                    );
                    println!();
                    println!("  {}", style("Quick-start:").bold());
                    println!(
                        "    {}  checkpoint your progress",
                        style("h5i context commit \"summary\" --detail \"…\"").cyan()
                    );
                    println!(
                        "    {}  explore an alternative",
                        style("h5i context branch experiment/foo --purpose \"…\"").cyan()
                    );
                    println!(
                        "    {}  view current context",
                        style("h5i context show --trace").cyan()
                    );
                }

                ContextCommands::Commit { summary, detail } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(
                            ".h5i-ctx/ not initialized. Run `h5i context init --goal \"<goal>\"` first."
                        );
                    }
                    ctx::prepare_context_write(workdir)?;
                    ctx::gcc_commit(workdir, &summary, &detail)?;
                    println!(
                        "{} {} — {}",
                        SUCCESS,
                        style("Context commit recorded").green().bold(),
                        style(&summary).cyan()
                    );
                }

                ContextCommands::Branch { name, purpose } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    if purpose.trim().is_empty() {
                        anyhow::bail!(
                            "Context branch '{}' requires a purpose. Run `h5i context branch {} --purpose \"<intent>\"`.",
                            name,
                            name
                        );
                    }
                    ctx::gcc_branch(workdir, &name, &purpose)?;
                    println!(
                        "{} Created and switched to branch {}",
                        SUCCESS,
                        style(&name).magenta().bold()
                    );
                }

                ContextCommands::Checkout { name } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::gcc_checkout(workdir, &name)?;
                    println!(
                        "{} Switched to branch {}",
                        SUCCESS,
                        style(&name).magenta().bold()
                    );
                }

                ContextCommands::Merge { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let target = ctx::current_branch(workdir);
                    let summary = ctx::gcc_merge(workdir, &branch)?;
                    println!(
                        "{} Merged {} into {}",
                        SUCCESS,
                        style(&branch).magenta(),
                        style(&target).magenta().bold()
                    );
                    println!("{}", style(&summary).dim());
                }

                ContextCommands::Show {
                    branch,
                    commit,
                    trace,
                    metadata,
                    window,
                    limit,
                    trace_offset,
                    depth,
                } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    // --trace is shorthand for --depth 3
                    let effective_depth = if trace { 3 } else { depth };
                    let opts = ctx::ContextOpts {
                        branch,
                        commit_hash: commit,
                        show_log: effective_depth >= 3,
                        log_offset: trace_offset,
                        metadata_segment: metadata,
                        window,
                        depth: effective_depth,
                    };
                    let mut snapshot = ctx::gcc_context(workdir, &opts)?;
                    // Cap milestones to the most recent N (--limit, 0 = all); the
                    // renderer notes how many older ones are hidden.
                    ctx::limit_recent_milestones(&mut snapshot, limit);
                    ctx::print_context_depth(&snapshot, effective_depth);
                }

                ContextCommands::Trace {
                    kind,
                    content,
                    ephemeral,
                } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(
                            ".h5i-ctx/ not initialized. Run `h5i context init --goal \"<goal>\"` first."
                        );
                    }
                    ctx::prepare_context_write(workdir)?;
                    ctx::append_log(workdir, &kind, &content, ephemeral)?;
                    let marker = if ephemeral {
                        style("◇").dim()
                    } else {
                        style("◈").cyan()
                    };
                    println!(
                        "{} [{}] {}",
                        marker,
                        style(kind.to_uppercase()).bold(),
                        style(&content).dim()
                    );
                }

                ContextCommands::Status { limit } => {
                    ctx::print_status(workdir, limit)?;
                    // Feature 5: append proactive review surface if git repo + notes exist.
                    if let Ok(repo) = H5iRepository::open(workdir) {
                        if let Ok(pts) = repo.suggest_review_points(3, 0.4) {
                            if !pts.is_empty() {
                                println!();
                                println!(
                                    "  {}",
                                    style("Commits flagged for review:").yellow().bold()
                                );
                                for pt in &pts {
                                    println!(
                                        "    {} {} score {:.2}  {}",
                                        style("⚑").red(),
                                        style(&pt.short_oid).dim(),
                                        pt.score,
                                        style(&pt.message).italic(),
                                    );
                                    for trig in pt.triggers.iter().take(2) {
                                        println!(
                                            "      {} {}",
                                            style("·").dim(),
                                            style(&trig.detail).dim()
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                ContextCommands::Goal { log } => {
                    if log {
                        ctx::print_goal_log(workdir)?;
                    } else {
                        ctx::print_goal(workdir)?;
                    }
                }

                ContextCommands::Unpin => {
                    ctx::unpin(workdir)?;
                    println!(
                        "{} context unpinned; auto-follow will track the current git branch",
                        SUCCESS
                    );
                }

                ContextCommands::Rm { name, force } => {
                    let outcome = ctx::rm_branch(workdir, &name, force)?;
                    println!(
                        "{} removed context branch {} ({} trace line{} dropped)",
                        SUCCESS,
                        style(&outcome.name).magenta().bold(),
                        outcome.trace_lines,
                        if outcome.trace_lines == 1 { "" } else { "s" },
                    );
                    if outcome.was_active {
                        println!(
                            "  {} it was the active branch — HEAD reset to {} and unpinned",
                            style("·").dim(),
                            style("main").magenta(),
                        );
                    }
                    println!(
                        "  {} per-commit snapshots kept — recover via {}",
                        style("·").dim(),
                        style("h5i context restore <sha>").cyan(),
                    );
                    println!(
                        "  {} local only: if shared, also delete it on the remote or {} will resurrect it",
                        style("·").dim(),
                        style("h5i share pull").cyan(),
                    );
                }

                ContextCommands::Prompt => {
                    print!("{}", ctx::system_prompt(workdir));
                }

                ContextCommands::Scan { branch, json } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let branch_ref = branch.as_deref();
                    let trace = ctx::read_trace(workdir, branch_ref)?;
                    let branch_label =
                        branch_ref.unwrap_or_else(|| ctx::current_branch(workdir).leak());
                    let result = h5i_core::injection::scan(&trace);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    } else {
                        h5i_core::injection::print_scan_result(&result, branch_label);
                    }
                }

                ContextCommands::Restore { sha } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let summary = ctx::restore(workdir, &sha)?;
                    println!(
                        "{} {} {}",
                        SUCCESS,
                        style("Context restored:").green().bold(),
                        style(&summary).dim()
                    );
                    println!(
                        "  {} Run {} to verify the restored state.",
                        style("→").dim(),
                        style("h5i context show --trace").cyan()
                    );
                }

                ContextCommands::Diff { from, to } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let diff = ctx::context_diff(workdir, &from, &to)?;
                    ctx::print_context_diff(&diff);
                }

                ContextCommands::Relevant { file } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let ctx_result = ctx::relevant(workdir, &file)?;
                    ctx::print_relevant(&ctx_result, &file);
                }

                ContextCommands::Pack => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let result = ctx::pack_lossless(workdir)?;
                    if result.kept_durable == 0
                        && result.removed_subsumed_observe == 0
                        && result.merged_consecutive_observe == 0
                    {
                        println!(
                            "{} Nothing to pack — context history is already compact.",
                            style("ℹ").blue()
                        );
                    } else {
                        println!("{} Three-pass lossless pack complete:", SUCCESS);
                        if result.removed_subsumed_observe > 0 {
                            println!(
                                "  {} {} subsumed OBSERVE entries removed",
                                style("−").red(),
                                style(result.removed_subsumed_observe).cyan().bold()
                            );
                        }
                        if result.merged_consecutive_observe > 0 {
                            println!(
                                "  {} {} consecutive OBSERVE entries merged",
                                style("⇒").yellow(),
                                style(result.merged_consecutive_observe).cyan().bold()
                            );
                        }
                        println!(
                            "  {} {} THINK/ACT/NOTE entries preserved verbatim",
                            style("✔").green(),
                            style(result.kept_durable).cyan().bold()
                        );
                        println!(
                            "  {} Run {} to reclaim disk space.",
                            style("→").dim(),
                            style("git gc").cyan()
                        );
                    }
                }

                ContextCommands::Scope { name, purpose } => {
                    let full_name = if name.starts_with("scope/") {
                        name.clone()
                    } else {
                        format!("scope/{name}")
                    };
                    let purpose_text = if purpose.is_empty() {
                        format!("Subagent scope: {name}")
                    } else {
                        purpose.clone()
                    };
                    ctx::gcc_scope(workdir, &full_name, &purpose_text)?;
                    println!(
                        "{} Scope {} created and activated.",
                        SUCCESS,
                        style(&full_name).magenta().bold()
                    );
                    println!(
                        "  {} Merge findings back with {}",
                        style("→").dim(),
                        style(format!("h5i context merge {full_name}")).cyan()
                    );
                }

                ContextCommands::Ephemeral { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let text = ctx::read_ephemeral(workdir, branch.as_deref())?;
                    if text
                        .lines()
                        .filter(|l| !l.starts_with('#') && !l.is_empty())
                        .count()
                        == 0
                    {
                        println!(
                            "{} No ephemeral traces (cleared on last context commit).",
                            style("ℹ").blue()
                        );
                    } else {
                        println!(
                            "{}",
                            style("── Ephemeral Traces (scratch, not persisted) ──────────────")
                                .dim()
                        );
                        for line in text.lines().filter(|l| !l.starts_with('#')) {
                            println!("  {}", style(line).dim());
                        }
                    }
                }

                ContextCommands::CachedPrefix { tail } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_cached_prefix(workdir, tail)?;
                }

                ContextCommands::Todo => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_todos(workdir)?;
                }

                ContextCommands::Knowledge => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_knowledge(workdir)?;
                }

                ContextCommands::Dag { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_dag(workdir, branch.as_deref())?;
                }

                ContextCommands::Recap {
                    session,
                    since,
                    dry_run,
                } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }

                    let cutoff = match since {
                        Some(s) => Some(
                            s.parse::<chrono::DateTime<chrono::Utc>>()
                                .map_err(|e| anyhow::anyhow!("invalid --since timestamp: {e}"))?,
                        ),
                        None => None,
                    };

                    // Session-log discovery matches on absolute cwd, so resolve first.
                    let scan_dir =
                        std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());

                    let opts = h5i_core::recap::ImportOpts {
                        since: cutoff,
                        session_path: session,
                        dry_run,
                    };

                    let results = h5i_core::recap::import_recaps(&scan_dir, &opts)?;

                    let imported: Vec<_> = results.iter().filter(|r| !r.skipped).collect();
                    let skipped: Vec<_> = results.iter().filter(|r| r.skipped).collect();

                    if results.is_empty() {
                        println!("{} No recaps found in session log.", style("·").dim());
                    } else {
                        let verb = if dry_run { "would import" } else { "imported" };
                        println!(
                            "{} {} {} new recap(s){}",
                            SUCCESS,
                            style(verb).green().bold(),
                            style(imported.len()).cyan(),
                            if skipped.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} already imported", skipped.len())
                            },
                        );
                        for r in &imported {
                            let (summary, _) =
                                h5i_core::recap::split_summary_detail(&r.recap.content);
                            let display = if summary.is_empty() {
                                r.recap.uuid.clone()
                            } else {
                                summary
                            };
                            let short = r.recap.uuid.get(..8).unwrap_or(&r.recap.uuid);
                            println!(
                                "  {} {}  {}",
                                style("✓").green(),
                                style(short).dim(),
                                display,
                            );
                        }
                    }
                }

                ContextCommands::Search {
                    query,
                    limit,
                    history,
                    json,
                } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let mut results = ctx::search(workdir, &query, limit)?;

                    // Enrich top results with git co-change data
                    if let Ok(repo) = H5iRepository::open(workdir) {
                        for r in results.iter_mut().take(5) {
                            if let Ok(cochanged) = repo.cochanged_files(&r.file, history, 5) {
                                r.cochanged_with = cochanged.into_iter().map(|(f, _)| f).collect();
                            }
                        }
                    }

                    if json {
                        let out: Vec<serde_json::Value> = results
                            .iter()
                            .map(|r| {
                                serde_json::json!({
                                    "file": r.file,
                                    "score": r.score,
                                    "signal": r.signal,
                                    "snippets": r.snippets,
                                    "cochanged_with": r.cochanged_with,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    } else {
                        ctx::print_search_results(&results, &query);
                    }
                }

                ContextCommands::Smart { query, limit } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let recall = ctx::smart_recall(workdir, &query, limit)?;
                    print_smart_recall(&recall);
                }
            }
        }
    Ok(())
}
