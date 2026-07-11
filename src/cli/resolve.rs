//! `h5i resolve` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(ours: String, theirs: String, file: String) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let our_oid = Oid::from_str(&ours)?;
            let their_oid = Oid::from_str(&theirs)?;

            println!(
                "{} {} for {}...",
                STEP,
                style("3-way text merge").cyan().bold(),
                style(&file).yellow()
            );
            let outcome = repo.merge_file_three_way(our_oid, their_oid, &file)?;

            println!(
                "\n{}\n{}",
                style("--- Merge Result ---").dim(),
                outcome.content
            );
            if outcome.had_conflicts {
                eprintln!(
                    "\n{} Conflict markers were left in the output. Resolve them and `git add {}`.",
                    style("⚠").yellow(),
                    style(&file).bold()
                );
                std::process::exit(1);
            } else {
                println!(
                    "\n{} Tip: Use {} to stage the resolved content.",
                    style("💡").yellow(),
                    style(format!("git add {}", file)).bold()
                );
            }
        }
    Ok(())
}
