use chrono::{DateTime, Utc};
use console::style;
use git2::Repository;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::H5iError;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claim {
    pub id: String,
    pub text: String,
    pub evidence_paths: Vec<String>,
    /// Merkle-style hash over `(path, blob_oid)` pairs at the time the claim
    /// was recorded. Invalidated whenever any of the evidence blobs change.
    pub evidence_oid: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Live,
    Stale,
}

#[derive(Debug, Clone)]
pub struct ClaimWithStatus {
    pub claim: Claim,
    pub status: ClaimStatus,
    pub current_evidence_oid: String,
}

// ── Storage layout ────────────────────────────────────────────────────────────

fn claims_dir(h5i_root: &Path) -> PathBuf {
    h5i_root.join("claims")
}

fn claim_file(h5i_root: &Path, id: &str) -> PathBuf {
    claims_dir(h5i_root).join(format!("{id}.json"))
}

// ── Evidence OID: Merkle hash over (path, blob_oid) at HEAD ───────────────────

/// Fingerprint the current blob-OIDs of `paths` at HEAD. Any edit to any of
/// the listed files changes the result; edits to other files do not.
pub fn compute_evidence_oid(
    repo: &Repository,
    paths: &[String],
) -> Result<String, H5iError> {
    let tree = repo.head()?.peel_to_commit()?.tree()?;

    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();

    let mut hasher = Sha256::new();
    for path in sorted {
        let entry = tree.get_path(Path::new(path)).map_err(|_| {
            H5iError::InvalidPath(format!(
                "Evidence path '{path}' is not tracked in HEAD"
            ))
        })?;
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.id().as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn derive_claim_id(text: &str, timestamp: &DateTime<Utc>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.update(b"\0");
    hasher.update(timestamp.to_rfc3339().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..12].to_string()
}

// ── CRUD ──────────────────────────────────────────────────────────────────────

pub fn add(
    h5i_root: &Path,
    repo: &Repository,
    text: &str,
    evidence_paths: Vec<String>,
    author: Option<String>,
) -> Result<Claim, H5iError> {
    if text.trim().is_empty() {
        return Err(H5iError::InvalidPath(
            "Claim text cannot be empty".to_string(),
        ));
    }
    if evidence_paths.is_empty() {
        return Err(H5iError::InvalidPath(
            "At least one evidence path is required".to_string(),
        ));
    }

    let evidence_oid = compute_evidence_oid(repo, &evidence_paths)?;
    let created_at = Utc::now();
    let id = derive_claim_id(text, &created_at);

    let claim = Claim {
        id: id.clone(),
        text: text.to_string(),
        evidence_paths,
        evidence_oid,
        author: author.unwrap_or_else(resolve_default_author),
        created_at,
    };

    let dir = claims_dir(h5i_root);
    fs::create_dir_all(&dir)?;
    fs::write(claim_file(h5i_root, &id), serde_json::to_string_pretty(&claim)?)?;

    Ok(claim)
}

fn resolve_default_author() -> String {
    std::env::var("H5I_AGENT_ID").unwrap_or_else(|_| "human".to_string())
}

pub fn list_all(h5i_root: &Path) -> Result<Vec<Claim>, H5iError> {
    let dir = claims_dir(h5i_root);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut claims = vec![];
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(entry.path())?;
        if let Ok(claim) = serde_json::from_str::<Claim>(&raw) {
            claims.push(claim);
        }
    }
    claims.sort_by_key(|c| c.created_at);
    Ok(claims)
}

pub fn list_with_status(
    h5i_root: &Path,
    repo: &Repository,
) -> Result<Vec<ClaimWithStatus>, H5iError> {
    let claims = list_all(h5i_root)?;
    let mut out = Vec::with_capacity(claims.len());
    for claim in claims {
        let current = compute_evidence_oid(repo, &claim.evidence_paths)
            .unwrap_or_default();
        let status = if !current.is_empty() && current == claim.evidence_oid {
            ClaimStatus::Live
        } else {
            ClaimStatus::Stale
        };
        out.push(ClaimWithStatus {
            claim,
            status,
            current_evidence_oid: current,
        });
    }
    Ok(out)
}

pub fn live_claims(
    h5i_root: &Path,
    repo: &Repository,
) -> Result<Vec<Claim>, H5iError> {
    Ok(list_with_status(h5i_root, repo)?
        .into_iter()
        .filter(|c| c.status == ClaimStatus::Live)
        .map(|c| c.claim)
        .collect())
}

pub fn prune_stale(
    h5i_root: &Path,
    repo: &Repository,
) -> Result<usize, H5iError> {
    let all = list_with_status(h5i_root, repo)?;
    let mut removed = 0;
    for entry in all {
        if entry.status == ClaimStatus::Stale {
            let path = claim_file(h5i_root, &entry.claim.id);
            if path.exists() {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

// ── Policy (controls how aggressively agents should record claims) ────────────

/// User-tunable frequency policy: how eagerly agents are nudged to record
/// claims. Read from the `H5I_CLAIMS_FREQUENCY` environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimsFrequency {
    /// Do not record any claims this session.
    Off,
    /// Record only non-obvious, genuinely reusable facts (default).
    Low,
    /// Record any reusable codebase insight, even small-scope ones.
    High,
}

impl ClaimsFrequency {
    pub fn from_env() -> Self {
        match std::env::var("H5I_CLAIMS_FREQUENCY")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "none" | "never" => Self::Off,
            "high" | "aggressive" | "eager" => Self::High,
            _ => Self::Low,
        }
    }

    /// A short, agent-facing hint describing how often to record claims.
    /// `Low` is the default and carries no extra hint (the CLAUDE.md base
    /// instructions already cover it), so this returns `None` for that case.
    pub fn prelude_hint(self) -> Option<&'static str> {
        match self {
            Self::Off => Some(
                "[h5i] Claims frequency: OFF — do NOT record claims in this session, \
                 even if you would normally consider one worth pinning.",
            ),
            Self::Low => None,
            Self::High => Some(
                "[h5i] Claims frequency: HIGH — record claims liberally for any reusable \
                 codebase insight you confirm. TWO HARD RULES: \
                 (1) Evidence paths minimal — only files whose content, if changed, \
                 would cast doubt on the claim; not every file you read. Most good \
                 claims cite 1 file; >3 is a red flag. \
                 (2) Caveman-style text, ≈30 tokens. Drop articles + copulas + fluff. \
                 Keep paths/names/numbers exact. Example: \"HTTP only src/api/client.py: \
                 fetch_user, create_post, delete_post.\" — not \"All HTTP-making \
                 functions in this project live only in...\". The text is injected \
                 into every future session's cached prefix; every word costs forever.",
            ),
        }
    }
}

// ── Preamble rendering (for h5i context prompt) ───────────────────────────────

/// Render a Markdown section listing live claims. Returns an empty string when
/// there are none, so callers can unconditionally append.
pub fn render_preamble(claims: &[Claim]) -> String {
    if claims.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("\n## Known facts (auto-invalidated when evidence files change)\n\n");
    out.push_str("These claims were recorded in prior sessions and their evidence blobs are still byte-identical at HEAD. Treat them as pre-verified; do not re-derive unless the user asks you to.\n\n");
    for claim in claims {
        let paths = claim.evidence_paths.join(", ");
        out.push_str(&format!(
            "- {}  \n  _evidence: {}_\n",
            claim.text, paths
        ));
    }
    out.push('\n');
    out
}

// ── Terminal display ──────────────────────────────────────────────────────────

pub fn print_list(entries: &[ClaimWithStatus]) {
    if entries.is_empty() {
        println!(
            "  {} No claims recorded. Run {} to add one.",
            style("ℹ").blue(),
            style("h5i claims add …").bold()
        );
        return;
    }

    println!(
        "{}",
        style(format!(
            "{:<8}  {:<14}  {:<22}  {}",
            "STATUS", "ID", "CREATED", "TEXT"
        ))
        .bold()
        .underlined()
    );

    for entry in entries {
        let badge = match entry.status {
            ClaimStatus::Live => style("● live ").green().bold().to_string(),
            ClaimStatus::Stale => style("○ stale").yellow().bold().to_string(),
        };
        println!(
            "{}  {}  {}  {}",
            badge,
            style(&entry.claim.id).magenta(),
            style(entry.claim.created_at.format("%Y-%m-%d %H:%M UTC")).dim(),
            entry.claim.text,
        );
        let paths = entry.claim.evidence_paths.join(", ");
        println!("          {}  {}", style("↳").dim(), style(paths).dim());
    }

    let live = entries.iter().filter(|c| c.status == ClaimStatus::Live).count();
    let stale = entries.len() - live;
    println!();
    println!(
        "  {} {} live, {} stale",
        style("→").dim(),
        style(live).cyan().bold(),
        style(stale).yellow().bold(),
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs as stdfs;
    use tempfile::tempdir;

    fn init_repo_with_file(dir: &Path, path: &str, content: &str) -> Repository {
        let repo = Repository::init(dir).unwrap();
        stdfs::write(dir.join(path), content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        let tree_oid = index.write_tree().unwrap();
        let sig = Signature::now("test", "test@test").unwrap();
        {
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn edit_and_commit(repo: &Repository, dir: &Path, path: &str, content: &str) {
        stdfs::write(dir.join(path), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        let tree_oid = index.write_tree().unwrap();
        let sig = Signature::now("test", "test@test").unwrap();
        let parent_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        let tree = repo.find_tree(tree_oid).unwrap();
        let parent = repo.find_commit(parent_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "edit", &tree, &[&parent])
            .unwrap();
    }

    fn add_file_and_commit(repo: &Repository, dir: &Path, path: &str, content: &str) {
        edit_and_commit(repo, dir, path, content);
    }

    #[test]
    fn compute_evidence_oid_deterministic() {
        let workdir = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "a.txt", "hello");

        let oid1 = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();
        let oid2 = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();
        assert_eq!(oid1, oid2);
    }

    #[test]
    fn compute_evidence_oid_changes_on_file_edit() {
        let workdir = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "a.txt", "hello");

        let before = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();
        edit_and_commit(&repo, workdir.path(), "a.txt", "hello world");
        let after = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn compute_evidence_oid_stable_when_unrelated_file_changes() {
        let workdir = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "a.txt", "hello");
        add_file_and_commit(&repo, workdir.path(), "b.txt", "other");

        let before = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();
        edit_and_commit(&repo, workdir.path(), "b.txt", "changed");
        let after = compute_evidence_oid(&repo, &vec!["a.txt".into()]).unwrap();

        assert_eq!(before, after);
    }

    #[test]
    fn add_then_list_roundtrips() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        let claim = add(
            h5i.path(),
            &repo,
            "foo.rs has no retry logic",
            vec!["foo.rs".into()],
            Some("claude-code".into()),
        )
        .unwrap();

        let all = list_all(h5i.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, claim.id);
        assert_eq!(all[0].text, "foo.rs has no retry logic");
        assert_eq!(all[0].author, "claude-code");
    }

    #[test]
    fn status_is_live_when_unchanged() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        add(
            h5i.path(),
            &repo,
            "claim about foo.rs",
            vec!["foo.rs".into()],
            None,
        )
        .unwrap();

        let entries = list_with_status(h5i.path(), &repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, ClaimStatus::Live);
    }

    #[test]
    fn status_becomes_stale_after_evidence_edit() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        add(
            h5i.path(),
            &repo,
            "claim about foo.rs",
            vec!["foo.rs".into()],
            None,
        )
        .unwrap();
        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { retry(); }");

        let entries = list_with_status(h5i.path(), &repo).unwrap();
        assert_eq!(entries[0].status, ClaimStatus::Stale);
    }

    #[test]
    fn live_claims_filters_out_stale() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        add_file_and_commit(&repo, workdir.path(), "bar.rs", "fn bar() {}");

        add(h5i.path(), &repo, "claim about foo.rs", vec!["foo.rs".into()], None).unwrap();
        add(h5i.path(), &repo, "claim about bar.rs", vec!["bar.rs".into()], None).unwrap();

        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { retry(); }");

        let live = live_claims(h5i.path(), &repo).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].text, "claim about bar.rs");
    }

    #[test]
    fn prune_stale_removes_only_stale_claims() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        add_file_and_commit(&repo, workdir.path(), "bar.rs", "fn bar() {}");

        add(h5i.path(), &repo, "claim about foo.rs", vec!["foo.rs".into()], None).unwrap();
        add(h5i.path(), &repo, "claim about bar.rs", vec!["bar.rs".into()], None).unwrap();

        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { retry(); }");

        let removed = prune_stale(h5i.path(), &repo).unwrap();
        assert_eq!(removed, 1);

        let remaining = list_all(h5i.path()).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].text, "claim about bar.rs");
    }

    #[test]
    fn add_rejects_empty_text() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        let r = add(h5i.path(), &repo, "   ", vec!["foo.rs".into()], None);
        assert!(r.is_err());
    }

    #[test]
    fn add_rejects_empty_paths() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        let r = add(h5i.path(), &repo, "x", vec![], None);
        assert!(r.is_err());
    }

    #[test]
    fn add_rejects_untracked_path() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");

        let r = add(h5i.path(), &repo, "x", vec!["nope.rs".into()], None);
        assert!(r.is_err());
    }

    #[test]
    fn render_preamble_empty_when_no_claims() {
        assert_eq!(render_preamble(&[]), "");
    }

    // Env-mutating tests are combined into a single test so they cannot race
    // each other under cargo's parallel test runner.
    #[test]
    fn frequency_parses_env_var_levels_and_aliases() {
        for s in ["", "default", "low", "medium", "weird value"] {
            std::env::set_var("H5I_CLAIMS_FREQUENCY", s);
            assert_eq!(
                ClaimsFrequency::from_env(),
                ClaimsFrequency::Low,
                "expected Low for {s:?}"
            );
        }
        for s in ["off", "OFF", "none", "never", " off "] {
            std::env::set_var("H5I_CLAIMS_FREQUENCY", s);
            assert_eq!(
                ClaimsFrequency::from_env(),
                ClaimsFrequency::Off,
                "expected Off for {s:?}"
            );
        }
        for s in ["high", "HIGH", "aggressive", "eager"] {
            std::env::set_var("H5I_CLAIMS_FREQUENCY", s);
            assert_eq!(
                ClaimsFrequency::from_env(),
                ClaimsFrequency::High,
                "expected High for {s:?}"
            );
        }
        std::env::remove_var("H5I_CLAIMS_FREQUENCY");
    }

    #[test]
    fn prelude_hint_is_none_for_low() {
        assert!(ClaimsFrequency::Low.prelude_hint().is_none());
    }

    #[test]
    fn prelude_hint_for_off_says_do_not_record() {
        let hint = ClaimsFrequency::Off.prelude_hint().unwrap();
        assert!(hint.contains("OFF"));
        assert!(hint.to_lowercase().contains("do not record")
            || hint.to_lowercase().contains("do not"));
    }

    #[test]
    fn prelude_hint_for_high_encourages_recording() {
        let hint = ClaimsFrequency::High.prelude_hint().unwrap();
        assert!(hint.contains("HIGH"));
        assert!(hint.to_lowercase().contains("liberal")
            || hint.to_lowercase().contains("liberally"));
    }

    #[test]
    fn render_preamble_includes_claim_text_and_paths() {
        let claim = Claim {
            id: "abc123".into(),
            text: "foo.rs has no retry logic".into(),
            evidence_paths: vec!["src/foo.rs".into()],
            evidence_oid: "deadbeef".into(),
            author: "claude-code".into(),
            created_at: Utc::now(),
        };
        let rendered = render_preamble(&[claim]);
        assert!(rendered.contains("Known facts"));
        assert!(rendered.contains("foo.rs has no retry logic"));
        assert!(rendered.contains("src/foo.rs"));
    }
}
