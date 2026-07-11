//! `h5i serve` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(port: u16) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let repo_path = repo
                .git()
                .workdir()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();

            println!(
                "{} {} on port {}",
                SUCCESS,
                style("Starting h5i dashboard").green().bold(),
                style(port).cyan()
            );
            println!(
                "  Open {} in your browser",
                style(format!("http://localhost:{}", port))
                    .underlined()
                    .blue()
            );
            println!("  Press Ctrl+C to stop\n");

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(h5i_core::server::serve(repo_path, port))?;
        }
    Ok(())
}
