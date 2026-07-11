//! `h5i claude` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum ClaudeCommands {
    /// Run as Claude Code's PostToolUse handler: reads JSON from stdin and emits traces.
    Sync,

    /// Run as Claude Code's Stop handler: mines reasoning and checkpoints context.
    Finish,

    /// Run as the UserPromptSubmit handler: reads the hook JSON from stdin and
    /// records the *verbatim* human prompt into `.git/.h5i/pending_context.json`,
    /// accumulating across turns. `h5i capture commit` then uses this raw human
    /// prompt as the recorded prompt — it wins over an agent-authored `--intent`
    /// — so AI provenance reflects what the human actually asked rather than the
    /// agent's paraphrase. No-ops outside an h5i-initialized repo and fails open
    /// on any error, so it never blocks the turn.
    /// Register in .claude/settings.json under "UserPromptSubmit" hooks.
    Prompt,
}

pub fn run(action: ClaudeCommands) -> anyhow::Result<()> {
    match action {
        ClaudeCommands::Sync => {
            use std::io::Read as _;
            // Read JSON from stdin (Claude Code sends PostToolUse payload here).
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).unwrap_or(0);
            if raw.trim().is_empty() {
                return Ok(());
            }
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return Ok(());
            };
            let tool = data.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
            let inp = data.get("tool_input").cloned().unwrap_or_default();
            let file_path = inp.get("file_path").and_then(|v| v.as_str()).unwrap_or("");

            if file_path.is_empty() || !matches!(tool, "Edit" | "Write" | "Read") {
                return Ok(());
            }

            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            // Only emit traces when inside a git repo that has h5i context initialized.
            let has_ctx = match git2::Repository::discover(&workdir) {
                Ok(r) => r.find_reference("refs/h5i/context/main").is_ok(),
                Err(_) => false,
            };
            if !has_ctx {
                return Ok(());
            }

            // Relativize the path against the workdir for readability.
            let display_path = std::path::Path::new(file_path)
                .strip_prefix(&workdir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| {
                    std::path::Path::new(file_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| file_path.to_owned())
                });

            let (kind, msg) = match tool {
                "Edit" => ("ACT", format!("edited {display_path}")),
                "Write" => ("ACT", format!("wrote {display_path}")),
                "Read" => ("OBSERVE", format!("read {display_path}")),
                _ => return Ok(()),
            };

            // Emit the trace; ignore errors so we never block Claude Code.
            let _ = ctx::append_log(&workdir, kind, &msg, false);

            // Feature 1: on Read, inject prior reasoning about this file into
            // Claude's context window (Claude Code surfaces hook stdout to the model).
            if tool == "Read" {
                if let Ok(rel) = ctx::relevant(&workdir, file_path) {
                    let has = !rel.commit_mentions.is_empty() || !rel.trace_mentions.is_empty();
                    if has {
                        println!("[h5i] Prior reasoning about {}:", display_path);
                        for m in &rel.commit_mentions {
                            println!("  [milestone] {m}");
                        }
                        for t in rel.trace_mentions.iter().take(5) {
                            println!("  {t}");
                        }
                        if !rel.cross_branch_mentions.is_empty() {
                            for c in rel.cross_branch_mentions.iter().take(2) {
                                println!("  [branch] {c}");
                            }
                        }
                    }
                }
            }
        },
        ClaudeCommands::Prompt => {
            use std::io::Read as _;
            // Read the UserPromptSubmit payload from stdin. Fail open on any
            // problem so we never block the human's turn (and emit no stdout,
            // which Claude Code would otherwise inject as added context).
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).unwrap_or(0);
            if raw.trim().is_empty() {
                return Ok(());
            }
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return Ok(());
            };
            let raw_prompt = data.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            // Claude Code delivers background-task completions and other
            // automated events through this same hook as synthetic "user" turns.
            // Only record genuine human input — otherwise a `<task-notification>`
            // block (etc.) gets recorded as the prompt behind a commit.
            let Some(prompt) = sanitize_human_prompt(raw_prompt) else {
                return Ok(());
            };
            let prompt = prompt.as_str();
            let session_id = data.get("session_id").and_then(|v| v.as_str());

            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            // Inside an env box the `.git/.h5i` sidecar is sealed (no read/write
            // grant), so H5iRepository::open + `record_human_prompt` can't reach
            // it — and the `h5i_root.exists()` probe below stats as false. Route
            // the prompt to the box-writable capture spool instead; the in-box
            // `h5i capture commit` reads it back from there to stamp the note.
            if let Some(spool_path) = h5i_core::env::inbox_pending_context_path() {
                let _ =
                    h5i_core::repository::record_human_prompt_at(&spool_path, prompt, session_id);
                return Ok(());
            }
            // On the host: only record inside a git repo that already has h5i
            // initialized — never create `.git/.h5i` just because the global
            // hook fired in some unrelated repo.
            let Ok(git_repo) = git2::Repository::discover(&workdir) else {
                return Ok(());
            };
            let Ok(h5i_root) = storage::h5i_root_for_repo(&git_repo) else {
                return Ok(());
            };
            if !h5i_root.exists() {
                return Ok(());
            }
            if let Ok(repo) = H5iRepository::open(&workdir) {
                let _ = repo.record_human_prompt(prompt, session_id);
            }
        },
        ClaudeCommands::Finish => {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            // 1. Mine the Claude session JSONL for key decisions + omissions and
            //    emit them as THINK/NOTE trace entries. The agent never has to
            //    call `h5i context trace --kind THINK …` itself.
            match auto_derive_traces_from_claude_session(&workdir) {
                Ok(0) => {}
                Ok(n) => eprintln!(
                    "{} Auto-traced {} reasoning entries from Claude session.",
                    style("✔").green(),
                    n
                ),
                Err(e) => eprintln!("{} Auto-trace failed: {e}", style("warn:").yellow()),
            }
            // 2. Checkpoint the context workspace milestone.
            if let Err(e) = auto_checkpoint_context(&workdir, None, true) {
                eprintln!("{} Context checkpoint failed: {e}", style("warn:").yellow());
            }
        },
    }
    Ok(())
}
