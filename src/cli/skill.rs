//! `h5i skill`: write or print the agent skill the binary carries.

use clap::Subcommand;
use console::style;

use h5i_core::ui::SUCCESS;

#[derive(Subcommand)]
pub enum SkillCommands {
    /// Write the skill to disk. Defaults to the per-user skill directory for
    /// the current runtime; inside a box, bootstrap points `$H5I_SKILL_DIR` at
    /// the in-box location.
    Install {
        /// Write here instead of the default target.
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },

    /// Print a page to stdout: the skill itself, or one reference
    /// (`boxes`, `policy`, `export`, `troubleshooting`).
    Show {
        #[arg(value_name = "PAGE")]
        page: Option<String>,
    },

    /// Print where `install` would write.
    Path,
}

pub fn run(action: SkillCommands) -> anyhow::Result<()> {
    match action {
        SkillCommands::Install { target } => {
            let target = match target {
                Some(t) => t,
                None => h5i_core::skill::default_target()?,
            };
            let written = h5i_core::skill::install(&target)?;
            println!(
                "{} installed the {} skill ({} page(s), h5i {}) → {}",
                SUCCESS,
                style(h5i_core::skill::NAME).cyan(),
                written.len(),
                env!("CARGO_PKG_VERSION"),
                style(target.display()).dim()
            );
        }
        SkillCommands::Show { page } => {
            print!("{}", h5i_core::skill::page(page.as_deref())?);
        }
        SkillCommands::Path => {
            println!("{}", h5i_core::skill::default_target()?.display());
        }
    }
    Ok(())
}
