//! `h5i codex` — CLI handlers (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum CodexCommands {
    /// Print the current shared context so Codex can resume with prior reasoning
    Prelude,

    /// Sync OBSERVE/ACT traces from the active Codex session JSONL
    Sync,

    /// Sync the current Codex session and auto-checkpoint the context workspace
    Finish {
        /// Optional summary for the context checkpoint
        #[arg(long)]
        summary: Option<String>,

        /// Suppress stdout for hook use
        #[arg(long)]
        quiet: bool,
    },
}

pub fn run(action: CodexCommands) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match action {
                CodexCommands::Prelude => {
                    print_shared_context_prelude(&workdir);
                    // Surface any messages addressed to Codex at task start.
                    deliver_codex_inbox(&workdir);
                }
                CodexCommands::Sync => {
                    match codex::sync_context(&workdir)? {
                        Some(result) => println!(
                            "{} Synced Codex session {} ({} OBSERVE, {} ACT, {} new line{})",
                            SUCCESS,
                            style(&result.session_id).magenta(),
                            result.observed,
                            result.acted,
                            result.processed_lines,
                            if result.processed_lines == 1 { "" } else { "s" }
                        ),
                        None => println!(
                            "{} No Codex session found in ~/.codex/sessions for this repo.",
                            WARN
                        ),
                    }
                    // Turn-delivery analog: check the inbox after a work burst.
                    deliver_codex_inbox(&workdir);
                }
                CodexCommands::Finish { summary, quiet } => {
                    match codex::sync_context(&workdir)? {
                        Some(result) if !quiet => {
                            println!(
                                "{} Synced Codex session {} ({} OBSERVE, {} ACT)",
                                SUCCESS,
                                style(&result.session_id).magenta(),
                                result.observed,
                                result.acted,
                            );
                        }
                        None if !quiet => {
                            println!(
                                "{} No Codex session found in ~/.codex/sessions for this repo.",
                                WARN
                            );
                        }
                        _ => {}
                    }
                    auto_checkpoint_context(&workdir, summary.as_deref(), quiet)?;
                    if !quiet {
                        deliver_codex_inbox(&workdir);
                    }
                }
            }
        }
    Ok(())
}
