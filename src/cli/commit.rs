//! `h5i commit` — CLI handler (migrated from main.rs).
use crate::*;

#[allow(clippy::too_many_arguments)]
pub fn run(message: String, intent: Option<String>, model: Option<String>, agent: Option<String>, tests: bool, test_results: Option<std::path::PathBuf>, test_cmd: Option<String>, audit: bool, force: bool, caused_by: Option<Vec<String>>, decisions_file: Option<std::path::PathBuf>, add_paths: Option<Vec<std::path::PathBuf>>) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let sig = repo.git().signature()?; // Fetch system-default Git signature

            // Stage any paths passed via --add before the nothing-staged guard.
            if let Some(ref paths) = add_paths {
                if !paths.is_empty() {
                    let mut idx = repo.git().index()?;
                    for p in paths {
                        idx.add_path(p.as_path())?;
                    }
                    idx.write()?;
                }
            }

            // Refuse to commit if nothing is staged — guide the caller to git add first.
            {
                let idx = repo.git().index()?;
                let head_empty = repo.git().head().is_err(); // true on first commit
                let staged = if head_empty {
                    !idx.is_empty()
                } else {
                    let head_tree = repo.git().head()?.peel_to_tree()?;
                    let diff = repo
                        .git()
                        .diff_tree_to_index(Some(&head_tree), Some(&idx), None)?;
                    diff.deltas().len() > 0
                };
                if !staged {
                    eprintln!(
                        "{} Nothing staged. Stage the files you want to commit first:\n\n  {}\n\nThen re-run {}.",
                        ERROR,
                        style("git add <file> …").cyan(),
                        style("h5i commit").cyan(),
                    );
                    std::process::exit(1);
                }
            }

            // Resolution order: captured raw human prompt (UserPromptSubmit
            // hook) > --intent flag > $H5I_INTENT/$H5I_PROMPT > pending.prompt.
            // The verbatim human prompt wins so provenance records what the
            // human actually asked, not the agent's paraphrase.
            let pending = repo.read_pending_context()?;
            let prompt = pending
                .as_ref()
                .and_then(|c| c.human_prompt.clone())
                .or(intent)
                .or_else(|| {
                    std::env::var("H5I_INTENT")
                        .or_else(|_| std::env::var("H5I_PROMPT"))
                        .ok()
                })
                .or_else(|| pending.as_ref().and_then(|c| c.prompt.clone()));
            let model = model
                .or_else(|| std::env::var("H5I_MODEL").ok())
                .or_else(|| pending.as_ref().and_then(|c| c.model.clone()));
            let agent = agent
                .or_else(|| std::env::var("H5I_AGENT_ID").ok())
                .or_else(|| pending.as_ref().and_then(|c| c.agent_id.clone()));

            if audit {
                let report = repo.verify_integrity(prompt.as_deref(), &message)?;

                // Print a header line based on the overall level.
                match report.level {
                    IntegrityLevel::Violation => println!(
                        "{} {} {}",
                        ERROR,
                        style("INTEGRITY VIOLATION").red().bold(),
                        style(format!("(score: {:.2})", report.score)).dim()
                    ),
                    IntegrityLevel::Warning => println!(
                        "{} {} {}",
                        WARN,
                        style("INTEGRITY WARNING").yellow().bold(),
                        style(format!("(score: {:.2})", report.score)).dim()
                    ),
                    IntegrityLevel::Valid => {
                        println!("{} {}", SUCCESS, style("Integrity check passed.").green());
                    }
                }

                // Print each finding with its rule ID and severity colour.
                for f in &report.findings {
                    let (bullet, label) = match f.severity {
                        Severity::Violation => (
                            style("✖").red().bold(),
                            style(format!("[{}]", f.rule_id)).red().bold(),
                        ),
                        Severity::Warning => (
                            style("⚠").yellow().bold(),
                            style(format!("[{}]", f.rule_id)).yellow().bold(),
                        ),
                        Severity::Info => {
                            (style("ℹ").cyan(), style(format!("[{}]", f.rule_id)).cyan())
                        }
                    };
                    println!("  {} {} {}", bullet, label, f.detail);
                }

                if matches!(report.level, IntegrityLevel::Violation) && !force {
                    println!(
                        "\n{} Commit aborted. Use {} to override.",
                        style("!").red(),
                        style("--force").bold()
                    );
                    return Ok(());
                }
            }

            let ai_meta = if prompt.is_some() || model.is_some() || agent.is_some() {
                Some(AiMetadata {
                    model_name: model.unwrap_or_else(|| "unknown".into()),
                    agent_id: agent.unwrap_or_else(|| "unknown".into()),
                    prompt: prompt.unwrap_or_else(|| "".into()),
                    usage: None,
                })
            } else {
                None
            };

            // ── Policy check ──────────────────────────────────────────────────
            // Run after ai_meta is constructed so path rules can inspect it.
            {
                let workdir = repo
                    .git()
                    .workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                if let Ok(Some(cfg)) = h5i_core::policy::load_policy(&workdir) {
                    // Collect staged file paths from the git index.
                    let staged_files: Vec<String> = {
                        let mut idx = repo.git().index()?;
                        let _ = idx.read(true);
                        idx.iter()
                            .map(|e| String::from_utf8_lossy(&e.path).to_string())
                            .collect()
                    };

                    let input = h5i_core::policy::CommitCheckInput {
                        message: &message,
                        ai_meta: ai_meta.as_ref(),
                        staged_files: &staged_files,
                        audit_passed: audit,
                    };
                    let violations = h5i_core::policy::check_commit(&cfg, &input);
                    if !violations.is_empty() {
                        let has_error = violations
                            .iter()
                            .any(|v| v.severity == h5i_core::policy::ViolationSeverity::Error);
                        let label = cfg.commit.label.as_deref().unwrap_or("policy");
                        println!(
                            "{} {} {}",
                            if has_error { ERROR } else { WARN },
                            style(format!("Policy violation ({})", label)).red().bold(),
                            style(format!("({} rule(s) failed)", violations.len())).dim()
                        );
                        h5i_core::policy::print_violations(&violations);
                        if has_error && !force {
                            println!(
                                "\n{} Commit aborted by policy. Use {} to override.",
                                style("!").red(),
                                style("--force").bold()
                            );
                            return Ok(());
                        }
                    }
                }
            }

            // Resolve TestSource — priority:
            //   1. --test-results <file>
            //   2. H5I_TEST_RESULTS env var (path to a JSON file)
            //   3. --test-cmd <cmd>
            //   4. --tests + H5I_TEST_CMD env var (run configured command)
            //   5. --tests alone (scan staged files for markers)
            //   6. Nothing
            let env_results = std::env::var("H5I_TEST_RESULTS").ok();
            let env_test_cmd = std::env::var("H5I_TEST_CMD").ok();
            let test_source = if let Some(ref path) = test_results {
                let metrics = repo.load_test_results_from_file(path)?;
                TestSource::Provided(metrics)
            } else if let Some(ref env_path) = env_results {
                let metrics = repo.load_test_results_from_file(std::path::Path::new(env_path))?;
                TestSource::Provided(metrics)
            } else if let Some(ref cmd) = test_cmd {
                println!(
                    "{} Running test command: {}",
                    style("▶").cyan(),
                    style(cmd).yellow()
                );
                let metrics = repo.run_test_command(cmd)?;
                let passing = metrics.is_passing();
                let icon = if passing {
                    style("✔").green()
                } else {
                    style("✖").red()
                };
                if let Some(ref s) = metrics.summary {
                    println!("  {} {}", icon, style(s).dim());
                }
                TestSource::Provided(metrics)
            } else if tests {
                if let Some(ref cmd) = env_test_cmd {
                    // --tests + H5I_TEST_CMD: actually run the test suite
                    println!(
                        "{} Running test command (H5I_TEST_CMD): {}",
                        style("▶").cyan(),
                        style(cmd).yellow()
                    );
                    let metrics = repo.run_test_command(cmd)?;
                    let passing = metrics.is_passing();
                    let icon = if passing {
                        style("✔").green()
                    } else {
                        style("✖").red()
                    };
                    if let Some(ref s) = metrics.summary {
                        println!("  {} {}", icon, style(s).dim());
                    } else {
                        let status = if passing { "passed" } else { "failed" };
                        println!(
                            "  {} exit code: {}",
                            icon,
                            metrics
                                .exit_code
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| status.into())
                        );
                    }
                    TestSource::Provided(metrics)
                } else {
                    // Fallback: scan staged files for marker blocks
                    TestSource::ScanMarkers
                }
            } else {
                TestSource::None
            };

            let caused_by = caused_by.unwrap_or_default();

            // Load structured design decisions from JSON file if provided.
            let decisions: Vec<Decision> = if let Some(ref path) = decisions_file {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    anyhow::anyhow!("--decisions: cannot read {}: {}", path.display(), e)
                })?;
                serde_json::from_str(&raw).map_err(|e| {
                    anyhow::anyhow!("--decisions: invalid JSON in {}: {}", path.display(), e)
                })?
            } else {
                vec![]
            };

            // In a sandboxed env the h5i sidecar (notes ref + object store) is
            // sealed, so the git commit lands but the note is STAGED to the env
            // capture spool for the host to apply after the session — instead of
            // failing the commit mid-way. Detected by the env-capture vars the
            // host injects (same gate as in-box `h5i capture run`).
            let note_spool = {
                let spool =
                    std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR).map(PathBuf::from);
                let in_env = spool.is_some()
                    && std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok()
                    && std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_ok();
                if in_env {
                    spool
                } else {
                    None
                }
            };
            let oid = repo.commit(
                &message,
                &sig,
                &sig,
                ai_meta,
                test_source,
                caused_by,
                decisions,
                note_spool.as_deref(),
            )?;
            repo.clear_pending_context()?;
            println!(
                "{} {} {}",
                SUCCESS,
                style("h5i Commit Created:").green(),
                style(oid).magenta().bold()
            );
            if note_spool.is_some() {
                println!(
                    "  {} sandboxed env — h5i note staged for host ingest (applied on session end)",
                    style("▢").cyan().dim()
                );
            }

            // Auto-snapshot the context workspace state linked to this git commit.
            let workdir = repo
                .git()
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            if ctx::is_initialized(&workdir) {
                if let Err(e) = ctx::snapshot_for_commit(&workdir, &oid.to_string()) {
                    eprintln!("{} context snapshot failed: {e}", style("warn:").yellow());
                } else {
                    println!(
                        "  {} context snapshot linked to {}",
                        style("◈").cyan().dim(),
                        style(&oid.to_string()[..8]).dim()
                    );
                }
            }
        }
    Ok(())
}
