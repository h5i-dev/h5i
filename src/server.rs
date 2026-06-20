use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::ctx;
use crate::memory;
use crate::metadata::{IntegrityReport, IntentGraph};
use crate::repository::H5iRepository;
use crate::review::{ReviewPoint, REVIEW_THRESHOLD};
use crate::session_log;

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub repo_path: PathBuf,
}

// ── API response types ────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub struct EnrichedCommit {
    pub git_oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub timestamp: String,
    // AI provenance
    pub ai_model: Option<String>,
    pub ai_agent: Option<String>,
    pub ai_prompt: Option<String>,
    pub ai_tokens: Option<usize>,
    // Test metrics — legacy field kept for backward-compat with old notes
    pub test_coverage: Option<f64>,
    // Test metrics — rich fields (populated when adapter JSON was used)
    pub test_passed: Option<u64>,
    pub test_failed: Option<u64>,
    pub test_skipped: Option<u64>,
    pub test_total: Option<u64>,
    pub test_duration_secs: Option<f64>,
    pub test_tool: Option<String>,
    pub test_exit_code: Option<i32>,
    pub test_summary: Option<String>,
    pub test_is_passing: Option<bool>,
    // Causal chain
    pub caused_by: Vec<String>,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogQuery {
    pub limit: Option<usize>,
    /// Optional branch (or any ref-ish: tag, "origin/x", abbrev). When omitted
    /// the walk starts at HEAD as before.
    pub branch: Option<String>,
}

#[derive(Deserialize)]
pub struct IntegrityQuery {
    pub message: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Deserialize)]
pub struct CommitIntegrityQuery {
    pub oid: String,
}

#[derive(Deserialize)]
pub struct IntentGraphQuery {
    pub limit: Option<usize>,
    pub mode: Option<String>,
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    pub limit: Option<usize>,
    pub min_score: Option<f32>,
}

#[derive(Deserialize)]
pub struct MemoryDiffQuery {
    pub from: String,
    /// OID of the second snapshot; omit to diff against live memory.
    pub to: Option<String>,
}

// ── Memory API response types ─────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct MemoryFileEntry {
    pub name: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct MemorySnapshotResponse {
    pub commit_oid: String,
    pub short_oid: String,
    pub timestamp: String,
    pub file_count: usize,
    pub files: Vec<MemoryFileEntry>,
}

#[derive(Serialize)]
pub struct DiffLineResponse {
    pub kind: String, // "context" | "added" | "removed"
    pub text: String,
}

#[derive(Serialize)]
pub struct ModifiedFileResponse {
    pub name: String,
    pub hunks: Vec<DiffLineResponse>,
}

#[derive(Serialize, Default)]
pub struct MemoryDiffResponse {
    pub from_label: String,
    pub to_label: String,
    pub added_files: Vec<MemoryFileEntry>,
    pub removed_files: Vec<String>,
    pub modified_files: Vec<ModifiedFileResponse>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Normalise a git remote URL to a browseable HTTPS GitHub URL, or None.
fn github_url_from_remote(url: &str) -> Option<String> {
    if !url.contains("github.com") {
        return None;
    }
    let s = if url.starts_with("git@github.com:") {
        url.replacen("git@github.com:", "https://github.com/", 1)
    } else {
        url.to_string()
    };
    Some(s.trim_end_matches(".git").to_string())
}

fn make_integrity_report(score: f32, level: crate::metadata::IntegrityLevel, findings: Vec<crate::metadata::RuleFinding>) -> IntegrityReport {
    IntegrityReport { level, score, findings }
}

fn fallback_report() -> IntegrityReport {
    make_integrity_report(1.0, crate::metadata::IntegrityLevel::Valid, vec![])
}

// ── Handlers ──────────────────────────────────────────────────────────────────

// `/` serves the React + Blueprint workbench (web/dist/) bundled into the
// binary at compile time.
//
// In debug builds rust-embed reads from disk on each request, so editing
// frontend files and re-running `npm run build` updates the served bundle
// without rebuilding the Rust binary. Release builds embed at compile time.

#[derive(rust_embed::Embed)]
#[folder = "web/dist/"]
struct WebAsset;

async fn index() -> Response {
    serve_embedded("index.html")
}

async fn workbench_asset(Path(path): Path<String>) -> Response {
    // The /assets/*path route strips the "assets/" prefix from the URL, but
    // rust-embed indexes by the full path under web/dist/, so we put it back.
    serve_embedded(&format!("assets/{}", path))
}

fn serve_embedded(path: &str) -> Response {
    match WebAsset::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn api_repo(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();

        let branch = git
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from))
            .unwrap_or_else(|| "HEAD".to_string());

        let name = git
            .workdir()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Auto-detect GitHub URL from "origin" remote
        let github_url = git
            .find_remote("origin")
            .ok()
            .and_then(|r| r.url().map(|u| u.to_string()))
            .and_then(|u| github_url_from_remote(&u));

        let records = repo.get_log(2000)?;
        let total = records.len();
        let ai = records.iter().filter(|r| r.ai_metadata.is_some()).count();
        let with_tests = records.iter().filter(|r| r.test_metrics.is_some()).count();

        // Aggregate test pass rate across all commits that have test data
        let (tests_pass, tests_total) = records.iter().fold((0usize, 0usize), |(p, t), r| {
            if let Some(tm) = &r.test_metrics {
                (p + if tm.is_passing() { 1 } else { 0 }, t + 1)
            } else {
                (p, t)
            }
        });
        let pass_rate = if tests_total > 0 {
            Some((tests_pass as f64 / tests_total as f64) * 100.0)
        } else {
            None
        };

        Ok(serde_json::json!({
            "name": name,
            "branch": branch,
            "total_commits": total,
            "ai_commits": ai,
            "tested_commits": with_tests,
            "test_pass_rate": pass_rate,
            "github_url": github_url,
        }))
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(serde_json::json!({}))).unwrap_or_default())
}

async fn api_commits(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LogQuery>,
) -> Json<Vec<EnrichedCommit>> {
    let path = state.repo_path.clone();
    let limit = params.limit.unwrap_or(100);

    let branch = params.branch.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<EnrichedCommit>> {
        let repo = H5iRepository::open(&path)?;
        let records = match branch.as_deref() {
            Some(b) if !b.is_empty() => repo.get_log_at_branch(b, limit)?,
            _ => repo.get_log(limit)?,
        };
        let mut enriched = Vec::new();

        for record in records {
            let oid = git2::Oid::from_str(&record.git_oid)?;
            let commit = repo.git().find_commit(oid)?;

            let message = commit.message().unwrap_or("").trim().to_string();
            let author = commit.author().name().unwrap_or("Unknown").to_string();
            let short_oid = record.git_oid[..8.min(record.git_oid.len())].to_string();
            let timestamp = record.timestamp.to_rfc3339();

            let (ai_model, ai_agent, ai_prompt, ai_tokens) =
                if let Some(ai) = &record.ai_metadata {
                    let tokens = ai.usage.as_ref().map(|u| u.total_tokens);
                    (
                        Some(ai.model_name.clone()).filter(|s| !s.is_empty()),
                        Some(ai.agent_id.clone()).filter(|s| !s.is_empty()),
                        Some(ai.prompt.clone()).filter(|s| !s.is_empty()),
                        tokens,
                    )
                } else {
                    (None, None, None, None)
                };

            let (
                test_coverage,
                test_passed,
                test_failed,
                test_skipped,
                test_total,
                test_duration_secs,
                test_tool,
                test_exit_code,
                test_summary,
                test_is_passing,
            ) = if let Some(tm) = &record.test_metrics {
                (
                    Some(tm.coverage),
                    Some(tm.passed),
                    Some(tm.failed),
                    Some(tm.skipped),
                    Some(tm.total),
                    Some(tm.duration_secs),
                    tm.tool.clone(),
                    tm.exit_code,
                    tm.summary.clone(),
                    Some(tm.is_passing()),
                )
            } else {
                (None, None, None, None, None, None, None, None, None, None)
            };

            let caused_by = record.caused_by.clone();

            enriched.push(EnrichedCommit {
                git_oid: record.git_oid,
                short_oid,
                message,
                author,
                timestamp,
                ai_model,
                ai_agent,
                ai_prompt,
                ai_tokens,
                test_coverage,
                test_passed,
                test_failed,
                test_skipped,
                test_total,
                test_duration_secs,
                test_tool,
                test_exit_code,
                test_summary,
                test_is_passing,
                caused_by,
            });
        }

        Ok(enriched)
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(vec![])).unwrap_or_default())
}

// ── Branches ──────────────────────────────────────────────────────────────────
//
// `/api/branches` is the unified branch list: git side (ahead/behind, last
// commit, AI ratio) joined with the matching context branch (by name) so the
// frontend can render a single branch row as the integrated unit.

#[derive(Serialize, Default)]
struct BranchLastCommit {
    oid: String,
    short_oid: String,
    message: String,
    author: String,
    timestamp: String,
}

#[derive(Serialize, Default)]
struct ContextBranchLink {
    name: String,
    purpose: String,
    last_milestone: String,
    last_activity: String,
    milestone_count: usize,
    trace_lines: usize,
    snapshot_count: usize,
    todo_count: usize,
}

#[derive(Serialize, Default)]
struct BranchInfo {
    name: String,
    is_head: bool,
    is_remote: bool,
    upstream: Option<String>,
    target_oid: Option<String>,
    /// Commits ahead of upstream (None when no upstream tracking).
    ahead: Option<usize>,
    /// Commits behind upstream (None when no upstream tracking).
    behind: Option<usize>,
    /// Tip of the branch — most recent commit.
    last_commit: Option<BranchLastCommit>,
    /// AI-assisted commits within the last `walked_commit_count` commits.
    ai_commit_count: Option<usize>,
    /// How many commits we walked from the branch tip (cap to keep API fast).
    walked_commit_count: Option<usize>,
    /// Same-named context branch info, when one exists. Lets the UI render
    /// git + reasoning as a single row per branch.
    context: Option<ContextBranchLink>,
    /// True if a context branch with the same name exists. Lets the UI offer
    /// "Create context for this branch" inline when this is false.
    has_context_branch: bool,
}

const BRANCH_WALK_CAP: usize = 100;

async fn api_branches(State(state): State<Arc<AppState>>) -> Json<Vec<BranchInfo>> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<BranchInfo>> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let head_name = git
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from));

        // Pre-compute context-branch listing once (cheap) so we can match by name
        // for every git branch without re-listing.
        let workdir = git.workdir().map(|p| p.to_path_buf());
        let ctx_branches: HashSet<String> = workdir
            .as_ref()
            .map(|w| ctx::list_branches(w).into_iter().collect())
            .unwrap_or_default();

        // Snapshot counts per context branch — same pattern api_context_status uses.
        let snapshot_counts: HashMap<String, usize> = list_context_snapshots(git)
            .iter()
            .fold(HashMap::new(), |mut acc, s| {
                *acc.entry(s.branch.clone()).or_insert(0) += 1;
                acc
            });

        let mut out = Vec::new();
        for typ in [git2::BranchType::Local, git2::BranchType::Remote] {
            let branches = match git.branches(Some(typ)) {
                Ok(b) => b,
                Err(_) => continue,
            };
            for b in branches.flatten() {
                let (branch, _) = b;
                let name = match branch.name() {
                    Ok(Some(n)) => n.to_string(),
                    _ => continue,
                };
                if name.ends_with("/HEAD") {
                    continue;
                }
                let is_local = matches!(typ, git2::BranchType::Local);
                let target_oid = branch.get().target();
                let upstream_ref = if is_local { branch.upstream().ok() } else { None };
                let upstream_name = upstream_ref
                    .as_ref()
                    .and_then(|u| u.name().ok().flatten().map(String::from));

                // Ahead/behind only meaningful when upstream exists.
                let (ahead, behind) = match (target_oid, upstream_ref.as_ref().and_then(|u| u.get().target())) {
                    (Some(local), Some(remote)) => {
                        match git.graph_ahead_behind(local, remote) {
                            Ok((a, b)) => (Some(a), Some(b)),
                            Err(_) => (None, None),
                        }
                    }
                    _ => (None, None),
                };

                // Walk commits from tip — only for local branches (remote tracking
                // refs have the same data as their local counterparts in most cases).
                let (last_commit, ai_count, walked) = if is_local {
                    if let Some(oid) = target_oid {
                        walk_branch_tip(&repo, oid, BRANCH_WALK_CAP)
                    } else {
                        (None, None, None)
                    }
                } else {
                    (None, None, None)
                };

                // Context branch match by name.
                let has_ctx = ctx_branches.contains(&name);
                let context = if is_local && has_ctx {
                    workdir
                        .as_ref()
                        .and_then(|w| build_context_branch_link(git, w, &name, &snapshot_counts))
                } else {
                    None
                };

                out.push(BranchInfo {
                    is_head: is_local && head_name.as_deref() == Some(name.as_str()),
                    is_remote: !is_local,
                    name: name.clone(),
                    upstream: upstream_name,
                    target_oid: target_oid.map(|o| o.to_string()),
                    ahead,
                    behind,
                    last_commit,
                    ai_commit_count: ai_count,
                    walked_commit_count: walked,
                    context,
                    has_context_branch: is_local && has_ctx,
                });
            }
        }
        // Local branches first (sorted), then remotes (sorted).
        out.sort_by(|a, b| match (a.is_remote, b.is_remote) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
        Ok(out)
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(vec![])).unwrap_or_default())
}

// Walks up to `cap` commits from `tip`, returning (latest commit info, count
// of those commits with AI metadata, count actually walked). Used by
// /api/branches to show the tip + AI density for each branch.
fn walk_branch_tip(
    repo: &H5iRepository,
    tip: git2::Oid,
    cap: usize,
) -> (Option<BranchLastCommit>, Option<usize>, Option<usize>) {
    let git = repo.git();
    let mut revwalk = match git.revwalk() {
        Ok(r) => r,
        Err(_) => return (None, None, None),
    };
    if revwalk.push(tip).is_err() {
        return (None, None, None);
    }

    let mut latest: Option<BranchLastCommit> = None;
    let mut walked = 0usize;
    let mut ai = 0usize;
    for oid_res in revwalk.take(cap) {
        let oid = match oid_res {
            Ok(o) => o,
            Err(_) => break,
        };
        if walked == 0 {
            if let Ok(commit) = git.find_commit(oid) {
                let msg = commit.message().unwrap_or("").trim().to_string();
                let author = commit.author().name().unwrap_or("Unknown").to_string();
                let ts = commit.time();
                let timestamp =
                    chrono::DateTime::<chrono::Utc>::from_timestamp(ts.seconds(), 0)
                        .map(|d| d.to_rfc3339())
                        .unwrap_or_default();
                latest = Some(BranchLastCommit {
                    oid: oid.to_string(),
                    short_oid: format!("{:.8}", oid.to_string()),
                    message: msg,
                    author,
                    timestamp,
                });
            }
        }
        walked += 1;
        if let Ok(record) = repo.load_h5i_record(oid) {
            if record.ai_metadata.is_some() {
                ai += 1;
            }
        }
    }
    (latest, Some(ai), Some(walked))
}

// Build a ContextBranchLink for a context branch matching the given git
// branch name. Mirrors the inline computation in api_context_status; pulled
// out so api_branches can reuse it.
fn build_context_branch_link(
    repo: &git2::Repository,
    workdir: &std::path::Path,
    name: &str,
    snapshot_counts: &HashMap<String, usize>,
) -> Option<ContextBranchLink> {
    let commit_text = read_ctx_file(repo, &format!("branches/{name}/commit.md"))?;
    let trace_text =
        read_ctx_file(repo, &format!("branches/{name}/trace.md")).unwrap_or_default();
    let branch_commits = parse_commit_contributions(&commit_text);
    let trace_lines = count_trace_entries(&trace_text);
    let branch_ctx = ctx::gcc_context(
        workdir,
        &ctx::ContextOpts {
            branch: Some(name.to_string()),
            commit_hash: None,
            show_log: false,
            log_offset: 0,
            metadata_segment: None,
            window: 10,
            depth: 2,
        },
    )
    .ok()
    .unwrap_or_default();
    Some(ContextBranchLink {
        name: name.to_string(),
        purpose: extract_branch_purpose(&commit_text, name),
        last_milestone: branch_commits.last().cloned().unwrap_or_default(),
        last_activity: extract_last_commit_timestamp(&commit_text),
        milestone_count: branch_commits.len(),
        trace_lines,
        snapshot_count: snapshot_counts.get(name).copied().unwrap_or(0),
        todo_count: branch_ctx.todo_items.len(),
    })
}

// ── Files changed by a single commit ──────────────────────────────────────────
//
// Used by the per-commit Context tab to look up which paths to ask
// `/api/context/relevant?file=X` about.

#[derive(Deserialize)]
pub struct CommitFilesQuery {
    pub oid: String,
}

#[derive(Serialize, Default)]
struct CommitFiles {
    oid: String,
    files: Vec<String>,
    truncated: bool,
}

async fn api_commit_files(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CommitFilesQuery>,
) -> Json<CommitFiles> {
    let path = state.repo_path.clone();
    let oid_str = params.oid.clone();
    const MAX_FILES: usize = 100;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<CommitFiles> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let oid = git2::Oid::from_str(&oid_str)?;
        let commit = git.find_commit(oid)?;
        let tree = commit.tree()?;
        // Diff against first parent (or the empty tree for the root commit).
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let diff = git.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

        let mut files: Vec<String> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if files.len() >= MAX_FILES {
                    return false;
                }
                if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    if let Some(s) = p.to_str() {
                        files.push(s.to_string());
                    }
                }
                true
            },
            None,
            None,
            None,
        )?;

        let truncated = files.len() >= MAX_FILES;
        files.sort();
        files.dedup();
        Ok(CommitFiles {
            oid: oid_str,
            files,
            truncated,
        })
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(CommitFiles::default())).unwrap_or_default())
}

/// Integrity check against the *current staging area* (manual form).
async fn api_integrity(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IntegrityQuery>,
) -> Json<IntegrityReport> {
    let path = state.repo_path.clone();
    let message = params.message.unwrap_or_default();
    let prompt = params.prompt;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<IntegrityReport> {
        let repo = H5iRepository::open(&path)?;
        Ok(repo.verify_integrity(prompt.as_deref(), &message)?)
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(fallback_report())).unwrap_or_else(|_| fallback_report()))
}

/// Integrity check against a *historical* commit's own diff.
async fn api_integrity_commit(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CommitIntegrityQuery>,
) -> Json<IntegrityReport> {
    let path = state.repo_path.clone();
    let oid_str = params.oid;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<IntegrityReport> {
        let repo = H5iRepository::open(&path)?;
        let oid = git2::Oid::from_str(&oid_str)?;
        Ok(repo.verify_commit_integrity(oid)?)
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(fallback_report())).unwrap_or_else(|_| fallback_report()))
}

async fn api_intent_graph(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IntentGraphQuery>,
) -> Json<IntentGraph> {
    let path = state.repo_path.clone();
    let limit = params.limit.unwrap_or(30);
    let analyze = params.mode.as_deref().unwrap_or("prompt") == "analyze";

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<IntentGraph> {
        let repo = H5iRepository::open(&path)?;
        Ok(repo.build_intent_graph(limit, analyze)?)
    })
    .await;

    Json(
        result
            .unwrap_or_else(|_| Ok(IntentGraph::default()))
            .unwrap_or_default(),
    )
}

async fn api_review_points(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ReviewQuery>,
) -> Json<Vec<ReviewPoint>> {
    let path = state.repo_path.clone();
    let limit = params.limit.unwrap_or(100);
    let min_score = params.min_score.unwrap_or(REVIEW_THRESHOLD);

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ReviewPoint>> {
        let repo = H5iRepository::open(&path)?;
        Ok(repo.suggest_review_points(limit, min_score)?)
    })
    .await;

    Json(
        result
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default(),
    )
}

// ── Memory handlers ───────────────────────────────────────────────────────────

async fn api_memory_snapshots(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<MemorySnapshotResponse>> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemorySnapshotResponse>> {
        let repo = H5iRepository::open(&path)?;
        let snapshots = memory::list_snapshots(&repo.h5i_root)?;
        let mut out = Vec::new();
        for snap in snapshots.iter().rev() {
            let snap_dir = repo.h5i_root.join("memory").join(&snap.commit_oid);
            let mut files: Vec<MemoryFileEntry> = std::fs::read_dir(&snap_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path().is_file()
                                && e.file_name() != "_meta.json"
                        })
                        .map(|e| MemoryFileEntry {
                            name: e.file_name().to_string_lossy().into_owned(),
                            content: std::fs::read_to_string(e.path()).unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            files.sort_by(|a, b| a.name.cmp(&b.name));
            out.push(MemorySnapshotResponse {
                short_oid: snap.commit_oid[..8.min(snap.commit_oid.len())].to_string(),
                commit_oid: snap.commit_oid.clone(),
                timestamp: snap.timestamp.to_rfc3339(),
                file_count: snap.file_count,
                files,
            });
        }
        Ok(out)
    })
    .await;
    Json(result.unwrap_or_else(|_| Ok(vec![])).unwrap_or_default())
}

async fn api_memory_diff(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MemoryDiffQuery>,
) -> Json<MemoryDiffResponse> {
    let path = state.repo_path.clone();
    let from = params.from.clone();
    let to = params.to.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<MemoryDiffResponse> {
        let repo = H5iRepository::open(&path)?;
        let workdir = repo
            .git()
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("bare repository"))?
            .to_path_buf();
        let diff = memory::diff_snapshots(
            &repo.h5i_root,
            &workdir,
            &from,
            to.as_deref(),
            memory::MemoryAgent::from_env(),
        )?;
        Ok(MemoryDiffResponse {
            from_label: diff.from_label,
            to_label: diff.to_label,
            added_files: diff
                .added_files
                .into_iter()
                .map(|(name, content)| MemoryFileEntry { name, content })
                .collect(),
            removed_files: diff.removed_files.into_iter().map(|(name, _)| name).collect(),
            modified_files: diff
                .modified_files
                .into_iter()
                .map(|f| ModifiedFileResponse {
                    name: f.name,
                    hunks: f
                        .hunks
                        .into_iter()
                        .map(|l| match l {
                            memory::DiffLine::Context(t) => {
                                DiffLineResponse { kind: "context".into(), text: t }
                            }
                            memory::DiffLine::Added(t) => {
                                DiffLineResponse { kind: "added".into(), text: t }
                            }
                            memory::DiffLine::Removed(t) => {
                                DiffLineResponse { kind: "removed".into(), text: t }
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
    })
    .await;
    Json(result.unwrap_or_else(|_| Ok(MemoryDiffResponse::default())).unwrap_or_default())
}

// ── Session log API ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SessionLogQuery {
    pub commit: Option<String>,
    pub file: Option<String>,
}

async fn api_session_log(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionLogQuery>,
) -> Json<Option<session_log::SessionAnalysis>> {
    let result = tokio::task::spawn_blocking(move || {
        let repo = H5iRepository::open(&state.repo_path).ok()?;
        let oid_str = match params.commit {
            Some(ref s) => s.clone(),
            None => repo.git().head().ok()?.peel_to_commit().ok()?.id().to_string(),
        };
        session_log::load_analysis(&repo.h5i_root, &oid_str).ok().flatten()
    })
    .await;
    Json(result.unwrap_or(None))
}

async fn api_session_churn(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<session_log::FileChurn>> {
    let result = tokio::task::spawn_blocking(move || {
        let repo = H5iRepository::open(&state.repo_path).ok()?;
        Some(session_log::aggregate_churn(&repo.h5i_root))
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(Serialize)]
struct SessionLogMeta {
    commit_oid: String,
    session_id: String,
    analyzed_at: String,
    message_count: usize,
    tool_call_count: usize,
    edited_count: usize,
    consulted_count: usize,
    uncertainty_count: usize,
}

async fn api_session_list(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<SessionLogMeta>> {
    let result = tokio::task::spawn_blocking(move || {
        let repo = H5iRepository::open(&state.repo_path).ok()?;
        let oids = session_log::list_analyses(&repo.h5i_root);
        let metas: Vec<SessionLogMeta> = oids
            .iter()
            .rev()
            .filter_map(|oid| {
                let a = session_log::load_analysis(&repo.h5i_root, oid).ok()??;
                Some(SessionLogMeta {
                    commit_oid: oid.clone(),
                    session_id: a.session_id.clone(),
                    analyzed_at: a.analyzed_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                    message_count: a.message_count,
                    tool_call_count: a.tool_call_count,
                    edited_count: a.footprint.edited.len(),
                    consulted_count: a.footprint.consulted.len(),
                    uncertainty_count: a.uncertainty.len(),
                })
            })
            .collect();
        Some(metas)
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

// ── Context API handlers ──────────────────────────────────────────────────────

#[derive(Serialize, Default)]
struct ContextStatusResponse {
    initialized: bool,
    current_branch: String,
    git_branch: String,
    git_branch_goal: String,
    goal: String,
    branch_count: usize,
    branches: Vec<String>,
    commit_count: usize,
    trace_lines: usize,
    snapshot_count: usize,
    stable_line_count: usize,
    dynamic_line_count: usize,
    todo_count: usize,
    latest_snapshot_timestamp: String,
    stale_branch_count: usize,
    branch_summaries: Vec<ContextBranchSummary>,
}

#[derive(Serialize, Default, Clone)]
struct ContextBranchSummary {
    branch: String,
    purpose: String,
    last_milestone: String,
    last_activity: String,
    todo_count: usize,
    trace_lines: usize,
    milestone_count: usize,
    snapshot_count: usize,
    exclusive_milestones: usize,
    exclusive_trace_lines: usize,
    is_scope: bool,
}

async fn api_context_status(State(state): State<Arc<AppState>>) -> Json<ContextStatusResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return ContextStatusResponse::default();
        }
        let branch = ctx::current_branch(workdir);
        let git_branch = ctx::current_git_branch(workdir);
        let git_branch_goal = ctx::git_branch_goal(workdir, &git_branch).unwrap_or_default();
        let branches = ctx::list_branches(workdir);
        let branch_count = branches.len();
        let current_ctx = ctx::gcc_context(
            workdir,
            &ctx::ContextOpts {
                branch: Some(branch.clone()),
                commit_hash: None,
                show_log: false,
                log_offset: 0,
                metadata_segment: None,
                window: 10,
                depth: 2,
            },
        )
        .ok();

        let repo = git2::Repository::discover(workdir).ok();
        let snapshots = repo
            .as_ref()
            .map(list_context_snapshots)
            .unwrap_or_default();
        let latest_snapshot_timestamp = snapshots
            .first()
            .map(|s| s.timestamp.clone())
            .unwrap_or_default();
        let snapshot_count = snapshots.len();
        let snapshot_counts: HashMap<String, usize> = snapshots.iter().fold(HashMap::new(), |mut acc, s| {
            *acc.entry(s.branch.clone()).or_insert(0) += 1;
            acc
        });

        let (goal, commit_count, trace_lines, branch_summaries) = repo
            .as_ref()
            .map(|r| {
                let goal = read_ctx_file(r, "main.md")
                    .map(|t| extract_ctx_section(&t, "Goal"))
                    .unwrap_or_default();
                let current_commit_text =
                    read_ctx_file(r, &format!("branches/{branch}/commit.md")).unwrap_or_default();
                let current_trace_text =
                    read_ctx_file(r, &format!("branches/{branch}/trace.md")).unwrap_or_default();
                let commit_count = current_commit_text.matches("## Commit ").count();
                let trace_lines = count_trace_entries(&current_trace_text);

                let main_commit_text =
                    read_ctx_file(r, &format!("branches/{}/commit.md", ctx::MAIN_BRANCH)).unwrap_or_default();
                let main_trace_text =
                    read_ctx_file(r, &format!("branches/{}/trace.md", ctx::MAIN_BRANCH)).unwrap_or_default();
                let main_commits: HashSet<String> = parse_commit_contributions(&main_commit_text).into_iter().collect();
                let main_trace: HashSet<String> = collect_trace_entries(&main_trace_text).into_iter().collect();

                let mut branch_summaries: Vec<ContextBranchSummary> = branches
                    .iter()
                    .map(|name| {
                        let commit_text =
                            read_ctx_file(r, &format!("branches/{name}/commit.md")).unwrap_or_default();
                        let trace_text =
                            read_ctx_file(r, &format!("branches/{name}/trace.md")).unwrap_or_default();
                        let branch_ctx = ctx::gcc_context(
                            workdir,
                            &ctx::ContextOpts {
                                branch: Some(name.clone()),
                                commit_hash: None,
                                show_log: false,
                                log_offset: 0,
                                metadata_segment: None,
                                window: 10,
                                depth: 2,
                            },
                        )
                        .ok()
                        .unwrap_or_default();
                        let branch_commits = parse_commit_contributions(&commit_text);
                        let branch_trace = collect_trace_entries(&trace_text);
                        let (exclusive_milestones, exclusive_trace_lines) = if name == ctx::MAIN_BRANCH {
                            (0, 0)
                        } else {
                            (
                                branch_commits.iter().filter(|c| !main_commits.contains(*c)).count(),
                                branch_trace.iter().filter(|l| !main_trace.contains(*l)).count(),
                            )
                        };
                        ContextBranchSummary {
                            branch: name.clone(),
                            purpose: extract_branch_purpose(&commit_text, name),
                            last_milestone: branch_commits.last().cloned().unwrap_or_default(),
                            last_activity: extract_last_commit_timestamp(&commit_text),
                            todo_count: branch_ctx.todo_items.len(),
                            trace_lines: count_trace_entries(&trace_text),
                            milestone_count: branch_commits.len(),
                            snapshot_count: snapshot_counts.get(name).copied().unwrap_or(0),
                            exclusive_milestones,
                            exclusive_trace_lines,
                            is_scope: name.starts_with("scope/"),
                        }
                    })
                    .collect();
                branch_summaries.sort_by(|a, b| a.branch.cmp(&b.branch));

                (goal, commit_count, trace_lines, branch_summaries)
            })
            .unwrap_or_default();

        let stale_branch_count = branch_summaries
            .iter()
            .filter(|b| b.branch != branch && b.exclusive_milestones == 0 && b.exclusive_trace_lines == 0)
            .count();

        ContextStatusResponse {
            initialized: true,
            current_branch: branch,
            git_branch,
            git_branch_goal,
            goal,
            branch_count,
            branches,
            commit_count,
            trace_lines,
            snapshot_count,
            stable_line_count: current_ctx.as_ref().map(|c| c.stable_line_count).unwrap_or(0),
            dynamic_line_count: current_ctx.as_ref().map(|c| c.dynamic_line_count).unwrap_or(0),
            todo_count: current_ctx.as_ref().map(|c| c.todo_items.len()).unwrap_or(0),
            latest_snapshot_timestamp,
            stale_branch_count,
            branch_summaries,
        }
    })
    .await;
    Json(result.unwrap_or_default())
}

#[derive(Serialize, Default, Clone)]
struct ContextSnapshotItem {
    sha: String,
    sha_short: String,
    context_oid: String,
    timestamp: String,
    branch: String,
    goal: String,
    recent_milestones: Vec<String>,
}

async fn api_context_snapshots(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ContextSnapshotItem>> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        let repo = git2::Repository::discover(workdir).ok()?;
        Some(list_context_snapshots(&repo))
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(serde::Deserialize, Default)]
struct ContextShowQuery {
    branch: Option<String>,
    window: Option<usize>,
    trace: Option<bool>,
}

async fn api_context_show(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextShowQuery>,
) -> Json<Option<ctx::GccContext>> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return None;
        }
        let opts = ctx::ContextOpts {
            branch: params.branch,
            commit_hash: None,
            show_log: params.trace.unwrap_or(false),
            log_offset: 0,
            metadata_segment: None,
            window: params.window.unwrap_or(10),
            depth: 2,
        };
        ctx::gcc_context(workdir, &opts).ok()
    })
    .await;
    Json(result.unwrap_or(None))
}

#[derive(serde::Deserialize)]
struct ContextDiffQuery {
    from: String,
    to: String,
}

#[derive(Serialize, Default)]
struct ContextDiffResponse {
    from: String,
    to: String,
    from_branch: String,
    to_branch: String,
    cross_branch: bool,
    goal_changed: bool,
    from_goal: String,
    to_goal: String,
    added_milestones: Vec<String>,
    removed_milestones: Vec<String>,
    added_trace_lines: Vec<String>,
    removed_trace_lines: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct ContextMilestonesQuery {
    /// Optional context branch (defaults to the current context branch).
    pub branch: Option<String>,
}

async fn api_context_milestones(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextMilestonesQuery>,
) -> Json<Vec<ContextMilestoneEntry>> {
    let result = tokio::task::spawn_blocking(move || -> Option<Vec<ContextMilestoneEntry>> {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return None;
        }
        let repo = H5iRepository::open(workdir).ok()?;
        let git = repo.git();
        let branch = params
            .branch
            .clone()
            .unwrap_or_else(|| ctx::current_branch(workdir));
        let commit_text = read_ctx_file(git, &format!("branches/{branch}/commit.md"))?;
        Some(parse_commit_milestones(&commit_text))
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

async fn api_context_diff(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextDiffQuery>,
) -> Json<ContextDiffResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        let diff = ctx::context_diff(workdir, &params.from, &params.to).ok()?;
        Some(ContextDiffResponse {
            from: diff.sha1,
            to: diff.sha2,
            from_branch: diff.from_branch.clone(),
            to_branch: diff.to_branch.clone(),
            cross_branch: diff.from_branch != diff.to_branch,
            goal_changed: diff.goal_changed,
            from_goal: diff.from_goal,
            to_goal: diff.to_goal,
            added_milestones: diff.added_commits,
            removed_milestones: diff.removed_commits,
            added_trace_lines: diff.added_trace_lines,
            removed_trace_lines: diff.removed_trace_lines,
        })
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(serde::Deserialize)]
struct ContextRelevantQuery {
    file: String,
}

#[derive(Serialize, Default)]
struct ContextRelevantResponse {
    file: String,
    milestone_mentions: Vec<String>,
    trace_mentions: Vec<String>,
    cross_branch_mentions: Vec<String>,
}

async fn api_context_relevant(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextRelevantQuery>,
) -> Json<ContextRelevantResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return Some(ContextRelevantResponse { file: params.file, ..Default::default() });
        }
        let r = ctx::relevant(workdir, &params.file).ok()?;
        Some(ContextRelevantResponse {
            file: params.file,
            milestone_mentions: r.commit_mentions,
            trace_mentions: r.trace_mentions,
            cross_branch_mentions: r.cross_branch_mentions,
        })
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(serde::Deserialize)]
struct ContextSearchQuery {
    q: String,
    limit: Option<usize>,
}

#[derive(Serialize, Default)]
struct ContextSearchResultResponse {
    file: String,
    score: f64,
    snippets: Vec<String>,
    signal: String,
    cochanged_with: Vec<String>,
}

async fn api_context_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextSearchQuery>,
) -> Json<Vec<ContextSearchResultResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let query = params.q.trim();
        if query.is_empty() {
            return Some(Vec::new());
        }
        let items = ctx::search(&state.repo_path, query, params.limit.unwrap_or(8)).ok()?;
        Some(
            items
                .into_iter()
                .map(|r| ContextSearchResultResponse {
                    file: r.file,
                    score: r.score,
                    snippets: r.snippets,
                    signal: r.signal,
                    cochanged_with: r.cochanged_with,
                })
                .collect(),
        )
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(serde::Deserialize, Default)]
struct ContextDagQuery {
    branch: Option<String>,
}

#[derive(Serialize, Default)]
struct ContextDagResponse {
    branch: String,
    node_count: usize,
    observe_count: usize,
    think_count: usize,
    act_count: usize,
    note_count: usize,
    merge_count: usize,
    nodes: Vec<ctx::TraceNode>,
}

async fn api_context_dag(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextDagQuery>,
) -> Json<ContextDagResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return Some(ContextDagResponse::default());
        }
        let repo = git2::Repository::discover(workdir).ok()?;
        let branch = params.branch.unwrap_or_else(|| ctx::current_branch(workdir));
        let dag_text = read_ctx_file(&repo, &format!("branches/{branch}/dag.json")).unwrap_or_default();
        let dag: ctx::TraceDag = serde_json::from_str(&dag_text).unwrap_or_default();
        let observe_count = dag.nodes.iter().filter(|n| n.kind == "OBSERVE").count();
        let think_count = dag.nodes.iter().filter(|n| n.kind == "THINK").count();
        let act_count = dag.nodes.iter().filter(|n| n.kind == "ACT").count();
        let note_count = dag.nodes.iter().filter(|n| n.kind == "NOTE").count();
        let merge_count = dag.nodes.iter().filter(|n| n.kind == "MERGE").count();
        Some(ContextDagResponse {
            branch,
            node_count: dag.nodes.len(),
            observe_count,
            think_count,
            act_count,
            note_count,
            merge_count,
            nodes: dag.nodes,
        })
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

#[derive(serde::Deserialize, Default)]
struct ContextPromotionQuery {
    branch: Option<String>,
}

#[derive(Serialize, Default)]
struct ContextPromotionResponse {
    branch: String,
    purpose: String,
    ephemeral_count: usize,
    durable_trace_count: usize,
    milestone_count: usize,
    snapshot_count: usize,
    todo_count: usize,
    stable_line_count: usize,
    dynamic_line_count: usize,
    last_snapshot_timestamp: String,
    recent_milestones: Vec<String>,
}

async fn api_context_promotion(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextPromotionQuery>,
) -> Json<ContextPromotionResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let workdir = &state.repo_path;
        if !ctx::is_initialized(workdir) {
            return Some(ContextPromotionResponse::default());
        }
        let branch = params.branch.unwrap_or_else(|| ctx::current_branch(workdir));
        let repo = git2::Repository::discover(workdir).ok()?;
        let commit_text = read_ctx_file(&repo, &format!("branches/{branch}/commit.md")).unwrap_or_default();
        let trace_text = read_ctx_file(&repo, &format!("branches/{branch}/trace.md")).unwrap_or_default();
        let ephemeral_text = read_ctx_file(&repo, &format!("branches/{branch}/ephemeral.md")).unwrap_or_default();
        let snapshots = list_context_snapshots(&repo);
        let last_snapshot_timestamp = snapshots
            .iter()
            .find(|s| s.branch == branch)
            .map(|s| s.timestamp.clone())
            .unwrap_or_default();
        let branch_ctx = ctx::gcc_context(
            workdir,
            &ctx::ContextOpts {
                branch: Some(branch.clone()),
                commit_hash: None,
                show_log: false,
                log_offset: 0,
                metadata_segment: None,
                window: 10,
                depth: 2,
            },
        )
        .ok()
        .unwrap_or_default();
        let milestones = parse_commit_contributions(&commit_text);
        Some(ContextPromotionResponse {
            branch: branch.clone(),
            purpose: extract_branch_purpose(&commit_text, &branch),
            ephemeral_count: count_trace_entries(&ephemeral_text),
            durable_trace_count: count_trace_entries(&trace_text),
            milestone_count: milestones.len(),
            snapshot_count: snapshots.iter().filter(|s| s.branch == branch).count(),
            todo_count: branch_ctx.todo_items.len(),
            stable_line_count: branch_ctx.stable_line_count,
            dynamic_line_count: branch_ctx.dynamic_line_count,
            last_snapshot_timestamp,
            recent_milestones: milestones.into_iter().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect(),
        })
    })
    .await;
    Json(result.unwrap_or(None).unwrap_or_default())
}

// ── Context API helpers ───────────────────────────────────────────────────────

fn read_ctx_file(repo: &git2::Repository, vpath: &str) -> Option<String> {
    // Delegate to ctx::read_ctx_file so legacy `branches/<x>/...` vpaths,
    // `main.md`, `git-goals/...`, and `snapshots/...` route correctly to
    // per-branch refs under refs/h5i/context/<name>.
    let workdir = repo.workdir()?;
    ctx::read_ctx_file(workdir, vpath)
}

fn extract_ctx_section(text: &str, section: &str) -> String {
    let header = format!("## {section}");
    if let Some(start) = text.find(&header) {
        let after = &text[start + header.len()..];
        let end = after.find("\n## ").unwrap_or(after.len());
        return after[..end].trim().to_string();
    }
    String::new()
}

fn list_context_snapshots(repo: &git2::Repository) -> Vec<ContextSnapshotItem> {
    // Snapshots live on refs/h5i/context/main (the main branch ref).
    let main_ref = ctx::branch_ref(ctx::MAIN_BRANCH);
    let tree = repo
        .find_reference(&main_ref)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());
    let tree = match tree {
        Some(t) => t,
        None => return vec![],
    };
    let snap_entry = match tree
        .get_name("snapshots")
        .filter(|e| e.kind() == Some(git2::ObjectType::Tree))
    {
        Some(e) => e,
        None => return vec![],
    };
    let snap_tree = match repo.find_tree(snap_entry.id()) {
        Ok(t) => t,
        Err(_) => return vec![],
    };

    let mut items = Vec::new();
    for entry in snap_tree.iter() {
        if entry.kind() != Some(git2::ObjectType::Blob) {
            continue;
        }
        let Ok(blob) = repo.find_blob(entry.id()) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(blob.content()) else {
            continue;
        };
        let mut item = ContextSnapshotItem::default();
        let mut milestones = Vec::new();
        let mut in_milestones = false;
        for line in text.lines() {
            if line.starts_with("**Linked commit:**") {
                item.sha = line.split("**Linked commit:**").nth(1).unwrap_or("").trim().to_string();
                item.sha_short = item.sha.chars().take(8).collect();
            } else if line.starts_with("**Context ref OID:**") {
                item.context_oid = line.split("**Context ref OID:**").nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("**Timestamp:**") {
                item.timestamp = line.split("**Timestamp:**").nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("**Branch:**") {
                item.branch = line.split("**Branch:**").nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("**Goal:**") {
                item.goal = line.split("**Goal:**").nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("## Recent Context Commits") {
                in_milestones = true;
            } else if in_milestones && line.starts_with("- ") {
                milestones.push(line[2..].trim().to_string());
            }
        }
        item.recent_milestones = milestones;
        if !item.sha.is_empty() {
            items.push(item);
        }
    }
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    items
}

fn parse_commit_contributions(commit_text: &str) -> Vec<String> {
    parse_commit_milestones(commit_text)
        .into_iter()
        .map(|m| m.contribution)
        .collect()
}

/// One entry in `commit.md`: header `## Commit <sha> — <ts>` plus the
/// `### This Commit's Contribution` body, parsed into structured form so the
/// UI can show the git SHA + timestamp next to each milestone instead of the
/// raw text alone.
#[derive(Serialize, Default)]
pub struct ContextMilestoneEntry {
    /// Short SHA as it appears in the commit.md header (typically 7-8 chars).
    pub sha_short: String,
    /// Timestamp string from the same header (everything after the em-dash).
    pub timestamp: String,
    /// The contribution body text.
    pub contribution: String,
}

fn parse_commit_milestones(commit_text: &str) -> Vec<ContextMilestoneEntry> {
    let mut entries = Vec::new();
    for entry in commit_text.split("## Commit ").skip(1) {
        // Header line: "<sha> — <ts>\n..." (or "<sha> — <ts> [MERGE: ...]\n...").
        let header_end = entry.find('\n').unwrap_or(entry.len());
        let header = &entry[..header_end];
        let mut header_iter = header.splitn(2, " — ");
        let sha_short = header_iter.next().unwrap_or("").trim().to_string();
        let timestamp = header_iter
            .next()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        if let Some(start) = entry.find("### This Commit's Contribution") {
            let after = &entry[start + "### This Commit's Contribution".len()..];
            let end = after.find("\n---").unwrap_or(after.len());
            let contribution = after[..end].trim().to_string();
            if !contribution.is_empty() {
                entries.push(ContextMilestoneEntry {
                    sha_short,
                    timestamp,
                    contribution,
                });
            }
        }
    }
    entries
}

fn extract_branch_purpose(commit_text: &str, _branch: &str) -> String {
    commit_text
        .lines()
        .find(|line| line.starts_with("**Purpose:**"))
        .map(|line| line.split("**Purpose:**").nth(1).unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

fn extract_last_commit_timestamp(commit_text: &str) -> String {
    commit_text
        .lines()
        .rev()
        .find(|line| line.starts_with("## Commit ") && line.contains(" — "))
        .and_then(|line| line.split(" — ").nth(1))
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn collect_trace_entries(trace_text: &str) -> Vec<String> {
    trace_text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("---")
                && !trimmed.starts_with("_[Checkpoint")
        })
        .map(str::to_string)
        .collect()
}

fn count_trace_entries(trace_text: &str) -> usize {
    collect_trace_entries(trace_text).len()
}

// ── Sandbox dashboard (read-only env monitoring) ──────────────────────────────
//
// These endpoints power the workbench's "Sandbox" mode (the flight recorder):
// the env fleet, per-env five-lane timelines, capture inspection, and host
// isolation readiness. All read-only — propose/apply/abort stay in the CLI/MCP
// (no mutating HTTP surface without a CSRF story; monitoring is safer and
// sufficient). Risk findings come from the deterministic `risk` classifier.

/// One row in the env fleet table.
#[derive(Serialize)]
pub struct EnvFleetItem {
    pub id: String,
    pub agent: String,
    pub slug: String,
    pub status: String,
    pub isolation: String,
    pub profile: String,
    pub backend: String,
    pub policy_digest: String,
    pub parent_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub captures: usize,
    /// Whether the workspace is materialized here (false = pulled/gc'd).
    pub has_workspace: bool,
    /// Base-vs-parent drift: "up-to-date" | "parent-ahead" | "diverged" | "parent-gone".
    pub drift: String,
    pub drift_summary: String,
    /// Most recent event (type + detail), for the "last activity" column.
    pub last_event: Option<EnvEventView>,
    /// Boundary-pressure roll-up from the deterministic classifier.
    pub risk: crate::risk::EnvRisk,
}

#[derive(Serialize)]
pub struct EnvEventView {
    pub ts: String,
    pub event: String,
    pub detail: Option<String>,
    pub capture: Option<String>,
}

impl From<&crate::env::EnvEvent> for EnvEventView {
    fn from(e: &crate::env::EnvEvent) -> Self {
        EnvEventView {
            ts: e.ts.clone(),
            event: e.event.clone(),
            detail: e.detail.clone(),
            capture: e.capture.clone(),
        }
    }
}

/// Map a `Drift` to a stable kind string for the UI.
fn drift_kind(d: &crate::env::Drift) -> &'static str {
    use crate::env::Drift;
    match d {
        Drift::UpToDate => "up-to-date",
        Drift::ParentAhead { .. } => "parent-ahead",
        Drift::Diverged { .. } => "diverged",
        Drift::ParentGone => "parent-gone",
    }
}

/// Resolve the capture manifests referenced by a manifest (best-effort; missing
/// captures are simply skipped). Newest last, matching `m.captures` order.
fn resolve_env_captures(
    git: &git2::Repository,
    m: &crate::env::EnvManifest,
) -> Vec<crate::objects::Manifest> {
    m.captures
        .iter()
        .filter_map(|id| crate::objects::resolve_manifest(git, id).ok())
        .collect()
}

/// Build the fleet item for one env (shared by list + detail).
fn build_fleet_item(
    git: &git2::Repository,
    h5i_root: &std::path::Path,
    m: &crate::env::EnvManifest,
    events: &[crate::env::EnvEvent],
    captures: &[crate::objects::Manifest],
) -> EnvFleetItem {
    let drift = crate::env::drift(git, m);
    let policy = crate::env::load_policy(h5i_root, m).ok().map(|rp| rp.profile);
    let risk = crate::risk::classify_env(m, policy.as_ref(), events, captures);
    EnvFleetItem {
        id: m.id.clone(),
        agent: m.agent.clone(),
        slug: m.slug.clone(),
        status: m.status.clone(),
        isolation: m.isolation_claim.clone(),
        profile: m.profile.clone(),
        backend: m.backend.clone(),
        policy_digest: m.policy_digest.clone(),
        parent_branch: m.parent_branch.clone(),
        created_at: m.created_at.clone(),
        updated_at: m.updated_at.clone(),
        captures: m.captures.len(),
        has_workspace: crate::env::has_workspace(m, h5i_root),
        drift: drift_kind(&drift).to_string(),
        drift_summary: drift.summary(),
        last_event: events.last().map(EnvEventView::from),
        risk,
    }
}

/// GET /api/envs — the env fleet, each enriched with drift + risk roll-up.
async fn api_envs(State(state): State<Arc<AppState>>) -> Json<Vec<EnvFleetItem>> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<EnvFleetItem>> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let h5i_root = &repo.h5i_root;
        let mut items: Vec<EnvFleetItem> = crate::env::list(h5i_root)
            .iter()
            .map(|m| {
                let events = crate::env::read_events(git, Some(&m.id));
                let captures = resolve_env_captures(git, m);
                build_fleet_item(git, h5i_root, m, &events, &captures)
            })
            .collect();
        // Most pressing first (highest risk), then most recently updated.
        items.sort_by(|a, b| {
            b.risk
                .score
                .cmp(&a.risk.score)
                .then(b.updated_at.cmp(&a.updated_at))
        });
        Ok(items)
    })
    .await;
    Json(result.ok().and_then(|r| r.ok()).unwrap_or_default())
}

/// Full per-env detail: manifest + enforced policy + events + capture summaries
/// + diffstat. The five-lane timeline is assembled client-side from these.
#[derive(Serialize)]
pub struct EnvDetail {
    pub item: EnvFleetItem,
    /// The enforced (resolved) policy, the dashboard's "what was actually
    /// allowed" panel. `None` when policy.resolved.toml is unreadable.
    pub policy: Option<EnforcedPolicy>,
    pub events: Vec<EnvEventView>,
    pub captures: Vec<EnvCaptureView>,
    /// `base..branch-tip` diffstat (the proposed state).
    pub diffstat: Option<String>,
}

/// The enforced policy, flattened for the inspector's allowance lanes.
#[derive(Serialize)]
pub struct EnforcedPolicy {
    pub isolation: String,
    pub net_mode: String,
    pub net_egress: Vec<String>,
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub fs_deny: Vec<String>,
    pub tools: Vec<String>,
    pub env_pass: Vec<String>,
    pub image: Option<String>,
    pub mem_bytes: Option<u64>,
    pub max_procs: Option<u64>,
    pub wall_secs: u64,
    pub cpu_secs: Option<u64>,
    pub fsize_bytes: Option<u64>,
}

impl From<&crate::sandbox::Profile> for EnforcedPolicy {
    fn from(p: &crate::sandbox::Profile) -> Self {
        EnforcedPolicy {
            isolation: p.isolation.as_str().to_string(),
            net_mode: match p.net_mode {
                crate::sandbox::NetMode::Deny => "deny".into(),
                crate::sandbox::NetMode::Host => "host".into(),
            },
            net_egress: p.net_egress.clone(),
            fs_read: p.fs_read.clone(),
            fs_write: p.fs_write.clone(),
            fs_deny: p.fs_deny.clone(),
            tools: p.tools.clone(),
            env_pass: p.env_pass.clone(),
            image: p.image.clone(),
            mem_bytes: p.mem_bytes,
            max_procs: p.max_procs,
            wall_secs: p.wall_secs,
            cpu_secs: p.cpu_secs,
            fsize_bytes: p.fsize_bytes,
        }
    }
}

/// A capture summary for the timeline (no raw rehydration).
#[derive(Serialize)]
pub struct EnvCaptureView {
    pub id: String,
    pub cmd: Option<String>,
    pub exit_code: Option<i32>,
    pub timestamp: String,
    pub summary: String,
    pub egress: Option<crate::objects::EgressSummary>,
    pub redactions: Vec<String>,
}

impl From<&crate::objects::Manifest> for EnvCaptureView {
    fn from(m: &crate::objects::Manifest) -> Self {
        EnvCaptureView {
            id: m.id.clone(),
            cmd: m.cmd.clone(),
            exit_code: m.exit_code,
            timestamp: m.timestamp.clone(),
            summary: m.summary.clone(),
            egress: m.egress.clone(),
            redactions: m.redactions.clone(),
        }
    }
}

/// GET /api/env/:agent/:slug — full detail for one environment.
async fn api_env_detail(
    State(state): State<Arc<AppState>>,
    Path((agent, slug)): Path<(String, String)>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<EnvDetail>> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let h5i_root = &repo.h5i_root;
        let id = format!("env/{agent}/{slug}");
        let Some(m) = crate::env::list(h5i_root).into_iter().find(|m| m.id == id) else {
            return Ok(None);
        };
        let events = crate::env::read_events(git, Some(&m.id));
        let captures = resolve_env_captures(git, &m);
        let policy = crate::env::load_policy(h5i_root, &m).ok().map(|rp| rp.profile);
        let item = build_fleet_item(git, h5i_root, &m, &events, &captures);
        let diffstat = crate::env::diff(git, h5i_root, &m, true).ok();
        Ok(Some(EnvDetail {
            item,
            policy: policy.as_ref().map(EnforcedPolicy::from),
            events: events.iter().map(EnvEventView::from).collect(),
            captures: captures.iter().map(EnvCaptureView::from).collect(),
            diffstat,
        }))
    })
    .await;
    match result.ok().and_then(|r| r.ok()).flatten() {
        Some(detail) => Json(detail).into_response(),
        None => (StatusCode::NOT_FOUND, "environment not found").into_response(),
    }
}

/// GET /api/env/:agent/:slug/captures/:id — the rendered/structured capture
/// (reuses `env::inspect`, which enforces the capture belongs to this env).
async fn api_env_capture(
    State(state): State<Arc<AppState>>,
    Path((agent, slug, cap_id)): Path<(String, String, String)>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let id = format!("env/{agent}/{slug}");
        let Some(m) = crate::env::list(&repo.h5i_root).into_iter().find(|m| m.id == id) else {
            return Ok(None);
        };
        Ok(crate::env::inspect(git, &m, &cap_id).ok())
    })
    .await;
    match result.ok().and_then(|r| r.ok()).flatten() {
        Some(text) => Json(serde_json::json!({ "render": text })).into_response(),
        None => (StatusCode::NOT_FOUND, "capture not found for this env").into_response(),
    }
}

// ── Replay (the flight recorder) ───────────────────────────────────────────────
//
// The roadmap's centerpiece: "Review the run, not just the diff." A Replay is a
// single chronological timeline of what an agent did inside a workspace before
// producing a diff — prompt → reads → commands → blocked accesses → tests →
// edits → diff — plus a per-file "workspace heatmap" (read/edited/tested/
// blocked/risky) and the evidence behind each event. Two anchors:
//   • env run    — the cleanest "what the agent could and couldn't reach" story,
//                  assembled from captures + egress + enforced policy + risk.
//   • commit     — the fallback for ordinary history, assembled from the commit
//                  record (prompt, tests) + the analyzed Claude Code session.
// Both produce the same shape so the frontend renders one view.

/// One event on the replay timeline. `kind` drives the icon/colour; `lane`
/// groups it (intent / fs / net / proc / test / provenance / lifecycle / msg).
#[derive(Serialize)]
pub struct ReplayEvent {
    pub seq: usize,
    pub ts: String,
    /// PROMPT | THINK | READ | RUN | TEST_PASS | TEST_FAIL | BLOCKED | EDIT |
    /// NOTE | DIFF | CREATE | PROPOSE | APPLY | ABORT | MSG | EVENT
    pub kind: String,
    pub lane: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// info | good | warning | critical — the loud one is `critical` (blocked).
    pub severity: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// One file's status across the run — the center-pane heatmap cell.
#[derive(Serialize, Clone)]
pub struct FileHeat {
    pub path: String,
    pub read: bool,
    pub edited: bool,
    pub tested: bool,
    pub blocked: bool,
    /// Edited without first reading it — "changed without enough context".
    pub risky: bool,
}

/// Run-level summary for the replay header / trust strip.
#[derive(Serialize)]
pub struct ReplayHeader {
    /// "env" | "commit"
    pub anchor: String,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    pub blocked_count: u64,
    pub allowed_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
    pub risk_score: u32,
    pub risk_level: String,
    pub run_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diffstat: Option<String>,
}

#[derive(Serialize)]
pub struct ReplayView {
    pub header: ReplayHeader,
    pub timeline: Vec<ReplayEvent>,
    pub heatmap: Vec<FileHeat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<EnforcedPolicy>,
    pub findings: Vec<crate::risk::Finding>,
}

/// Get-or-create a heatmap cell.
fn heat_cell<'a>(
    heat: &'a mut std::collections::BTreeMap<String, FileHeat>,
    path: &str,
) -> &'a mut FileHeat {
    heat.entry(path.to_string()).or_insert_with(|| FileHeat {
        path: path.to_string(),
        read: false,
        edited: false,
        tested: false,
        blocked: false,
        risky: false,
    })
}

/// Test pass/fail tallies from a capture's structured result, when it is a test
/// run. `None` for non-test captures.
fn capture_test_counts(cap: &crate::objects::Manifest) -> Option<(u64, u64)> {
    let s = cap.structured.as_ref()?;
    let is_test = matches!(s.kind, crate::structured::ResultKind::Test)
        || s.counts.contains_key("passed")
        || s.counts.contains_key("failed");
    if !is_test {
        return None;
    }
    let passed = s.counts.get("passed").copied().unwrap_or(0);
    let failed = s.counts.get("failed").copied().unwrap_or(0);
    Some((passed, failed))
}

/// Assemble a replay from an environment's runs (the primary anchor).
#[allow(clippy::too_many_arguments)]
fn build_env_replay(
    git: &git2::Repository,
    h5i_root: &std::path::Path,
    m: &crate::env::EnvManifest,
    events: &[crate::env::EnvEvent],
    captures: &[crate::objects::Manifest],
    policy: Option<&crate::sandbox::Profile>,
    risk: &crate::risk::EnvRisk,
) -> ReplayView {
    use std::collections::BTreeMap;
    let mut timeline: Vec<ReplayEvent> = Vec::new();
    let mut heat: BTreeMap<String, FileHeat> = BTreeMap::new();
    let mut seq = 0usize;
    let mut push = |timeline: &mut Vec<ReplayEvent>, e: ReplayEvent| {
        let mut e = e;
        e.seq = seq;
        seq += 1;
        timeline.push(e);
    };

    let cap_by_id: HashMap<&str, &crate::objects::Manifest> =
        captures.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut blocked_count: u64 = 0;
    let mut allowed_count: u64 = 0;
    let mut tests_passed: u64 = 0;
    let mut tests_failed: u64 = 0;
    let mut had_tests = false;
    let mut run_count = 0usize;

    for ev in events {
        match ev.event.as_str() {
            "created" => push(
                &mut timeline,
                ReplayEvent {
                    seq: 0,
                    ts: ev.ts.clone(),
                    kind: "CREATE".into(),
                    lane: "lifecycle".into(),
                    title: format!("Environment created · {}", m.isolation_claim),
                    detail: ev.detail.clone(),
                    severity: "info".into(),
                    files: vec![],
                    capture_id: None,
                    exit_code: None,
                },
            ),
            "exec" => {
                run_count += 1;
                let cap = ev.capture.as_deref().and_then(|id| cap_by_id.get(id).copied());
                let cmd = cap
                    .and_then(|c| c.cmd.clone())
                    .or_else(|| {
                        ev.detail
                            .as_ref()
                            .and_then(|d| d.split("cmd=`").nth(1))
                            .and_then(|s| s.split('`').next())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| "command".into());
                let exit = cap.and_then(|c| c.exit_code);
                push(
                    &mut timeline,
                    ReplayEvent {
                        seq: 0,
                        ts: ev.ts.clone(),
                        kind: "RUN".into(),
                        lane: "proc".into(),
                        title: cmd.clone(),
                        detail: cap.map(|c| c.summary.clone()),
                        severity: if exit.unwrap_or(0) != 0 { "warning" } else { "info" }.into(),
                        files: vec![],
                        capture_id: ev.capture.clone(),
                        exit_code: exit,
                    },
                );
                if let Some(cap) = cap {
                    // reads (files mentioned, not edited)
                    let edited: HashSet<&str> = cap.diff_files.iter().map(|s| s.as_str()).collect();
                    let reads: Vec<String> = cap
                        .files
                        .iter()
                        .filter(|f| !edited.contains(f.as_str()))
                        .cloned()
                        .collect();
                    for f in &reads {
                        heat_cell(&mut heat, f).read = true;
                    }
                    if !reads.is_empty() {
                        push(
                            &mut timeline,
                            ReplayEvent {
                                seq: 0,
                                ts: ev.ts.clone(),
                                kind: "READ".into(),
                                lane: "fs".into(),
                                title: format!("touched {} file(s)", reads.len()),
                                detail: Some(reads.join("\n")),
                                severity: "info".into(),
                                files: reads.clone(),
                                capture_id: ev.capture.clone(),
                                exit_code: None,
                            },
                        );
                    }
                    // edits (working-tree diff at capture time)
                    if !cap.diff_files.is_empty() {
                        for f in &cap.diff_files {
                            let c = heat_cell(&mut heat, f);
                            c.edited = true;
                        }
                        push(
                            &mut timeline,
                            ReplayEvent {
                                seq: 0,
                                ts: ev.ts.clone(),
                                kind: "EDIT".into(),
                                lane: "fs".into(),
                                title: format!("changed {} file(s)", cap.diff_files.len()),
                                detail: Some(cap.diff_files.join("\n")),
                                severity: "info".into(),
                                files: cap.diff_files.clone(),
                                capture_id: ev.capture.clone(),
                                exit_code: None,
                            },
                        );
                    }
                    // tests
                    if let Some((p, f)) = capture_test_counts(cap) {
                        had_tests = true;
                        tests_passed += p;
                        tests_failed += f;
                        for path in &cap.files {
                            heat_cell(&mut heat, path).tested = true;
                        }
                        let failed = f > 0;
                        push(
                            &mut timeline,
                            ReplayEvent {
                                seq: 0,
                                ts: ev.ts.clone(),
                                kind: if failed { "TEST_FAIL" } else { "TEST_PASS" }.into(),
                                lane: "test".into(),
                                title: if failed {
                                    format!("{f} failed · {p} passed")
                                } else {
                                    format!("{p} passed")
                                },
                                detail: cap.structured.as_ref().and_then(|s| s.body.clone()),
                                severity: if failed { "critical" } else { "good" }.into(),
                                files: vec![],
                                capture_id: ev.capture.clone(),
                                exit_code: cap.exit_code,
                            },
                        );
                    }
                    // egress — the hero signal: blocked accesses
                    if let Some(eg) = &cap.egress {
                        allowed_count += eg.allowed;
                        blocked_count += eg.denied;
                        for h in &eg.hosts {
                            if h.denied > 0 {
                                push(
                                    &mut timeline,
                                    ReplayEvent {
                                        seq: 0,
                                        ts: ev.ts.clone(),
                                        kind: "BLOCKED".into(),
                                        lane: "net".into(),
                                        title: format!("blocked egress → {}:{}", h.host, h.port),
                                        detail: Some(format!(
                                            "{} request(s) refused by the egress allowlist (off-policy host)",
                                            h.denied
                                        )),
                                        severity: "critical".into(),
                                        files: vec![],
                                        capture_id: ev.capture.clone(),
                                        exit_code: None,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            "violation" => {
                blocked_count += 1;
                push(
                    &mut timeline,
                    ReplayEvent {
                        seq: 0,
                        ts: ev.ts.clone(),
                        kind: "BLOCKED".into(),
                        lane: "provenance".into(),
                        title: "mediated commit refused".into(),
                        detail: ev.detail.clone(),
                        severity: "critical".into(),
                        files: vec![],
                        capture_id: ev.capture.clone(),
                        exit_code: None,
                    },
                );
            }
            "proposed" | "applied" | "aborted" | "gc" | "status" => {
                let kind = match ev.event.as_str() {
                    "proposed" => "PROPOSE",
                    "applied" => "APPLY",
                    "aborted" => "ABORT",
                    _ => "EVENT",
                };
                push(
                    &mut timeline,
                    ReplayEvent {
                        seq: 0,
                        ts: ev.ts.clone(),
                        kind: kind.into(),
                        lane: "lifecycle".into(),
                        title: ev.event.clone(),
                        detail: ev.detail.clone(),
                        severity: "info".into(),
                        files: vec![],
                        capture_id: ev.capture.clone(),
                        exit_code: None,
                    },
                );
            }
            _ => {}
        }
    }

    // diffstat (the proposed state) as a closing DIFF event.
    let diffstat = crate::env::diff(git, h5i_root, m, true).ok().filter(|s| !s.trim().is_empty());
    if let Some(ds) = &diffstat {
        let first = ds.lines().last().unwrap_or("").trim().to_string();
        push(
            &mut timeline,
            ReplayEvent {
                seq: 0,
                ts: m.updated_at.clone(),
                kind: "DIFF".into(),
                lane: "fs".into(),
                title: if first.is_empty() { "proposed diff".into() } else { first },
                detail: Some(ds.clone()),
                severity: "info".into(),
                files: vec![],
                capture_id: None,
                exit_code: None,
            },
        );
    }

    // mark blocked/risky heat from risk findings + read-before-edit.
    for f in &risk.findings {
        if matches!(f.lane, crate::risk::Lane::Fs) && f.severity == crate::risk::Severity::Critical {
            for c in heat.values_mut() {
                if f.evidence.contains(&c.path) {
                    c.blocked = true;
                }
            }
        }
    }
    for c in heat.values_mut() {
        if c.edited && !c.read {
            c.risky = true;
        }
    }

    let mut heatmap: Vec<FileHeat> = heat.into_values().collect();
    heatmap.sort_by(|a, b| score_heat(b).cmp(&score_heat(a)).then(a.path.cmp(&b.path)));

    ReplayView {
        header: ReplayHeader {
            anchor: "env".into(),
            id: m.id.clone(),
            title: m.slug.clone(),
            subtitle: Some(format!("{} · {}", m.agent, m.status)),
            agent: Some(m.agent.clone()),
            model: None,
            isolation: Some(m.isolation_claim.clone()),
            prompt: None,
            policy_digest: Some(m.policy_digest.clone()),
            blocked_count,
            allowed_count,
            tests_passed: had_tests.then_some(tests_passed),
            tests_failed: had_tests.then_some(tests_failed),
            risk_score: risk.score,
            risk_level: format!("{:?}", risk.level).to_lowercase(),
            run_count,
            created_at: Some(m.created_at.clone()),
            diffstat,
        },
        timeline,
        heatmap,
        policy: policy.map(EnforcedPolicy::from),
        findings: risk.findings.clone(),
    }
}

/// Sort key so the loudest files float up: blocked > risky > edited > tested > read.
fn score_heat(h: &FileHeat) -> u8 {
    (h.blocked as u8) << 4
        | (h.risky as u8) << 3
        | (h.edited as u8) << 2
        | (h.tested as u8) << 1
        | (h.read as u8)
}

/// Paths changed by a commit (vs its first parent / the empty tree for a root).
fn commit_changed_files(git: &git2::Repository, commit: &git2::Commit) -> Vec<String> {
    let Ok(tree) = commit.tree() else {
        return vec![];
    };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let Ok(diff) = git.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return vec![];
    };
    let mut files: Vec<String> = Vec::new();
    let _ = diff.foreach(
        &mut |delta, _| {
            if files.len() >= 200 {
                return false;
            }
            if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                if let Some(s) = p.to_str() {
                    files.push(s.to_string());
                }
            }
            true
        },
        None,
        None,
        None,
    );
    files.sort();
    files.dedup();
    files
}

/// Assemble a replay from a commit + its analyzed session (the fallback anchor).
fn build_commit_replay(
    repo: &H5iRepository,
    oid: git2::Oid,
) -> anyhow::Result<ReplayView> {
    use std::collections::BTreeMap;
    let git = repo.git();
    let commit = git.find_commit(oid)?;
    let message = commit.message().unwrap_or("").trim().to_string();
    let subject = message.lines().next().unwrap_or("").to_string();
    let author = commit.author().name().unwrap_or("Unknown").to_string();
    let ts = repo
        .load_h5i_record(oid)
        .ok()
        .map(|r| r.timestamp.to_rfc3339())
        .unwrap_or_default();
    let record = repo.load_h5i_record(oid).ok();
    let analysis = session_log::load_analysis(&repo.h5i_root, &oid.to_string())
        .ok()
        .flatten();

    let mut timeline: Vec<ReplayEvent> = Vec::new();
    let mut heat: BTreeMap<String, FileHeat> = BTreeMap::new();
    let mut seq = 0usize;
    let mut emit = |timeline: &mut Vec<ReplayEvent>, kind: &str, lane: &str, title: String, detail: Option<String>, severity: &str, files: Vec<String>| {
        timeline.push(ReplayEvent {
            seq,
            ts: ts.clone(),
            kind: kind.into(),
            lane: lane.into(),
            title,
            detail,
            severity: severity.into(),
            files,
            capture_id: None,
            exit_code: None,
        });
        seq += 1;
    };

    // PROMPT
    let prompt = record
        .as_ref()
        .and_then(|r| r.ai_metadata.as_ref())
        .map(|ai| ai.prompt.clone())
        .filter(|p| !p.is_empty())
        .or_else(|| analysis.as_ref().map(|a| a.causal_chain.user_trigger.clone()).filter(|s| !s.is_empty()));
    if let Some(p) = &prompt {
        emit(&mut timeline, "PROMPT", "intent", "Prompt".into(), Some(p.clone()), "info", vec![]);
    }

    if let Some(a) = &analysis {
        for d in a.causal_chain.key_decisions.iter().take(8) {
            emit(&mut timeline, "THINK", "intent", d.clone(), None, "info", vec![]);
        }
        let reads: Vec<String> = a.footprint.consulted.iter().map(|c| c.path.clone()).collect();
        for r in &reads {
            heat_cell(&mut heat, r).read = true;
        }
        if !reads.is_empty() {
            emit(&mut timeline, "READ", "fs", format!("consulted {} file(s)", reads.len()), Some(reads.join("\n")), "info", reads.clone());
        }
        for cmd in a.footprint.bash_commands.iter().take(20) {
            emit(&mut timeline, "RUN", "proc", cmd.clone(), None, "info", vec![]);
        }
        let mut edits = a.causal_chain.edit_sequence.clone();
        edits.sort_by_key(|e| e.turn);
        for e in &edits {
            heat_cell(&mut heat, &e.file).edited = true;
            emit(&mut timeline, "EDIT", "fs", format!("{} {}", e.operation, e.file), None, "info", vec![e.file.clone()]);
        }
        for u in a.uncertainty.iter().take(6) {
            emit(&mut timeline, "NOTE", "intent", format!("uncertainty in {}", u.context_file), Some(u.snippet.clone()), "warning", vec![u.context_file.clone()]);
        }
        for o in a.omissions.iter().take(6) {
            emit(&mut timeline, "NOTE", "intent", format!("{:?} near {}", o.kind, o.context_file), Some(o.snippet.clone()), "warning", vec![o.context_file.clone()]);
        }
        // coverage heat
        for cov in &a.coverage {
            let c = heat_cell(&mut heat, &cov.file);
            if !cov.read_ranges.is_empty() {
                c.read = true;
            }
            if !cov.edit_turns.is_empty() {
                c.edited = true;
            }
            if cov.blind_edit_count > 0 {
                c.risky = true;
            }
        }
    }

    // tests
    if let Some(tm) = record.as_ref().and_then(|r| r.test_metrics.as_ref()) {
        let failed = tm.failed > 0;
        emit(
            &mut timeline,
            if failed { "TEST_FAIL" } else { "TEST_PASS" },
            "test",
            if failed { format!("{} failed · {} passed", tm.failed, tm.passed) } else { format!("{} passed", tm.passed) },
            tm.summary.clone(),
            if failed { "critical" } else { "good" },
            vec![],
        );
    }

    // diff (changed files in this commit)
    let changed = commit_changed_files(git, &commit);
    for f in &changed {
        heat_cell(&mut heat, f).edited = true;
    }
    if !changed.is_empty() {
        emit(&mut timeline, "DIFF", "fs", format!("{} file(s) changed", changed.len()), Some(changed.join("\n")), "info", changed.clone());
    }

    for c in heat.values_mut() {
        if c.edited && !c.read && !c.risky {
            c.risky = true;
        }
    }
    let mut heatmap: Vec<FileHeat> = heat.into_values().collect();
    heatmap.sort_by(|a, b| score_heat(b).cmp(&score_heat(a)).then(a.path.cmp(&b.path)));

    let (agent, model) = record
        .as_ref()
        .and_then(|r| r.ai_metadata.as_ref())
        .map(|ai| (Some(ai.agent_id.clone()).filter(|s| !s.is_empty()), Some(ai.model_name.clone()).filter(|s| !s.is_empty())))
        .unwrap_or((None, None));
    let (tp, tf) = record
        .as_ref()
        .and_then(|r| r.test_metrics.as_ref())
        .map(|tm| (Some(tm.passed), Some(tm.failed)))
        .unwrap_or((None, None));
    let prov = record.as_ref().and_then(|r| r.env_provenance.as_ref());

    Ok(ReplayView {
        header: ReplayHeader {
            anchor: "commit".into(),
            id: oid.to_string(),
            title: subject,
            subtitle: Some(author),
            agent,
            model,
            isolation: prov.map(|p| p.isolation_claim.clone()),
            prompt,
            policy_digest: prov.map(|p| p.policy_digest.clone()),
            blocked_count: 0,
            allowed_count: 0,
            tests_passed: tp,
            tests_failed: tf,
            risk_score: 0,
            risk_level: "info".into(),
            run_count: analysis.as_ref().map(|a| a.footprint.bash_commands.len()).unwrap_or(0),
            created_at: Some(ts),
            diffstat: None,
        },
        timeline,
        heatmap,
        policy: None,
        findings: vec![],
    })
}

/// GET /api/env/:agent/:slug/replay — the env-anchored flight recorder.
async fn api_env_replay(
    State(state): State<Arc<AppState>>,
    Path((agent, slug)): Path<(String, String)>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ReplayView>> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();
        let h5i_root = &repo.h5i_root;
        let id = format!("env/{agent}/{slug}");
        let Some(m) = crate::env::list(h5i_root).into_iter().find(|m| m.id == id) else {
            return Ok(None);
        };
        let events = crate::env::read_events(git, Some(&m.id));
        let captures = resolve_env_captures(git, &m);
        let policy = crate::env::load_policy(h5i_root, &m).ok().map(|rp| rp.profile);
        let risk = crate::risk::classify_env(&m, policy.as_ref(), &events, &captures);
        Ok(Some(build_env_replay(
            git,
            h5i_root,
            &m,
            &events,
            &captures,
            policy.as_ref(),
            &risk,
        )))
    })
    .await;
    match result.ok().and_then(|r| r.ok()).flatten() {
        Some(v) => Json(v).into_response(),
        None => (StatusCode::NOT_FOUND, "environment not found").into_response(),
    }
}

/// GET /api/commit/:oid/replay — the commit-anchored fallback replay.
async fn api_commit_replay(
    State(state): State<Arc<AppState>>,
    Path(oid): Path<String>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<ReplayView> {
        let repo = H5iRepository::open(&path)?;
        let oid = git2::Oid::from_str(&oid)?;
        build_commit_replay(&repo, oid)
    })
    .await;
    match result.ok().and_then(|r| r.ok()) {
        Some(v) => Json(v).into_response(),
        None => (StatusCode::NOT_FOUND, "commit not found").into_response(),
    }
}

// ── Reviewer cockpit + prompt coach + agent radio ──────────────────────────────

/// A file the reviewer should look at first, with the reason it surfaced.
#[derive(Serialize)]
pub struct CockpitFile {
    pub path: String,
    pub reason: String,
    pub severity: String,
}

/// The compact "should I trust this PR?" card (roadmap §4).
#[derive(Serialize)]
pub struct ReviewerCockpit {
    pub oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub timestamp: String,
    /// 0..=100, higher = safer to merge.
    pub merge_confidence: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_maturity: Option<f64>,
    pub provenance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    pub net_blocked: u64,
    pub net_allowed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
    pub integrity_level: String,
    pub integrity_score: f64,
    pub risk: String,
    pub review_first: Vec<CockpitFile>,
    pub review_score: f32,
}

#[derive(Deserialize)]
struct OidQuery {
    oid: String,
}

/// GET /api/cockpit?oid=… — the reviewer cockpit card for one commit.
async fn api_cockpit(
    State(state): State<Arc<AppState>>,
    Query(q): Query<OidQuery>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<ReviewerCockpit> {
        let repo = H5iRepository::open(&path)?;
        let oid = git2::Oid::from_str(&q.oid)?;
        let commit = repo.git().find_commit(oid)?;
        let message = commit.message().unwrap_or("").trim().lines().next().unwrap_or("").to_string();
        let author = commit.author().name().unwrap_or("Unknown").to_string();
        let record = repo.load_h5i_record(oid).ok();
        let integrity = repo.verify_commit_integrity(oid).unwrap_or_else(|_| fallback_report());
        let analysis = session_log::load_analysis(&repo.h5i_root, &oid.to_string()).ok().flatten();

        let prompt_score = record
            .as_ref()
            .and_then(|r| r.ai_metadata.as_ref())
            .map(|ai| crate::prompt_score::score_prompt(&ai.prompt))
            .filter(|s| s.words > 0);

        let model = record.as_ref().and_then(|r| r.ai_metadata.as_ref()).map(|ai| ai.model_name.clone()).filter(|s| !s.is_empty());
        let prov = record.as_ref().and_then(|r| r.env_provenance.as_ref());
        let (tp, tf) = record.as_ref().and_then(|r| r.test_metrics.as_ref()).map(|tm| (Some(tm.passed), Some(tm.failed))).unwrap_or((None, None));

        // review point for this commit (deterministic triggers).
        let rp = repo
            .suggest_review_points(500, 0.0)
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.commit_oid == oid.to_string());

        // review-first files: integrity findings paths + session blind edits + edits.
        let mut review_first: Vec<CockpitFile> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        if let Some(a) = &analysis {
            for cov in &a.coverage {
                if cov.blind_edit_count > 0 && seen.insert(cov.file.clone()) {
                    review_first.push(CockpitFile {
                        path: cov.file.clone(),
                        reason: "edited without reading first".into(),
                        severity: "warning".into(),
                    });
                }
            }
            for o in &a.omissions {
                if seen.insert(o.context_file.clone()) {
                    review_first.push(CockpitFile {
                        path: o.context_file.clone(),
                        reason: format!("{:?}", o.kind).to_lowercase(),
                        severity: "warning".into(),
                    });
                }
            }
            for e in &a.causal_chain.edit_sequence {
                if review_first.len() >= 8 {
                    break;
                }
                if seen.insert(e.file.clone()) {
                    review_first.push(CockpitFile {
                        path: e.file.clone(),
                        reason: "changed in this run".into(),
                        severity: "info".into(),
                    });
                }
            }
        }
        review_first.truncate(8);

        // merge confidence: start high, dock for real risk signals.
        let mut conf: i32 = 100;
        if tf.unwrap_or(0) > 0 {
            conf -= 35;
        }
        match integrity.level {
            crate::metadata::IntegrityLevel::Violation => conf -= 30,
            crate::metadata::IntegrityLevel::Warning => conf -= 12,
            _ => {}
        }
        if let Some(ps) = &prompt_score {
            if ps.score < 40.0 {
                conf -= 12;
            } else if ps.score < 60.0 {
                conf -= 6;
            }
        }
        if let Some(rp) = &rp {
            conf -= (rp.quality_score * 25.0) as i32;
        }
        let blind = analysis.as_ref().map(|a| a.coverage.iter().map(|c| c.blind_edit_count).sum::<usize>()).unwrap_or(0);
        if blind > 0 {
            conf -= (blind.min(4) as i32) * 4;
        }
        let merge_confidence = conf.clamp(0, 100) as u32;
        let risk = if merge_confidence >= 75 {
            "low"
        } else if merge_confidence >= 50 {
            "medium"
        } else {
            "high"
        };

        let provenance = match (&record.as_ref().and_then(|r| r.ai_metadata.as_ref()).map(|ai| ai.agent_id.clone()), prov) {
            (Some(agent), Some(p)) if !agent.is_empty() => format!("{} · {}", agent, p.isolation_claim),
            (Some(agent), None) if !agent.is_empty() => agent.clone(),
            _ => "unknown".into(),
        };

        Ok(ReviewerCockpit {
            oid: oid.to_string(),
            short_oid: oid.to_string()[..8].to_string(),
            message,
            author,
            timestamp: record.as_ref().map(|r| r.timestamp.to_rfc3339()).unwrap_or_default(),
            merge_confidence,
            prompt_maturity: prompt_score.as_ref().map(|s| s.score),
            provenance,
            model,
            sandbox: prov.map(|p| p.isolation_claim.clone()),
            policy_digest: prov.map(|p| p.policy_digest.clone()),
            net_blocked: 0,
            net_allowed: 0,
            tests_passed: tp,
            tests_failed: tf,
            integrity_level: format!("{:?}", integrity.level),
            integrity_score: integrity.score as f64,
            risk: risk.into(),
            review_first,
            review_score: rp.as_ref().map(|p| p.score).unwrap_or(0.0),
        })
    })
    .await;
    match result.ok().and_then(|r| r.ok()) {
        Some(c) => Json(c).into_response(),
        None => (StatusCode::NOT_FOUND, "commit not found").into_response(),
    }
}

/// The prompt-maturity coach payload (roadmap §6): score + weak spots + a
/// concrete suggested rewrite. Scores the task delegation, not the developer.
#[derive(Serialize)]
pub struct PromptMaturity {
    pub prompt: String,
    pub score: f64,
    pub level: String,
    pub words: usize,
    pub flags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_upgrade: Option<String>,
}

#[derive(Deserialize)]
struct PromptQuery {
    oid: Option<String>,
    text: Option<String>,
}

/// Build a concrete "upgrade" scaffold from the diagnostic flags — never a
/// keyword list, just the missing delegation structure.
fn suggested_upgrade(prompt: &str, flags: &[crate::prompt_score::Flag]) -> Option<String> {
    use crate::prompt_score::Flag;
    let mut adds: Vec<&str> = Vec::new();
    for f in flags {
        match f {
            Flag::WeakVerification => adds.push("Run <command> and confirm it passes before finishing."),
            Flag::WeakContext => adds.push("Scope: change only <file/module>; do not touch <out-of-scope area>."),
            Flag::Vague => adds.push("Acceptance criteria: <observable outcome that means done>."),
            Flag::TooShort => adds.push("State the goal, the files in scope, and how to verify the result."),
            _ => {}
        }
    }
    if adds.is_empty() {
        return None;
    }
    let base = prompt.trim();
    let mut out = String::new();
    if !base.is_empty() {
        out.push_str(base);
        out.push_str("\n\n");
    }
    for a in adds {
        out.push_str("- ");
        out.push_str(a);
        out.push('\n');
    }
    Some(out.trim_end().to_string())
}

/// GET /api/prompt-score?oid=…  or  ?text=… — the prompt-maturity coach.
async fn api_prompt_score(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PromptQuery>,
) -> Response {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<PromptMaturity>> {
        let prompt = if let Some(t) = q.text.filter(|s| !s.is_empty()) {
            t
        } else if let Some(oid_s) = q.oid {
            let repo = H5iRepository::open(&path)?;
            let oid = git2::Oid::from_str(&oid_s)?;
            match repo.load_h5i_record(oid).ok().and_then(|r| r.ai_metadata).map(|ai| ai.prompt) {
                Some(p) if !p.is_empty() => p,
                _ => return Ok(None),
            }
        } else {
            return Ok(None);
        };
        let s = crate::prompt_score::score_prompt(&prompt);
        let upgrade = suggested_upgrade(&prompt, &s.flags);
        Ok(Some(PromptMaturity {
            prompt,
            score: s.score,
            level: s.level.label().to_string(),
            words: s.words,
            flags: s.flags.iter().map(|f| f.label().to_string()).collect(),
            suggested_upgrade: upgrade,
        }))
    })
    .await;
    match result.ok().and_then(|r| r.ok()).flatten() {
        Some(p) => Json(p).into_response(),
        None => Json(serde_json::Value::Null).into_response(),
    }
}

/// One agent-radio message, sanitized for display.
#[derive(Serialize)]
pub struct RadioMessage {
    pub id: String,
    pub ts: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

/// A review/risk-resolution thread (roadmap §7 — code review, not chat).
#[derive(Serialize)]
pub struct RadioThread {
    pub thread_id: String,
    pub latest_ts: String,
    pub branch: Option<String>,
    pub status: String,
    pub messages: Vec<RadioMessage>,
}

/// GET /api/radio — agent messages grouped into review threads.
async fn api_radio(State(state): State<Arc<AppState>>) -> Json<Vec<RadioThread>> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RadioThread>> {
        let repo = H5iRepository::open(&path)?;
        let msgs = crate::msg::history(repo.git(), None, None, 1000).unwrap_or_default();
        let mut by_root: HashMap<String, Vec<crate::msg::Message>> = HashMap::new();
        for m in msgs {
            by_root.entry(m.thread_root()).or_default().push(m);
        }
        let mut threads: Vec<RadioThread> = by_root
            .into_iter()
            .map(|(thread_id, mut ms)| {
                ms.sort_by(|a, b| (a.ts.as_str(), a.id.as_str()).cmp(&(b.ts.as_str(), b.id.as_str())));
                let latest_ts = ms.last().map(|m| m.ts.clone()).unwrap_or_default();
                let branch = ms.iter().find_map(|m| m.branch.clone());
                // Resolution state: latest typed status in the thread.
                let status = ms
                    .iter()
                    .rev()
                    .find_map(|m| m.status.clone())
                    .unwrap_or_else(|| "open".into());
                let messages = ms
                    .into_iter()
                    .map(|m| RadioMessage {
                        kind: m.effective_kind(),
                        id: m.id,
                        ts: m.ts,
                        from: crate::msg::sanitize_display(&m.from),
                        to: crate::msg::sanitize_display(&m.to),
                        body: crate::msg::sanitize_display(&m.body),
                        status: m.status,
                        priority: m.priority,
                        branch: m.branch,
                        focus: m.focus.into_iter().map(|f| crate::msg::sanitize_display(&f)).collect(),
                        risk: m.risk.map(|r| crate::msg::sanitize_display(&r)),
                    })
                    .collect();
                RadioThread { thread_id, latest_ts, branch, status, messages }
            })
            .collect();
        threads.sort_by(|a, b| b.latest_ts.cmp(&a.latest_ts));
        Ok(threads)
    })
    .await;
    Json(result.ok().and_then(|r| r.ok()).unwrap_or_default())
}

/// Host isolation readiness, for the dashboard's top-strip vitals.
#[derive(Serialize)]
pub struct ProbeResponse {
    pub os: String,
    pub landlock_abi: Option<i32>,
    pub userns: bool,
    pub seccomp: bool,
    pub container_runtime: Option<String>,
    /// Per-tier satisfiability ("workspace"/"process"/"container").
    pub tiers: Vec<ProbeTier>,
    /// Functional self-test: are the `process`-tier bits actually runnable here?
    pub process_runnable: bool,
    pub process_runnable_detail: Option<String>,
    /// cgroup v2 resource-control availability (rootless, best-effort).
    pub cgroups: CgroupProbe,
    /// `isolation=supervised` readiness (fail-closed; unusable = impossible claim).
    pub supervisor: SupervisorProbe,
}

/// cgroup v2 readiness for the dashboard.
#[derive(Serialize)]
pub struct CgroupProbe {
    pub v2_mounted: bool,
    /// True iff h5i can actually create + limit a run cgroup here (delegation).
    pub usable: bool,
    pub controllers: Vec<String>,
    /// Why it's unusable, when it is (e.g. "no delegation").
    pub detail: Option<String>,
}

/// `isolation=supervised` readiness: the per-component breakdown plus the
/// fail-closed verdict. An unusable claim is *impossible*, not degraded.
#[derive(Serialize)]
pub struct SupervisorProbe {
    pub usable: bool,
    pub components: Vec<SupervisorComponent>,
}

#[derive(Serialize)]
pub struct SupervisorComponent {
    pub name: String,
    pub ok: bool,
    pub detail: Option<String>,
}

#[derive(Serialize)]
pub struct ProbeTier {
    pub claim: String,
    pub satisfiable: bool,
    pub note: Option<String>,
}

/// GET /api/env/probe — host sandbox capabilities + per-tier readiness.
async fn api_env_probe() -> Json<ProbeResponse> {
    let resp = tokio::task::spawn_blocking(|| {
        use crate::sandbox::{self, IsolationClaim, NetMode, Profile};
        let caps = sandbox::probe_host();

        let mut tiers = Vec::new();
        for (claim, net_host) in [
            (IsolationClaim::Workspace, true),
            (IsolationClaim::Process, false),
        ] {
            let mut p = Profile::builtin("probe", claim);
            if net_host {
                p.net_mode = NetMode::Host;
            }
            tiers.push(ProbeTier {
                claim: claim.as_str().to_string(),
                satisfiable: sandbox::resolve(&p, &caps).is_ok(),
                note: None,
            });
        }
        tiers.push(ProbeTier {
            claim: "container".into(),
            satisfiable: caps.container_runtime.is_some(),
            note: Some("needs rootless Podman + profile container.image".into()),
        });

        let sup = crate::supervisor::probe();
        tiers.push(ProbeTier {
            claim: "supervised".into(),
            satisfiable: sup.usable,
            note: Some(if sup.usable {
                "full mediation stack present".into()
            } else {
                // Impossible-claim language (Codex), not "degraded".
                format!("impossible here — missing {}", sup.missing().join(", "))
            }),
        });

        let probe = Profile::builtin("probe", IsolationClaim::Process);
        let (process_runnable, process_runnable_detail) =
            match sandbox::resolve(&probe, &caps).and_then(|pol| sandbox::verify_exec(&pol)) {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e.to_string())),
            };

        let cg = crate::cgroup::probe();

        ProbeResponse {
            os: caps.os.clone(),
            landlock_abi: caps.landlock_abi,
            userns: caps.userns,
            seccomp: caps.seccomp,
            container_runtime: caps.container_runtime.clone(),
            tiers,
            process_runnable,
            process_runnable_detail,
            cgroups: CgroupProbe {
                v2_mounted: cg.v2_mounted,
                usable: cg.usable,
                controllers: cg.controllers.clone(),
                detail: cg.detail.clone(),
            },
            supervisor: SupervisorProbe {
                usable: sup.usable,
                components: sup
                    .components
                    .iter()
                    .map(|c| SupervisorComponent {
                        name: c.name.to_string(),
                        ok: c.ok,
                        detail: c.detail.clone(),
                    })
                    .collect(),
            },
        }
    })
    .await
    .unwrap_or_else(|_| ProbeResponse {
        os: "unknown".into(),
        landlock_abi: None,
        userns: false,
        seccomp: false,
        container_runtime: None,
        tiers: Vec::new(),
        process_runnable: false,
        process_runnable_detail: Some("probe task panicked".into()),
        cgroups: CgroupProbe {
            v2_mounted: false,
            usable: false,
            controllers: Vec::new(),
            detail: Some("probe task panicked".into()),
        },
        supervisor: SupervisorProbe { usable: false, components: Vec::new() },
    });
    Json(resp)
}

// ── Server entry point ────────────────────────────────────────────────────────

pub async fn serve(repo_path: PathBuf, port: u16) -> anyhow::Result<()> {
    let state = Arc::new(AppState { repo_path });
    let app = build_router(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("  h5i UI →  http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the full router with all routes wired to `state`. Extracted from
/// [`serve`] so tests can drive the HTTP surface against a temp repo.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/v2", get(index))
        .route("/v2/", get(index))
        .route("/assets/*path", get(workbench_asset))
        .route("/api/repo", get(api_repo))
        .route("/api/branches", get(api_branches))
        .route("/api/commits", get(api_commits))
        .route("/api/commit-files", get(api_commit_files))
        .route("/api/integrity", get(api_integrity))
        .route("/api/integrity/commit", get(api_integrity_commit))
        .route("/api/intent-graph", get(api_intent_graph))
        .route("/api/review-points", get(api_review_points))
        .route("/api/memory/snapshots", get(api_memory_snapshots))
        .route("/api/memory/diff", get(api_memory_diff))
        .route("/api/session-log", get(api_session_log))
        .route("/api/session-log/list", get(api_session_list))
        .route("/api/session-log/churn", get(api_session_churn))
        .route("/api/context/status", get(api_context_status))
        .route("/api/context/snapshots", get(api_context_snapshots))
        .route("/api/context/show", get(api_context_show))
        .route("/api/context/milestones", get(api_context_milestones))
        .route("/api/context/diff", get(api_context_diff))
        .route("/api/context/relevant", get(api_context_relevant))
        .route("/api/context/search", get(api_context_search))
        .route("/api/context/dag", get(api_context_dag))
        .route("/api/context/promotion", get(api_context_promotion))
        // Sandbox dashboard (read-only). `/probe` is registered before the
        // `:agent/:slug` param route so it isn't captured as an env path.
        .route("/api/envs", get(api_envs))
        .route("/api/env/probe", get(api_env_probe))
        .route("/api/env/:agent/:slug", get(api_env_detail))
        .route("/api/env/:agent/:slug/replay", get(api_env_replay))
        .route("/api/env/:agent/:slug/captures/:id", get(api_env_capture))
        // Replay (commit fallback) + reviewer cockpit + prompt coach + radio
        .route("/api/commit/:oid/replay", get(api_commit_replay))
        .route("/api/cockpit", get(api_cockpit))
        .route("/api/prompt-score", get(api_prompt_score))
        .route("/api/radio", get(api_radio))
        .with_state(state)
}
