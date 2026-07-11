//! `h5i migrate_remote` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(remote: String, dry_run: bool) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;
            cmd_migrate_remote(&remote, dry_run, &workdir)?;
        }
    Ok(())
}
