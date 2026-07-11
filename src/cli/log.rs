//! `h5i log` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(limit: usize, ancestry: Option<String>) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;

            if let Some(spec) = ancestry {
                // ── Prompt ancestry mode ──────────────────────────────────────
                // Parse "file:line" spec.
                let (file_part, line_part) = spec.rsplit_once(':').ok_or_else(|| {
                    anyhow::anyhow!("--ancestry expects FILE:LINE format, e.g. src/model.py:42")
                })?;
                let line_number: usize = line_part.parse().map_err(|_| {
                    anyhow::anyhow!("--ancestry: '{}' is not a valid line number", line_part)
                })?;
                let path = std::path::Path::new(file_part);

                println!(
                    "\n{} {}\n",
                    style("──").dim(),
                    style(format!("Prompt ancestry for {}:{}", file_part, line_number))
                        .cyan()
                        .bold(),
                );

                let chain = repo.blame_ancestry(path, line_number)?;

                if chain.is_empty() {
                    println!("  (no ancestry found — file may be untracked or line out of range)");
                } else {
                    let total = chain.len();
                    for (i, entry) in chain.iter().enumerate() {
                        let depth = total - i;
                        let short_oid = &entry.commit_id[..8];
                        let ts = entry.timestamp.format("%Y-%m-%d %H:%M UTC");
                        let agent_label = match &entry.agent {
                            Some(a) => format!("AI:{a}"),
                            None => "Human".to_string(),
                        };

                        println!(
                            "  [{}] {}  {} · {}",
                            style(format!("{depth} of {total}")).dim(),
                            style(short_oid).magenta(),
                            style(&entry.author).cyan(),
                            style(ts).dim(),
                        );

                        // The line content at this point in history
                        println!(
                            "       {}  {}",
                            style("line:").dim(),
                            style(&entry.line_content).italic(),
                        );

                        match &entry.prompt {
                            Some(p) => println!(
                                "       {}  {}",
                                style("prompt:").dim(),
                                style(format!("\"{}\"", truncate(p, 80))).yellow().italic(),
                            ),
                            None => println!(
                                "       {}  {} ({})",
                                style("prompt:").dim(),
                                style("(none recorded)").dim(),
                                style(agent_label).dim(),
                            ),
                        }
                        println!();
                    }
                }
            } else {
                let log_limit = if limit == 0 { usize::MAX } else { limit };
                repo.print_log(log_limit)?;
            }
        }
    Ok(())
}
