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
    /// (`app`, `test`, `websec`, `policy`, …).
    Show {
        #[arg(value_name = "PAGE")]
        page: Option<String>,
    },

    /// Print where `install` would write.
    Path,

    /// Write the Spec Kit preset for h5i-app features. Install it in a Spec
    /// Kit project with `specify preset add --dev <dir>`.
    Speckit {
        /// Write here instead of `./h5i-speckit-preset`.
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },
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
        SkillCommands::Speckit { target } => {
            let target = target.unwrap_or_else(|| h5i_core::speckit::DEFAULT_DIR.into());
            let written = h5i_core::speckit::export(&target, env!("CARGO_PKG_VERSION"))?;
            println!(
                "{} wrote the Spec Kit preset ({} file(s), h5i {}) → {}",
                SUCCESS,
                written.len(),
                env!("CARGO_PKG_VERSION"),
                style(target.display()).dim()
            );
            println!(
                "  install it from a Spec Kit project: specify preset add --dev {}",
                target.display()
            );
        }
    }
    Ok(())
}
