//! `h5i hook` — hook management (setup/wrap-bash/session-start). Migrated from main.rs.
use crate::*;

#[derive(Subcommand)]
pub enum HookCommands {
    /// Print install instructions for agent hooks, or write the hook wiring
    /// into Claude/Codex config directly with --write.
    Setup {
        /// Write the SessionStart/PostToolUse/Stop wiring into
        /// the agent config (idempotent merge) instead of printing
        /// instructions.
        #[arg(long)]
        write: bool,

        /// Which agent config to write. Omit to write both Claude and Codex.
        #[arg(long, value_enum, requires = "write")]
        target: Option<HookTarget>,

        /// Where --write puts the settings: the repo's agent config or your
        /// user-level agent config.
        #[arg(long, value_enum, default_value_t = SetupScope::Project, requires = "write")]
        scope: SetupScope,

        /// Also register the OPTIONAL Bash capture-wrap hook
        /// (`h5i hook wrap-bash`): routes every Bash command through
        /// `h5i capture run`, so large/failing output reaches the agent as a
        /// token-reduced summary (full raw stored for `h5i recall`). Off by
        /// default. Note: permission allowlists then match the rewritten
        /// `h5i capture run …` command, not the original.
        #[arg(long, requires = "write")]
        wrap_bash: bool,

        /// Also register the team peer-review Stop hook
        /// (`h5i team agent hook`): when this agent is running in an active
        /// `h5i team` round, it keeps the agent from stopping while it still
        /// owes work and surfaces incoming review requests between turns. For
        /// Claude it blocks the stop; for Codex it prints the pending review.
        /// Off by default; safe to leave on outside a team (it no-ops).
        #[arg(long, requires = "write")]
        team: bool,
    },

    /// Run as the shared SessionStart handler: injects prior context into the agent context window.
    /// Register under "SessionStart" hooks as `h5i hook session-start`.
    SessionStart,

    /// OPTIONAL PreToolUse handler for the Bash tool: rewrites the command into
    /// a `h5i capture run` wrapper (via updatedInput, Claude Code ≥ 2.0.10), so
    /// the agent receives a token-reduced summary for large/failing output while
    /// the full raw bytes are stored for `h5i recall`. Skips h5i's own commands,
    /// top-level `cd` (session cwd tracking), and anything outside a git repo;
    /// every failure path emits nothing, so the original command runs untouched.
    /// Register in .claude/settings.json under "PreToolUse" with matcher "Bash".
    WrapBash,

    /// Claude Code integration hook handlers (PostToolUse / Stop / UserPromptSubmit).
    Claude {
        #[command(subcommand)]
        action: cli::claude::ClaudeCommands,
    },

    /// Codex integration hook handlers for context restore, trace sync, and closeout.
    Codex {
        #[command(subcommand)]
        action: cli::codex::CodexCommands,
    },
}

pub fn run(action: HookCommands) -> anyhow::Result<()> {
    match action {
        HookCommands::Setup {
            write,
            target,
            scope,
            wrap_bash,
            team,
        } => {
            if write {
                let targets = target
                    .map(|t| vec![t])
                    .unwrap_or_else(|| vec![HookTarget::Claude, HookTarget::Codex]);
                let mut written = Vec::new();
                for target in targets {
                    let config_dir = match (target, scope) {
                        (_, SetupScope::User) => {
                            let home = std::env::var("HOME").map_err(|_| {
                                anyhow::anyhow!("$HOME is not set — use --scope project")
                            })?;
                            let agent_dir = match target {
                                HookTarget::Claude => ".claude",
                                HookTarget::Codex => ".codex",
                            };
                            PathBuf::from(home).join(agent_dir)
                        }
                        (HookTarget::Claude, SetupScope::Project) => {
                            let repo = git2::Repository::discover(".").map_err(|_| {
                                anyhow::anyhow!("not inside a git repository — use --scope user")
                            })?;
                            let workdir = repo.workdir().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "bare repository has no working dir — use --scope user"
                                )
                            })?;
                            workdir.join(".claude")
                        }
                        (HookTarget::Codex, SetupScope::Project) => {
                            let repo = git2::Repository::discover(".").map_err(|_| {
                                anyhow::anyhow!("not inside a git repository — use --scope user")
                            })?;
                            let workdir = repo.workdir().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "bare repository has no working dir — use --scope user"
                                )
                            })?;
                            workdir.join(".codex")
                        }
                    };
                    let path = match target {
                        HookTarget::Claude => config_dir.join("settings.json"),
                        HookTarget::Codex => config_dir.join("config.toml"),
                    };

                    let existing = std::fs::read_to_string(&path).unwrap_or_default();
                    let merged = match target {
                        HookTarget::Claude => {
                            let core =
                                h5i_core::hooks::merge_hook_settings_json(&existing, wrap_bash)?;
                            if team {
                                h5i_core::hooks::merge_team_hook_settings_json(&core)?
                            } else {
                                core
                            }
                        }
                        HookTarget::Codex => {
                            let core =
                                h5i_core::hooks::merge_codex_config_toml(&existing, wrap_bash)?;
                            if team {
                                h5i_core::hooks::merge_team_hook_codex_toml(&core)?
                            } else {
                                core
                            }
                        }
                    };
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, merged)?;

                    let agent_name = match target {
                        HookTarget::Claude => "Claude Code",
                        HookTarget::Codex => "Codex",
                    };
                    written.push((target, agent_name, path));
                }

                println!("{} Agent hooks configured:", SUCCESS);
                for (_, agent_name, path) in &written {
                    println!(
                        "   {} {}",
                        style(*agent_name).bold(),
                        style(path.display()).cyan()
                    );
                }
                println!(
                    "   {} {}   ·   {} {} ({})",
                    style("SessionStart:").dim(),
                    style("h5i hook session-start").bold(),
                    style("Claude PostToolUse:").dim(),
                    style("h5i hook claude sync").bold(),
                    style("Edit|Write|Read").dim(),
                );
                println!(
                    "   {} {}   ·   {} {}",
                    style("Claude Stop:").dim(),
                    style("h5i hook claude finish").bold(),
                    style("Codex Stop:").dim(),
                    style("h5i hook codex finish").bold(),
                );
                if wrap_bash {
                    println!(
                        "   {} {} ({}) — Bash commands run through {}: token-reduced\n\
                         \x20  summaries for large/failing output, full raw stored for {}.",
                        style("Bash capture-wrap:").dim(),
                        style("h5i hook wrap-bash").bold(),
                        style("PreToolUse · Bash").dim(),
                        style("h5i capture run").yellow(),
                        style("h5i recall").yellow(),
                    );
                    println!(
                        "   {} permission allowlists now match the rewritten {} command.",
                        style("note:").dim(),
                        style("h5i capture run …").bold(),
                    );
                } else {
                    println!(
                        "   {} off — pass {} to route Bash commands through {}\n\
                         \x20  (token-reduced summaries; full raw stored for {}).",
                        style("Bash capture-wrap:").dim(),
                        style("--wrap-bash").bold(),
                        style("h5i capture run").yellow(),
                        style("h5i recall").yellow(),
                    );
                }
                if team {
                    println!(
                        "   {} {} ({}) — keeps an agent in an active {} round from stopping\n\
                         \x20  while it owes work; surfaces review requests between turns.",
                        style("Team peer-review:").dim(),
                        style("h5i team agent hook").bold(),
                        style("Stop").dim(),
                        style("h5i team").yellow(),
                    );
                }
                println!();
                println!(
                    "   {} open {} once (or restart) so configured agents review and reload hooks.",
                    style("→").dim(),
                    style("/hooks").bold()
                );
                if written
                    .iter()
                    .any(|(target, _, _)| *target == HookTarget::Claude)
                {
                    println!(
                        "   {} prompt capture (UserPromptSubmit → {}) is now wired; the MCP\n\
                         \x20    server stays manual — run {} for those instructions.",
                        style("→").dim(),
                        style("h5i hook claude prompt").bold(),
                        style("h5i hook setup").bold(),
                    );
                }
                if written
                    .iter()
                    .any(|(target, _, _)| *target == HookTarget::Codex)
                {
                    println!(
                        "   {} Codex loads repo hooks only when the project {} layer is trusted.",
                        style("→").dim(),
                        style(".codex/").bold(),
                    );
                }
                println!(
                    "   {} for messaging identity + turn delivery, run {}.",
                    style("→").dim(),
                    style("h5i msg setup <name>").bold(),
                );
                return Ok(());
            }

            println!(
                "{} {} writes the SessionStart/PostToolUse/Stop/UserPromptSubmit\n\
                 wiring below into .claude/settings.json for you ({} for ~/.claude,\n\
                 {} to capture-wrap Bash). Prompt capture is native now — no jq\n\
                 script needed. The step below only matters if you wire it by hand.\n",
                style("Tip:").bold(),
                style("h5i hook setup --write").cyan().bold(),
                style("--scope user").bold(),
                style("--wrap-bash").bold(),
            );

            println!(
                "{}",
                style("── Add to ~/.claude/settings.json ──").bold()
            );
            println!(
                "Add (or merge) the {} block into your {}:\n",
                style("hooks").yellow(),
                style("~/.claude/settings.json").yellow()
            );
            println!(
                "{}",
                style(
                    r#"{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook claude prompt"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook session-start"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook claude sync"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook claude finish"
          }
        ]
      }
    ]
  }
}"#
                )
                .dim()
            );
            println!();
            println!(
                "  {} — injects prior context into every new session automatically",
                style("SessionStart").yellow()
            );
            println!(
                "  {} — auto-traces OBSERVE for every Read, ACT for every Edit/Write",
                style("PostToolUse").yellow()
            );
            println!(
                "  {} — mines THINK / NOTE entries from your session transcript and",
                style("Stop").yellow()
            );
            println!("         auto-checkpoints the context workspace milestone.",);
            println!("         You never have to call `h5i context trace` by hand.");
            println!(
                "  {} — captures the verbatim human prompt so {} records",
                style("UserPromptSubmit").yellow(),
                style("h5i capture commit").yellow()
            );
            println!("         what you actually typed, not the agent's paraphrase.");

            println!();
            println!(
                "{} Bash capture-wrap — rewrite every Bash command into {}",
                style("Optional:").bold(),
                style("h5i capture run").yellow()
            );
            println!("  (PreToolUse updatedInput, Claude Code ≥ 2.0.10): the agent receives a");
            println!("  token-reduced summary for large/failing output; the full raw bytes stay");
            println!(
                "  stored and searchable via {}. h5i's own commands and top-level",
                style("h5i recall").yellow()
            );
            println!("  `cd` are never wrapped, and any hook failure runs the command untouched.");
            println!(
                "  Not written by default — opt in with {},",
                style("h5i hook setup --write --wrap-bash").cyan()
            );
            println!("  or add a PreToolUse entry by hand:");
            println!(
                "{}",
                style(
                    r#"    { "matcher": "Bash", "hooks": [ { "type": "command", "command": "h5i hook wrap-bash" } ] }"#
                )
                .dim()
            );

            println!();
            println!(
                "{} For cross-agent messaging ({}), run the one-liner — it sets your",
                style("Messaging:").bold(),
                style("h5i msg").yellow(),
            );
            println!(
                "  identity ({}) and adds the turn-delivery Stop hook for you:",
                style("env H5I_AGENT").bold(),
            );
            println!("        {}", style("h5i msg setup claude").cyan().bold());
            println!(
                "  Identity is {} (no {} on commands). Default writes {} and an\n\
                 autonomous {} hook; pass {} for all projects, or {} for a notify-only hook.\n\
                 For {}, just launch it with {} — it doesn't read .claude/settings.json.",
                style("per-agent").bold(),
                style("--as").dim(),
                style("./.claude/settings.json").bold(),
                style("--block").bold(),
                style("--scope user").bold(),
                style("--no-block").bold(),
                style("Codex").yellow(),
                style("H5I_AGENT=codex").bold(),
            );
            println!();
            println!(
                "  {} Turn delivery is primary — the Stop hook surfaces messages between turns,\n\
                 and {} notes any unread on resume. {} is a human side-terminal\n\
                 dashboard; real-time push via the Monitor tool is experimental / host-dependent.",
                style("Delivery:").bold(),
                style("h5i hook session-start").yellow(),
                style("h5i msg watch").bold(),
            );
            println!(
                "  {} For autonomous turn delivery (force the agent to handle a message),\n\
                 use {} instead of the plain hook — it emits {} (honors stop_hook_active).",
                style("Turn mode:").bold(),
                style("h5i msg hook --as <name> --block").bold(),
                style("decision:block").bold(),
            );

            println!("{}", style("── Step 3: Register the MCP server ──").bold());
            println!(
                "Add the {} block to {} so Claude Code can call h5i tools natively:\n",
                style("mcpServers").yellow(),
                style("~/.claude/settings.json").yellow()
            );
            println!(
                "{}",
                style(
                    r#"{
  "mcpServers": {
    "h5i": {
      "command": "h5i",
      "args": ["mcp"]
    }
  }
}"#
                )
                .dim()
            );
            println!(
                "\nOnce registered, Claude Code gains native access to h5i tools\n\
                 (h5i_log, h5i_blame, h5i_context_trace, h5i_notes_show, etc.)\n\
                 without needing shell commands.\n"
            );

            println!(
                "\n{} Set {} and",
                style("Tip:").bold(),
                style("H5I_MODEL").yellow(),
            );
            println!(
                "    {} in your shell profile to override the defaults captured by the hook.",
                style("H5I_AGENT_ID").yellow()
            );
            println!(
                "\n{} also work without hooks — {} / H5I_MODEL / H5I_AGENT_ID are read automatically at commit time.",
                style("Env vars").bold(),
                style("H5I_INTENT").yellow()
            );
        },
        HookCommands::WrapBash => {
            use std::io::Read as _;
            // PreToolUse handler (matcher "Bash"): rewrite the command into a
            // token-reducing `h5i capture run` wrapper via updatedInput. The
            // agent then receives capture run's summary for large/failing
            // output instead of the raw bytes (which stay stored for
            // `h5i recall`). Every failure path emits nothing and exits 0, so
            // the original command runs untouched — a wrapper hook must never
            // break the session.
            let mut raw_in = String::new();
            std::io::stdin().read_to_string(&mut raw_in).unwrap_or(0);
            if raw_in.trim().is_empty() {
                return Ok(());
            }
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw_in) else {
                return Ok(());
            };
            if data.get("tool_name").and_then(|v| v.as_str()) != Some("Bash") {
                return Ok(());
            }
            let Some(command) = data.pointer("/tool_input/command").and_then(|v| v.as_str()) else {
                return Ok(());
            };
            // `capture run` stores into .git/.h5i — only wrap when the session
            // cwd is inside a git repository.
            let cwd = data
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok());
            let Some(cwd) = cwd else {
                return Ok(());
            };
            let Ok(repo) = git2::Repository::discover(&cwd) else {
                return Ok(());
            };
            let in_env_capture = std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR).is_some()
                && std::env::var_os(h5i_core::env::H5I_ENV_ID_VAR).is_some()
                && std::env::var_os(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).is_some();
            if !in_env_capture && !h5i_capture_store_writable(&repo) {
                return Ok(());
            }
            let Some(wrapped) = h5i_core::hooks::wrap_bash_command(command) else {
                return Ok(());
            };
            // Patch only `command`, preserving the other tool_input fields
            // (description, timeout, run_in_background, …). Codex requires
            // permissionDecision=allow when updatedInput is returned.
            let mut updated = data
                .pointer("/tool_input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let Some(obj) = updated.as_object_mut() else {
                return Ok(());
            };
            obj.insert("command".to_string(), serde_json::Value::String(wrapped));
            let mut hook_output = serde_json::json!({
                "hookEventName": "PreToolUse",
                "updatedInput": updated,
            });
            if data
                .get("hook_event_name")
                .and_then(|v| v.as_str())
                .is_some()
            {
                hook_output["permissionDecision"] = serde_json::Value::String("allow".to_string());
            }
            println!(
                "{}",
                serde_json::json!({
                    "hookSpecificOutput": hook_output
                })
            );
        },
        HookCommands::SessionStart => {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if let Some(additional_context) = session_start_context(&workdir) {
                println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": additional_context
                        }
                    })
                );
            }
        },
        HookCommands::Claude { .. } | HookCommands::Codex { .. } => {
            unreachable!("`h5i hook claude|codex` is normalized to the top-level alias before dispatch")
        },
    }
    Ok(())
}
