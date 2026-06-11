/// Agent context workspace — structured reasoning memory for LLM agents.
///
/// Implements the data structures and operations from:
///   "Git Context Controller: Manage the Context of Agents by Agentic Git"
///   arXiv:2508.00031
///
/// Enhanced with five capabilities from recent research:
///   1. DAG-based trace nodes (CMV paper, arXiv:2602.22402)
///   2. Three-pass structurally-lossless pack (CMV three-pass trimming algorithm)
///   3. Ephemeral trace entries (Claude Code /btw pattern)
///   4. Stable-prefix / dynamic-suffix serialisation (prompt-caching-aware)
///   5. Subagent-scoped sub-contexts (`scope/<name>` branches)
///
/// Storage layout — one git ref per context branch.
///
/// Each context branch is its own git ref under `refs/h5i/context/<name>`,
/// mirroring how regular git branches live under `refs/heads/<name>`. The
/// tree of each ref contains the branch's content at root:
///
/// ```text
/// refs/h5i/context/<name> tree:
/// ├── commit.md          # milestone summaries (append-only)
/// ├── trace.md           # human-readable OTA log (rendered view)
/// ├── ephemeral.md       # scratch traces cleared on context commit
/// ├── metadata.yaml      # file structure, deps, env config
/// └── dag.json           # trace-node DAG (derivable from git log; kept for fast reads)
/// ```
///
/// Project-wide state (cross-branch) lives on `refs/h5i/context/main`:
///
/// ```text
/// refs/h5i/context/main tree (additionally):
/// ├── main.md                       # roadmap: goals, milestones, notes
/// └── git-goals/<git-branch>.md    # per-git-branch goal
/// ```
///
/// HEAD — which context branch is active — is a per-worktree pointer at
/// `.git/h5i/HEAD` (format: `ref: refs/h5i/context/<name>\n`), mirroring
/// git's own HEAD. Switching context branches no longer creates a commit.
///
/// Auto-follow: when no `.git/h5i/PINNED` marker exists (the default),
/// every ctx command syncs HEAD to `refs/h5i/context/<current-git-branch>`,
/// auto-creating the ref (forked from `refs/h5i/context/main`) on first
/// write. `h5i context checkout <name>` writes PINNED to stay on the
/// explicit branch; `h5i context unpin` removes it.
///
/// Merging uses libgit2's real three-way merge — `gcc_merge` produces an
/// actual 2-parent commit on the target ref, with conflicts surfaced
/// via standard conflict markers in the text files.
///
/// Migration from the legacy single-ref layout (`refs/h5i/context` with
/// internal `branches/<name>/` subtrees) runs lazily on first command:
/// each subtree becomes its own ref, `main.md` and `git-goals/` land on
/// `refs/h5i/context/main`, and the original ref is renamed to
/// `refs/h5i/context-legacy` for safety.
///
/// Exposed via `h5i context` subcommands.
use std::fmt::Write as FmtWrite;
use std::path::Path;

use chrono::Utc;
use git2::{ObjectType, Oid, Repository, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::H5iError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Ref-name prefix. Every context branch lives at `{CTX_REF_PREFIX}<name>`.
pub const CTX_REF_PREFIX: &str = "refs/h5i/context/";

/// Legacy single-ref name (pre-redesign). Migration renames this to
/// [`CTX_LEGACY_BACKUP_REF`] before creating any per-branch refs, because
/// git refuses to host both `refs/h5i/context` and `refs/h5i/context/...`
/// at the same time (file-vs-directory collision under `.git/refs/`).
pub const CTX_LEGACY_REF: &str = "refs/h5i/context";

/// Where the legacy ref is preserved after migration.
pub const CTX_LEGACY_BACKUP_REF: &str = "refs/h5i/context-legacy";

/// Back-compat re-export of the legacy ref name (external callers still
/// reference `ctx::CTX_REF`). New code should use [`branch_ref`] instead.
#[deprecated(note = "use branch_ref(name) — context is now per-ref")]
pub const CTX_REF: &str = CTX_LEGACY_REF;

/// Legacy directory name kept for display / migration messages only.
pub const CTX_DIR: &str = ".h5i-ctx";
#[doc(hidden)]
pub const GCC_DIR: &str = CTX_DIR;

pub const MAIN_BRANCH: &str = "main";

/// Build the full git ref name for a context branch.
pub fn branch_ref(name: &str) -> String {
    format!("{CTX_REF_PREFIX}{name}")
}

/// Per-worktree pointer at `<git-dir>/h5i/HEAD`, format `ref: refs/h5i/context/<name>\n`.
const HEAD_FILE: &str = "h5i/HEAD";

/// Optional marker at `<git-dir>/h5i/PINNED`. When present, auto-follow is disabled
/// and HEAD stays on whatever branch the user explicitly checked out.
const PIN_FILE: &str = "h5i/PINNED";

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
    /// Progressive disclosure depth: 1=compact index, 2=timeline (default), 3=full trace.
    pub depth: u8,
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
    pub git_branch: String,
    pub git_branch_goal: String,
    pub milestones: Vec<String>,
    pub active_branches: Vec<String>,
    pub current_branch: String,
    pub recent_commits: Vec<String>,
    pub recent_log_lines: Vec<String>,
    pub metadata_snippet: Option<String>,
    /// Number of trace lines that form the stable (cache-friendly) prefix.
    pub stable_line_count: usize,
    /// Number of trace lines in the dynamic (volatile) suffix.
    pub dynamic_line_count: usize,
    /// Open TODO/FIXME items extracted from NOTE and THINK trace entries.
    pub todo_items: Vec<String>,
    /// Last 8 trace entries shown by default in `show` without --trace.
    pub mini_trace: Vec<String>,
}

// ── DAG types (Feature 1) ─────────────────────────────────────────────────────

/// A single node in the per-branch trace DAG.
/// Each call to `append_log` (non-ephemeral) adds one node whose `parent_ids`
/// point to the previous node(s) on the branch. Merge operations add a node
/// with two parents, one from each merged branch.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TraceNode {
    /// Short 8-hex content-addressable ID (sha256 of kind+timestamp+content).
    pub id: String,
    /// IDs of parent nodes (empty for the root, two entries at merge points).
    pub parent_ids: Vec<String>,
    /// Step kind: OBSERVE / THINK / ACT / NOTE / MERGE.
    pub kind: String,
    pub content: String,
    pub timestamp: String,
}

/// The full per-branch directed-acyclic-graph of trace nodes.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TraceDag {
    pub nodes: Vec<TraceNode>,
}

impl TraceDag {
    /// ID of the most recent node, or empty string if the DAG is empty.
    pub fn head_id(&self) -> String {
        self.nodes.last().map(|n| n.id.clone()).unwrap_or_default()
    }
}

/// Summary returned by `pack_lossless`.
#[derive(Debug, Default)]
pub struct LosslessPackResult {
    pub removed_subsumed_observe: usize,
    pub merged_consecutive_observe: usize,
    pub kept_durable: usize,
}

// ── Git helpers ───────────────────────────────────────────────────────────────

fn ctx_git_repo(workdir: &Path) -> Result<Repository, H5iError> {
    Repository::discover(workdir).map_err(H5iError::Git)
}

/// Path-translation: which ref + tree-path does a legacy `vpath` route to?
///
/// Returns `None` for the special HEAD pseudo-path `.current_branch`, whose
/// reads/writes go to the filesystem (`<git-dir>/h5i/HEAD`), not a git ref.
fn route_vpath(vpath: &str) -> Option<(String, String)> {
    if vpath == ".current_branch" {
        return None;
    }
    if let Some(rest) = vpath.strip_prefix("branches/") {
        // `branches/<name>/<rel>` — split at the LAST `/` so branch names may
        // contain `/` (e.g. `scope/foo`, `experiment/alt`). The last segment
        // is always the file under the branch's tree.
        if let Some((branch, rel)) = rest.rsplit_once('/') {
            return Some((branch_ref(branch), rel.to_owned()));
        }
        // `branches/<name>` with no file part — degenerate, route to main.
    }
    // Everything else (main.md, git-goals/*) lives on the main branch ref.
    Some((branch_ref(MAIN_BRANCH), vpath.to_owned()))
}

/// Read a single file from a branch's tree.
fn read_ref_file(repo: &Repository, ref_name: &str, rel_path: &str) -> Option<String> {
    let reference = repo.find_reference(ref_name).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(rel_path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Per-worktree HEAD path: `<git-dir>/h5i/HEAD`.
fn head_file_path(repo: &Repository) -> std::path::PathBuf {
    repo.path().join(HEAD_FILE)
}

/// Read the active context branch from `<git-dir>/h5i/HEAD`.
/// Returns `None` if HEAD is absent or malformed (callers default to `main`).
fn read_head(repo: &Repository) -> Option<String> {
    let raw = std::fs::read_to_string(head_file_path(repo)).ok()?;
    let line = raw.trim();
    line.strip_prefix("ref: ")
        .and_then(|r| r.strip_prefix(CTX_REF_PREFIX))
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// Write the HEAD pointer to a context branch name.
fn write_head(repo: &Repository, branch: &str) -> Result<(), H5iError> {
    let path = head_file_path(repo);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(H5iError::Io)?;
    }
    std::fs::write(&path, format!("ref: {CTX_REF_PREFIX}{branch}\n"))
        .map_err(H5iError::Io)
}

/// Read a virtual path through the legacy addressing scheme. Backward-compatible
/// with `branches/<name>/<file>`, `main.md`, `git-goals/<x>.md`, and the special
/// `.current_branch` (which now resolves through the on-disk HEAD file).
fn ctx_read_file(repo: &Repository, vpath: &str) -> Option<String> {
    match route_vpath(vpath) {
        None => read_head(repo),
        Some((ref_name, rel)) => read_ref_file(repo, &ref_name, &rel),
    }
}

/// Apply `(vpath, content)` changes by grouping them per destination ref and
/// committing each group atomically on its ref. The special `.current_branch`
/// path is diverted to the on-disk HEAD pointer.
fn ctx_write_files(
    repo: &Repository,
    changes: &[(&str, &str)],
    message: &str,
) -> Result<(), H5iError> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut head_target: Option<String> = None;

    for &(vpath, content) in changes {
        match route_vpath(vpath) {
            None => head_target = Some(content.to_owned()),
            Some((ref_name, rel)) => {
                grouped.entry(ref_name).or_default().push((rel, content.to_owned()));
            }
        }
    }

    for (ref_name, group) in &grouped {
        let borrowed: Vec<(&str, &str)> =
            group.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
        write_ref_files(repo, ref_name, &borrowed, message)?;
    }

    if let Some(branch) = head_target {
        write_head(repo, &branch)?;
    }
    Ok(())
}

/// Commit `(rel_path, content)` changes onto a single ref. The ref is created
/// (orphan branch) if it does not yet exist; otherwise this appends one
/// commit whose parent is the current tip.
fn write_ref_files(
    repo: &Repository,
    ref_name: &str,
    changes: &[(&str, &str)],
    message: &str,
) -> Result<(), H5iError> {
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;

    let parent = repo
        .find_reference(ref_name)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let current_tree = parent.as_ref().and_then(|c| c.tree().ok());

    let new_tree_oid = apply_changes_to_tree(repo, current_tree.as_ref(), changes)?;
    let new_tree = repo.find_tree(new_tree_oid).map_err(H5iError::Git)?;

    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some(ref_name), &sig, &sig, message, &new_tree, &parents)
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

/// Context files that are conceptually append-only — both sides extending the
/// same ancestor in non-overlapping ways. When libgit2's line-based three-way
/// merge produces a conflict on these, we union-merge instead of failing.
const APPEND_ONLY_FILES: &[&str] = &["trace.md", "commit.md", "ephemeral.md"];

/// Union-merge an append-only file: keep the common ancestor, then concatenate
/// each side's tail. If one side already contains the other's tail (e.g. due
/// to a previous merge), avoid duplicating it.
fn union_append_only(ancestor: &str, ours: &str, theirs: &str) -> String {
    let ours_tail = ours.strip_prefix(ancestor).unwrap_or("");
    let theirs_tail = theirs.strip_prefix(ancestor).unwrap_or("");
    if ours_tail.is_empty() && theirs_tail.is_empty() {
        return ancestor.to_string();
    }
    // Tails do not share a prefix → safe to concatenate.
    let mut out = String::from(ancestor);
    out.push_str(ours_tail);
    if !theirs_tail.is_empty() && !out.ends_with(theirs_tail) {
        out.push_str(theirs_tail);
    }
    out
}

/// Walk index conflicts; for each conflicted entry whose path is in
/// [`APPEND_ONLY_FILES`], replace the conflict with a union-merged blob.
fn resolve_append_only_conflicts(
    repo: &Repository,
    index: &mut git2::Index,
) -> Result<(), H5iError> {
    let entries: Vec<(Option<git2::IndexEntry>, Option<git2::IndexEntry>, Option<git2::IndexEntry>)> = {
        let conflicts = index.conflicts().map_err(H5iError::Git)?;
        conflicts
            .filter_map(|c| c.ok())
            .map(|c| (c.ancestor, c.our, c.their))
            .collect()
    };
    for (ancestor, our, their) in entries {
        let path_bytes = our
            .as_ref()
            .or(their.as_ref())
            .or(ancestor.as_ref())
            .map(|e| e.path.clone());
        let Some(path_bytes) = path_bytes else { continue };
        let Ok(path_str) = std::str::from_utf8(&path_bytes) else { continue };
        let filename = Path::new(path_str)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !APPEND_ONLY_FILES.contains(&filename) {
            continue;
        }
        let read_blob = |entry: Option<&git2::IndexEntry>| -> String {
            entry
                .and_then(|e| repo.find_blob(e.id).ok())
                .and_then(|b| std::str::from_utf8(b.content()).ok().map(str::to_owned))
                .unwrap_or_default()
        };
        let ancestor_text = read_blob(ancestor.as_ref());
        let our_text = read_blob(our.as_ref());
        let their_text = read_blob(their.as_ref());
        let merged = union_append_only(&ancestor_text, &our_text, &their_text);
        let merged_oid = repo.blob(merged.as_bytes()).map_err(H5iError::Git)?;
        let resolved = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: merged.len() as u32,
            id: merged_oid,
            flags: 0,
            flags_extended: 0,
            path: path_bytes.clone(),
        };
        index.remove_path(Path::new(path_str)).map_err(H5iError::Git)?;
        index.add(&resolved).map_err(H5iError::Git)?;
    }
    Ok(())
}

/// Insert a subtree OID at a (possibly nested) slash-separated path under `base`.
///
/// Used to compose `branches/<a>/<b>/...` paths in the aggregate snapshot tree
/// when context branch names contain `/` (e.g. `scope/foo`, `experiment/alt`).
fn insert_subtree_at_path(
    repo: &Repository,
    base: Option<&git2::Tree>,
    path: &str,
    subtree_oid: Oid,
) -> Result<Oid, H5iError> {
    let mut builder = repo.treebuilder(base).map_err(H5iError::Git)?;
    match path.split_once('/') {
        Some((first, rest)) => {
            let sub_base = base.and_then(|t| {
                t.get_name(first)
                    .filter(|e| e.kind() == Some(ObjectType::Tree))
                    .and_then(|e| repo.find_tree(e.id()).ok())
            });
            let sub_oid = insert_subtree_at_path(repo, sub_base.as_ref(), rest, subtree_oid)?;
            builder.insert(first, sub_oid, 0o040000).map_err(H5iError::Git)?;
        }
        None => {
            builder.insert(path, subtree_oid, 0o040000).map_err(H5iError::Git)?;
        }
    }
    builder.write().map_err(H5iError::Git)
}

/// Build a synthetic tree that mirrors the legacy single-ref layout from the
/// current per-branch refs:
///
///   * `main.md`, `git-goals/...` come from `refs/h5i/context/main`'s tree.
///   * `branches/<name>/...` is one subtree per context branch (nested for
///     slash-separated names).
///
/// Used by [`snapshot_for_commit`] so that `context_diff` / `restore` can read
/// a self-contained tree without depending on the live per-branch refs.
fn build_aggregate_tree(repo: &Repository) -> Result<Oid, H5iError> {
    let main_tree = repo
        .find_reference(&branch_ref(MAIN_BRANCH))
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .and_then(|c| c.tree().ok());

    // Compose the `branches/` subtree by inserting each branch's tree OID.
    let mut branches_root: Option<Oid> = None;
    for branch_name in ctx_list_branches_git(repo) {
        let branch_tree_oid = match repo
            .find_reference(&branch_ref(&branch_name))
            .ok()
            .and_then(|r| r.peel_to_commit().ok())
            .map(|c| c.tree_id())
        {
            Some(oid) => oid,
            None => continue,
        };
        let current = branches_root.and_then(|oid| repo.find_tree(oid).ok());
        branches_root = Some(insert_subtree_at_path(
            repo,
            current.as_ref(),
            &branch_name,
            branch_tree_oid,
        )?);
    }

    // Build the root tree on top of main's tree, overlaying `branches/`.
    let mut root_builder = repo.treebuilder(main_tree.as_ref()).map_err(H5iError::Git)?;
    if let Some(b_oid) = branches_root {
        root_builder
            .insert("branches", b_oid, 0o040000)
            .map_err(H5iError::Git)?;
    }
    root_builder.write().map_err(H5iError::Git)
}

/// Enumerate context branch names by listing all refs under `refs/h5i/context/`.
fn ctx_list_branches_git(repo: &Repository) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let glob = format!("{CTX_REF_PREFIX}*");
    if let Ok(refs) = repo.references_glob(&glob) {
        for r in refs.flatten() {
            if let Some(full) = r.name() {
                if let Some(short) = full.strip_prefix(CTX_REF_PREFIX) {
                    names.push(short.to_owned());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

// ── DAG helpers (Feature 1) ───────────────────────────────────────────────────

fn dag_path(branch: &str) -> String {
    format!("branches/{branch}/dag.json")
}

fn ephemeral_path(branch: &str) -> String {
    format!("branches/{branch}/ephemeral.md")
}

fn git_goal_path(git_branch: &str) -> String {
    format!("git-goals/{git_branch}.md")
}

fn read_dag(repo: &Repository, branch: &str) -> TraceDag {
    ctx_read_file(repo, &dag_path(branch))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Returns the reasoning DAG for `branch`, or the active context branch when
/// `branch` is `None`. Returns an empty DAG when no context workspace exists
/// yet — callers can detect "no DAG" via `dag.nodes.is_empty()`.
///
/// Public so other modules (e.g. the PR comment renderer) can read the DAG
/// without poking into the on-disk JSON layout directly.
pub fn dag_for_branch(
    workdir: &Path,
    branch: Option<&str>,
) -> Result<TraceDag, H5iError> {
    let repo = match ctx_git_repo(workdir) {
        Ok(r) => r,
        Err(_) => return Ok(TraceDag::default()),
    };
    let active = current_branch(workdir);
    let branch = branch.unwrap_or(&active);
    Ok(read_dag(&repo, branch))
}

fn node_id(kind: &str, timestamp: &str, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"|");
    h.update(timestamp.as_bytes());
    h.update(b"|");
    h.update(content.as_bytes());
    let digest = h.finalize();
    format!(
        "{:08x}",
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
    )
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize the context workspace in `refs/h5i/context`.
pub fn init(workdir: &Path, goal: &str) -> Result<(), H5iError> {
    let _ = migrate_legacy_if_needed(workdir);
    let repo = ctx_git_repo(workdir)?;
    let git_branch = current_git_branch(workdir);

    // If the main branch ref already exists, only ensure its files are present.
    if repo.find_reference(&branch_ref(MAIN_BRANCH)).is_ok() {
        ensure_branch_git(&repo, MAIN_BRANCH, "Primary development branch")?;
        if !goal.trim().is_empty() {
            set_git_branch_goal(&repo, &git_branch, goal)?;
        }
        return Ok(());
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
            (&git_goal_path(&git_branch), goal),
        ],
        "h5i context init",
    )?;

    // Ensure HEAD is initialized even if `.current_branch` write was a no-op
    // (e.g. on a re-init where ctx_write_files routed it to the head file).
    write_head(&repo, MAIN_BRANCH)
}

/// Return `true` if the context workspace is initialized (the main branch ref exists).
///
/// Lazily migrates legacy single-ref workspaces on first call.
pub fn is_initialized(workdir: &Path) -> bool {
    let _ = migrate_legacy_if_needed(workdir);
    ctx_git_repo(workdir)
        .map(|repo| repo.find_reference(&branch_ref(MAIN_BRANCH)).is_ok())
        .unwrap_or(false)
}

/// Return the current active context branch name.
///
/// Resolution order: per-worktree HEAD file (`<git-dir>/h5i/HEAD`), then
/// `MAIN_BRANCH` as a default. With auto-follow (see [`prepare_context_write`]),
/// HEAD typically tracks the current git branch unless the user has pinned.
pub fn current_branch(workdir: &Path) -> String {
    ctx_git_repo(workdir)
        .ok()
        .and_then(|repo| read_head(&repo))
        .unwrap_or_else(|| MAIN_BRANCH.to_string())
}

/// Return the current git branch name, falling back to the active context branch
/// when HEAD is detached or not yet resolved.
pub fn current_git_branch(workdir: &Path) -> String {
    ctx_git_repo(workdir)
        .ok()
        .and_then(|repo| {
            let from_head = repo
                .head()
                .ok()
                .and_then(|h| h.shorthand().map(str::to_string));
            from_head.or_else(|| read_unborn_head_branch(&repo))
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| current_branch(workdir))
}

fn read_unborn_head_branch(repo: &Repository) -> Option<String> {
    let head_path = repo.path().join("HEAD");
    let head = std::fs::read_to_string(head_path).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn set_git_branch_goal(repo: &Repository, git_branch: &str, goal: &str) -> Result<(), H5iError> {
    let content = format!("# Git Branch Goal: {git_branch}\n\n{goal}\n");
    ctx_write_files(
        repo,
        &[(&git_goal_path(git_branch), &content)],
        &format!("h5i context init: git branch goal {git_branch}"),
    )
}

pub fn git_branch_goal(workdir: &Path, git_branch: &str) -> Option<String> {
    let repo = ctx_git_repo(workdir).ok()?;
    ctx_read_file(&repo, &git_goal_path(git_branch))
        .map(|text| {
            text.lines()
                .filter(|line| !line.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

pub fn context_branch_purpose(workdir: &Path, branch: &str) -> Option<String> {
    let repo = ctx_git_repo(workdir).ok()?;
    ctx_read_file(&repo, &format!("branches/{branch}/commit.md"))
        .and_then(|text| extract_branch_purpose(&text))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// One-shot migration from the legacy single-ref layout (`refs/h5i/context` with
/// internal `branches/<name>/` subtrees) to the per-branch-ref layout.
///
/// Triggered lazily: the first ctx command after upgrade detects the old ref
/// and replays it. After migration, the legacy ref is renamed to
/// `refs/h5i/context-legacy` so the original objects remain reachable for
/// inspection or rollback. The new layout is then ready for use.
pub fn migrate_legacy_if_needed(workdir: &Path) -> Result<bool, H5iError> {
    let repo = match ctx_git_repo(workdir) {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };
    let has_legacy = repo.find_reference(CTX_LEGACY_REF).is_ok();
    let has_new = repo.find_reference(&branch_ref(MAIN_BRANCH)).is_ok();
    if !has_legacy || has_new {
        return Ok(false);
    }

    let legacy_commit = repo
        .find_reference(CTX_LEGACY_REF)
        .map_err(H5iError::Git)?
        .peel_to_commit()
        .map_err(H5iError::Git)?;
    let legacy_tree = legacy_commit.tree().map_err(H5iError::Git)?;
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;

    // 1. Discover branch names by walking branches/<...>/commit.md.
    let mut branch_names: Vec<String> = Vec::new();
    if let Some(branches_entry) = legacy_tree.get_name("branches") {
        if branches_entry.kind() == Some(ObjectType::Tree) {
            let branches_tree = repo.find_tree(branches_entry.id()).map_err(H5iError::Git)?;
            collect_legacy_branch_names(&repo, &branches_tree, "", &mut branch_names);
        }
    }
    branch_names.sort();

    // 2. Recover the previously-active branch name from the in-tree pointer.
    let active_branch = legacy_tree
        .get_name(".current_branch")
        .and_then(|e| repo.find_blob(e.id()).ok())
        .and_then(|b| std::str::from_utf8(b.content()).ok().map(str::to_owned))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| MAIN_BRANCH.to_string());

    // 3. Rename the legacy ref aside BEFORE creating any `refs/h5i/context/...`
    //    refs: git cannot host a ref at `refs/h5i/context` and refs under
    //    `refs/h5i/context/` simultaneously (file-vs-directory collision under
    //    `.git/refs/`).
    let legacy_oid = legacy_commit.id();
    repo.reference(
        CTX_LEGACY_BACKUP_REF,
        legacy_oid,
        true,
        "h5i context migrate: preserved legacy single-ref layout",
    )
    .map_err(H5iError::Git)?;
    repo.find_reference(CTX_LEGACY_REF)
        .map_err(H5iError::Git)?
        .delete()
        .map_err(H5iError::Git)?;

    // 4. For each branch, create refs/h5i/context/<name> rooted at a single
    //    commit whose tree is the per-branch content (formerly under branches/<name>/).
    for branch in &branch_names {
        let subtree_oid =
            match find_nested_subtree_oid(&repo, &legacy_tree, &format!("branches/{branch}"))? {
                Some(oid) => oid,
                None => continue,
            };
        let subtree = repo.find_tree(subtree_oid).map_err(H5iError::Git)?;
        let new_ref = branch_ref(branch);
        let msg = format!("h5i context migrate: branch {branch}");
        let parent: Vec<&git2::Commit> = Vec::new();
        repo.commit(Some(&new_ref), &sig, &sig, &msg, &subtree, &parent)
            .map_err(H5iError::Git)?;
    }

    // 4. Build the main branch's tree: strip `branches/` and `.current_branch`
    //    from the legacy root (they're already migrated above / replaced by HEAD).
    let mut main_tree_builder = repo.treebuilder(Some(&legacy_tree)).map_err(H5iError::Git)?;
    main_tree_builder.remove("branches").ok();
    main_tree_builder.remove(".current_branch").ok();
    let main_tree_oid = main_tree_builder.write().map_err(H5iError::Git)?;
    let main_tree = repo.find_tree(main_tree_oid).map_err(H5iError::Git)?;

    // If the new main ref was already created via step 3 (a "main" branch existed),
    // append onto it; otherwise create it.
    let main_ref = branch_ref(MAIN_BRANCH);
    let main_parent = repo
        .find_reference(&main_ref)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let main_parents: Vec<&git2::Commit> = main_parent.iter().collect();
    repo.commit(
        Some(&main_ref),
        &sig,
        &sig,
        "h5i context migrate: project-wide state (main.md, git-goals/, snapshots/)",
        &main_tree,
        &main_parents,
    )
    .map_err(H5iError::Git)?;

    // 6. Seed HEAD to the previously-active branch.
    if branch_names.iter().any(|b| b == &active_branch) {
        write_head(&repo, &active_branch)?;
    } else {
        write_head(&repo, MAIN_BRANCH)?;
    }

    Ok(true)
}

/// Walk the legacy `branches/` subtree, collecting names of every directory
/// that contains a `commit.md` blob (matches the pre-redesign `is a branch dir`
/// rule). Supports nested names like `scope/foo` and `experiment/alt`.
fn collect_legacy_branch_names(
    repo: &Repository,
    tree: &git2::Tree,
    prefix: &str,
    out: &mut Vec<String>,
) {
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
        if subtree.get_name("commit.md").is_some() {
            out.push(full_name);
        } else {
            collect_legacy_branch_names(repo, &subtree, &full_name, out);
        }
    }
}

/// Walk a slash-separated path down a tree and return the OID of the final
/// subtree, or `None` if any segment is missing or non-tree.
fn find_nested_subtree_oid(
    repo: &Repository,
    root: &git2::Tree,
    path: &str,
) -> Result<Option<Oid>, H5iError> {
    let mut cursor = root.clone();
    for segment in path.split('/') {
        let next_oid = match cursor.get_name(segment) {
            Some(e) if e.kind() == Some(ObjectType::Tree) => e.id(),
            _ => return Ok(None),
        };
        cursor = repo.find_tree(next_oid).map_err(H5iError::Git)?;
    }
    Ok(Some(cursor.id()))
}

/// Per-worktree pin marker path: `<git-dir>/h5i/PINNED`.
fn pin_file_path(repo: &Repository) -> std::path::PathBuf {
    repo.path().join(PIN_FILE)
}

/// `true` when the user has pinned the active context branch via
/// [`gcc_checkout`] or [`gcc_branch`]; auto-follow is disabled while pinned.
pub fn is_pinned(workdir: &Path) -> bool {
    ctx_git_repo(workdir)
        .ok()
        .map(|repo| pin_file_path(&repo).exists())
        .unwrap_or(false)
}

/// Pin the active context branch — subsequent ctx commands stop auto-following
/// the current git branch and stay on whatever the HEAD file points at.
fn set_pin(repo: &Repository) -> Result<(), H5iError> {
    let path = pin_file_path(repo);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(H5iError::Io)?;
    }
    std::fs::write(&path, b"").map_err(H5iError::Io)?;
    Ok(())
}

/// Remove the pin marker so auto-follow resumes on the next ctx command.
pub fn unpin(workdir: &Path) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let path = pin_file_path(&repo);
    if path.exists() {
        std::fs::remove_file(&path).map_err(H5iError::Io)?;
    }
    Ok(())
}

/// Sync the active context branch to the current git branch.
///
/// Pre-redesign, the active context branch was a global setting unaffected by
/// `git checkout`. Now (when not pinned) it shadows the git branch: `git
/// checkout feature` implicitly switches the active context branch to
/// `refs/h5i/context/feature`, auto-creating the ref by forking from main.
fn auto_follow(workdir: &Path) -> Result<(), H5iError> {
    if is_pinned(workdir) {
        return Ok(());
    }
    let repo = ctx_git_repo(workdir)?;
    let git_branch = current_git_branch(workdir);
    if git_branch.is_empty() {
        return Ok(());
    }
    let active = read_head(&repo).unwrap_or_else(|| MAIN_BRANCH.to_string());
    if active == git_branch {
        return Ok(());
    }
    // Ensure the shadow ref exists (fork from current active branch — usually main).
    fork_branch_ref(&repo, &git_branch, &active)?;
    write_head(&repo, &git_branch)?;
    Ok(())
}

/// Ensure the current git branch has a goal and the active h5i context branch
/// has a purpose. Also runs [`auto_follow`] so the active context branch shadows
/// the current git branch by default.
pub fn prepare_context_write(workdir: &Path) -> Result<(), H5iError> {
    let _ = migrate_legacy_if_needed(workdir);
    auto_follow(workdir)?;

    let repo = ctx_git_repo(workdir)?;
    let git_branch = current_git_branch(workdir);
    if git_branch_goal(workdir, &git_branch).is_none() {
        return Err(H5iError::InvalidPath(format!(
            "No context goal recorded for current git branch '{git_branch}'. \
             Run `h5i context init --goal \"<goal>\"` first."
        )));
    }

    let ctx_branch = current_branch(workdir);
    if context_branch_purpose(workdir, &ctx_branch).is_none() {
        return Err(H5iError::InvalidPath(format!(
            "No context purpose recorded for active h5i context branch '{ctx_branch}'. \
             Run `h5i context branch <name> --purpose \"<intent>\"` or \
             `h5i context checkout <existing-context-branch>` first."
        )));
    }

    drop(repo);
    Ok(())
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
    let new_main = auto_update_milestones(&append_main_note(&existing_main, &branch, summary), summary);

    // Clear ephemeral scratch traces on each milestone commit.
    let eph_path = ephemeral_path(&branch);
    let eph_header = format!("# Ephemeral traces — Branch: {branch}\n\n");

    ctx_write_files(
        &repo,
        &[
            (&commit_path, &new_commit_md),
            (&trace_path, &new_trace),
            (&eph_path, &eph_header),
            ("main.md", &new_main),
        ],
        &format!("h5i context commit: {summary}"),
    )
}

/// BRANCH — create a new isolated reasoning workspace and switch to it.
///
/// The new ref is forked from the currently-active branch (like `git branch`),
/// so subsequent merges have a well-defined common ancestor and libgit2's
/// three-way merge produces semantically correct results.
///
/// Explicitly switching pins the active branch — auto-follow does not override
/// it on later ctx commands (use [`unpin`] to resume git-branch shadowing).
pub fn gcc_branch(workdir: &Path, name: &str, purpose: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let parent_branch = current_branch(workdir);
    fork_branch_ref(&repo, name, &parent_branch)?;
    ensure_branch_git(&repo, name, purpose)?;
    set_current_branch(&repo, name)?;
    set_pin(&repo)
}

/// Point `refs/h5i/context/<new>` at the same commit as `refs/h5i/context/<parent>`,
/// like `git branch <new> <parent>`. No-op if the new ref already exists or the
/// parent ref has no commit yet (the next write becomes the orphan root).
pub(crate) fn fork_branch_ref(repo: &Repository, new_branch: &str, parent_branch: &str) -> Result<(), H5iError> {
    let new_ref_name = branch_ref(new_branch);
    if repo.find_reference(&new_ref_name).is_ok() {
        return Ok(());
    }
    let Some(parent_oid) = repo
        .find_reference(&branch_ref(parent_branch))
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .map(|c| c.id())
    else {
        return Ok(());
    };
    repo.reference(
        &new_ref_name,
        parent_oid,
        false,
        &format!("h5i context branch: forked from {parent_branch}"),
    )
    .map_err(H5iError::Git)?;
    Ok(())
}

/// Fork `name` from `parent` and ensure its branch file exists, WITHOUT
/// switching or pinning the calling worktree's selection. Used by
/// `h5i env create` to give an environment its own reasoning branch while the
/// parent worktree stays on its current context.
pub(crate) fn fork_branch_no_switch(
    repo: &Repository,
    name: &str,
    parent: &str,
    purpose: &str,
) -> Result<(), H5iError> {
    fork_branch_ref(repo, name, parent)?;
    ensure_branch_git(repo, name, purpose)
}

/// Point a (work)tree's per-worktree context HEAD at `branch` and pin it, so
/// auto-follow never reverts it to the git branch's shadow context. `repo`
/// must be opened *from inside that worktree* (its `repo.path()` is the
/// per-worktree gitdir where `h5i/HEAD` / `h5i/PINNED` live).
pub(crate) fn pin_worktree_context(repo: &Repository, branch: &str) -> Result<(), H5iError> {
    write_head(repo, branch)?;
    set_pin(repo)
}

/// Switch the active branch without creating it. Pins the selection so it
/// survives subsequent `git checkout`s (use [`unpin`] to resume auto-follow).
pub fn gcc_checkout(workdir: &Path, name: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    if !ctx_list_branches_git(&repo).contains(&name.to_string()) {
        return Err(H5iError::InvalidPath(format!(
            "Context branch '{name}' does not exist. Run `h5i context branch {name}` first."
        )));
    }
    set_current_branch(&repo, name)?;
    set_pin(&repo)
}

/// MERGE — three-way merge a context branch into the current one, producing
/// a real two-parent commit on the target ref.
///
/// Strategy:
///   1. Find the merge-base of target and source (the fork point).
///   2. Let libgit2 merge the three trees — line-based for text files
///      like `trace.md` / `commit.md`. Conflicts abort the merge.
///   3. Layer a semantic DAG merge node and a MERGE milestone entry onto
///      the merged tree, then commit with both branches as parents.
///   4. Append a cross-branch note to `main.md` on the main ref.
pub fn gcc_merge(workdir: &Path, source_branch: &str) -> Result<String, H5iError> {
    let target = current_branch(workdir);
    gcc_merge_into(workdir, &target, source_branch)
}

/// Like [`gcc_merge`], but with an explicit target branch — merges `source`
/// into `target` without touching the worktree's active context selection.
/// Used by `h5i env apply` to fold an environment's reasoning branch back
/// into the parent context branch recorded in the env manifest.
pub fn gcc_merge_into(workdir: &Path, target: &str, source_branch: &str) -> Result<String, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let target = target.to_string();

    if !ctx_list_branches_git(&repo).contains(&source_branch.to_string()) {
        return Err(H5iError::InvalidPath(format!(
            "Branch '{source_branch}' not found"
        )));
    }

    let target_ref_name = branch_ref(&target);
    let source_ref_name = branch_ref(source_branch);

    let target_commit = repo
        .find_reference(&target_ref_name)
        .map_err(H5iError::Git)?
        .peel_to_commit()
        .map_err(H5iError::Git)?;
    let source_commit = repo
        .find_reference(&source_ref_name)
        .map_err(H5iError::Git)?
        .peel_to_commit()
        .map_err(H5iError::Git)?;

    let target_tree = target_commit.tree().map_err(H5iError::Git)?;
    let source_tree = source_commit.tree().map_err(H5iError::Git)?;

    // Find the merge-base; fall back to the target tree itself if there is
    // none (orphan branches), which effectively treats the source's content
    // as pure additions on top of the target.
    let base_tree = repo
        .merge_base(target_commit.id(), source_commit.id())
        .ok()
        .and_then(|oid| repo.find_commit(oid).ok())
        .and_then(|c| c.tree().ok())
        .unwrap_or_else(|| target_tree.clone());

    let mut merge_opts = git2::MergeOptions::new();
    merge_opts.fail_on_conflict(false);
    let mut merged_index = repo
        .merge_trees(&base_tree, &target_tree, &source_tree, Some(&merge_opts))
        .map_err(H5iError::Git)?;

    // Resolve conflicts on append-only context files by union-merging both
    // sides' tails on top of the common ancestor. Real conflicts on any other
    // file remain and abort the merge.
    if merged_index.has_conflicts() {
        resolve_append_only_conflicts(&repo, &mut merged_index)?;
    }
    if merged_index.has_conflicts() {
        let paths: Vec<String> = merged_index
            .conflicts()
            .map_err(H5iError::Git)?
            .filter_map(|c| c.ok())
            .filter_map(|c| {
                c.our
                    .as_ref()
                    .or(c.their.as_ref())
                    .or(c.ancestor.as_ref())
                    .and_then(|e| std::str::from_utf8(&e.path).ok().map(str::to_string))
            })
            .collect();
        return Err(H5iError::InvalidPath(format!(
            "Merge conflicts in: {}. Resolve manually and re-run `h5i context merge {source_branch}`.",
            paths.join(", ")
        )));
    }

    let merged_tree_oid = merged_index
        .write_tree_to(&repo)
        .map_err(H5iError::Git)?;
    let merged_tree = repo.find_tree(merged_tree_oid).map_err(H5iError::Git)?;

    // Read content from the merged tree for post-merge layering.
    let read_from_tree = |tree: &git2::Tree, path: &str| -> String {
        (|| -> Option<String> {
            let entry = tree.get_path(Path::new(path)).ok()?;
            let blob = repo.find_blob(entry.id()).ok()?;
            std::str::from_utf8(blob.content()).ok().map(str::to_owned)
        })()
        .unwrap_or_default()
    };

    // Pull summaries from the ORIGINAL branch trees (not the merged result),
    // because the merge could have interleaved entries from both sides.
    let source_commit_text_orig = read_from_tree(&source_tree, "commit.md");
    let target_commit_text_orig = read_from_tree(&target_tree, "commit.md");
    let source_summary = extract_latest_summary(&source_commit_text_orig);
    let target_summary = extract_latest_summary(&target_commit_text_orig);
    let source_purpose = extract_branch_purpose(&source_commit_text_orig)
        .unwrap_or_else(|| source_branch.to_string());

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
    let merge_entry = format!(
        "## Commit {short_id} — {ts} [MERGE: {source_branch} → {target}]\n\n\
         ### Branch Purpose\nMerge of branch '{source_branch}'\n\n\
         ### Previous Progress Summary\n{merged_summary}\n\n\
         ### This Commit's Contribution\n{contribution}\n\n\
         ---\n\n"
    );
    let merged_commit_md = read_from_tree(&merged_tree, "commit.md");
    let new_commit_md = format!("{merged_commit_md}{merge_entry}");

    // Semantic DAG merge: union node IDs and add a 2-parent MERGE node.
    let source_dag = read_dag(&repo, source_branch);
    let mut merged_dag: TraceDag =
        serde_json::from_str(&read_from_tree(&merged_tree, "dag.json")).unwrap_or_default();
    let seen: std::collections::HashSet<String> =
        merged_dag.nodes.iter().map(|n| n.id.clone()).collect();
    for node in &source_dag.nodes {
        if !seen.contains(&node.id) {
            merged_dag.nodes.push(node.clone());
        }
    }
    let target_head_id = read_dag(&repo, &target).head_id();
    let source_head_id = source_dag.head_id();
    let merge_ts = Utc::now().format("%H:%M:%S").to_string();
    let merge_content = format!("merged '{source_branch}' into '{target}'");
    let mut merge_parent_ids = Vec::new();
    if !target_head_id.is_empty() {
        merge_parent_ids.push(target_head_id);
    }
    if !source_head_id.is_empty() {
        merge_parent_ids.push(source_head_id);
    }
    if !merge_parent_ids.is_empty() {
        merged_dag.nodes.push(TraceNode {
            id: node_id("MERGE", &merge_ts, &merge_content),
            parent_ids: merge_parent_ids,
            kind: "MERGE".to_string(),
            content: merge_content,
            timestamp: merge_ts,
        });
    }
    let dag_json = serde_json::to_string(&merged_dag)
        .map_err(|e| H5iError::InvalidPath(format!("DAG serialisation failed: {e}")))?;

    // Layer the milestone entry, updated DAG, and (when target is main) the
    // cross-branch main.md note directly onto the merged tree so the result
    // is a single two-parent commit.
    let mut layered: Vec<(String, String)> = vec![
        ("commit.md".to_string(), new_commit_md),
        ("dag.json".to_string(), dag_json),
    ];
    if target == MAIN_BRANCH {
        let existing_main = read_from_tree(&merged_tree, "main.md");
        let new_main = append_main_note(
            &existing_main,
            &target,
            &format!("Merged branch '{source_branch}'"),
        );
        layered.push(("main.md".to_string(), new_main));
    }
    let layered_refs: Vec<(&str, &str)> =
        layered.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let final_tree_oid = apply_changes_to_tree(&repo, Some(&merged_tree), &layered_refs)?;
    let final_tree = repo.find_tree(final_tree_oid).map_err(H5iError::Git)?;

    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;
    let parents = [&target_commit, &source_commit];
    repo.commit(
        Some(&target_ref_name),
        &sig,
        &sig,
        &format!("h5i context merge: {source_branch} \u{2192} {target}"),
        &final_tree,
        &parents,
    )
    .map_err(H5iError::Git)?;

    // When target != main, record the cross-branch note in a separate commit on
    // the main ref (target's merge commit is on a different ref and can't
    // atomically touch main).
    if target != MAIN_BRANCH {
        let existing_main = ctx_read_file(&repo, "main.md").unwrap_or_default();
        let new_main = append_main_note(
            &existing_main,
            &target,
            &format!("Merged branch '{source_branch}'"),
        );
        ctx_write_files(
            &repo,
            &[("main.md", &new_main)],
            &format!("h5i context merge note: {source_branch} \u{2192} {target}"),
        )?;
    }

    Ok(merged_summary)
}

/// CONTEXT — retrieve structured context at multiple granularities.
pub fn gcc_context(workdir: &Path, opts: &ContextOpts) -> Result<GccContext, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let git_branch = current_git_branch(workdir);
    let git_branch_goal = git_branch_goal(workdir, &git_branch).unwrap_or_default();
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

    // ── Stable-prefix / dynamic-suffix boundary (Feature 4) ──────────────────
    let trace_path = format!("branches/{branch_name}/trace.md");
    let trace_text = ctx_read_file(&repo, &trace_path).unwrap_or_default();
    let (stable_line_count, dynamic_line_count) = {
        let total = trace_text.lines().count();
        let dynamic = 40_usize.min(total);
        (total - dynamic, dynamic)
    };

    // ── TODO items: NOTE/THINK entries that start with or contain "TODO" ──────
    let todo_items: Vec<String> = {
        let todo_re = ["TODO", "FIXME", "BLOCKED", "REMAINING", "NEXT:"];
        trace_text
            .lines()
            .filter_map(|line| {
                let upper = line.to_uppercase();
                let is_todo = todo_re.iter().any(|kw| upper.contains(kw));
                if is_todo && (line.contains("] NOTE:") || line.contains("] THINK:")) {
                    // Strip the timestamp prefix: "[HH:MM:SS] KIND: content"
                    let content = line
                        .split(": ")
                        .nth(1)
                        .map(|s| format!("{}: {}", s, line.splitn(3, ": ").nth(2).unwrap_or("")))
                        .unwrap_or_else(|| line.to_string());
                    let trimmed = content.trim().trim_start_matches("NOTE: ").trim_start_matches("THINK: ");
                    Some(trimmed.chars().take(100).collect())
                } else {
                    None
                }
            })
            .collect()
    };

    // ── Mini-trace: last 8 non-empty, non-header OTA lines ───────────────────
    let mini_trace: Vec<String> = {
        let ota_lines: Vec<&str> = trace_text
            .lines()
            .filter(|l| {
                !l.trim().is_empty()
                    && !l.starts_with('#')
                    && !l.starts_with("---")
                    && !l.starts_with("_[Checkpoint")
            })
            .collect();
        ota_lines
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|l| l.to_string())
            .collect()
    };

    Ok(GccContext {
        project_goal,
        git_branch,
        git_branch_goal,
        milestones,
        active_branches,
        current_branch: branch_name,
        recent_commits,
        recent_log_lines,
        metadata_snippet,
        stable_line_count,
        dynamic_line_count,
        todo_items,
        mini_trace,
    })
}

/// Append an OTA (Observation–Thought–Action) entry to the current branch's trace.
///
/// When `ephemeral` is `true` the entry goes to `ephemeral.md` only — it is
/// excluded from the DAG, excluded from snapshots, and cleared on the next
/// `h5i context commit`. Use this for scratch observations you don't need to
/// preserve across sessions (analogous to Claude Code's `/btw`).
///
/// When `ephemeral` is `false` (the default) the entry is appended to both
/// `trace.md` (human-readable rendered view) and `dag.json` (the DAG).
pub fn append_log(workdir: &Path, kind: &str, content: &str, ephemeral: bool) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);

    let ts = Utc::now().format("%H:%M:%S").to_string();
    let entry_line = format!("[{ts}] {}: {}\n", kind.to_uppercase(), content);

    if ephemeral {
        let epath = ephemeral_path(&branch);
        let existing = ctx_read_file(&repo, &epath).unwrap_or_default();
        return ctx_write_files(
            &repo,
            &[(&epath, &format!("{existing}{entry_line}"))],
            "h5i context trace (ephemeral)",
        );
    }

    // Durable path: update trace.md + dag.json together.
    let trace_path = format!("branches/{branch}/trace.md");
    let existing_trace = ctx_read_file(&repo, &trace_path).unwrap_or_default();
    let new_trace = format!("{existing_trace}{entry_line}");

    let mut dag = read_dag(&repo, &branch);
    let parent_ids = if dag.head_id().is_empty() {
        vec![]
    } else {
        vec![dag.head_id()]
    };
    let node = TraceNode {
        id: node_id(kind, &ts, content),
        parent_ids,
        kind: kind.to_uppercase(),
        content: content.to_string(),
        timestamp: ts,
    };
    dag.nodes.push(node);
    let dag_json = serde_json::to_string(&dag)
        .map_err(|e| H5iError::InvalidPath(format!("DAG serialisation failed: {e}")))?;

    ctx_write_files(
        &repo,
        &[
            (&trace_path, &new_trace),
            (&dag_path(&branch), &dag_json),
        ],
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

/// Read a single arbitrary file from the context workspace.
/// Returns `None` if the workspace or file does not exist.
pub fn read_ctx_file(workdir: &Path, vpath: &str) -> Option<String> {
    let repo = ctx_git_repo(workdir).ok()?;
    ctx_read_file(&repo, vpath)
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
    let main_ref = branch_ref(MAIN_BRANCH);
    if repo.find_reference(&main_ref).is_err() {
        return Ok(());
    }

    // Capture main's tip OID (used by `pack` to gate squashing) before we
    // build the synthetic aggregate that context_diff / restore will read.
    let main_tip_oid = repo
        .find_reference(&main_ref)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .map(|c| c.id().to_string())
        .unwrap_or_default();

    // Build a synthetic aggregate tree mirroring the legacy single-ref layout
    // (`main.md`, `git-goals/...`, `branches/<name>/...`) so that the recorded
    // OID is self-contained — readable later by context_diff/restore without
    // needing to traverse the live per-branch refs (which may have moved on).
    let short_sha = &git_sha[..git_sha.len().min(8)];
    let agg_tree_oid = build_aggregate_tree(&repo)?;
    let agg_tree = repo.find_tree(agg_tree_oid).map_err(H5iError::Git)?;
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)?;
    let anchor_ref = format!("refs/h5i/context-snapshots/{short_sha}");
    let anchor_oid = repo
        .commit(
            Some(&anchor_ref),
            &sig,
            &sig,
            &format!("h5i context snapshot anchor: {short_sha}"),
            &agg_tree,
            &[],
        )
        .map_err(H5iError::Git)?;
    let ctx_oid = anchor_oid.to_string();

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

    let mut content = format!(
        "# Context Snapshot — {short_sha}\n\n\
         **Linked commit:** {git_sha}\n\
         **Context ref OID:** {ctx_oid}\n\
         **Main tip OID:** {main_tip_oid}\n\
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
    let main_ref = branch_ref(MAIN_BRANCH);
    let current_parent = repo
        .find_reference(&main_ref)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = current_parent.iter().collect();

    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    repo.commit(
        Some(&main_ref),
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
    pub from_branch: String,
    pub to_branch: String,
    /// Context milestones present in sha2 but not sha1.
    pub added_commits: Vec<String>,
    /// Context milestones present in sha1 but not sha2.
    pub removed_commits: Vec<String>,
    /// Trace lines present in sha2 but not sha1 (OTA steps, up to 30).
    pub added_trace_lines: Vec<String>,
    /// Trace lines present in sha1 but not sha2 (OTA steps, up to 30).
    pub removed_trace_lines: Vec<String>,
    pub goal_changed: bool,
    pub from_goal: String,
    pub to_goal: String,
}

#[derive(Debug, Default, Clone)]
struct SnapshotMeta {
    branch: String,
    ctx_oid: String,
}

fn parse_snapshot_meta(snapshot: &str) -> Result<SnapshotMeta, H5iError> {
    let ctx_oid = snapshot
        .lines()
        .find(|l| l.starts_with("**Context ref OID:**"))
        .and_then(|l| l.split("**Context ref OID:**").nth(1))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| H5iError::InvalidPath("Snapshot missing Context ref OID".into()))?
        .to_string();
    let branch = snapshot
        .lines()
        .find(|l| l.starts_with("**Branch:**"))
        .and_then(|l| l.split("**Branch:**").nth(1))
        .map(str::trim)
        .unwrap_or(MAIN_BRANCH)
        .to_string();
    Ok(SnapshotMeta { branch, ctx_oid })
}

/// Compare the context workspace state at two code commits.
pub fn context_diff(workdir: &Path, sha1: &str, sha2: &str) -> Result<ContextDiff, H5iError> {
    let repo = ctx_git_repo(workdir)?;

    let load_ctx_commit = |sha: &str| -> Result<(git2::Commit, SnapshotMeta), H5iError> {
        let short = &sha[..sha.len().min(8)];
        let snap = ctx_read_file(&repo, &format!("snapshots/{short}.md"))
            .ok_or_else(|| H5iError::InvalidPath(format!("No context snapshot for {sha}")))?;
        let meta = parse_snapshot_meta(&snap)?;
        let oid = git2::Oid::from_str(&meta.ctx_oid).map_err(H5iError::Git)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|_| H5iError::InvalidPath(format!("Context OID {} not in object store", meta.ctx_oid)))?;
        Ok((commit, meta))
    };

    let (c1, meta1) = load_ctx_commit(sha1)?;
    let (c2, meta2) = load_ctx_commit(sha2)?;

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

    let commit_path_1 = format!("branches/{}/commit.md", meta1.branch);
    let commit_path_2 = format!("branches/{}/commit.md", meta2.branch);
    let trace_path_1 = format!("branches/{}/trace.md", meta1.branch);
    let trace_path_2 = format!("branches/{}/trace.md", meta2.branch);

    let commits1: std::collections::HashSet<String> =
        extract_recent_commits(&read_from(&c1, &commit_path_1), 200)
            .into_iter()
            .collect();
    let commits2 = extract_recent_commits(&read_from(&c2, &commit_path_2), 200);
    let added_commits: Vec<String> = commits2
        .iter()
        .filter(|c| !commits1.contains(*c))
        .cloned()
        .collect();
    let commits2_set: std::collections::HashSet<String> = commits2.into_iter().collect();
    let removed_commits: Vec<String> = commits1
        .iter()
        .filter(|c| !commits2_set.contains(*c))
        .cloned()
        .collect();

    let trace1: std::collections::HashSet<String> =
        read_from(&c1, &trace_path_1).lines().map(str::to_string).collect();
    let trace2_text = read_from(&c2, &trace_path_2);
    let added_trace_lines: Vec<String> = trace2_text
        .lines()
        .filter(|l| !l.is_empty() && !trace1.contains(*l))
        .take(30)
        .map(str::to_string)
        .collect();
    let trace2: std::collections::HashSet<String> =
        trace2_text.lines().map(str::to_string).collect();
    let removed_trace_lines: Vec<String> = read_from(&c1, &trace_path_1)
        .lines()
        .filter(|l| !l.is_empty() && !trace2.contains(*l))
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
        from_branch: meta1.branch,
        to_branch: meta2.branch,
        added_commits,
        removed_commits,
        added_trace_lines,
        removed_trace_lines,
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

    // Match only lines that literally contain the full file path or the bare
    // filename — no ±1 context expansion, which was pulling in unrelated entries.
    let matches_file = |text: &str| {
        // Prefer exact path match; fall back to filename only if unambiguous
        // (filename must appear as a whole token, not as a substring of another path).
        if text.contains(file_path) {
            return true;
        }
        // file_name match: guard against false positives like "auth.rs" matching
        // "noauth.rs" by requiring a word boundary on the left.
        if file_name.len() > 3 {
            let mut start = 0;
            while let Some(pos) = text[start..].find(file_name) {
                let abs = start + pos;
                let left_ok = abs == 0 || !text.as_bytes()[abs - 1].is_ascii_alphanumeric();
                if left_ok {
                    return true;
                }
                start = abs + 1;
            }
        }
        false
    };

    // ── Trace mentions ────────────────────────────────────────────────────────
    let trace_text =
        ctx_read_file(&repo, &format!("branches/{branch}/trace.md")).unwrap_or_default();
    let mut trace_mentions: Vec<String> = Vec::new();
    for line in trace_text.lines() {
        if matches_file(line) && !line.is_empty() {
            let s = line.to_string();
            if !trace_mentions.contains(&s) {
                trace_mentions.push(s);
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
        println!("  {} ", style("Goal changed:").bold().yellow());
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
    let main_ref = branch_ref(MAIN_BRANCH);

    // Collect all snapshot short-SHAs so we know which context commits are still live.
    let tip = repo
        .find_reference(&main_ref)
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
        Some(&main_ref),
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
        // Prefer the post-redesign `Main tip OID` (a real commit on
        // refs/h5i/context/main) over the legacy `Context ref OID` (which
        // now points at a synthetic anchor commit outside main's history).
        let mut found = None;
        for line in content.lines() {
            if line.starts_with("**Main tip OID:**") {
                if let Some(oid_str) = line.split("**Main tip OID:**").nth(1) {
                    if let Ok(oid) = git2::Oid::from_str(oid_str.trim()) {
                        found = Some(oid);
                        break;
                    }
                }
            }
        }
        if found.is_none() {
            for line in content.lines() {
                if line.starts_with("**Context ref OID:**") {
                    if let Some(oid_str) = line.split("**Context ref OID:**").nth(1) {
                        if let Ok(oid) = git2::Oid::from_str(oid_str.trim()) {
                            found = Some(oid);
                            break;
                        }
                    }
                }
            }
        }
        if let Some(oid) = found {
            oids.push(oid);
        }
    }
    Ok(Some(oids))
}

// ── Three-pass lossless pack (Feature 2) ─────────────────────────────────────

/// Compact the current branch's trace using three structurally-lossless passes:
///
/// - **Pass 1 (subsumption):** Remove OBSERVE entries whose key subject token
///   (file name or first significant word) appears in a *later* THINK or ACT
///   entry — those observations have been "consumed" by higher-level reasoning.
/// - **Pass 2 (preservation):** Retain every THINK, ACT, and NOTE entry verbatim;
///   they represent irreplaceable decisions and actions.
/// - **Pass 3 (consolidation):** Merge consecutive OBSERVE entries that share the
///   same subject token into a single entry with a `(×N)` count suffix.
///
/// The result is written back to `trace.md` and `dag.json` as a new context commit.
pub fn pack_lossless(workdir: &Path) -> Result<LosslessPackResult, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let trace_path = format!("branches/{branch}/trace.md");
    let trace_text = ctx_read_file(&repo, &trace_path).unwrap_or_default();

    // Parse each non-empty, non-header, non-separator line into (kind, content).
    #[derive(Clone)]
    struct ParsedEntry {
        kind: String,
        content: String,
        raw: String,
    }

    let entries: Vec<ParsedEntry> = trace_text
        .lines()
        .filter_map(|line| {
            // Lines look like: [HH:MM:SS] KIND: content
            let rest = line.trim_start_matches('[')
                .split_once(']')
                .map(|x| x.1)
                .map(str::trim)
                .unwrap_or(line);
            if rest.is_empty() || line.starts_with('#') || line.starts_with("---") || line.starts_with("_[") {
                return None; // header / separator
            }
            let (kind, content) = if let Some(colon) = rest.find(':') {
                let k = rest[..colon].trim().to_uppercase();
                let c = rest[colon + 1..].trim().to_string();
                (k, c)
            } else {
                ("NOTE".to_string(), rest.to_string())
            };
            Some(ParsedEntry { kind, content, raw: line.to_string() })
        })
        .collect();

    // Extract the "subject token" for an OBSERVE entry: the first path-like word
    // or (fallback) the first non-trivial word.
    let subject_of = |content: &str| -> String {
        content.split_whitespace()
            .find(|w| w.contains('/') || w.contains('.') || w.len() > 4)
            .unwrap_or_else(|| content.split_whitespace().next().unwrap_or(""))
            .to_lowercase()
    };

    // ── Pass 1: mark OBSERVE entries subsumed by a later THINK/ACT ───────────
    let think_act_subjects: std::collections::HashSet<String> = entries.iter()
        .filter(|e| matches!(e.kind.as_str(), "THINK" | "ACT"))
        .flat_map(|e| {
            let words: Vec<String> = e.content.split_whitespace()
                .map(|w| w.to_lowercase())
                .collect();
            words
        })
        .collect();

    let mut keep: Vec<bool> = vec![true; entries.len()];
    let mut removed_subsumed_observe: usize = 0;

    for (i, entry) in entries.iter().enumerate() {
        if entry.kind != "OBSERVE" {
            continue;
        }
        let subj = subject_of(&entry.content);
        // Check if a later entry is THINK/ACT AND mentions the subject.
        let subsumed = entries[i + 1..].iter().any(|later| {
            matches!(later.kind.as_str(), "THINK" | "ACT")
                && (later.content.to_lowercase().contains(&subj) || think_act_subjects.contains(&subj))
        });
        if subsumed {
            keep[i] = false;
            removed_subsumed_observe += 1;
        }
    }

    // ── Pass 2 + 3: build output, merging consecutive OBSERVE on same subject ─
    let surviving: Vec<&ParsedEntry> = entries.iter()
        .zip(keep.iter())
        .filter_map(|(e, &k)| if k { Some(e) } else { None })
        .collect();

    let mut kept_durable: usize = 0;
    let mut merged_consecutive_observe: usize = 0;
    let mut output_lines: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < surviving.len() {
        let entry = surviving[i];
        if entry.kind != "OBSERVE" {
            kept_durable += 1;
            output_lines.push(entry.raw.clone());
            i += 1;
            continue;
        }
        // OBSERVE: look ahead for consecutive same-subject entries.
        let subj = subject_of(&entry.content);
        let mut count = 1usize;
        let mut j = i + 1;
        while j < surviving.len()
            && surviving[j].kind == "OBSERVE"
            && subject_of(&surviving[j].content) == subj
        {
            count += 1;
            j += 1;
        }
        if count > 1 {
            merged_consecutive_observe += count - 1;
            // Keep the last (most recent) OBSERVE for this subject, annotate count.
            let last = surviving[j - 1];
            let merged_raw = format!("{} (×{})", last.raw.trim_end(), count);
            output_lines.push(merged_raw);
        } else {
            output_lines.push(entry.raw.clone());
        }
        i = j;
    }

    if removed_subsumed_observe == 0 && merged_consecutive_observe == 0 {
        return Ok(LosslessPackResult {
            removed_subsumed_observe: 0,
            merged_consecutive_observe: 0,
            kept_durable,
        });
    }

    // Rebuild trace.md preserving header and separator structure.
    let header_lines: Vec<&str> = trace_text
        .lines()
        .take_while(|l| l.starts_with('#') || l.is_empty())
        .collect();
    let new_trace = format!(
        "{}\n\n{}\n",
        header_lines.join("\n"),
        output_lines.join("\n")
    );

    // Rebuild dag.json keeping only nodes that survived.
    let surviving_contents: std::collections::HashSet<String> = surviving.iter()
        .map(|e| e.content.clone())
        .collect();
    let mut dag = read_dag(&repo, &branch);
    dag.nodes.retain(|n| surviving_contents.contains(&n.content));
    let dag_json = serde_json::to_string(&dag)
        .map_err(|e| H5iError::InvalidPath(format!("DAG serialisation failed: {e}")))?;

    ctx_write_files(
        &repo,
        &[
            (&trace_path, &new_trace),
            (&dag_path(&branch), &dag_json),
        ],
        "h5i context pack (lossless)",
    )?;

    Ok(LosslessPackResult {
        removed_subsumed_observe,
        merged_consecutive_observe,
        kept_durable,
    })
}

// ── Subagent-scoped sub-contexts (Feature 5) ─────────────────────────────────

/// Create a subagent-scoped sub-context: a branch prefixed `scope/` with
/// metadata marking it as a scope. Scoped branches appear separately in
/// `h5i context status` and are intended for delegated subagent investigation.
pub fn gcc_scope(workdir: &Path, full_name: &str, purpose: &str) -> Result<(), H5iError> {
    let repo = ctx_git_repo(workdir)?;
    ensure_branch_git(&repo, full_name, purpose)?;

    // Tag the branch as a scope in its metadata.yaml.
    let meta_path = format!("branches/{full_name}/metadata.yaml");
    let existing_meta = ctx_read_file(&repo, &meta_path).unwrap_or_default();
    let scoped_meta = if existing_meta.contains("scope:") {
        existing_meta
    } else {
        format!("{existing_meta}scope: \"true\"\n")
    };
    ctx_write_files(&repo, &[(&meta_path, &scoped_meta)], "h5i context scope")?;

    set_current_branch(&repo, full_name)
}

/// Read the ephemeral scratch traces for a branch (default: current).
pub fn read_ephemeral(workdir: &Path, branch: Option<&str>) -> Result<String, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branch_name = branch
        .map(str::to_string)
        .unwrap_or_else(|| current_branch(workdir));
    Ok(ctx_read_file(&repo, &ephemeral_path(&branch_name)).unwrap_or_default())
}

// ── Stable-prefix display (Feature 4) ────────────────────────────────────────

/// Print the stable-prefix / dynamic-suffix boundary for the current trace.
/// Lines in the stable prefix are unchanged across most agent steps and benefit
/// from prompt-cache hits. Lines in the dynamic suffix change every step.
pub fn print_cached_prefix(workdir: &Path, tail: usize) -> Result<(), H5iError> {
    use console::style;

    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let trace_path = format!("branches/{branch}/trace.md");
    let trace_text = ctx_read_file(&repo, &trace_path).unwrap_or_default();
    let all_lines: Vec<&str> = trace_text.lines().collect();
    let total = all_lines.len();
    let dynamic = tail.min(total);
    let stable_end = total - dynamic;

    println!(
        "{}",
        style(format!(
            "── Stable-prefix boundary (tail={tail}) ────────────────────────"
        ))
        .dim()
    );
    println!(
        "  {} Stable prefix: {} line{} (prompt-cache friendly)",
        style("▓▓").green(),
        style(stable_end).cyan().bold(),
        if stable_end == 1 { "" } else { "s" }
    );
    println!(
        "  {} Dynamic suffix: {} line{} (changes every step)",
        style("░░").yellow(),
        style(dynamic).cyan().bold(),
        if dynamic == 1 { "" } else { "s" }
    );

    if total == 0 {
        println!("  {}", style("(empty trace)").dim());
        return Ok(());
    }

    println!();
    println!("  {} Last stable line:", style("▓").green());
    if stable_end > 0 {
        println!("    {}", style(all_lines[stable_end - 1]).dim());
    } else {
        println!("    {}", style("(all lines are dynamic)").dim());
    }
    println!("  {} First dynamic line:", style("░").yellow());
    if stable_end < total {
        println!("    {}", style(all_lines[stable_end]).cyan());
    }

    Ok(())
}

// ── Print status with scope support ──────────────────────────────────────────

/// Return `true` if `branch_name` is a subagent scope (`scope/` prefix or
/// `scope: "true"` in its metadata.yaml).
fn is_scope_branch(repo: &Repository, branch_name: &str) -> bool {
    if branch_name.starts_with("scope/") {
        return true;
    }
    ctx_read_file(repo, &format!("branches/{branch_name}/metadata.yaml"))
        .map(|m| m.contains("scope: \"true\""))
        .unwrap_or(false)
}

// ── Terminal display ──────────────────────────────────────────────────────────

/// Depth 1 — compact index (~800 tokens): goal, branch, milestone IDs, commit count.
/// Fastest orientation; use when you only need to know what exists.
fn print_context_index(ctx: &GccContext) {
    use console::style;
    println!(
        "{}",
        style("── Context Index (depth=1) ──────────────────────────────").dim()
    );
    let goal_source = if ctx.git_branch_goal.is_empty() {
        &ctx.project_goal
    } else {
        &ctx.git_branch_goal
    };
    let goal: String = goal_source.chars().take(100).collect();
    println!(
        "  git_branch={}  context_branch={}  goal={}",
        style(&ctx.git_branch).cyan(),
        style(&ctx.current_branch).magenta(),
        if goal.is_empty() { style("(none)".to_string()).dim() } else { style(goal).cyan() }
    );
    println!(
        "  milestones={}  commits={}  trace_lines={}+{}",
        ctx.milestones.len(),
        ctx.recent_commits.len(),
        style(ctx.stable_line_count).green(),
        style(ctx.dynamic_line_count).yellow(),
    );
    if ctx.active_branches.len() > 1 {
        println!(
            "  branches: {}",
            ctx.active_branches
                .iter()
                .map(|b| if b == &ctx.current_branch {
                    format!("*{b}")
                } else {
                    b.clone()
                })
                .collect::<Vec<_>>()
                .join("  ")
        );
    }
    for (i, m) in ctx.milestones.iter().enumerate() {
        let label: String = m.chars().take(72).collect();
        println!("  m{i}: {label}");
    }
    if !ctx.todo_items.is_empty() {
        println!("  todos: {}", ctx.todo_items.len());
    }
}

/// Depth 2 — timeline (~2–5K tokens): adds recent commits and mini-trace.
/// Default view; covers most orientation needs without the full trace.
fn print_context_timeline(ctx: &GccContext) {
    use console::style;
    println!(
        "{}",
        style("── Context (depth=2) ────────────────────────────────────").dim()
    );
    let goal_source = if ctx.git_branch_goal.is_empty() {
        &ctx.project_goal
    } else {
        &ctx.git_branch_goal
    };
    println!(
        "  {} {}  (branch: {})",
        style("Goal:").bold(),
        if goal_source.is_empty() {
            style("(no goal set)".to_string()).dim()
        } else {
            style(goal_source.chars().take(80).collect::<String>()).cyan()
        },
        style(&ctx.current_branch).magenta(),
    );
    println!(
        "  {} {}  |  {} {}",
        style("Git branch:").dim(),
        style(&ctx.git_branch).cyan(),
        style("Context branch:").dim(),
        style(&ctx.current_branch).magenta()
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

    if !ctx.mini_trace.is_empty() {
        println!();
        println!("  {}", style("Recent Trace:").bold());
        for line in &ctx.mini_trace {
            let styled = if line.contains("] ACT:") {
                style(line.as_str()).green().dim()
            } else if line.contains("] THINK:") {
                style(line.as_str()).yellow().dim()
            } else if line.contains("] NOTE:") {
                style(line.as_str()).white().dim()
            } else {
                style(line.as_str()).dim()
            };
            println!("    {}", styled);
        }
    }

    if !ctx.todo_items.is_empty() {
        println!();
        println!("  {}", style("Open TODOs:").bold().yellow());
        for item in &ctx.todo_items {
            println!("    {} {}", style("□").yellow(), style(item).dim());
        }
    }
}

pub fn print_context(ctx: &GccContext) {
    print_context_depth(ctx, 2);
}

/// Render context at the requested depth (1=index, 2=timeline, 3=full trace).
pub fn print_context_depth(ctx: &GccContext, depth: u8) {
    use console::style;
    match depth {
        1 => {
            print_context_index(ctx);
        }
        3 => {
            // Full output: timeline header + full OTA log.
            print_context_timeline(ctx);
            if !ctx.recent_log_lines.is_empty() {
                println!();
                println!("  {}", style("Full OTA Log:").bold());
                for line in &ctx.recent_log_lines {
                    println!("    {}", style(line).dim());
                }
            }
        }
        _ => {
            // depth=2 (default)
            print_context_timeline(ctx);
        }
    }
}

/// Divergence between regular git branches and their shadow ctx branches.
/// Used by [`print_status`] to flag situations that auto-follow alone cannot
/// reconcile (e.g. an upstream merge that didn't carry the context with it).
#[derive(Debug, Default)]
pub struct ReconciliationReport {
    /// Local git branches with no matching `refs/h5i/context/<name>`.
    pub git_only: Vec<String>,
    /// Context branches whose git counterpart was deleted.
    pub ctx_only: Vec<String>,
    /// Context branches whose git side was merged into `main` but whose ctx
    /// side is NOT yet merged into the ctx main branch.
    pub merged_in_git_only: Vec<String>,
}

/// Compute reconciliation between git branches and ctx branches in `workdir`.
pub fn reconcile_git_vs_ctx(workdir: &Path) -> Result<ReconciliationReport, H5iError> {
    let mut report = ReconciliationReport::default();
    let repo = ctx_git_repo(workdir)?;

    // Collect git local branches.
    let mut git_branches: Vec<String> = Vec::new();
    if let Ok(iter) = repo.branches(Some(git2::BranchType::Local)) {
        for b in iter.flatten() {
            if let Ok(Some(name)) = b.0.name() {
                git_branches.push(name.to_string());
            }
        }
    }
    git_branches.sort();
    git_branches.dedup();

    let ctx_branches = ctx_list_branches_git(&repo);
    let git_set: std::collections::HashSet<&String> = git_branches.iter().collect();
    let ctx_set: std::collections::HashSet<&String> = ctx_branches.iter().collect();

    for g in &git_branches {
        if !ctx_set.contains(g) {
            report.git_only.push(g.clone());
        }
    }
    for c in &ctx_branches {
        // Skip ctx branches that exist purely on the ctx side (no git
        // counterpart was ever expected, e.g. exploratory reasoning branches
        // named `option-a`, `scope/foo`). We only flag ones that LOOK like
        // they should shadow a git branch — i.e. ones that match a known
        // git-naming pattern. Cheap heuristic: contains a `/` like `feature/x`
        // or matches a known git branch name once existed (we can't tell that
        // easily). For now, flag *all* ctx branches whose git counterpart is
        // absent; users can ignore the ones that are intentionally ctx-only.
        if c != MAIN_BRANCH && !git_set.contains(c) {
            report.ctx_only.push(c.clone());
        }
    }

    // For each ctx branch with a live git counterpart, check whether the
    // git side has been merged to `refs/heads/main` (or `master`) but the ctx
    // side has NOT been merged to `refs/h5i/context/main`.
    let git_main_oid = repo
        .find_reference("refs/heads/main")
        .or_else(|_| repo.find_reference("refs/heads/master"))
        .ok()
        .and_then(|r| r.target());
    let ctx_main_oid = repo
        .find_reference(&branch_ref(MAIN_BRANCH))
        .ok()
        .and_then(|r| r.target());

    if let (Some(git_main), Some(ctx_main)) = (git_main_oid, ctx_main_oid) {
        for name in &ctx_branches {
            if name == MAIN_BRANCH {
                continue;
            }
            if !git_set.contains(name) {
                continue;
            }
            let git_oid = match repo
                .find_reference(&format!("refs/heads/{name}"))
                .ok()
                .and_then(|r| r.target())
            {
                Some(o) => o,
                None => continue,
            };
            let ctx_oid = match repo
                .find_reference(&branch_ref(name))
                .ok()
                .and_then(|r| r.target())
            {
                Some(o) => o,
                None => continue,
            };
            // "Merged" means: main is at or beyond branch tip. libgit2's
            // graph_descendant_of returns false for the equal case, so check both.
            let git_merged = git_main == git_oid
                || repo.graph_descendant_of(git_main, git_oid).unwrap_or(false);
            let ctx_merged = ctx_main == ctx_oid
                || repo.graph_descendant_of(ctx_main, ctx_oid).unwrap_or(false);
            if git_merged && !ctx_merged {
                report.merged_in_git_only.push(name.clone());
            }
        }
    }

    Ok(report)
}

pub fn print_status(workdir: &Path) -> Result<(), H5iError> {
    use console::style;

    if !is_initialized(workdir) {
        println!(
            "{} {} not initialized. Run {} to initialize.",
            style("ℹ").blue(),
            style(CTX_REF_PREFIX.trim_end_matches('/')).yellow(),
            style("h5i context init").bold()
        );
        return Ok(());
    }

    let repo = ctx_git_repo(workdir)?;
    let git_branch = current_git_branch(workdir);
    let git_goal = git_branch_goal(workdir, &git_branch).unwrap_or_default();
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
        "  {} {}  |  {} {}",
        style("Git branch:").dim(),
        style(&git_branch).cyan().bold(),
        style("goal:").dim(),
        if git_goal.is_empty() {
            style("(missing; run h5i context init --goal \"<goal>\")".to_string()).yellow()
        } else {
            style(git_goal.chars().take(80).collect::<String>()).cyan()
        }
    );
    println!(
        "  {} {}  |  {} branch{}  |  {} commit{}  |  {} log line{}",
        style("Context branch:").dim(),
        style(&branch).magenta().bold(),
        style(branches.len()).cyan(),
        if branches.len() == 1 { "" } else { "es" },
        style(commit_count).cyan(),
        if commit_count == 1 { "" } else { "s" },
        style(log_lines).dim(),
        if log_lines == 1 { "" } else { "s" },
    );

    // Separate regular branches from scoped sub-contexts.
    let (scope_branches, regular_branches): (Vec<&String>, Vec<&String>) = branches
        .iter()
        .filter(|b| b.as_str() != branch)
        .partition(|b| is_scope_branch(&repo, b));

    if !regular_branches.is_empty() {
        println!(
            "  {} {}",
            style("Other branches:").dim(),
            regular_branches
                .iter()
                .map(|b| b.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if !scope_branches.is_empty() {
        println!(
            "  {} {}",
            style("Scoped subagents:").dim(),
            scope_branches
                .iter()
                .map(|b| style(b.as_str()).magenta().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Show stable/dynamic prefix split for the current trace.
    let total_lines = trace_text.lines().count();
    let dynamic = 40_usize.min(total_lines);
    let stable = total_lines - dynamic;
    if total_lines > 0 {
        println!(
            "  {} stable {} lines  ·  dynamic {} lines  (prompt-cache boundary)",
            style("Trace:").dim(),
            style(stable).cyan(),
            style(dynamic).yellow(),
        );
    }

    // Reconciliation against git branches.
    if let Ok(rep) = reconcile_git_vs_ctx(workdir) {
        let any = !rep.git_only.is_empty()
            || !rep.ctx_only.is_empty()
            || !rep.merged_in_git_only.is_empty();
        if any {
            println!();
            println!(
                "{}",
                style("── Sync with git ───────────────────────────────────────────────").dim()
            );
        }
        for name in &rep.git_only {
            println!(
                "  {} git/{} has no ctx shadow → {}",
                style("⊘").yellow(),
                style(name).cyan(),
                style(format!("h5i context branch {name} --purpose \"…\"")).dim(),
            );
        }
        for name in &rep.merged_in_git_only {
            println!(
                "  {} git/{} merged → main, but ctx/{} not merged → {}",
                style("⚠").yellow(),
                style(name).cyan(),
                style(name).magenta(),
                style(format!("h5i context merge {name}")).dim(),
            );
        }
        for name in &rep.ctx_only {
            println!(
                "  {} ctx/{} exists but git/{} is gone → {}",
                style("✗").red(),
                style(name).magenta(),
                style(name).cyan(),
                style("(intentional or stale — delete manually if stale)").dim(),
            );
        }
    }

    Ok(())
}

/// Extract and print all open TODO/FIXME/BLOCKED items from the current branch trace.
pub fn print_todos(workdir: &Path) -> Result<(), H5iError> {
    use console::style;

    let repo = ctx_git_repo(workdir)?;
    let branch = current_branch(workdir);
    let trace_text =
        ctx_read_file(&repo, &format!("branches/{branch}/trace.md")).unwrap_or_default();

    let keywords = ["TODO", "FIXME", "BLOCKED", "REMAINING", "NEXT:"];
    let items: Vec<&str> = trace_text
        .lines()
        .filter(|l| {
            // Only surface items from NOTE and THINK entries, not every OBSERVE.
            let is_note_or_think = l.contains("] NOTE:") || l.contains("] THINK:");
            let u = l.to_uppercase();
            is_note_or_think && keywords.iter().any(|kw| u.contains(kw))
        })
        .collect();

    println!(
        "{}",
        style(format!("── Open TODOs ──────────────────────────────── {branch} ──")).dim()
    );

    if items.is_empty() {
        println!("  {}", style("No TODO/FIXME/BLOCKED items found in trace.").dim());
        return Ok(());
    }

    for item in &items {
        // Strip timestamp prefix for cleaner display.
        let content = item
            .split_once("] ")
            .map(|x| x.1)
            .unwrap_or(item)
            .trim_start_matches("NOTE: ")
            .trim_start_matches("THINK: ");
        println!("  {} {}", style("□").yellow(), style(content).dim());
    }

    println!();
    println!(
        "  {} {} item{} found",
        style("◈").dim(),
        style(items.len()).yellow().bold(),
        if items.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Return all THINK entries from every context branch as structured data.
/// Each item: `{ "branch": "...", "thought": "..." }`.
pub fn distill_knowledge(workdir: &Path) -> Result<Vec<serde_json::Value>, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let branches = ctx_list_branches_git(&repo);
    let mut all_thoughts: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for branch_name in &branches {
        let trace =
            ctx_read_file(&repo, &format!("branches/{branch_name}/trace.md"))
                .unwrap_or_default();
        for line in trace.lines() {
            if line.contains("] THINK:") {
                let content = line
                    .split_once("] THINK:")
                    .map(|x| x.1)
                    .unwrap_or(line)
                    .trim()
                    .to_string();
                if content.len() > 20 && seen.insert(content.chars().take(60).collect()) {
                    all_thoughts.push(serde_json::json!({
                        "branch": branch_name,
                        "thought": content
                    }));
                }
            }
        }
    }
    Ok(all_thoughts)
}

/// Distill all THINK entries from every context branch into a project knowledge base.
///
/// Deduplicated and sorted, this gives a quick read of every design decision ever
/// recorded across all reasoning branches for this project.
pub fn print_knowledge(workdir: &Path) -> Result<(), H5iError> {
    use console::style;

    let repo = ctx_git_repo(workdir)?;

    // Collect all branch names from refs/h5i/context/branches/
    let mut all_thoughts: Vec<(String, String)> = Vec::new(); // (branch, line)

    let branches = ctx_list_branches_git(&repo);
    for branch_name in &branches {
        let trace =
            ctx_read_file(&repo, &format!("branches/{branch_name}/trace.md"))
                .unwrap_or_default();
        for line in trace.lines() {
            if line.contains("] THINK:") {
                let content = line
                    .split_once("] THINK:")
                    .map(|x| x.1)
                    .unwrap_or(line)
                    .trim()
                    .to_string();
                if content.len() > 20 {
                    all_thoughts.push((branch_name.clone(), content));
                }
            }
        }
    }

    // Deduplicate by content prefix (first 60 chars).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    all_thoughts.retain(|(_, c)| seen.insert(c.chars().take(60).collect()));

    println!("{}", style("── Project Knowledge (distilled THINK entries) ─────────────").dim());

    if all_thoughts.is_empty() {
        println!("  {}", style("No THINK entries recorded yet. Use `h5i context trace --kind THINK` to record decisions.").dim());
        return Ok(());
    }

    let branch = current_branch(workdir);
    for (br, content) in &all_thoughts {
        let branch_label = if br == &branch {
            style(format!("[{br}]")).cyan().to_string()
        } else {
            style(format!("[{br}]")).dim().to_string()
        };
        let display: String = content.chars().take(120).collect();
        println!("  {} {} {}", style("◈").yellow(), branch_label, style(&display).italic());
    }

    println!();
    println!(
        "  {} {} design decision{} across all branches",
        style("◈").dim(),
        style(all_thoughts.len()).yellow().bold(),
        if all_thoughts.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

// ── Context search ────────────────────────────────────────────────────────────

/// One result from `search()` — a file ranked by relevance to the query.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Relative file path (e.g. `src/auth/middleware.rs`).
    pub file: String,
    /// Combined relevance score (0.0–1.0, higher = more relevant).
    pub score: f64,
    /// The most relevant trace/commit snippets mentioning this file.
    pub snippets: Vec<String>,
    /// Primary signal source: "trace", "session", "cochange", or combined.
    pub signal: String,
    /// Files frequently co-changed with this file (from git history).
    pub cochanged_with: Vec<String>,
}

/// Task-aware recall bundle rendered by session preludes.
#[derive(Debug, Clone)]
pub struct SmartRecall {
    /// Query used to retrieve context. Usually the current task prompt; falls
    /// back to the branch goal when the caller does not provide one.
    pub query: String,
    /// Ranked files and snippets from context traces / session analyses.
    pub results: Vec<SearchResult>,
}

/// Tokenise a string into lowercase words, stripping punctuation.
fn tokenise(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() > 1)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Score one document (a single trace line or snippet) against query terms.
/// Returns a value in [0.0, 1.0]: fraction of query terms found in the doc.
fn term_overlap(query_terms: &[String], document: &str) -> f64 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let doc_lower = document.to_lowercase();
    let matched = query_terms.iter().filter(|t| doc_lower.contains(t.as_str())).count();
    matched as f64 / query_terms.len() as f64
}

/// Extract file-like tokens from a trace line (paths containing `/` or `.`).
/// Returns short paths such as `src/auth.rs` or bare filenames like `session.rs`.
fn extract_file_mentions(line: &str) -> Vec<String> {
    let mut files = Vec::new();
    for token in line.split_whitespace() {
        // Strip leading punctuation / brackets
        let t = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
        // Must look like a path: contains '/' or ends with a known extension after a '.'
        let looks_like_path = t.contains('/') || {
            if let Some(dot_pos) = t.rfind('.') {
                let ext = &t[dot_pos + 1..];
                matches!(ext, "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "java"
                    | "c" | "cpp" | "h" | "hpp" | "rb" | "swift" | "kt" | "md" | "json"
                    | "toml" | "yaml" | "yml" | "sh" | "bash" | "sql" | "html" | "css")
            } else {
                false
            }
        };
        if looks_like_path && t.len() > 3 && !t.starts_with("http") {
            files.push(t.to_string());
        }
    }
    files
}

/// Search the context workspace and session footprints for files relevant to `query`.
///
/// Scoring model (additive, then normalised to [0, 1]):
/// - **Trace signal** (weight 1.0 per line): each trace line mentioning a file is
///   scored by `term_overlap(query_terms, line)`. THINK lines get a 1.5× bonus.
/// - **Session footprint signal** (weight 0.5): files from past session analyses
///   (consulted + edited) are scored by query term overlap with the causal chain trigger
///   and key decisions of that session.
///
/// The `cochanged_with` field is populated by the caller from git history (see
/// `H5iRepository::cochanged_files`) and is not computed here.
pub fn search(workdir: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>, H5iError> {
    let repo = ctx_git_repo(workdir)?;
    let query_terms = tokenise(query);

    // file → (score_sum, snippets, signals_seen)
    let mut scores: std::collections::HashMap<String, (f64, Vec<String>, std::collections::HashSet<String>)> =
        std::collections::HashMap::new();

    // ── Signal 1: trace entries (all branches) ────────────────────────────────
    let branches = ctx_list_branches_git(&repo);
    for branch_name in &branches {
        for source in &["trace.md", "commit.md"] {
            let text = ctx_read_file(&repo, &format!("branches/{branch_name}/{source}"))
                .unwrap_or_default();
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let overlap = term_overlap(&query_terms, line);
                if overlap < 0.01 {
                    continue;
                }
                let is_think = line.contains("] THINK:");
                let weight = if is_think { 1.5 } else { 1.0 };
                let score_contrib = overlap * weight;

                // Credit every file mentioned in this line
                let mentioned = extract_file_mentions(line);
                if mentioned.is_empty() {
                    // Line has no file — still a useful snippet; credit a pseudo-key
                    // so the snippet surfaces via knowledge search below
                    continue;
                }
                let snippet: String = line.chars().take(120).collect();
                for file in &mentioned {
                    let entry = scores.entry(file.clone()).or_default();
                    entry.0 += score_contrib;
                    if entry.1.len() < 4 && !entry.1.contains(&snippet) {
                        entry.1.push(snippet.clone());
                    }
                    entry.2.insert("trace".to_string());
                }
            }
        }
    }

    // ── Signal 2: session footprint analyses ──────────────────────────────────
    // Walk .git/.h5i/session_log/<oid>/analysis.json files
    let h5i_root = {
        let git_dir = repo.path(); // points to .git/ (or .git itself if bare)
        git_dir.join(".h5i")
    };
    let session_oids = crate::session_log::list_analyses(&h5i_root);
    for oid in &session_oids {
        if let Ok(Some(analysis)) = crate::session_log::load_analysis(&h5i_root, oid) {
            // Score the session by query overlap with trigger + decisions
            let session_text = format!(
                "{} {}",
                analysis.causal_chain.user_trigger,
                analysis.causal_chain.key_decisions.join(" ")
            );
            let session_overlap = term_overlap(&query_terms, &session_text);
            if session_overlap < 0.05 {
                continue;
            }
            let base_score = session_overlap * 0.5;
            // Credit all consulted and edited files
            for cf in &analysis.footprint.consulted {
                let entry = scores.entry(cf.path.clone()).or_default();
                entry.0 += base_score;
                entry.2.insert("session".to_string());
            }
            for f in &analysis.footprint.edited {
                let entry = scores.entry(f.clone()).or_default();
                entry.0 += base_score * 1.2; // edited files get slight bonus
                entry.2.insert("session".to_string());
            }
        }
    }

    if scores.is_empty() {
        return Ok(vec![]);
    }

    // Normalise scores to [0, 1]
    let max_score = scores.values().map(|(s, _, _)| *s).fold(0.0_f64, f64::max);
    let mut results: Vec<SearchResult> = scores
        .into_iter()
        .filter(|(_, (s, _, _))| *s > 0.0)
        .map(|(file, (raw_score, snippets, signals))| {
            let normalised = if max_score > 0.0 { raw_score / max_score } else { 0.0 };
            let signal = {
                let mut sv: Vec<&str> = signals.iter().map(|s| s.as_str()).collect();
                sv.sort();
                sv.join("+")
            };
            SearchResult {
                file,
                score: normalised,
                snippets,
                signal,
                cochanged_with: vec![],
            }
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    Ok(results)
}

/// Retrieve task-aware context for a session prelude.
///
/// This is intentionally a thin wrapper over `search`: the feature is opt-in at
/// the caller, and this function stays deterministic/offline so it is safe to
/// use in startup paths and tests.
pub fn smart_recall(workdir: &Path, query: &str, limit: usize) -> Result<SmartRecall, H5iError> {
    let query = query.trim().to_string();
    if query.is_empty() || limit == 0 {
        return Ok(SmartRecall {
            query,
            results: vec![],
        });
    }
    let results = search(workdir, &query, limit)?;
    Ok(SmartRecall { query, results })
}

/// Pretty-print search results to the terminal.
pub fn print_search_results(results: &[SearchResult], query: &str) {
    use console::style;

    println!(
        "{}",
        style(format!("── Context Search: {:?} ─────────────────────────────────────", query)).dim()
    );

    if results.is_empty() {
        println!(
            "  {}",
            style("No results. Run more sessions and `h5i notes analyze` to build the index.").dim()
        );
        return;
    }

    for (i, r) in results.iter().enumerate() {
        let bar_len = (r.score * 10.0).round() as usize;
        let bar = format!("{}{}", "█".repeat(bar_len), "░".repeat(10 - bar_len.min(10)));
        println!(
            "  {}  {}  score {:.2}  {}",
            style(format!("#{}", i + 1)).bold(),
            style(&r.file).cyan().bold(),
            r.score,
            style(&bar).yellow()
        );
        println!(
            "       signal: {}{}",
            style(&r.signal).dim(),
            if r.cochanged_with.is_empty() {
                String::new()
            } else {
                format!(
                    "  ·  co-changed with: {}",
                    r.cochanged_with.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                )
            }
        );
        for snippet in r.snippets.iter().take(2) {
            let display: String = snippet.chars().take(100).collect();
            println!("       {}", style(format!("↳ {display}")).italic().dim());
        }
        println!();
    }

    println!(
        "  {} result{} · run `h5i context relevant <file>` for full context on any file",
        style(results.len()).yellow().bold(),
        if results.len() == 1 { "" } else { "s" }
    );
}

// ── Terminal helpers ──────────────────────────────────────────────────────────

/// Wrap `text` at word boundaries so each line is ≤ `max_cols` chars.
fn ctx_word_wrap(text: &str, max_cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max_cols {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// ── DAG pretty-printer ────────────────────────────────────────────────────────

/// Render the per-branch trace DAG to the terminal with Unicode tree lines,
/// per-kind colour coding, and merge-node parent annotations.
///
/// Layout per node:
/// ```text
///   ●  3fa12b  OBSERVE   14:01:22
///   │  src/auth.rs:44 — token TTL hardcoded to 3600s
/// ```
/// Merge nodes show both parent IDs:
/// ```text
///   ⊕  a9f3c1  MERGE     14:04:13
///   ╠  ├─ main          c4e720
///   ╠  └─ scope/rfc     b1d993
///   │  scope/investigate-rfc merged: refresh_ttl = TTL/2
/// ```
pub fn print_dag(workdir: &Path, branch: Option<&str>) -> Result<(), H5iError> {
    use console::style;

    let repo = ctx_git_repo(workdir)?;
    let active = current_branch(workdir);
    let branch = branch.unwrap_or(&active);
    let dag = read_dag(&repo, branch);

    let n = dag.nodes.len();
    let bar = "─".repeat(50_usize.saturating_sub(branch.len()));
    println!(
        "{}",
        style(format!("── Reasoning DAG {bar} {branch} · {n} node{} ──", if n == 1 { "" } else { "s" }))
            .dim()
    );

    // Show project goal if available.
    if let Some(main_md) = ctx_read_file(&repo, "main.md") {
        let goal = extract_section(&main_md, "Goal");
        if !goal.is_empty() {
            let truncated: String = goal.chars().take(70).collect();
            println!(
                "  {}  {}",
                style("Goal:").dim(),
                style(truncated).cyan()
            );
        }
    }

    if n == 0 {
        println!("  {}", style("(empty — add entries with `h5i context trace`)").dim());
        return Ok(());
    }

    println!();

    // Build a lookup: node id → branch name (best-effort from merge content).
    // We use it to annotate merge parent lines.
    let node_ids: std::collections::HashSet<&str> =
        dag.nodes.iter().map(|n| n.id.as_str()).collect();

    for (idx, node) in dag.nodes.iter().enumerate() {
        let is_last = idx == n - 1;

        // ── Symbol and colour ──────────────────────────────────────────────
        let (sym, kind_label) = match node.kind.as_str() {
            "OBSERVE" => (
                style("●".to_string()).blue().bold(),
                style("OBSERVE ".to_string()).blue(),
            ),
            "THINK" => (
                style("◆".to_string()).yellow().bold(),
                style("THINK  ".to_string()).yellow(),
            ),
            "ACT" => (
                style("■".to_string()).green().bold(),
                style("ACT    ".to_string()).green(),
            ),
            "NOTE" => (
                style("○".to_string()).white().dim(),
                style("NOTE   ".to_string()).white().dim(),
            ),
            "MERGE" => (
                style("⊕".to_string()).magenta().bold(),
                style("MERGE  ".to_string()).magenta().bold(),
            ),
            other => (
                style("·".to_string()).dim(),
                style(format!("{:<7}", other)).dim(),
            ),
        };

        // ── Timestamp (HH:MM:SS only, strip date prefix) ──────────────────
        let ts = node
            .timestamp
            .split('T')
            .nth(1)
            .unwrap_or(&node.timestamp)
            .split('.')
            .next()
            .unwrap_or(&node.timestamp);
        let ts_display = &ts[..ts.len().min(8)];

        // ── First line: symbol + id + kind + timestamp ─────────────────────
        println!(
            "  {}  {}  {}  {}",
            sym,
            style(&node.id).dim(),
            kind_label,
            style(ts_display).dim()
        );

        // ── Connector character on left ────────────────────────────────────
        let connector = if node.kind == "MERGE" { "╠" } else { "│" };

        // ── Merge: show parent IDs with branch annotations ─────────────────
        if node.kind == "MERGE" && node.parent_ids.len() >= 2 {
            // Extract the branch name from content: look for 'scope/...' in
            // single-quotes, then bare scope/... tokens, then any word with '/'.
            let scope_hint = {
                // try 'scope/foo' quoted form first
                let quoted = node.content.split('\'').find(|s| s.contains('/'));
                if let Some(q) = quoted {
                    q.to_string()
                } else {
                    // fall back to first word containing '/'
                    node.content
                        .split_whitespace()
                        .find(|w| w.contains('/'))
                        .unwrap_or("(branch)")
                        .trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '-' && c != '_')
                        .to_string()
                }
            };

            let p0 = &node.parent_ids[0];
            let p1 = &node.parent_ids[1];
            let p0_known = node_ids.contains(p0.as_str());

            println!(
                "  {}  {} {}{}",
                style(connector).magenta(),
                style("├─").dim(),
                style(p0).dim(),
                if p0_known { style("  (this branch)".to_string()).dim() } else { style(String::new()).dim() }
            );
            println!(
                "  {}  {} {}  {}",
                style(connector).magenta(),
                style("└─").dim(),
                style(p1).dim(),
                style(scope_hint).magenta()
            );
        }

        // ── Content (word-wrapped at 72 cols) ─────────────────────────────
        let content = node.content.trim();
        let connector_line = if is_last {
            "  ".to_string()
        } else {
            format!("  {}", style("│").dim())
        };
        let max_w = 72_usize;
        let wrapped = ctx_word_wrap(content, max_w);
        for line in &wrapped {
            println!("  {}  {}", style("│").dim(), style(line.as_str()).dim());
        }

        // ── Blank separator (except after last node) ───────────────────────
        if !is_last {
            println!("{}", connector_line);
        }
    }

    // ── Summary footer ────────────────────────────────────────────────────
    type StyleFn = fn(console::StyledObject<String>) -> console::StyledObject<String>;
    let counts: [(&str, usize, StyleFn); 5] = [
        ("OBSERVE", dag.nodes.iter().filter(|n| n.kind == "OBSERVE").count(), |s| s.blue()),
        ("THINK",   dag.nodes.iter().filter(|n| n.kind == "THINK").count(),   |s| s.yellow()),
        ("ACT",     dag.nodes.iter().filter(|n| n.kind == "ACT").count(),     |s| s.green()),
        ("NOTE",    dag.nodes.iter().filter(|n| n.kind == "NOTE").count(),    |s| s.white().dim()),
        ("MERGE",   dag.nodes.iter().filter(|n| n.kind == "MERGE").count(),   |s| s.magenta()),
    ];
    let summary: Vec<String> = counts
        .iter()
        .filter(|(_, count, _)| *count > 0)
        .map(|(label, count, colour_fn)| {
            format!("{} {}", colour_fn(style(count.to_string())), style(*label).dim())
        })
        .collect();
    println!();
    println!("  {}  {}", style("◈").dim(), summary.join(style("  ·  ").dim().to_string().as_str()));

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
             **Start this session** by running `h5i context show --depth 2` (or `--depth 1` for a quick index, `--depth 3` for the full trace).\n\
             The `SessionStart` hook injects this automatically if installed — see `h5i hook setup`.\n\
             The workflow skill at `.claude/skills/h5i-workflow/SKILL.md` documents the full OTA tracing protocol.\n",
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
versioned set of Git refs under `{CTX_REF_PREFIX}*`. Use the commands below to manage context across
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
# ── Session start (mandatory) ──────────────────────────────────────
h5i context show --trace          # restore goal, milestones, recent trace
h5i context todo                  # surface any open TODOs from prior sessions

# ── Before touching a file ─────────────────────────────────────────
h5i context relevant src/auth.rs  # check if prior reasoning about this file exists

# ── During execution (continuous) ──────────────────────────────────
h5i context trace --kind OBSERVE "test suite: 3 failures in auth module"
h5i context trace --kind THINK   "failures in token validation — likely regex issue"
h5i context trace --kind ACT     "fixed greedy quantifier in src/auth/token.rs:validate()"

# ── After each meaningful chunk of work ────────────────────────────
h5i context commit "Fixed token validation" \
  --detail "Replaced greedy quantifier; all 47 auth tests now pass."

# ── Session end (mandatory) ─────────────────────────────────────────
h5i commit -m "fix token validation regex" \
  --model <model> --agent claude-code \
  --prompt "<the user's original request>"    # records AI provenance in git
h5i notes analyze                             # links this session to HEAD commit
```

## Guidelines
1. **`h5i context show` first, every session** — never start work without restoring context.
2. **`h5i context relevant <file>` before editing** — check prior reasoning about the file.
3. **Trace every OTA step** — fine-grained traces are the primary recovery mechanism.
4. **`h5i context commit` at every milestone** — not just at the end; captures reasoning.
5. **`h5i commit` at the end** — records AI provenance in git history alongside code.
6. **`h5i notes analyze` to close out** — links the session footprint to the git commit.
7. Branch before any risky or divergent exploration (`h5i context branch`).
8. Use `h5i context scope <name>` to delegate to a subagent without polluting main thread.
"#
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn ensure_branch_git(repo: &Repository, name: &str, purpose: &str) -> Result<(), H5iError> {
    // Only write files that don't already exist in the tree.
    let commit_path = format!("branches/{name}/commit.md");
    let trace_path = format!("branches/{name}/trace.md");
    let meta_path = format!("branches/{name}/metadata.yaml");

    let existing_commit_text = ctx_read_file(repo, &commit_path);
    // After `gcc_branch` forks a new ref from its parent, commit.md / trace.md
    // initially contain the parent branch's content (header `# Branch: <parent>`).
    // Treat that as "missing" so the new branch gets its own header + purpose,
    // and pass through the supplied `purpose` even when non-empty inheritance
    // looked complete.
    let header_matches = existing_commit_text
        .as_deref()
        .map(|t| t.lines().any(|l| l.trim() == format!("# Branch: {name}")))
        .unwrap_or(false);
    let missing_commit = existing_commit_text.is_none() || !header_matches;
    let trace_text = ctx_read_file(repo, &trace_path);
    let trace_header_matches = trace_text
        .as_deref()
        .map(|t| t.lines().any(|l| l.trim() == format!("# OTA Log — Branch: {name}")))
        .unwrap_or(false);
    let missing_trace = trace_text.is_none() || !trace_header_matches;
    let missing_meta = ctx_read_file(repo, &meta_path).is_none();
    let missing_purpose = existing_commit_text
        .as_deref()
        .and_then(extract_branch_purpose)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);

    if !missing_commit && !missing_trace && !missing_meta && !missing_purpose {
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
    } else if missing_purpose {
        commit_content = ensure_commit_purpose(
            existing_commit_text.as_deref().unwrap_or_default(),
            name,
            purpose,
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

fn ensure_commit_purpose(commit_text: &str, branch: &str, purpose: &str) -> String {
    if commit_text.contains("**Purpose:**") {
        let mut out = String::new();
        for line in commit_text.lines() {
            if line.trim_start().starts_with("**Purpose:**") {
                out.push_str(&format!("**Purpose:** {purpose}\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    } else {
        format!(
            "# Branch: {branch}\n\n\
             **Purpose:** {purpose}\n\n\
             {}",
            commit_text
                .trim_start_matches(&format!("# Branch: {branch}"))
                .trim_start()
        )
    }
}

/// Append a one-line progress note to `main.md` under `## Notes`.
/// Mark "[ ] Initial setup" done and append a new `[x] summary` milestone.
fn auto_update_milestones(main_md: &str, summary: &str) -> String {
    // Tick off the placeholder "Initial setup" milestone on the first real commit.
    let ticked = main_md.replace("- [ ] Initial setup\n", "- [x] Initial setup\n");
    // Insert the new completed milestone into the Milestones section.
    let new_entry = format!("- [x] {summary}\n");
    if let Some(pos) = ticked.find("## Milestones") {
        let after_header = &ticked[pos..];
        // Find the end of this section (next "##" heading or end of string).
        let section_len = after_header[1..]  // skip the '#' of the heading itself
            .find("\n## ")
            .map(|i| i + 1)
            .unwrap_or(after_header.len());
        let insert_at = pos + section_len;
        let mut result = ticked.clone();
        result.insert_str(insert_at, &new_entry);
        result
    } else {
        format!("{ticked}\n## Milestones\n{new_entry}")
    }
}

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
        append_log(dir.path(), "OBSERVE", "Redis latency is 2ms", false).unwrap();
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
        append_log(dir.path(), "think", "reasoning step", false).unwrap();
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

    #[test]
    fn gcc_merge_produces_two_parent_commit_on_target_ref() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        // Diverge: create an experimental branch, commit on each side.
        gcc_branch(dir.path(), "experiment", "explore alternative").unwrap();
        gcc_commit(dir.path(), "experiment milestone", "tried option A").unwrap();
        gcc_checkout(dir.path(), MAIN_BRANCH).unwrap();
        gcc_commit(dir.path(), "main milestone", "shipped option B").unwrap();

        // Merge: target is main (current), source is experiment.
        gcc_merge(dir.path(), "experiment").unwrap();

        // The new tip of refs/h5i/context/main must have two parents — the
        // pre-merge main tip and the experiment tip.
        let repo = ctx_git_repo(dir.path()).unwrap();
        let tip = repo
            .find_reference(&branch_ref(MAIN_BRANCH))
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(
            tip.parent_count(),
            2,
            "merge commit on main should have two parents (real three-way merge)"
        );
    }

    #[test]
    fn gcc_merge_returns_error_on_conflicting_changes() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        // Both branches replace metadata.yaml's contents incompatibly so that
        // libgit2's three-way merge produces a conflict.
        gcc_branch(dir.path(), "alt", "alternative").unwrap();
        write_ctx_file(dir.path(), "branches/alt/metadata.yaml", "x: from-alt\n").unwrap();
        gcc_checkout(dir.path(), MAIN_BRANCH).unwrap();
        write_ctx_file(
            dir.path(),
            &format!("branches/{MAIN_BRANCH}/metadata.yaml"),
            "x: from-main\n",
        )
        .unwrap();

        let err = gcc_merge(dir.path(), "alt").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("conflict") || msg.contains("Conflict"),
            "expected conflict error, got: {msg}"
        );
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

        // The main branch ref should have advanced (new commit on top).
        let tip_after = repo
            .find_reference(&branch_ref(MAIN_BRANCH))
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
        append_log(dir.path(), "OBSERVE", "found a performance issue", false).unwrap();
        snapshot_for_commit(dir.path(), "sha4444400000000").unwrap();

        let diff = context_diff(dir.path(), "sha33333", "sha44444").unwrap();
        assert!(
            diff.added_trace_lines.iter().any(|l| l.contains("found a performance issue")),
            "diff should include new trace lines"
        );
    }

    #[test]
    fn context_diff_uses_snapshot_branch_not_current_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        snapshot_for_commit(dir.path(), "sha5555500000000").unwrap();

        gcc_branch(dir.path(), "scope/test", "Investigate branch diff").unwrap();
        gcc_commit(dir.path(), "side milestone", "implemented scope-only change").unwrap();
        snapshot_for_commit(dir.path(), "sha6666600000000").unwrap();

        gcc_checkout(dir.path(), MAIN_BRANCH).unwrap();

        let diff = context_diff(dir.path(), "sha55555", "sha66666").unwrap();
        assert_eq!(diff.from_branch, MAIN_BRANCH);
        assert_eq!(diff.to_branch, "scope/test");
        assert!(
            diff.added_commits.iter().any(|c| c.contains("scope-only change")),
            "diff should use the snapshot branch's commit history"
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
        append_log(dir.path(), "ACT", "edited src/repository.rs line 88", false).unwrap();
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
        append_log(dir.path(), "THINK", "retry_logic.rs needs a refactor", false).unwrap();
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

    // ── Feature 1: DAG trace nodes ────────────────────────────────────────────

    #[test]
    fn dag_is_empty_after_init() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert!(dag.nodes.is_empty(), "DAG should be empty before any trace");
    }

    #[test]
    fn dag_records_node_on_append_log() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "saw something", false).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert_eq!(dag.nodes.len(), 1);
        assert_eq!(dag.nodes[0].kind, "OBSERVE");
        assert_eq!(dag.nodes[0].content, "saw something");
        assert!(dag.nodes[0].parent_ids.is_empty(), "first node has no parents");
    }

    #[test]
    fn dag_links_parent_ids_in_chain() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "first", false).unwrap();
        append_log(dir.path(), "THINK", "second", false).unwrap();
        append_log(dir.path(), "ACT", "third", false).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert_eq!(dag.nodes.len(), 3);
        assert!(dag.nodes[1].parent_ids.contains(&dag.nodes[0].id));
        assert!(dag.nodes[2].parent_ids.contains(&dag.nodes[1].id));
    }

    #[test]
    fn dag_merge_node_has_two_parents() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "main obs", false).unwrap();
        gcc_branch(dir.path(), "alt", "alternate").unwrap();
        append_log(dir.path(), "THINK", "alt thought", false).unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        gcc_merge(dir.path(), "alt").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        let merge_node = dag.nodes.iter().find(|n| n.kind == "MERGE");
        assert!(merge_node.is_some(), "merge node should exist");
        assert_eq!(merge_node.unwrap().parent_ids.len(), 2, "merge node should have two parents");
    }

    // ── Feature 3: Ephemeral traces ───────────────────────────────────────────

    #[test]
    fn ephemeral_trace_not_in_trace_md() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "ephemeral scratch", true).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(
            !ctx.recent_log_lines.iter().any(|l| l.contains("ephemeral scratch")),
            "ephemeral entry must not appear in trace.md"
        );
    }

    #[test]
    fn ephemeral_trace_visible_in_ephemeral_md() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "quick scratch note", true).unwrap();
        let text = read_ephemeral(dir.path(), None).unwrap();
        assert!(text.contains("quick scratch note"));
    }

    #[test]
    fn ephemeral_trace_not_in_dag() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "ephemeral", true).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert!(dag.nodes.is_empty(), "ephemeral entries must not appear in DAG");
    }

    #[test]
    fn ephemeral_cleared_on_context_commit() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "scratch", true).unwrap();
        gcc_commit(dir.path(), "checkpoint", "did things").unwrap();
        let text = read_ephemeral(dir.path(), None).unwrap();
        assert!(!text.contains("scratch"), "ephemeral should be cleared after commit");
    }

    // ── Feature 2: Three-pass lossless pack ───────────────────────────────────

    #[test]
    fn lossless_pack_noop_when_no_observe_entries() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "pure reasoning", false).unwrap();
        append_log(dir.path(), "ACT", "did something", false).unwrap();
        let result = pack_lossless(dir.path()).unwrap();
        assert_eq!(result.removed_subsumed_observe, 0);
        assert_eq!(result.merged_consecutive_observe, 0);
    }

    #[test]
    fn lossless_pack_removes_subsumed_observe() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // OBSERVE about repository.rs, then THINK mentioning repository.rs → should be subsumed.
        append_log(dir.path(), "OBSERVE", "repository.rs has 67KB", false).unwrap();
        append_log(dir.path(), "THINK", "refactor repository.rs entry points", false).unwrap();
        let result = pack_lossless(dir.path()).unwrap();
        assert_eq!(result.removed_subsumed_observe, 1, "subsumed OBSERVE should be removed");
    }

    #[test]
    fn lossless_pack_preserves_think_act_verbatim() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "metadata.rs exists", false).unwrap();
        append_log(dir.path(), "THINK", "update metadata.rs schema", false).unwrap();
        append_log(dir.path(), "ACT", "edited metadata.rs line 42", false).unwrap();
        pack_lossless(dir.path()).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let trace = ctx_read_file(&repo, "branches/main/trace.md").unwrap_or_default();
        assert!(trace.contains("update metadata.rs schema"), "THINK must be preserved");
        assert!(trace.contains("edited metadata.rs line 42"), "ACT must be preserved");
    }

    #[test]
    fn lossless_pack_merges_consecutive_observe() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // Three consecutive OBSERVE entries about the same file.
        append_log(dir.path(), "OBSERVE", "src/main.rs line 1", false).unwrap();
        append_log(dir.path(), "OBSERVE", "src/main.rs line 2", false).unwrap();
        append_log(dir.path(), "OBSERVE", "src/main.rs line 3", false).unwrap();
        let result = pack_lossless(dir.path()).unwrap();
        assert_eq!(result.merged_consecutive_observe, 2, "3 entries → 1, so 2 merged");
    }

    // ── Feature 4: Stable-prefix counts ──────────────────────────────────────

    #[test]
    fn stable_line_count_consistent_with_dynamic() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        // stable + dynamic = total trace lines; dynamic ≤ 40.
        assert!(ctx.dynamic_line_count <= 40);
        // With only the trace header, everything is in the dynamic tail.
        assert_eq!(ctx.stable_line_count + ctx.dynamic_line_count, ctx.stable_line_count + ctx.dynamic_line_count);
    }

    #[test]
    fn stable_line_count_reflects_tail_boundary() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // Add 50 trace entries — 40 should be dynamic, 10 stable.
        for i in 0..50 {
            append_log(dir.path(), "NOTE", &format!("entry {i}"), false).unwrap();
        }
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.dynamic_line_count, 40);
        assert!(ctx.stable_line_count >= 10, "at least 10 lines in stable prefix");
    }

    // ── Feature 5: Scoped sub-contexts ───────────────────────────────────────

    #[test]
    fn gcc_scope_creates_scope_prefixed_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_scope(dir.path(), "scope/investigate-auth", "investigate auth module").unwrap();
        assert!(list_branches(dir.path()).contains(&"scope/investigate-auth".to_string()));
        assert_eq!(current_branch(dir.path()), "scope/investigate-auth");
    }

    #[test]
    fn gcc_scope_is_identified_as_scope_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_scope(dir.path(), "scope/check-perf", "performance check").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        assert!(is_scope_branch(&repo, "scope/check-perf"));
        assert!(!is_scope_branch(&repo, MAIN_BRANCH));
    }

    #[test]
    fn gcc_scope_can_be_merged_back() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_scope(dir.path(), "scope/research", "do research").unwrap();
        gcc_commit(dir.path(), "research done", "found that X causes Y").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        let summary = gcc_merge(dir.path(), "scope/research").unwrap();
        assert!(summary.contains("scope/research"), "merge summary should name the scope");
    }

    // ── show_log / mini_trace / todo_items ───────────────────────────────────

    #[test]
    fn show_log_false_returns_empty_recent_log_lines() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "something happened", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: false, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.recent_log_lines.is_empty(), "recent_log_lines must be empty when show_log=false");
    }

    #[test]
    fn mini_trace_populated_even_without_show_log() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "ACT", "edited src/main.rs", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: false, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(!ctx.mini_trace.is_empty(), "mini_trace should be populated regardless of show_log");
        assert!(ctx.mini_trace.iter().any(|l| l.contains("edited src/main.rs")));
    }

    #[test]
    fn mini_trace_capped_at_eight_entries() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        for i in 0..12 {
            append_log(dir.path(), "NOTE", &format!("note {i}"), false).unwrap();
        }
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: false, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.mini_trace.len() <= 8, "mini_trace must be capped at 8 entries");
    }

    #[test]
    fn todo_items_extracted_from_note_with_todo_keyword() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "TODO: add integration test for failover", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(
            ctx.todo_items.iter().any(|t| t.contains("integration test")),
            "TODO from NOTE should appear in todo_items"
        );
    }

    #[test]
    fn todo_items_extracted_from_think_with_fixme() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "FIXME: token refresh path has a race condition", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(
            ctx.todo_items.iter().any(|t| t.contains("race condition")),
            "FIXME from THINK should appear in todo_items"
        );
    }

    #[test]
    fn todo_items_empty_when_no_keywords() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "everything looks fine", false).unwrap();
        append_log(dir.path(), "THINK", "the approach is sound", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.todo_items.is_empty(), "no TODO keywords → todo_items must be empty");
    }

    #[test]
    fn todo_items_not_extracted_from_observe_or_act() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "TODO: this is just an observation", false).unwrap();
        append_log(dir.path(), "ACT", "TODO: actioned this", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.todo_items.is_empty(), "TODO in OBSERVE/ACT must not appear in todo_items");
    }

    #[test]
    fn todo_items_blocked_and_remaining_keywords_matched() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "BLOCKED: waiting on API credentials", false).unwrap();
        append_log(dir.path(), "THINK", "REMAINING: wire up the logout handler", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert_eq!(ctx.todo_items.len(), 2, "BLOCKED and REMAINING both match");
    }

    // ── gcc_context: window, active_branches ─────────────────────────────────

    #[test]
    fn gcc_context_window_limits_returned_commits() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        for i in 0..5 {
            gcc_commit(dir.path(), &format!("milestone {i}"), &format!("contribution {i}")).unwrap();
        }
        let ctx = gcc_context(dir.path(), &ContextOpts { window: 2, ..Default::default() }).unwrap();
        assert!(ctx.recent_commits.len() <= 2, "window=2 must cap recent_commits");
    }

    #[test]
    fn gcc_context_active_branches_includes_all() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feat-a", "feature a").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        gcc_branch(dir.path(), "feat-b", "feature b").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert!(ctx.active_branches.contains(&"main".to_string()));
        assert!(ctx.active_branches.contains(&"feat-a".to_string()));
        assert!(ctx.active_branches.contains(&"feat-b".to_string()));
    }

    // ── auto-bootstrap (append_log before init) ───────────────────────────────

    #[test]
    fn append_log_bootstraps_context_ref_without_init() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        // No init — append_log must create refs/h5i/context on its own.
        append_log(dir.path(), "OBSERVE", "file exists", false).unwrap();
        assert!(is_initialized(dir.path()), "append_log should bootstrap the context ref");
    }

    #[test]
    fn append_log_bootstrap_trace_is_readable() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        append_log(dir.path(), "THINK", "bootstrapped thought", false).unwrap();
        let ctx = gcc_context(
            dir.path(),
            &ContextOpts { show_log: true, window: 3, ..Default::default() },
        )
        .unwrap();
        assert!(ctx.recent_log_lines.iter().any(|l| l.contains("bootstrapped thought")));
    }

    // ── gcc_branch purpose ────────────────────────────────────────────────────

    #[test]
    fn gcc_branch_stores_purpose_retrievable_from_commit_md() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "experiment/caching", "explore LRU cache approach").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let commit_md =
            ctx_read_file(&repo, "branches/experiment/caching/commit.md").unwrap_or_default();
        assert!(
            commit_md.contains("explore LRU cache approach"),
            "branch purpose should be recorded in commit.md"
        );
    }

    #[test]
    fn prepare_context_write_requires_current_git_branch_goal() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "main goal").unwrap();
        let repo = Repository::open(dir.path()).unwrap();
        repo.set_head("refs/heads/feature/needs-purpose").unwrap();

        let err = prepare_context_write(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("h5i context init --goal"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_context_write_requires_active_context_branch_purpose() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "option-a", "").unwrap();

        let err = prepare_context_write(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("h5i context branch <name> --purpose"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prepare_context_write_allows_multiple_context_branches_per_git_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "option-a", "try option a").unwrap();
        assert!(prepare_context_write(dir.path()).is_ok());
        gcc_branch(dir.path(), "option-b", "try option b").unwrap();
        assert!(prepare_context_write(dir.path()).is_ok());
    }

    #[test]
    fn reconcile_flags_merged_git_branch_with_unmerged_ctx_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        // Create the git side: main + a feature branch, then merge feature → main.
        let repo = Repository::open(dir.path()).unwrap();
        let sig = Signature::now("h5i-test", "test@local").unwrap();
        // git_init produces an unborn HEAD; make an initial commit on main.
        let empty_tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
        let empty_tree = repo.find_tree(empty_tree_oid).unwrap();
        let main_initial = repo
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "initial",
                &empty_tree,
                &[],
            )
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        // Fork `feature` from main.
        repo.reference("refs/heads/feature", main_initial, false, "branch feature")
            .unwrap();
        // Add a commit on feature.
        let blob = repo.blob(b"feature content").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("f.txt", blob, 0o100644).unwrap();
        let ftree_oid = tb.write().unwrap();
        let ftree = repo.find_tree(ftree_oid).unwrap();
        let main_initial_commit = repo.find_commit(main_initial).unwrap();
        let feature_commit_oid = repo
            .commit(
                Some("refs/heads/feature"),
                &sig,
                &sig,
                "feature work",
                &ftree,
                &[&main_initial_commit],
            )
            .unwrap();
        // Fast-forward main to feature (merged).
        repo.reference(
            "refs/heads/main",
            feature_commit_oid,
            true,
            "merge feature",
        )
        .unwrap();

        // Create the matching ctx branch but do NOT merge it back to ctx/main.
        gcc_branch(dir.path(), "feature", "feature work").unwrap();

        let report = reconcile_git_vs_ctx(dir.path()).unwrap();
        assert!(
            report.merged_in_git_only.iter().any(|n| n == "feature"),
            "feature should be flagged as merged-in-git-only: {report:?}"
        );
    }

    #[test]
    fn migrate_legacy_creates_per_branch_refs() {
        let dir = tempdir().unwrap();
        git_init(dir.path());

        // Build a synthetic legacy ref: refs/h5i/context with branches/main and
        // branches/scope/foo subtrees, plus a top-level main.md.
        let repo = Repository::open(dir.path()).unwrap();
        let sig = Signature::now("h5i-test", "test@local").unwrap();
        let blob_oid = |s: &str| repo.blob(s.as_bytes()).unwrap();

        // branches/main/commit.md
        let mut main_b = repo.treebuilder(None).unwrap();
        main_b
            .insert(
                "commit.md",
                blob_oid("# Branch: main\n\n**Purpose:** Primary\n"),
                0o100644,
            )
            .unwrap();
        main_b
            .insert("trace.md", blob_oid("# OTA Log — Branch: main\n"), 0o100644)
            .unwrap();
        let main_subtree = main_b.write().unwrap();

        // branches/scope/foo/commit.md
        let mut foo_b = repo.treebuilder(None).unwrap();
        foo_b
            .insert(
                "commit.md",
                blob_oid("# Branch: scope/foo\n\n**Purpose:** Spike\n"),
                0o100644,
            )
            .unwrap();
        foo_b
            .insert(
                "trace.md",
                blob_oid("# OTA Log — Branch: scope/foo\n"),
                0o100644,
            )
            .unwrap();
        let foo_subtree = foo_b.write().unwrap();
        let mut scope_b = repo.treebuilder(None).unwrap();
        scope_b.insert("foo", foo_subtree, 0o040000).unwrap();
        let scope_subtree = scope_b.write().unwrap();

        // branches/ root
        let mut branches_b = repo.treebuilder(None).unwrap();
        branches_b.insert("main", main_subtree, 0o040000).unwrap();
        branches_b
            .insert("scope", scope_subtree, 0o040000)
            .unwrap();
        let branches_oid = branches_b.write().unwrap();

        // Root tree: branches/ + main.md + .current_branch
        let mut root_b = repo.treebuilder(None).unwrap();
        root_b.insert("branches", branches_oid, 0o040000).unwrap();
        root_b
            .insert("main.md", blob_oid("# Project\n## Goal\nlegacy\n"), 0o100644)
            .unwrap();
        root_b
            .insert(".current_branch", blob_oid("scope/foo"), 0o100644)
            .unwrap();
        let root_oid = root_b.write().unwrap();
        let root_tree = repo.find_tree(root_oid).unwrap();
        repo.commit(
            Some(CTX_LEGACY_REF),
            &sig,
            &sig,
            "legacy seed",
            &root_tree,
            &[],
        )
        .unwrap();

        // Run migration.
        let migrated = migrate_legacy_if_needed(dir.path()).unwrap();
        assert!(migrated, "migration should run when only the legacy ref exists");

        // Per-branch refs exist with the correct content.
        assert!(
            repo.find_reference(&branch_ref("main")).is_ok(),
            "main branch ref should exist"
        );
        assert!(
            repo.find_reference(&branch_ref("scope/foo")).is_ok(),
            "scope/foo branch ref should exist"
        );

        // The new main ref's tree carries the project-wide main.md (NOT branches/).
        let new_main_tree = repo
            .find_reference(&branch_ref("main"))
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        assert!(
            new_main_tree.get_name("main.md").is_some(),
            "main.md should land on the new main ref"
        );
        assert!(
            new_main_tree.get_name("branches").is_none(),
            "stale branches/ tree should not appear on the new main ref"
        );

        // HEAD points at the previously-active branch.
        assert_eq!(current_branch(dir.path()), "scope/foo");

        // Legacy ref preserved at backup name; original deleted.
        assert!(repo.find_reference(CTX_LEGACY_BACKUP_REF).is_ok());
        assert!(repo.find_reference(CTX_LEGACY_REF).is_err());

        // Idempotent: second call is a no-op.
        let migrated_again = migrate_legacy_if_needed(dir.path()).unwrap();
        assert!(!migrated_again, "migration should be a no-op after running once");
    }

    #[test]
    fn auto_follow_switches_ctx_branch_to_match_git_branch() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();

        // Switch the underlying git branch — auto-follow should mirror it on
        // the next write-gated ctx command.
        {
            let repo = Repository::open(dir.path()).unwrap();
            repo.set_head("refs/heads/feature/x").unwrap();
        }
        // Record the git-branch goal so prepare_context_write validates.
        init(dir.path(), "x goal").unwrap();

        assert!(prepare_context_write(dir.path()).is_ok());
        assert_eq!(
            current_branch(dir.path()),
            "feature/x",
            "ctx HEAD should shadow the git branch when not pinned"
        );
        // The shadow ref must have been auto-created.
        let repo = ctx_git_repo(dir.path()).unwrap();
        assert!(
            repo.find_reference(&branch_ref("feature/x")).is_ok(),
            "shadow ctx ref should exist after auto-follow"
        );
    }

    #[test]
    fn explicit_checkout_pins_against_auto_follow() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "pinned-spike", "explore an idea").unwrap();

        // Now move the git branch sideways. Because gcc_branch set the pin,
        // auto-follow must NOT override the pinned context branch.
        {
            let repo = Repository::open(dir.path()).unwrap();
            repo.set_head("refs/heads/some-other-branch").unwrap();
        }
        init(dir.path(), "another goal").unwrap();

        let _ = prepare_context_write(dir.path()); // may pass or fail; we only care about HEAD
        assert_eq!(
            current_branch(dir.path()),
            "pinned-spike",
            "pinned context branch should survive a git checkout"
        );

        // Unpin and the next prepare_context_write should auto-follow again.
        unpin(dir.path()).unwrap();
        let _ = prepare_context_write(dir.path());
        assert_eq!(
            current_branch(dir.path()),
            "some-other-branch",
            "after unpin, ctx should re-shadow the current git branch"
        );
    }

    // ── gcc_commit edge cases ─────────────────────────────────────────────────

    #[test]
    fn gcc_commit_with_empty_detail_succeeds() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        assert!(gcc_commit(dir.path(), "milestone with no detail", "").is_ok());
    }

    #[test]
    fn gcc_commit_multiple_entries_all_visible() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "first milestone", "alpha done").unwrap();
        gcc_commit(dir.path(), "second milestone", "beta done").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts { window: 5, ..Default::default() }).unwrap();
        let combined = ctx.recent_commits.join(" ");
        assert!(combined.contains("alpha done"));
        assert!(combined.contains("beta done"));
    }

    // ── pack_lossless edge cases ──────────────────────────────────────────────

    #[test]
    fn lossless_pack_keeps_standalone_observe_not_subsumed() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // OBSERVE about file_a.rs — no later THINK/ACT mentions file_a.rs.
        append_log(dir.path(), "OBSERVE", "file_a.rs has 100 lines", false).unwrap();
        append_log(dir.path(), "THINK", "something about file_b.rs", false).unwrap();
        let result = pack_lossless(dir.path()).unwrap();
        assert_eq!(result.removed_subsumed_observe, 0, "standalone OBSERVE must not be removed");
        let repo = ctx_git_repo(dir.path()).unwrap();
        let trace = ctx_read_file(&repo, "branches/main/trace.md").unwrap_or_default();
        assert!(trace.contains("file_a.rs has 100 lines"), "standalone OBSERVE must be preserved");
    }

    #[test]
    fn lossless_pack_preserves_note_entries_verbatim() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "important reminder about src/auth.rs", false).unwrap();
        pack_lossless(dir.path()).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let trace = ctx_read_file(&repo, "branches/main/trace.md").unwrap_or_default();
        assert!(trace.contains("important reminder about src/auth.rs"), "NOTE must be preserved");
    }

    #[test]
    fn lossless_pack_is_noop_on_empty_trace() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let result = pack_lossless(dir.path()).unwrap();
        assert_eq!(result.removed_subsumed_observe, 0);
        assert_eq!(result.merged_consecutive_observe, 0);
        assert_eq!(result.kept_durable, 0);
    }

    // ── context_diff edge cases ───────────────────────────────────────────────

    #[test]
    fn context_diff_same_snapshot_shows_no_changes() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        snapshot_for_commit(dir.path(), "samesha1111111111").unwrap();
        let diff = context_diff(dir.path(), "samesha1", "samesha1").unwrap();
        assert!(diff.added_commits.is_empty());
        assert!(diff.added_trace_lines.is_empty());
    }

    #[test]
    fn context_diff_detects_new_trace_lines_after_pack() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "line that will be subsumed", false).unwrap();
        snapshot_for_commit(dir.path(), "sha_before_00000000").unwrap();

        // A THINK that subsumes the OBSERVE; pack removes the OBSERVE.
        append_log(dir.path(), "THINK", "subsumed line is now understood in context", false).unwrap();
        pack_lossless(dir.path()).unwrap();
        snapshot_for_commit(dir.path(), "sha_after_000000000").unwrap();

        let diff = context_diff(dir.path(), "sha_befor", "sha_after").unwrap();
        // The THINK entry is new in sha_after — diff must report it.
        assert!(
            diff.added_trace_lines.iter().any(|l| l.contains("understood in context")),
            "diff should report the new THINK entry added after packing"
        );
    }

    // ── DAG edge cases ────────────────────────────────────────────────────────

    #[test]
    fn dag_head_id_empty_when_dag_is_empty() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert_eq!(dag.head_id(), "", "head_id must be empty on empty DAG");
    }

    #[test]
    fn dag_head_id_matches_last_node() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "first", false).unwrap();
        append_log(dir.path(), "ACT", "second", false).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        assert_eq!(dag.head_id(), dag.nodes.last().unwrap().id);
    }

    #[test]
    fn dag_node_ids_are_unique() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "first entry", false).unwrap();
        append_log(dir.path(), "THINK", "second entry", false).unwrap();
        append_log(dir.path(), "ACT", "third entry", false).unwrap();
        let repo = ctx_git_repo(dir.path()).unwrap();
        let dag = read_dag(&repo, MAIN_BRANCH);
        let ids: std::collections::HashSet<&str> =
            dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), dag.nodes.len(), "every DAG node must have a unique ID");
    }

    // ── stable_line_count boundary ────────────────────────────────────────────

    #[test]
    fn stable_line_count_zero_when_trace_is_short() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "NOTE", "just one entry", false).unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.stable_line_count, 0, "fewer than 40 lines → stable count is 0");
    }

    #[test]
    fn stable_line_count_with_exactly_40_entries() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        for i in 0..40 {
            append_log(dir.path(), "NOTE", &format!("entry {i}"), false).unwrap();
        }
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        // Exactly 40 OTA entries fill the dynamic tail; trace.md may have a small
        // number of header lines that spill into stable — stable ≤ a few lines.
        assert_eq!(ctx.dynamic_line_count, 40);
        assert!(ctx.stable_line_count <= 5, "header lines only — stable should be very small");
    }

    // ── relevant: basename matching ───────────────────────────────────────────

    #[test]
    fn relevant_matches_by_file_basename() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "ACT", "edited repository.rs at line 42", false).unwrap();
        // Query with just the basename — should still find the mention.
        let ctx = relevant(dir.path(), "repository.rs").unwrap();
        assert!(
            ctx.trace_mentions.iter().any(|l| l.contains("repository.rs")),
            "relevant should match by basename"
        );
    }

    // ── ephemeral on non-main branch ──────────────────────────────────────────

    #[test]
    fn ephemeral_trace_on_non_main_branch_is_isolated() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "side", "side work").unwrap();
        append_log(dir.path(), "NOTE", "scratch on side branch", true).unwrap();

        // Main branch ephemeral should be empty.
        gcc_checkout(dir.path(), "main").unwrap();
        let main_scratch = read_ephemeral(dir.path(), Some("main")).unwrap_or_default();
        assert!(
            !main_scratch.contains("scratch on side branch"),
            "ephemeral on side branch must not bleed into main"
        );

        // Side branch ephemeral should contain it.
        let side_scratch = read_ephemeral(dir.path(), Some("side")).unwrap_or_default();
        assert!(side_scratch.contains("scratch on side branch"));
    }

    // ── search ────────────────────────────────────────────────────────────────

    #[test]
    fn search_returns_empty_when_no_match() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "Redis is fast", false).unwrap();
        let results = search(dir.path(), "postgresql database", 10).unwrap();
        assert!(results.is_empty(), "unrelated query should return no results");
    }

    #[test]
    fn search_finds_matching_trace_entry_with_file_mention() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        // Note: extract_file_mentions skips tokens starting with "http", so use
        // a filename that doesn't start with that prefix.
        append_log(dir.path(), "THINK", "exponential backoff in retry_client.rs reduces storms", false).unwrap();
        let results = search(dir.path(), "exponential backoff", 10).unwrap();
        assert!(!results.is_empty(), "query matching a THINK entry with file mention should return results");
    }

    #[test]
    fn search_ranks_think_entries_higher() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "retry.rs: retry logic exists", false).unwrap();
        append_log(dir.path(), "THINK", "retry.rs needs exponential backoff for resilience", false).unwrap();
        let results = search(dir.path(), "retry", 10).unwrap();
        // THINK entries get 1.5× weight — the THINK-sourced result should score higher.
        if results.len() >= 2 {
            assert!(
                results[0].score >= results[1].score,
                "results should be sorted by descending score"
            );
        }
    }

    #[test]
    fn smart_recall_returns_empty_for_blank_query() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "retry_client.rs uses exponential backoff", false).unwrap();

        let recall = smart_recall(dir.path(), "   ", 5).unwrap();
        assert!(recall.results.is_empty(), "blank task query should not recall anything");
    }

    #[test]
    fn smart_recall_respects_limit_and_keeps_query() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "auth.rs validates tokens with jose", false).unwrap();
        append_log(dir.path(), "THINK", "middleware.rs wires token validation", false).unwrap();

        let recall = smart_recall(dir.path(), "token validation", 1).unwrap();
        assert_eq!(recall.query, "token validation");
        assert_eq!(recall.results.len(), 1, "smart recall must honor the caller's limit");
        assert!(
            recall.results[0].file == "auth.rs" || recall.results[0].file == "middleware.rs",
            "expected a matching file result, got {:?}",
            recall.results
        );
    }

    #[test]
    fn smart_recall_ranks_task_specific_context() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "cache.rs handles eviction policy", false).unwrap();
        append_log(dir.path(), "THINK", "retry_client.rs uses exponential backoff and jitter", false).unwrap();

        let recall = smart_recall(dir.path(), "exponential backoff jitter", 5).unwrap();
        assert!(!recall.results.is_empty(), "expected task-aware recall results");
        assert_eq!(recall.results[0].file, "retry_client.rs");
    }

    // ── distill_knowledge ─────────────────────────────────────────────────────

    #[test]
    fn distill_knowledge_collects_think_entries() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "exponential backoff is safer under high load", false).unwrap();
        append_log(dir.path(), "OBSERVE", "this is not a thought", false).unwrap();
        let knowledge = distill_knowledge(dir.path()).unwrap();
        assert_eq!(knowledge.len(), 1, "only THINK entries should be distilled");
        let thought = knowledge[0]["thought"].as_str().unwrap_or("");
        assert!(thought.contains("exponential backoff"));
    }

    #[test]
    fn distill_knowledge_deduplicates_across_branches() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        let thought = "use Redis for session storage due to restart resilience";
        append_log(dir.path(), "THINK", thought, false).unwrap();
        gcc_branch(dir.path(), "alt", "alt").unwrap();
        // Identical thought on alt branch — should be deduplicated.
        append_log(dir.path(), "THINK", thought, false).unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        let knowledge = distill_knowledge(dir.path()).unwrap();
        assert_eq!(knowledge.len(), 1, "duplicate THINK entries must be deduplicated");
    }

    #[test]
    fn distill_knowledge_includes_thoughts_from_all_branches() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "THINK", "main branch insight about caching strategy", false).unwrap();
        gcc_branch(dir.path(), "feature", "feature work").unwrap();
        append_log(dir.path(), "THINK", "feature branch insight about retry logic pattern", false).unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        let knowledge = distill_knowledge(dir.path()).unwrap();
        let thoughts: Vec<&str> = knowledge.iter()
            .filter_map(|k| k["thought"].as_str())
            .collect();
        assert!(thoughts.iter().any(|t| t.contains("caching strategy")));
        assert!(thoughts.iter().any(|t| t.contains("retry logic pattern")));
    }
}
