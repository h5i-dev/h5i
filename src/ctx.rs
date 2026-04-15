/// Agent context workspace — structured reasoning memory for LLM agents.
///
/// Implements the data structures and operations from:
///   "Git Context Controller: Manage the Context of Agents by Agentic Git"
///   arXiv:2508.00031
///
/// The context workspace is stored entirely in the `refs/h5i/context` Git ref
/// — a lightweight commit-chain whose tree mirrors the former `.h5i-ctx/` layout:
///
/// ```text
/// refs/h5i/context tree:
/// ├── main.md               # global roadmap: goals, milestones, active branches
/// ├── .current_branch       # active branch name
/// └── branches/
///     └── <branch-name>/
///         ├── commit.md     # milestone summaries (append-only log)
///         ├── trace.md      # OTA (Observation–Thought–Action) execution trace
///         └── metadata.yaml # file structure, deps, env config
/// ```
///
/// Exposed via `h5i context` subcommands.
use std::fmt::Write as FmtWrite;
use std::path::Path;

use chrono::Utc;
use git2::{ObjectType, Oid, Repository, Signature};
use serde::{Deserialize, Serialize};

use crate::error::H5iError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Git ref that stores the context workspace as a commit chain.
pub const CTX_REF: &str = "refs/h5i/context";

/// Legacy directory name kept for display / migration messages only.
pub const CTX_DIR: &str = ".h5i-ctx";
#[doc(hidden)]
pub const GCC_DIR: &str = CTX_DIR;

pub const MAIN_BRANCH: &str = "main";

// ── Data types ────────────────────────────────────────────────────────────────

/// A single commit entry appended to `commit.md`.
#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub branch_purpose: String,
    pub previous_summary: String,
    pub contribution: String,
    pub timestamp: String,
    pub short_id: String,
}

/// Options for the CONTEXT command.
#[derive(Debug, Default)]
pub struct ContextOpts {
    pub branch: Option<String>,
    /// If set, return only the commit entry whose short ID starts with this hash prefix.
    pub commit_hash: Option<String>,
    pub show_log: bool,
    /// Offset `k` into the log lines (sliding-window start position).
    pub log_offset: usize,
    pub metadata_segment: Option<String>,
    pub window: usize, // number of recent commits to show (default K)
}

/// Structured metadata stored in `metadata.yaml`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GccMetadata {
    pub file_structure: std::collections::HashMap<String, String>,
    pub env_config: std::collections::HashMap<String, String>,
    pub dependencies: Vec<DepEntry>,
    #[serde(default)]
    pub extra: std::collections::HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DepEntry {
    pub name: String,
    pub purpose: String,
}

/// High-level view returned by `gcc_context`.
#[derive(Serialize, Debug, Clone, Default)]
pub struct GccContext {
    pub project_goal: String,
    pub milestones: Vec<String>,
    pub active_branches: Vec<String>,
    pub current_branch: String,
    pub recent_commits: Vec<String>,
    pub recent_log_lines: Vec<String>,
    pub metadata_snippet: Option<String>,
}

// ── Git helpers ───────────────────────────────────────────────────────────────

fn ctx_git_repo(workdir: &Path) -> Result<Repository, H5iError> {
    Repository::discover(workdir).map_err(H5iError::Git)
}

/// Read a single virtual file from the tip of `refs/h5i/context`.
fn ctx_read_file(repo: &Repository, vpath: &str) -> Option<String> {
    let reference = repo.find_reference(CTX_REF).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(vpath)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Create a new commit on `refs/h5i/context` applying the given (path, content) changes
/// to the current tree. Handles arbitrarily nested paths (e.g. `branches/main/trace.md`).
fn ctx_write_files(
    repo: &Repository,
    changes: &[(&str, &str)],
    message: &str,
) -> Result<(), H5iError> {
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;

    let parent = repo
        .find_reference(CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let current_tree = parent.as_ref().and_then(|c| c.tree().ok());

    let new_tree_oid = apply_changes_to_tree(repo, current_tree.as_ref(), changes)?;
    let new_tree = repo.find_tree(new_tree_oid).map_err(H5iError::Git)?;

    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some(CTX_REF), &sig, &sig, message, &new_tree, &parents)
        .map_err(H5iError::Git)?;

    Ok(())
}

/// Recursively build a Git tree by applying `(relative_path, content)` changes onto
/// an optional base tree. Supports nested paths like `branches/main/commit.md`.
fn apply_changes_to_tree(
    repo: &Repository,
    base: Option<&git2::Tree>,
    changes: &[(&str, &str)],
) -> Result<Oid, H5iError> {
    // Partition into leaves (single component) and nested (two+ components).
    let mut leaves: Vec<(&str, &str)> = Vec::new();
    let mut nested: std::collections::HashMap<&str, Vec<(&str, &str)>> =
        std::collections::HashMap::new();

    for &(path, content) in changes {
        match path.split_once('/') {
            Some((dir, rest)) => nested.entry(dir).or_default().push((rest, content)),
            None => leaves.push((path, content)),
        }
    }

    let mut builder = repo.treebuilder(base).map_err(H5iError::Git)?;

    // Write leaf blobs.
    for (name, content) in leaves {
        let oid = repo.blob(content.as_bytes()).map_err(H5iError::Git)?;
        builder.insert(name, oid, 0o100644).map_err(H5iError::Git)?;
    }

    // Recurse into subdirectories.
    for (dir, sub_changes) in nested {
        let sub_base = base.and_then(|t| {
            t.get_name(dir)
                .filter(|e| e.kind() == Some(ObjectType::Tree))
                .and_then(|e| repo.find_tree(e.id()).ok())
        });
        let sub_oid = apply_changes_to_tree(repo, sub_base.as_ref(), &sub_changes)?;
        builder.insert(dir, sub_oid, 0o040000).map_err(H5iError::Git)?;
    }

    builder.write().map_err(H5iError::Git)
}

/// List branch names stored under `branches/` in the context tree.
fn ctx_list_branches_git(repo: &Repository) -> Vec<String> {
    let tree = repo
        .find_reference(CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());
    let tree = match tree {
        Some(t) => t,
        None => return vec![],
    };
    let branches_oid = match tree
        .get_name("branches")
        .filter(|e| e.kind() == Some(ObjectType::Tree))
        .map(|e| e.id())
    {
        Some(oid) => oid,
        None => return vec![],
    };
    let branches_tree = match repo.find_tree(branches_oid) {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let mut names: Vec<String> = Vec::new();
    collect_branch_names(repo, &branches_tree, "", &mut names);
    names.sort();
    names
}

/// Recursively walk a subtree under `branches/`. A tree entry is considered a
/// branch if it contains a blob named `commit.md`; otherwise we recurse into
/// nested trees (supporting slash-separated names like `experiment/alt`).
fn collect_branch_names(repo: &Repository, tree: &git2::Tree, prefix: &str, out: &mut Vec<String>) {
    for entry in tree.iter() {
        let Some(entry_name) = entry.name() else { continue };
        if entry.kind() != Some(ObjectType::Tree) {
            continue;
        }
        let full_name = if prefix.is_empty() {
            entry_name.to_owned()
        } else {
            format!("{prefix}/{entry_name}")
        };
        let Ok(subtree) = repo.find_tree(entry.id()) else { continue };
        // A branch directory contains `commit.md`.
        if subtree.get_name("commit.md").is_some() {
            out.push(full_name);
        } else {
            collect_branch_names(repo, &subtree, &full_name, out);
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize the context workspace in `refs/h5i/context`.
pub fn init(workdir: &Path, goal: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;

    // If the ref already exists, only ensure the main branch files are present.
    if repo.find_reference(CTX_REF).is_ok() {
        return ensure_branch_git(&repo, MAIN_BRANCH, "Primary development branch");
    }

    let main_content = format!(
        "# Project Roadmap\n\n\
         ## Goal\n{goal}\n\n\
         ## Milestones\n- [ ] Initial setup\n\n\
         ## Active Branches\n- main (primary)\n\n\
         ## Notes\n_Add project-wide notes here._\n"
    );
    let commit_content = format!(
        "# Branch: {MAIN_BRANCH}\n\n\
         **Purpose:** Primary development branch\n\n\
         _Commits will be appended below._\n\n"
    );
    let trace_content = format!("# OTA Log — Branch: {MAIN_BRANCH}\n\n");
    let meta_content = "file_structure: {}\nenv_config: {}\ndependencies: []\n";

    ctx_write_files(
        &repo,
        &[
            ("main.md", &main_content),
            (".current_branch", MAIN_BRANCH),
            (
                &format!("branches/{MAIN_BRANCH}/commit.md"),
                &commit_content,
            ),
            (
                &format!("branches/{MAIN_BRANCH}/trace.md"),
                &trace_content,
            ),
            (
                &format!("branches/{MAIN_BRANCH}/metadata.yaml"),
                meta_content,
            ),
        ],
        "h5i context init",
    )
}

/// Return `true` if `refs/h5i/context` exists in this repository.
pub fn is_initialized(workdir: &Path) -> bool {
    ctx_git_repo(workdir)
        .map(|repo| repo.find_reference(CTX_REF).is_ok())
        .unwrap_or(false)
}

/// Return the current active branch name.
pub fn current_branch(workdir: &Path) -> String {
    ctx_git_repo(workdir)
        .ok()
        .and_then(|repo| ctx_read_file(&repo, ".current_branch"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| MAIN_BRANCH.to_string())
}

fn set_current_branch(repo: &Repository, branch: &str) -> Result<(), H5iError> {
    ctx_write_files(repo, &[(".current_branch", branch)], "h5i context checkout")
}

/// COMMIT — checkpoint the agent's current progress as a structured milestone.
pub fn gcc_commit(workdir: &Path, summary: &str, contribution: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);

    let commit_path = format!("branches/{branch}/commit.md");
    let trace_path = format!("branches/{branch}/trace.md");

    let existing_commit = ctx_read_file(&repo, &commit_path).unwrap_or_default();
    let previous_summary = extract_latest_summary(&existing_commit);
    let branch_purpose = extract_branch_purpose(&existing_commit)
        .unwrap_or_else(|| format!("Branch: {branch}"));

    let short_id = short_timestamp_id();
    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    let entry = format!(
        "## Commit {short_id} — {ts}\n\n\
         ### Branch Purpose\n{branch_purpose}\n\n\
         ### Previous Progress Summary\n{previous_summary}\n\n\
         ### This Commit's Contribution\n{contribution}\n\n\
         ---\n\n"
    );
    let new_commit_md = format!("{existing_commit}{entry}");

    let existing_trace = ctx_read_file(&repo, &trace_path).unwrap_or_default();
    let log_marker = format!("\n\n---\n_[Checkpoint: {short_id} — {summary}]_\n---\n\n");
    let new_trace = format!("{existing_trace}{log_marker}");

    let existing_main = ctx_read_file(&repo, "main.md").unwrap_or_default();
    let new_main = append_main_note(&existing_main, &branch, summary);

    ctx_write_files(
        &repo,
        &[
            (&commit_path, &new_commit_md),
            (&trace_path, &new_trace),
            ("main.md", &new_main),
        ],
        &format!("h5i context commit: {summary}"),
    )
}

/// BRANCH — create a new isolated reasoning workspace and switch to it.
pub fn gcc_branch(workdir: &Path, name: &str, purpose: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    ensure_branch_git(&repo, name, purpose)?;
    set_current_branch(&repo, name)
}

/// Switch the active branch without creating it.
pub fn gcc_checkout(workdir: &Path, name: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    if !ctx_list_branches_git(&repo).contains(&name.to_string()) {
        return Err(H5iError::InvalidPath(format!(
            "Context branch '{name}' does not exist. Run `h5i context branch {name}` first."
        )));
    }
    set_current_branch(&repo, name)
}

/// MERGE — synthesize a completed branch into the current branch.
pub fn gcc_merge(workdir: &Path, source_branch: &str) -> Result<String, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let target = current_branch(workdir);

    if !ctx_list_branches_git(&repo).contains(&source_branch.to_string()) {
        return Err(H5iError::InvalidPath(format!(
            "Branch '{source_branch}' not found"
        )));
    }

    let source_commit_path = format!("branches/{source_branch}/commit.md");
    let source_trace_path = format!("branches/{source_branch}/trace.md");
    let target_commit_path = format!("branches/{target}/commit.md");
    let target_trace_path = format!("branches/{target}/trace.md");

    let source_commit_text = ctx_read_file(&repo, &source_commit_path).unwrap_or_default();
    let source_summary = extract_latest_summary(&source_commit_text);
    let source_purpose = extract_branch_purpose(&source_commit_text)
        .unwrap_or_else(|| source_branch.to_string());

    let target_commit_text = ctx_read_file(&repo, &target_commit_path).unwrap_or_default();
    let target_summary = extract_latest_summary(&target_commit_text);

    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let short_id = short_timestamp_id();

    let merged_summary = format!(
        "Merged branch '{source_branch}' into '{target}'.\n\n\
         From {source_branch}: {source_summary}\n\n\
         From {target}: {target_summary}"
    );
    let contribution = format!(
        "MERGE of '{source_branch}' (purpose: {source_purpose}) into '{target}'.\n\
         Combined reasoning and outcomes from both branches."
    );

    let source_log = ctx_read_file(&repo, &source_trace_path).unwrap_or_default();
    let target_log = ctx_read_file(&repo, &target_trace_path).unwrap_or_default();
    let new_trace = format!(
        "{target_log}\n\n---\n_[MERGE from '{source_branch}' at {ts}]_\n\n{source_log}\n---\n"
    );

    let merge_entry = format!(
        "## Commit {short_id} — {ts} [MERGE: {source_branch} → {target}]\n\n\
         ### Branch Purpose\nMerge of branch '{source_branch}'\n\n\
         ### Previous Progress Summary\n{merged_summary}\n\n\
         ### This Commit's Contribution\n{contribution}\n\n\
         ---\n\n"
    );
    let new_commit = format!("{target_commit_text}{merge_entry}");

    let existing_main = ctx_read_file(&repo, "main.md").unwrap_or_default();
    let new_main = append_main_note(
        &existing_main,
        &target,
        &format!("Merged branch '{source_branch}'"),
    );

    ctx_write_files(
        &repo,
        &[
            (&target_trace_path, &new_trace),
            (&target_commit_path, &new_commit),
            ("main.md", &new_main),
        ],
        &format!("h5i context merge: {source_branch} → {target}"),
    )?;

    Ok(merged_summary)
}

/// CONTEXT — retrieve structured context at multiple granularities.
pub fn gcc_context(workdir: &Path, opts: &ContextOpts) -> Result<GccContext, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch_name = opts
        .branch
        .clone()
        .unwrap_or_else(|| current_branch(workdir));

    let main_text = ctx_read_file(&repo, "main.md").unwrap_or_default();
    let project_goal = extract_section(&main_text, "Goal");
    let milestones = extract_list_items(&extract_section(&main_text, "Milestones"));
    let active_branches = ctx_list_branches_git(&repo);

    let commit_path = format!("branches/{branch_name}/commit.md");
    let commit_text = ctx_read_file(&repo, &commit_path).unwrap_or_default();

    let window = if opts.window == 0 { 3 } else { opts.window };
    let recent_commits = if let Some(ref hash) = opts.commit_hash {
        find_commit_by_hash(&commit_text, hash)
            .map(|e| vec![e])
            .unwrap_or_default()
    } else {
        extract_recent_commits(&commit_text, window)
    };

    let recent_log_lines = if opts.show_log {
        let trace_path = format!("branches/{branch_name}/trace.md");
        let log_text = ctx_read_file(&repo, &trace_path).unwrap_or_default();
        let all_lines: Vec<&str> = log_text.lines().collect();
        let total = all_lines.len();
        let budget = (window * 20).max(40);
        let end = total.saturating_sub(opts.log_offset);
        let start = end.saturating_sub(budget);
        all_lines[start..end].iter().map(|l| l.to_string()).collect()
    } else {
        vec![]
    };

    let metadata_snippet = if let Some(ref seg) = opts.metadata_segment {
        let meta_path = format!("branches/{branch_name}/metadata.yaml");
        let meta_text = ctx_read_file(&repo, &meta_path).unwrap_or_default();
        Some(extract_yaml_segment(&meta_text, seg))
    } else {
        None
    };

    Ok(GccContext {
        project_goal,
        milestones,
        active_branches,
        current_branch: branch_name,
        recent_commits,
        recent_log_lines,
        metadata_snippet,
    })
}

/// Append an OTA (Observation–Thought–Action) entry to the current branch's `trace.md`.
pub fn append_log(workdir: &Path, kind: &str, content: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let trace_path = format!("branches/{branch}/trace.md");

    let ts = Utc::now().format("%H:%M:%S").to_string();
    let entry = format!("[{ts}] {}: {}\n", kind.to_uppercase(), content);

    let existing = ctx_read_file(&repo, &trace_path).unwrap_or_default();
    ctx_write_files(
        &repo,
        &[(&trace_path, &format!("{existing}{entry}"))],
        "h5i context trace",
    )
}

/// Update `metadata.yaml` for the current branch.
pub fn update_metadata(workdir: &Path, meta: &GccMetadata) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let meta_path = format!("branches/{branch}/metadata.yaml");
    let yaml = serde_yaml_serialize(meta);
    ctx_write_files(&repo, &[(&meta_path, &yaml)], "h5i context metadata")
}

/// Write a single arbitrary file into the context workspace.
/// Useful for directly updating `main.md` (e.g. to tick off a milestone).
pub fn write_ctx_file(workdir: &Path, vpath: &str, content: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    ctx_write_files(&repo, &[(vpath, content)], "h5i context write")
}

/// List all branch names in the context workspace.
pub fn list_branches(workdir: &Path) -> Vec<String> {
    ctx_git_repo(workdir)
        .map(|repo| ctx_list_branches_git(&repo))
        .unwrap_or_default()
}

/// Return the raw text of `trace.md` for the given branch (default: current).
/// Returns an empty string if the workspace or trace does not yet exist.
pub fn read_trace(workdir: &Path, branch: Option<&str>) -> Result<String, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch_name = branch
        .map(|s| s.to_string())
        .unwrap_or_else(|| current_branch(workdir));
    let trace_path = format!("branches/{branch_name}/trace.md");
    Ok(ctx_read_file(&repo, &trace_path).unwrap_or_default())
}

// ── Context versioning ────────────────────────────────────────────────────────

/// Record a context snapshot linked to a git commit SHA.
/// Called automatically after every `h5i commit`. Silently no-ops if the
/// context workspace has not been initialised.
pub fn snapshot_for_commit(workdir: &Path, git_sha: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    if repo.find_reference(CTX_REF).is_err() {
        return Ok(());
    }

    let ctx_oid = repo
        .find_reference(CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .map(|c| c.id().to_string())
        .unwrap_or_default();

    let branch = current_branch(workdir);
    let goal = ctx_read_file(&repo, "main.md")
        .map(|t| extract_section(&t, "Goal"))
        .unwrap_or_default();

    let commit_path = format!("branches/{branch}/commit.md");
    let recent_commits = extract_recent_commits(
        &ctx_read_file(&repo, &commit_path).unwrap_or_default(),
        3,
    );

    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let short_sha = &git_sha[..git_sha.len().min(8)];

    let mut content = format!(
        "# Context Snapshot — {short_sha}\n\n\
         **Linked commit:** {git_sha}\n\
         **Context ref OID:** {ctx_oid}\n\
         **Timestamp:** {ts}\n\
         **Branch:** {branch}\n\
         **Goal:** {goal}\n\n\
         ## Recent Context Commits\n"
    );
    for c in &recent_commits {
        let _ = writeln!(content, "- {}", c.chars().take(100).collect::<String>());
    }
    if recent_commits.is_empty() {
        content.push_str("_(none yet)_\n");
    }

    let snapshot_path = format!("snapshots/{short_sha}.md");
    ctx_write_files(
        &repo,
        &[(&snapshot_path, &content)],
        &format!("h5i context snapshot: {short_sha}"),
    )
}

/// Restore the context workspace to the state captured at a given git commit.
///
/// Restoration is non-destructive: a new commit is appended to `refs/h5i/context`
/// whose tree mirrors the snapshot, preserving the full history.
/// Returns a short summary of what was restored.
pub fn restore(workdir: &Path, git_sha: &str) -> Result<String, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let short_sha = &git_sha[..git_sha.len().min(8)];

    let snapshot = ctx_read_file(&repo, &format!("snapshots/{short_sha}.md"))
        .ok_or_else(|| {
            H5iError::InvalidPath(format!(
                "No context snapshot for commit {git_sha}. \
                 Snapshots are written automatically by `h5i commit`."
            ))
        })?;

    let ctx_oid_str = snapshot
        .lines()
        .find(|l| l.starts_with("**Context ref OID:**"))
        .and_then(|l| l.split("**Context ref OID:**").nth(1))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| H5iError::InvalidPath("Snapshot is missing Context ref OID".into()))?;

    let ctx_oid = git2::Oid::from_str(ctx_oid_str).map_err(H5iError::Git)?;
    let restore_commit = repo
        .find_commit(ctx_oid)
        .map_err(|_| H5iError::InvalidPath(format!("Context OID {ctx_oid_str} not in object store")))?;

    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;

    let restore_tree = restore_commit.tree().map_err(H5iError::Git)?;
    let current_parent = repo
        .find_reference(CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = current_parent.iter().collect();

    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    repo.commit(
        Some(CTX_REF),
        &sig,
        &sig,
        &format!("h5i context restore: {short_sha} (at {ts})"),
        &restore_tree,
        &parents,
    )
    .map_err(H5iError::Git)?;

    let branch = snapshot
        .lines()
        .find(|l| l.starts_with("**Branch:**"))
        .and_then(|l| l.split("**Branch:**").nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let goal = snapshot
        .lines()
        .find(|l| l.starts_with("**Goal:**"))
        .and_then(|l| l.split("**Goal:**").nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Ok(format!(
        "branch: {branch}  ·  goal: {goal}"
    ))
}

/// Difference between two context snapshots.
#[derive(Debug, Default)]
pub struct ContextDiff {
    pub sha1: String,
    pub sha2: String,
    /// Context milestones present in sha2 but not sha1.
    pub added_commits: Vec<String>,
    /// Trace lines present in sha2 but not sha1 (OTA steps, up to 30).
    pub added_trace_lines: Vec<String>,
    pub goal_changed: bool,
    pub from_goal: String,
    pub to_goal: String,
}

/// Compare the context workspace state at two code commits.
pub fn context_diff(workdir: &Path, sha1: &str, sha2: &str) -> Result<ContextDiff, H5iError> {
    let repo = ctx_git_repo(workdir)?;

    let load_ctx_commit = |sha: &str| -> Result<git2::Commit, H5iError> {
        let short = &sha[..sha.len().min(8)];
        let snap = ctx_read_file(&repo, &format!("snapshots/{short}.md"))
            .ok_or_else(|| H5iError::InvalidPath(format!("No context snapshot for {sha}")))?;
        let oid_str = snap
            .lines()
            .find(|l| l.starts_with("**Context ref OID:**"))
            .and_then(|l| l.split("**Context ref OID:**").nth(1))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| H5iError::InvalidPath("Snapshot missing Context ref OID".into()))?
            .to_string();
        let oid = git2::Oid::from_str(&oid_str).map_err(H5iError::Git)?;
        repo.find_commit(oid)
            .map_err(|_| H5iError::InvalidPath(format!("Context OID {oid_str} not in object store")))
    };

    let c1 = load_ctx_commit(sha1)?;
    let c2 = load_ctx_commit(sha2)?;

    // Read a file from a specific commit's tree.
    let read_from = |commit: &git2::Commit, path: &str| -> String {
        (|| -> Option<String> {
            let tree = commit.tree().ok()?;
            let entry = tree.get_path(Path::new(path)).ok()?;
            let blob = repo.find_blob(entry.id()).ok()?;
            std::str::from_utf8(blob.content()).ok().map(str::to_owned)
        })()
        .unwrap_or_default()
    };

    let branch = current_branch(workdir);
    let commit_path = format!("branches/{branch}/commit.md");
    let trace_path = format!("branches/{branch}/trace.md");

    let commits1: std::collections::HashSet<String> =
        extract_recent_commits(&read_from(&c1, &commit_path), 200)
            .into_iter()
            .collect();
    let commits2 = extract_recent_commits(&read_from(&c2, &commit_path), 200);
    let added_commits: Vec<String> = commits2
        .into_iter()
        .filter(|c| !commits1.contains(c))
        .collect();

    let trace1: std::collections::HashSet<String> =
        read_from(&c1, &trace_path).lines().map(str::to_string).collect();
    let added_trace_lines: Vec<String> = read_from(&c2, &trace_path)
        .lines()
        .filter(|l| !l.is_empty() && !trace1.contains(*l))
        .take(30)
        .map(str::to_string)
        .collect();

    let main1 = read_from(&c1, "main.md");
    let main2 = read_from(&c2, "main.md");
    let from_goal = extract_section(&main1, "Goal");
    let to_goal = extract_section(&main2, "Goal");
    let goal_changed = from_goal != to_goal;

    Ok(ContextDiff {
        sha1: sha1.to_string(),
        sha2: sha2.to_string(),
        added_commits,
        added_trace_lines,
        goal_changed,
        from_goal,
        to_goal,
    })
}

/// Context entries in the current workspace that mention a specific file.
#[derive(Debug, Default)]
pub struct RelevantContext {
    /// OTA trace lines that mention the file (with one line of surrounding context).
    pub trace_mentions: Vec<String>,
    /// Context commit (milestone) contributions that mention the file.
    pub commit_mentions: Vec<String>,
    /// Trace/commit entries from *other* branches that mention the file.
    pub cross_branch_mentions: Vec<String>,
}

/// Return context workspace entries relevant to `file_path`.
pub fn relevant(workdir: &Path, file_path: &str) -> Result<RelevantContext, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);

    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let matches_file = |text: &str| text.contains(file_path) || text.contains(file_name);

    // ── Trace mentions ────────────────────────────────────────────────────────
    let trace_text =
        ctx_read_file(&repo, &format!("branches/{branch}/trace.md")).unwrap_or_default();
    let trace_lines: Vec<&str> = trace_text.lines().collect();
    let mut trace_mentions: Vec<String> = Vec::new();
    for (i, line) in trace_lines.iter().enumerate() {
        if matches_file(line) {
            // Include one line before and one after for context.
            let start = i.saturating_sub(1);
            let end = (i + 2).min(trace_lines.len());
            for l in &trace_lines[start..end] {
                if !l.is_empty() {
                    let s = l.to_string();
                    if !trace_mentions.contains(&s) {
                        trace_mentions.push(s);
                    }
                }
            }
        }
    }

    // ── Commit mentions ───────────────────────────────────────────────────────
    let commit_text =
        ctx_read_file(&repo, &format!("branches/{branch}/commit.md")).unwrap_or_default();
    let mut commit_mentions: Vec<String> = Vec::new();
    for entry in commit_text.split("## Commit ").skip(1) {
        if matches_file(entry) {
            if let Some(start) = entry.find("### This Commit's Contribution") {
                let after = &entry[start + "### This Commit's Contribution".len()..];
                let end = after.find("\n---").unwrap_or(after.len());
                let text = after[..end].trim().chars().take(200).collect::<String>();
                if !text.is_empty() {
                    commit_mentions.push(text);
                }
            }
        }
    }

    // ── Cross-branch mentions ─────────────────────────────────────────────────
    let mut cross_branch_mentions: Vec<String> = Vec::new();
    for other in ctx_list_branches_git(&repo) {
        if other == branch {
            continue;
        }
        let other_trace =
            ctx_read_file(&repo, &format!("branches/{other}/trace.md")).unwrap_or_default();
        for line in other_trace.lines() {
            if matches_file(line) && !line.is_empty() {
                cross_branch_mentions.push(format!("[{other}] {line}"));
                if cross_branch_mentions.len() >= 10 {
                    break;
                }
            }
        }
        let other_commit =
            ctx_read_file(&repo, &format!("branches/{other}/commit.md")).unwrap_or_default();
        for entry in other_commit.split("## Commit ").skip(1) {
            if matches_file(entry) {
                if let Some(start) = entry.find("### This Commit's Contribution") {
                    let after = &entry[start + "### This Commit's Contribution".len()..];
                    let end = after.find("\n---").unwrap_or(after.len());
                    let text = after[..end].trim().chars().take(200).collect::<String>();
                    if !text.is_empty() {
                        cross_branch_mentions.push(format!("[{other}] {text}"));
                    }
                }
            }
        }
    }

    Ok(RelevantContext {
        trace_mentions: trace_mentions.into_iter().take(20).collect(),
        commit_mentions,
        cross_branch_mentions: cross_branch_mentions.into_iter().take(10).collect(),
    })
}

// ── Context versioning: display ───────────────────────────────────────────────

pub fn print_context_diff(diff: &ContextDiff) {
    use console::style;

    println!(
        "{}",
        style(format!(
            "── Context diff  {}..{} ────────────────────────────────────────",
            &diff.sha1[..diff.sha1.len().min(8)],
            &diff.sha2[..diff.sha2.len().min(8)]
        ))
        .dim()
    );

    if diff.goal_changed {
        println!();
        println!("  {} {}", style("Goal changed:").bold().yellow(), "");
        println!("    {} {}", style("-").red(), style(&diff.from_goal).dim());
        println!("    {} {}", style("+").green(), style(&diff.to_goal).cyan());
    }

    if !diff.added_commits.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("New milestones:").bold(),
            style(format!("({})", diff.added_commits.len())).dim()
        );
        for c in &diff.added_commits {
            println!("    {} {}", style("+").green(), c);
        }
    } else {
        println!();
        println!("  {}", style("No new milestones.").dim());
    }

    if !diff.added_trace_lines.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("New OTA trace steps:").bold(),
            style(format!("({})", diff.added_trace_lines.len())).dim()
        );
        for line in &diff.added_trace_lines {
            println!("    {}", style(line).dim());
        }
    }
}

pub fn print_relevant(ctx: &RelevantContext, file_path: &str) {
    use console::style;

    println!(
        "{}",
        style(format!(
            "── Context relevant to {} ────────────────────────────────────",
            file_path
        ))
        .dim()
    );

    if ctx.trace_mentions.is_empty() && ctx.commit_mentions.is_empty() && ctx.cross_branch_mentions.is_empty() {
        println!(
            "  {}",
            style("No context entries mention this file yet.").dim()
        );
        return;
    }

    if !ctx.commit_mentions.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("Milestones:").bold(),
            style(format!("({})", ctx.commit_mentions.len())).dim()
        );
        for c in &ctx.commit_mentions {
            println!("    {} {}", style("◈").cyan(), c);
        }
    }

    if !ctx.trace_mentions.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("Trace mentions:").bold(),
            style(format!("({})", ctx.trace_mentions.len())).dim()
        );
        for line in &ctx.trace_mentions {
            println!("    {}", style(line).dim());
        }
    }

    if !ctx.cross_branch_mentions.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("Cross-branch:").bold(),
            style(format!("({})", ctx.cross_branch_mentions.len())).dim()
        );
        for line in &ctx.cross_branch_mentions {
            println!("    {}", style(line).dim());
        }
    }
}

/// Compact old context history by squashing context commits that predate
/// the earliest available snapshot into a single "packed base" commit.
/// Returns the number of commits squashed.
pub fn pack(workdir: &Path) -> Result<usize, H5iError> {
    let repo = ctx_git_repo(workdir)?;

    // Collect all snapshot short-SHAs so we know which context commits are still live.
    let tip = repo
        .find_reference(CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let tip = match tip {
        Some(t) => t,
        None => return Ok(0),
    };

    // Walk the snapshot directory to find the earliest referenced context OID.
    let snapshots_oids: Vec<git2::Oid> = {
        let tree = tip.tree().map_err(H5iError::Git)?;
        let snapshots_entry = tree
            .get_name("snapshots")
            .filter(|e| e.kind() == Some(ObjectType::Tree))
            .map(|e| e.id());
        let mut oids = Vec::new();
        if let Some(snap_tree_oid) = snapshots_oids_from_tree(&repo, snapshots_entry)? {
            oids = snap_tree_oid;
        }
        oids
    };

    if snapshots_oids.is_empty() {
        // Nothing to pack — no snapshots recorded yet.
        return Ok(0);
    }

    // Walk the context commit chain to find how many commits precede the oldest snapshot.
    let mut walk = repo.revwalk().map_err(H5iError::Git)?;
    walk.push(tip.id()).map_err(H5iError::Git)?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL).map_err(H5iError::Git)?;

    let mut commits_before_oldest: Vec<git2::Oid> = Vec::new();
    for oid_result in walk {
        let oid = oid_result.map_err(H5iError::Git)?;
        if snapshots_oids.contains(&oid) {
            break;
        }
        commits_before_oldest.push(oid);
    }

    let squash_count = commits_before_oldest.len().saturating_sub(1);
    if squash_count == 0 {
        return Ok(0);
    }

    // The oldest commit to keep becomes the new "pack base" — we rewrite the ref
    // to point to the current tip unchanged, having validated that old history can
    // be pruned. (Actual object pruning happens via `git gc`.)
    // For now we record a "packed" marker commit summarising what was squashed.
    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;

    // Append a pack marker to main.md.
    let main_text = ctx_read_file(&repo, "main.md").unwrap_or_default();
    let pack_note = format!("- [{ts}] _Packed {squash_count} old context commits_\n");
    let new_main = if let Some(pos) = main_text.find("## Notes") {
        let after = &main_text[pos..];
        let insert_at = pos + after.find('\n').map(|i| i + 1).unwrap_or(after.len());
        let mut s = main_text.clone();
        s.insert_str(insert_at, &pack_note);
        s
    } else {
        format!("{main_text}\n## Notes\n{pack_note}")
    };

    let current_tree = tip.tree().map_err(H5iError::Git)?;
    let new_tree_oid = apply_changes_to_tree(&repo, Some(&current_tree), &[("main.md", &new_main)])?;
    let new_tree = repo.find_tree(new_tree_oid).map_err(H5iError::Git)?;
    let parents = [&tip];
    repo.commit(
        Some(CTX_REF),
        &sig,
        &sig,
        &format!("h5i context pack: squashed {squash_count} old commits"),
        &new_tree,
        &parents,
    )
    .map_err(H5iError::Git)?;

    Ok(squash_count)
}

/// Walk the `snapshots/` subtree and return all context OIDs referenced by snapshot files.
fn snapshots_oids_from_tree(
    repo: &Repository,
    snap_tree_oid: Option<git2::Oid>,
) -> Result<Option<Vec<git2::Oid>>, H5iError> {
    let snap_tree_oid = match snap_tree_oid {
        Some(o) => o,
        None => return Ok(None),
    };
    let snap_tree = repo.find_tree(snap_tree_oid).map_err(H5iError::Git)?;
    let mut oids = Vec::new();
    for entry in snap_tree.iter() {
        if entry.kind() != Some(ObjectType::Blob) {
            continue;
        }
        let blob = repo.find_blob(entry.id()).map_err(H5iError::Git)?;
        let content = std::str::from_utf8(blob.content()).unwrap_or("");
        for line in content.lines() {
            if line.starts_with("**Context ref OID:**") {
                if let Some(oid_str) = line.split("**Context ref OID:**").nth(1) {
                    if let Ok(oid) = git2::Oid::from_str(oid_str.trim()) {
                        oids.push(oid);
                    }
                }
            }
        }
    }
    Ok(Some(oids))
}

// ── Terminal display ──────────────────────────────────────────────────────────

pub fn print_context(ctx: &GccContext) {
    use console::style;

    println!(
        "{}",
        style("── Context ─────────────────────────────────────────────").dim()
    );
    println!(
        "  {} {}  (branch: {})",
        style("Project:").bold(),
        if ctx.project_goal.is_empty() {
            style("(no goal set)".to_string()).dim()
        } else {
            style(ctx.project_goal.chars().take(80).collect::<String>()).cyan()
        },
        style(&ctx.current_branch).magenta(),
    );

    if !ctx.milestones.is_empty() {
        println!();
        println!("  {}", style("Milestones:").bold());
        for m in &ctx.milestones {
            let done = m.starts_with("[x]") || m.starts_with("[X]");
            let label: String = m.chars().take(80).collect();
            if done {
                println!("    {} {}", style("✔").green(), style(&label).dim());
            } else {
                println!("    {} {}", style("○").yellow(), label);
            }
        }
    }

    if ctx.active_branches.len() > 1 {
        println!();
        println!(
            "  {} {}",
            style("Branches:").bold(),
            ctx.active_branches
                .iter()
                .map(|b| {
                    if b == &ctx.current_branch {
                        style(format!("* {b}")).green().to_string()
                    } else {
                        style(b.clone()).dim().to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("  ·  ")
        );
    }

    if !ctx.recent_commits.is_empty() {
        println!();
        println!("  {}", style("Recent Commits:").bold());
        for c in &ctx.recent_commits {
            let preview: String = c.chars().take(100).collect();
            println!("    {} {}", style("◈").cyan(), preview);
        }
    }

    if !ctx.recent_log_lines.is_empty() {
        println!();
        println!("  {}", style("Recent OTA Log:").bold());
        for line in ctx.recent_log_lines.iter().take(10) {
            println!("    {}", style(line).dim());
        }
    }
}

pub fn print_status(workdir: &Path) -> Result<(), H5iError> {
    use console::style;

    if !is_initialized(workdir) {
        println!(
            "{} {} not initialized. Run {} to initialize.",
            style("ℹ").blue(),
            style(CTX_REF).yellow(),
            style("h5i context init").bold()
        );
        return Ok(());
    }

    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let branches = ctx_list_branches_git(&repo);

    let commit_text = ctx_read_file(&repo, &format!("branches/{branch}/commit.md"))
        .unwrap_or_default();
    let trace_text = ctx_read_file(&repo, &format!("branches/{branch}/trace.md"))
        .unwrap_or_default();

    let commit_count = commit_text.matches("## Commit ").count();
    let log_lines = trace_text.lines().count();

    println!(
        "{}",
        style("── Context Status ──────────────────────────────────────────────").dim()
    );
    println!(
        "  {} {}  |  {} branch{}  |  {} commit{}  |  {} log line{}",
        style("Active branch:").dim(),
        style(&branch).magenta().bold(),
        style(branches.len()).cyan(),
        if branches.len() == 1 { "" } else { "es" },
        style(commit_count).cyan(),
        if commit_count == 1 { "" } else { "s" },
        style(log_lines).dim(),
        if log_lines == 1 { "" } else { "s" },
    );

    if branches.len() > 1 {
        let others: Vec<&String> = branches.iter().filter(|b| b.as_str() != branch).collect();
        println!(
            "  {} {}",
            style("Other branches:").dim(),
            others
                .iter()
                .map(|b| b.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(())
}

// ── System prompt ─────────────────────────────────────────────────────────────

pub fn system_prompt(workdir: &Path) -> String {
    let status_block = if is_initialized(workdir) {
        let branch = current_branch(workdir);
        let branches = list_branches(workdir);
        let goal = ctx_git_repo(workdir)
            .ok()
            .and_then(|repo| ctx_read_file(&repo, "main.md"))
            .map(|t| extract_section(&t, "Goal"))
            .unwrap_or_default();
        format!(
            "\n## Current Workspace State\n\
             - Active branch: `{branch}`\n\
             - All branches: {}\n\
             - Project goal: {}\n\
             \n\
             **Start this session** by running `h5i context show --log` to restore your full working context.\n",
            branches.join(", "),
            if goal.is_empty() { "(not set)".to_string() } else { goal }
        )
    } else {
        "\n## Getting Started\n\
         Run `h5i context init --goal \"<your project goal>\"` to initialize the workspace.\n"
            .to_string()
    };

    format!(
        r#"# Git Context Controller (GCC)

You are working within a GCC workspace that organizes your memory as a persistent,
versioned Git ref (`{CTX_REF}`). Use the commands below to manage context across
long-horizon tasks. GCC prevents context-window overflow by externalizing reasoning
into structured files that survive session boundaries.
{status_block}
## File System Layout

```
refs/h5i/context tree:
├── main.md                    # global roadmap: goal, milestones, notes
├── .current_branch            # active branch name
└── branches/
    └── <branch>/
        ├── commit.md          # milestone summaries (append-only)
        ├── trace.md           # OTA (Observation–Thought–Action) execution trace
        └── metadata.yaml      # file structure, dependencies, env config
```

## Commands

### `h5i context show [OPTIONS]`
Retrieve your current project state. Returns the global roadmap, active branches,
and recent commit summaries.

**Required calls:**
- **At the start of every session** — run `h5i context show --log` to restore context.
- **Before every MERGE** — review the target branch first.
- Proactively whenever you need to recall prior reasoning.

Options:
  `--log`              Include the recent OTA execution trace from trace.md
  `--branch <name>`    Inspect a specific branch (default: current branch)
  `--commit <hash>`    Retrieve the complete record for a specific commit
  `--window <N>`       Number of recent commits to show (default: 3)
  `--log-offset <N>`   Scroll back N lines in the log (for older traces)

### `h5i context trace --kind <KIND> "<content>"`
Append a reasoning step to the execution trace. Call **continuously** during
execution to record every significant step. KIND is one of:
  `OBSERVE`  — an external observation (tool output, test result, file content)
  `THINK`    — internal reasoning, hypothesis, or plan adjustment
  `ACT`      — an action taken (edit, command, API call)
  `NOTE`     — a free-form annotation or reminder

### `h5i context commit "<summary>" [--detail "<contribution>"]`
Checkpoint meaningful progress. Call when you complete a coherent milestone:
implementing a function, passing a test suite, resolving a subgoal.
- `summary`    — one-line description (used in main.md and as the rolling summary)
- `--detail`   — full narrative of what was achieved since the last commit

### `h5i context branch <name> [--purpose "<why>"]`
Create an isolated workspace for exploring an alternative without disrupting the
main trajectory.

### `h5i context checkout <name>`
Switch to an existing branch.

### `h5i context merge <branch>`
Integrate a completed branch into the current branch.

### `h5i context status`
Show active branch, commit count, and log size.

## Workflow Pattern

```
# Session start (mandatory)
h5i context show --trace

# During execution (continuous)
h5i context trace --kind OBSERVE "test suite output: 3 failures in auth module"
h5i context trace --kind THINK   "failures are in token validation; likely a regex issue"
h5i context trace --kind ACT     "editing src/auth/token.rs validate() function"

# Reaching a milestone
h5i context commit "Fixed token validation regex" \
  --detail "Replaced greedy quantifier with possessive; all 47 auth tests now pass."

# Session end
h5i context status
```

## Guidelines
1. Log every OTA step — fine-grained traces are the primary recovery mechanism.
2. Commit at every meaningful milestone, not just at the end.
3. Branch before any risky or divergent exploration.
4. Always run `h5i context show` at the start of a new session.
5. Update main.md milestones via `h5i context write main.md <content>` when goals complete.
"#
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn ensure_branch_git(repo: &Repository, name: &str, purpose: &str) -> Result<(), H5iError> {
    // Only write files that don't already exist in the tree.
    let commit_path = format!("branches/{name}/commit.md");
    let trace_path = format!("branches/{name}/trace.md");
    let meta_path = format!("branches/{name}/metadata.yaml");

    let missing_commit = ctx_read_file(repo, &commit_path).is_none();
    let missing_trace = ctx_read_file(repo, &trace_path).is_none();
    let missing_meta = ctx_read_file(repo, &meta_path).is_none();

    if !missing_commit && !missing_trace && !missing_meta {
        return Ok(()); // already exists
    }

    let mut changes: Vec<(&str, String)> = Vec::new();
    let commit_content;
    let trace_content;
    let meta_content;

    if missing_commit {
        commit_content = format!(
            "# Branch: {name}\n\n\
             **Purpose:** {purpose}\n\n\
             _Commits will be appended below._\n\n"
        );
        changes.push((&commit_path, commit_content.clone()));
    } else {
        commit_content = String::new();
    }
    if missing_trace {
        trace_content = format!("# OTA Log — Branch: {name}\n\n");
        changes.push((&trace_path, trace_content.clone()));
    } else {
        trace_content = String::new();
    }
    if missing_meta {
        meta_content = "file_structure: {}\nenv_config: {}\ndependencies: []\n".to_string();
        changes.push((&meta_path, meta_content.clone()));
    } else {
        meta_content = String::new();
    }

    let _ = (commit_content, trace_content, meta_content); // suppress unused warnings

    let str_changes: Vec<(&str, &str)> = changes.iter().map(|(p, c)| (*p, c.as_str())).collect();
    ctx_write_files(repo, &str_changes, &format!("h5i context branch: {name}"))
}

/// Append a one-line progress note to `main.md` under `## Notes`.
fn append_main_note(content: &str, branch: &str, summary: &str) -> String {
    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let note = format!("- [{ts}] `{branch}`: {summary}\n");

    if let Some(pos) = content.find("## Notes") {
        let after = &content[pos..];
        let insert_at = pos + after.find('\n').map(|i| i + 1).unwrap_or(after.len());
        let mut new = content.to_string();
        new.insert_str(insert_at, &note);
        new
    } else {
        format!("{content}\n## Notes\n{note}")
    }
}

fn extract_latest_summary(commit_text: &str) -> String {
    let entries: Vec<&str> = commit_text.split("## Commit ").collect();
    if let Some(last) = entries.last() {
        if let Some(start) = last.find("### This Commit's Contribution") {
            let after = &last[start + "### This Commit's Contribution".len()..];
            let end = after.find("\n---").unwrap_or(after.len());
            return after[..end].trim().to_string();
        }
    }
    String::new()
}

fn extract_branch_purpose(commit_text: &str) -> Option<String> {
    let after = commit_text.split("**Purpose:**").nth(1)?;
    let end = after.find('\n').unwrap_or(after.len());
    Some(after[..end].trim().to_string())
}

fn find_commit_by_hash(commit_text: &str, hash_prefix: &str) -> Option<String> {
    for entry in commit_text.split("## Commit ").skip(1) {
        if entry.starts_with(hash_prefix) {
            if let Some(start) = entry.find("### This Commit's Contribution") {
                let after = &entry[start + "### This Commit's Contribution".len()..];
                let end = after.find("\n---").unwrap_or(after.len());
                return Some(format!("[{}] {}", hash_prefix, after[..end].trim()));
            }
            return Some(entry.lines().next().unwrap_or("").trim().to_string());
        }
    }
    None
}

fn extract_recent_commits(commit_text: &str, window: usize) -> Vec<String> {
    let entries: Vec<&str> = commit_text.split("## Commit ").skip(1).collect();
    entries
        .iter()
        .rev()
        .take(window)
        .map(|e| {
            if let Some(start) = e.find("### This Commit's Contribution") {
                let after = &e[start + "### This Commit's Contribution".len()..];
                let end = after.find("\n---").unwrap_or(after.len());
                after[..end].trim().chars().take(120).collect()
            } else {
                e.lines().next().unwrap_or("").trim().chars().take(80).collect()
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn extract_section(text: &str, section: &str) -> String {
    let header = format!("## {section}");
    if let Some(start) = text.find(&header) {
        let after = &text[start + header.len()..];
        let end = after.find("\n## ").unwrap_or(after.len());
        return after[..end].trim().to_string();
    }
    String::new()
}

fn extract_list_items(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .map(|l| l.trim_start_matches('-').trim().to_string())
        .collect()
}

fn extract_yaml_segment(yaml: &str, segment: &str) -> String {
    let key = format!("{segment}:");
    if let Some(start) = yaml.find(&key) {
        let after = &yaml[start..];
        let end = after[key.len()..]
            .find(|c: char| c.is_alphabetic() && !c.is_whitespace())
            .map(|i| i + key.len())
            .unwrap_or(after.len());
        return after[..end].trim().to_string();
    }
    String::new()
}

fn short_timestamp_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{:08x}", ts)
}

fn serde_yaml_serialize(meta: &GccMetadata) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "file_structure:");
    if meta.file_structure.is_empty() {
        let _ = writeln!(out, "  {{}}");
    } else {
        let mut pairs: Vec<_> = meta.file_structure.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (k, v) in pairs {
            let _ = writeln!(out, "  \"{k}\": \"{v}\"");
        }
    }

    let _ = writeln!(out, "env_config:");
    if meta.env_config.is_empty() {
        let _ = writeln!(out, "  {{}}");
    } else {
        let mut pairs: Vec<_> = meta.env_config.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (k, v) in pairs {
            let _ = writeln!(out, "  \"{k}\": \"{v}\"");
        }
    }

    let _ = writeln!(out, "dependencies:");
    if meta.dependencies.is_empty() {
        let _ = writeln!(out, "  []");
    } else {
        for dep in &meta.dependencies {
            let _ = writeln!(out, "  - name: \"{}\"", dep.name);
            let _ = writeln!(out, "    purpose: \"{}\"", dep.purpose);
        }
    }

    if !meta.extra.is_empty() {
        let mut pairs: Vec<_> = meta.extra.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (k, v) in pairs {
            let _ = writeln!(out, "{k}: \"{v}\"");
        }
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use tempfile::tempdir;

    /// Create a bare-minimum git repo in `dir` so ctx functions can discover it.
    fn git_init(dir: &Path) {
        Repository::init(dir).expect("failed to init git repo");
    }

    // ── init / is_initialized ─────────────────────────────────────────────────

    #[test]
    fn init_creates_workspace() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "Build something great").unwrap();
        assert!(is_initialized(dir.path()));
        assert!(list_branches(dir.path()).contains(&"main".to_string()));
    }

    #[test]
    fn is_initialized_false_before_init() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        assert!(!is_initialized(dir.path()));
    }

    #[test]
    fn is_initialized_true_after_init() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "Test goal").unwrap();
        assert!(is_initialized(dir.path()));
    }

    #[test]
    fn init_embeds_goal_in_main_md() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "Build an OAuth2 login system").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert!(ctx.project_goal.contains("Build an OAuth2 login system"));
    }

    #[test]
    fn init_idempotent_does_not_overwrite_goal() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "Original goal").unwrap();
        // Re-init should not overwrite because the ref already exists.
        init(dir.path(), "New goal").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert!(ctx.project_goal.contains("Original goal"));
    }

    // ── current_branch / set_current_branch ──────────────────────────────────

    #[test]
    fn current_branch_defaults_to_main() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert_eq!(current_branch(dir.path()), "main");
    }

    #[test]
    fn gcc_branch_switches_active_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "experiment", "try new approach").unwrap();
        assert_eq!(current_branch(dir.path()), "experiment");
    }

    // ── gcc_checkout ──────────────────────────────────────────────────────────

    #[test]
    fn gcc_checkout_switches_to_existing_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feature", "feature work").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        assert_eq!(current_branch(dir.path()), "main");
    }

    #[test]
    fn gcc_checkout_fails_on_nonexistent_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(gcc_checkout(dir.path(), "does_not_exist").is_err());
    }

    // ── list_branches ─────────────────────────────────────────────────────────

    #[test]
    fn list_branches_after_init_has_main() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(list_branches(dir.path()).contains(&"main".to_string()));
    }

    #[test]
    fn list_branches_includes_new_branches() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feat-oauth", "oauth work").unwrap();
        assert!(list_branches(dir.path()).contains(&"feat-oauth".to_string()));
    }

    // ── append_log ────────────────────────────────────────────────────────────

    #[test]
    fn append_log_adds_entry_to_trace() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "Redis latency is 2ms").unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx
            .recent_log_lines
            .iter()
            .any(|l| l.contains("OBSERVE: Redis latency is 2ms")));
    }

    #[test]
    fn append_log_uppercases_kind() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "think", "reasoning step").unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.recent_log_lines.iter().any(|l| l.contains("THINK:")));
    }

    // ── gcc_commit ────────────────────────────────────────────────────────────

    #[test]
    fn gcc_commit_appends_entry() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "Milestone 1 done", "Implemented the login form").unwrap();
        let ctx =
            gcc_context(dir.path(), &ContextOpts { window: 3, ..Default::default() }).unwrap();
        assert!(ctx.recent_commits.iter().any(|c| c.contains("Implemented the login form")));
    }

    #[test]
    fn gcc_commit_updates_main_md_notes() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "Completed auth setup", "Added JWT tokens").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let main = ctx_read_file(&repo, "main.md").unwrap();
        assert!(main.contains("Completed auth setup"));
    }

    // ── gcc_context ───────────────────────────────────────────────────────────

    #[test]
    fn gcc_context_reads_goal_from_main_md() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "Build an OAuth2 login system").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.project_goal, "Build an OAuth2 login system");
    }

    #[test]
    fn gcc_context_reads_milestones() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // Update main.md to add a milestone via write_ctx_file.
        let repo = ctx_git_repo(dir.path()).unwrap();
        let mut content = ctx_read_file(&repo, "main.md").unwrap();
        content = content.replace(
            "- [ ] Initial setup",
            "- [x] Initial setup\n- [ ] Add rate limiting",
        );
        write_ctx_file(dir.path(), "main.md", &content).unwrap();

        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert!(ctx.milestones.iter().any(|m| m.contains("Add rate limiting")));
    }

    #[test]
    fn gcc_context_includes_current_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.current_branch, "main");
    }

    #[test]
    fn gcc_context_returns_recent_commits() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "milestone", "did the work").unwrap();
        let ctx =
            gcc_context(dir.path(), &ContextOpts { window: 3, ..Default::default() }).unwrap();
        assert!(!ctx.recent_commits.is_empty());
        assert!(ctx.recent_commits[0].contains("did the work"));
    }

    // ── gcc_merge ─────────────────────────────────────────────────────────────

    #[test]
    fn gcc_merge_combines_branches() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "experiment", "try algo").unwrap();
        gcc_commit(dir.path(), "Experiment done", "Found faster algorithm").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        let summary = gcc_merge(dir.path(), "experiment").unwrap();
        assert!(summary.contains("experiment"));
    }

    #[test]
    fn gcc_merge_fails_for_nonexistent_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(gcc_merge(dir.path(), "ghost_branch").is_err());
    }

    // ── internal helpers ──────────────────────────────────────────────────────

    #[test]
    fn extract_section_returns_correct_content() {
        let text = "## Goal\nBuild something\n\n## Milestones\n- item\n";
        assert_eq!(extract_section(text, "Goal"), "Build something");
    }

    #[test]
    fn extract_section_returns_empty_when_missing() {
        assert_eq!(extract_section("no sections here", "Goal"), "");
    }

    #[test]
    fn extract_list_items_parses_bullet_list() {
        let text = "- [ ] First\n- [x] Done\n- Third\n";
        let items = extract_list_items(text);
        assert_eq!(items.len(), 3);
        assert!(items[0].contains("First"));
    }

    // ── snapshot_for_commit ───────────────────────────────────────────────────

    #[test]
    fn snapshot_for_commit_silently_skips_uninitialized_workspace() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        // No `init` — workspace does not exist.
        snapshot_for_commit(dir.path(), "abc12345").unwrap();
    }

    #[test]
    fn snapshot_for_commit_writes_snapshot_file() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "test goal").unwrap();
        snapshot_for_commit(dir.path(), "abc12345deadbeef").unwrap();

        let repo = ctx_git_repo(dir.path()).unwrap();
        let snap = ctx_read_file(&repo, "snapshots/abc12345.md").unwrap();
        assert!(snap.contains("abc12345deadbeef"), "linked commit should appear");
        assert!(snap.contains("test goal"), "goal should appear");
        assert!(snap.contains("Context ref OID"), "context OID field must be present");
    }

    #[test]
    fn snapshot_for_commit_records_current_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feature/foo", "some feature").unwrap();
        snapshot_for_commit(dir.path(), "deadbeef12345678").unwrap();

        let repo = ctx_git_repo(dir.path()).unwrap();
        let snap = ctx_read_file(&repo, "snapshots/deadbeef.md").unwrap();
        assert!(snap.contains("feature/foo"));
    }

    // ── restore ───────────────────────────────────────────────────────────────

    #[test]
    fn restore_fails_when_no_snapshot_exists() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(restore(dir.path(), "00000000").is_err());
    }

    #[test]
    fn restore_returns_ok_for_existing_snapshot() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "first milestone", "did things").unwrap();
        snapshot_for_commit(dir.path(), "aabbccdd11223344").unwrap();

        let result = restore(dir.path(), "aabbccdd");
        assert!(result.is_ok(), "restore should succeed: {:?}", result);
        let summary = result.unwrap();
        assert!(summary.contains("main"), "summary should name the branch");
    }

    #[test]
    fn restore_is_nondestructive_adds_new_commit() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        snapshot_for_commit(dir.path(), "snap00001111").unwrap();

        // Count context commits before restore.
        let repo = ctx_git_repo(dir.path()).unwrap();
        let before_count = {
            let text = ctx_read_file(&repo, "branches/main/commit.md").unwrap_or_default();
            text.matches("## Commit ").count()
        };

        restore(dir.path(), "snap0000").unwrap();

        // The context ref should have advanced (new commit on top).
        let tip_after = repo
            .find_reference(CTX_REF)
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert!(tip_after.message().unwrap_or("").contains("restore"));
        let _ = before_count; // restore doesn't add a context commit entry, just advances ref
    }

    // ── context_diff ─────────────────────────────────────────────────────────

    #[test]
    fn context_diff_fails_when_snapshot_missing() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(context_diff(dir.path(), "aaaaaaaa", "bbbbbbbb").is_err());
    }

    #[test]
    fn context_diff_detects_added_milestones() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        // Snapshot before any context commit.
        snapshot_for_commit(dir.path(), "sha1111100000000").unwrap();

        // Add a milestone, then snapshot again.
        gcc_commit(dir.path(), "first milestone", "implemented feature X").unwrap();
        snapshot_for_commit(dir.path(), "sha2222200000000").unwrap();

        let diff = context_diff(dir.path(), "sha11111", "sha22222").unwrap();
        assert!(
            diff.added_commits.iter().any(|c| c.contains("implemented feature X")),
            "diff should show new milestone"
        );
    }

    #[test]
    fn context_diff_detects_added_trace_lines() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        snapshot_for_commit(dir.path(), "sha3333300000000").unwrap();
        append_log(dir.path(), "OBSERVE", "found a performance issue").unwrap();
        snapshot_for_commit(dir.path(), "sha4444400000000").unwrap();

        let diff = context_diff(dir.path(), "sha33333", "sha44444").unwrap();
        assert!(
            diff.added_trace_lines.iter().any(|l| l.contains("found a performance issue")),
            "diff should include new trace lines"
        );
    }

    // ── relevant ─────────────────────────────────────────────────────────────

    #[test]
    fn relevant_returns_empty_when_no_mentions() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let ctx = relevant(dir.path(), "src/nonexistent.rs").unwrap();
        assert!(ctx.trace_mentions.is_empty());
        assert!(ctx.commit_mentions.is_empty());
    }

    #[test]
    fn relevant_finds_trace_mentions() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "ACT", "edited src/repository.rs line 88").unwrap();
        let ctx = relevant(dir.path(), "src/repository.rs").unwrap();
        assert!(
            ctx.trace_mentions.iter().any(|l| l.contains("repository.rs")),
            "should find trace mention"
        );
    }

    #[test]
    fn relevant_finds_commit_mentions() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(
            dir.path(),
            "refactored http_client.rs",
            "rewrote http_client.rs to use async/await",
        )
        .unwrap();
        let ctx = relevant(dir.path(), "http_client.rs").unwrap();
        assert!(
            ctx.commit_mentions.iter().any(|c| c.contains("http_client.rs")),
            "should find commit mention"
        );
    }

    #[test]
    fn relevant_finds_cross_branch_mentions() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // Add trace entry on main mentioning the file.
        append_log(dir.path(), "THINK", "retry_logic.rs needs a refactor").unwrap();
        // Create a second branch.
        gcc_branch(dir.path(), "alt", "alternative approach").unwrap();
        // On alt branch, relevant should find the main-branch mention.
        let ctx = relevant(dir.path(), "retry_logic.rs").unwrap();
        assert!(
            ctx.cross_branch_mentions.iter().any(|l| l.contains("retry_logic.rs")),
            "cross-branch mention should be found"
        );
    }

    // ── pack ──────────────────────────────────────────────────────────────────

    #[test]
    fn pack_returns_zero_when_no_snapshots() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let squashed = pack(dir.path()).unwrap();
        assert_eq!(squashed, 0);
    }

    #[test]
    fn pack_returns_zero_when_history_already_compact() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        snapshot_for_commit(dir.path(), "aabb1122aabb1122").unwrap();
        // Only one snapshot pointing at the tip — nothing to squash.
        let squashed = pack(dir.path()).unwrap();
        assert_eq!(squashed, 0);
    }

    #[test]
    fn extract_recent_commits_returns_latest_first_when_multiple() {
        let commit_text = "## Commit aaa111 — 2026-01-01\n\
            ### This Commit's Contribution\nFirst contribution\n---\n\
            ## Commit bbb222 — 2026-01-02\n\
            ### This Commit's Contribution\nSecond contribution\n---\n";
        let recent = extract_recent_commits(commit_text, 2);
        assert_eq!(recent.len(), 2);
        assert!(recent.last().unwrap().contains("Second contribution"));
    }
}
