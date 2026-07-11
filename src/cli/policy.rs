//! `h5i policy` — governance policy file (`.h5i/policy.toml`).

use clap::Subcommand;
use console::style;

use h5i_core::repository::H5iRepository;
use h5i_core::ui::{ERROR, SUCCESS, WARN};

#[derive(Subcommand)]
pub enum PolicyCommands {
    /// Create `.h5i/policy.toml` with starter rules
    Init,

    /// Check staged files against the current policy (dry-run)
    Check,

    /// Display the current policy configuration
    Show,
}

pub fn run(action: PolicyCommands) -> anyhow::Result<()> {
    let workdir = std::env::current_dir()?;
    match action {
        PolicyCommands::Init => {
            let path = h5i_core::policy::init_policy(&workdir)?;
            println!(
                "{} {} at {}",
                SUCCESS,
                style("Policy file created").green().bold(),
                style(path.display()).yellow()
            );
            println!(
                "  {} Edit {} to define your governance rules.",
                style("→").dim(),
                style(path.display()).cyan()
            );
        }
        PolicyCommands::Check => {
            let repo = H5iRepository::open(".")?;
            match h5i_core::policy::load_policy(&workdir)? {
                None => {
                    println!(
                        "{} No policy file found at {}",
                        WARN,
                        style(h5i_core::policy::policy_path(&workdir).display()).dim()
                    );
                    println!("  Run `h5i policy init` to create one.");
                }
                Some(cfg) => {
                    // Get staged files.
                    let staged_files: Vec<String> = {
                        let mut idx = repo.git().index()?;
                        idx.read(true)?;
                        idx.iter()
                            .map(|e| String::from_utf8_lossy(&e.path).to_string())
                            .collect()
                    };
                    let input = h5i_core::policy::CommitCheckInput {
                        message: "",
                        ai_meta: None,
                        staged_files: &staged_files,
                        audit_passed: false,
                    };
                    let violations = h5i_core::policy::check_commit(&cfg, &input);
                    if violations.is_empty() {
                        println!(
                            "{} {}",
                            SUCCESS,
                            style("No policy violations in staged files.").green()
                        );
                    } else {
                        println!(
                            "{} {} violation(s):",
                            ERROR,
                            style(violations.len()).red().bold()
                        );
                        h5i_core::policy::print_violations(&violations);
                    }
                }
            }
        }
        PolicyCommands::Show => {
            let path = h5i_core::policy::policy_path(&workdir);
            match h5i_core::policy::load_policy(&workdir)? {
                None => {
                    println!(
                        "{} No policy file found at {}",
                        WARN,
                        style(path.display()).dim()
                    );
                    println!("  Run `h5i policy init` to create one.");
                }
                Some(cfg) => {
                    h5i_core::policy::print_policy(&cfg, &path);
                }
            }
        }
    }
    Ok(())
}
