//! `h5i doctor` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(repair: bool, export: Option<PathBuf>, json: bool) -> anyhow::Result<()> {
    {
            let git_repo = git2::Repository::discover(".")?;
            let report = storage::doctor(&git_repo, repair, export.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_doctor_report(&report);
            }
            if !report.ok {
                std::process::exit(2);
            }
        }
    Ok(())
}
