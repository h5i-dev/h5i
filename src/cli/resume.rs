//! `h5i resume` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(branch: Option<String>) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                .to_path_buf();
            if let Some(ref b) = branch {
                println!(
                    "{} {} {}",
                    STEP,
                    style("Generating handoff briefing for branch")
                        .cyan()
                        .bold(),
                    style(b).yellow()
                );
            } else {
                println!(
                    "{} {}",
                    STEP,
                    style("Generating handoff briefing...").cyan().bold()
                );
            }
            match h5i_core::resume::generate_briefing(&repo, &workdir, branch.as_deref()) {
                Ok(briefing) => h5i_core::resume::print_briefing(&briefing),
                Err(e) => println!("{} Failed to generate briefing: {}", ERROR, style(e).red()),
            }
        }
    Ok(())
}
