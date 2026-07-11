//! `h5i blame` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(file: PathBuf, show_prompt: bool) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;

            let results = repo.blame(&file)?;
            println!(
                "{}",
                style(format!(
                    "{:<4} {:<8} {:<15} | {}",
                    "STAT", "COMMIT", "AUTHOR/AGENT", "CONTENT"
                ))
                .bold()
                .underlined()
            );

            // Track the previous commit id so we can print the prompt once per
            // commit boundary rather than once per line.
            let mut prev_commit: Option<String> = None;

            for r in &results {
                let test_indicator = match r.test_passed {
                    Some(true) => "✅",
                    Some(false) => "❌",
                    None => "  ",
                };

                // Print prompt annotation when the commit changes (show_prompt mode).
                if show_prompt {
                    let commit_changed = prev_commit.as_deref() != Some(&r.commit_id);
                    if commit_changed {
                        if let Some(ref prompt) = r.prompt {
                            // Blank separator + indented prompt label
                            println!(
                                "           {:<15}   {}",
                                "",
                                style(format!("prompt: \"{}\"", truncate(prompt, 72)))
                                    .italic()
                                    .yellow()
                            );
                        }
                        prev_commit = Some(r.commit_id.clone());
                    }
                }

                println!(
                    "{} {} {:<15} | {}",
                    test_indicator,
                    style(&r.commit_id[..8]).dim(),
                    style(&r.agent_info).blue(),
                    r.line_content
                );
            }
        }
    Ok(())
}
