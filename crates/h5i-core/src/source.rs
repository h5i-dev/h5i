//! Where the code in a box comes from.
//!
//! An environment is pinned to an immutable base commit. Today that base can
//! be any revision in the local repository, or a pull request head fetched
//! from a remote. Roadmap M2 adds the other two sources (an external repo URL
//! and an empty box) behind the same `h5i box <source>` surface.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

use crate::error::H5iError;

/// A pull request head, fetched and pinned to a local branch.
#[derive(Debug, Clone, Serialize)]
pub struct PrBase {
    pub number: u64,
    /// PR head commit (full hex), fetched and present locally.
    pub oid: String,
    /// Local tracking branch (short name, `pr/<n>`) pinned at `oid`.
    pub local_branch: String,
    /// The PR's head branch name on its source repo (via `gh`; best-effort:
    /// `None` when `gh` is absent or the remote isn't a GitHub repo).
    pub head_ref: Option<String>,
    /// True when the PR comes from a fork (via `gh`; best-effort).
    pub cross_repo: Option<bool>,
    /// Remote the head was fetched from.
    pub remote: String,
}

/// Accept `123`, `#123`, or a GitHub PR URL
/// (`https://github.com/<owner>/<repo>/pull/<n>`).
pub fn parse_pr_spec(spec: &str) -> Result<u64, H5iError> {
    let s = spec.trim().trim_start_matches('#');
    if let Ok(n) = s.parse::<u64>()
        && n > 0
    {
        return Ok(n);
    }
    let segs: Vec<&str> = s.split('/').collect();
    for i in 0..segs.len().saturating_sub(1) {
        if segs[i] == "pull" {
            let digits: String = segs[i + 1]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<u64>()
                && n > 0
            {
                return Ok(n);
            }
        }
    }
    Err(H5iError::Metadata(format!(
        "cannot parse PR spec '{spec}' — pass a PR number or a GitHub PR URL \
         (https://github.com/<owner>/<repo>/pull/<n>)"
    )))
}

/// Fetch PR `spec`'s head from `remote` and pin it to a local `pr/<n>` branch,
/// fail-closed on collision: an existing `pr/<n>` pointing elsewhere is
/// refused, never force-moved (it may carry local work). Requires only `git`;
/// `gh` (when present) enriches the result with the PR's head branch name and
/// whether it comes from a fork.
pub fn resolve_pr_base(workdir: &Path, spec: &str, remote: &str) -> Result<PrBase, H5iError> {
    let number = parse_pr_spec(spec)?;
    // Fetch into a throwaway incoming ref, then pin a branch and drop the temp
    // ref. The branch keeps the commit alive.
    let tmp_ref = format!("refs/h5i/_incoming/pr-{number}");
    let refspec = format!("+refs/pull/{number}/head:{tmp_ref}");
    let out = Command::new("git")
        // `--end-of-options` before the remote: `--remote` is user-supplied, and
        // git would otherwise read `--upload-pack=<cmd>` as an option and run it.
        .args(["fetch", "--no-write-fetch-head", "--end-of-options", remote, &refspec])
        .current_dir(workdir)
        .output()
        .map_err(|e| H5iError::Metadata(format!("failed to invoke git fetch: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "cannot fetch PR #{number} from '{remote}': {}\n(GitHub exposes PR heads at \
             refs/pull/<n>/head — check the remote name and PR number)",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let oid = git_capture(workdir, &["rev-parse", &tmp_ref])?
        .trim()
        .to_string();

    let local_branch = format!("pr/{number}");
    let existing = git_capture(
        workdir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{local_branch}"),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    match existing {
        None => {
            git_capture(workdir, &["branch", &local_branch, &oid])?;
        }
        Some(tip) if tip == oid => {}
        Some(tip) => {
            return Err(H5iError::Metadata(format!(
                "local branch '{local_branch}' already exists at {} but PR #{number}'s head is \
                 {} — move or delete it (`git branch -D {local_branch}`) and retry",
                &tip[..12.min(tip.len())],
                &oid[..12.min(oid.len())]
            )))
        }
    }
    let _ = Command::new("git")
        .args(["update-ref", "-d", &tmp_ref])
        .current_dir(workdir)
        .status();

    // Best-effort `gh` enrichment. Never fatal (plain-git users and mirrors
    // of GitHub repos still get a fully working env).
    let (head_ref, cross_repo) = match gh_capture(
        workdir,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "headRefName,isCrossRepository",
        ],
    ) {
        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => (
                v.get("headRefName")
                    .and_then(|x| x.as_str())
                    .map(str::to_owned),
                v.get("isCrossRepository").and_then(|x| x.as_bool()),
            ),
            Err(_) => (None, None),
        },
        Err(_) => (None, None),
    };

    Ok(PrBase {
        number,
        oid,
        local_branch,
        head_ref,
        cross_repo,
        remote: remote.to_string(),
    })
}

fn git_capture(workdir: &Path, args: &[&str]) -> Result<String, H5iError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .map_err(|e| H5iError::Metadata(format!("failed to invoke git {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn gh_capture(workdir: &Path, args: &[&str]) -> Result<String, H5iError> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(workdir)
        .output()
        .map_err(|e| H5iError::Metadata(format!("failed to invoke gh {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "gh {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numbers_and_urls() {
        assert_eq!(parse_pr_spec("123").unwrap(), 123);
        assert_eq!(parse_pr_spec("#7").unwrap(), 7);
        assert_eq!(
            parse_pr_spec("https://github.com/h5i-dev/h5i/pull/42").unwrap(),
            42
        );
        assert_eq!(
            parse_pr_spec("https://github.com/h5i-dev/h5i/pull/42/files").unwrap(),
            42
        );
    }

    #[test]
    fn rejects_nonsense() {
        for bad in ["", "0", "-1", "abc", "https://example.com/issues/9"] {
            assert!(parse_pr_spec(bad).is_err(), "{bad} must be refused");
        }
    }
}
