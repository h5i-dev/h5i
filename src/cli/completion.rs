//! `h5i completion` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(shell: clap_complete::Shell) -> anyhow::Result<()> {
    {
            clap_complete::generate(shell, &mut Cli::command(), "h5i", &mut std::io::stdout());
        }
    Ok(())
}
