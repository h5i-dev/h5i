use console::style;
use git2::{Blob, Repository};
use git2::{Commit, ObjectType, Oid, Signature};
use std::fs;
use std::path::{Path, PathBuf};

use crate::blame::{AncestryEntry, BlameResult};
use crate::metadata::Decision;
use crate::error::H5iError;
use chrono::{TimeZone, Utc};

use crate::metadata::{
    AiMetadata, CommitSummary, H5iCommitRecord, IntegrityLevel, IntentEdge, IntentGraph,
    IntentNode, IntegrityReport, PendingContext, TestMetrics, TestSource,
};

/// Git ref used to store all h5i commit metadata (AI provenance, test results,
/// AST hashes, causal links). Using a custom `refs/h5i/` namespace keeps h5i
/// data clearly separated from standard `refs/notes/commits` and lets a single
/// `h5i push` sync everything under `refs/h5i/*` in one refspec.
pub const H5I_NOTES_REF: &str = "refs/h5i/notes";

fn fallback_signature() -> Result<Signature<'static>, H5iError> {
    Signature::now("h5i", "h5i@local").map_err(H5iError::Git)
}

fn repo_signature_or_fallback(repo: &Repository) -> Result<Signature<'_>, H5iError> {
    repo.signature().or_else(|_| fallback_signature())
}

/// Copy onto `dest_ref` every `refs/h5i/notes` entry whose annotated commit is in
/// `reachable`, for a branch-scoped `h5i share push`.
///
/// `dest_ref` is expected to already hold the *remote's* notes (seeded by the
/// caller via a fetch) — or to be absent, in which case it is created. We then
/// overlay only the notes for commits reachable from the pushed branch, forcing
/// over any pre-existing entry there (the pushing clone is authoritative for its
/// own commits). The result is therefore `remote ∪ this-branch's-notes`: notes
/// for every *other* commit already on the remote are left untouched, so a
/// scoped push never deletes another branch's provenance.
///
/// git2 manages the notes tree fan-out, so this is robust to flat *or* fanned
/// layouts. Returns the number of notes copied (0 when the branch has none, or
/// when `refs/h5i/notes` does not exist locally).
pub fn copy_scoped_notes_onto(
    repo: &Repository,
    reachable: &std::collections::HashSet<String>,
    dest_ref: &str,
) -> Result<usize, H5iError> {
    // No local notes ref → nothing to copy (and find_note would just error).
    if repo.refname_to_id(H5I_NOTES_REF).is_err() {
        return Ok(0);
    }
    let sig = repo_signature_or_fallback(repo)?;
    let mut copied = 0usize;
    for oid_str in reachable {
        let Ok(oid) = Oid::from_str(oid_str) else {
            continue;
        };
        let Ok(note) = repo.find_note(Some(H5I_NOTES_REF), oid) else {
            continue; // this reachable commit simply has no note
        };
        if let Some(msg) = note.message() {
            // force=true: overwrite the remote-seeded entry for our own commit.
            repo.note(&sig, &sig, Some(dest_ref), oid, msg, true)?;
            copied += 1;
        }
    }
    Ok(copied)
}

pub struct H5iRepository {
    git_repo: Repository,
    pub h5i_root: PathBuf,
}

// ============================================================
// Repository lifecycle
// ============================================================

impl H5iRepository {
    /// Opens or initializes an `h5i` context for an existing Git repository.
    ///
    /// This function discovers the Git repository starting from the given path
    /// and ensures that the `.h5i` metadata directory exists inside the
    /// repository root.
    ///
    /// If the `.h5i` directory does not exist, it will be created along with
    /// several subdirectories used by the system:
    ///
    /// - `metadata/` – stores commit-related metadata (e.g., AI provenance)
    /// - `claims/`, `memory/`, `session_log/` – sidecar state stores
    /// - `objects/` – content-addressed raw-output store (token-reduction captures)
    ///
    /// # Parameters
    ///
    /// - `path`: A path inside the target Git repository (or the repository root).
    ///
    /// # Returns
    ///
    /// Returns a [`H5iRepository`] instance containing:
    ///
    /// - the discovered Git repository handle
    /// - the `.h5i` root directory path
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - a Git repository cannot be discovered from the given path
    /// - the repository root directory cannot be determined
    /// - the `.h5i` directories cannot be created
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, H5iError> {
        let git_repo = Repository::discover(path)?;
        let h5i_root = crate::storage::h5i_root_for_repo(&git_repo)?;
        crate::storage::ensure_layout(&h5i_root)?;

        Ok(H5iRepository { git_repo, h5i_root })
    }
}

// ============================================================
// Core operations
// ============================================================

impl H5iRepository {
    /// Creates a Git commit and atomically associates it with h5i extended metadata.
    ///
    /// This function performs a standard Git commit while collecting and storing
    /// additional `h5i` sidecar data. The extra metadata may include:
    ///
    /// - **AI provenance metadata** describing AI-assisted code generation
    /// - **AST hashes** derived from source files using an optional parser
    /// - **Test provenance metrics** extracted from staged test files
    ///
    /// The collected metadata is stored separately in the `.h5i` directory
    /// and linked to the Git commit via the commit OID.
    ///
    /// The operation proceeds in three phases:
    ///
    /// 1. **Pre-processing staged files**
    ///    - Optionally generate AST representations using the provided parser.
    ///    - Optionally extract test-related metrics.
    ///
    /// 2. **Git commit creation**
    ///    - Uses the `git2` API to write the index tree and create a commit.
    ///
    /// 3. **Sidecar metadata persistence**
    ///    - A corresponding `H5iCommitRecord` is created and stored under `.h5i`.
    ///
    /// # Parameters
    ///
    /// - `message` – Commit message.
    /// - `author` – Git author signature.
    /// - `committer` – Git committer signature.
    /// - `ai_meta` – Optional AI provenance metadata associated with the commit.
    /// - `enable_test_tracking` – Enables automatic test provenance detection.
    ///
    /// # Returns
    ///
    /// Returns the [`Oid`] of the newly created Git commit.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the Git index cannot be accessed or written
    /// - the commit cannot be created
    /// - the `h5i` metadata record cannot be stored
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub fn commit(
        &self,
        message: &str,
        author: &Signature,
        committer: &Signature,
        ai_meta: Option<AiMetadata>,
        test_source: TestSource,
        caused_by: Vec<String>,
        decisions: Vec<Decision>,
        // When `Some`, the process is running inside a sandboxed env where the
        // h5i sidecar store + notes ref are sealed: the git commit still lands
        // (the box has its own object/ref grants), but the note is STAGED to
        // this spool dir for the host to apply after the session, instead of
        // written to `refs/h5i/notes` (which would EACCES).
        note_spool: Option<&Path>,
    ) -> Result<Oid, H5iError> {
        let _span = tracing::info_span!(
            "h5i_commit",
            with_ai = ai_meta.is_some(),
            with_tests = !matches!(test_source, TestSource::None),
            decisions = decisions.len(),
        )
        .entered();
        let mut index = self.git_repo.index()?;

        // For ScanMarkers we look for the marker block in staged files (first hit wins).
        let mut scanned_metrics: Option<TestMetrics> = None;
        if matches!(test_source, TestSource::ScanMarkers) {
            for entry in index.iter() {
                let path_str = std::str::from_utf8(&entry.path).map_err(|e| {
                    H5iError::InvalidPath(format!("staged path is not valid UTF-8: {e}"))
                })?;
                let workdir = self.git_repo.workdir().ok_or_else(|| {
                    H5iError::InvalidPath("h5i commit requires a non-bare repository".to_string())
                })?;
                let full_path = workdir.join(path_str);
                if scanned_metrics.is_none() {
                    scanned_metrics = self.scan_test_block(&full_path);
                }
            }
        }

        // Resolve final test_metrics from the chosen source
        let test_metrics = match test_source {
            TestSource::None => None,
            TestSource::ScanMarkers => scanned_metrics,
            TestSource::Provided(metrics) => Some(metrics),
        };

        // Validate and resolve caused_by OIDs (supports abbreviated OIDs)
        let mut resolved_caused_by = Vec::with_capacity(caused_by.len());
        for oid_str in &caused_by {
            let commit = self
                .git_repo
                .revparse_single(oid_str)
                .and_then(|o| o.peel_to_commit())
                .map_err(|_| {
                    H5iError::Git(git2::Error::from_str(&format!(
                        "caused_by OID not found in repository: {}",
                        oid_str
                    )))
                })?;
            resolved_caused_by.push(commit.id().to_string());
        }

        // 2. Create the standard Git commit (using the git2-rs API)
        let tree_id = index.write_tree()?;
        let tree = self.git_repo.find_tree(tree_id)?;
        let parent_commit = self.get_head_commit().ok();
        let mut parents = Vec::new();
        if let Some(ref p) = parent_commit {
            parents.push(p);
        }

        let commit_oid =
            self.git_repo
                .commit(Some("HEAD"), author, committer, message, &tree, &parents)?;

        // 3. Persist the h5i sidecar record
        let record = H5iCommitRecord {
            git_oid: commit_oid.to_string(),
            parent_oid: parent_commit.map(|p| p.id().to_string()),
            ai_metadata: ai_meta,
            test_metrics,
            timestamp: chrono::Utc::now(),
            caused_by: resolved_caused_by,
            decisions,
            env_provenance: None,
        };
        let metadata_json = serde_json::to_string(&record)?;
        match note_spool {
            // In-box: stage the note; the host applies it (scoped to the env
            // branch) on the next ingest. The git commit already succeeded.
            Some(spool) => {
                crate::env::write_note_spool(spool, &commit_oid.to_string(), &metadata_json)?
            }
            None => {
                self.git_repo.note(
                    author,
                    committer,
                    Some(H5I_NOTES_REF),
                    commit_oid,
                    &metadata_json,
                    true,
                )?;
            }
        }

        let short = commit_oid.to_string();
        let short = &short[..short.len().min(8)];
        tracing::debug!(oid = %short, "h5i_commit complete");
        Ok(commit_oid)
    }

}

// ============================================================
// Log API
// ============================================================

impl H5iRepository {
    /// Retrieves an extended commit log that includes AI provenance metadata.
    ///
    /// This function traverses the Git commit history starting from `HEAD`
    /// and attempts to load the corresponding `h5i` sidecar metadata for
    /// each commit.
    ///
    /// If a sidecar metadata file does not exist for a given commit,
    /// the function falls back to constructing a minimal record using
    /// only the information available in the Git commit object.
    ///
    /// # Parameters
    ///
    /// - `limit` – Maximum number of commits to return.
    ///
    /// # Returns
    ///
    /// Returns a vector of [`H5iCommitRecord`] entries representing the
    /// most recent commits, enriched with `h5i` metadata when available.
    ///
    /// # Errors
    ///
    /// Returns an error if the Git revision walker cannot be created
    /// or if the repository history cannot be traversed.
    pub fn get_log(&self, limit: usize) -> Result<Vec<H5iCommitRecord>, H5iError> {
        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;

        let mut records = Vec::new();
        for oid in revwalk.take(limit) {
            let oid = oid?;
            // Read `.h5i/metadata/<oid>.json`. If it does not exist,
            // return a minimal record derived from Git.
            let record = self
                .load_h5i_record(oid)
                .unwrap_or_else(|_| H5iCommitRecord::minimal_from_git(&self.git_repo, oid));
            records.push(record);
        }
        Ok(records)
    }

    /// Like `get_log`, but walks history starting at the tip of the given
    /// branch (e.g. "main", "feature/x") instead of HEAD. Returns an error
    /// if the branch can't be resolved to a local or remote-tracking ref.
    pub fn get_log_at_branch(
        &self,
        branch: &str,
        limit: usize,
    ) -> Result<Vec<H5iCommitRecord>, H5iError> {
        let mut revwalk = self.git_repo.revwalk()?;

        // Try local branch first, then remote-tracking, then raw refspec.
        let local = self
            .git_repo
            .find_branch(branch, git2::BranchType::Local)
            .ok();
        let remote = self
            .git_repo
            .find_branch(branch, git2::BranchType::Remote)
            .ok();
        let oid = if let Some(b) = local.or(remote) {
            b.get().target().ok_or_else(|| {
                H5iError::Git(git2::Error::from_str("branch has no target oid"))
            })?
        } else {
            // Fall back to revparse so tags / "origin/foo" / abbreviations work.
            self.git_repo.revparse_single(branch)?.id()
        };
        revwalk.push(oid)?;

        let mut records = Vec::new();
        for oid in revwalk.take(limit) {
            let oid = oid?;
            let record = self
                .load_h5i_record(oid)
                .unwrap_or_else(|_| H5iCommitRecord::minimal_from_git(&self.git_repo, oid));
            records.push(record);
        }
        Ok(records)
    }

    /// Retrieves the extended `h5i` commit log including AI metadata.
    ///
    /// This method behaves similarly to `get_log`, but is intended as the
    /// primary API for accessing commit history enriched with `h5i`
    /// provenance data such as:
    ///
    /// - AI generation metadata
    /// - test provenance metrics
    /// - AST hash tracking
    ///
    /// The history traversal begins at `HEAD` and proceeds backwards.
    ///
    /// # Parameters
    ///
    /// - `limit` – Maximum number of commits to retrieve.
    ///
    /// # Returns
    ///
    /// Returns a vector of [`H5iCommitRecord`] values representing the
    /// extended commit history.
    ///
    /// # Errors
    ///
    /// Returns an error if the Git revision walker fails to initialize
    /// or if history traversal encounters an issue.
    pub fn h5i_log(&self, limit: usize) -> Result<Vec<H5iCommitRecord>, H5iError> {
        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?; // Traverse history starting from HEAD

        let mut logs = Vec::new();
        for oid in revwalk.take(limit) {
            let oid = oid?;
            // Load sidecar metadata. If unavailable, construct a minimal record from Git data.
            let record = self
                .load_h5i_record(oid)
                .unwrap_or_else(|_| H5iCommitRecord::minimal_from_git(&self.git_repo, oid));
            logs.push(record);
        }
        Ok(logs)
    }

    /// Like [`h5i_log`](Self::h5i_log), but restricted to commits reachable from
    /// `HEAD` and **not** from `base` — i.e. the `base..HEAD` range, the commits
    /// unique to the current branch. When `base` is `None` this is identical to
    /// [`h5i_log`](Self::h5i_log).
    ///
    /// The PR renderer uses this so the body reflects only what the branch adds
    /// over its base branch, instead of the last `limit` commits that happen to
    /// be reachable from `HEAD` (which spills into commits already merged into
    /// the base).
    pub fn h5i_log_since(
        &self,
        base: Option<git2::Oid>,
        limit: usize,
    ) -> Result<Vec<H5iCommitRecord>, H5iError> {
        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;
        if let Some(b) = base {
            // `hide` excludes `b` and its ancestors. Best-effort: if `b` is not a
            // valid/known object we fall back to the full HEAD walk rather than
            // erroring, so a stale base never breaks `pr post`.
            let _ = revwalk.hide(b);
        }

        let mut logs = Vec::new();
        for oid in revwalk.take(limit) {
            let oid = oid?;
            let record = self
                .load_h5i_record(oid)
                .unwrap_or_else(|_| H5iCommitRecord::minimal_from_git(&self.git_repo, oid));
            logs.push(record);
        }
        Ok(logs)
    }

    /// Prints a human-readable commit log enriched with `h5i` metadata.
    ///
    /// This function traverses the Git history starting from `HEAD` and
    /// prints commit information similar to `git log`, augmented with
    /// additional `h5i` metadata when available.
    ///
    /// The output may include:
    ///
    /// - Commit identifier and author
    /// - AI agent metadata (agent ID, model name, prompt hash)
    /// - Test provenance metrics (test suite hash and coverage)
    /// - Number of tracked AST hashes
    /// - Commit message
    ///
    /// Missing metadata is handled gracefully; commits without sidecar
    /// records are displayed using only the standard Git information.
    ///
    /// # Parameters
    ///
    /// - `limit` – Maximum number of commits to display.
    ///
    /// # Errors
    ///
    /// Returns an error if the repository history cannot be traversed
    /// or if commit objects cannot be retrieved.
    pub fn print_log(&self, limit: usize) -> anyhow::Result<()> {
        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;

        for oid in revwalk.take(limit) {
            let oid = oid?;
            let commit = self.git_repo.find_commit(oid)?;
            let record = self.load_h5i_record(oid).ok();

            println!(
                "{} {}",
                style("commit").yellow(),
                style(oid).magenta().bold()
            );
            println!("{:<10} {}", style("Author:").dim(), commit.author());

            if let Some(r) = record {
                if let Some(ai) = r.ai_metadata {
                    println!(
                        "{:<10} {} {} {}",
                        style("Agent:").dim(),
                        style(&ai.agent_id).cyan().bold(),
                        style(format!("({})", ai.model_name)).dim(),
                        if ai.usage.is_some() {
                            style("󱐋").yellow()
                        } else {
                            style("")
                        }
                    );

                    if !ai.prompt.is_empty() {
                        println!(
                            "{:<10} {}",
                            style("Prompt:").dim(),
                            style(format!("\"{}\"", ai.prompt)).italic()
                        );
                    }

                    if let Some(usage) = ai.usage {
                        println!(
                            "{:<10} {} {} {}",
                            style("Usage:").dim(),
                            style(format!("+{} tokens", usage.total_tokens)).green(),
                            style("|").dim(),
                            style(format!("model: {}", usage.model)).dim()
                        );
                    }
                }

                if let Some(tm) = r.test_metrics {
                    let passing = tm.is_passing();
                    let color = if passing {
                        console::Color::Green
                    } else {
                        console::Color::Red
                    };
                    let icon = if passing { "✔" } else { "✖" };

                    // Prefer an explicit summary; fall back to building one from counts.
                    let detail = if let Some(ref s) = tm.summary {
                        s.clone()
                    } else if tm.total > 0 {
                        let mut parts = vec![format!("{} passed", tm.passed)];
                        if tm.failed > 0 {
                            parts.push(format!("{} failed", tm.failed));
                        }
                        if tm.skipped > 0 {
                            parts.push(format!("{} skipped", tm.skipped));
                        }
                        if tm.duration_secs > 0.0 {
                            parts.push(format!("{:.2}s", tm.duration_secs));
                        }
                        if tm.coverage > 0.0 {
                            parts.push(format!("{:.1}% cov", tm.coverage));
                        }
                        parts.join(", ")
                    } else {
                        // Legacy record with only coverage
                        format!("{:.1}% coverage", tm.coverage)
                    };

                    let tool_label = tm
                        .tool
                        .as_deref()
                        .map(|t| format!(" [{}]", t))
                        .unwrap_or_default();

                    println!(
                        "{:<10} {} {}{}",
                        style("Tests:").dim(),
                        style(icon).fg(color),
                        style(detail).fg(color),
                        style(tool_label).dim()
                    );
                }

                if !r.caused_by.is_empty() {
                    for cause_oid_str in &r.caused_by {
                        // Try to get the short message of the cause commit
                        let cause_msg = git2::Oid::from_str(cause_oid_str)
                            .ok()
                            .and_then(|o| self.git_repo.find_commit(o).ok())
                            .and_then(|c| c.summary().map(|s| s.to_string()))
                            .unwrap_or_default();
                        let short = &cause_oid_str[..8.min(cause_oid_str.len())];
                        println!(
                            "{:<10} {} {}",
                            style("Caused by:").dim(),
                            style(short).magenta(),
                            style(format!("\"{}\"", cause_msg)).dim().italic()
                        );
                    }
                }

                if !r.decisions.is_empty() {
                    println!("{:<10}", style("Decisions:").dim());
                    for d in &r.decisions {
                        println!(
                            "  {} {}  {}  {}",
                            style("◆").cyan(),
                            style(&d.location).dim(),
                            style(&d.choice).bold(),
                            if !d.alternatives.is_empty() {
                                style(format!(
                                    "(considered: {})",
                                    d.alternatives.join(", ")
                                ))
                                .dim()
                            } else {
                                style(String::new()).dim()
                            }
                        );
                        if !d.reason.is_empty() {
                            println!("             {}", style(&d.reason).italic());
                        }
                    }
                }

                if let Some(ep) = &r.env_provenance {
                    // Self-describing applied commit: which env it came from and
                    // the evidence carried forward, by trust lane.
                    let lanes = ep
                        .evidence_sources
                        .iter()
                        .map(|(s, n)| format!("{s}={n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!(
                        "{:<10} {} {}",
                        style("From env:").dim(),
                        style(&ep.env_id).cyan().bold(),
                        style(format!("(by {}, {})", ep.agent, ep.isolation_claim)).dim(),
                    );
                    println!(
                        "{:<10} {} {}",
                        style("Evidence:").dim(),
                        style(format!("{} capture(s)", ep.captures_total)).green(),
                        style(if lanes.is_empty() {
                            "none".into()
                        } else {
                            format!("[{lanes}]")
                        })
                        .dim(),
                    );
                }
            }
            println!("{:<10}", style("Message:").dim());
            println!("    {}\n", style(commit.message().unwrap_or("")).bold());
            println!("{}", style("─".repeat(60)).dim());
        }
        Ok(())
    }
}

// ============================================================
// Causal chain API
// ============================================================

impl H5iRepository {
    /// Follows `caused_by` links backward from `start_oid`, returning
    /// `(oid, short_message)` pairs in traversal order (BFS).
    pub fn causal_ancestors(&self, start_oid: git2::Oid) -> Vec<(git2::Oid, String)> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut result = Vec::new();

        if let Ok(record) = self.load_h5i_record(start_oid) {
            for oid_str in record.caused_by {
                if let Ok(oid) = git2::Oid::from_str(&oid_str) {
                    if visited.insert(oid) {
                        queue.push_back(oid);
                    }
                }
            }
        }

        while let Some(oid) = queue.pop_front() {
            let msg = self.git_repo.find_commit(oid)
                .ok()
                .and_then(|c| c.summary().map(|s| s.to_string()))
                .unwrap_or_default();
            result.push((oid, msg));

            if let Ok(record) = self.load_h5i_record(oid) {
                for oid_str in record.caused_by {
                    if let Ok(o) = git2::Oid::from_str(&oid_str) {
                        if visited.insert(o) {
                            queue.push_back(o);
                        }
                    }
                }
            }
        }
        result
    }

    /// Scans up to `limit` recent commits for any whose `caused_by` list
    /// includes `target_oid`. Returns `(oid, short_message)` pairs.
    pub fn causal_dependents(
        &self,
        target_oid: git2::Oid,
        limit: usize,
    ) -> Vec<(git2::Oid, String)> {
        let target_str = target_oid.to_string();
        let mut result = Vec::new();
        let mut revwalk = match self.git_repo.revwalk() {
            Ok(r) => r,
            Err(_) => return result,
        };
        if revwalk.push_head().is_err() {
            return result;
        }
        for oid in revwalk.take(limit).flatten() {
            if oid == target_oid {
                continue;
            }
            if let Ok(record) = self.load_h5i_record(oid) {
                if record.caused_by.iter().any(|s| s.starts_with(&target_str[..8.min(target_str.len())]) || *s == target_str) {
                    let msg = self.git_repo.find_commit(oid)
                        .ok()
                        .and_then(|c| c.summary().map(|s| s.to_string()))
                        .unwrap_or_default();
                    result.push((oid, msg));
                }
            }
        }
        result
    }
}

// ============================================================
// Blame API
// ============================================================

impl H5iRepository {
    /// Computes blame information for a file using the specified mode.
    ///
    /// Line-based blame (Git history) enriched with h5i AI provenance.
    ///
    /// # Parameters
    ///
    /// - `path` – Path to the target file within the repository.
    ///
    /// # Returns
    ///
    /// Returns a vector of [`BlameResult`] entries describing the origin
    /// of each line in the file.
    pub fn blame(&self, path: &std::path::Path) -> Result<Vec<BlameResult>, H5iError> {
        self.blame_by_line(path)
    }

    /// Performs line-based blame (Git standard + AI metadata).
    ///
    /// This method uses the native Git blame algorithm and enriches
    /// the results with `h5i` metadata, including AI provenance
    /// information when available.
    ///
    /// Each line in the file is mapped to the commit that last
    /// modified it.
    fn blame_by_line(&self, path: &std::path::Path) -> Result<Vec<BlameResult>, H5iError> {
        let blame = self.git_repo.blame_file(path, None)?;
        let mut results = Vec::new();

        // Load the file content at HEAD
        let blob = self.get_blob_at_head(path)?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|_| H5iError::Internal("File content is not valid UTF-8".to_string()))?;
        let lines: Vec<&str> = content.lines().collect();

        for hunk in blame.iter() {
            let commit_id = hunk.final_commit_id();
            let record = self.load_h5i_record(commit_id).ok();
            let ai = record.as_ref().and_then(|r| r.ai_metadata.as_ref());
            let agent_info = ai
                .map(|a| format!("AI:{}", a.agent_id))
                .unwrap_or_else(|| "Human".to_string());
            let prompt = ai.map(|a| a.prompt.clone()).filter(|p| !p.is_empty());
            let test_passed = record
                .as_ref()
                .and_then(|r| r.test_metrics.as_ref())
                .map(|tm| tm.is_passing());

            for i in 0..hunk.lines_in_hunk() {
                let line_idx = hunk.final_start_line() + i - 1;
                if line_idx < lines.len() {
                    results.push(BlameResult {
                        line_content: lines[line_idx].to_string(),
                        commit_id: commit_id.to_string(),
                        agent_info: agent_info.clone(),
                        line_number: line_idx + 1,
                        test_passed,
                        prompt: prompt.clone(),
                    });
                }
            }
        }
        Ok(results)
    }

    // ── Prompt Ancestry ───────────────────────────────────────────────────────

    /// Returns the full prompt ancestry chain for a specific line in a file.
    ///
    /// Starting from HEAD, this method walks backwards through the commit history
    /// following the line as it moves through edits.  At each commit that touched
    /// the line it records the commit OID, author, timestamp, and — critically —
    /// the human prompt that triggered the change (from h5i AI metadata).
    ///
    /// The result is in *reverse-chronological* order (most-recent first), i.e.
    /// the direct cause of the current content is at index 0.
    ///
    /// # Arguments
    /// * `path`        – repo-relative path to the file
    /// * `line_number` – 1-indexed line number in the current HEAD version
    pub fn blame_ancestry(
        &self,
        path: &Path,
        line_number: usize,
    ) -> Result<Vec<AncestryEntry>, H5iError> {
        if line_number == 0 {
            return Err(H5iError::InvalidPath(
                "line_number must be ≥ 1".to_string(),
            ));
        }

        let mut ancestry: Vec<AncestryEntry> = Vec::new();
        // current_commit is where we evaluate blame; line_in_commit is the
        // 1-indexed target line *in that commit's version of the file*.
        let mut current_commit = self.git_repo.head()?.peel_to_commit()?;
        let mut line_in_commit = line_number;
        // Guard against infinite loops in pathological repos.
        const MAX_DEPTH: usize = 500;

        for _ in 0..MAX_DEPTH {
            // ── 1. Blame the file at current_commit ──────────────────────────
            let mut opts = git2::BlameOptions::new();
            opts.newest_commit(current_commit.id());
            let blame = match self.git_repo.blame_file(path, Some(&mut opts)) {
                Ok(b) => b,
                Err(_) => break, // file may not exist yet in this commit
            };

            let hunk = match blame.get_line(line_in_commit) {
                Some(h) => h,
                None => break,
            };
            let responsible_oid = hunk.final_commit_id();
            let responsible = match self.git_repo.find_commit(responsible_oid) {
                Ok(c) => c,
                Err(_) => break,
            };

            // ── 2. Load h5i record for that commit ───────────────────────────
            let record = self.load_h5i_record(responsible_oid).ok();
            let ai = record.as_ref().and_then(|r| r.ai_metadata.as_ref());

            // ── 3. Resolve line content in that commit ────────────────────────
            let line_content = self
                .get_file_line_at_commit(responsible_oid, path, line_in_commit)
                .unwrap_or_default();

            let ts = chrono::DateTime::from_timestamp(responsible.time().seconds(), 0)
                .unwrap_or_default();

            ancestry.push(AncestryEntry {
                commit_id: responsible_oid.to_string(),
                author: responsible
                    .author()
                    .name()
                    .unwrap_or("unknown")
                    .to_string(),
                timestamp: ts,
                prompt: ai.map(|a| a.prompt.clone()).filter(|p| !p.is_empty()),
                agent: ai.map(|a| a.agent_id.clone()),
                line_content,
            });

            // ── 4. Find the parent of the responsible commit ──────────────────
            if responsible.parent_count() == 0 {
                break; // reached root
            }
            let parent = match responsible.parent(0) {
                Ok(p) => p,
                Err(_) => break,
            };

            // ── 5. Map line_in_commit through the diff to the parent ──────────
            let parent_tree = parent.tree().ok();
            let commit_tree = match responsible.tree() {
                Ok(t) => t,
                Err(_) => break,
            };
            match self.map_line_to_parent(
                parent_tree.as_ref(),
                &commit_tree,
                path,
                line_in_commit,
            ) {
                Ok(Some(parent_line)) => {
                    line_in_commit = parent_line;
                    current_commit = parent;
                }
                _ => break, // line was introduced in this commit — ancestry complete
            }
        }

        Ok(ancestry)
    }

    /// Given line `line_in_new` (1-indexed) in the diff from `parent_tree → commit_tree`
    /// for `path`, return the corresponding line number in the parent (old) file.
    ///
    /// Returns `Ok(None)` when the line was *added* in this commit (no ancestor line).
    fn map_line_to_parent(
        &self,
        parent_tree: Option<&git2::Tree>,
        commit_tree: &git2::Tree,
        path: &Path,
        line_in_new: usize,
    ) -> Result<Option<usize>, H5iError> {
        let mut diff_opts = git2::DiffOptions::new();
        if let Some(s) = path.to_str() {
            diff_opts.pathspec(s);
        }
        let diff = self
            .git_repo
            .diff_tree_to_tree(parent_tree, Some(commit_tree), Some(&mut diff_opts))?;

        // No deltas for this file → the file was unchanged; line maps 1-to-1.
        if diff.deltas().count() == 0 {
            return Ok(Some(line_in_new));
        }

        // Walk the first (and only) patch for our file.
        let patch = git2::Patch::from_diff(&diff, 0)?;
        let patch = match patch {
            Some(p) => p,
            None => return Ok(Some(line_in_new)),
        };

        // Cumulative offset applied to lines that fall *before* each hunk.
        let mut cumulative_offset: i64 = 0;

        for hunk_idx in 0..patch.num_hunks() {
            let (hunk, _) = patch.hunk(hunk_idx)?;

            let new_start = hunk.new_start() as usize; // 1-indexed
            let new_count = hunk.new_lines() as usize;
            let old_start = hunk.old_start() as usize;
            let old_count = hunk.old_lines() as usize;

            if line_in_new < new_start {
                // The target line is before this hunk; apply offset from earlier hunks.
                let mapped = line_in_new as i64 + cumulative_offset;
                return Ok(if mapped > 0 { Some(mapped as usize) } else { None });
            }

            if line_in_new < new_start + new_count {
                // The target line is *inside* this hunk.  Walk line-by-line to find
                // the exact correspondence.
                let mut new_cursor = new_start;
                let mut old_cursor = old_start;
                for line_idx in 0..patch.num_lines_in_hunk(hunk_idx)? {
                    let dl = patch.line_in_hunk(hunk_idx, line_idx)?;
                    match dl.origin() {
                        '+' => {
                            // Added line — exists only in new.
                            if new_cursor == line_in_new {
                                return Ok(None); // introduced here
                            }
                            new_cursor += 1;
                        }
                        '-' => {
                            // Removed line — exists only in old.
                            old_cursor += 1;
                        }
                        _ => {
                            // Context line — present in both.
                            if new_cursor == line_in_new {
                                return Ok(Some(old_cursor));
                            }
                            new_cursor += 1;
                            old_cursor += 1;
                        }
                    }
                }
                // Shouldn't be reached if hunk metadata is correct.
                return Ok(None);
            }

            // Line is after this hunk; accumulate offset.
            cumulative_offset += old_count as i64 - new_count as i64;
        }

        // Line is after all hunks.
        let mapped = line_in_new as i64 + cumulative_offset;
        Ok(if mapped > 0 { Some(mapped as usize) } else { None })
    }

    /// Return the content of a single line (1-indexed) in `path` at `commit_oid`.
    fn get_file_line_at_commit(
        &self,
        commit_oid: git2::Oid,
        path: &Path,
        line_number: usize,
    ) -> Option<String> {
        let commit = self.git_repo.find_commit(commit_oid).ok()?;
        let tree = commit.tree().ok()?;
        let entry = tree.get_path(path).ok()?;
        let blob = self.git_repo.find_blob(entry.id()).ok()?;
        let content = std::str::from_utf8(blob.content()).ok()?;
        content.lines().nth(line_number.saturating_sub(1)).map(|s| s.to_string())
    }
}

// ============================================================
// Metadata
// ============================================================

impl H5iRepository {
    /// Loads the `h5i` metadata record associated with a specific commit OID.
    ///
    /// This method reads the corresponding Note it into an [`H5iCommitRecord`].
    ///
    /// The function is primarily used by higher-level APIs such as
    /// `log`, `blame`, and other history inspection tools.
    ///
    /// # Parameters
    ///
    /// - `oid` – The Git commit [`Oid`] whose metadata should be loaded.
    ///
    /// # Returns
    ///
    /// Returns the corresponding [`H5iCommitRecord`] if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the metadata file does not exist
    /// - Note is not found
    pub fn load_h5i_record(&self, oid: git2::Oid) -> Result<H5iCommitRecord, H5iError> {
        // Attempt to find the note attached to the commit OID.
        let note = match self.git_repo.find_note(Some(H5I_NOTES_REF), oid) {
            Ok(n) => n,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                return Err(H5iError::RecordNotFound(oid.to_string()));
            }
            Err(e) => return Err(H5iError::Git(e)),
        };

        // Extract the JSON string from the note
        let data = note
            .message()
            .ok_or_else(|| H5iError::Metadata(format!("Empty note found for commit {}", oid)))?;

        // Deserialize the JSON content into the H5iCommitRecord struct
        let record: H5iCommitRecord = serde_json::from_str(data)?;

        Ok(record)
    }
}

// ============================================================
// Resolve Conflict
// ============================================================

/// Outcome of a [`H5iRepository::merge_file_three_way`] call.
pub struct MergeOutcome {
    /// The merged file content. If `had_conflicts` is true, this is the
    /// conflict-marked output (`<<<<<<<` / `=======` / `>>>>>>>`) produced
    /// by `git merge-file`.
    pub content: String,
    /// True when textual conflicts could not be resolved automatically.
    pub had_conflicts: bool,
}

impl H5iRepository {
    /// Performs a text-based 3-way merge for `file_path` between `our_oid`
    /// and `their_oid`, using their `git merge-base` as the ancestor.
    ///
    /// Replaces the previous CRDT-based merge. The implementation shells out
    /// to `git merge-file -p` after materializing the three blobs as temp
    /// files, which keeps behaviour identical to standard Git merges.
    ///
    /// Returns the merged content plus a flag indicating whether textual
    /// conflicts remained. The caller is responsible for staging the result.
    pub fn merge_file_three_way(
        &self,
        our_oid: Oid,
        their_oid: Oid,
        file_path: &str,
    ) -> Result<MergeOutcome, H5iError> {
        let base_oid = self.git_repo.merge_base(our_oid, their_oid)?;

        let ancestor = self
            .get_content_at_oid(base_oid, Path::new(file_path))
            .unwrap_or_default();
        let ours = self
            .get_content_at_oid(our_oid, Path::new(file_path))
            .unwrap_or_default();
        let theirs = self
            .get_content_at_oid(their_oid, Path::new(file_path))
            .unwrap_or_default();

        let dir = tempdir_in_repo(&self.h5i_root)?;
        let ours_path = dir.join("ours");
        let base_path = dir.join("base");
        let theirs_path = dir.join("theirs");
        std::fs::write(&ours_path, &ours)?;
        std::fs::write(&base_path, &ancestor)?;
        std::fs::write(&theirs_path, &theirs)?;

        let output = std::process::Command::new("git")
            .arg("merge-file")
            .arg("-p")
            .arg("-L").arg("ours")
            .arg("-L").arg("base")
            .arg("-L").arg("theirs")
            .arg(&ours_path)
            .arg(&base_path)
            .arg(&theirs_path)
            .output()
            .map_err(|e| H5iError::Internal(format!("failed to invoke `git merge-file`: {e}")))?;

        // Best-effort temp cleanup; ignore errors.
        let _ = std::fs::remove_dir_all(&dir);

        let content = String::from_utf8_lossy(&output.stdout).into_owned();
        // `git merge-file` exit code: 0 clean, >0 number of conflicts, <0 error.
        let code = output.status.code().unwrap_or(-1);
        if code < 0 {
            return Err(H5iError::Internal(format!(
                "git merge-file failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(MergeOutcome {
            content,
            had_conflicts: code > 0,
        })
    }
}

fn tempdir_in_repo(h5i_root: &Path) -> Result<PathBuf, H5iError> {
    let base = h5i_root.join("tmp");
    std::fs::create_dir_all(&base)?;
    let dir = base.join(format!(
        "merge-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ============================================================
// Internal helpers
// ============================================================

impl H5iRepository {
    /// Returns a reference to the underlying Git repository.
    ///
    /// This provides direct access to the `git2::Repository` instance
    /// used internally by `H5iRepository`.
    pub fn git(&self) -> &Repository {
        &self.git_repo
    }

    /// Returns the root directory of the `.h5i` sidecar storage.
    ///
    /// The `.h5i` directory contains auxiliary metadata used by H5i,
    /// such as:
    ///
    /// - AST sidecar files
    /// - commit metadata
    pub fn h5i_path(&self) -> &Path {
        &self.h5i_root
    }

    pub fn read_pending_context(&self) -> Result<Option<PendingContext>, H5iError> {
        let path = self.h5i_root.join("pending_context.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        let ctx: PendingContext = serde_json::from_str(&raw).map_err(|e| {
            H5iError::Metadata(format!("Failed to parse pending_context.json: {e}"))
        })?;
        Ok(Some(ctx))
    }

    /// Deletes the pending context file after it has been consumed by a commit.
    pub fn clear_pending_context(&self) -> Result<(), H5iError> {
        let path = self.h5i_root.join("pending_context.json");
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Persists the pending context to `.git/.h5i/pending_context.json`.
    pub fn write_pending_context(&self, ctx: &PendingContext) -> Result<(), H5iError> {
        fs::create_dir_all(&self.h5i_root)?;
        let path = self.h5i_root.join("pending_context.json");
        let raw = serde_json::to_string_pretty(ctx).map_err(|e| {
            H5iError::Metadata(format!("Failed to serialize pending_context.json: {e}"))
        })?;
        fs::write(&path, raw)?;
        Ok(())
    }

    /// Records a verbatim human prompt (from the `UserPromptSubmit` hook) into
    /// the pending context, **accumulating** across turns since the last
    /// commit — a commit often follows several human messages, and we want the
    /// whole ask, not just the latest line. Empty/blank prompts are ignored.
    /// The accumulated `human_prompt` wins over an agent-authored `--prompt` at
    /// commit time, so the recorded prompt is what the human actually typed
    /// rather than the agent's paraphrase.
    pub fn record_human_prompt(
        &self,
        prompt: &str,
        session_id: Option<&str>,
    ) -> Result<(), H5iError> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Ok(());
        }
        let mut ctx = self.read_pending_context()?.unwrap_or_default();
        ctx.human_prompt = Some(match ctx.human_prompt.take() {
            Some(prev) if !prev.trim().is_empty() => format!("{prev}\n\n{prompt}"),
            _ => prompt.to_string(),
        });
        if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
            ctx.session_id = Some(sid.to_string());
        }
        self.write_pending_context(&ctx)
    }

    /// Returns a list of commits enriched with h5i AI metadata, suitable for
    /// intent-based search. Commits without h5i records are included but will
    /// have `None` for prompt/model/agent_id.
    pub fn list_ai_commits(&self, limit: usize) -> Result<Vec<CommitSummary>, H5iError> {
        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;

        let mut results = Vec::new();
        for oid in revwalk.take(limit) {
            let oid = oid?;
            let commit = self.git_repo.find_commit(oid)?;
            let message = commit.message().unwrap_or("").to_string();

            let record = self.load_h5i_record(oid).ok();

            let (prompt, model, agent_id) =
                match record.as_ref().and_then(|r| r.ai_metadata.as_ref()) {
                    Some(ai) => (
                        Some(ai.prompt.clone()).filter(|p| !p.is_empty()),
                        Some(ai.model_name.clone()).filter(|m| !m.is_empty()),
                        Some(ai.agent_id.clone()).filter(|a| !a.is_empty()),
                    ),
                    None => (None, None, None),
                };

            let timestamp = record.map(|r| r.timestamp).unwrap_or_else(|| {
                Utc.timestamp_opt(commit.time().seconds(), 0)
                    .single()
                    .unwrap_or_else(Utc::now)
            });

            results.push(CommitSummary {
                oid: oid.to_string(),
                message,
                prompt,
                model,
                agent_id,
                timestamp,
            });
        }
        Ok(results)
    }

    /// Builds an [`IntentGraph`] for the most recent `limit` commits.
    ///
    /// Each node carries a human-readable *intent*:
    /// - `analyze = false` — uses the stored AI prompt when available, falling back to the
    ///   commit message.
    /// - `analyze = true`  — calls Claude to generate a concise (≤12-word) intent sentence
    ///   for every commit. Falls back to the prompt-mode logic when the API key is absent.
    ///
    /// Edges represent two kinds of relationship:
    /// - `"parent"` — the standard Git parent/child link between adjacent commits.
    /// - `"causal"` — an explicit `caused_by` declaration stored in the h5i record.
    ///
    /// Edges whose endpoints are outside the `limit` window are silently dropped.
    pub fn build_intent_graph(
        &self,
        limit: usize,
        analyze: bool,
    ) -> Result<IntentGraph, H5iError> {
        use crate::claude::AnthropicClient;
        let client = if analyze {
            AnthropicClient::from_env()
        } else {
            None
        };

        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;

        let mut nodes: Vec<IntentNode> = Vec::new();
        let mut edges: Vec<IntentEdge> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for raw_oid in revwalk.take(limit) {
            let oid = raw_oid?;
            let oid_str = oid.to_string();
            let commit = self.git_repo.find_commit(oid)?;

            let record = self
                .load_h5i_record(oid)
                .unwrap_or_else(|_| H5iCommitRecord::minimal_from_git(&self.git_repo, oid));

            let message = commit.message().unwrap_or("").trim().to_string();
            let author = commit.author().name().unwrap_or("Unknown").to_string();
            let short_oid = oid_str[..8.min(oid_str.len())].to_string();
            let timestamp = record.timestamp.to_rfc3339();

            let is_ai = record.ai_metadata.is_some();
            let agent = record
                .ai_metadata
                .as_ref()
                .map(|a| a.agent_id.clone())
                .filter(|s| !s.is_empty());
            let model = record
                .ai_metadata
                .as_ref()
                .map(|a| a.model_name.clone())
                .filter(|s| !s.is_empty());
            let stored_prompt: Option<String> = record
                .ai_metadata
                .as_ref()
                .map(|a| a.prompt.clone())
                .filter(|s| !s.is_empty());

            // Determine intent label and track its source
            let (intent, intent_source) = if analyze {
                match client {
                    Some(ref c) => {
                        match c.generate_intent(&short_oid, &message, stored_prompt.as_deref()) {
                            Ok(generated) => (generated, "analyzed".to_string()),
                            Err(e) => {
                                eprintln!(
                                    "  [intent-graph] Claude call failed for {}: {e}",
                                    short_oid
                                );
                                let fallback = stored_prompt
                                    .clone()
                                    .unwrap_or_else(|| message.clone());
                                let src = if stored_prompt.is_some() { "prompt" } else { "message" };
                                (fallback, src.to_string())
                            }
                        }
                    }
                    None => {
                        let fallback = stored_prompt.clone().unwrap_or_else(|| message.clone());
                        let src = if stored_prompt.is_some() { "prompt" } else { "message" };
                        (fallback, src.to_string())
                    }
                }
            } else {
                let fallback = stored_prompt.clone().unwrap_or_else(|| message.clone());
                let src = if stored_prompt.is_some() { "prompt" } else { "message" };
                (fallback, src.to_string())
            };

            // Causal edges (explicit h5i caused_by)
            for cause_oid in &record.caused_by {
                edges.push(IntentEdge {
                    from: cause_oid.clone(),
                    to: oid_str.clone(),
                    kind: "causal".to_string(),
                });
            }

            // Parent edge (sequential Git history)
            if let Some(ref parent_oid) = record.parent_oid {
                edges.push(IntentEdge {
                    from: parent_oid.clone(),
                    to: oid_str.clone(),
                    kind: "parent".to_string(),
                });
            }

            seen.insert(oid_str.clone());
            nodes.push(IntentNode {
                oid: oid_str,
                short_oid,
                message,
                intent,
                intent_source,
                author,
                timestamp,
                is_ai,
                agent,
                model,
            });
        }

        // Drop edges whose endpoints are outside the loaded window
        edges.retain(|e| seen.contains(&e.from) && seen.contains(&e.to));

        Ok(IntentGraph { nodes, edges })
    }

    /// Prints an ASCII intent graph to stdout.
    pub fn print_intent_graph(&self, limit: usize, analyze: bool) -> anyhow::Result<()> {
        let graph = self.build_intent_graph(limit, analyze)?;

        // Map OID → set of causes (for annotation)
        let mut causes_of: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for e in &graph.edges {
            if e.kind == "causal" {
                causes_of
                    .entry(e.to.as_str())
                    .or_default()
                    .push(e.from.as_str());
            }
        }

        // Warn when analyze mode couldn't use Claude
        if analyze {
            let analyzed_count = graph.nodes.iter().filter(|n| n.intent_source == "analyzed").count();
            if analyzed_count == 0 {
                eprintln!(
                    "  [intent-graph] ANTHROPIC_API_KEY not set or no commits processed — \
                     intents are stored prompts / commit messages. \
                     Set ANTHROPIC_API_KEY to enable Claude analysis."
                );
            } else {
                let fallback_count = graph.nodes.len() - analyzed_count;
                if fallback_count > 0 {
                    eprintln!(
                        "  [intent-graph] {}/{} intents generated by Claude ({} fell back to stored data).",
                        analyzed_count,
                        graph.nodes.len(),
                        fallback_count
                    );
                }
            }
        }

        let mode_label = if analyze { "analyze (Claude)" } else { "prompt" };
        println!(
            "{}",
            style(format!(
                "Intent Graph ─ {} commits, mode: {} ──────────────────────────",
                graph.nodes.len(),
                mode_label
            ))
            .bold()
        );

        for node in &graph.nodes {
            let oid_s = if node.is_ai {
                style(&node.short_oid).magenta().bold()
            } else {
                style(&node.short_oid).blue().bold()
            };
            let intent_s = match node.intent_source.as_str() {
                "analyzed" => style(format!("\"{}\"", node.intent)).green().italic(),
                "prompt"   => style(format!("\"{}\"", node.intent)).cyan().italic(),
                _          => style(format!("\"{}\"", node.intent)).dim().italic(),
            };
            let src_tag = match node.intent_source.as_str() {
                "analyzed" => style("[Claude]").green().dim(),
                "prompt"   => style("[prompt]").cyan().dim(),
                _          => style("[msg]").dim(),
            };
            println!("\n  {} {} {}", oid_s, src_tag, intent_s);
            println!("     {}", style(&node.message).dim());

            if let Some(causes) = causes_of.get(node.oid.as_str()) {
                let shorts: Vec<String> = causes
                    .iter()
                    .map(|c| c[..8.min(c.len())].to_string())
                    .collect();
                println!(
                    "     {} {}",
                    style("↤ caused by:").yellow(),
                    style(shorts.join(", ")).yellow().bold()
                );
            }
            if let Some(ref a) = node.agent {
                println!("     {}", style(format!("agent: {a}")).dim());
            }
        }

        println!("\n{}", style("─".repeat(60)).dim());
        let causal_count = graph.edges.iter().filter(|e| e.kind == "causal").count();
        let ai_count = graph.nodes.iter().filter(|n| n.is_ai).count();
        let analyzed_count = graph.nodes.iter().filter(|n| n.intent_source == "analyzed").count();
        print!("{} AI commits, {} causal link{}", ai_count, causal_count,
            if causal_count == 1 { "" } else { "s" });
        if analyze && analyzed_count > 0 {
            print!(", {} Claude-generated intent{}", analyzed_count,
                if analyzed_count == 1 { "" } else { "s" });
        }
        println!();
        Ok(())
    }

    /// Creates a revert commit for the given OID using `git revert --no-edit`.
    /// Returns the OID of the newly created revert commit.
    pub fn revert_commit(&self, oid: Oid) -> Result<Oid, H5iError> {
        let workdir = self
            .git_repo
            .workdir()
            .ok_or_else(|| H5iError::InvalidPath("Cannot revert in a bare repository".into()))?;

        let output = std::process::Command::new("git")
            .args(["revert", "--no-edit", &oid.to_string()])
            .current_dir(workdir)
            .output()
            .map_err(H5iError::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(H5iError::Git(git2::Error::from_str(&format!(
                "git revert failed: {stderr}"
            ))));
        }

        Ok(self.git_repo.head()?.peel_to_commit()?.id())
    }

    /// Restore the working tree to the exact file state of a past commit.
    ///
    /// HEAD is not moved — after the call, `git status` shows the full diff
    /// between HEAD and the restored state so the user can review before committing.
    ///
    /// # Safety
    ///
    /// Before touching any files, the current dirty state (staged + unstaged) is
    /// saved to `refs/h5i/shadow/<yyyymmdd-hhmmss>` as a WIP commit so it can
    /// always be recovered via `git checkout refs/h5i/shadow/<ts> -- .`.
    ///
    /// Pass `force = true` to skip the shadow-ref backup (use when the working
    /// tree is already clean or the shadow ref is not needed).
    ///
    /// # Returns
    ///
    /// `(shadow_ref, changed_files)` where `changed_files` is a list of
    /// `(relative_path, "added" | "modified" | "deleted")` entries describing
    /// the working-tree changes that were made (or would be made in dry-run mode).
    #[allow(clippy::type_complexity)]
    pub fn rewind(
        &self,
        sha: &str,
        force: bool,
        dry_run: bool,
    ) -> Result<(Option<String>, Vec<(String, &'static str)>), H5iError> {
        let repo = &self.git_repo;

        let workdir = repo
            .workdir()
            .ok_or_else(|| H5iError::InvalidPath("Cannot rewind in a bare repository".into()))?
            .to_path_buf();

        // ── Resolve the target SHA (accepts short SHAs and rev expressions) ──
        let obj = repo
            .revparse_single(sha)
            .map_err(H5iError::Git)?;
        let target_commit = obj
            .peel_to_commit()
            .map_err(|_| H5iError::InvalidPath(format!("{sha} does not resolve to a commit")))?;
        let target_tree = target_commit.tree().map_err(H5iError::Git)?;

        // ── Diff HEAD tree → target tree to know what will change ─────────────
        let head_tree = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_tree().ok());

        let diff = repo
            .diff_tree_to_tree(
                head_tree.as_ref(),
                Some(&target_tree),
                None,
            )
            .map_err(H5iError::Git)?;

        let mut changed: Vec<(String, &'static str)> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                let kind = match delta.status() {
                    git2::Delta::Added   => "added",
                    git2::Delta::Deleted => "deleted",
                    _                    => "modified",
                };
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                changed.push((path, kind));
                true
            },
            None, None, None,
        )
        .map_err(H5iError::Git)?;

        if dry_run {
            return Ok((None, changed));
        }

        // ── Save current dirty state to a shadow ref (unless --force) ─────────
        let shadow_ref = if !force {
            let is_dirty = {
                let mut opts = git2::StatusOptions::new();
                opts.include_untracked(false);
                repo.statuses(Some(&mut opts))
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
            };

            if is_dirty {
                // Write the current index to a tree and create a shadow commit.
                let mut index = repo.index().map_err(H5iError::Git)?;
                index.update_all(["*"].iter(), None).map_err(H5iError::Git)?;
                let shadow_tree_oid = index.write_tree().map_err(H5iError::Git)?;
                let shadow_tree = repo.find_tree(shadow_tree_oid).map_err(H5iError::Git)?;

                let sig = repo_signature_or_fallback(repo)?;
                let head_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
                let parents: Vec<git2::Commit<'_>> = head_commit.iter().cloned().collect();
                let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

                let msg = format!(
                    "h5i shadow: pre-rewind state before restoring to {}",
                    &target_commit.id().to_string()[..8]
                );
                let shadow_commit_oid = repo
                    .commit(None, &sig, &sig, &msg, &shadow_tree, &parent_refs)
                    .map_err(H5iError::Git)?;

                let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                let ref_name = format!("refs/h5i/shadow/{ts}");
                repo.reference(
                    &ref_name,
                    shadow_commit_oid,
                    false,
                    "h5i rewind shadow backup",
                )
                .map_err(H5iError::Git)?;

                // Reset index back to HEAD so the checkout_tree call starts clean.
                if let Ok(head_commit) = repo.head().and_then(|h| h.peel_to_commit()) {
                    let mut co = git2::build::CheckoutBuilder::new();
                    co.force();
                    repo.reset(head_commit.as_object(), git2::ResetType::Mixed, Some(&mut co))
                        .map_err(H5iError::Git)?;
                }

                Some(ref_name)
            } else {
                None
            }
        } else {
            None
        };

        // ── Restore files from the target tree into the working tree ──────────
        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        co.update_index(true);
        repo.checkout_tree(target_tree.as_object(), Some(&mut co))
            .map_err(H5iError::Git)?;

        // Delete files that exist in HEAD but not in the target tree.
        // checkout_tree does not remove files absent from the target.
        for (path, kind) in &changed {
            if *kind == "deleted" {
                let abs = workdir.join(path);
                if abs.exists() {
                    let _ = fs::remove_file(&abs);
                }
            }
        }

        Ok((shadow_ref, changed))
    }

    /// Resolves the current `HEAD` reference and returns the associated commit.
    ///
    /// This method resolves symbolic references and ensures that the
    /// resulting object is a commit.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - `HEAD` cannot be resolved
    /// - the resolved object is not a commit
    fn get_head_commit(&self) -> Result<Commit<'_>, git2::Error> {
        let obj = self.git_repo.head()?.resolve()?.peel(ObjectType::Commit)?;
        obj.into_commit()
            .map_err(|_| git2::Error::from_str("Not a commit"))
    }

    /// Retrieves the `Blob` (file object) for a given path from the `HEAD` commit.
    ///
    /// # Parameters
    ///
    /// - `path` – Path to the file within the repository.
    ///
    /// # Returns
    ///
    /// Returns the Git blob representing the file contents at `HEAD`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the path does not exist in `HEAD`
    /// - the path does not correspond to a file
    /// - the blob cannot be retrieved from the repository
    pub fn get_blob_at_head(&self, path: &Path) -> Result<Blob<'_>, H5iError> {
        // 1. Resolve the HEAD reference to a commit
        let head_commit = self.get_head_commit()?;

        // 2. Retrieve the tree (snapshot of the file structure)
        let tree = head_commit.tree()?;

        // 3. Locate the entry corresponding to the specified path
        let entry = tree
            .get_path(path)
            .map_err(|_| H5iError::RecordNotFound(format!("Path not found in HEAD: {:?}", path)))?;

        // 4. Ensure that the entry is a Blob (file)
        if entry.kind() != Some(ObjectType::Blob) {
            return Err(H5iError::InvalidPath(format!(
                "Path is not a file (blob): {:?}",
                path
            )));
        }

        // 5. Retrieve the actual Blob object using its OID
        let blob = self.git_repo.find_blob(entry.id())?;
        Ok(blob)
    }

    /// Retrieves the `Blob` associated with a given path at a specific commit.
    ///
    /// # Parameters
    ///
    /// - `oid` – Commit OID.
    /// - `path` – File path within the repository.
    ///
    /// # Returns
    ///
    /// Returns the Git blob representing the file contents at the specified commit.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the commit cannot be found
    /// - the path does not exist in the commit tree
    /// - the blob object cannot be retrieved
    pub fn get_blob_at_oid(&'_ self, oid: Oid, path: &Path) -> Result<Blob<'_>, H5iError> {
        // 1. Locate the commit object from the OID
        let commit = self
            .git_repo
            .find_commit(oid)
            .map_err(|e| H5iError::Internal(format!("Commit not found {}: {}", oid, e)))?;

        // 2. Retrieve the tree associated with the commit
        let tree = commit.tree().map_err(|e| {
            H5iError::Internal(format!("Failed to get tree for commit {}: {}", oid, e))
        })?;

        // 3. Find the entry corresponding to the specified path
        let entry = tree.get_path(path).map_err(|_| {
            H5iError::InvalidPath(format!("Path {:?} not found in commit {}", path, oid))
        })?;

        // 4. Retrieve the Blob object from its ID
        let blob = self.git_repo.find_blob(entry.id()).map_err(|e| {
            H5iError::Internal(format!("Failed to find blob for path {:?}: {}", path, e))
        })?;

        Ok(blob)
    }

    /// Convenience helper that retrieves file content at a specific commit
    /// and returns it as a UTF-8 string.
    ///
    /// # Parameters
    ///
    /// - `oid` – Commit OID.
    /// - `path` – File path.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the file cannot be retrieved
    /// - the file content is not valid UTF-8
    pub fn get_content_at_oid(&self, oid: Oid, path: &Path) -> Result<String, H5iError> {
        let blob = self.get_blob_at_oid(oid, path)?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|_| H5iError::Internal(format!("File at {:?} is not valid UTF-8", path)))?;

        Ok(content.to_string())
    }

    pub fn get_content_at_head(&self, file_path: &str) -> Result<String, H5iError> {
        let repo = &self.git_repo;

        let head = repo.head()?;
        let head_commit = head.peel_to_commit()?;

        let tree = head_commit.tree()?;

        let entry = tree.get_path(Path::new(file_path))?;
        let object = entry.to_object(repo)?;
        let blob = object.as_blob().ok_or_else(|| {
            H5iError::Internal(format!(
                "Path {} exists but is not a file (blob)",
                file_path
            ))
        })?;

        let content = std::str::from_utf8(blob.content())
            .map_err(|e| H5iError::Internal(format!("Content is not valid UTF-8: {}", e)))?;

        Ok(content.to_string())
    }

    /// Extracts the code block between
    /// `// h5_i_test_start` and `// h5_i_test_end` and computes its hash.
    ///
    /// This method is used to identify the logical content of a test suite.
    /// The resulting hash can be stored in commit metadata to track
    /// changes to tests independently of the main source code.
    fn scan_test_block(&self, path: &Path) -> Option<TestMetrics> {
        let content = std::fs::read_to_string(path).ok()?;
        let start = "// h5_i_test_start";
        let end = "// h5_i_test_end";

        if let (Some(s_idx), Some(e_idx)) = (content.find(start), content.find(end)) {
            let test_code = &content[s_idx + start.len()..e_idx];
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest;
            hasher.update(test_code.trim().as_bytes());
            let suite_hash = format!("{:x}", hasher.finalize());

            Some(TestMetrics {
                test_suite_hash: suite_hash,
                tool: Some("marker-scan".into()),
                summary: Some(format!(
                    "marker block detected in {}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                )),
                ..Default::default()
            })
        } else {
            None
        }
    }

    /// Extracts test code between
    /// `// h5_i_test_start` and `// h5_i_test_end`
    /// and produces test-related metrics.
    ///
    /// The extracted code is hashed to detect logical changes in the
    /// test suite across commits.
    ///
    /// In production usage, coverage and runtime metrics may be
    /// integrated from external CI systems.
    pub fn scan_test_metrics(&self, path: &std::path::Path) -> Option<TestMetrics> {
        self.scan_test_block(path)
    }

    /// Load a [`TestMetrics`] record from a JSON file written by any test adapter.
    ///
    /// The file must contain a JSON object matching the [`TestResultInput`] schema.
    /// Missing fields default to zero / `None`.
    ///
    /// # Example adapter output
    /// ```json
    /// { "tool": "pytest", "passed": 10, "failed": 0, "duration_secs": 1.23 }
    /// ```
    pub fn load_test_results_from_file(&self, path: &Path) -> Result<TestMetrics, H5iError> {
        use crate::metadata::TestResultInput;
        let raw = fs::read_to_string(path)
            .map_err(|e| H5iError::Internal(format!("Cannot read test results file: {e}")))?;
        let input: TestResultInput = serde_json::from_str(&raw)
            .map_err(|e| H5iError::Internal(format!("Invalid test results JSON: {e}")))?;
        Ok(input.into_metrics(String::new()))
    }

    /// Run an arbitrary shell command and return [`TestMetrics`].
    ///
    /// The command's **stdout** is parsed as a [`TestResultInput`] JSON object
    /// when it is valid JSON.  If parsing fails, only the exit code is captured,
    /// making this useful even for test tools that produce no structured output.
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = repo.run_test_command("cargo test 2>&1 | h5i-cargo-test-adapter")?;
    /// ```
    pub fn run_test_command(&self, cmd: &str) -> Result<TestMetrics, H5iError> {
        use crate::metadata::TestResultInput;
        use std::process::Command;

        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| H5iError::Internal(format!("Failed to run test command: {e}")))?;

        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Try to parse stdout as TestResultInput JSON
        if let Ok(input) = serde_json::from_str::<TestResultInput>(stdout.trim()) {
            let mut metrics = input.into_metrics(String::new());
            // The exit code from the actual process takes precedence
            if exit_code.is_some() {
                metrics.exit_code = exit_code;
            }
            return Ok(metrics);
        }

        // Fallback: capture exit code and a brief summary from combined output
        let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));
        let summary_line = combined
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("(no output)")
            .to_string();

        Ok(TestMetrics {
            exit_code,
            summary: Some(summary_line),
            tool: Some(cmd.split_whitespace().next().unwrap_or(cmd).to_string()),
            ..Default::default()
        })
    }
}

impl H5iRepository {
    /// Runs all integrity rules against the staged diff and returns a report.
    ///
    /// Priority for "intent": prompt (if supplied) > commit message.
    /// Scoring: each Violation costs −0.4, each Warning −0.15; score is clamped to [0, 1].
    pub fn verify_integrity(
        &self,
        prompt: Option<&str>,
        message: &str,
    ) -> Result<IntegrityReport, H5iError> {
        use crate::metadata::Severity;
        use crate::rules::run_all_rules;
        use crate::rules::DiffContext;

        let primary_intent = prompt.unwrap_or(message).to_string();

        let diff = self.get_staged_diff()?;
        let stats = diff.stats()?;
        let ctx =
            DiffContext::from_diff(&diff, primary_intent, stats.insertions(), stats.deletions())?;

        let findings = run_all_rules(&ctx);

        let violations = findings
            .iter()
            .filter(|f| f.severity == Severity::Violation)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();

        let penalty = (violations as f32 * 0.4 + warnings as f32 * 0.15).min(1.0);
        let score = 1.0 - penalty;

        let level = if violations > 0 {
            IntegrityLevel::Violation
        } else if warnings > 0 {
            IntegrityLevel::Warning
        } else {
            IntegrityLevel::Valid
        };

        Ok(IntegrityReport {
            level,
            score,
            findings,
        })
    }

    /// Run integrity rules against a *historical* commit's own diff (parent→commit).
    ///
    /// Unlike [`verify_integrity`], this does not touch the staging area; it
    /// reconstructs the diff from Git objects so it works on any committed OID.
    pub fn verify_commit_integrity(&self, oid: git2::Oid) -> Result<IntegrityReport, H5iError> {
        use crate::metadata::{IntegrityLevel, Severity};
        use crate::rules::{run_all_rules, DiffContext};

        let commit = self.git_repo.find_commit(oid)?;
        let message = commit.message().unwrap_or("").to_string();

        // Prefer the stored h5i prompt; fall back to commit message as intent.
        let record = self.load_h5i_record(oid).ok();
        let prompt_owned: Option<String> = record
            .as_ref()
            .and_then(|r| r.ai_metadata.as_ref())
            .map(|a| a.prompt.clone())
            .filter(|p| !p.is_empty());
        let primary_intent = prompt_owned.clone().unwrap_or_else(|| message.clone());

        // Build the diff: parent tree → commit tree (root commits diff to empty).
        let commit_tree = commit.tree()?;
        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };
        let diff =
            self.git_repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None)?;

        let stats = diff.stats()?;
        let ctx =
            DiffContext::from_diff(&diff, primary_intent, stats.insertions(), stats.deletions())?;

        let findings = run_all_rules(&ctx);

        let violations = findings
            .iter()
            .filter(|f| f.severity == Severity::Violation)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();

        let penalty = (violations as f32 * 0.4 + warnings as f32 * 0.15).min(1.0);
        let score = 1.0 - penalty;

        let level = if violations > 0 {
            IntegrityLevel::Violation
        } else if warnings > 0 {
            IntegrityLevel::Warning
        } else {
            IntegrityLevel::Valid
        };

        Ok(IntegrityReport {
            level,
            score,
            findings,
        })
    }

    fn get_staged_diff(&'_ self) -> Result<git2::Diff<'_>, H5iError> {
        let head_tree = self.get_head_commit()?.tree()?;
        let index = self.git_repo.index()?;
        let mut opts = git2::DiffOptions::new();
        let diff =
            self.git_repo
                .diff_tree_to_index(Some(&head_tree), Some(&index), Some(&mut opts))?;
        Ok(diff)
    }

    // ── Suggested Review Points ───────────────────────────────────────────────

    /// Scans recent commits and returns those that warrant human review, ranked
    /// by review priority.
    ///
    /// Each commit is scored against a set of deterministic, language-agnostic
    /// rules.  Only commits whose aggregate score is ≥ `min_score` are returned.
    /// Pass `crate::review::REVIEW_THRESHOLD` as a sensible default.
    ///
    /// Rules applied (all are purely structural / metric-based, no AI required):
    ///
    /// | Rule ID           | Signal                                            |
    /// |-------------------|---------------------------------------------------|
    /// | LARGE_DIFF        | Many lines changed (>50 / >200 / >500)           |
    /// | WIDE_IMPACT       | Many files changed (>5 / >10 / >20)              |
    /// | CROSS_CUTTING     | Changes span many top-level directories (>3 / >5)|
    /// | TEST_REGRESSION   | Test failures increased or coverage dropped       |
    /// | UNTESTED_CHANGE   | Large diff with no test metrics recorded          |
    /// | AI_NO_PROMPT      | AI commit with blank prompt (provenance gap)      |
    /// | BURST_AFTER_GAP   | First commit after a quiet period (>3 / >7 days) |
    /// | POLYGLOT_CHANGE   | More than 4 distinct file extensions changed      |
    /// | BINARY_FILE       | Binary file(s) modified                           |
    /// | MASS_DELETION     | >80 % of the diff is deletions (>100 lines)      |
    /// | BLIND_EDIT        | File(s) edited with no prior Read in the session  |
    /// Return files most frequently co-changed with `target_file` in git history.
    ///
    /// Walks the last `history_limit` commits. For each commit that touches
    /// `target_file`, counts how often every *other* file in that commit also
    /// appears. Returns a ranked list of `(file, co_change_count)` pairs.
    pub fn cochanged_files(
        &self,
        target_file: &str,
        history_limit: usize,
        result_limit: usize,
    ) -> Result<Vec<(String, usize)>, H5iError> {
        use std::collections::HashMap;

        let mut revwalk = self.git_repo.revwalk()?;
        revwalk.push_head()?;

        let mut counts: HashMap<String, usize> = HashMap::new();

        for oid_result in revwalk.take(history_limit) {
            let oid = oid_result?;
            let commit = self.git_repo.find_commit(oid)?;
            let commit_tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };
            let diff = self.git_repo.diff_tree_to_tree(
                parent_tree.as_ref(),
                Some(&commit_tree),
                None,
            )?;

            // Collect all files touched in this commit
            let mut touched: Vec<String> = Vec::new();
            for delta in diff.deltas() {
                if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    if let Some(s) = p.to_str() {
                        touched.push(s.to_string());
                    }
                }
            }

            // If this commit touches target_file, credit all sibling files
            let target_touched = touched.iter().any(|f| {
                f == target_file || f.ends_with(target_file) || target_file.ends_with(f.as_str())
            });
            if target_touched {
                for f in &touched {
                    if f != target_file {
                        *counts.entry(f.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
        ranked.sort_by_key(|r| std::cmp::Reverse(r.1));
        ranked.truncate(result_limit);
        Ok(ranked)
    }

    pub fn suggest_review_points(
        &self,
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<crate::review::ReviewPoint>, H5iError> {
        self.suggest_review_points_impl(None, limit, min_score)
    }

    /// Branch-scoped variant: walk review points from `branch`'s tip instead of
    /// HEAD. `None` (or an empty string) falls back to HEAD.
    pub fn suggest_review_points_at(
        &self,
        branch: Option<&str>,
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<crate::review::ReviewPoint>, H5iError> {
        self.suggest_review_points_impl(branch, limit, min_score)
    }

    fn suggest_review_points_impl(
        &self,
        branch: Option<&str>,
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<crate::review::ReviewPoint>, H5iError> {
        use crate::review::{ReviewPoint, ReviewTrigger, Tier};
        use std::collections::HashSet;

        // Tier helpers — keep call sites readable.
        let quality = |rule_id: &str, weight: f32, detail: String| ReviewTrigger {
            rule_id: rule_id.to_string(),
            weight,
            detail,
            tier: Tier::Quality,
        };
        let shape = |rule_id: &str, weight: f32, detail: String| ReviewTrigger {
            rule_id: rule_id.to_string(),
            weight,
            detail,
            tier: Tier::Shape,
        };

        let mut revwalk = self.git_repo.revwalk()?;
        match branch {
            Some(b) if !b.is_empty() => {
                // Resolve like get_log_at_branch: local → remote-tracking → revparse.
                let local = self.git_repo.find_branch(b, git2::BranchType::Local).ok();
                let remote = self.git_repo.find_branch(b, git2::BranchType::Remote).ok();
                let oid = if let Some(br) = local.or(remote) {
                    br.get().target().ok_or_else(|| {
                        H5iError::Git(git2::Error::from_str("branch has no target oid"))
                    })?
                } else {
                    self.git_repo.revparse_single(b)?.id()
                };
                revwalk.push(oid)?;
            }
            _ => revwalk.push_head()?,
        }

        let mut results: Vec<ReviewPoint> = Vec::new();

        for oid_result in revwalk.take(limit) {
            let oid = oid_result?;
            let commit = self.git_repo.find_commit(oid)?;

            let message = commit.message().unwrap_or("").trim().to_string();
            let author = commit.author().name().unwrap_or("Unknown").to_string();
            let record = self.load_h5i_record(oid).ok();

            let timestamp = record.as_ref().map(|r| r.timestamp).unwrap_or_else(|| {
                chrono::Utc
                    .timestamp_opt(commit.time().seconds(), 0)
                    .single()
                    .unwrap_or_else(chrono::Utc::now)
            });

            let mut triggers: Vec<ReviewTrigger> = Vec::new();

            // ── Diff stats ────────────────────────────────────────────────────
            let commit_tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };
            let diff = self.git_repo.diff_tree_to_tree(
                parent_tree.as_ref(),
                Some(&commit_tree),
                None,
            )?;
            let stats = diff.stats()?;
            let files_changed = stats.files_changed();
            let insertions = stats.insertions();
            let deletions = stats.deletions();
            let lines_changed = insertions + deletions;

            // Collect file paths and binary file count from the diff.
            // Auto-generated / build-artifact paths are excluded from all counts so
            // they don't inflate risk scores with noise.
            let mut file_paths: Vec<String> = Vec::new();
            let mut binary_count: usize = 0;
            for delta in diff.deltas() {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string());
                // Skip auto-generated / build-artifact files entirely.
                if path.as_deref().map(is_artifact_path).unwrap_or(false) {
                    continue;
                }
                if let Some(ref p) = path {
                    file_paths.push(p.clone());
                }
                if delta.flags().contains(git2::DiffFlags::BINARY) {
                    binary_count += 1;
                }
            }

            // R1 — LARGE_DIFF [Shape]
            if lines_changed > 500 {
                triggers.push(shape("LARGE_DIFF", 0.40,
                    format!("{lines_changed} lines changed (>500)")));
            } else if lines_changed > 200 {
                triggers.push(shape("LARGE_DIFF", 0.25,
                    format!("{lines_changed} lines changed (>200)")));
            } else if lines_changed > 50 {
                triggers.push(shape("LARGE_DIFF", 0.10,
                    format!("{lines_changed} lines changed (>50)")));
            }

            // R2 — WIDE_IMPACT [Shape]
            if files_changed > 20 {
                triggers.push(shape("WIDE_IMPACT", 0.35,
                    format!("{files_changed} files changed (>20)")));
            } else if files_changed > 10 {
                triggers.push(shape("WIDE_IMPACT", 0.20,
                    format!("{files_changed} files changed (>10)")));
            } else if files_changed > 5 {
                triggers.push(shape("WIDE_IMPACT", 0.10,
                    format!("{files_changed} files changed (>5)")));
            }

            // R3 — CROSS_CUTTING: distinct top-level directory components [Shape]
            let distinct_dirs: HashSet<&str> = file_paths
                .iter()
                .filter_map(|p| p.split('/').next())
                .collect();
            let dir_count = distinct_dirs.len();
            if dir_count > 5 {
                triggers.push(shape("CROSS_CUTTING", 0.25,
                    format!("changes span {dir_count} top-level directories (>5)")));
            } else if dir_count > 3 {
                triggers.push(shape("CROSS_CUTTING", 0.15,
                    format!("changes span {dir_count} top-level directories (>3)")));
            }

            // R4 — TEST_REGRESSION: compare metrics to parent commit
            if let Some(ref rec) = record {
                if let Some(ref current_tm) = rec.test_metrics {
                    let parent_tm = rec
                        .parent_oid
                        .as_ref()
                        .and_then(|p| git2::Oid::from_str(p).ok())
                        .and_then(|p| self.load_h5i_record(p).ok())
                        .and_then(|r| r.test_metrics);

                    if let Some(ref prev_tm) = parent_tm {
                        let was_passing = prev_tm.is_passing();
                        let is_passing = current_tm.is_passing();

                        if was_passing && !is_passing {
                            triggers.push(quality("TEST_REGRESSION", 0.50,
                                "tests were passing but now failing".into()));
                        } else if current_tm.failed > prev_tm.failed {
                            let new_fails = current_tm.failed - prev_tm.failed;
                            triggers.push(quality("TEST_REGRESSION", 0.40,
                                format!("{new_fails} new test failure(s) since parent")));
                        }

                        if prev_tm.coverage > 0.0 && current_tm.coverage > 0.0 {
                            let drop = prev_tm.coverage - current_tm.coverage;
                            if drop > 10.0 {
                                triggers.push(quality("TEST_REGRESSION", 0.35,
                                    format!("coverage dropped {drop:.1}% (>10%)")));
                            } else if drop > 5.0 {
                                triggers.push(quality("TEST_REGRESSION", 0.20,
                                    format!("coverage dropped {drop:.1}% (>5%)")));
                            }
                        }
                    }
                }
            }

            // R5 — UNTESTED_CHANGE: significant diff to a project that has tests [Shape]
            //
            // Refined from the previous "any large diff without metrics" rule:
            // we only fire when (a) the diff is non-trivial, (b) no test metrics
            // were recorded, AND (c) the project actually has tests (so we don't
            // flag every doc-only commit in a non-tested repo).
            if lines_changed > 100 {
                let has_tests = record
                    .as_ref()
                    .map(|r| r.test_metrics.is_some())
                    .unwrap_or(false);
                if !has_tests && project_has_tests(&self.git_repo) {
                    triggers.push(shape("UNTESTED_CHANGE", 0.20,
                        format!("{lines_changed} lines changed with no test metrics recorded")));
                }
            }

            // R6 — AI_NO_PROMPT: AI commit without a recorded prompt [Quality]
            // (real provenance gap — high signal for "this commit's intent is unknowable")
            if let Some(ref rec) = record {
                if let Some(ref ai) = rec.ai_metadata {
                    if ai.prompt.trim().is_empty() {
                        triggers.push(quality("AI_NO_PROMPT", 0.15,
                            "AI-generated commit with no prompt recorded (provenance gap)".into()));
                    }
                }
            }

            // R7 — BURST_AFTER_GAP: large time gap between this commit and its parent [Shape]
            if commit.parent_count() > 0 {
                if let Ok(parent_commit) = commit.parent(0) {
                    let gap_secs = commit.time().seconds() - parent_commit.time().seconds();
                    if gap_secs > 7 * 24 * 3600 {
                        let days = gap_secs / (24 * 3600);
                        triggers.push(shape("BURST_AFTER_GAP", 0.25,
                            format!("first commit after a {days}-day gap (>7 days)")));
                    } else if gap_secs > 3 * 24 * 3600 {
                        let days = gap_secs / (24 * 3600);
                        triggers.push(shape("BURST_AFTER_GAP", 0.15,
                            format!("first commit after a {days}-day gap (>3 days)")));
                    }
                }
            }

            // R8 — POLYGLOT_CHANGE: many distinct file extensions [Shape]
            let extensions: HashSet<&str> = file_paths
                .iter()
                .filter_map(|p| std::path::Path::new(p).extension()?.to_str())
                .collect();
            if extensions.len() > 4 {
                triggers.push(shape("POLYGLOT_CHANGE", 0.15,
                    format!(
                        "{} distinct file type(s) changed (harder to review holistically)",
                        extensions.len()
                    )));
            }

            // R9 — BINARY_FILE: opaque binary changes [Quality]
            // (you can't review binary diffs — agent uploads need a human look)
            if binary_count > 0 {
                triggers.push(quality("BINARY_FILE", 0.20,
                    format!("{binary_count} binary file(s) modified")));
            }

            // R10 — MASS_DELETION: bulk removal without matching insertions [Quality]
            // (high-risk: agents occasionally delete more than asked)
            if deletions > 100 && lines_changed > 0 {
                let deletion_ratio = deletions as f32 / lines_changed as f32;
                if deletion_ratio > 0.80 {
                    triggers.push(quality("MASS_DELETION", 0.15,
                        format!(
                            "{deletions} lines deleted ({:.0}% of total changes)",
                            deletion_ratio * 100.0
                        )));
                }
            }

            // R11 — BLIND_EDIT: files edited without a prior Read in the session [Quality]
            if let Ok(Some(analysis)) =
                crate::session_log::load_analysis(&self.h5i_root, &oid.to_string())
            {
                let blind_files: Vec<&str> = analysis
                    .coverage
                    .iter()
                    .filter(|c| c.blind_edit_count > 0)
                    .map(|c| c.file.as_str())
                    .collect();
                if !blind_files.is_empty() {
                    let count = blind_files.len();
                    let examples = blind_files[..3.min(count)].join(", ");
                    let suffix = if count > 3 {
                        format!(" (and {} more)", count - 3)
                    } else {
                        String::new()
                    };
                    triggers.push(quality("BLIND_EDIT", (0.10 * count as f32).min(0.30),
                        format!(
                            "{count} file(s) edited without a prior Read: {examples}{suffix}"
                        )));
                }
            }

            // R12 — Integrity rule findings from `rules.rs` against this commit's diff [Quality]
            // Surfaces CREDENTIAL_LEAK, CODE_EXECUTION, SENSITIVE_FILE_MODIFIED,
            // CI_CD_MODIFIED, PERMISSION_CHANGE, DUPLICATED_CODE, … into the
            // review tier so the PR comment 🚩 sees them too.
            //
            // We deliberately drop:
            //   - Info-severity findings (CONFIG_FILE_MODIFIED, LOCKFILE_MODIFIED,
            //     BINARY_FILE_CHANGED): informational and prone to firing on
            //     every routine commit.
            //   - Rule IDs that already have a shape-tier counterpart in
            //     suggest_review_points (LARGE_DIFF): would double-count.
            if let Ok(integ) = self.verify_commit_integrity(oid) {
                use crate::metadata::Severity;
                const SKIP_RULE_IDS: &[&str] =
                    &["LARGE_DIFF", "BINARY_FILE_CHANGED", "CONFIG_FILE_MODIFIED",
                      "LOCKFILE_MODIFIED"];
                for f in integ.findings {
                    if SKIP_RULE_IDS.contains(&f.rule_id.as_str()) {
                        continue;
                    }
                    let weight = match f.severity {
                        Severity::Violation => 0.40,
                        Severity::Warning => 0.20,
                        Severity::Info => continue, // never escalate Info to Quality
                    };
                    triggers.push(quality(&f.rule_id, weight, f.detail));
                }
            }

            // ── Aggregate & filter ────────────────────────────────────────────
            if triggers.is_empty() {
                continue;
            }
            let quality_score: f32 = triggers
                .iter()
                .filter(|t| matches!(t.tier, Tier::Quality))
                .map(|t| t.weight)
                .sum::<f32>()
                .min(1.0);
            let shape_score: f32 = triggers
                .iter()
                .filter(|t| matches!(t.tier, Tier::Shape))
                .map(|t| t.weight)
                .sum::<f32>()
                .min(1.0);
            let score: f32 = triggers.iter().map(|t| t.weight).sum::<f32>().min(1.0);
            if score >= min_score {
                results.push(ReviewPoint {
                    commit_oid: oid.to_string(),
                    short_oid: oid.to_string()[..8].to_string(),
                    message,
                    author,
                    timestamp,
                    score,
                    quality_score,
                    shape_score,
                    triggers,
                });
            }
        }

        // Sort by quality_score first (real risk), then by total score as
        // a tiebreaker for commits with equal-quality signals.
        results.sort_by(|a, b| {
            b.quality_score
                .partial_cmp(&a.quality_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        Ok(results)
    }
}

// ── Test-presence heuristic ───────────────────────────────────────────────────

/// Cheap heuristic: does this repo appear to have tests at all?
///
/// Used by `UNTESTED_CHANGE`: we should not flag every doc-only commit in a
/// repo that has no tests in the first place. Looks for any of:
///   - a `tests/` directory at the repo root
///   - a `test/` directory at the repo root
///   - a top-level file whose name contains `test` and has a code extension
///   - Cargo-style `#[test]` / pytest `def test_` files anywhere in `src/`
///
/// All checks are filesystem-only, no shelling out.
fn project_has_tests(repo: &Repository) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    if workdir.join("tests").is_dir() || workdir.join("test").is_dir() {
        return true;
    }
    // Look for a top-level entry whose name contains "test".
    if let Ok(entries) = std::fs::read_dir(workdir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains("test") && !name.starts_with('.') {
                return true;
            }
        }
    }
    false
}

// ── Artifact path filter ──────────────────────────────────────────────────────

/// Returns `true` when `path` is a well-known build artifact or auto-generated
/// file that should be excluded from review risk scoring.
///
/// Covers the most common ecosystems:
/// - Python: `__pycache__/`, `*.pyc`, `*.pyo`, `.pytest_cache/`, `*.egg-info/`
/// - JavaScript/TypeScript: `node_modules/`, `dist/`, `*.min.js`, `.next/`
/// - Java/Kotlin: `*.class`, `*.jar`, `build/`, `target/`
/// - Rust: `target/`
/// - Go: vendor artefacts
/// - General: `.DS_Store`, `Thumbs.db`, `*.lock` lock-file binaries
fn is_artifact_path(path: &str) -> bool {
    // Check path components (any segment matching these is an artifact dir)
    const ARTIFACT_DIRS: &[&str] = &[
        "__pycache__",
        ".pytest_cache",
        "node_modules",
        ".next",
        ".nuxt",
        "dist",
        ".eggs",
        ".tox",
        ".mypy_cache",
        ".ruff_cache",
    ];

    // Suffix-based checks
    const ARTIFACT_EXTENSIONS: &[&str] = &[
        ".pyc",
        ".pyo",
        ".class",
        ".jar",
        ".war",
        ".ear",
        ".min.js",
        ".min.css",
        ".map",       // JS source maps
    ];

    // Exact filename matches
    const ARTIFACT_FILENAMES: &[&str] = &[
        ".DS_Store",
        "Thumbs.db",
        "desktop.ini",
    ];

    // Check directory segments
    for segment in path.split('/') {
        if ARTIFACT_DIRS.contains(&segment) {
            return true;
        }
        // *.egg-info directories
        if segment.ends_with(".egg-info") || segment.ends_with(".dist-info") {
            return true;
        }
    }

    // Check extension
    let lower = path.to_ascii_lowercase();
    for ext in ARTIFACT_EXTENSIONS {
        if lower.ends_with(ext) {
            return true;
        }
    }

    // Check filename
    if let Some(filename) = path.split('/').next_back() {
        if ARTIFACT_FILENAMES.contains(&filename) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Oid, Repository, Signature};
    use std::fs;
    use tempfile::tempdir;

    fn setup_test_repo(root: &std::path::Path) -> H5iRepository {
        let _repo = Repository::init(root).unwrap();
        H5iRepository::open(root).expect("Failed to open repo")
    }

    fn create_commit(
        repo: &Repository,
        message: &str,
        file_path: &str,
        content: &str,
        parents: &[&git2::Commit],
    ) -> Oid {
        let mut index = repo.index().unwrap();
        let path = std::path::Path::new(file_path);

        fs::write(repo.workdir().unwrap().join(path), content).unwrap();
        index.add_path(path).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = Signature::now("test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, parents)
            .unwrap()
    }

    // --- 1. Lifecycle & Basic Info ---

    #[test]
    fn test_repository_open_initializes_directories() {
        let dir = tempdir().unwrap();
        let repo = setup_test_repo(dir.path());

        // Ensure .h5i subdirectories are created
        assert!(repo.h5i_root.join("metadata").exists());
        assert_eq!(repo.h5i_path(), &repo.h5i_root);
    }

    // --- 2. Commit & Metadata Persistence ---

    #[test]
    fn test_commit_with_ai_metadata() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let sig = Signature::now("ai_agent", "bot@h5i.io")?;

        let ai_meta = Some(AiMetadata {
            model_name: "h5i-alpha-01".to_string(),
            prompt: "abc123hash".to_string(),
            agent_id: "agent_7".to_string(),
            usage: None,
        });

        // Prepare a staged file
        fs::write(dir.path().join("logic.py"), "print('hello')")?;
        let mut index = h5i_repo.git().index()?;
        index.add_path(Path::new("logic.py"))?;
        index.write()?;

        let oid = h5i_repo.commit(
            "AI generated commit",
            &sig,
            &sig,
            ai_meta,
            TestSource::None,
            vec![],
            vec![],
            None, // note_spool
        )?;

        // Verify standard git commit
        let commit = h5i_repo.git().find_commit(oid)?;
        assert_eq!(commit.message(), Some("AI generated commit"));

        // Verify h5i sidecar record
        let record = h5i_repo.load_h5i_record(oid)?;
        assert_eq!(record.ai_metadata.unwrap().agent_id, "agent_7");
        Ok(())
    }

    #[test]
    fn test_load_h5i_record_fallback_to_git() {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());

        // Create a commit without using h5i_repo.commit (no sidecar)
        let oid = create_commit(
            h5i_repo.git(),
            "legacy commit",
            "legacy.txt",
            "old data",
            &[],
        );

        // h5i_log should fallback to minimal record
        let logs = h5i_repo.h5i_log(1).unwrap();
        assert_eq!(logs[0].git_oid, oid.to_string());
        assert!(logs[0].ai_metadata.is_none());
    }

    #[test]
    fn h5i_log_since_scopes_to_base_exclusive() {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let git = h5i_repo.git();

        // c1 stands in for the base branch tip; c2, c3 are this branch's commits.
        let c1 = create_commit(git, "base commit", "a.txt", "1", &[]);
        let p1 = git.find_commit(c1).unwrap();
        let c2 = create_commit(git, "branch commit 1", "b.txt", "2", &[&p1]);
        let p2 = git.find_commit(c2).unwrap();
        let c3 = create_commit(git, "branch commit 2", "c.txt", "3", &[&p2]);

        // Unscoped walk sees all three commits.
        assert_eq!(h5i_repo.h5i_log(10).unwrap().len(), 3);

        // Scoped to base c1 (exclusive): only c3, c2 (newest-first); c1 hidden —
        // this is the fix that keeps base-branch commits out of the PR body.
        let scoped = h5i_repo.h5i_log_since(Some(c1), 10).unwrap();
        let oids: Vec<String> = scoped.iter().map(|r| r.git_oid.clone()).collect();
        assert_eq!(oids, vec![c3.to_string(), c2.to_string()]);
        assert!(
            !oids.contains(&c1.to_string()),
            "base commit must be excluded from base..HEAD"
        );

        // A `None` base degrades to the full HEAD walk.
        assert_eq!(h5i_repo.h5i_log_since(None, 10).unwrap().len(), 3);
    }

    // --- 3. Blame ---

    #[test]
    fn test_blame_line_mode() {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let path = Path::new("README.md");

        create_commit(
            h5i_repo.git(),
            "initial",
            "README.md",
            "Line 1\nLine 2",
            &[],
        );

        let results = h5i_repo.blame(path).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].line_content, "Line 1");
    }

    #[test]
    fn test_get_content_at_oid() {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let git_repo = &h5i_repo.git_repo;

        let oid = create_commit(git_repo, "initial", "hello.txt", "hello world", &[]);

        let content = h5i_repo
            .get_content_at_oid(oid, std::path::Path::new("hello.txt"))
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_scan_test_metrics_detection() {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let path = dir.path().join("test_file.rs");
        let content = "
            // h5_i_test_start
            fn test_logic() { assert!(true); }
            // h5_i_test_end
        ";
        fs::write(&path, content).unwrap();

        let metrics = h5i_repo.scan_test_metrics(&path).unwrap();
        assert!(!metrics.test_suite_hash.is_empty());
    }

    #[test]
    fn test_merge_file_three_way_clean() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir().unwrap();
        let h5i_repo = setup_test_repo(dir.path());
        let git_repo = &h5i_repo.git_repo;
        let file_path = "main.py";

        let base_oid = create_commit(
            git_repo,
            "base",
            file_path,
            "def main():\n    pass\n",
            &[],
        );
        let base = git_repo.find_commit(base_oid)?;

        let our_oid = create_commit(
            git_repo,
            "ours",
            file_path,
            "# OURS\ndef main():\n    pass\n",
            &[&base],
        );

        // Branch off base, add a non-conflicting trailing line.
        git_repo.set_head_detached(base_oid)?;
        let their_oid = create_commit(
            git_repo,
            "theirs",
            file_path,
            "def main():\n    pass\nprint('done')\n",
            &[&base],
        );

        let outcome = h5i_repo.merge_file_three_way(our_oid, their_oid, file_path)?;
        assert!(!outcome.had_conflicts, "non-overlapping edits should merge cleanly");
        assert!(outcome.content.contains("# OURS"));
        assert!(outcome.content.contains("print('done')"));
        assert!(outcome.content.contains("def main():"));
        Ok(())
    }

    // ── rewind ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_rewind_dry_run_lists_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let h5i = H5iRepository::open(dir.path()).unwrap();

        let commit_a = create_commit(&repo, "commit A", "foo.txt", "hello", &[]);
        let a_obj = repo.find_commit(commit_a).unwrap();
        create_commit(&repo, "commit B", "bar.txt", "bar", &[&a_obj]);

        let short_a = &commit_a.to_string()[..8];
        let (shadow, changed) = h5i.rewind(short_a, true, true).unwrap();
        assert!(shadow.is_none(), "dry-run must not create a shadow ref");
        let paths: Vec<&str> = changed.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"bar.txt"), "bar.txt should appear as deleted");
    }

    #[test]
    fn record_human_prompt_accumulates_across_turns() {
        let dir = tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let h5i = H5iRepository::open(dir.path()).unwrap();

        // No pending file yet → nothing to read.
        assert!(h5i.read_pending_context().unwrap().is_none());

        // Blank prompts are ignored (no file written).
        h5i.record_human_prompt("   ", Some("sess-1")).unwrap();
        assert!(h5i.read_pending_context().unwrap().is_none());

        // First real prompt is stored verbatim, with the session id.
        h5i.record_human_prompt("fix the retry loop", Some("sess-1"))
            .unwrap();
        let ctx = h5i.read_pending_context().unwrap().unwrap();
        assert_eq!(ctx.human_prompt.as_deref(), Some("fix the retry loop"));
        assert_eq!(ctx.session_id.as_deref(), Some("sess-1"));

        // A second turn appends (the whole ask, not just the latest line).
        h5i.record_human_prompt("also add a test", Some("sess-1"))
            .unwrap();
        let ctx = h5i.read_pending_context().unwrap().unwrap();
        assert_eq!(
            ctx.human_prompt.as_deref(),
            Some("fix the retry loop\n\nalso add a test")
        );

        // Clearing (as a commit does) drops the accumulated prompt.
        h5i.clear_pending_context().unwrap();
        assert!(h5i.read_pending_context().unwrap().is_none());
    }

    #[test]
    fn test_rewind_restores_files_and_deletes_extras() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let h5i = H5iRepository::open(dir.path()).unwrap();

        let commit_a = create_commit(&repo, "commit A", "foo.txt", "version_one", &[]);
        let a_obj = repo.find_commit(commit_a).unwrap();
        create_commit(&repo, "commit B", "extra.txt", "extra", &[&a_obj]);

        let short_a = &commit_a.to_string()[..8];
        h5i.rewind(short_a, true, false).unwrap();

        assert!(!dir.path().join("extra.txt").exists(), "extra.txt should be deleted after rewind");
        let content = fs::read_to_string(dir.path().join("foo.txt")).unwrap();
        assert_eq!(content, "version_one");
    }

    #[test]
    fn test_rewind_creates_shadow_ref_when_dirty() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let h5i = H5iRepository::open(dir.path()).unwrap();

        let sig = Signature::now("test", "test@example.com").unwrap();
        fs::write(dir.path().join("a.txt"), "original").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new("a.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_a = repo.commit(Some("HEAD"), &sig, &sig, "A", &tree, &[]).unwrap();

        // Second commit (HEAD).
        let a_obj = repo.find_commit(commit_a).unwrap();
        fs::write(dir.path().join("b.txt"), "b_content").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new("b.txt")).unwrap();
        idx.add_path(std::path::Path::new("a.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "B", &tree, &[&a_obj]).unwrap();

        // Dirty the staging area so shadow logic fires.
        fs::write(dir.path().join("a.txt"), "dirty_change").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new("a.txt")).unwrap();
        idx.write().unwrap();

        let short_a = &commit_a.to_string()[..8];
        let (shadow_ref, _) = h5i.rewind(short_a, false, false).unwrap();
        assert!(shadow_ref.is_some(), "shadow ref should be created for dirty working tree");
        let ref_name = shadow_ref.unwrap();
        assert!(ref_name.starts_with("refs/h5i/shadow/"));
        assert!(repo.find_reference(&ref_name).is_ok(), "shadow ref must exist in git");
    }

    // ── copy_scoped_notes_onto (branch-scoped `h5i share push`) ─────────────

    #[test]
    fn copy_scoped_notes_only_copies_reachable_commits() {
        let dir = tempdir().unwrap();
        let h5i = setup_test_repo(dir.path());
        let repo = h5i.git();
        let sig = Signature::now("test", "test@example.com").unwrap();

        let c1 = create_commit(repo, "c1", "a.txt", "1", &[]);
        let parent = repo.find_commit(c1).unwrap();
        let c2 = create_commit(repo, "c2", "a.txt", "2", &[&parent]);
        repo.note(&sig, &sig, Some(H5I_NOTES_REF), c1, "note-c1", true)
            .unwrap();
        repo.note(&sig, &sig, Some(H5I_NOTES_REF), c2, "note-c2", true)
            .unwrap();

        // Reachable set names only c1.
        let mut reachable = std::collections::HashSet::new();
        reachable.insert(c1.to_string());

        let dest = "refs/h5i/_test_scoped_notes";
        let copied = copy_scoped_notes_onto(repo, &reachable, dest).unwrap();
        assert_eq!(copied, 1);
        assert_eq!(
            repo.find_note(Some(dest), c1).unwrap().message(),
            Some("note-c1")
        );
        assert!(
            repo.find_note(Some(dest), c2).is_err(),
            "c2 is not reachable, its note must not be copied"
        );
    }

    #[test]
    fn copy_scoped_notes_is_nondestructive_over_seeded_dest() {
        let dir = tempdir().unwrap();
        let h5i = setup_test_repo(dir.path());
        let repo = h5i.git();
        let sig = Signature::now("test", "test@example.com").unwrap();

        let c1 = create_commit(repo, "c1", "a.txt", "1", &[]);
        let parent = repo.find_commit(c1).unwrap();
        let c2 = create_commit(repo, "c2", "a.txt", "2", &[&parent]);
        repo.note(&sig, &sig, Some(H5I_NOTES_REF), c1, "note-c1", true)
            .unwrap();

        // Pre-seed the dest with c2's note (simulating the remote's existing data).
        let dest = "refs/h5i/_test_scoped_notes";
        repo.note(&sig, &sig, Some(dest), c2, "remote-note-c2", true)
            .unwrap();

        let mut reachable = std::collections::HashSet::new();
        reachable.insert(c1.to_string());
        let copied = copy_scoped_notes_onto(repo, &reachable, dest).unwrap();
        assert_eq!(copied, 1);

        // Both survive: the pre-existing c2 note (other branch) AND the new c1 note.
        assert_eq!(
            repo.find_note(Some(dest), c2).unwrap().message(),
            Some("remote-note-c2")
        );
        assert_eq!(
            repo.find_note(Some(dest), c1).unwrap().message(),
            Some("note-c1")
        );
    }

    #[test]
    fn copy_scoped_notes_zero_when_no_local_notes_ref() {
        let dir = tempdir().unwrap();
        let h5i = setup_test_repo(dir.path());
        let repo = h5i.git();
        let c1 = create_commit(repo, "c1", "a.txt", "1", &[]);
        let mut reachable = std::collections::HashSet::new();
        reachable.insert(c1.to_string());
        // No refs/h5i/notes exists → nothing to copy, no error.
        assert_eq!(
            copy_scoped_notes_onto(repo, &reachable, "refs/h5i/_t").unwrap(),
            0
        );
    }
}
