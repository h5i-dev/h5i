//! `h5i mcp` — CLI handler (migrated from main.rs).

pub fn run() -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;
            eprintln!(
                "h5i-mcp: listening on stdio (workdir: {})",
                workdir.display()
            );
            h5i_core::mcp::run_stdio(workdir)?;
        }
    Ok(())
}
