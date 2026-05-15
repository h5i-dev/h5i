//! GitHub pull-request integration for h5i.
//!
//! Renders a sticky PR comment that surfaces h5i provenance for every commit
//! on the current branch vs. the PR's base. Uses the `gh` CLI under the hood.
//!
//! The comment is identified by an HTML marker tag so re-running `h5i pr post`
//! upserts in place rather than spamming new comments.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::repository::H5iRepository;
use crate::review::REVIEW_THRESHOLD;
use crate::review::ReviewPoint;

/// HTML marker we put at the top of the comment. `gh` returns comment bodies
/// verbatim, so we can find ours by prefix-matching this string.
pub const MARKER: &str = "<!-- h5i:pr-comment v1 -->";

/// Render the full Markdown body for the PR comment.
///
/// Includes, for each AI-authored commit on the current branch:
///   • the prompt that drove it
///   • model + agent + token usage
///   • test metrics (if any)
///   • structured decisions (if any)
///   • a 🚩 banner when `h5i notes review` would flag this commit
pub fn render_body(workdir: &Path, limit: usize) -> Result<String> {
    let repo = H5iRepository::open(workdir)?;
    let records = repo
        .h5i_log(limit)
        .context("failed to read h5i log for PR body")?;

    // Build a lookup of review points so we can flag commits inline.
    // We pull every review point above the legacy threshold but only render
    // the 🚩 when `should_flag_in_pr()` says so (Quality-tier score gates
    // the flag; Shape signals are surfaced only as supporting context).
    let review_points = repo
        .suggest_review_points(limit, REVIEW_THRESHOLD)
        .unwrap_or_default();
    let by_oid: std::collections::HashMap<String, &ReviewPoint> = review_points
        .iter()
        .map(|p| (p.commit_oid.clone(), p))
        .collect();

    let mut body = String::new();
    body.push_str(MARKER);
    body.push_str("\n");
    body.push_str("## 🪙 h5i provenance\n\n");
    body.push_str(
        "_AI provenance for every commit on this branch — prompt, model, decisions, and review signals._\n\n",
    );

    let mut ai_count = 0usize;
    let mut total_tokens: usize = 0;
    let mut tests_passing: usize = 0;
    let mut tests_failing: usize = 0;
    let mut flagged_count: usize = 0;

    for r in &records {
        let short = &r.git_oid[..r.git_oid.len().min(8)];
        let ai = match r.ai_metadata.as_ref() {
            Some(ai) => ai,
            None => continue, // skip human-only commits in the PR body
        };
        ai_count += 1;
        if let Some(u) = ai.usage.as_ref() {
            total_tokens = total_tokens.saturating_add(u.total_tokens);
        }
        if let Some(tm) = r.test_metrics.as_ref() {
            if tm.is_passing() {
                tests_passing += 1;
            } else {
                tests_failing += 1;
            }
        }

        let rp = by_oid.get(&r.git_oid).copied();
        let should_flag = rp.map(|p| p.should_flag_in_pr()).unwrap_or(false);
        if should_flag {
            flagged_count += 1;
        }

        body.push_str(&format!("### `{}` {}\n\n", short, escape_md(&first_line(&r.git_oid, &repo))));

        // Provenance block
        body.push_str(&format!(
            "- **prompt** — _{}_\n",
            escape_md(&truncate(&ai.prompt, 280))
        ));
        body.push_str(&format!(
            "- **model** — `{}` · **agent** — `{}`",
            ai.model_name, ai.agent_id
        ));
        if let Some(u) = ai.usage.as_ref() {
            body.push_str(&format!(" · **tokens** — {}", u.total_tokens));
        }
        body.push_str("\n");

        if let Some(tm) = r.test_metrics.as_ref() {
            let status = if tm.is_passing() { "✅" } else { "❌" };
            body.push_str(&format!(
                "- **tests** — {} {} passed, {} failed ({} total, {:.2}s)\n",
                status, tm.passed, tm.failed, tm.total, tm.duration_secs
            ));
        }
        if !r.decisions.is_empty() {
            body.push_str("- **decisions:**\n");
            for d in &r.decisions {
                body.push_str(&format!(
                    "  - `{}` — {} (vs. {})\n",
                    escape_md(&d.location),
                    escape_md(&d.choice),
                    if d.alternatives.is_empty() {
                        "no alternatives recorded".to_string()
                    } else {
                        d.alternatives.iter().map(|s| escape_md(s)).collect::<Vec<_>>().join(", ")
                    }
                ));
            }
        }
        // 🚩 only fires on Quality-tier score. Shape signals appear as a
        // secondary "shape" line and ONLY when at least one Quality signal
        // also fired — `LARGE_DIFF` on its own is noise.
        if let Some(p) = rp {
            if should_flag {
                let quality_rules: Vec<String> = p
                    .quality_triggers()
                    .map(|t| t.rule_id.clone())
                    .collect();
                body.push_str(&format!(
                    "- 🚩 **review signals** — score {:.2}: {}\n",
                    p.quality_score,
                    escape_md(&quality_rules.join(", "))
                ));
                let shape_rules: Vec<String> = p
                    .shape_triggers()
                    .map(|t| t.rule_id.clone())
                    .collect();
                if !shape_rules.is_empty() {
                    body.push_str(&format!(
                        "  - _shape signals (informational):_ {}\n",
                        escape_md(&shape_rules.join(", "))
                    ));
                }
            }
        }
        body.push_str("\n");
    }

    // ── Summary footer ────────────────────────────────────────────────────
    body.push_str("---\n\n");
    body.push_str(&format!(
        "**Summary:** {} AI-authored commit(s) · {} flagged for review · ",
        ai_count, flagged_count
    ));
    if tests_passing + tests_failing > 0 {
        body.push_str(&format!(
            "tests: {} ✅ / {} ❌ · ",
            tests_passing, tests_failing
        ));
    }
    body.push_str(&format!("≈{} total tokens.\n\n", total_tokens));
    body.push_str("<sub>Generated by [h5i](https://github.com/Koukyosyumei/h5i). Re-run `h5i share pr post` to refresh.</sub>\n");

    Ok(body)
}

fn first_line(oid: &str, repo: &H5iRepository) -> String {
    let summary = git2::Oid::from_str(oid)
        .ok()
        .and_then(|o| repo.git().find_commit(o).ok())
        .and_then(|c| c.summary().map(|s| s.to_string()));
    summary.unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn escape_md(s: &str) -> String {
    // Minimal escape: backticks (so file paths render correctly) and pipe
    // (in case we ever embed in tables). Newlines collapse to spaces.
    s.replace('\n', " ").replace('|', "\\|")
}

/// Post (or upsert) the PR comment for the current branch.
///
/// Strategy:
///   1. Resolve `owner/repo` and PR number via `gh`.
///   2. List existing comments; find one starting with [`MARKER`].
///   3. If found → PATCH that comment via `gh api`. Else → `gh pr comment`.
pub fn post_comment(workdir: &Path, number: Option<u64>, body: &str) -> Result<()> {
    require_gh()?;

    let repo_full = gh_capture(workdir, &["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"])
        .context("gh could not resolve the GitHub repo for this working directory. Is `gh auth status` clean?")?;
    let repo_full = repo_full.trim();
    if repo_full.is_empty() {
        return Err(anyhow!("gh returned an empty repo name. Check `gh auth status`."));
    }

    let pr_number = match number {
        Some(n) => n,
        None => {
            let raw = gh_capture(
                workdir,
                &["pr", "view", "--json", "number", "-q", ".number"],
            )
            .context("could not detect the PR for the current branch. Pass --number, or push the branch and open a PR first.")?;
            raw.trim()
                .parse::<u64>()
                .with_context(|| format!("gh returned a non-numeric PR number: {:?}", raw))?
        }
    };

    // Find existing h5i-marked comment.
    let existing_json = gh_capture(
        workdir,
        &[
            "api",
            &format!("/repos/{repo_full}/issues/{pr_number}/comments"),
        ],
    )
    .unwrap_or_default();
    let existing_id: Option<u64> = serde_json::from_str::<serde_json::Value>(&existing_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .into_iter()
        .flatten()
        .find_map(|c| {
            let body = c.get("body").and_then(|b| b.as_str()).unwrap_or("");
            if body.starts_with(MARKER) {
                c.get("id").and_then(|i| i.as_u64())
            } else {
                None
            }
        });

    if let Some(id) = existing_id {
        // Upsert via PATCH.
        gh_with_stdin(
            workdir,
            &[
                "api",
                "-X",
                "PATCH",
                &format!("/repos/{repo_full}/issues/comments/{id}"),
                "-F",
                "body=@-",
            ],
            body,
        )
        .context("gh api PATCH failed while updating the h5i PR comment")?;
        eprintln!("✔ Updated h5i comment on {}#{} (id {})", repo_full, pr_number, id);
    } else {
        // First-time post.
        gh_with_stdin(
            workdir,
            &["pr", "comment", &pr_number.to_string(), "--body-file", "-"],
            body,
        )
        .context("gh pr comment failed while posting the h5i PR comment")?;
        eprintln!("✔ Posted h5i comment on {}#{}", repo_full, pr_number);
    }

    Ok(())
}

fn require_gh() -> Result<()> {
    let status = Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(anyhow!(
            "the `gh` CLI is required for `h5i share pr` (install: https://cli.github.com/)"
        )),
    }
}

fn gh_capture(workdir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(workdir)
        .output()
        .with_context(|| format!("failed to invoke gh {:?}", args))?;
    if !out.status.success() {
        return Err(anyhow!(
            "gh {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn gh_with_stdin(workdir: &Path, args: &[&str], body: &str) -> Result<()> {
    use std::io::Write as _;
    let mut child = Command::new("gh")
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn gh {:?}", args))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("could not open gh stdin"))?;
        stdin.write_all(body.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "gh {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}
