//! `h5i browser` — the control lock.
//!
//! Deliberately three verbs. Driving the browser is `agent-browser`'s job;
//! what h5i owns is arbitration between the agent and a human at the viewer,
//! because nothing upstream does it (roadmap 5.4).

use clap::Subcommand;
use console::style;

use h5i_core::ui::SUCCESS;

#[derive(Subcommand)]
pub enum BrowserCommands {
    /// Who holds control of this box's browser, and whether the agent's
    /// snapshot handles are stale.
    Status {
        name: String,
        #[arg(long)]
        json: bool,
    },

    /// Take control as a human. The agent's automation pauses at its next
    /// command boundary.
    Take { name: String },

    /// Hand control back to the agent. It must re-snapshot before acting,
    /// because the page moved while you were driving.
    Release { name: String },

    /// Print the loopback viewer URL for a box, token included. `h5i box view`
    /// is what actually serves it; this is for pasting into a browser when a
    /// forward is already running.
    Url {
        name: String,
        /// The loopback port `h5i box view` was given. Defaults to the one it
        /// picks when unspecified.
        #[arg(long, default_value_t = 7331)]
        port: u16,
    },
}

pub fn run(action: BrowserCommands) -> anyhow::Result<()> {
    let repo = git2::Repository::discover(".")?;
    let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;

    let dir_of = |name: &str| -> anyhow::Result<std::path::PathBuf> {
        let m = h5i_core::env::find(&h5i_root, name)?;
        Ok(h5i_core::env::env_dir(&h5i_root, &m.agent, &m.slug))
    };

    match action {
        BrowserCommands::Status { name, json } => {
            let dir = dir_of(&name)?;
            let c = h5i_core::control::read(&dir);
            if json {
                println!("{}", serde_json::to_string_pretty(&c)?);
                return Ok(());
            }
            println!(
                "  control  : {} (since {})",
                style(c.holder.as_str()).cyan(),
                c.since
            );
            if c.needs_resnapshot {
                println!(
                    "  {}    the agent's @refs are stale — it must `agent-browser snapshot` \
                     before acting",
                    style("stale").yellow()
                );
            }
        }
        BrowserCommands::Take { name } => {
            let c = h5i_core::control::take(&dir_of(&name)?)?;
            println!(
                "{} control taken by {} — agent automation is paused",
                SUCCESS,
                c.holder.as_str()
            );
        }
        BrowserCommands::Release { name } => {
            let c = h5i_core::control::release(&dir_of(&name)?)?;
            println!(
                "{} control returned to {} — it will re-snapshot before acting",
                SUCCESS,
                c.holder.as_str()
            );
        }
        BrowserCommands::Url { name, port } => {
            let dir = dir_of(&name)?;
            let token = h5i_core::view::read_token(&dir).ok_or_else(|| {
                anyhow::anyhow!(
                    "this box has no viewer token — it predates the viewer. Create a new box."
                )
            })?;
            // Printed whether or not a forward is running: the URL is a
            // property of the box, and `h5i box view` is what makes it answer.
            println!("http://127.0.0.1:{port}/?token={token}");
        }
    }
    Ok(())
}
