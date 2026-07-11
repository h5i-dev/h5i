//! `h5i recall_rm` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(branch: String, force: bool) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;
            cmd_recall_rm(&workdir, &branch, force)?;
        }
    Ok(())
}
