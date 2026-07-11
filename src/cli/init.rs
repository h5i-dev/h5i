//! `h5i init` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run() -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            println!(
                "{} {} at {}",
                SUCCESS,
                style("h5i sidecar initialized").green().bold(),
                style(repo.h5i_path().display()).dim()
            );

            let workdir = std::env::current_dir()?;
            match write_claude_instructions(&workdir) {
                Ok(()) => println!(
                    "{} {} (imported via {})",
                    SUCCESS,
                    style("Claude instructions written to .claude/h5i.md").green(),
                    style("CLAUDE.md").yellow()
                ),
                Err(e) => println!(
                    "{} Could not write Claude instructions: {}",
                    style("warn:").yellow(),
                    e
                ),
            }
            match write_codex_instructions(&workdir) {
                Ok(()) => println!(
                    "{} {}",
                    SUCCESS,
                    style("Codex instructions written to AGENTS.md").green()
                ),
                Err(e) => println!(
                    "{} Could not write Codex instructions: {}",
                    style("warn:").yellow(),
                    e
                ),
            }
            match write_persona_scaffold(&workdir) {
                Ok(()) => println!(
                    "{} {} ({} auto-loads it; set per-env content via {})",
                    SUCCESS,
                    style("Persona scaffold written to PERSONA.md").green(),
                    style("CLAUDE.md").yellow(),
                    style("persona = [...] in .h5i/env.toml").cyan()
                ),
                Err(e) => println!(
                    "{} Could not write persona scaffold: {}",
                    style("warn:").yellow(),
                    e
                ),
            }

            println!();
            println!("  {}", style("Quick-start:").bold());
            println!(
                "    {}  capture AI provenance on every commit",
                style("h5i commit -m \"…\" --agent <claude-code|codex>  (--intent fallback for CI/scripts)").cyan()
            );
            println!(
                "    {}  snapshot agent memory after a session",
                style("h5i memory snapshot [--agent <claude-code|codex>]").cyan()
            );
            println!(
                "    {}  wire the Claude Code hooks (add {} for token-reduced Bash output)",
                style("h5i hook setup --write").cyan(),
                style("--wrap-bash").bold()
            );
            println!(
                "    {}  push all h5i data to your remote",
                style("h5i push").cyan()
            );
            println!();
            println!(
                "  {} h5i stores metadata in {} and {}.",
                style("Note:").dim(),
                style("refs/h5i/notes").yellow(),
                style("refs/h5i/memory").yellow()
            );
            println!(
                "  {} These refs are NOT included in a plain {}.",
                style("     ").dim(),
                style("git push").yellow()
            );
            println!(
                "  {} Run {} (or see README §9) to share them with your team.",
                style("     ").dim(),
                style("h5i push").bold()
            );
        }
    Ok(())
}
