//! `h5i man` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run() -> anyhow::Result<()> {
    {
            let mut out = std::io::stdout().lock();
            render_man_page(&mut out)?;
        }
    Ok(())
}
