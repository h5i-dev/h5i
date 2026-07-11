//! `h5i compliance` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(since: Option<String>, until: Option<String>, format: String, output: Option<std::path::PathBuf>, limit: usize) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            let policy_cfg = h5i_core::policy::load_policy(&workdir)?;

            println!(
                "{} {}",
                STEP,
                style("Scanning commits for compliance report…")
                    .cyan()
                    .bold()
            );

            let report = h5i_core::compliance::compute_compliance_report(
                &repo,
                since.as_deref(),
                until.as_deref(),
                policy_cfg.as_ref(),
                limit,
            )?;

            let content: String = match format.as_str() {
                "json" => h5i_core::compliance::to_json(&report)?,
                "html" => h5i_core::compliance::to_html(&report),
                _ => {
                    // Print text directly and return early.
                    h5i_core::compliance::print_compliance_text(&report);
                    if let Some(ref path) = output {
                        // Re-generate for file write.
                        let text = format!(
                            "h5i compliance report\n{} commits scanned · {} AI · {} policy violations\n",
                            report.total_commits, report.ai_commits, report.policy_violations
                        );
                        std::fs::write(path, text)?;
                        println!(
                            "{} Report written to {}",
                            SUCCESS,
                            style(path.display()).yellow()
                        );
                    }
                    return Ok(());
                }
            };

            if let Some(ref path) = output {
                std::fs::write(path, &content)?;
                println!(
                    "{} {} report written to {}",
                    SUCCESS,
                    style(format.to_uppercase()).cyan(),
                    style(path.display()).yellow()
                );
            } else {
                println!("{}", content);
            }
        }
    Ok(())
}
