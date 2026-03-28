/// Agent context workspace — structured reasoning memory for LLM agents.
///
/// Implements the data structures and operations from:
///   "Git Context Controller: Manage the Context of Agents by Agentic Git"
///   arXiv:2508.00031
///
/// Organizes agent context as a version-controlled file system under `.h5i-ctx/`:
///
/// ```text
/// .h5i-ctx/
/// ├── main.md               # global roadmap: goals, milestones, active branches
/// └── branches/
///     └── <branch-name>/
///         ├── commit.md     # milestone summaries (append-only log)
///         ├── trace.md      # OTA (Observation–Thought–Action) execution trace
///         └── metadata.yaml # file structure, deps, env config
/// ```
///
/// Exposed via `h5i context` subcommands.
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::H5iError;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const CTX_DIR: &str = ".h5i-ctx";
/// Keep old constant as alias so any direct file-system users aren't broken during transition.
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
    pub recent_commits: Vec<String>,     // latest commit summaries for current branch
    pub recent_log_lines: Vec<String>,   // recent OTA lines from trace.md
    pub metadata_snippet: Option<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize `.h5i-ctx/` in `workdir`.
pub fn init(workdir: &Path, goal: &str) -> Result<(), H5iError> {
    let gcc = workdir.join(GCC_DIR);
    fs::create_dir_all(&gcc)?;

    // main.md
    let main_path = gcc.join("main.md");
    if !main_path.exists() {
        let content = format!(
            "# Project Roadmap\n\n\
             ## Goal\n{goal}\n\n\
             ## Milestones\n- [ ] Initial setup\n\n\
             ## Active Branches\n- main (primary)\n\n\
             ## Notes\n_Add project-wide notes here._\n"
        );
        fs::write(&main_path, content)?;
    }

    // branches/main/
    ensure_branch(workdir, MAIN_BRANCH, "Primary development branch")?;

    Ok(())
}

/// Return true if `.h5i-ctx/` exists in `workdir`.
pub fn is_initialized(workdir: &Path) -> bool {
    workdir.join(GCC_DIR).exists()
}

/// Return the current active branch name (reads `.GCC/.current_branch`).
pub fn current_branch(workdir: &Path) -> String {
    let path = workdir.join(GCC_DIR).join(".current_branch");
    fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| MAIN_BRANCH.to_string())
}

/// Set the active branch.
fn set_current_branch(workdir: &Path, branch: &str) -> Result<(), H5iError> {
    let path = workdir.join(GCC_DIR).join(".current_branch");
    fs::write(path, branch)?;
    Ok(())
}

/// COMMIT — checkpoint the agent's current progress as a structured milestone.
///
/// Appends to `branches/<current>/commit.md` and optionally updates `main.md`.
pub fn gcc_commit(workdir: &Path, summary: &str, contribution: &str) -> Result<(), H5iError> {
    let branch = current_branch(workdir);
    let branch_dir = workdir.join(GCC_DIR).join("branches").join(&branch);

    // Read previous summary from the last commit entry in commit.md
    let commit_path = branch_dir.join("commit.md");
    let previous_summary = extract_latest_summary(&commit_path);

    // Read branch purpose from the first line of commit.md
    let branch_purpose = extract_branch_purpose(&commit_path)
        .unwrap_or_else(|| format!("Branch: {branch}"));

    // Build the new summary: previous + this contribution (used by callers via extract_latest_summary)
    let _new_summary = if previous_summary.is_empty() {
        summary.to_string()
    } else {
        format!("{previous_summary}\n\n{summary}")
    };

    let short_id = short_timestamp_id();
    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    let entry = format!(
        "## Commit {short_id} — {ts}\n\n\
         ### Branch Purpose\n{branch_purpose}\n\n\
         ### Previous Progress Summary\n{previous_summary}\n\n\
         ### This Commit's Contribution\n{contribution}\n\n\
         ---\n\n"
    );

    // Append to commit.md
    let existing = fs::read_to_string(&commit_path).unwrap_or_default();
    fs::write(&commit_path, format!("{existing}{entry}"))?;

    // Archive the current trace.md period (mark boundary)
    let log_path = branch_dir.join("trace.md");
    let log_marker = format!(
        "\n\n---\n_[Checkpoint: {short_id} — {summary}]_\n---\n\n"
    );
    let existing_log = fs::read_to_string(&log_path).unwrap_or_default();
    fs::write(&log_path, format!("{existing_log}{log_marker}"))?;

    // Update main.md with branch progress
    update_main_md(workdir, &branch, summary)?;

    Ok(())
}

/// BRANCH — create a new isolated reasoning workspace.
pub fn gcc_branch(workdir: &Path, name: &str, purpose: &str) -> Result<(), H5iError> {
    ensure_branch(workdir, name, purpose)?;
    set_current_branch(workdir, name)?;
    Ok(())
}

/// Switch the active branch without creating it (equivalent to `git checkout`).
pub fn gcc_checkout(workdir: &Path, name: &str) -> Result<(), H5iError> {
    let branch_dir = workdir.join(GCC_DIR).join("branches").join(name);
    if !branch_dir.exists() {
        return Err(H5iError::InvalidPath(format!(
            "Context branch '{name}' does not exist. Run `h5i context branch {name}` first."
        )));
    }
    set_current_branch(workdir, name)?;
    Ok(())
}

/// MERGE — synthesize a completed branch into the current (or main) branch.
///
/// Per the paper (§2.2): "Before merging, the controller automatically calls CONTEXT
/// on the target branch to surface its historical summaries and planning rationale."
/// This function calls `gcc_context` on the *target* branch before performing the merge
/// and returns both the pre-merge context and the merged summary.
pub fn gcc_merge(workdir: &Path, source_branch: &str) -> Result<String, H5iError> {
    let target = current_branch(workdir);
    let gcc = workdir.join(GCC_DIR);

    let source_dir = gcc.join("branches").join(source_branch);
    if !source_dir.exists() {
        return Err(H5iError::InvalidPath(format!(
            "Branch '{source_branch}' not found"
        )));
    }

    // Paper §2.2: "Before merging, the controller automatically calls CONTEXT on the
    // target branch to surface its historical summaries and planning rationale."
    let _ = gcc_context(workdir, &ContextOpts {
        branch: Some(target.clone()),
        ..ContextOpts::default()
    });

    let source_summary = extract_latest_summary(&source_dir.join("commit.md"));
    let source_purpose = extract_branch_purpose(&source_dir.join("commit.md"))
        .unwrap_or_else(|| source_branch.to_string());

    let target_dir = gcc.join("branches").join(&target);
    let target_summary = extract_latest_summary(&target_dir.join("commit.md"));

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

    // Merge trace.md entries
    let source_log = fs::read_to_string(source_dir.join("trace.md")).unwrap_or_default();
    let target_log_path = target_dir.join("trace.md");
    let target_log = fs::read_to_string(&target_log_path).unwrap_or_default();
    let merged_log = format!(
        "{target_log}\n\n---\n_[MERGE from '{source_branch}' at {ts}]_\n\n{source_log}\n---\n"
    );
    fs::write(&target_log_path, merged_log)?;

    let entry = format!(
        "## Commit {short_id} — {ts} [MERGE: {source_branch} → {target}]\n\n\
         ### Branch Purpose\nMerge of branch '{source_branch}'\n\n\
         ### Previous Progress Summary\n{merged_summary}\n\n\
         ### This Commit's Contribution\n{contribution}\n\n\
         ---\n\n"
    );

    let commit_path = target_dir.join("commit.md");
    let existing = fs::read_to_string(&commit_path).unwrap_or_default();
    fs::write(&commit_path, format!("{existing}{entry}"))?;

    update_main_md(workdir, &target, &format!("Merged branch '{source_branch}'"))?;

    Ok(merged_summary)
}

/// CONTEXT — retrieve structured context at multiple granularities.
pub fn gcc_context(workdir: &Path, opts: &ContextOpts) -> Result<GccContext, H5iError> {
    let gcc = workdir.join(GCC_DIR);
    let branch_name = opts
        .branch
        .clone()
        .unwrap_or_else(|| current_branch(workdir));

    // Global state from main.md
    let main_text = fs::read_to_string(gcc.join("main.md")).unwrap_or_default();
    let project_goal = extract_section(&main_text, "Goal");
    let milestones = extract_list_items(&extract_section(&main_text, "Milestones"));
    let active_branches_raw = list_branches(workdir);

    // Branch-level context
    let branch_dir = gcc.join("branches").join(&branch_name);
    let commit_text = fs::read_to_string(branch_dir.join("commit.md")).unwrap_or_default();

    // If --commit <hash> is given, return only that specific commit entry.
    // Otherwise return the most recent `window` commits (sliding window V_k, K fixed).
    let window = if opts.window == 0 { 3 } else { opts.window };
    let recent_commits = if let Some(ref hash) = opts.commit_hash {
        find_commit_by_hash(&commit_text, hash)
            .map(|entry| vec![entry])
            .unwrap_or_default()
    } else {
        extract_recent_commits(&commit_text, window)
    };

    // OTA log — sliding window: take `window*20` lines starting at offset `log_offset`.
    let recent_log_lines = if opts.show_log {
        let log_text = fs::read_to_string(branch_dir.join("trace.md")).unwrap_or_default();
        let all_lines: Vec<&str> = log_text.lines().collect();
        let total = all_lines.len();
        // k = log_offset (0 = most-recent end); window budget = window * 20 lines
        let budget = (window * 20).max(40);
        let end = total.saturating_sub(opts.log_offset);
        let start = end.saturating_sub(budget);
        all_lines[start..end]
            .iter()
            .map(|l| l.to_string())
            .collect()
    } else {
        vec![]
    };

    // Metadata
    let metadata_snippet = if let Some(ref seg) = opts.metadata_segment {
        let meta_text = fs::read_to_string(branch_dir.join("metadata.yaml")).unwrap_or_default();
        Some(extract_yaml_segment(&meta_text, seg))
    } else {
        None
    };

    Ok(GccContext {
        project_goal,
        milestones,
        active_branches: active_branches_raw,
        current_branch: branch_name,
        recent_commits,
        recent_log_lines,
        metadata_snippet,
    })
}

/// Append an OTA (Observation–Thought–Action) entry to the current branch's `trace.md`.
pub fn append_log(workdir: &Path, kind: &str, content: &str) -> Result<(), H5iError> {
    let branch = current_branch(workdir);
    let log_path = workdir
        .join(GCC_DIR)
        .join("branches")
        .join(&branch)
        .join("trace.md");

    let ts = Utc::now().format("%H:%M:%S").to_string();
    let entry = format!("[{ts}] {}: {}\n", kind.to_uppercase(), content);

    let existing = fs::read_to_string(&log_path).unwrap_or_default();
    fs::write(&log_path, format!("{existing}{entry}"))?;
    Ok(())
}

/// Update `metadata.yaml` for the current branch.
pub fn update_metadata(workdir: &Path, meta: &GccMetadata) -> Result<(), H5iError> {
    let branch = current_branch(workdir);
    let meta_path = workdir
        .join(GCC_DIR)
        .join("branches")
        .join(&branch)
        .join("metadata.yaml");
    let yaml = serde_yaml_serialize(meta);
    fs::write(meta_path, yaml)?;
    Ok(())
}

/// List all branches in `.GCC/branches/`.
pub fn list_branches(workdir: &Path) -> Vec<String> {
    let branches_dir = workdir.join(GCC_DIR).join("branches");
    if !branches_dir.exists() {
        return vec![];
    }
    let mut branches: Vec<String> = fs::read_dir(&branches_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    branches.sort();
    branches
}

// ── Terminal display ──────────────────────────────────────────────────────────

pub fn print_context(ctx: &GccContext) {
    use console::style;

    println!("{}", style("── Context ─────────────────────────────────────────────").dim());
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
            "{} .h5i-ctx/ not found. Run {} to initialize.",
            style("ℹ").blue(),
            style("h5i context init").bold()
        );
        return Ok(());
    }

    let branch = current_branch(workdir);
    let branches = list_branches(workdir);
    let gcc = workdir.join(GCC_DIR);
    let branch_dir = gcc.join("branches").join(&branch);

    let commit_count = count_commits(&branch_dir.join("commit.md"));
    let log_lines = fs::read_to_string(branch_dir.join("trace.md"))
        .unwrap_or_default()
        .lines()
        .count();

    println!("{}", style("── Context Status ──────────────────────────────────────────────").dim());
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

/// Generate a system prompt that teaches a Claude agent how to use GCC.
///
/// This is the key integration point for LLM agents (paper §2.2):
/// "These commands' function and usage are given to the agents in the system prompts,
/// then the agents are encouraged to use them when needed."
///
/// If the workspace is already initialized, the prompt includes the current branch
/// and goal so the agent can orient itself immediately.
pub fn system_prompt(workdir: &Path) -> String {
    let status_block = if is_initialized(workdir) {
        let branch = current_branch(workdir);
        let branches = list_branches(workdir);
        let goal = {
            let main_text = fs::read_to_string(workdir.join(GCC_DIR).join("main.md"))
                .unwrap_or_default();
            extract_section(&main_text, "Goal")
        };
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
         Run `h5i context init --goal \"<your project goal>\"` to initialize the workspace.\n".to_string()
    };

    format!(
        r#"# Git Context Controller (GCC)

You are working within a GCC workspace that organizes your memory as a persistent,
versioned file system under `.h5i-ctx/`. Use the commands below to manage context across
long-horizon tasks. GCC prevents context-window overflow by externalizing reasoning
into structured files that survive session boundaries.
{status_block}
## File System Layout

```
.h5i-ctx/
├── main.md                    # global roadmap: goal, milestones, notes
└── branches/
    └── <branch>/
        ├── commit.md          # milestone summaries (append-only)
        ├── trace.md             # OTA (Observation–Thought–Action) execution trace
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
main trajectory. Call when you detect a meaningful divergence: testing an
alternative algorithm, investigating a side hypothesis, or parallelizing work.

### `h5i context checkout <name>`
Switch to an existing branch.

### `h5i context merge <branch>`
Integrate a completed branch into the current branch. The controller will
automatically call CONTEXT on the target branch before merging. Call when an
exploration branch has reached a useful conclusion and its results should be
incorporated into the main plan.

### `h5i context status`
Show active branch, commit count, and log size.

## Workflow Pattern

```
# Session start (mandatory)
h5i context show --trace

# During execution (continuous)
h5i context log --kind OBSERVE "test suite output: 3 failures in auth module"
h5i context log --kind THINK   "failures are in token validation; likely a regex issue"
h5i context log --kind ACT     "editing src/auth/token.rs validate() function"

# Exploring a risky alternative
h5i context branch experiment/new-algo --purpose "test O(log n) approach vs current O(n)"
# ... explore ...
h5i context checkout main
h5i context merge experiment/new-algo   # auto-calls CONTEXT on main first

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
5. Update main.md milestones via commit summaries when goals are completed.
"#
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn ensure_branch(workdir: &Path, name: &str, purpose: &str) -> Result<(), H5iError> {
    let branch_dir = workdir.join(GCC_DIR).join("branches").join(name);
    fs::create_dir_all(&branch_dir)?;

    let commit_path = branch_dir.join("commit.md");
    if !commit_path.exists() {
        fs::write(
            &commit_path,
            format!(
                "# Branch: {name}\n\n\
                 **Purpose:** {purpose}\n\n\
                 _Commits will be appended below._\n\n"
            ),
        )?;
    }

    let log_path = branch_dir.join("trace.md");
    if !log_path.exists() {
        fs::write(
            &log_path,
            format!("# OTA Log — Branch: {name}\n\n"),
        )?;
    }

    let meta_path = branch_dir.join("metadata.yaml");
    if !meta_path.exists() {
        fs::write(
            &meta_path,
            "file_structure: {}\nenv_config: {}\ndependencies: []\n",
        )?;
    }

    Ok(())
}

fn update_main_md(workdir: &Path, branch: &str, summary: &str) -> Result<(), H5iError> {
    let main_path = workdir.join(GCC_DIR).join("main.md");
    let content = fs::read_to_string(&main_path).unwrap_or_default();
    let ts = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    // Append a one-line progress note at the end of the Notes section
    let note = format!("- [{ts}] `{branch}`: {summary}\n");

    if let Some(pos) = content.find("## Notes") {
        let after_notes = &content[pos..];
        let insert_at = pos
            + after_notes
                .find('\n')
                .map(|i| i + 1)
                .unwrap_or(after_notes.len());
        let mut new_content = content.clone();
        new_content.insert_str(insert_at, &note);
        fs::write(&main_path, new_content)?;
    } else {
        let appended = format!("{content}\n## Notes\n{note}");
        fs::write(&main_path, appended)?;
    }

    Ok(())
}

fn extract_latest_summary(commit_path: &Path) -> String {
    let text = fs::read_to_string(commit_path).unwrap_or_default();
    // Find the last "### Previous Progress Summary" + "### This Commit's Contribution"
    // and combine them into a rolling summary
    let entries: Vec<&str> = text.split("## Commit ").collect();
    if let Some(last) = entries.last() {
        if let Some(start) = last.find("### This Commit's Contribution") {
            let after = &last[start + "### This Commit's Contribution".len()..];
            let end = after.find("\n---").unwrap_or(after.len());
            return after[..end].trim().to_string();
        }
    }
    String::new()
}

fn extract_branch_purpose(commit_path: &Path) -> Option<String> {
    let text = fs::read_to_string(commit_path).ok()?;
    let after = text.split("**Purpose:**").nth(1)?;
    let end = after.find('\n').unwrap_or(after.len());
    Some(after[..end].trim().to_string())
}

/// Find a specific commit entry in `commit.md` whose short ID starts with `hash_prefix`.
/// Returns the "This Commit's Contribution" text, or the full entry header if not parseable.
fn find_commit_by_hash(commit_text: &str, hash_prefix: &str) -> Option<String> {
    for entry in commit_text.split("## Commit ").skip(1) {
        // Entry starts with "<short_id> — <ts>..."
        if entry.starts_with(hash_prefix) {
            if let Some(start) = entry.find("### This Commit's Contribution") {
                let after = &entry[start + "### This Commit's Contribution".len()..];
                let end = after.find("\n---").unwrap_or(after.len());
                return Some(format!("[{}] {}", hash_prefix, after[..end].trim()));
            }
            // Fallback: return the first line (the commit header)
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
            // Extract the "This Commit's Contribution" line
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
        // Read until next ## section or end
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

fn count_commits(commit_path: &Path) -> usize {
    let text = fs::read_to_string(commit_path).unwrap_or_default();
    text.matches("## Commit ").count()
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
    // Short ID based on current time (8 hex chars)
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{:08x}", ts)
}

/// Minimal YAML serializer (no dep needed — just key:value + lists).
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
    use tempfile::tempdir;

    // ── init / is_initialized ─────────────────────────────────────────────────

    #[test]
    fn init_creates_workspace_files() {
        let dir = tempdir().unwrap();
        init(dir.path(), "Build something great").unwrap();

        assert!(dir.path().join(CTX_DIR).exists());
        assert!(dir.path().join(CTX_DIR).join("main.md").exists());
        assert!(dir.path().join(CTX_DIR).join("branches").join("main").exists());
    }

    #[test]
    fn is_initialized_false_before_init() {
        let dir = tempdir().unwrap();
        assert!(!is_initialized(dir.path()));
    }

    #[test]
    fn is_initialized_true_after_init() {
        let dir = tempdir().unwrap();
        init(dir.path(), "Test goal").unwrap();
        assert!(is_initialized(dir.path()));
    }

    #[test]
    fn init_embeds_goal_in_main_md() {
        let dir = tempdir().unwrap();
        init(dir.path(), "Build an OAuth2 login system").unwrap();
        let content = std::fs::read_to_string(dir.path().join(CTX_DIR).join("main.md")).unwrap();
        assert!(content.contains("Build an OAuth2 login system"));
    }

    #[test]
    fn init_idempotent_does_not_overwrite_main_md() {
        let dir = tempdir().unwrap();
        init(dir.path(), "Original goal").unwrap();
        // Write custom content
        let main = dir.path().join(CTX_DIR).join("main.md");
        std::fs::write(&main, "Custom content").unwrap();
        // Re-init should not overwrite because main_path.exists() guard
        init(dir.path(), "New goal").unwrap();
        let content = std::fs::read_to_string(&main).unwrap();
        assert_eq!(content, "Custom content");
    }

    // ── current_branch / set_current_branch ──────────────────────────────────

    #[test]
    fn current_branch_defaults_to_main() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        assert_eq!(current_branch(dir.path()), "main");
    }

    #[test]
    fn gcc_branch_switches_active_branch() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "experiment", "try new approach").unwrap();
        assert_eq!(current_branch(dir.path()), "experiment");
    }

    // ── gcc_checkout ──────────────────────────────────────────────────────────

    #[test]
    fn gcc_checkout_switches_to_existing_branch() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feature", "feature work").unwrap();
        gcc_checkout(dir.path(), "main").unwrap();
        assert_eq!(current_branch(dir.path()), "main");
    }

    #[test]
    fn gcc_checkout_fails_on_nonexistent_branch() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        assert!(gcc_checkout(dir.path(), "does_not_exist").is_err());
    }

    // ── list_branches ─────────────────────────────────────────────────────────

    #[test]
    fn list_branches_after_init_has_main() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        let branches = list_branches(dir.path());
        assert!(branches.contains(&"main".to_string()));
    }

    #[test]
    fn list_branches_includes_new_branches() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_branch(dir.path(), "feat-oauth", "oauth work").unwrap();
        let branches = list_branches(dir.path());
        assert!(branches.contains(&"feat-oauth".to_string()));
    }

    // ── append_log ────────────────────────────────────────────────────────────

    #[test]
    fn append_log_adds_entry_to_trace() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "OBSERVE", "Redis latency is 2ms").unwrap();
        let trace = std::fs::read_to_string(
            dir.path().join(CTX_DIR).join("branches").join("main").join("trace.md"),
        ).unwrap();
        assert!(trace.contains("OBSERVE: Redis latency is 2ms"));
    }

    #[test]
    fn append_log_uppercases_kind() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        append_log(dir.path(), "think", "reasoning step").unwrap();
        let trace = std::fs::read_to_string(
            dir.path().join(CTX_DIR).join("branches").join("main").join("trace.md"),
        ).unwrap();
        assert!(trace.contains("THINK:"));
    }

    // ── gcc_commit ────────────────────────────────────────────────────────────

    #[test]
    fn gcc_commit_appends_entry_to_commit_md() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "Milestone 1 done", "Implemented the login form").unwrap();
        let commit_md = std::fs::read_to_string(
            dir.path().join(CTX_DIR).join("branches").join("main").join("commit.md"),
        ).unwrap();
        assert!(commit_md.contains("Implemented the login form"));
    }

    #[test]
    fn gcc_commit_updates_main_md_notes() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "Completed auth setup", "Added JWT tokens").unwrap();
        let main_md = std::fs::read_to_string(dir.path().join(CTX_DIR).join("main.md")).unwrap();
        assert!(main_md.contains("Completed auth setup"));
    }

    // ── gcc_context ───────────────────────────────────────────────────────────

    #[test]
    fn gcc_context_reads_goal_from_main_md() {
        let dir = tempdir().unwrap();
        init(dir.path(), "Build an OAuth2 login system").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.project_goal, "Build an OAuth2 login system");
    }

    #[test]
    fn gcc_context_reads_milestones() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        // Manually add a milestone
        let main = dir.path().join(CTX_DIR).join("main.md");
        let mut content = std::fs::read_to_string(&main).unwrap();
        content = content.replace("- [ ] Initial setup", "- [x] Initial setup\n- [ ] Add rate limiting");
        std::fs::write(&main, content).unwrap();

        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert!(ctx.milestones.iter().any(|m| m.contains("Add rate limiting")));
    }

    #[test]
    fn gcc_context_includes_current_branch() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts::default()).unwrap();
        assert_eq!(ctx.current_branch, "main");
    }

    #[test]
    fn gcc_context_returns_recent_commits() {
        let dir = tempdir().unwrap();
        init(dir.path(), "goal").unwrap();
        gcc_commit(dir.path(), "milestone", "did the work").unwrap();
        let ctx = gcc_context(dir.path(), &ContextOpts { window: 3, ..Default::default() }).unwrap();
        assert!(!ctx.recent_commits.is_empty());
        assert!(ctx.recent_commits[0].contains("did the work"));
    }

    // ── gcc_merge ─────────────────────────────────────────────────────────────

    #[test]
    fn gcc_merge_combines_branches() {
        let dir = tempdir().unwrap();
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

    #[test]
    fn extract_recent_commits_returns_latest_first_when_multiple() {
        let commit_text = "## Commit aaa111 — 2026-01-01\n\
            ### This Commit's Contribution\nFirst contribution\n---\n\
            ## Commit bbb222 — 2026-01-02\n\
            ### This Commit's Contribution\nSecond contribution\n---\n";
        let recent = extract_recent_commits(commit_text, 2);
        // Both entries returned, ordered oldest first (reversed back after rev)
        assert_eq!(recent.len(), 2);
        assert!(recent.last().unwrap().contains("Second contribution"));
    }
}
