//! `h5i notes` — CLI handlers (migrated from main.rs).
use crate::*;

#[derive(Subcommand)]
pub enum NotesCommands {
    /// Parse a Claude Code session log and store enriched metadata linked to a commit
    /// (footprint, causal chain, uncertainty, file churn)
    Analyze {
        /// Path to the Claude Code .jsonl session file (default: auto-detect latest session)
        #[arg(long, value_name = "JSONL")]
        session: Option<PathBuf>,
        /// Commit OID to link this analysis to (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Only include session events that occurred *after* this commit was made.
        /// Useful when a single Claude Code session spans multiple h5i commits:
        ///   h5i notes analyze --since <first-commit-sha>
        /// links only the work done *after* that commit to HEAD.
        #[arg(long, value_name = "OID")]
        since: Option<String>,
    },

    /// Show which files the AI consulted vs edited for a given commit
    Show {
        /// Commit OID whose session analysis to display (default: HEAD)
        commit: Option<String>,
    },

    /// Show moments where the AI expressed uncertainty, optionally filtered by file
    Uncertainty {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Filter to annotations recorded while editing this file
        #[arg(long)]
        file: Option<String>,
    },

    /// Show file edit-churn across all analyzed sessions
    Churn {
        /// Number of files to show
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Visualise the chain of intents associated with recent commits
    Graph {
        /// Number of recent commits to include
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Intent source: 'prompt' uses the stored AI prompt; 'analyze' calls Claude
        #[arg(long, default_value = "prompt")]
        mode: String,
    },

    /// Identify commits most likely to benefit from human review
    Review {
        /// Number of recent commits to scan
        #[arg(short, long, default_value_t = 100)]
        limit: usize,
        /// Minimum score threshold (0.0–1.0) for a commit to be flagged
        #[arg(long, default_value_t = REVIEW_THRESHOLD)]
        min_score: f32,
        /// Output raw JSON instead of the styled table
        #[arg(long)]
        json: bool,
    },

    /// Show where Claude deferred, left placeholders, or made promises it didn't keep
    Omissions {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Filter to annotations recorded while editing this file
        #[arg(long)]
        file: Option<String>,
    },

    /// Show per-file attention coverage: which files were read before being edited.
    /// Files with a low read-before-edit ratio are likely blind edits — higher risk.
    Coverage {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Only show files with read_before_edit_ratio below this threshold (0.0–1.0)
        #[arg(long, default_value_t = 1.01)]
        max_ratio: f32,
    },
}

pub fn run(action: NotesCommands) -> anyhow::Result<()> {
    match action {
            NotesCommands::Analyze {
                session,
                commit,
                since,
            } => {
                let repo = H5iRepository::open(".")?;
                let workdir = repo
                    .git()
                    .workdir()
                    .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                    .to_path_buf();
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                let jsonl_path = match session {
                    Some(p) => p,
                    None => match session_log::find_latest_session(&workdir) {
                        Some(p) => {
                            println!(
                                "{} {}",
                                STEP,
                                style(format!("Auto-detected session: {}", p.display())).dim()
                            );
                            p
                        }
                        None => {
                            println!(
                                "{} No Claude Code session found in ~/.claude/projects/.",
                                WARN
                            );
                            println!(
                                "  {} Use {} to specify a session file.",
                                style("ℹ").blue(),
                                style("h5i notes analyze --session <path>").bold()
                            );
                            return Ok(());
                        }
                    },
                };

                // Resolve --since to a UTC timestamp so analyze_session can filter events.
                let since_time: Option<chrono::DateTime<chrono::Utc>> = match since {
                    None => None,
                    Some(ref sha) => {
                        let oid = git2::Oid::from_str(sha)
                            .or_else(|_| -> Result<git2::Oid, git2::Error> {
                                repo.git()
                                    .revparse_single(sha)?
                                    .peel_to_commit()
                                    .map(|c| c.id())
                            })
                            .map_err(|e| {
                                anyhow::anyhow!("--since: cannot resolve '{}': {}", sha, e)
                            })?;
                        let c = repo
                            .git()
                            .find_commit(oid)
                            .map_err(|e| anyhow::anyhow!("--since: {}", e))?;
                        let secs = c.time().seconds();
                        chrono::DateTime::from_timestamp(secs, 0).inspect(|dt| {
                            println!(
                                "{} Filtering session to events after {} ({})",
                                STEP,
                                style(&sha[..8.min(sha.len())]).magenta(),
                                style(dt.format("%Y-%m-%d %H:%M UTC")).dim()
                            );
                        })
                    }
                };

                println!(
                    "{} {} → commit {}",
                    STEP,
                    style("Analyzing session log").cyan().bold(),
                    style(&oid_str[..8.min(oid_str.len())]).magenta()
                );
                let analysis = session_log::analyze_session(&jsonl_path, since_time)?;
                session_log::save_analysis(&repo.h5i_root, &oid_str, &analysis)?;
                println!(
                    "{} {} messages · {} tool calls · {} edited · {} consulted",
                    SUCCESS,
                    style(analysis.message_count).cyan(),
                    style(analysis.tool_call_count).cyan(),
                    style(analysis.footprint.edited.len()).green(),
                    style(analysis.footprint.consulted.len()).yellow()
                );
                println!(
                    "  {} Run {} to inspect results.",
                    style("ℹ").blue(),
                    style(format!("h5i notes show {}", &oid_str[..8])).bold()
                );
            }

            NotesCommands::Show { commit } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_footprint(&analysis);
                        session_log::print_causal_chain(&analysis);
                    }
                }
            }

            NotesCommands::Uncertainty { commit, file } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for commit {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_uncertainty(&analysis, file.as_deref());
                    }
                }
            }

            NotesCommands::Churn { limit } => {
                let repo = H5iRepository::open(".")?;
                let mut churn = session_log::aggregate_churn(&repo.h5i_root);
                churn.truncate(limit);
                if churn.is_empty() {
                    println!(
                        "{} No churn data yet. Run {} after sessions to build history.",
                        WARN,
                        style("h5i notes analyze").bold()
                    );
                } else {
                    session_log::print_churn(&churn);
                }
            }

            NotesCommands::Graph { limit, mode } => {
                let repo = H5iRepository::open(".")?;
                let analyze = mode.to_lowercase() == "analyze";
                if analyze {
                    if std::env::var("ANTHROPIC_API_KEY").is_err() {
                        println!(
                            "{} {} — set {} to enable Claude analysis.",
                            WARN,
                            style("ANTHROPIC_API_KEY not set, falling back to stored prompts")
                                .yellow(),
                            style("ANTHROPIC_API_KEY").bold(),
                        );
                    } else {
                        println!(
                            "{} {} for {} commits…",
                            STEP,
                            style("Calling Claude to generate intent labels")
                                .cyan()
                                .bold(),
                            style(limit).cyan(),
                        );
                    }
                }
                repo.print_intent_graph(limit, analyze)?;
            }

            NotesCommands::Review {
                limit,
                min_score,
                json,
            } => {
                let repo = H5iRepository::open(".")?;
                let points = repo.suggest_review_points(limit, min_score)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&points)?);
                } else if points.is_empty() {
                    println!(
                        "{} No commits exceeded the review threshold (min_score={:.2}) in the last {} commits.",
                        SUCCESS, min_score, limit
                    );
                } else {
                    println!(
                        "{} — {} commit{} flagged (scanned {}, min_score={:.2})",
                        style("Suggested Review Points").bold().underlined(),
                        style(points.len()).yellow().bold(),
                        if points.len() == 1 { "" } else { "s" },
                        limit,
                        min_score
                    );
                    println!("{}", style("─".repeat(62)).dim());
                    for (i, rp) in points.iter().enumerate() {
                        let filled = (rp.score * 10.0).round() as usize;
                        let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);
                        let score_color = if rp.score >= 0.7 {
                            style(format!("{:.2}", rp.score)).red().bold()
                        } else if rp.score >= 0.45 {
                            style(format!("{:.2}", rp.score)).yellow().bold()
                        } else {
                            style(format!("{:.2}", rp.score)).cyan().bold()
                        };
                        println!(
                            "\n  {} {}  score {}  {}",
                            style(format!("#{}", i + 1)).dim(),
                            style(&rp.short_oid).magenta().bold(),
                            score_color,
                            style(&bar).dim()
                        );
                        println!(
                            "     {} · {}",
                            style(&rp.author).blue(),
                            style(rp.timestamp.format("%Y-%m-%d %H:%M UTC")).dim()
                        );
                        println!("     {}", style(&rp.message).bold());
                        for trigger in &rp.triggers {
                            let bullet = match trigger.rule_id.as_str() {
                                "TEST_REGRESSION" | "INTEGRITY_VIOLATION" => style("⬦").red(),
                                "LARGE_DIFF" | "WIDE_IMPACT" => style("⬦").yellow(),
                                _ => style("⬦").cyan(),
                            };
                            println!(
                                "       {} {:<18}  {}",
                                bullet,
                                style(&trigger.rule_id).bold(),
                                style(&trigger.detail).dim()
                            );
                        }
                    }
                    println!("\n{}", style("─".repeat(62)).dim());
                }
            }

            NotesCommands::Omissions { commit, file } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_omissions(&analysis, file.as_deref());
                    }
                }
            }

            NotesCommands::Coverage { commit, max_ratio } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        let short = &oid_str[..8.min(oid_str.len())];
                        println!(
                            "\n{} {}\n",
                            style("──").dim(),
                            style(format!("Attention Coverage — {}", short))
                                .cyan()
                                .bold()
                        );
                        let cov: Vec<_> = analysis
                            .coverage
                            .iter()
                            .filter(|c| c.read_before_edit_ratio <= max_ratio)
                            .collect();
                        if cov.is_empty() {
                            println!(
                                "  {} All edited files were read before modification.",
                                style("✔").green()
                            );
                        } else {
                            println!(
                                "  {:<42}  {:>8}  {:>12}  {}",
                                style("File").bold(),
                                style("Edits").bold(),
                                style("Coverage").bold(),
                                style("Blind edits").bold(),
                            );
                            println!("  {}", style("─".repeat(74)).dim());
                            for fc in &cov {
                                let pct = (fc.read_before_edit_ratio * 100.0) as u32;
                                let blind = fc.blind_edit_count;
                                let ratio_style = if blind == 0 {
                                    style(format!("{:>10}%", pct)).green()
                                } else if fc.read_before_edit_ratio >= 0.5 {
                                    style(format!("{:>10}%", pct)).yellow()
                                } else {
                                    style(format!("{:>10}%", pct)).red().bold()
                                };
                                let blind_style = if blind == 0 {
                                    style(format!("{:>11}", 0)).dim()
                                } else {
                                    style(format!("{:>11}", blind)).red().bold()
                                };
                                println!(
                                    "  {:<42}  {:>8}  {}  {}",
                                    style(truncate(&fc.file, 42)).cyan(),
                                    fc.edit_turns.len(),
                                    ratio_style,
                                    blind_style,
                                );
                            }
                            println!(
                                "\n  {} file(s) with blind edits (no prior Read).",
                                style(cov.iter().filter(|c| c.blind_edit_count > 0).count()).bold()
                            );
                        }
                        println!();
                    }
                }
            }
        }
    Ok(())
}
