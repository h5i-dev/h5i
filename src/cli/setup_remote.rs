//! `h5i setup_remote` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(remote: String, dry_run: bool) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;
            cmd_setup_remote(&remote, dry_run, &workdir)?;
        }
    Ok(())
}
