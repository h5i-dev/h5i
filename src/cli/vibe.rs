//! `h5i vibe` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(limit: usize, json: bool) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let report = h5i_core::vibe::compute_vibe_report(&repo, limit)?;
            if json {
                #[derive(serde::Serialize)]
                struct VibeJson<'a> {
                    repo_name: &'a str,
                    total_commits: usize,
                    ai_commits: usize,
                    ai_pct: f32,
                    human_authors: &'a [String],
                    ai_models: &'a [(String, usize)],
                    total_blind_edits: usize,
                    blind_edit_file_count: usize,
                }
                let out = VibeJson {
                    repo_name: &report.repo_name,
                    total_commits: report.total_commits,
                    ai_commits: report.ai_commits,
                    ai_pct: report.ai_pct(),
                    human_authors: &report.human_authors,
                    ai_models: &report.ai_models,
                    total_blind_edits: report.total_blind_edits,
                    blind_edit_file_count: report.blind_edit_file_count,
                };
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                h5i_core::vibe::print_vibe_report(&report);
            }
        }
    Ok(())
}
