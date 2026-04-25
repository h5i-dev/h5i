//! Blob-OID-keyed file summaries.
//!
//! A complement to `claims`: where a claim pins a *cross-file fact* via a
//! Merkle hash over multiple paths, a summary is a short text describing
//! the contents of *one* file blob, keyed by that blob's git OID. Because
//! git blobs are content-addressed and immutable, a summary written for
//! blob X is correct for blob X forever — there is no staleness, only
//! "was the current HEAD's blob covered yet?"
//!
//! Use case: an agent re-reads the same big files for orientation every
//! session. A persistent ≤200-token outline (exports, key types, the file's
//! job) that maps `blob_oid → summary` lets a session fetch the summary
//! instead of reading the whole file when only orientation is needed.
//!
//! Storage: `.git/.h5i/summaries/<blob_oid>.json`.

use chrono::{DateTime, Utc};
use console::style;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::H5iError;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileSummary {
    /// Git blob OID this summary describes (40-hex SHA-1). The summary
    /// applies to *exactly* this blob — any byte change makes a new blob,
    /// which would need its own summary.
    pub blob_oid: String,
    /// Path observed when the summary was written. Informational only;
    /// renames don't lose the summary because lookup is by blob OID.
    pub path: String,
    /// The summary text (≈100–300 tokens, markdown).
    pub text: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
}

/// View of a HEAD-tracked path joined against its summary, if one exists.
#[derive(Debug, Clone)]
pub struct HeadFileStatus {
    pub path: String,
    pub current_blob_oid: String,
    pub summary: Option<FileSummary>,
}

// ── Storage layout ────────────────────────────────────────────────────────────

fn summaries_dir(h5i_root: &Path) -> PathBuf {
    h5i_root.join("summaries")
}

fn summary_file(h5i_root: &Path, blob_oid: &str) -> PathBuf {
    summaries_dir(h5i_root).join(format!("{blob_oid}.json"))
}

// ── Lookup helpers ────────────────────────────────────────────────────────────

/// Resolve a path tracked at HEAD to its current blob OID. Errors if the
/// path is not tracked.
pub fn blob_oid_at_head(repo: &Repository, path: &str) -> Result<String, H5iError> {
    let tree = repo.head()?.peel_to_commit()?.tree()?;
    let entry = tree.get_path(Path::new(path)).map_err(|_| {
        H5iError::InvalidPath(format!(
            "Path '{path}' is not tracked in HEAD"
        ))
    })?;
    Ok(entry.id().to_string())
}

fn resolve_default_author() -> String {
    std::env::var("H5I_AGENT_ID").unwrap_or_else(|_| "human".to_string())
}

// ── CRUD ──────────────────────────────────────────────────────────────────────

/// Record a summary for the file at `path`'s current HEAD blob. If a
/// summary already exists for the same blob, it is overwritten.
pub fn set(
    h5i_root: &Path,
    repo: &Repository,
    path: &str,
    text: &str,
    author: Option<String>,
) -> Result<FileSummary, H5iError> {
    if text.trim().is_empty() {
        return Err(H5iError::InvalidPath(
            "Summary text cannot be empty".to_string(),
        ));
    }
    let blob_oid = blob_oid_at_head(repo, path)?;
    let summary = FileSummary {
        blob_oid: blob_oid.clone(),
        path: path.to_string(),
        text: text.to_string(),
        author: author.unwrap_or_else(resolve_default_author),
        created_at: Utc::now(),
    };
    let dir = summaries_dir(h5i_root);
    fs::create_dir_all(&dir)?;
    fs::write(
        summary_file(h5i_root, &blob_oid),
        serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(summary)
}

/// Look up the summary for the file at `path`'s current HEAD blob. Returns
/// `Ok(None)` when the path has no summary for its current content (the
/// path may have a summary for a *prior* blob — that's not surfaced here).
pub fn get_for_head(
    h5i_root: &Path,
    repo: &Repository,
    path: &str,
) -> Result<Option<FileSummary>, H5iError> {
    let blob_oid = blob_oid_at_head(repo, path)?;
    get_by_blob(h5i_root, &blob_oid)
}

/// Read a summary by its blob OID directly.
pub fn get_by_blob(
    h5i_root: &Path,
    blob_oid: &str,
) -> Result<Option<FileSummary>, H5iError> {
    let path = summary_file(h5i_root, blob_oid);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).ok())
}

/// All recorded summaries (across every blob ever summarised). Sorted by
/// creation time. Useful for `h5i summary list --all`.
pub fn list_all(h5i_root: &Path) -> Result<Vec<FileSummary>, H5iError> {
    let dir = summaries_dir(h5i_root);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(entry.path())?;
        if let Ok(s) = serde_json::from_str::<FileSummary>(&raw) {
            out.push(s);
        }
    }
    out.sort_by_key(|s| s.created_at);
    Ok(out)
}

/// Per HEAD-tracked path: its current blob OID and the summary (if any)
/// for that exact blob. The result is the "what does the agent see *now*"
/// view — historical summaries for older blobs of the same path are not
/// included even when present in storage.
pub fn list_for_head(
    h5i_root: &Path,
    repo: &Repository,
) -> Result<Vec<HeadFileStatus>, H5iError> {
    let tree = repo.head()?.peel_to_commit()?.tree()?;
    let mut out = Vec::new();
    walk_tree(repo, &tree, "", &mut |path, oid| {
        let summary = get_by_blob(h5i_root, &oid).ok().flatten();
        out.push(HeadFileStatus {
            path: path.to_string(),
            current_blob_oid: oid,
            summary,
        });
    })?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk_tree(
    repo: &Repository,
    tree: &git2::Tree,
    prefix: &str,
    f: &mut impl FnMut(&str, String),
) -> Result<(), H5iError> {
    for entry in tree.iter() {
        let name = entry.name().unwrap_or_default();
        let full = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                f(&full, entry.id().to_string());
            }
            Some(git2::ObjectType::Tree) => {
                if let Ok(sub) = repo.find_tree(entry.id()) {
                    walk_tree(repo, &sub, &full, f)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Drop summaries that are no longer reachable — i.e. their blob OID does
/// not match any path in HEAD. Returns the number of files removed.
/// Mirrors `claims::prune_stale` ergonomically, but the semantics differ:
/// blob-keyed summaries never go "wrong", they just become irrelevant.
pub fn prune_unreachable(
    h5i_root: &Path,
    repo: &Repository,
) -> Result<usize, H5iError> {
    let mut reachable = std::collections::HashSet::new();
    let tree = repo.head()?.peel_to_commit()?.tree()?;
    walk_tree(repo, &tree, "", &mut |_path, oid| {
        reachable.insert(oid);
    })?;

    let mut removed = 0;
    let dir = summaries_dir(h5i_root);
    if !dir.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let oid = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !reachable.contains(&oid) {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

// ── Preamble rendering (announces what's available) ───────────────────────────

/// Soft cap on how many summaries to render eagerly (i.e. inline their full
/// text in the prelude). Above this count we fall back to a listing-only
/// banner — the agent should call `h5i_summary_get(path)` for the ones it
/// cares about rather than paying for everything upfront.
pub const EAGER_COUNT_CAP: usize = 10;

/// Soft cap on total summary characters to render eagerly. Even a single
/// long summary can blow the prefix budget; this caps total injected text.
pub const EAGER_CHAR_CAP: usize = 2_000;

/// Pick eager-render vs listing-only based on count and total size budgets.
///
/// **Why eager:** the alternative is the agent calling `h5i_summary_get` once
/// per file, each call costing a separate assistant turn. Each turn re-reads
/// the cached prefix (~30K tokens of context). Loading 4 summaries totalling
/// ~600 chars adds those 600 chars to every cached-prefix read but eliminates
/// 4 round trips × 30K of cache-read — a net win whenever the eager budget is
/// not blown.
///
/// **When listing-only wins:** large repos with many summaries. Above the
/// caps, the prefix bloat from eagerly loading them all dominates the
/// per-fetch turn savings, and the agent can lazily fetch only what it needs.
pub fn render_full_or_banner(statuses: &[HeadFileStatus]) -> String {
    let with: Vec<&HeadFileStatus> = statuses
        .iter()
        .filter(|s| s.summary.is_some())
        .collect();
    if with.is_empty() {
        return String::new();
    }
    let total_chars: usize = with
        .iter()
        .map(|s| s.summary.as_ref().map(|x| x.text.len()).unwrap_or(0))
        .sum();
    let fits_eager =
        with.len() <= EAGER_COUNT_CAP && total_chars <= EAGER_CHAR_CAP;
    if fits_eager {
        render_full_preamble(&with)
    } else {
        render_availability_banner(statuses)
    }
}

fn render_full_preamble(with_summary: &[&HeadFileStatus]) -> String {
    let mut out = String::new();
    out.push_str(
        "\n## Pre-cached file summaries (orientation; trust without re-reading)\n\n",
    );
    out.push_str(
        "These are blob-OID-keyed orientations for files at HEAD. The summary \
         content is included inline below — DO NOT call `h5i_summary_get` for \
         these paths, the text is already here. Use a full `Read` only if you \
         need to *edit* a file or verify a specific line.\n\n",
    );
    for status in with_summary {
        if let Some(s) = &status.summary {
            out.push_str(&format!("### `{}`\n{}\n\n", status.path, s.text));
        }
    }
    out
}

/// Short banner describing how many of HEAD's tracked files have summaries
/// available. Designed to be cheap (no full text), so the agent learns
/// summaries exist without paying the per-file token cost upfront — it can
/// then call `h5i_summary_get` for the specific files it cares about. Used
/// by `render_full_or_banner` as the fallback when eager rendering would
/// blow the budget.
pub fn render_availability_banner(statuses: &[HeadFileStatus]) -> String {
    let total = statuses.len();
    let with_summary: Vec<&HeadFileStatus> =
        statuses.iter().filter(|s| s.summary.is_some()).collect();
    if with_summary.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "\n## File summaries available ({}/{} HEAD-tracked files)\n\n",
        with_summary.len(),
        total
    ));
    out.push_str(
        "These are pre-written, blob-OID-keyed orientations for files at HEAD. \
         Fetch one via `h5i_summary_get(path=\"…\")` instead of reading the file \
         when you only need orientation (exports, role, structure). Reading the \
         full file is still correct for line-level edits — but skip the read for \
         files you only need to *know about*.\n\n",
    );
    for status in with_summary {
        out.push_str(&format!("- `{}`\n", status.path));
    }
    out.push('\n');
    out
}

// ── Terminal display ──────────────────────────────────────────────────────────

pub fn print_list_for_head(statuses: &[HeadFileStatus]) {
    if statuses.is_empty() {
        println!(
            "  {} No files tracked in HEAD.",
            style("ℹ").blue()
        );
        return;
    }
    let with = statuses.iter().filter(|s| s.summary.is_some()).count();
    let without = statuses.len() - with;
    println!(
        "{}",
        style(format!(
            "{:<8}  {:<14}  {}",
            "STATUS", "BLOB", "PATH"
        ))
        .bold()
        .underlined()
    );
    for s in statuses {
        let badge = if s.summary.is_some() {
            style("● set  ").green().bold().to_string()
        } else {
            style("○ none ").dim().to_string()
        };
        let short_blob = &s.current_blob_oid[..s.current_blob_oid.len().min(12)];
        println!(
            "{}  {}  {}",
            badge,
            style(short_blob).magenta(),
            s.path,
        );
    }
    println!();
    println!(
        "  {} {} with summary, {} without",
        style("→").dim(),
        style(with).cyan().bold(),
        style(without).yellow().bold(),
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

    #[test]
    fn set_then_get_roundtrips() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        let s = set(
            h5i.path(),
            &repo,
            "foo.rs",
            "trivial Rust function",
            Some("test".into()),
        )
        .unwrap();
        assert_eq!(s.path, "foo.rs");
        assert_eq!(s.text, "trivial Rust function");
        assert_eq!(s.author, "test");

        let got = get_for_head(h5i.path(), &repo, "foo.rs").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().text, "trivial Rust function");
    }

    #[test]
    fn get_returns_none_after_evidence_change() {
        // Summary written for blob X. After an edit, HEAD's blob is Y; the
        // summary for X still exists but isn't returned for path lookups.
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        set(h5i.path(), &repo, "foo.rs", "v1 summary", None).unwrap();
        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { 1; }");
        let got = get_for_head(h5i.path(), &repo, "foo.rs").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn historical_summary_still_accessible_by_blob() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        let v1_oid = blob_oid_at_head(&repo, "foo.rs").unwrap();
        set(h5i.path(), &repo, "foo.rs", "v1 summary", None).unwrap();
        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { 1; }");

        let by_old_blob = get_by_blob(h5i.path(), &v1_oid).unwrap();
        assert!(by_old_blob.is_some());
        assert_eq!(by_old_blob.unwrap().text, "v1 summary");
    }

    #[test]
    fn set_overwrites_when_called_twice_for_same_blob() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        set(h5i.path(), &repo, "foo.rs", "first", None).unwrap();
        set(h5i.path(), &repo, "foo.rs", "second", None).unwrap();
        let got = get_for_head(h5i.path(), &repo, "foo.rs").unwrap().unwrap();
        assert_eq!(got.text, "second");
        assert_eq!(list_all(h5i.path()).unwrap().len(), 1);
    }

    #[test]
    fn set_rejects_empty_text() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        let r = set(h5i.path(), &repo, "foo.rs", "   ", None);
        assert!(r.is_err());
    }

    #[test]
    fn set_rejects_untracked_path() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        let r = set(h5i.path(), &repo, "missing.rs", "x", None);
        assert!(r.is_err());
    }

    #[test]
    fn list_for_head_marks_files_with_and_without_summaries() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        // Add a second tracked file.
        stdfs::write(workdir.path().join("bar.rs"), "fn bar() {}").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("bar.rs")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let sig = Signature::now("test", "test@test").unwrap();
        let parent_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        let tree = repo.find_tree(tree_oid).unwrap();
        let parent = repo.find_commit(parent_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add bar", &tree, &[&parent])
            .unwrap();

        set(h5i.path(), &repo, "foo.rs", "summary of foo", None).unwrap();

        let head = list_for_head(h5i.path(), &repo).unwrap();
        let foo = head.iter().find(|s| s.path == "foo.rs").unwrap();
        let bar = head.iter().find(|s| s.path == "bar.rs").unwrap();
        assert!(foo.summary.is_some());
        assert!(bar.summary.is_none());
    }

    #[test]
    fn prune_unreachable_drops_orphaned_blob_summaries() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        set(h5i.path(), &repo, "foo.rs", "v1", None).unwrap();
        edit_and_commit(&repo, workdir.path(), "foo.rs", "fn foo() { 2; }");
        // Now the v1 blob is no longer reachable from HEAD.
        let removed = prune_unreachable(h5i.path(), &repo).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(list_all(h5i.path()).unwrap().len(), 0);
    }

    #[test]
    fn prune_unreachable_keeps_live_blob_summaries() {
        let workdir = tempdir().unwrap();
        let h5i = tempdir().unwrap();
        let repo = init_repo_with_file(workdir.path(), "foo.rs", "fn foo() {}");
        set(h5i.path(), &repo, "foo.rs", "v1", None).unwrap();
        let removed = prune_unreachable(h5i.path(), &repo).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(list_all(h5i.path()).unwrap().len(), 1);
    }

    #[test]
    fn render_availability_banner_lists_paths_with_summaries_only() {
        let statuses = vec![
            HeadFileStatus {
                path: "foo.rs".into(),
                current_blob_oid: "abc".into(),
                summary: Some(FileSummary {
                    blob_oid: "abc".into(),
                    path: "foo.rs".into(),
                    text: "x".into(),
                    author: "a".into(),
                    created_at: Utc::now(),
                }),
            },
            HeadFileStatus {
                path: "bar.rs".into(),
                current_blob_oid: "def".into(),
                summary: None,
            },
        ];
        let banner = render_availability_banner(&statuses);
        assert!(banner.contains("File summaries available"));
        assert!(banner.contains("`foo.rs`"));
        assert!(!banner.contains("`bar.rs`"));
    }

    #[test]
    fn render_availability_banner_empty_when_no_summaries() {
        let statuses = vec![HeadFileStatus {
            path: "foo.rs".into(),
            current_blob_oid: "abc".into(),
            summary: None,
        }];
        assert_eq!(render_availability_banner(&statuses), "");
    }

    fn make_status(path: &str, oid: &str, summary_text: Option<&str>) -> HeadFileStatus {
        HeadFileStatus {
            path: path.into(),
            current_blob_oid: oid.into(),
            summary: summary_text.map(|t| FileSummary {
                blob_oid: oid.into(),
                path: path.into(),
                text: t.into(),
                author: "test".into(),
                created_at: Utc::now(),
            }),
        }
    }

    #[test]
    fn render_full_or_banner_eager_inlines_content_when_under_caps() {
        let statuses = vec![
            make_status("foo.rs", "a", Some("foo summary text")),
            make_status("bar.rs", "b", Some("bar summary text")),
        ];
        let out = render_full_or_banner(&statuses);
        assert!(out.contains("Pre-cached file summaries"));
        assert!(out.contains("foo summary text"));
        assert!(out.contains("bar summary text"));
        assert!(out.contains("DO NOT call `h5i_summary_get`"));
    }

    #[test]
    fn render_full_or_banner_falls_back_to_banner_when_count_over_cap() {
        // EAGER_COUNT_CAP + 1 summaries, each tiny.
        let statuses: Vec<HeadFileStatus> = (0..(EAGER_COUNT_CAP + 1))
            .map(|i| {
                make_status(
                    &format!("f{i}.rs"),
                    &format!("oid{i}"),
                    Some("x"),
                )
            })
            .collect();
        let out = render_full_or_banner(&statuses);
        assert!(out.contains("File summaries available"));
        // Should NOT inline content (banner mode lists paths only).
        assert!(!out.contains("DO NOT call"));
    }

    #[test]
    fn render_full_or_banner_falls_back_when_total_chars_over_cap() {
        let big = "x".repeat(EAGER_CHAR_CAP + 100);
        let statuses = vec![make_status("foo.rs", "a", Some(&big))];
        let out = render_full_or_banner(&statuses);
        assert!(out.contains("File summaries available"));
        // Should NOT inline the content (it's huge).
        assert!(!out.contains(&big[..50]));
    }

    #[test]
    fn render_full_or_banner_empty_when_no_summaries() {
        let statuses = vec![make_status("foo.rs", "a", None)];
        assert_eq!(render_full_or_banner(&statuses), "");
    }
}
