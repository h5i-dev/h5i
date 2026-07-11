//! `h5i memory` — CLI handlers (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum MemoryCommands {
    /// Snapshot agent memory into .git/.h5i/memory/<commit-oid>/
    Snapshot {
        /// Git commit OID to associate this snapshot with (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Agent memory backend to snapshot (default: inferred from H5I_AGENT_ID, else claude)
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
        /// Override the source directory to snapshot
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },

    /// Show how agent memory changed between two snapshots
    Diff {
        /// Snapshot to diff from (default: second-to-last snapshot)
        from: Option<String>,
        /// Snapshot to diff to; omit to compare against live memory (default: latest snapshot)
        to: Option<String>,
        /// Agent memory backend to compare against when diffing to live state
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
    },

    /// List all memory snapshots
    Log,

    /// Restore agent memory to the state captured in a snapshot
    Restore {
        /// Commit OID whose snapshot to restore
        commit: String,
        /// Agent memory backend to restore into
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Push the latest memory snapshot to a git remote via refs/h5i/memory
    Push {
        /// Remote to push to
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },

    /// Fetch a teammate's memory snapshot from a git remote
    Pull {
        /// Remote to pull from
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },
}

pub fn run(action: MemoryCommands) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                .to_path_buf();

            match action {
                MemoryCommands::Snapshot {
                    commit,
                    path,
                    agent,
                } => {
                    // Resolve commit OID: explicit arg or HEAD
                    let oid_str = match commit {
                        Some(ref s) => s.clone(),
                        None => {
                            let head = repo.git().head()?;
                            head.peel_to_commit()?.id().to_string()
                        }
                    };

                    let memory_agent = resolve_memory_agent(agent);
                    let src = path.as_deref();
                    let default_dir = memory::default_memory_dir(&workdir, memory_agent);
                    let display_src = src.unwrap_or(&default_dir).display().to_string();

                    println!(
                        "{} {} → commit {}",
                        STEP,
                        style(format!("Snapshotting {} memory", memory_agent.label()))
                            .cyan()
                            .bold(),
                        style(&oid_str[..8.min(oid_str.len())]).magenta()
                    );

                    let count = memory::take_snapshot(
                        &repo.h5i_root,
                        &workdir,
                        &oid_str,
                        src,
                        memory_agent,
                    )?;

                    if count == 0 {
                        println!(
                            "{} {} at {}",
                            WARN,
                            style("No memory files found — empty snapshot recorded.").yellow(),
                            style(&display_src).dim()
                        );
                        println!(
                            "  {} {} may create this directory lazily on the first memory write.",
                            style("ℹ").blue(),
                            style(memory_agent.label()).cyan()
                        );
                        println!(
                            "  {} You can also snapshot any directory with {}",
                            style("ℹ").blue(),
                            style("h5i memory snapshot --path <dir>").bold()
                        );
                    } else {
                        println!(
                            "{} Saved {} file{} from {}",
                            SUCCESS,
                            style(count).cyan(),
                            if count == 1 { "" } else { "s" },
                            style(&display_src).dim()
                        );
                    }
                }

                MemoryCommands::Diff { from, to, agent } => {
                    // Default: diff last two snapshots (or last snapshot vs. live)
                    let snapshots = memory::list_snapshots(&repo.h5i_root)?;
                    let memory_agent = resolve_memory_agent(agent);

                    let (from_oid, to_oid_opt): (String, Option<String>) = match (from, to) {
                        (Some(f), t) => (f, t),
                        (None, Some(t)) => {
                            // from = latest snapshot, to = specified
                            let latest = snapshots.last().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No snapshots found. Run `h5i memory snapshot` first."
                                )
                            })?;
                            (latest.commit_oid.clone(), Some(t))
                        }
                        (None, None) => {
                            // from = second-to-last, to = live
                            let Some(latest) = snapshots.last() else {
                                println!(
                                    "{} No snapshots yet. Run {} first.",
                                    WARN,
                                    style("h5i memory snapshot").bold()
                                );
                                return Ok(());
                            };
                            (latest.commit_oid.clone(), None) // to=None means live
                        }
                    };

                    let to_label = to_oid_opt.as_deref().unwrap_or("live");
                    println!(
                        "{} {} {}..{}",
                        LOOKING,
                        style("Computing memory diff").cyan().bold(),
                        style(&from_oid[..8.min(from_oid.len())]).magenta(),
                        style(to_label).magenta()
                    );

                    let diff = memory::diff_snapshots(
                        &repo.h5i_root,
                        &workdir,
                        &from_oid,
                        to_oid_opt.as_deref(),
                        memory_agent,
                    )?;
                    memory::print_memory_diff(&diff);
                }

                MemoryCommands::Log => {
                    println!("{}\n", style("Claude Memory Snapshots").bold().underlined());
                    memory::print_memory_log(&repo.h5i_root)?;
                }

                MemoryCommands::Restore { commit, agent, yes } => {
                    let snap_meta = {
                        let snaps = memory::list_snapshots(&repo.h5i_root)?;
                        snaps
                            .into_iter()
                            .find(|s| s.commit_oid.starts_with(&commit))
                            .ok_or_else(|| {
                                anyhow::anyhow!("No snapshot found for commit {}", commit)
                            })?
                    };
                    let memory_agent = resolve_memory_agent(agent);

                    println!(
                        "{} Restore memory snapshot from commit {} ({} file{})?",
                        WARN,
                        style(&snap_meta.commit_oid[..8]).magenta().bold(),
                        snap_meta.file_count,
                        if snap_meta.file_count == 1 { "" } else { "s" }
                    );
                    println!(
                        "  {} This will overwrite your current {} memory files.",
                        style("!").yellow(),
                        style(memory_agent.label()).cyan()
                    );

                    if !yes {
                        print!("\nContinue? [y/N] ");
                        use std::io::Write as _;
                        std::io::stdout().flush()?;
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("{} Aborted.", style("!").dim());
                            return Ok(());
                        }
                    }

                    let count = memory::restore_snapshot(
                        &repo.h5i_root,
                        &workdir,
                        &snap_meta.commit_oid,
                        memory_agent,
                    )?;
                    println!(
                        "{} Restored {} file{} to {}",
                        SUCCESS,
                        style(count).cyan(),
                        if count == 1 { "" } else { "s" },
                        style(
                            memory::default_memory_dir(&workdir, memory_agent)
                                .display()
                                .to_string()
                        )
                        .dim()
                    );
                }

                MemoryCommands::Push { remote } => {
                    println!(
                        "{} {} to {}",
                        STEP,
                        style("Pushing memory snapshot").cyan().bold(),
                        style(&remote).yellow()
                    );

                    let commit_oid = memory::push(repo.git(), &repo.h5i_root, &remote)?;
                    println!(
                        "{} Memory commit {} pushed to {} ({})",
                        SUCCESS,
                        style(&commit_oid[..8]).magenta().bold(),
                        style(&remote).yellow(),
                        style(memory::MEMORY_REF).dim()
                    );
                    println!(
                        "  {} Teammates can run {} to receive it.",
                        style("→").dim(),
                        style("h5i memory pull").bold()
                    );
                }

                MemoryCommands::Pull { remote } => {
                    println!(
                        "{} {} from {}",
                        STEP,
                        style("Pulling memory snapshot").cyan().bold(),
                        style(&remote).yellow()
                    );

                    let result = memory::pull(repo.git(), &repo.h5i_root, &remote)?;
                    println!(
                        "{} Received {} file{} linked to code commit {}",
                        SUCCESS,
                        style(result.file_count).cyan(),
                        if result.file_count == 1 { "" } else { "s" },
                        style(&result.linked_code_oid[..8.min(result.linked_code_oid.len())])
                            .magenta()
                            .bold()
                    );
                    println!(
                        "  {} Run {} to apply it to your Claude session.",
                        style("→").dim(),
                        style(format!(
                            "h5i memory restore {}",
                            &result.linked_code_oid[..8.min(result.linked_code_oid.len())]
                        ))
                        .bold()
                    );
                }
            }
        }
    Ok(())
}
