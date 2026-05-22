use clap::{Parser, Subcommand, ValueEnum};
use console::style;
use git2::Oid;
use std::path::{Path, PathBuf};

use h5i_core::blame::BlameMode;
use h5i_core::claims;
use h5i_core::claude::{keyword_search, AnthropicClient};
use h5i_core::codex;
use h5i_core::ctx;
use h5i_core::memory;
use h5i_core::metadata::{AiMetadata, Decision, IntegrityLevel, Severity, TestSource};
use h5i_core::session_log;
use h5i_core::storage::{self, DoctorSeverity};
use h5i_core::repository::H5iRepository;
use h5i_core::review::REVIEW_THRESHOLD;
use h5i_core::ui::{ERROR, LOOKING, STEP, SUCCESS, WARN};

/// Truncate a string to at most `max_chars` characters, appending `…` if cut.
fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let mut result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[derive(Parser)]
#[command(name = "h5i", about = "Advanced Git for the AI Era", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum AgentRuntime {
    Claude,
    Codex,
}

impl AgentRuntime {
    fn to_memory_agent(self) -> memory::MemoryAgent {
        match self {
            Self::Claude => memory::MemoryAgent::Claude,
            Self::Codex => memory::MemoryAgent::Codex,
        }
    }
}

fn resolve_memory_agent(agent: Option<AgentRuntime>) -> memory::MemoryAgent {
    match agent {
        Some(agent) => agent.to_memory_agent(),
        None => memory::MemoryAgent::from_env(),
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize the h5i sidecar in the current repository
    Init,

    /// Record provenance — commit, claim, memory snapshot.
    /// Run `h5i capture --help` for the verb table with runnable examples.
    Capture {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },

    /// Read AI history — log, blame, diff, context, claims, notes, memory, recap, resume, vibe.
    /// Run `h5i recall --help` for the verb table.
    Recall {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },

    /// Assess risk — review-rank, prompt-injection scan, compliance, policy, vibe.
    /// Run `h5i audit --help` for the verb table.
    Audit {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },

    /// Publish — push, pull, and post a sticky GitHub PR comment with AI provenance.
    /// Run `h5i share --help` for the verb table.
    Share {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },

    /// Commit staged changes with AI provenance and quality tracking
    #[command(hide = true)]
    Commit {
        /// Standard Git commit message
        #[arg(short, long)]
        message: String,

        // Prompt
        #[arg(long)]
        prompt: Option<String>,

        /// The name of the AI model that assisted in these changes
        #[arg(long)]
        model: Option<String>,

        /// The unique ID of the AI agent
        #[arg(long)]
        agent: Option<String>,

        /// Run the test suite and capture metrics.
        /// If the `H5I_TEST_CMD` environment variable is set, that command is executed
        /// and its output is parsed for test results (pass/fail counts, duration, etc.).
        /// Falls back to scanning staged source files for `// h5_i_test_start` /
        /// `// h5_i_test_end` markers when no command is configured.
        #[arg(long)]
        tests: bool,

        /// Path to a JSON file produced by a test adapter (any tool, any language).
        /// Takes precedence over --tests and H5I_TEST_RESULTS.
        /// Schema: { "tool", "passed", "failed", "skipped", "total",
        ///           "duration_secs", "coverage", "exit_code", "summary" }
        #[arg(long, value_name = "FILE")]
        test_results: Option<std::path::PathBuf>,

        /// Shell command to run as the test suite.
        /// h5i captures its exit code and tries to parse stdout as h5i JSON.
        /// Used when no --test-results file is provided.
        #[arg(long, value_name = "CMD")]
        test_cmd: Option<String>,

        /// Enable AST-based structural tracking for the commit
        #[arg(long)]
        ast: bool,

        #[arg(long)]
        audit: bool,

        #[arg(long)]
        force: bool,

        /// OID(s) of commits that causally triggered this one.
        /// Can be specified multiple times: --caused-by abc123 --caused-by def456
        #[arg(long, value_name = "OID", action = clap::ArgAction::Append)]
        caused_by: Option<Vec<String>>,

        /// Path to a JSON file containing structured design decisions for this commit.
        /// Schema: array of { "location", "choice", "alternatives"?, "reason" }
        /// Example: [{"location":"src/model.py:42","choice":"use Adam optimizer",
        ///            "alternatives":["SGD","RMSProp"],"reason":"faster convergence on this dataset"}]
        #[arg(long, value_name = "FILE")]
        decisions: Option<std::path::PathBuf>,

        /// Stage these paths before committing (equivalent to `git add <path>` beforehand).
        /// Can be specified multiple times: --add src/foo.rs --add src/bar.rs
        #[arg(long, value_name = "PATH", action = clap::ArgAction::Append)]
        add: Option<Vec<std::path::PathBuf>>,
    },

    /// Display the enriched 5D commit history
    #[command(hide = true)]
    Log {
        /// Number of recent commits to display
        #[arg(short, long, default_value_t = 10)]
        limit: usize,

        /// Show the full prompt ancestry chain for a specific line.
        /// Format: <file>:<line>  e.g.  src/model.py:42
        /// Prints every commit that ever touched that line, annotated with the
        /// human prompt that caused each change.
        #[arg(long, value_name = "FILE:LINE")]
        ancestry: Option<String>,
    },

    /// Analyze file ownership with optional structural (AST) logic
    #[command(hide = true)]
    Blame {
        /// Path to the file to inspect
        file: PathBuf,

        /// Mode of blame: 'line' (standard) or 'ast' (semantic)
        #[arg(short, long, default_value = "line")]
        mode: String,

        /// Annotate each commit boundary with the human prompt that triggered it.
        /// The prompt is printed once per unique commit, immediately after the
        /// last line belonging to that commit.
        #[arg(long)]
        show_prompt: bool,
    },

    /// Resolve branch conflicts using CRDT-based semantic merging
    Resolve {
        /// OID of the local branch (OURS)
        ours: String,
        /// OID of the incoming branch (THEIRS)
        theirs: String,
        /// Relative path to the file to resolve
        file: String,
    },

    /// Show the AST-level structural diff for a file
    Diff {
        /// Path to the file to analyse (must be a supported language, e.g. .py)
        file: PathBuf,

        /// Compare from this commit OID (default: HEAD)
        #[arg(long)]
        from: Option<String>,

        /// Compare to this commit OID (default: working-tree file)
        #[arg(long)]
        to: Option<String>,
    },

    /// Revert the AI-generated commit whose intent best matches a description
    Rollback {
        /// Natural-language description of the change to undo (e.g. "OAuth login")
        intent: String,

        /// Number of recent commits to search
        #[arg(short, long, default_value_t = 50)]
        limit: usize,

        /// Show the matched commit without actually reverting
        #[arg(long)]
        dry_run: bool,

        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Restore the working tree to the exact state of any past commit.
    ///
    /// Unlike `rollback` (which creates a revert commit), `rewind` directly
    /// overwrites files in your working tree — HEAD stays where it is, so
    /// `git status` shows the full diff and you can review before committing.
    ///
    /// Current dirty state is saved to `refs/h5i/shadow/<timestamp>` before
    /// any files are touched, so recovery is always possible.
    Rewind {
        /// Git commit SHA to restore (full or short). Also accepts HEAD, HEAD~1, etc.
        sha: String,

        /// Show what would change without touching the working tree.
        #[arg(long)]
        dry_run: bool,

        /// Skip saving the current dirty state to a shadow ref before rewinding.
        #[arg(long)]
        force: bool,
    },

    /// Launch the h5i web dashboard in your browser
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value_t = 7150)]
        port: u16,
    },

    /// Push all h5i refs (notes + memory) to a remote in one shot
    #[command(hide = true)]
    Push {
        /// Remote to push to
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },

    /// Fetch all h5i refs (notes + memory + context + ast) from a remote in one shot.
    ///
    /// By default, divergent local refs are KEPT — fast-forwards apply silently,
    /// `refs/h5i/notes` is auto-merged via `git notes merge -s union`, and other
    /// chain-style refs (memory / context / ast) are left alone with a warning
    /// when they have diverged. Pass `--force` to overwrite those local refs.
    #[command(hide = true)]
    Pull {
        /// Remote to pull from
        #[arg(short, long, default_value = "origin")]
        remote: String,

        /// Overwrite local refs that have diverged from the remote.
        /// Has no effect on refs/h5i/notes (always merged with strategy=union).
        #[arg(short, long)]
        force: bool,
    },

    /// Manage Claude Code hooks for automatic prompt capture and context tracing.
    /// Run `h5i hook setup` to print install instructions.
    /// Run `h5i hook run` (or just `h5i hook`) as the PostToolUse handler in .claude/settings.json.
    #[command(subcommand)]
    Hook(HookCommands),

    /// Codex integration helpers for context restore, trace sync, and closeout
    Codex {
        #[command(subcommand)]
        action: CodexCommands,
    },

    /// Version-control agent memory state alongside your code
    #[command(hide = true)]
    Memory {
        #[command(subcommand)]
        action: MemoryCommands,
    },

    /// Record and query content-addressed claims about the codebase.
    /// Each claim pins (path, blob_oid) evidence at HEAD; it stays "live"
    /// until any evidence blob changes, then auto-invalidates.
    #[command(hide = true)]
    Claims {
        #[command(subcommand)]
        action: ClaimsCommands,
    },

    /// Inspect AI session activity: footprint, uncertainty, churn, and intent graph
    /// (analogous to `git notes` — structured annotations attached to commits)
    #[command(hide = true)]
    Notes {
        #[command(subcommand)]
        action: NotesCommands,
    },

    /// Manage the agent reasoning workspace across sessions
    /// (git-style branching/committing applied to `.h5i-ctx/`, arXiv:2508.00031)
    #[command(hide = true)]
    Context {
        #[command(subcommand)]
        action: ContextCommands,
    },

    /// Generate a structured handoff briefing to resume an AI session
    Resume {
        /// Branch to resume (defaults to current branch)
        branch: Option<String>,
    },

    /// Start the h5i MCP (Model Context Protocol) server on stdio
    ///
    /// Exposes h5i tools and resources to any MCP client (e.g. Claude Code).
    /// Add to your Claude Code config:
    ///
    ///   "h5i": { "command": "h5i", "args": ["mcp"] }
    Mcp,

    /// Validate and repair h5i sidecar storage and refs
    Doctor {
        /// Create missing sidecar directories and schema metadata
        #[arg(long)]
        repair: bool,

        /// Export a recovery copy of .git/.h5i plus a refs manifest into this directory
        #[arg(long, value_name = "DIR")]
        export: Option<PathBuf>,

        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },

    /// Show an instant AI footprint audit: how much of this repo is AI-generated,
    /// which directories are fully AI-written, and where the riskiest files are
    #[command(hide = true)]
    Vibe {
        /// Number of recent commits to scan
        #[arg(short, long, default_value_t = 500)]
        limit: usize,

        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },

    /// Manage governance policy for AI-assisted commits (.h5i/policy.toml)
    Policy {
        #[command(subcommand)]
        action: PolicyCommands,
    },

    /// Generate a compliance audit report over a date range
    #[command(hide = true)]
    Compliance {
        /// Start of date range (inclusive), format: YYYY-MM-DD
        #[arg(long)]
        since: Option<String>,

        /// End of date range (inclusive), format: YYYY-MM-DD
        #[arg(long)]
        until: Option<String>,

        /// Output format: text, json, or html
        #[arg(long, default_value = "text")]
        format: String,

        /// Write output to this file (default: stdout)
        #[arg(long, value_name = "FILE")]
        output: Option<std::path::PathBuf>,

        /// Maximum number of commits to scan
        #[arg(short, long, default_value_t = 500)]
        limit: usize,
    },

    /// Post or preview a GitHub pull-request comment with h5i provenance
    /// for every commit on the current branch vs. the PR's base branch.
    #[command(hide = true)]
    Pr {
        #[command(subcommand)]
        action: PrCommands,
    },
}

#[derive(Subcommand)]
enum PrCommands {
    /// Post (or upsert) a sticky comment on the current branch's open PR.
    /// Uses `gh` CLI under the hood.
    Post {
        /// PR number (default: auto-detect from current branch)
        #[arg(long, value_name = "N")]
        number: Option<u64>,

        /// Limit number of commits included
        #[arg(short, long, default_value_t = 25)]
        limit: usize,

        /// Hero block layout: `receipt` (default — scannable summary block),
        /// `detective` (narrative: goal → considered → key insight → shipped),
        /// or `replay` (DAG-as-hero with milestone markers).
        #[arg(long, value_enum, default_value_t = PrStyleArg::Receipt)]
        style: PrStyleArg,

        /// Print the markdown body and exit without calling `gh`
        #[arg(long)]
        dry_run: bool,
    },

    /// Print the PR comment markdown to stdout (for piping into `gh pr edit --body-file -`)
    Body {
        /// Limit number of commits included
        #[arg(short, long, default_value_t = 25)]
        limit: usize,

        /// Hero block layout — see `h5i share pr post --help` for options.
        #[arg(long, value_enum, default_value_t = PrStyleArg::Receipt)]
        style: PrStyleArg,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum PrStyleArg {
    Receipt,
    Detective,
    Replay,
}

impl From<PrStyleArg> for h5i_core::pr::PrStyle {
    fn from(s: PrStyleArg) -> Self {
        match s {
            PrStyleArg::Receipt => h5i_core::pr::PrStyle::Receipt,
            PrStyleArg::Detective => h5i_core::pr::PrStyle::Detective,
            PrStyleArg::Replay => h5i_core::pr::PrStyle::Replay,
        }
    }
}

#[derive(Subcommand)]
enum NotesCommands {
    /// Parse a Claude Code session log and store enriched metadata linked to a commit
    /// (footprint, causal chain, uncertainty, file churn)
    Analyze {
        /// Path to the Claude Code .jsonl session file (default: auto-detect latest session)
        #[arg(long, value_name = "JSONL")]
        session: Option<PathBuf>,
        /// Commit OID to link this analysis to (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Only include session events that occurred *after* this commit was made.
        /// Useful when a single Claude Code session spans multiple h5i commits:
        ///   h5i notes analyze --since <first-commit-sha>
        /// links only the work done *after* that commit to HEAD.
        #[arg(long, value_name = "OID")]
        since: Option<String>,
    },

    /// Show which files the AI consulted vs edited for a given commit
    Show {
        /// Commit OID whose session analysis to display (default: HEAD)
        commit: Option<String>,
    },

    /// Show moments where the AI expressed uncertainty, optionally filtered by file
    Uncertainty {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Filter to annotations recorded while editing this file
        #[arg(long)]
        file: Option<String>,
    },

    /// Show file edit-churn across all analyzed sessions
    Churn {
        /// Number of files to show
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Visualise the chain of intents associated with recent commits
    Graph {
        /// Number of recent commits to include
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Intent source: 'prompt' uses the stored AI prompt; 'analyze' calls Claude
        #[arg(long, default_value = "prompt")]
        mode: String,
    },

    /// Identify commits most likely to benefit from human review
    Review {
        /// Number of recent commits to scan
        #[arg(short, long, default_value_t = 100)]
        limit: usize,
        /// Minimum score threshold (0.0–1.0) for a commit to be flagged
        #[arg(long, default_value_t = REVIEW_THRESHOLD)]
        min_score: f32,
        /// Output raw JSON instead of the styled table
        #[arg(long)]
        json: bool,
    },

    /// Show where Claude deferred, left placeholders, or made promises it didn't keep
    Omissions {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Filter to annotations recorded while editing this file
        #[arg(long)]
        file: Option<String>,
    },

    /// Show per-file attention coverage: which files were read before being edited.
    /// Files with a low read-before-edit ratio are likely blind edits — higher risk.
    Coverage {
        /// Commit OID whose session analysis to display (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Only show files with read_before_edit_ratio below this threshold (0.0–1.0)
        #[arg(long, default_value_t = 1.01)]
        max_ratio: f32,
    },
}

#[derive(Subcommand)]
enum ContextCommands {
    /// Initialize the `.h5i-ctx/` reasoning workspace for this project
    Init {
        /// High-level project goal written to main.md
        #[arg(long, default_value = "")]
        goal: String,
    },

    /// Checkpoint the agent's current progress as a structured milestone
    /// (like `git commit` but for the reasoning workspace)
    Commit {
        /// One-line summary of what was accomplished
        summary: String,
        /// Detailed description of this commit's contribution
        #[arg(long, default_value = "")]
        detail: String,
    },

    /// Create a new isolated reasoning branch for exploring an alternative
    /// (like `git branch` but for the `.h5i-ctx/` workspace)
    Branch {
        /// Branch name (e.g. "experiment/cache-strategy")
        name: String,
        /// Why this branch exists / what hypothesis it explores
        #[arg(long, default_value = "")]
        purpose: String,
    },

    /// Switch to an existing reasoning branch
    /// (like `git checkout` but for the `.h5i-ctx/` workspace)
    Checkout {
        /// Branch name to switch to
        name: String,
    },

    /// Merge a completed reasoning branch into the current branch
    /// (like `git merge` but for the `.h5i-ctx/` workspace)
    Merge {
        /// Name of the branch to merge in
        branch: String,
    },

    /// Retrieve the current project state at multiple levels of detail
    /// (like `git show` — global roadmap, recent commits, optional trace)
    ///
    /// Three depths inspired by progressive-disclosure retrieval:
    ///   --depth 1  compact index (~800 tokens): goal, branch, milestone IDs, counts
    ///   --depth 2  timeline (default, ~2-5K tokens): adds recent commits + mini-trace
    ///   --depth 3  full trace: adds the complete OTA log
    Show {
        /// Show context for this branch (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// Return the complete record for a specific commit hash
        #[arg(long)]
        commit: Option<String>,
        /// Include recent OTA execution trace from trace.md (equivalent to --depth 3)
        #[arg(long)]
        trace: bool,
        /// Retrieve a specific metadata segment from metadata.yaml (e.g. "file_structure")
        #[arg(long)]
        metadata: Option<String>,
        /// Number of recent commits to show (context window K)
        #[arg(long, default_value_t = 3)]
        window: usize,
        /// Scroll back N lines in the trace (sliding-window offset k)
        #[arg(long, default_value_t = 0)]
        trace_offset: usize,
        /// Progressive disclosure depth: 1=compact index, 2=timeline (default), 3=full trace
        #[arg(long, default_value_t = 2)]
        depth: u8,
    },

    /// Append an OTA (Observation–Thought–Action) step to the current branch trace
    Trace {
        /// Step type: OBSERVE, THINK, ACT, or NOTE
        #[arg(long, default_value = "NOTE")]
        kind: String,
        /// Trace entry content
        content: String,
        /// Mark this entry as ephemeral (scratch-only, cleared on next context commit,
        /// not persisted to the DAG or snapshots — like Claude Code's /btw)
        #[arg(long)]
        ephemeral: bool,
    },

    /// Show the current reasoning workspace state (branch, commit count, trace size)
    Status,

    /// Print a system prompt for injecting h5i context commands into an agent session
    Prompt,

    /// Scan the reasoning trace for prompt-injection patterns and report a risk score
    Scan {
        /// Branch to scan (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },

    /// Restore the context workspace to the state captured at a given git commit
    Restore {
        /// Git commit SHA whose context snapshot to restore (prefix OK)
        sha: String,
    },

    /// Show how the context workspace evolved between two git commits
    Diff {
        /// Earlier git commit SHA (prefix OK)
        from: String,
        /// Later git commit SHA (prefix OK)
        to: String,
    },

    /// Show context workspace entries relevant to a specific file
    Relevant {
        /// File path to look up (e.g. src/repository.rs)
        file: String,
    },

    /// Compact old context history using three-pass structurally-lossless trimming.
    /// Pass 1: remove OBSERVE entries subsumed by a later THINK/ACT on the same topic.
    /// Pass 2: keep all THINK, ACT, NOTE entries verbatim.
    /// Pass 3: merge consecutive OBSERVE entries mentioning the same file.
    Pack,

    /// Create a subagent-scoped sub-context for isolated delegation.
    /// Scoped branches are prefixed `scope/` and shown separately in `status`.
    /// Merge them back with `h5i context merge scope/<name>` when the subagent finishes.
    Scope {
        /// Sub-context name (will be stored as `scope/<name>`)
        name: String,
        /// Why this scope exists / what the subagent is investigating
        #[arg(long, default_value = "")]
        purpose: String,
    },

    /// Show the ephemeral scratch traces for the current branch (cleared on context commit)
    Ephemeral {
        /// Branch to inspect (default: current)
        #[arg(long)]
        branch: Option<String>,
    },

    /// Show the stable-prefix / dynamic-suffix boundary for the current trace
    /// (useful for understanding prompt-caching efficiency)
    CachedPrefix {
        /// Number of dynamic (volatile) tail lines to exclude from stable prefix
        #[arg(long, default_value_t = 40)]
        tail: usize,
    },

    /// Show all open TODO / FIXME / BLOCKED items extracted from the trace.
    /// These are NOTE and THINK entries that contain actionable keywords.
    Todo,

    /// Distill all THINK entries across every context branch into a project knowledge base.
    /// Useful for reviewing every design decision ever recorded in this workspace.
    Knowledge,

    /// Render the per-branch trace DAG as a coloured graph in the terminal.
    /// Each node shows its kind (OBSERVE/THINK/ACT/NOTE/MERGE), 8-hex ID,
    /// timestamp, and content. Merge nodes display both parent IDs.
    Dag {
        /// Branch whose DAG to display (default: current branch)
        #[arg(long)]
        branch: Option<String>,
    },

    /// Import Claude Code "Recap" (`away_summary`) entries from the active
    /// session log as context commits. Idempotent — each recap UUID is
    /// recorded and skipped on subsequent runs.
    Recap {
        /// Explicit JSONL session file to scan (default: auto-detect latest)
        #[arg(long)]
        session: Option<PathBuf>,
        /// Only import recaps with an ISO-8601 timestamp after this cutoff
        /// (e.g. `2026-04-23T00:00:00Z`)
        #[arg(long)]
        since: Option<String>,
        /// Show what would be imported without modifying the workspace
        #[arg(long)]
        dry_run: bool,
    },

    /// Search context traces and session footprints for files relevant to a query.
    /// Combines BM25-style scoring over OBSERVE/THINK/ACT entries with git
    /// co-change analysis — no AST or embeddings required.
    Search {
        /// Natural-language query (e.g. "auth token expiry" or "retry logic")
        query: String,
        /// Maximum number of results to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Enrich top results with git co-change partners (walks last N commits)
        #[arg(long, default_value_t = 200)]
        history: usize,
        /// Output raw JSON instead of the pretty report
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ClaimsCommands {
    /// Record a claim with evidence pinned to one or more file paths at HEAD
    Add {
        /// The claim text (what you want future sessions to treat as pre-verified)
        text: String,
        /// One or more file paths that are the evidence for this claim.
        /// Pass repeatedly: --path src/foo.rs --path src/bar.rs
        #[arg(short, long = "path", value_name = "PATH", required = true)]
        paths: Vec<String>,
        /// Author tag (default: $H5I_AGENT_ID, else "human")
        #[arg(long)]
        author: Option<String>,
    },

    /// List all claims with live/stale status based on current HEAD
    List {
        /// Group claims by file path. Multi-path claims appear under each
        /// of their evidence paths. Replaces the per-file orientation view
        /// that `h5i summary list` used to provide.
        #[arg(long = "group-by-path")]
        group_by_path: bool,
    },

    /// Remove all claims whose evidence blobs have changed since recording
    Prune,
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Snapshot agent memory into .git/.h5i/memory/<commit-oid>/
    Snapshot {
        /// Git commit OID to associate this snapshot with (default: HEAD)
        #[arg(long)]
        commit: Option<String>,
        /// Agent memory backend to snapshot (default: inferred from H5I_AGENT_ID, else claude)
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
        /// Override the source directory to snapshot
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },

    /// Show how agent memory changed between two snapshots
    Diff {
        /// Snapshot to diff from (default: second-to-last snapshot)
        from: Option<String>,
        /// Snapshot to diff to; omit to compare against live memory (default: latest snapshot)
        to: Option<String>,
        /// Agent memory backend to compare against when diffing to live state
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
    },

    /// List all memory snapshots
    Log,

    /// Restore agent memory to the state captured in a snapshot
    Restore {
        /// Commit OID whose snapshot to restore
        commit: String,
        /// Agent memory backend to restore into
        #[arg(long, value_enum)]
        agent: Option<AgentRuntime>,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Push the latest memory snapshot to a git remote via refs/h5i/memory
    Push {
        /// Remote to push to
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },

    /// Fetch a teammate's memory snapshot from a git remote
    Pull {
        /// Remote to pull from
        #[arg(short, long, default_value = "origin")]
        remote: String,
    },
}

#[derive(Subcommand)]
enum HookCommands {
    /// Print install instructions for all Claude Code hooks
    Setup,

    /// Run as the PostToolUse handler: reads JSON from stdin, emits h5i context traces.
    /// Register in .claude/settings.json as: { "command": "h5i hook run" }
    Run,

    /// Run as the SessionStart handler: injects prior context into Claude's context window.
    /// Prints the current context summary + relevant prior reasoning to stdout,
    /// which Claude Code surfaces to the model at the start of each session.
    /// Register in .claude/settings.json under "SessionStart" hooks.
    SessionStart,

    /// Run as the Stop handler: auto-checkpoints the context workspace before the session ends.
    /// Summarises recent OBSERVE/THINK/ACT entries and calls `h5i context commit`.
    /// Register in .claude/settings.json under "Stop" hooks.
    Stop,
}

#[derive(Subcommand)]
enum CodexCommands {
    /// Print the current shared context so Codex can resume with prior reasoning
    Prelude,

    /// Sync OBSERVE/ACT traces from the active Codex session JSONL
    Sync,

    /// Sync the current Codex session and auto-checkpoint the context workspace
    Finish {
        /// Optional summary for the context checkpoint
        #[arg(long)]
        summary: Option<String>,
    },
}

#[derive(Subcommand)]
enum PolicyCommands {
    /// Create `.h5i/policy.toml` with starter rules
    Init,

    /// Check staged files against the current policy (dry-run)
    Check,

    /// Display the current policy configuration
    Show,
}

const H5I_CLAUDE_INSTRUCTIONS: &str = r#"## h5i Integration

This repository uses **h5i** (a Git sidecar for AI-era version control).

**Prefer MCP tools over Bash commands wherever possible.** h5i exposes native MCP tools (`h5i_context_trace`, `h5i_context_commit`, `h5i_commit`, `h5i_claims_add`, …) — they're faster and avoid shell-quoting pitfalls. Use `Bash: h5i …` only when no MCP tool covers the operation.

h5i metadata lives in `refs/h5i/*` and is NOT pushed by plain `git push`. Use `h5i push` to share it.

---

## Rules — MUST follow

Apply these automatically, without being asked.

### Context workspace

**At the start of every non-trivial task:**
```bash
h5i context status
# If no workspace exists yet, initialize one:
h5i context init --goal "<one-line summary of what you are about to do>"
```

**You do not need to call `h5i context trace` yourself.** h5i's hooks derive
the trace automatically:

- `PostToolUse` → OBSERVE for every `Read`, ACT for every `Edit` / `Write`.
- `Stop` → THINK entries mined from your own reasoning in the session
  transcript, plus NOTE entries for any deferrals / placeholders / unfulfilled
  promises detected.

The only trace entry worth emitting by hand is an explicit flag you want a
future reviewer to see *immediately* (not at next Stop). For that, use:

```bash
h5i context trace --kind NOTE "TODO: … / LIMITATION: … / RISK: …"
```

**After completing a logical milestone** (analysis done, feature implemented, bug fixed):
```bash
h5i context commit "<milestone summary>" --detail "<what was done and what is left>"
```

**Branch your reasoning** when you want to explore an alternative without losing the current thread:
```bash
h5i context branch experiment/sync-retry --purpose "try sync retry as a simpler fallback"
# ... explore ...
h5i context checkout main                   # return to main reasoning branch
h5i context merge experiment/sync-retry     # merge findings back if useful
```

**Before editing a non-trivial file**, surface prior reasoning that mentions it:
```bash
h5i context relevant src/repository.rs
```

---

### Committing code

**Always stage files before committing.** `h5i_commit` only commits what is staged and errors if nothing is staged.

```bash
git add <file1> <file2> …   # never `git add .`
```

Then commit via MCP (preferred):
```
h5i_commit(message="…", model="claude-sonnet-4-6", agent="claude-code", prompt="…")
```

Or via Bash if MCP is unavailable:
```bash
h5i commit -m "…" --model claude-sonnet-4-6 --agent claude-code --prompt "…"
```

Add flags when relevant:
- `--tests`  — tests were added or modified (captures test metrics)
- `--audit`  — security-sensitive, authentication, or high-risk changes

Every `h5i commit` automatically snapshots the context workspace and links it to the git commit SHA, so the workspace state is recoverable per code commit (`h5i context restore <sha>`, `h5i context diff <sha1> <sha2>`).

---

### Claims — pin reusable facts

`h5i claims` records content-addressed facts so future sessions don't re-derive them. Each claim pins a Merkle hash over its evidence files at HEAD; it stays **live** until any evidence blob changes, then auto-invalidates. Live claims are injected into the SessionStart prelude / `h5i context prompt` as pre-verified facts.

**Two flavors, both stored as plain claims (only the length and path-count differ):**
- **Cross-cutting fact** (~30 tokens, multiple paths). Example: *"HTTP only src/api/{client,auth,billing}.py."*
- **Per-file orientation** (~80 tokens, single path) — replaces the deprecated `h5i summary`. Example: *"src/api/client.py | HTTP. fetch_user(id: int)→dict GET, create_post(...)→dict POST, delete_post(id: int)→bool DELETE. Logger \`log\` top."*

**Record a claim when you have just established a non-obvious fact a future session would otherwise re-derive** — "X lives only in Y", "module M owns concern N", a subtle invariant, the public API of a struct, where *not* to look. Don't pin trivia a quick grep would answer.

Prefer the MCP tool:
```
h5i_claims_add(
  text="HTTP only src/api/client.py: fetch_user, create_post, delete_post.",
  paths=["src/api/client.py"]
)
h5i_claims_list()       # → {claims: [...], live: N, stale: M}
h5i_claims_prune()      # → {removed: N}
```

Or via Bash:
```bash
h5i claims add "HTTP only src/api/client.py: fetch_user, create_post, delete_post." \
  --path src/api/client.py
h5i claims list                  # all claims, flat
h5i claims list --group-by-path  # claims grouped by file ("what's known about each file")
h5i claims prune                 # drop claims whose evidence changed
```

**Evidence-path rule — the single most important thing to get right:**
Pick the *minimum* set of files whose content, if edited, should cause the claim to be re-checked. Ask: *"If I changed file X, would this claim's truth be in doubt?"* If no, do not include X — even if you read X while establishing the claim.

Why: the claim auto-invalidates the moment *any* evidence blob changes. Over-listing guarantees rapid staleness from unrelated edits and trains future sessions to distrust claims.

Concrete example. Claim: *"HTTP only in `src/api/client.py`"*.
- ✔ Good: `--path src/api/client.py` (one path). If client.py changes, re-check. Edits to formatters/validators/main.py do not affect the truth of this claim.
- ✖ Bad: `--path src/api/client.py --path src/utils/format.py --path main.py`. Goes stale the next time someone touches an unrelated helper — even though the claim was still true.

Rule of thumb: **most good claims cite 1 file; >3 is a red flag** you're confusing "files I read" with "files that back the claim".

**Other rules:**
- Evidence paths must be tracked in HEAD.
- If the SessionStart prelude already shows a claim covering what you were about to investigate, trust it — don't re-read the files unless the user asks.
- If a live claim is wrong, fix it: `h5i claims prune` removes only stale ones; you can also delete the JSON in `.git/.h5i/claims/` directly to remove a wrong-but-live claim.

**Write claim text in caveman style.**
- Cross-cutting: ~30 tokens. Per-file orientation: ~80 tokens.
- Drop articles, copulas, fluff. Keep paths, identifier names, types, numeric constants exact.
- Live claims are re-read on every cached-prefix turn forever — every word costs forever.

| | Bloated (don't) | Caveman (do) |
|---|---|---|
| Cross-cutting | "All HTTP-making functions in this project live only in src/api/client.py (fetch_user, create_post, delete_post). main.py and src/utils/* contain no direct HTTP." | "HTTP only src/api/client.py: fetch_user, create_post, delete_post. main.py + utils/* no HTTP." |
| Per-file | "The src/api/client.py file is an HTTP client module that uses the requests library to call the example API. It exports three functions and a logger." | "src/api/client.py \\| HTTP. requests to api.example.com. fetch_user(id: int)→dict GET, create_post(...)→dict POST, delete_post(id: int)→bool DELETE. Logger \\`log\\` top." |
| Invariant | "The session token must be validated using a constant-time comparison to avoid timing attacks." | "Session token: constant-time compare. Timing attack risk." |

**Frequency knob (`$H5I_CLAIMS_FREQUENCY`)** — the user can tune how eagerly you record claims:
- `off` — do not record any this session, even if one would normally be warranted.
- `low` (default) — only non-obvious, genuinely reusable facts.
- `high` — record liberally; pin any reusable codebase insight. The evidence-path rule applies *especially* here.

The SessionStart prelude prints the active policy when it is `off` or `high`. Follow the most recent policy line you see, even if it contradicts this base guidance.

---

### Memory Snapshots

After a significant Claude Code session, snapshot Claude's memory so it can be shared or restored:

```bash
h5i memory snapshot        # snapshot current ~/.claude/projects/<repo>/memory/ → HEAD
h5i memory log             # list all snapshots
h5i memory diff            # show what changed since the previous snapshot
h5i memory restore <oid>   # restore memory to the state at a given commit
```

---

### Sharing h5i Data

```bash
h5i push   # push all h5i refs to origin
h5i pull   # pull h5i refs from origin
```
"#;

const H5I_CODEX_INSTRUCTIONS: &str = r#"## h5i Integration

This repository uses **h5i** (a Git sidecar for AI-era version control).

Codex should use `h5i context` as shared cross-session memory and `h5i commit` to record AI provenance on code commits.

### Workflow

**At the start of a non-trivial task:**
```bash
h5i codex prelude
# If no workspace exists yet, initialize it once:
h5i context init --goal "<one-line task summary>"
```

**While working:**
```bash
h5i context relevant <file>   # before editing — surfaces prior reasoning + claims that mention this file
h5i codex sync                # after a burst of reads/edits — auto-traces OBSERVE/ACT and mines THINK/NOTE from your transcript
```

You do not need to emit OBSERVE / THINK / ACT trace entries by hand —
`h5i codex sync` (and `h5i codex finish`) derives them from the Codex
session JSONL. The only trace you should write directly is an explicit
flag a reviewer must see immediately:

```bash
h5i context trace --kind NOTE "TODO: … / LIMITATION: … / RISK: …"
```

**After a logical milestone:**
```bash
h5i codex finish --summary "<milestone summary>"
```

### Claims — pin reusable facts

After establishing a non-obvious fact a future session would otherwise re-derive
(where a helper lives, which module owns a concern, a subtle invariant), record
a content-addressed claim pointing at the files that back it. Live claims are
injected into `h5i codex prelude` / `h5i context prompt`, so the next session
treats them as pre-verified — trust them; don't re-read the files.

**Two flavors:**

Cross-cutting fact (~30 tokens, multiple paths):
```bash
h5i claims add "HTTP only src/api/client.py: fetch_user, create_post, delete_post." \
  --path src/api/client.py
```

Per-file orientation (~80 tokens, single path) — replaces the deprecated `h5i summary`:
```bash
h5i claims add "src/api/client.py | HTTP. fetch_user(id: int)→dict GET, create_post(...)→dict POST, delete_post(id: int)→bool DELETE. Logger \`log\` top." \
  --path src/api/client.py
```

Inspect:
```bash
h5i claims list                    # live / stale badges
h5i claims list --group-by-path    # claims grouped by file ("what's known about each file")
h5i claims prune                   # drop stale claims
```

**Caveman style.** Drop articles, copulas, fluff. Keep paths, identifier names, types, numbers exact. Pick the *minimum* evidence-path set: most good claims cite 1 file; >3 is a red flag you're confusing "files I read" with "files that back the claim". Live claim text is re-read on every cached-prefix turn forever — every word costs forever.

### Code commits

```bash
git add <exact paths>
h5i commit -m "…" --agent codex --prompt "…"
```

Add flags when relevant:
- `--tests`  — tests were added or modified
- `--audit`  — security-sensitive or high-risk changes

### Sharing h5i Data

```bash
h5i push   # push all h5i refs to origin
h5i pull   # pull h5i refs from origin
```
"#;

fn write_claude_instructions(workdir: &Path) -> anyhow::Result<()> {
    use std::io::Write as _;

    let claude_dir = workdir.join(".claude");
    std::fs::create_dir_all(&claude_dir)?;

    let h5i_md = claude_dir.join("h5i.md");
    if !h5i_md.exists() {
        std::fs::write(&h5i_md, H5I_CLAUDE_INSTRUCTIONS)?;
    }

    let claude_md = workdir.join("CLAUDE.md");
    let existing = std::fs::read_to_string(&claude_md).unwrap_or_default();
    if !existing.contains("@.claude/h5i.md") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&claude_md)?;
        writeln!(f, "\n@.claude/h5i.md")?;
    }

    Ok(())
}

fn write_codex_instructions(workdir: &Path) -> anyhow::Result<()> {
    use std::io::Write as _;

    let agents_md = workdir.join("AGENTS.md");
    let existing = std::fs::read_to_string(&agents_md).unwrap_or_default();
    if existing.contains("h5i codex prelude") {
        return Ok(());
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&agents_md)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(f)?;
    }
    writeln!(f, "\n{H5I_CODEX_INSTRUCTIONS}")?;
    Ok(())
}

fn print_shared_context_prelude(workdir: &Path) {
    let has_ctx = match git2::Repository::discover(workdir) {
        Ok(r) => r.find_reference("refs/h5i/context/main").is_ok(),
        Err(_) => false,
    };
    if !has_ctx {
        println!("[h5i] No context workspace yet. Run `h5i context init --goal \"...\"`.");
        return;
    }

    let opts = ctx::ContextOpts {
        branch: None,
        commit_hash: None,
        show_log: true,
        log_offset: 0,
        metadata_segment: None,
        window: 3,
        depth: 1,
    };
    let Ok(snap) = ctx::gcc_context(workdir, &opts) else {
        return;
    };

    println!("[h5i] Context workspace active — prior reasoning follows.");
    println!();
    ctx::print_context_depth(&snap, 1);

    let thinks_acts: Vec<&String> = snap
        .recent_log_lines
        .iter()
        .filter(|l| l.contains("] THINK:") || l.contains("] ACT:"))
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !thinks_acts.is_empty() {
        println!();
        println!("[h5i] Last decisions & actions:");
        for line in thinks_acts {
            println!("  {line}");
        }
    }

    if !snap.todo_items.is_empty() {
        println!();
        println!("[h5i] Open TODOs:");
        for t in snap.todo_items.iter().take(5) {
            println!("  □ {t}");
        }
    }

    if let Ok(h5i_repo) = H5iRepository::open(workdir) {
        if let Ok(live) = claims::live_claims(&h5i_repo.h5i_root, h5i_repo.git()) {
            if !live.is_empty() {
                const MAX_SHOWN: usize = 10;
                println!();
                println!(
                    "[h5i] Live claims (pre-verified facts — trust, don't re-derive):"
                );
                for claim in live.iter().take(MAX_SHOWN) {
                    let paths = claim.evidence_paths.join(", ");
                    println!("  ● {}", claim.text);
                    println!("      ↳ {paths}");
                }
                if live.len() > MAX_SHOWN {
                    println!(
                        "  … {} more. Run `h5i claims list` to see all.",
                        live.len() - MAX_SHOWN
                    );
                }
            }
        }

    }

    if let Some(hint) = claims::ClaimsFrequency::from_env().prelude_hint() {
        println!();
        println!("{hint}");
    }

    println!();
    println!("[h5i] Use `h5i context show` for full details.");
}

/// Persisted cursor for [`auto_derive_traces_from_claude_session`].
///
/// Stored at `.git/.h5i/claude_autotrace_state.json`. We track which session
/// has been processed so the Stop hook is idempotent across re-runs and
/// re-attaches: re-running the hook on the same JSONL emits zero traces.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ClaudeAutoTraceState {
    /// The Claude session UUID (jsonl filename stem) last consumed.
    session_id: String,
    /// Number of JSONL lines we'd already mined. Reserved for future
    /// incremental analysis; today we always re-analyze the whole file
    /// and rely on textual dedup against the trace log.
    processed_lines: usize,
}

/// Mine the active Claude Code session JSONL and emit derived trace entries.
///
/// PostToolUse already emits OBSERVE for `Read` and ACT for `Edit`/`Write`
/// as the agent works. This function fills the remaining gap: turning the
/// reasoning recorded in the transcript into trace entries the agent did
/// not have to write itself.
///
/// Specifically:
///   - `causal_chain.key_decisions` → THINK entries
///   - `omissions` (Deferral / Placeholder / UnfulfilledPromise) → NOTE entries
///
/// Returns the number of new entries appended. Existing entries are deduped
/// against the current branch's `trace.md` so re-running is idempotent.
fn auto_derive_traces_from_claude_session(workdir: &Path) -> anyhow::Result<usize> {
    // Only emit when h5i context is initialized — otherwise we have nowhere
    // to write and shouldn't surprise users who haven't opted in.
    let has_ctx = match git2::Repository::discover(workdir) {
        Ok(r) => r.find_reference("refs/h5i/context/main").is_ok(),
        Err(_) => false,
    };
    if !has_ctx {
        return Ok(0);
    }

    let Some(jsonl) = session_log::find_latest_session(workdir) else {
        return Ok(0);
    };

    let session_id = jsonl
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Read the existing trace.md content for dedup.
    let branch = ctx::current_branch(workdir);
    let trace_path = format!("branches/{branch}/trace.md");
    let existing = ctx::read_ctx_file(workdir, &trace_path).unwrap_or_default();

    let analysis = session_log::analyze_session(&jsonl, None)?;
    let mut emitted = 0usize;

    for decision in &analysis.causal_chain.key_decisions {
        let trimmed = truncate(decision.trim(), 240);
        if trimmed.is_empty() {
            continue;
        }
        // Substring dedup against the existing trace log. Cheap and good
        // enough — `key_decisions` is capped at 12 sentences per session.
        if existing.contains(&trimmed) {
            continue;
        }
        if ctx::append_log(workdir, "THINK", &trimmed, false).is_ok() {
            emitted += 1;
        }
    }

    for omission in &analysis.omissions {
        // Prefer the contextual snippet ("…I'll skip integration tests for
        // now since the repo has no harness…") over the bare matched phrase
        // ("for now"). The phrase alone makes NOTEs unreadable in the DAG.
        let detail = omission.snippet.trim();
        let detail = if detail.is_empty() {
            omission.phrase.trim()
        } else {
            detail
        };
        let body = if omission.context_file.is_empty() {
            format!("{}: {}", omission.kind, detail)
        } else {
            format!("{} ({}): {}", omission.kind, omission.context_file, detail)
        };
        let body = truncate(&body, 240);
        // Dedup against the snippet when available (so the same passage
        // ingested twice via different phrase matches collapses to one NOTE)
        // and fall back to the phrase for legacy entries.
        let dedup_key = if !omission.snippet.trim().is_empty() {
            omission.snippet.trim()
        } else {
            omission.phrase.trim()
        };
        if body.is_empty() || existing.contains(dedup_key) {
            continue;
        }
        if ctx::append_log(workdir, "NOTE", &body, false).is_ok() {
            emitted += 1;
        }
    }

    // Persist cursor so a re-run on the same JSONL is a no-op even if the
    // trace log gets manually truncated. (Strict idempotency belt-and-suspenders.)
    if let Ok(state_path) = autotrace_state_path(workdir) {
        let next = ClaudeAutoTraceState {
            session_id,
            processed_lines: std::fs::read_to_string(&jsonl)
                .map(|raw| raw.lines().count())
                .unwrap_or(0),
        };
        let _ = std::fs::write(&state_path, serde_json::to_string_pretty(&next).unwrap_or_default());
    }

    Ok(emitted)
}

fn autotrace_state_path(workdir: &Path) -> anyhow::Result<PathBuf> {
    let repo = git2::Repository::discover(workdir)?;
    let h5i_dir = repo.path().join(".h5i");
    std::fs::create_dir_all(&h5i_dir)?;
    Ok(h5i_dir.join("claude_autotrace_state.json"))
}

fn auto_checkpoint_context(workdir: &Path, explicit_summary: Option<&str>) -> anyhow::Result<()> {
    let has_ctx = match git2::Repository::discover(workdir) {
        Ok(r) => r.find_reference("refs/h5i/context/main").is_ok(),
        Err(_) => false,
    };
    if !has_ctx {
        return Ok(());
    }

    let opts = ctx::ContextOpts {
        branch: None,
        commit_hash: None,
        show_log: true,
        log_offset: 0,
        metadata_segment: None,
        window: 1,
        depth: 3,
    };
    let summary = if let Some(summary) = explicit_summary {
        summary.to_string()
    } else if let Ok(snap) = ctx::gcc_context(workdir, &opts) {
        let acts: Vec<String> = snap
            .recent_log_lines
            .iter()
            .filter(|l| l.contains("] ACT:"))
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if acts.is_empty() {
            "session ended (auto-checkpoint)".to_string()
        } else {
            let joined = acts
                .iter()
                .map(|l| {
                    l.split("] ACT:")
                        .nth(1)
                        .unwrap_or(l)
                        .trim()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("; ");
            truncate(&joined, 120)
        }
    } else {
        "session ended (auto-checkpoint)".to_string()
    };

    ctx::gcc_commit(workdir, &summary, "")?;
    println!(
        "{} Auto-checkpointed context: {}",
        SUCCESS,
        style(summary).italic()
    );
    Ok(())
}

/// Recursively merge two git trees with `overlay` winning on path conflicts.
///
/// Used by `h5i pull` to union-merge `refs/h5i/notes` after a divergence:
/// since each tree entry is keyed by code-commit OID and code commits are
/// content-addressed, two parties' notes typically annotate disjoint OIDs
/// and "union" is exactly the right merge for them. On the rare case the
/// same code-commit OID is annotated on both sides (would imply offline
/// concurrent annotation of the same commit), `overlay` wins — we use this
/// to prefer local content over incoming so a pull is never destructive.
///
/// Subtrees are merged recursively so a future fan-out by libgit2 (which
/// our notes refs use today only with flat trees, but may not forever)
/// keeps working without code changes here.
fn union_merge_trees(
    repo: &git2::Repository,
    base: Option<&git2::Tree<'_>>,
    overlay: Option<&git2::Tree<'_>>,
) -> Result<git2::Oid, git2::Error> {
    use std::collections::BTreeMap;

    enum Slot {
        Blob(i32, git2::Oid),
        Subtree(git2::Oid),
    }

    let mut merged: BTreeMap<String, Slot> = BTreeMap::new();

    if let Some(t) = base {
        for entry in t.iter() {
            let name = match entry.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            match entry.kind() {
                Some(git2::ObjectType::Blob) => {
                    merged.insert(name, Slot::Blob(entry.filemode(), entry.id()));
                }
                Some(git2::ObjectType::Tree) => {
                    merged.insert(name, Slot::Subtree(entry.id()));
                }
                _ => {}
            }
        }
    }

    if let Some(t) = overlay {
        for entry in t.iter() {
            let name = match entry.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            match entry.kind() {
                Some(git2::ObjectType::Blob) => {
                    merged.insert(name, Slot::Blob(entry.filemode(), entry.id()));
                }
                Some(git2::ObjectType::Tree) => {
                    let merged_oid = match merged.get(&name) {
                        Some(Slot::Subtree(prev_oid)) => {
                            let prev = repo.find_tree(*prev_oid)?;
                            let new = repo.find_tree(entry.id())?;
                            union_merge_trees(repo, Some(&prev), Some(&new))?
                        }
                        _ => entry.id(),
                    };
                    merged.insert(name, Slot::Subtree(merged_oid));
                }
                _ => {}
            }
        }
    }

    let mut builder = repo.treebuilder(None)?;
    for (name, slot) in &merged {
        match slot {
            Slot::Blob(mode, oid) => {
                builder.insert(name.as_str(), *oid, *mode)?;
            }
            Slot::Subtree(oid) => {
                builder.insert(name.as_str(), *oid, 0o040000)?;
            }
        }
    }
    builder.write()
}

/// Build a union-merge commit of two notes commits and return its OID.
///
/// The new commit has both inputs as parents (so future fast-forwards from
/// either side stay valid) and a tree that is the union of both — with the
/// `local` side winning on the (theoretical) per-OID conflict.
fn union_merge_notes_commits(
    repo: &git2::Repository,
    local_oid: git2::Oid,
    incoming_oid: git2::Oid,
) -> Result<git2::Oid, git2::Error> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;
    let local_tree = local_commit.tree()?;
    let incoming_tree = incoming_commit.tree()?;

    // base = incoming (loser), overlay = local (winner) → local wins on conflict.
    let merged_tree_oid = union_merge_trees(repo, Some(&incoming_tree), Some(&local_tree))?;
    let merged_tree = repo.find_tree(merged_tree_oid)?;

    let sig = repo.signature().unwrap_or_else(|_| {
        git2::Signature::now("h5i", "h5i@local")
            .expect("static signature components 'h5i' / 'h5i@local' are always valid")
    });

    let parents = [&local_commit, &incoming_commit];
    repo.commit(
        None,
        &sig,
        &sig,
        "h5i pull: union-merge of refs/h5i/notes",
        &merged_tree,
        &parents,
    )
}

fn print_doctor_report(report: &storage::DoctorReport) {
    let status = if report.ok { SUCCESS } else { ERROR };
    let label = if report.ok {
        style("storage healthy").green().bold()
    } else {
        style("storage problems found").red().bold()
    };
    println!("{} {}", status, label);
    println!("  root: {}", style(report.h5i_root.display()).dim());
    match report.schema_version {
        Some(v) => println!("  schema: {}", style(v).cyan()),
        None => println!("  schema: {}", style("missing").yellow()),
    }
    if report.repaired {
        println!("  repaired: {}", style("yes").green());
    }
    if let Some(path) = &report.export_path {
        println!("  export: {}", style(path.display()).cyan());
    }

    if report.issues.is_empty() {
        println!("\n  {}", style("No issues found.").dim());
        return;
    }

    println!();
    for issue in &report.issues {
        let prefix = match issue.severity {
            DoctorSeverity::Ok => style("ok").green(),
            DoctorSeverity::Warning => style("warn").yellow(),
            DoctorSeverity::Error => style("error").red().bold(),
        };
        println!("  {} [{}] {}", prefix, issue.code, issue.detail);
        if let Some(repair) = &issue.repair {
            println!("      repair: {}", style(repair).dim());
        }
    }
}

/// Translate `h5i <noun> <verb> ...` into the legacy form before clap parses.
///
/// Returns the rewritten argv. When `argv[1]` is one of the four noun groups
/// (`capture` / `recall` / `audit` / `share`), the noun + verb tokens are
/// looked up in [`noun_alias`] and replaced with the legacy verb (possibly
/// multiple tokens). When the verb is missing or `--help`/`-h`, a help block
/// for that noun is printed and the process exits.
fn rewrite_noun_argv(argv: Vec<String>) -> Vec<String> {
    if argv.len() < 2 {
        return argv;
    }
    // `h5i help <noun>` is a synonym for `h5i <noun> --help`.
    if argv[1] == "help"
        && argv
            .get(2)
            .map(|t| matches!(t.as_str(), "capture" | "recall" | "audit" | "share"))
            .unwrap_or(false)
    {
        print_noun_help(&argv[2]);
        std::process::exit(0);
    }
    let noun = match argv[1].as_str() {
        "capture" | "recall" | "audit" | "share" => argv[1].clone(),
        _ => return argv,
    };

    // No verb (or asking for help): print the noun's verb listing and exit.
    if argv.len() < 3 || matches!(argv[2].as_str(), "--help" | "-h" | "help") {
        print_noun_help(&noun);
        std::process::exit(0);
    }

    let verb = argv[2].as_str();

    // Allow `h5i <noun> help` as a synonym for `h5i <noun> --help`.
    if matches!(verb, "help") {
        print_noun_help(&noun);
        std::process::exit(0);
    }

    let Some(mapped) = noun_alias(&noun, verb) else {
        // Suggest the closest known verb under this noun.
        let suggestion = nearest_verb(&noun, verb);
        eprintln!(
            "{} `h5i {} {}` is not a known subcommand.",
            style("error:").red().bold(),
            noun,
            verb,
        );
        if let Some(sugg) = suggestion {
            eprintln!(
                "       Did you mean `{}`?",
                style(format!("h5i {} {}", noun, sugg)).cyan().bold(),
            );
        }
        eprintln!(
            "       Run `{}` for the full list.",
            style(format!("h5i {} --help", noun)).cyan(),
        );
        std::process::exit(2);
    };

    // Rebuild argv: [bin, ...mapped, ...rest]
    let mut out = Vec::with_capacity(argv.len() + mapped.len());
    out.push(argv[0].clone());
    for tok in mapped {
        out.push(tok.to_string());
    }
    out.extend(argv.into_iter().skip(3));
    out
}

/// Return the verb under `noun` whose name is closest (Levenshtein ≤ 2) to `typo`.
fn nearest_verb(noun: &str, typo: &str) -> Option<&'static str> {
    let candidates: &[&'static str] = match noun {
        "capture" => &["commit", "claim", "memory"],
        "recall" => &[
            "log", "blame", "diff", "context", "claims", "notes", "memory", "recap", "resume",
            "vibe",
        ],
        "audit" => &["review", "scan", "compliance", "policy", "vibe"],
        "share" => &["push", "pull", "pr", "memory"],
        _ => return None,
    };
    let typo_l = typo.to_lowercase();
    let mut best: Option<(usize, &'static str)> = None;
    for &c in candidates {
        let d = levenshtein(&typo_l, c);
        if d <= 2 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, v)| v)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (cur[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Map `(noun, verb)` to the legacy argv tokens that implement it.
fn noun_alias(noun: &str, verb: &str) -> Option<&'static [&'static str]> {
    Some(match (noun, verb) {
        // ── capture ─────────────────────────────────────────────────────
        ("capture", "commit")   => &["commit"],
        ("capture", "claim")    => &["claims", "add"],
        ("capture", "claims")   => &["claims", "add"],
        ("capture", "memory")   => &["memory", "snapshot"],

        // ── recall ──────────────────────────────────────────────────────
        ("recall",  "log")      => &["log"],
        ("recall",  "blame")    => &["blame"],
        ("recall",  "diff")     => &["diff"],
        ("recall",  "context")  => &["context"],
        ("recall",  "claims")   => &["claims", "list"],
        ("recall",  "claim")    => &["claims", "list"],
        ("recall",  "notes")    => &["notes"],
        ("recall",  "memory")   => &["memory"],
        ("recall",  "recap")    => &["context", "recap"],
        ("recall",  "resume")   => &["resume"],
        ("recall",  "vibe")     => &["vibe"],

        // ── audit ───────────────────────────────────────────────────────
        ("audit",   "review")   => &["notes", "review"],
        ("audit",   "scan")     => &["context", "scan"],
        ("audit",   "compliance") => &["compliance"],
        ("audit",   "policy")   => &["policy"],
        ("audit",   "vibe")     => &["vibe"],
        ("audit",   "notes")    => &["notes", "review"],

        // ── share ───────────────────────────────────────────────────────
        ("share",   "push")     => &["push"],
        ("share",   "pull")     => &["pull"],
        ("share",   "pr")       => &["pr"],
        ("share",   "memory")   => &["memory"],

        _ => return None,
    })
}

/// One row in a noun-group help table.
struct NounVerb {
    verb: &'static str,
    summary: &'static str,
    legacy: &'static str,
    example: &'static str,
}

fn noun_table(noun: &str) -> (&'static str, &'static [NounVerb], &'static [&'static str]) {
    match noun {
        "capture" => (
            "record provenance as you make changes",
            &[
                NounVerb {
                    verb: "commit",
                    summary: "Git commit + AI provenance (prompt, model, tokens, tests, decisions).",
                    legacy: "h5i commit",
                    example: "h5i capture commit -m \"fix retry loop\" \\\n        --model claude-sonnet-4-6 --agent claude-code \\\n        --prompt \"add exponential backoff\" --tests",
                },
                NounVerb {
                    verb: "claim",
                    summary: "Pin a content-addressed fact backed by evidence files (auto-invalidates on edit).",
                    legacy: "h5i claims add",
                    example: "h5i capture claim \"HTTP only src/api/client.py: fetch_user, create_post\" \\\n        --path src/api/client.py",
                },
                NounVerb {
                    verb: "memory",
                    summary: "Snapshot agent (Claude/Codex) memory state into refs/h5i/memory.",
                    legacy: "h5i memory snapshot",
                    example: "h5i capture memory --agent claude",
                },
            ],
            &[
                "Tip: `h5i commit` and `h5i claims add` still work but emit a deprecation hint.",
                "MCP equivalents: h5i_commit, h5i_claims_add, h5i_memory_snapshot.",
            ],
        ),
        "recall" => (
            "read AI history, context, and review signals",
            &[
                NounVerb {
                    verb: "log",
                    summary: "Commit history with AI provenance (model, prompt, tokens, tests).",
                    legacy: "h5i log",
                    example: "h5i recall log --limit 20",
                },
                NounVerb {
                    verb: "blame",
                    summary: "Line- or AST-level blame, annotated with AI prompts per commit boundary.",
                    legacy: "h5i blame",
                    example: "h5i recall blame src/api/client.py --mode ast --show-prompt",
                },
                NounVerb {
                    verb: "diff",
                    summary: "Structural (AST) diff for a single file between two commits.",
                    legacy: "h5i diff",
                    example: "h5i recall diff src/model.py --from HEAD~3",
                },
                NounVerb {
                    verb: "context",
                    summary: "Reasoning workspace: goal, milestones, OBSERVE/THINK/ACT trace, branches.",
                    legacy: "h5i context",
                    example: "h5i recall context show --trace --window 5",
                },
                NounVerb {
                    verb: "claims",
                    summary: "List live & stale content-addressed claims.",
                    legacy: "h5i claims list",
                    example: "h5i recall claims --group-by-path",
                },
                NounVerb {
                    verb: "notes",
                    summary: "Per-commit signals: footprint, uncertainty, omissions, churn, coverage.",
                    legacy: "h5i notes",
                    example: "h5i recall notes show",
                },
                NounVerb {
                    verb: "memory",
                    summary: "Log / diff / restore agent memory snapshots.",
                    legacy: "h5i memory",
                    example: "h5i recall memory log",
                },
                NounVerb {
                    verb: "recap",
                    summary: "Import Claude Code `away_summary` entries as context milestones.",
                    legacy: "h5i context recap",
                    example: "h5i recall recap",
                },
                NounVerb {
                    verb: "resume",
                    summary: "Print a structured handoff briefing to resume an AI session.",
                    legacy: "h5i resume",
                    example: "h5i recall resume",
                },
                NounVerb {
                    verb: "vibe",
                    summary: "Quick AI-footprint audit (also under `audit`).",
                    legacy: "h5i vibe",
                    example: "h5i recall vibe",
                },
            ],
            &[
                "Tip: legacy top-level forms (`h5i log`, `h5i blame`, …) still work — they print a one-line deprecation hint.",
                "MCP equivalents: h5i_log, h5i_blame, h5i_context_show, h5i_claims_list, h5i_notes_show.",
            ],
        ),
        "audit" => (
            "assess risk on AI-generated changes",
            &[
                NounVerb {
                    verb: "review",
                    summary: "Rank commits by uncertainty, blind edits, churn, scope — surface the riskiest first.",
                    legacy: "h5i notes review",
                    example: "h5i audit review --limit 50",
                },
                NounVerb {
                    verb: "scan",
                    summary: "Scan reasoning traces for prompt-injection patterns and exfil attempts.",
                    legacy: "h5i context scan",
                    example: "h5i audit scan",
                },
                NounVerb {
                    verb: "compliance",
                    summary: "Date-ranged audit report — text, JSON, or HTML (regulated workflows).",
                    legacy: "h5i compliance",
                    example: "h5i audit compliance --since 2026-01-01 --until 2026-03-31 \\\n        --format html --output audit.html",
                },
                NounVerb {
                    verb: "policy",
                    summary: "Manage `.h5i/policy.toml` rules (block on credential leak, audit on auth, …).",
                    legacy: "h5i policy",
                    example: "h5i audit policy init",
                },
                NounVerb {
                    verb: "vibe",
                    summary: "Repo-wide AI footprint: % AI-generated, fully-AI directories, token leak signals.",
                    legacy: "h5i vibe",
                    example: "h5i audit vibe --limit 1000 --json",
                },
            ],
            &[
                "Use `h5i audit review` as a triage funnel before merging an AI-heavy branch.",
                "Pair `h5i audit compliance` with `h5i share pr post` for an auditable PR trail.",
            ],
        ),
        "share" => (
            "publish provenance — push, pull, and surface on PRs",
            &[
                NounVerb {
                    verb: "push",
                    summary: "Push all refs/h5i/* (notes, context, memory, ast) to a remote in one shot.",
                    legacy: "h5i push",
                    example: "h5i share push",
                },
                NounVerb {
                    verb: "pull",
                    summary: "Fetch & union-merge refs/h5i/* from a remote (notes auto-merge, chain refs warn on divergence).",
                    legacy: "h5i pull",
                    example: "h5i share pull",
                },
                NounVerb {
                    verb: "pr",
                    summary: "Post or preview a sticky GitHub PR comment with h5i provenance per AI commit.",
                    legacy: "(new)",
                    example: "h5i share pr post              # upsert sticky comment\n      h5i share pr body --limit 25  # render markdown to stdout\n      h5i share pr post --dry-run   # preview without calling gh",
                },
                NounVerb {
                    verb: "memory",
                    summary: "Push or pull only the agent-memory refs (refs/h5i/memory/*).",
                    legacy: "h5i memory push|pull",
                    example: "h5i share memory push",
                },
            ],
            &[
                "`h5i share pr post` needs the `gh` CLI authenticated (`gh auth login`).",
                "The PR comment is idempotent — re-running upserts in place via an HTML marker.",
            ],
        ),
        _ => ("", &[], &[]),
    }
}

fn print_noun_help(noun: &str) {
    let (tagline, rows, tips) = noun_table(noun);
    if rows.is_empty() {
        return;
    }

    println!(
        "{}{}\n",
        style(format!("h5i {noun} — ")).bold().cyan(),
        style(tagline).dim(),
    );

    // Column-aligned table of verbs.
    let verb_w = rows.iter().map(|r| r.verb.len()).max().unwrap_or(0);
    let legacy_w = rows.iter().map(|r| r.legacy.len()).max().unwrap_or(0);

    println!(
        "  {:<vw$}  {:<lw$}  {}",
        style("VERB").dim().bold(),
        style("LEGACY").dim().bold(),
        style("SUMMARY").dim().bold(),
        vw = verb_w,
        lw = legacy_w,
    );
    for r in rows {
        println!(
            "  {:<vw$}  {:<lw$}  {}",
            style(r.verb).bold().green(),
            style(r.legacy).dim(),
            r.summary,
            vw = verb_w,
            lw = legacy_w,
        );
    }

    println!("\n{}", style("Examples").bold());
    // Width of the "  <verb>  $ " prefix used on the first line so continuation
    // lines line up underneath the command, not under the verb column.
    let cont_indent = 2 + verb_w + 2 + 2;
    for r in rows {
        let mut lines = r.example.lines();
        if let Some(first) = lines.next() {
            println!(
                "  {}  $ {}",
                style(format!("{:<vw$}", r.verb, vw = verb_w)).dim(),
                style(first).cyan(),
            );
        }
        for cont in lines {
            // Trim leading whitespace from the embedded example so all
            // continuations share the same column, regardless of how the
            // string literal was indented.
            let trimmed = cont.trim_start();
            println!("{}{}", " ".repeat(cont_indent), style(trimmed).cyan());
        }
    }

    if !tips.is_empty() {
        println!("\n{}", style("Tips").bold());
        for t in tips {
            println!("  • {t}");
        }
    }
    println!(
        "\nFor flag-level help on any verb, run e.g. `{}`.",
        style(format!("h5i {} <verb> --help", noun)).cyan()
    );
}

/// One-line deprecation hint for the hidden legacy top-level verbs.
///
/// Goes to stderr so it never pollutes piped stdout (`h5i log | grep ...`).
fn legacy_hint(legacy_verb: &str, new_form: &str) {
    eprintln!(
        "{} `{}` → use `{}` (see `{}`). Legacy form still works for now.",
        style("h5i hint:").yellow().bold(),
        style(format!("h5i {}", legacy_verb)).dim(),
        style(new_form).cyan().bold(),
        style(format!("h5i {} --help", new_form.split_whitespace().nth(1).unwrap_or(""))).dim(),
    );
}

/// Check if argv[1] is a hidden legacy verb and emit the deprecation hint.
fn maybe_legacy_hint(argv: &[String]) {
    if argv.len() < 2 {
        return;
    }
    let hint_for = |v: &str| -> Option<&'static str> {
        match v {
            "commit"     => Some("h5i capture commit"),
            "log"        => Some("h5i recall log"),
            "blame"      => Some("h5i recall blame"),
            "push"       => Some("h5i share push"),
            "pull"       => Some("h5i share pull"),
            "memory"     => Some("h5i recall memory  (or `h5i capture memory` / `h5i share memory`)"),
            "claims"     => Some("h5i recall claims  (or `h5i capture claim`)"),
            "notes"      => Some("h5i recall notes   (or `h5i audit review`)"),
            "context"    => Some("h5i recall context"),
            "vibe"       => Some("h5i recall vibe    (or `h5i audit vibe`)"),
            "compliance" => Some("h5i audit compliance"),
            "pr"         => Some("h5i share pr"),
            _ => None,
        }
    };
    if let Some(new_form) = hint_for(argv[1].as_str()) {
        legacy_hint(&argv[1], new_form);
    }
}

fn init_tracing() {
    // Off by default. Users opt in via RUST_LOG / H5I_LOG (e.g.
    // `H5I_LOG=h5i_core=debug`). Writes to stderr so it doesn't poison stdout
    // for piped/MCP consumers.
    let filter = tracing_subscriber::EnvFilter::try_from_env("H5I_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let argv: Vec<String> = std::env::args().collect();
    // `rewrote` is true when we translated a `capture/recall/audit/share`
    // invocation — in that case the user did NOT type the legacy form, so we
    // must NOT emit the "this has moved" hint.
    let rewrote = matches!(argv.get(1).map(String::as_str), Some("capture" | "recall" | "audit" | "share"));
    let argv = rewrite_noun_argv(argv);
    if !rewrote {
        maybe_legacy_hint(&argv);
    }
    let cli = Cli::parse_from(argv);

    match cli.command {
        // These four arms only fire if the pre-clap rewriter missed (it shouldn't —
        // it always rewrites or exits). Defensive fallback: print noun help.
        Commands::Capture { .. } => {
            print_noun_help("capture");
            std::process::exit(0);
        }
        Commands::Recall { .. } => {
            print_noun_help("recall");
            std::process::exit(0);
        }
        Commands::Audit { .. } => {
            print_noun_help("audit");
            std::process::exit(0);
        }
        Commands::Share { .. } => {
            print_noun_help("share");
            std::process::exit(0);
        }

        Commands::Pr { action } => match action {
            PrCommands::Post { number, limit, style, dry_run } => {
                let workdir = std::env::current_dir()?;
                let body = h5i_core::pr::render_body_with_style(&workdir, limit, style.into())?;
                if dry_run {
                    println!("{}", body);
                    return Ok(());
                }
                h5i_core::pr::post_comment(&workdir, number, &body)?;
            }
            PrCommands::Body { limit, style } => {
                let workdir = std::env::current_dir()?;
                let body = h5i_core::pr::render_body_with_style(&workdir, limit, style.into())?;
                println!("{}", body);
            }
        },

        Commands::Init => {
            let repo = H5iRepository::open(".")?;
            println!(
                "{} {} at {}",
                SUCCESS,
                style("h5i sidecar initialized").green().bold(),
                style(repo.h5i_path().display()).dim()
            );

            let workdir = std::env::current_dir()?;
            match write_claude_instructions(&workdir) {
                Ok(()) => println!(
                    "{} {} (imported via {})",
                    SUCCESS,
                    style("Claude instructions written to .claude/h5i.md").green(),
                    style("CLAUDE.md").yellow()
                ),
                Err(e) => println!(
                    "{} Could not write Claude instructions: {}",
                    style("warn:").yellow(),
                    e
                ),
            }
            match write_codex_instructions(&workdir) {
                Ok(()) => println!(
                    "{} {}",
                    SUCCESS,
                    style("Codex instructions written to AGENTS.md").green()
                ),
                Err(e) => println!(
                    "{} Could not write Codex instructions: {}",
                    style("warn:").yellow(),
                    e
                ),
            }

            println!();
            println!("  {}", style("Quick-start:").bold());
            println!(
                "    {}  capture AI provenance on every commit",
                style("h5i commit -m \"…\" --prompt \"…\" --agent <claude-code|codex>").cyan()
            );
            println!(
                "    {}  snapshot agent memory after a session",
                style("h5i memory snapshot [--agent <claude-code|codex>]").cyan()
            );
            println!(
                "    {}  push all h5i data to your remote",
                style("h5i push").cyan()
            );
            println!();
            println!(
                "  {} h5i stores metadata in {} and {}.",
                style("Note:").dim(),
                style("refs/h5i/notes").yellow(),
                style("refs/h5i/memory").yellow()
            );
            println!(
                "  {} These refs are NOT included in a plain {}.",
                style("     ").dim(),
                style("git push").yellow()
            );
            println!(
                "  {} Run {} (or see README §9) to share them with your team.",
                style("     ").dim(),
                style("h5i push").bold()
            );
        }

        Commands::Commit {
            message,
            prompt,
            model,
            agent,
            tests,
            test_results,
            test_cmd,
            ast,
            audit,
            force,
            caused_by,
            decisions: decisions_file,
            add: add_paths,
        } => {
            let repo = H5iRepository::open(".")?;
            let sig = repo.git().signature()?; // Fetch system-default Git signature

            // Stage any paths passed via --add before the nothing-staged guard.
            if let Some(ref paths) = add_paths {
                if !paths.is_empty() {
                    let mut idx = repo.git().index()?;
                    for p in paths {
                        idx.add_path(p.as_path())?;
                    }
                    idx.write()?;
                }
            }

            // Refuse to commit if nothing is staged — guide the caller to git add first.
            {
                let idx = repo.git().index()?;
                let head_empty = repo.git().head().is_err(); // true on first commit
                let staged = if head_empty {
                    !idx.is_empty()
                } else {
                    let head_tree = repo.git().head()?.peel_to_tree()?;
                    let diff = repo.git().diff_tree_to_index(Some(&head_tree), Some(&idx), None)?;
                    diff.deltas().len() > 0
                };
                if !staged {
                    eprintln!(
                        "{} Nothing staged. Stage the files you want to commit first:\n\n  {}\n\nThen re-run {}.",
                        ERROR,
                        style("git add <file> …").cyan(),
                        style("h5i commit").cyan(),
                    );
                    std::process::exit(1);
                }
            }

            // Resolution order: CLI flag > environment variable > pending_context.json
            let pending = repo.read_pending_context()?;
            let prompt = prompt
                .or_else(|| std::env::var("H5I_PROMPT").ok())
                .or_else(|| pending.as_ref().and_then(|c| c.prompt.clone()));
            let model = model
                .or_else(|| std::env::var("H5I_MODEL").ok())
                .or_else(|| pending.as_ref().and_then(|c| c.model.clone()));
            let agent = agent
                .or_else(|| std::env::var("H5I_AGENT_ID").ok())
                .or_else(|| pending.as_ref().and_then(|c| c.agent_id.clone()));

            if audit {
                let report = repo.verify_integrity(prompt.as_deref(), &message)?;

                // Print a header line based on the overall level.
                match report.level {
                    IntegrityLevel::Violation => println!(
                        "{} {} {}",
                        ERROR,
                        style("INTEGRITY VIOLATION").red().bold(),
                        style(format!("(score: {:.2})", report.score)).dim()
                    ),
                    IntegrityLevel::Warning => println!(
                        "{} {} {}",
                        WARN,
                        style("INTEGRITY WARNING").yellow().bold(),
                        style(format!("(score: {:.2})", report.score)).dim()
                    ),
                    IntegrityLevel::Valid => {
                        println!("{} {}", SUCCESS, style("Integrity check passed.").green());
                    }
                }

                // Print each finding with its rule ID and severity colour.
                for f in &report.findings {
                    let (bullet, label) = match f.severity {
                        Severity::Violation => (
                            style("✖").red().bold(),
                            style(format!("[{}]", f.rule_id)).red().bold(),
                        ),
                        Severity::Warning => (
                            style("⚠").yellow().bold(),
                            style(format!("[{}]", f.rule_id)).yellow().bold(),
                        ),
                        Severity::Info => (
                            style("ℹ").cyan(),
                            style(format!("[{}]", f.rule_id)).cyan(),
                        ),
                    };
                    println!("  {} {} {}", bullet, label, f.detail);
                }

                if matches!(report.level, IntegrityLevel::Violation) && !force {
                    println!(
                        "\n{} Commit aborted. Use {} to override.",
                        style("!").red(),
                        style("--force").bold()
                    );
                    return Ok(());
                }
            }

            let ai_meta = if prompt.is_some() || model.is_some() || agent.is_some() {
                Some(AiMetadata {
                    model_name: model.unwrap_or_else(|| "unknown".into()),
                    agent_id: agent.unwrap_or_else(|| "unknown".into()),
                    prompt: prompt.unwrap_or_else(|| "".into()),
                    usage: None,
                })
            } else {
                None
            };

            // ── Policy check ──────────────────────────────────────────────────
            // Run after ai_meta is constructed so path rules can inspect it.
            {
                let workdir = repo
                    .git()
                    .workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                if let Ok(Some(cfg)) = h5i_core::policy::load_policy(&workdir) {
                    // Collect staged file paths from the git index.
                    let staged_files: Vec<String> = {
                        let mut idx = repo.git().index()?;
                        let _ = idx.read(true);
                        idx.iter()
                            .map(|e| String::from_utf8_lossy(&e.path).to_string())
                            .collect()
                    };

                    let input = h5i_core::policy::CommitCheckInput {
                        message: &message,
                        ai_meta: ai_meta.as_ref(),
                        staged_files: &staged_files,
                        audit_passed: audit,
                    };
                    let violations = h5i_core::policy::check_commit(&cfg, &input);
                    if !violations.is_empty() {
                        let has_error = violations
                            .iter()
                            .any(|v| v.severity == h5i_core::policy::ViolationSeverity::Error);
                        let label = cfg.commit.label.as_deref().unwrap_or("policy");
                        println!(
                            "{} {} {}",
                            if has_error { ERROR } else { WARN },
                            style(format!("Policy violation ({})", label))
                                .red()
                                .bold(),
                            style(format!("({} rule(s) failed)", violations.len())).dim()
                        );
                        h5i_core::policy::print_violations(&violations);
                        if has_error && !force {
                            println!(
                                "\n{} Commit aborted by policy. Use {} to override.",
                                style("!").red(),
                                style("--force").bold()
                            );
                            return Ok(());
                        }
                    }
                }
            }

            // Resolve TestSource — priority:
            //   1. --test-results <file>
            //   2. H5I_TEST_RESULTS env var (path to a JSON file)
            //   3. --test-cmd <cmd>
            //   4. --tests + H5I_TEST_CMD env var (run configured command)
            //   5. --tests alone (scan staged files for markers)
            //   6. Nothing
            let env_results = std::env::var("H5I_TEST_RESULTS").ok();
            let env_test_cmd = std::env::var("H5I_TEST_CMD").ok();
            let test_source = if let Some(ref path) = test_results {
                let metrics = repo.load_test_results_from_file(path)?;
                TestSource::Provided(metrics)
            } else if let Some(ref env_path) = env_results {
                let metrics = repo.load_test_results_from_file(std::path::Path::new(env_path))?;
                TestSource::Provided(metrics)
            } else if let Some(ref cmd) = test_cmd {
                println!(
                    "{} Running test command: {}",
                    style("▶").cyan(),
                    style(cmd).yellow()
                );
                let metrics = repo.run_test_command(cmd)?;
                let passing = metrics.is_passing();
                let icon = if passing { style("✔").green() } else { style("✖").red() };
                if let Some(ref s) = metrics.summary {
                    println!("  {} {}", icon, style(s).dim());
                }
                TestSource::Provided(metrics)
            } else if tests {
                if let Some(ref cmd) = env_test_cmd {
                    // --tests + H5I_TEST_CMD: actually run the test suite
                    println!(
                        "{} Running test command (H5I_TEST_CMD): {}",
                        style("▶").cyan(),
                        style(cmd).yellow()
                    );
                    let metrics = repo.run_test_command(cmd)?;
                    let passing = metrics.is_passing();
                    let icon = if passing { style("✔").green() } else { style("✖").red() };
                    if let Some(ref s) = metrics.summary {
                        println!("  {} {}", icon, style(s).dim());
                    } else {
                        let status = if passing { "passed" } else { "failed" };
                        println!("  {} exit code: {}", icon,
                            metrics.exit_code.map(|c| c.to_string()).unwrap_or_else(|| status.into()));
                    }
                    TestSource::Provided(metrics)
                } else {
                    // Fallback: scan staged files for marker blocks
                    TestSource::ScanMarkers
                }
            } else {
                TestSource::None
            };

            // Build a real language-aware AST parser closure.
            let parser_box = repo.make_ast_parser();
            type AstParser<'a> = &'a dyn Fn(&std::path::Path) -> Option<String>;
            let ast_parser: Option<AstParser> = if ast {
                Some(parser_box.as_ref())
            } else {
                None
            };

            let caused_by = caused_by.unwrap_or_default();

            // Load structured design decisions from JSON file if provided.
            let decisions: Vec<Decision> = if let Some(ref path) = decisions_file {
                let raw = std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("--decisions: cannot read {}: {}", path.display(), e))?;
                serde_json::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("--decisions: invalid JSON in {}: {}", path.display(), e))?
            } else {
                vec![]
            };

            let oid = repo.commit(&message, &sig, &sig, ai_meta, test_source, ast_parser, caused_by, decisions)?;
            repo.clear_pending_context()?;
            println!(
                "{} {} {}",
                SUCCESS,
                style("h5i Commit Created:").green(),
                style(oid).magenta().bold()
            );

            // Auto-snapshot the context workspace state linked to this git commit.
            let workdir = repo
                .git()
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            if ctx::is_initialized(&workdir) {
                if let Err(e) = ctx::snapshot_for_commit(&workdir, &oid.to_string()) {
                    eprintln!(
                        "{} context snapshot failed: {e}",
                        style("warn:").yellow()
                    );
                } else {
                    println!(
                        "  {} context snapshot linked to {}",
                        style("◈").cyan().dim(),
                        style(&oid.to_string()[..8]).dim()
                    );
                }
            }
        }

        Commands::Log { limit, ancestry } => {
            let repo = H5iRepository::open(".")?;

            if let Some(spec) = ancestry {
                // ── Prompt ancestry mode ──────────────────────────────────────
                // Parse "file:line" spec.
                let (file_part, line_part) = spec
                    .rsplit_once(':')
                    .ok_or_else(|| anyhow::anyhow!(
                        "--ancestry expects FILE:LINE format, e.g. src/model.py:42"
                    ))?;
                let line_number: usize = line_part.parse().map_err(|_| {
                    anyhow::anyhow!("--ancestry: '{}' is not a valid line number", line_part)
                })?;
                let path = std::path::Path::new(file_part);

                println!(
                    "\n{} {}\n",
                    style("──").dim(),
                    style(format!("Prompt ancestry for {}:{}", file_part, line_number))
                        .cyan()
                        .bold(),
                );

                let chain = repo.blame_ancestry(path, line_number)?;

                if chain.is_empty() {
                    println!("  (no ancestry found — file may be untracked or line out of range)");
                } else {
                    let total = chain.len();
                    for (i, entry) in chain.iter().enumerate() {
                        let depth = total - i;
                        let short_oid = &entry.commit_id[..8];
                        let ts = entry.timestamp.format("%Y-%m-%d %H:%M UTC");
                        let agent_label = match &entry.agent {
                            Some(a) => format!("AI:{a}"),
                            None => "Human".to_string(),
                        };

                        println!(
                            "  [{}] {}  {} · {}",
                            style(format!("{depth} of {total}")).dim(),
                            style(short_oid).magenta(),
                            style(&entry.author).cyan(),
                            style(ts).dim(),
                        );

                        // The line content at this point in history
                        println!(
                            "       {}  {}",
                            style("line:").dim(),
                            style(&entry.line_content).italic(),
                        );

                        match &entry.prompt {
                            Some(p) => println!(
                                "       {}  {}",
                                style("prompt:").dim(),
                                style(format!("\"{}\"", truncate(p, 80))).yellow().italic(),
                            ),
                            None => println!(
                                "       {}  {} ({})",
                                style("prompt:").dim(),
                                style("(none recorded)").dim(),
                                style(agent_label).dim(),
                            ),
                        }
                        println!();
                    }
                }
            } else {
                repo.print_log(limit)?;
            }
        }

        Commands::Blame { file, mode, show_prompt } => {
            let repo = H5iRepository::open(".")?;
            let blame_mode = if mode.to_lowercase() == "ast" {
                BlameMode::Ast
            } else {
                BlameMode::Line
            };

            let results = repo.blame(&file, blame_mode)?;
            println!(
                "{}",
                style(format!(
                    "{:<4} {:<8} {:<15} | {}",
                    "STAT", "COMMIT", "AUTHOR/AGENT", "CONTENT"
                ))
                .bold()
                .underlined()
            );

            // Track the previous commit id so we can print the prompt once per
            // commit boundary rather than once per line.
            let mut prev_commit: Option<String> = None;

            for r in &results {
                let test_indicator = match r.test_passed {
                    Some(true) => "✅",
                    Some(false) => "❌",
                    None => "  ",
                };
                let semantic_indicator = if r.is_semantic_change { "✨" } else { "  " };

                // Print prompt annotation when the commit changes (show_prompt mode).
                if show_prompt {
                    let commit_changed = prev_commit.as_deref() != Some(&r.commit_id);
                    if commit_changed {
                        if let Some(ref prompt) = r.prompt {
                            // Blank separator + indented prompt label
                            println!(
                                "           {:<15}   {}",
                                "",
                                style(format!("prompt: \"{}\"", truncate(prompt, 72)))
                                    .italic()
                                    .yellow()
                            );
                        }
                        prev_commit = Some(r.commit_id.clone());
                    }
                }

                println!(
                    "{} {} {} {:<15} | {}",
                    test_indicator,
                    semantic_indicator,
                    style(&r.commit_id[..8]).dim(),
                    style(&r.agent_info).blue(),
                    r.line_content
                );
            }
        }

        Commands::Diff { file, from, to } => {
            let repo = H5iRepository::open(".")?;

            let from_oid = from.map(|s| Oid::from_str(&s)).transpose()?;
            let to_oid = to.map(|s| Oid::from_str(&s)).transpose()?;

            let label = match (&from_oid, &to_oid) {
                (None, None) => "HEAD → working tree".to_string(),
                (Some(f), None) => format!("{}… → working tree", &f.to_string()[..8]),
                (None, Some(t)) => format!("HEAD → {}…", &t.to_string()[..8]),
                (Some(f), Some(t)) => format!("{}… → {}…", &f.to_string()[..8], &t.to_string()[..8]),
            };

            println!(
                "{} {} {} {}",
                LOOKING,
                style("Computing structural diff for").cyan().bold(),
                style(file.display()).yellow(),
                style(format!("({label})")).dim(),
            );

            let ast_diff = repo.diff_ast(&file, from_oid, to_oid)?;
            ast_diff.print_stylish(&file.to_string_lossy());
        }

        Commands::Rollback {
            intent,
            limit,
            dry_run,
            yes,
        } => {
            let repo = H5iRepository::open(".")?;

            println!(
                "{} {} \"{}\" {} {} commits",
                LOOKING,
                style("Searching for intent:").cyan().bold(),
                style(&intent).yellow(),
                style("across last").dim(),
                style(limit).dim(),
            );

            let commits = repo.list_ai_commits(limit)?;
            if commits.is_empty() {
                println!("{} No commits found in this repository.", WARN);
                return Ok(());
            }

            // Semantic search via Claude, or fall back to keyword matching.
            let matched_oid: Option<String> = if let Some(claude) = AnthropicClient::from_env() {
                println!(
                    "{} {} {}",
                    STEP,
                    style("Using Claude for semantic search").dim(),
                    style(format!("({})", claude.model())).dim(),
                );
                claude.find_matching_commit(&commits, &intent)?
            } else {
                println!(
                    "{} {} {}",
                    WARN,
                    style("ANTHROPIC_API_KEY not set — using keyword fallback.").yellow(),
                    style("Set it for semantic search.").dim(),
                );
                keyword_search(&commits, &intent).map(|c| c.oid.clone())
            };

            let oid_str = match matched_oid {
                Some(o) => o,
                None => {
                    println!(
                        "{} No commit found matching: \"{}\"",
                        WARN,
                        style(&intent).yellow()
                    );
                    return Ok(());
                }
            };

            let oid = Oid::from_str(&oid_str)?;
            let commit = repo.git().find_commit(oid)?;
            let record = repo.load_h5i_record(oid).ok();

            println!("\n{}", style("Matched commit:").bold().underlined());
            println!(
                "  {} {}",
                style("commit").yellow(),
                style(&oid_str).magenta().bold()
            );
            println!(
                "  {:<10} {}",
                style("Message:").dim(),
                commit.message().unwrap_or("").trim()
            );
            if let Some(ref r) = record {
                if let Some(ref ai) = r.ai_metadata {
                    if !ai.agent_id.is_empty() {
                        println!(
                            "  {:<10} {} {}",
                            style("Agent:").dim(),
                            style(&ai.agent_id).cyan(),
                            style(format!("({})", ai.model_name)).dim(),
                        );
                    }
                    if !ai.prompt.is_empty() {
                        println!(
                            "  {:<10} \"{}\"",
                            style("Prompt:").dim(),
                            style(&ai.prompt).italic()
                        );
                    }
                }
                println!(
                    "  {:<10} {}",
                    style("Date:").dim(),
                    r.timestamp.format("%Y-%m-%d %H:%M UTC")
                );
            }

            if dry_run {
                println!(
                    "\n{} {}",
                    style("--dry-run").bold(),
                    style("No changes made.").dim()
                );
                return Ok(());
            }

            // Warn if later commits causally depend on this one
            let dependents = repo.causal_dependents(oid, 200);
            if !dependents.is_empty() {
                println!(
                    "\n{} {} later commit{} causally depend{} on this one:",
                    style("⚠ Warning:").yellow().bold(),
                    dependents.len(),
                    if dependents.len() == 1 { "" } else { "s" },
                    if dependents.len() == 1 { "s" } else { "" },
                );
                for (dep_oid, dep_msg) in &dependents {
                    println!(
                        "  {} {} {}",
                        style("→").yellow(),
                        style(&dep_oid.to_string()[..8]).magenta(),
                        style(format!("\"{}\"", dep_msg)).dim().italic()
                    );
                }
                if !yes {
                    print!("\nContinue anyway? [y/N] ");
                    use std::io::Write as _;
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    if !input.trim().eq_ignore_ascii_case("y") {
                        println!("{} Aborted.", style("!").dim());
                        return Ok(());
                    }
                }
            }

            if !yes {
                print!("\n{} [y/N] ", style("Revert this commit?").bold());
                use std::io::Write as _;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("{} Aborted.", style("!").dim());
                    return Ok(());
                }
            }

            let new_oid = repo.revert_commit(oid)?;
            println!(
                "{} {} {}",
                SUCCESS,
                style("Revert commit created:").green(),
                style(new_oid).magenta().bold()
            );
        }

        Commands::Rewind { sha, dry_run, force } => {
            let repo = H5iRepository::open(".")?;

            // Resolve the SHA to show a friendly preview before touching anything.
            let target_obj = repo.git().revparse_single(&sha)
                .map_err(|_| anyhow::anyhow!("'{}' does not resolve to a git object", sha))?;
            let target_commit = target_obj.peel_to_commit()
                .map_err(|_| anyhow::anyhow!("'{}' is not a commit", sha))?;
            let short_sha = &target_commit.id().to_string()[..8];
            let msg = target_commit.message().unwrap_or("").lines().next().unwrap_or("").trim();

            println!(
                "{} {} {} {}",
                LOOKING,
                style("Rewinding to:").bold(),
                style(short_sha).magenta(),
                style(format!("\"{}\"", msg)).italic().dim(),
            );

            let (shadow_ref, changed) = repo.rewind(&sha, force, dry_run)?;

            if dry_run {
                println!(
                    "\n  {} {} file{} would change:\n",
                    style("◈").dim(),
                    style(changed.len()).cyan().bold(),
                    if changed.len() == 1 { "" } else { "s" }
                );
                for (path, kind) in &changed {
                    let symbol = match *kind {
                        "added"    => style("+").green(),
                        "deleted"  => style("-").red(),
                        _          => style("~").yellow(),
                    };
                    println!("    {} {}", symbol, style(path).dim());
                }
                println!(
                    "\n{} {}",
                    style("--dry-run").bold(),
                    style("No changes made.").dim()
                );
                return Ok(());
            }

            if let Some(ref r) = shadow_ref {
                println!(
                    "  {} Dirty state saved → {}",
                    style("◈").dim(),
                    style(r).cyan(),
                );
                println!(
                    "    {} {}",
                    style("Recover with:").dim(),
                    style(format!("git checkout {} -- .", r)).cyan(),
                );
            }

            let added   = changed.iter().filter(|(_, k)| *k == "added").count();
            let deleted = changed.iter().filter(|(_, k)| *k == "deleted").count();
            let modded  = changed.len() - added - deleted;

            println!(
                "\n{} {} file{} restored  {} added  {} modified  {} deleted",
                SUCCESS,
                style(changed.len()).green().bold(),
                if changed.len() == 1 { "" } else { "s" },
                style(added).green(),
                style(modded).yellow(),
                style(deleted).red(),
            );
            println!(
                "  {} HEAD stays at {} — review with {} before committing.",
                style("◈").dim(),
                style(repo.git().head()?.peel_to_commit()
                    .map(|c| c.id().to_string()[..8].to_string())
                    .unwrap_or_default()).magenta(),
                style("git diff HEAD").cyan(),
            );

            // Record the rewind in the context workspace if one exists.
            let workdir = repo.git().workdir().map(|p| p.to_path_buf());
            if let Some(ref wd) = workdir {
                if ctx::is_initialized(wd) {
                    let _ = ctx::append_log(
                        wd,
                        "ACT",
                        &format!("h5i rewind: restored working tree to {short_sha} \"{msg}\""),
                        false,
                    );
                }
            }
        }

        Commands::Notes { action } => match action {
            NotesCommands::Analyze { session, commit, since } => {
                let repo = H5iRepository::open(".")?;
                let workdir = repo
                    .git()
                    .workdir()
                    .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                    .to_path_buf();
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                let jsonl_path = match session {
                    Some(p) => p,
                    None => match session_log::find_latest_session(&workdir) {
                        Some(p) => {
                            println!("{} {}", STEP,
                                style(format!("Auto-detected session: {}", p.display())).dim());
                            p
                        }
                        None => {
                            println!("{} No Claude Code session found in ~/.claude/projects/.", WARN);
                            println!("  {} Use {} to specify a session file.",
                                style("ℹ").blue(),
                                style("h5i notes analyze --session <path>").bold());
                            return Ok(());
                        }
                    },
                };

                // Resolve --since to a UTC timestamp so analyze_session can filter events.
                let since_time: Option<chrono::DateTime<chrono::Utc>> = match since {
                    None => None,
                    Some(ref sha) => {
                        let oid = git2::Oid::from_str(sha)
                            .or_else(|_| -> Result<git2::Oid, git2::Error> {
                                repo.git()
                                    .revparse_single(sha)?
                                    .peel_to_commit()
                                    .map(|c| c.id())
                            })
                            .map_err(|e| anyhow::anyhow!("--since: cannot resolve '{}': {}", sha, e))?;
                        let c = repo.git().find_commit(oid)
                            .map_err(|e| anyhow::anyhow!("--since: {}", e))?;
                        let secs = c.time().seconds();
                        chrono::DateTime::from_timestamp(secs, 0)
                            .inspect(|dt| {
                                println!("{} Filtering session to events after {} ({})",
                                    STEP,
                                    style(&sha[..8.min(sha.len())]).magenta(),
                                    style(dt.format("%Y-%m-%d %H:%M UTC")).dim());
                            })
                    }
                };

                println!("{} {} → commit {}", STEP,
                    style("Analyzing session log").cyan().bold(),
                    style(&oid_str[..8.min(oid_str.len())]).magenta());
                let analysis = session_log::analyze_session(&jsonl_path, since_time)?;
                session_log::save_analysis(&repo.h5i_root, &oid_str, &analysis)?;
                println!("{} {} messages · {} tool calls · {} edited · {} consulted",
                    SUCCESS,
                    style(analysis.message_count).cyan(),
                    style(analysis.tool_call_count).cyan(),
                    style(analysis.footprint.edited.len()).green(),
                    style(analysis.footprint.consulted.len()).yellow());
                println!("  {} Run {} to inspect results.",
                    style("ℹ").blue(),
                    style(format!("h5i notes show {}", &oid_str[..8])).bold());
            }

            NotesCommands::Show { commit } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_footprint(&analysis);
                        session_log::print_causal_chain(&analysis);
                    }
                }
            }

            NotesCommands::Uncertainty { commit, file } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for commit {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_uncertainty(&analysis, file.as_deref());
                    }
                }
            }

            NotesCommands::Churn { limit } => {
                let repo = H5iRepository::open(".")?;
                let mut churn = session_log::aggregate_churn(&repo.h5i_root);
                churn.truncate(limit);
                if churn.is_empty() {
                    println!(
                        "{} No churn data yet. Run {} after sessions to build history.",
                        WARN,
                        style("h5i notes analyze").bold()
                    );
                } else {
                    session_log::print_churn(&churn);
                }
            }

            NotesCommands::Graph { limit, mode } => {
                let repo = H5iRepository::open(".")?;
                let analyze = mode.to_lowercase() == "analyze";
                if analyze {
                    if std::env::var("ANTHROPIC_API_KEY").is_err() {
                        println!(
                            "{} {} — set {} to enable Claude analysis.",
                            WARN,
                            style("ANTHROPIC_API_KEY not set, falling back to stored prompts").yellow(),
                            style("ANTHROPIC_API_KEY").bold(),
                        );
                    } else {
                        println!(
                            "{} {} for {} commits…",
                            STEP,
                            style("Calling Claude to generate intent labels").cyan().bold(),
                            style(limit).cyan(),
                        );
                    }
                }
                repo.print_intent_graph(limit, analyze)?;
            }

            NotesCommands::Review { limit, min_score, json } => {
                let repo = H5iRepository::open(".")?;
                let points = repo.suggest_review_points(limit, min_score)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&points)?);
                } else if points.is_empty() {
                    println!(
                        "{} No commits exceeded the review threshold (min_score={:.2}) in the last {} commits.",
                        SUCCESS, min_score, limit
                    );
                } else {
                    println!(
                        "{} — {} commit{} flagged (scanned {}, min_score={:.2})",
                        style("Suggested Review Points").bold().underlined(),
                        style(points.len()).yellow().bold(),
                        if points.len() == 1 { "" } else { "s" },
                        limit, min_score
                    );
                    println!("{}", style("─".repeat(62)).dim());
                    for (i, rp) in points.iter().enumerate() {
                        let filled = (rp.score * 10.0).round() as usize;
                        let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);
                        let score_color = if rp.score >= 0.7 {
                            style(format!("{:.2}", rp.score)).red().bold()
                        } else if rp.score >= 0.45 {
                            style(format!("{:.2}", rp.score)).yellow().bold()
                        } else {
                            style(format!("{:.2}", rp.score)).cyan().bold()
                        };
                        println!(
                            "\n  {} {}  score {}  {}",
                            style(format!("#{}", i + 1)).dim(),
                            style(&rp.short_oid).magenta().bold(),
                            score_color,
                            style(&bar).dim()
                        );
                        println!("     {} · {}", style(&rp.author).blue(),
                            style(rp.timestamp.format("%Y-%m-%d %H:%M UTC")).dim());
                        println!("     {}", style(&rp.message).bold());
                        for trigger in &rp.triggers {
                            let bullet = match trigger.rule_id.as_str() {
                                "TEST_REGRESSION" | "INTEGRITY_VIOLATION" => style("⬦").red(),
                                "LARGE_DIFF" | "WIDE_IMPACT" => style("⬦").yellow(),
                                _ => style("⬦").cyan(),
                            };
                            println!("       {} {:<18}  {}", bullet,
                                style(&trigger.rule_id).bold(), style(&trigger.detail).dim());
                        }
                    }
                    println!("\n{}", style("─".repeat(62)).dim());
                }
            }

            NotesCommands::Omissions { commit, file } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        session_log::print_omissions(&analysis, file.as_deref());
                    }
                }
            }

            NotesCommands::Coverage { commit, max_ratio } => {
                let repo = H5iRepository::open(".")?;
                let oid_str = match commit {
                    Some(ref s) => s.clone(),
                    None => repo.git().head()?.peel_to_commit()?.id().to_string(),
                };
                match session_log::load_analysis(&repo.h5i_root, &oid_str)? {
                    None => println!(
                        "{} No session analysis for {}. Run {} first.",
                        WARN,
                        style(&oid_str[..8.min(oid_str.len())]).magenta(),
                        style("h5i notes analyze").bold()
                    ),
                    Some(analysis) => {
                        let short = &oid_str[..8.min(oid_str.len())];
                        println!(
                            "\n{} {}\n",
                            style("──").dim(),
                            style(format!("Attention Coverage — {}", short)).cyan().bold()
                        );
                        let cov: Vec<_> = analysis
                            .coverage
                            .iter()
                            .filter(|c| c.read_before_edit_ratio <= max_ratio)
                            .collect();
                        if cov.is_empty() {
                            println!(
                                "  {} All edited files were read before modification.",
                                style("✔").green()
                            );
                        } else {
                            println!(
                                "  {:<42}  {:>8}  {:>12}  {}",
                                style("File").bold(),
                                style("Edits").bold(),
                                style("Coverage").bold(),
                                style("Blind edits").bold(),
                            );
                            println!("  {}", style("─".repeat(74)).dim());
                            for fc in &cov {
                                let pct = (fc.read_before_edit_ratio * 100.0) as u32;
                                let blind = fc.blind_edit_count;
                                let ratio_style = if blind == 0 {
                                    style(format!("{:>10}%", pct)).green()
                                } else if fc.read_before_edit_ratio >= 0.5 {
                                    style(format!("{:>10}%", pct)).yellow()
                                } else {
                                    style(format!("{:>10}%", pct)).red().bold()
                                };
                                let blind_style = if blind == 0 {
                                    style(format!("{:>11}", 0)).dim()
                                } else {
                                    style(format!("{:>11}", blind)).red().bold()
                                };
                                println!(
                                    "  {:<42}  {:>8}  {}  {}",
                                    style(truncate(&fc.file, 42)).cyan(),
                                    fc.edit_turns.len(),
                                    ratio_style,
                                    blind_style,
                                );
                            }
                            println!("\n  {} file(s) with blind edits (no prior Read).",
                                style(cov.iter().filter(|c| c.blind_edit_count > 0).count()).bold());
                        }
                        println!();
                    }
                }
            }
        },

        Commands::Hook(HookCommands::Setup) => {
            let hook_script = r#"#!/usr/bin/env bash
# h5i Claude Code hook — writes the user prompt to .git/.h5i/pending_context.json
# so that `h5i commit` can pick it up automatically without --prompt.
set -euo pipefail
GIT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
H5I_DIR="$GIT_ROOT/.git/.h5i"
[ -d "$H5I_DIR" ] || exit 0
jq -c '{
  prompt: .prompt,
  model: (env.H5I_MODEL // "claude-sonnet-4-6"),
  agent_id: (env.H5I_AGENT_ID // "claude-code"),
  session_id: .session_id
}' > "$H5I_DIR/pending_context.json"
"#;

            println!("{}", style("── Step 0: Installl `jq` ──").bold());
            println!(
                "If you don't have {} installed, run the following command:\n\n{}\n",
                style("jq").yellow(),
                style("apt install jq").dim()
            );

            println!("{}", style("── Step 1: Save hook script ──").bold());
            println!(
                "Save the following script to {} and make it executable:\n",
                style("~/.claude/hooks/h5i-capture-prompt.sh").yellow()
            );
            println!("{}", style(hook_script).dim());

            println!("{}", style("── Step 2: Add to ~/.claude/settings.json ──").bold());
            println!(
                "Add (or merge) the {} block into your {}:\n",
                style("hooks").yellow(),
                style("~/.claude/settings.json").yellow()
            );
            println!(
                "{}",
                style(
                    r#"{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/h5i-capture-prompt.sh"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook session-start"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook run"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "h5i hook stop"
          }
        ]
      }
    ]
  }
}"#
                )
                .dim()
            );
            println!();
            println!(
                "  {} — injects prior context into every new session automatically",
                style("SessionStart").yellow()
            );
            println!(
                "  {} — auto-traces OBSERVE for every Read, ACT for every Edit/Write",
                style("PostToolUse").yellow()
            );
            println!(
                "  {} — mines THINK / NOTE entries from your session transcript and",
                style("Stop").yellow()
            );
            println!(
                "         auto-checkpoints the context workspace milestone.",
            );
            println!(
                "         You never have to call `h5i context trace` by hand."
            );

            println!("{}", style("── Step 3: Register the MCP server ──").bold());
            println!(
                "Add the {} block to {} so Claude Code can call h5i tools natively:\n",
                style("mcpServers").yellow(),
                style("~/.claude/settings.json").yellow()
            );
            println!(
                "{}",
                style(
                    r#"{
  "mcpServers": {
    "h5i": {
      "command": "h5i",
      "args": ["mcp"]
    }
  }
}"#
                )
                .dim()
            );
            println!(
                "\nOnce registered, Claude Code gains native access to h5i tools\n\
                 (h5i_log, h5i_blame, h5i_context_trace, h5i_notes_show, etc.)\n\
                 without needing shell commands.\n"
            );

            println!(
                "\n{} Set {} and",
                style("Tip:").bold(),
                style("H5I_MODEL").yellow(),
            );
            println!(
                "    {} in your shell profile to override the defaults captured by the hook.",
                style("H5I_AGENT_ID").yellow()
            );
            println!(
                "\n{} also work without hooks — {} / H5I_MODEL / H5I_AGENT_ID are read automatically at commit time.",
                style("Env vars").bold(),
                style("H5I_PROMPT").yellow()
            );
        }

        Commands::Hook(HookCommands::Run) => {
            use std::io::Read as _;
            // Read JSON from stdin (Claude Code sends PostToolUse payload here).
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).unwrap_or(0);
            if raw.trim().is_empty() {
                return Ok(());
            }
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return Ok(());
            };
            let tool = data.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
            let inp = data.get("tool_input").cloned().unwrap_or_default();
            let file_path = inp.get("file_path").and_then(|v| v.as_str()).unwrap_or("");

            if file_path.is_empty() || !matches!(tool, "Edit" | "Write" | "Read") {
                return Ok(());
            }

            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            // Only emit traces when inside a git repo that has h5i context initialized.
            let has_ctx = match git2::Repository::discover(&workdir) {
                Ok(r) => r.find_reference("refs/h5i/context/main").is_ok(),
                Err(_) => false,
            };
            if !has_ctx {
                return Ok(());
            }

            // Relativize the path against the workdir for readability.
            let display_path = std::path::Path::new(file_path)
                .strip_prefix(&workdir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| {
                    std::path::Path::new(file_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| file_path.to_owned())
                });

            let (kind, msg) = match tool {
                "Edit" => ("ACT",     format!("edited {display_path}")),
                "Write" => ("ACT",    format!("wrote {display_path}")),
                "Read" => ("OBSERVE", format!("read {display_path}")),
                _ => return Ok(()),
            };

            // Emit the trace; ignore errors so we never block Claude Code.
            let _ = ctx::append_log(&workdir, kind, &msg, false);

            // Feature 1: on Read, inject prior reasoning about this file into
            // Claude's context window (Claude Code surfaces hook stdout to the model).
            if tool == "Read" {
                if let Ok(rel) = ctx::relevant(&workdir, file_path) {
                    let has = !rel.commit_mentions.is_empty() || !rel.trace_mentions.is_empty();
                    if has {
                        println!("[h5i] Prior reasoning about {}:", display_path);
                        for m in &rel.commit_mentions {
                            println!("  [milestone] {m}");
                        }
                        for t in rel.trace_mentions.iter().take(5) {
                            println!("  {t}");
                        }
                        if !rel.cross_branch_mentions.is_empty() {
                            for c in rel.cross_branch_mentions.iter().take(2) {
                                println!("  [branch] {c}");
                            }
                        }
                    }
                }
            }
        }

        Commands::Hook(HookCommands::SessionStart) => {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            print_shared_context_prelude(&workdir);
        }

        Commands::Hook(HookCommands::Stop) => {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            // 1. Mine the Claude session JSONL for key decisions + omissions and
            //    emit them as THINK/NOTE trace entries. The agent never has to
            //    call `h5i context trace --kind THINK …` itself.
            match auto_derive_traces_from_claude_session(&workdir) {
                Ok(0) => {}
                Ok(n) => eprintln!(
                    "{} Auto-traced {} reasoning entries from Claude session.",
                    style("✔").green(),
                    n
                ),
                Err(e) => eprintln!(
                    "{} Auto-trace failed: {e}",
                    style("warn:").yellow()
                ),
            }
            // 2. Checkpoint the context workspace milestone.
            if let Err(e) = auto_checkpoint_context(&workdir, None) {
                eprintln!("{} Context checkpoint failed: {e}", style("warn:").yellow());
            }
        }

        Commands::Codex { action } => {
            let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match action {
                CodexCommands::Prelude => {
                    print_shared_context_prelude(&workdir);
                }
                CodexCommands::Sync => match codex::sync_context(&workdir)? {
                    Some(result) => println!(
                        "{} Synced Codex session {} ({} OBSERVE, {} ACT, {} new line{})",
                        SUCCESS,
                        style(&result.session_id).magenta(),
                        result.observed,
                        result.acted,
                        result.processed_lines,
                        if result.processed_lines == 1 { "" } else { "s" }
                    ),
                    None => println!(
                        "{} No Codex session found in ~/.codex/sessions for this repo.",
                        WARN
                    ),
                },
                CodexCommands::Finish { summary } => {
                    match codex::sync_context(&workdir)? {
                        Some(result) => println!(
                            "{} Synced Codex session {} ({} OBSERVE, {} ACT)",
                            SUCCESS,
                            style(&result.session_id).magenta(),
                            result.observed,
                            result.acted,
                        ),
                        None => println!(
                            "{} No Codex session found in ~/.codex/sessions for this repo.",
                            WARN
                        ),
                    }
                    auto_checkpoint_context(&workdir, summary.as_deref())?;
                }
            }
        }

        Commands::Serve { port } => {
            let repo = H5iRepository::open(".")?;
            let repo_path = repo
                .git()
                .workdir()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();

            println!(
                "{} {} on port {}",
                SUCCESS,
                style("Starting h5i dashboard").green().bold(),
                style(port).cyan()
            );
            println!(
                "  Open {} in your browser",
                style(format!("http://localhost:{}", port)).underlined().blue()
            );
            println!("  Press Ctrl+C to stop\n");

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(h5i_core::server::serve(repo_path, port))?;
        }

        Commands::Push { remote } => {
            let workdir = std::env::current_dir()?;

            println!(
                "{} {} to {}",
                STEP,
                style("Pushing all h5i refs").cyan().bold(),
                style(&remote).yellow()
            );

            use std::io::Write as _;

            // Pre-check whether a ref exists locally before invoking `git push`.
            // Skipping a missing ref with our own warning avoids two lines of
            // git stderr noise ("error: src refspec ... does not match any" +
            // "error: failed to push some refs") for the expected case where
            // the user simply hasn't generated that artifact yet.
            let ref_exists = |refname: &str| -> bool {
                std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "--quiet", refname])
                    .current_dir(&workdir)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };

            // Push one h5i ref. On missing ref, prints a yellow warning with
            // the hint command. On real push failure, lets git's stderr
            // through unchanged. Returns true iff the push actually ran and
            // succeeded — used downstream to gate the "Tip:" footer.
            let try_push = |refname: &str,
                            missing_hint: console::StyledObject<&str>,
                            missing_reason: &str|
             -> anyhow::Result<bool> {
                print!("  {} {} … ", style("→").dim(), style(refname).yellow());
                std::io::stdout().flush()?;
                if !ref_exists(refname) {
                    println!(
                        "{} ({} — run {})",
                        style("skipped").yellow(),
                        missing_reason,
                        missing_hint
                    );
                    return Ok(false);
                }
                let refspec = format!("+{}:{}", refname, refname);
                let status = std::process::Command::new("git")
                    .args(["push", &remote, &refspec])
                    .current_dir(&workdir)
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                if status.success() {
                    println!("{}", style("ok").green());
                    Ok(true)
                } else {
                    println!("{}", style("failed").red());
                    Ok(false)
                }
            };

            // Push h5i notes (AI provenance, test metrics, causal links)
            let notes_pushed = try_push(
                "refs/h5i/notes",
                style("h5i commit").bold(),
                "no AI-provenance commits yet",
            )?;

            // Push memory ref (Claude memory snapshots)
            try_push(
                memory::MEMORY_REF,
                style("h5i memory snapshot").bold(),
                "no memory snapshots yet",
            )?;

            // Push context workspace.
            //
            // Post-redesign: one ref per context branch under
            // `refs/h5i/context/<name>`. Use a wildcard refspec so every
            // branch syncs in a single git invocation. For backward compat,
            // also push the legacy single ref (`refs/h5i/context`) if it
            // still exists locally — older clients on the receiving side may
            // still expect it. Migration aside-name (`refs/h5i/context-legacy`)
            // is pushed too as a safety net for diagnosing rollbacks.
            let any_per_branch_ctx = std::process::Command::new("git")
                .args([
                    "for-each-ref",
                    "--count=1",
                    "--format=%(refname)",
                    "refs/h5i/context/",
                ])
                .current_dir(&workdir)
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            if any_per_branch_ctx {
                print!(
                    "  {} {} … ",
                    style("→").dim(),
                    style("refs/h5i/context/*").yellow()
                );
                std::io::stdout().flush()?;
                let status = std::process::Command::new("git")
                    .args([
                        "push",
                        &remote,
                        "+refs/h5i/context/*:refs/h5i/context/*",
                    ])
                    .current_dir(&workdir)
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                println!(
                    "{}",
                    if status.success() {
                        style("ok").green()
                    } else {
                        style("failed").red()
                    }
                );
            } else {
                println!(
                    "  {} {} … {} (no context workspace yet — run {})",
                    style("→").dim(),
                    style("refs/h5i/context/*").yellow(),
                    style("skipped").yellow(),
                    style("h5i context init").bold(),
                );
            }
            if ref_exists("refs/h5i/context") {
                try_push(
                    "refs/h5i/context",
                    style("(legacy)").dim(),
                    "(no legacy ref)",
                )?;
            }
            if ref_exists("refs/h5i/context-legacy") {
                try_push(
                    "refs/h5i/context-legacy",
                    style("(migration backup)").dim(),
                    "(no migration backup)",
                )?;
            }

            // Push AST blobs (refs/h5i/ast)
            try_push(
                "refs/h5i/ast",
                style("h5i commit --ast").bold(),
                "no AST snapshots yet",
            )?;

            // Bind to the original variable name so the existing "Tip:" footer
            // (gated on notes_status.success()) keeps working unchanged.
            let notes_status_success = notes_pushed;

            if notes_status_success {
                println!(
                    "\n{} To receive these refs on another machine:\n\
                    \n    git fetch {} refs/h5i/notes:refs/h5i/notes\
                    \n    git fetch {} refs/h5i/memory:refs/h5i/memory\
                    \n    git fetch {} 'refs/h5i/context/*:refs/h5i/context/*'\
                    \n    git fetch {} refs/h5i/ast:refs/h5i/ast\
                    \n\n  Or add fetch refspecs to .git/config (see README §9) so {} picks them up automatically.",
                    style("Tip:").bold(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style("git pull").bold()
                );
            }
        }

        Commands::Pull { remote, force } => {
            let workdir = std::env::current_dir()?;

            println!(
                "{} {} from {}",
                STEP,
                style("Pulling all h5i refs").cyan().bold(),
                style(&remote).yellow()
            );

            use std::io::Write as _;

            // Helper: run `git <args>` in the working dir, capturing output.
            let git = |args: &[&str]| -> std::io::Result<std::process::Output> {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&workdir)
                    .output()
            };

            // Helper: resolve a ref to its full SHA, or None if it doesn't exist.
            let resolve_ref = |refname: &str| -> Option<String> {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "--quiet", refname])
                    .current_dir(&workdir)
                    .output()
                    .ok()?;
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    None
                }
            };

            // Helper: is `ancestor` an ancestor of `descendant`?
            let is_ancestor = |ancestor: &str, descendant: &str| -> bool {
                std::process::Command::new("git")
                    .args(["merge-base", "--is-ancestor", ancestor, descendant])
                    .current_dir(&workdir)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };

            // Sync one h5i ref from the remote, choosing the safest action that
            // preserves local data:
            //
            //   missing on remote → skip
            //   no local copy     → install (fast install)
            //   identical         → up to date
            //   local ⊑ remote    → fast-forward
            //   remote ⊑ local    → keep local (we're ahead)
            //   diverged          → notes: union-merge; others: keep unless --force
            //
            // We always fetch into a per-call temp ref under refs/h5i/_incoming/
            // first so the remote's value can never overwrite the live local ref
            // implicitly — every ref update goes through `git update-ref` here.
            // The temp ref is deleted at the end of each call.
            //
            // Returns true iff the live local ref was changed by this call.
            let sync_one = |refname: &str| -> anyhow::Result<bool> {
                print!("  {} {} … ", style("→").dim(), style(refname).yellow());
                std::io::stdout().flush()?;

                let basename = refname.rsplit('/').next().unwrap_or("ref");
                let incoming = format!("refs/h5i/_incoming/{}", basename);

                // Always force-fetch into the temp ref. The temp ref is
                // private to this call, so this can never destroy user data;
                // it just guarantees we get the remote's latest into a known
                // local name we can compare against.
                let fetch_refspec = format!("+{}:{}", refname, incoming);
                let fetch = git(&["fetch", "--no-write-fetch-head", &remote, &fetch_refspec])?;

                if !fetch.status.success() {
                    let stderr = String::from_utf8_lossy(&fetch.stderr);
                    let missing = stderr.contains("couldn't find remote ref")
                        || stderr.contains("does not exist");
                    if missing {
                        println!(
                            "{} ({})",
                            style("skipped").yellow(),
                            style("not present on remote").dim()
                        );
                    } else {
                        println!("{}", style("failed").red());
                        eprint!("{}", stderr);
                    }
                    return Ok(false);
                }

                let local = resolve_ref(refname);
                let incoming_oid = match resolve_ref(&incoming) {
                    Some(oid) => oid,
                    None => {
                        println!("{}", style("failed").red());
                        eprintln!(
                            "internal: fetched {} but could not resolve {}",
                            refname, incoming
                        );
                        return Ok(false);
                    }
                };

                // Outcome decided per-branch; helper closures keep the match
                // arms readable without repeating the update-ref + report code.
                let install = |label: &str| -> anyhow::Result<bool> {
                    let st = git(&["update-ref", refname, &incoming_oid])?;
                    if !st.status.success() {
                        println!("{}", style("failed").red());
                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                        Ok(false)
                    } else {
                        println!("{} ({})", style("ok").green(), style(label).dim());
                        Ok(true)
                    }
                };

                let updated = match local.as_deref() {
                    None => install("new")?,
                    Some(l) if l == incoming_oid => {
                        println!("{} ({})", style("ok").green(), style("up to date").dim());
                        false
                    }
                    Some(l) if is_ancestor(l, &incoming_oid) => install("fast-forward")?,
                    Some(l) if is_ancestor(&incoming_oid, l) => {
                        println!(
                            "{} ({})",
                            style("ok").green(),
                            style("local ahead — kept").dim()
                        );
                        false
                    }
                    Some(local_oid_str) => {
                        // Diverged. For `refs/h5i/notes` we can union-merge
                        // safely because each tree entry is keyed by a
                        // content-addressed code-commit OID, so disjoint
                        // annotations never overlap. Other refs (memory /
                        // context / ast) are linear chains where merging
                        // would require domain-specific knowledge — for
                        // those we keep local unless --force.
                        //
                        // We can't use `git notes merge` directly: it
                        // refuses to operate on refs outside `refs/notes/*`.
                        // Instead we drive the merge ourselves via git2,
                        // build the merged commit, and update the ref to
                        // point at it.
                        if refname == "refs/h5i/notes" {
                            let g2 = git2::Repository::open(&workdir)
                                .map_err(|e| anyhow::anyhow!("open git2 repo: {e}"))?;
                            let local_git2 = git2::Oid::from_str(local_oid_str)
                                .map_err(|e| anyhow::anyhow!("parse local oid: {e}"))?;
                            let incoming_git2 = git2::Oid::from_str(&incoming_oid)
                                .map_err(|e| anyhow::anyhow!("parse incoming oid: {e}"))?;
                            let merge_result =
                                union_merge_notes_commits(&g2, local_git2, incoming_git2);
                            match merge_result {
                                Ok(new_oid) => {
                                    let new_oid_str = new_oid.to_string();
                                    let st = git(&[
                                        "update-ref",
                                        refname,
                                        &new_oid_str,
                                        local_oid_str,
                                    ])?;
                                    if st.status.success() {
                                        println!(
                                            "{} ({})",
                                            style("ok").green(),
                                            style("merged (union)").dim()
                                        );
                                        true
                                    } else {
                                        println!("{}", style("failed").red());
                                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                                        false
                                    }
                                }
                                Err(e) => {
                                    println!("{}", style("failed").red());
                                    eprintln!("union-merge of notes refs failed: {e}");
                                    false
                                }
                            }
                        } else if force {
                            install("forced over divergent local")?
                        } else {
                            println!(
                                "{} ({})",
                                style("kept local").yellow(),
                                style("diverged — pass --force to overwrite").dim()
                            );
                            false
                        }
                    }
                };

                // Always clean up the temp ref. We ignore errors here because
                // (a) it's best-effort housekeeping and (b) `update-ref -d`
                // returns success even if the ref is already gone on most git
                // versions, but we don't want a flaky cleanup to mask the
                // primary outcome.
                let _ = git(&["update-ref", "-d", &incoming]);

                Ok(updated)
            };

            let notes_changed = sync_one("refs/h5i/notes")?;
            sync_one(memory::MEMORY_REF)?;

            // Context refs: per-branch. Fetch the whole namespace into a temp
            // tree first, then sync each branch through the same safe-merge
            // logic. Legacy single ref (`refs/h5i/context`) is also tried for
            // backward compat with pre-redesign remotes.
            {
                let fetch = git(&[
                    "fetch",
                    "--no-write-fetch-head",
                    &remote,
                    "+refs/h5i/context/*:refs/h5i/_incoming/context/*",
                ])?;
                if !fetch.status.success() {
                    let stderr = String::from_utf8_lossy(&fetch.stderr);
                    if !stderr.contains("couldn't find remote ref")
                        && !stderr.contains("does not exist")
                    {
                        eprint!("{}", stderr);
                    }
                }
                // Enumerate fetched per-branch refs and sync each.
                if let Ok(out) = std::process::Command::new("git")
                    .args([
                        "for-each-ref",
                        "--format=%(refname)",
                        "refs/h5i/_incoming/context/",
                    ])
                    .current_dir(&workdir)
                    .output()
                {
                    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
                    let mut branch_names: Vec<String> = listing
                        .lines()
                        .filter_map(|l| {
                            l.strip_prefix("refs/h5i/_incoming/context/")
                                .map(str::to_owned)
                        })
                        .collect();
                    branch_names.sort();
                    for branch in &branch_names {
                        let live = format!("refs/h5i/context/{branch}");
                        // sync_one re-fetches into refs/h5i/_incoming/<basename>
                        // and uses the safe compare-and-install dance. Reusing
                        // it keeps semantics identical to other h5i refs.
                        let _ = sync_one(&live);
                    }
                    // Clean up the namespace temp refs.
                    for branch in &branch_names {
                        let incoming = format!("refs/h5i/_incoming/context/{branch}");
                        let _ = git(&["update-ref", "-d", &incoming]);
                    }
                }
                // Also try the legacy single ref (older remotes that pre-date
                // the per-branch redesign).
                let _ = sync_one("refs/h5i/context");
            }

            sync_one("refs/h5i/ast")?;

            if notes_changed {
                println!(
                    "\n{} Inspect what arrived with:\n\
                    \n    {}\
                    \n    {}\
                    \n    {}",
                    style("Tip:").bold(),
                    style("h5i log").bold(),
                    style("h5i notes show").bold(),
                    style("h5i memory log").bold(),
                );
            }
        }

        Commands::Memory { action } => {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                .to_path_buf();

            match action {
                MemoryCommands::Snapshot { commit, path, agent } => {
                    // Resolve commit OID: explicit arg or HEAD
                    let oid_str = match commit {
                        Some(ref s) => s.clone(),
                        None => {
                            let head = repo.git().head()?;
                            head.peel_to_commit()?.id().to_string()
                        }
                    };

                    let memory_agent = resolve_memory_agent(agent);
                    let src = path.as_deref();
                    let default_dir = memory::default_memory_dir(&workdir, memory_agent);
                    let display_src = src
                        .unwrap_or(&default_dir)
                        .display()
                        .to_string();

                    println!(
                        "{} {} → commit {}",
                        STEP,
                        style(format!("Snapshotting {} memory", memory_agent.label()))
                            .cyan()
                            .bold(),
                        style(&oid_str[..8.min(oid_str.len())]).magenta()
                    );

                    let count = memory::take_snapshot(
                        &repo.h5i_root,
                        &workdir,
                        &oid_str,
                        src,
                        memory_agent,
                    )?;

                    if count == 0 {
                        println!(
                            "{} {} at {}",
                            WARN,
                            style("No memory files found — empty snapshot recorded.").yellow(),
                            style(&display_src).dim()
                        );
                        println!(
                            "  {} {} may create this directory lazily on the first memory write.",
                            style("ℹ").blue(),
                            style(memory_agent.label()).cyan()
                        );
                        println!(
                            "  {} You can also snapshot any directory with {}",
                            style("ℹ").blue(),
                            style("h5i memory snapshot --path <dir>").bold()
                        );
                    } else {
                        println!(
                            "{} Saved {} file{} from {}",
                            SUCCESS,
                            style(count).cyan(),
                            if count == 1 { "" } else { "s" },
                            style(&display_src).dim()
                        );
                    }
                }

                MemoryCommands::Diff { from, to, agent } => {
                    // Default: diff last two snapshots (or last snapshot vs. live)
                    let snapshots = memory::list_snapshots(&repo.h5i_root)?;
                    let memory_agent = resolve_memory_agent(agent);

                    let (from_oid, to_oid_opt): (String, Option<String>) = match (from, to) {
                        (Some(f), t) => (f, t),
                        (None, Some(t)) => {
                            // from = latest snapshot, to = specified
                            let latest = snapshots.last().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No snapshots found. Run `h5i memory snapshot` first."
                                )
                            })?;
                            (latest.commit_oid.clone(), Some(t))
                        }
                        (None, None) => {
                            // from = second-to-last, to = live
                            let Some(latest) = snapshots.last() else {
                                println!(
                                    "{} No snapshots yet. Run {} first.",
                                    WARN,
                                    style("h5i memory snapshot").bold()
                                );
                                return Ok(());
                            };
                            (latest.commit_oid.clone(), None) // to=None means live
                        }
                    };

                    let to_label = to_oid_opt.as_deref().unwrap_or("live");
                    println!(
                        "{} {} {}..{}",
                        LOOKING,
                        style("Computing memory diff").cyan().bold(),
                        style(&from_oid[..8.min(from_oid.len())]).magenta(),
                        style(to_label).magenta()
                    );

                    let diff = memory::diff_snapshots(
                        &repo.h5i_root,
                        &workdir,
                        &from_oid,
                        to_oid_opt.as_deref(),
                        memory_agent,
                    )?;
                    memory::print_memory_diff(&diff);
                }

                MemoryCommands::Log => {
                    println!(
                        "{}\n",
                        style("Claude Memory Snapshots").bold().underlined()
                    );
                    memory::print_memory_log(&repo.h5i_root)?;
                }

                MemoryCommands::Restore { commit, agent, yes } => {
                    let snap_meta = {
                        let snaps = memory::list_snapshots(&repo.h5i_root)?;
                        snaps
                            .into_iter()
                            .find(|s| s.commit_oid.starts_with(&commit))
                            .ok_or_else(|| {
                                anyhow::anyhow!("No snapshot found for commit {}", commit)
                            })?
                    };
                    let memory_agent = resolve_memory_agent(agent);

                    println!(
                        "{} Restore memory snapshot from commit {} ({} file{})?",
                        WARN,
                        style(&snap_meta.commit_oid[..8]).magenta().bold(),
                        snap_meta.file_count,
                        if snap_meta.file_count == 1 { "" } else { "s" }
                    );
                    println!(
                        "  {} This will overwrite your current {} memory files.",
                        style("!").yellow(),
                        style(memory_agent.label()).cyan()
                    );

                    if !yes {
                        print!("\nContinue? [y/N] ");
                        use std::io::Write as _;
                        std::io::stdout().flush()?;
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("{} Aborted.", style("!").dim());
                            return Ok(());
                        }
                    }

                    let count = memory::restore_snapshot(
                        &repo.h5i_root,
                        &workdir,
                        &snap_meta.commit_oid,
                        memory_agent,
                    )?;
                    println!(
                        "{} Restored {} file{} to {}",
                        SUCCESS,
                        style(count).cyan(),
                        if count == 1 { "" } else { "s" },
                        style(
                            memory::default_memory_dir(&workdir, memory_agent)
                                .display()
                                .to_string()
                        )
                        .dim()
                    );
                }

                MemoryCommands::Push { remote } => {
                    println!(
                        "{} {} to {}",
                        STEP,
                        style("Pushing memory snapshot").cyan().bold(),
                        style(&remote).yellow()
                    );

                    let commit_oid = memory::push(repo.git(), &repo.h5i_root, &remote)?;
                    println!(
                        "{} Memory commit {} pushed to {} ({})",
                        SUCCESS,
                        style(&commit_oid[..8]).magenta().bold(),
                        style(&remote).yellow(),
                        style(memory::MEMORY_REF).dim()
                    );
                    println!(
                        "  {} Teammates can run {} to receive it.",
                        style("→").dim(),
                        style("h5i memory pull").bold()
                    );
                }

                MemoryCommands::Pull { remote } => {
                    println!(
                        "{} {} from {}",
                        STEP,
                        style("Pulling memory snapshot").cyan().bold(),
                        style(&remote).yellow()
                    );

                    let result = memory::pull(repo.git(), &repo.h5i_root, &remote)?;
                    println!(
                        "{} Received {} file{} linked to code commit {}",
                        SUCCESS,
                        style(result.file_count).cyan(),
                        if result.file_count == 1 { "" } else { "s" },
                        style(&result.linked_code_oid[..8.min(result.linked_code_oid.len())])
                            .magenta()
                            .bold()
                    );
                    println!(
                        "  {} Run {} to apply it to your Claude session.",
                        style("→").dim(),
                        style(format!(
                            "h5i memory restore {}",
                            &result.linked_code_oid[..8.min(result.linked_code_oid.len())]
                        ))
                        .bold()
                    );
                }
            }
        }

        Commands::Claims { action } => {
            let repo = H5iRepository::open(".")?;

            match action {
                ClaimsCommands::Add { text, paths, author } => {
                    let claim = claims::add(
                        &repo.h5i_root,
                        repo.git(),
                        &text,
                        paths,
                        author,
                    )?;
                    println!(
                        "{} Recorded claim {}",
                        SUCCESS,
                        style(&claim.id).magenta().bold(),
                    );
                    println!("  {}  {}", style("↳").dim(), style(&claim.text).dim());
                    println!(
                        "  {}  evidence: {}",
                        style("↳").dim(),
                        style(claim.evidence_paths.join(", ")).dim()
                    );
                }

                ClaimsCommands::List { group_by_path } => {
                    let entries = claims::list_with_status(&repo.h5i_root, repo.git())?;
                    if group_by_path {
                        claims::print_list_grouped_by_path(&entries);
                    } else {
                        claims::print_list(&entries);
                    }
                }

                ClaimsCommands::Prune => {
                    let removed = claims::prune_stale(&repo.h5i_root, repo.git())?;
                    if removed == 0 {
                        println!(
                            "{} No stale claims — nothing to prune.",
                            style("ℹ").blue(),
                        );
                    } else {
                        println!(
                            "{} Pruned {} stale claim{}",
                            SUCCESS,
                            style(removed).cyan().bold(),
                            if removed == 1 { "" } else { "s" },
                        );
                    }
                }
            }
        }

        Commands::Context { action } => {
            let workdir = Path::new(".");
            match action {
                ContextCommands::Init { goal } => {
                    ctx::init(workdir, &goal)?;
                    println!(
                        "{} {} at {}",
                        SUCCESS,
                        style(".h5i-ctx/ workspace initialized").green().bold(),
                        style(".h5i-ctx/").dim()
                    );
                    println!();
                    println!("  {}", style("Quick-start:").bold());
                    println!(
                        "    {}  checkpoint your progress",
                        style("h5i context commit \"summary\" --detail \"…\"").cyan()
                    );
                    println!(
                        "    {}  explore an alternative",
                        style("h5i context branch experiment/foo --purpose \"…\"").cyan()
                    );
                    println!(
                        "    {}  view current context",
                        style("h5i context show --trace").cyan()
                    );
                }

                ContextCommands::Commit { summary, detail } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(
                            ".h5i-ctx/ not initialized. Run `h5i context init --goal \"<goal>\"` first."
                        );
                    }
                    ctx::prepare_context_write(workdir)?;
                    ctx::gcc_commit(workdir, &summary, &detail)?;
                    println!(
                        "{} {} — {}",
                        SUCCESS,
                        style("Context commit recorded").green().bold(),
                        style(&summary).cyan()
                    );
                }

                ContextCommands::Branch { name, purpose } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    if purpose.trim().is_empty() {
                        anyhow::bail!(
                            "Context branch '{}' requires a purpose. Run `h5i context branch {} --purpose \"<intent>\"`.",
                            name,
                            name
                        );
                    }
                    ctx::gcc_branch(workdir, &name, &purpose)?;
                    println!(
                        "{} Created and switched to branch {}",
                        SUCCESS,
                        style(&name).magenta().bold()
                    );
                }

                ContextCommands::Checkout { name } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::gcc_checkout(workdir, &name)?;
                    println!(
                        "{} Switched to branch {}",
                        SUCCESS,
                        style(&name).magenta().bold()
                    );
                }

                ContextCommands::Merge { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let target = ctx::current_branch(workdir);
                    let summary = ctx::gcc_merge(workdir, &branch)?;
                    println!(
                        "{} Merged {} into {}",
                        SUCCESS,
                        style(&branch).magenta(),
                        style(&target).magenta().bold()
                    );
                    println!("{}", style(&summary).dim());
                }

                ContextCommands::Show {
                    branch,
                    commit,
                    trace,
                    metadata,
                    window,
                    trace_offset,
                    depth,
                } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    // --trace is shorthand for --depth 3
                    let effective_depth = if trace { 3 } else { depth };
                    let opts = ctx::ContextOpts {
                        branch,
                        commit_hash: commit,
                        show_log: effective_depth >= 3,
                        log_offset: trace_offset,
                        metadata_segment: metadata,
                        window,
                        depth: effective_depth,
                    };
                    let snapshot = ctx::gcc_context(workdir, &opts)?;
                    ctx::print_context_depth(&snapshot, effective_depth);
                }

                ContextCommands::Trace { kind, content, ephemeral } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(
                            ".h5i-ctx/ not initialized. Run `h5i context init --goal \"<goal>\"` first."
                        );
                    }
                    ctx::prepare_context_write(workdir)?;
                    ctx::append_log(workdir, &kind, &content, ephemeral)?;
                    let marker = if ephemeral {
                        style("◇").dim()
                    } else {
                        style("◈").cyan()
                    };
                    println!(
                        "{} [{}] {}",
                        marker,
                        style(kind.to_uppercase()).bold(),
                        style(&content).dim()
                    );
                }

                ContextCommands::Status => {
                    ctx::print_status(workdir)?;
                    // Feature 5: append proactive review surface if git repo + notes exist.
                    if let Ok(repo) = H5iRepository::open(workdir) {
                        if let Ok(pts) = repo.suggest_review_points(3, 0.4) {
                            if !pts.is_empty() {
                                println!();
                                println!(
                                    "  {}",
                                    style("Commits flagged for review:").yellow().bold()
                                );
                                for pt in &pts {
                                    println!(
                                        "    {} {} score {:.2}  {}",
                                        style("⚑").red(),
                                        style(&pt.short_oid).dim(),
                                        pt.score,
                                        style(&pt.message).italic(),
                                    );
                                    for trig in pt.triggers.iter().take(2) {
                                        println!(
                                            "      {} {}",
                                            style("·").dim(),
                                            style(&trig.detail).dim()
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                ContextCommands::Prompt => {
                    print!("{}", ctx::system_prompt(workdir));
                    // Append live, content-addressed claims so the next session
                    // can skip re-deriving facts that are still evidence-valid.
                    // Single-path claims serve as per-file orientations
                    // (the role formerly held by h5i summary).
                    if let Ok(h5i_repo) = H5iRepository::open(".") {
                        if let Ok(live) = claims::live_claims(&h5i_repo.h5i_root, h5i_repo.git()) {
                            print!("{}", claims::render_preamble(&live));
                        }
                    }
                    // Surface the user-tuned frequency policy (off/high) so the
                    // agent's claim-recording behaviour tracks the env var under
                    // pipelines that build the prompt via `h5i context prompt`,
                    // not just via the SessionStart hook.
                    if let Some(hint) = claims::ClaimsFrequency::from_env().prelude_hint() {
                        println!();
                        println!("{hint}");
                    }
                }

                ContextCommands::Scan { branch, json } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let branch_ref = branch.as_deref();
                    let trace = ctx::read_trace(workdir, branch_ref)?;
                    let branch_label = branch_ref
                        .unwrap_or_else(|| ctx::current_branch(workdir).leak());
                    let result = h5i_core::injection::scan(&trace);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    } else {
                        h5i_core::injection::print_scan_result(&result, branch_label);
                    }
                }

                ContextCommands::Restore { sha } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let summary = ctx::restore(workdir, &sha)?;
                    println!(
                        "{} {} {}",
                        SUCCESS,
                        style("Context restored:").green().bold(),
                        style(&summary).dim()
                    );
                    println!(
                        "  {} Run {} to verify the restored state.",
                        style("→").dim(),
                        style("h5i context show --trace").cyan()
                    );
                }

                ContextCommands::Diff { from, to } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let diff = ctx::context_diff(workdir, &from, &to)?;
                    ctx::print_context_diff(&diff);
                }

                ContextCommands::Relevant { file } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let ctx_result = ctx::relevant(workdir, &file)?;
                    ctx::print_relevant(&ctx_result, &file);
                }

                ContextCommands::Pack => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let result = ctx::pack_lossless(workdir)?;
                    if result.kept_durable == 0
                        && result.removed_subsumed_observe == 0
                        && result.merged_consecutive_observe == 0
                    {
                        println!(
                            "{} Nothing to pack — context history is already compact.",
                            style("ℹ").blue()
                        );
                    } else {
                        println!("{} Three-pass lossless pack complete:", SUCCESS);
                        if result.removed_subsumed_observe > 0 {
                            println!(
                                "  {} {} subsumed OBSERVE entries removed",
                                style("−").red(),
                                style(result.removed_subsumed_observe).cyan().bold()
                            );
                        }
                        if result.merged_consecutive_observe > 0 {
                            println!(
                                "  {} {} consecutive OBSERVE entries merged",
                                style("⇒").yellow(),
                                style(result.merged_consecutive_observe).cyan().bold()
                            );
                        }
                        println!(
                            "  {} {} THINK/ACT/NOTE entries preserved verbatim",
                            style("✔").green(),
                            style(result.kept_durable).cyan().bold()
                        );
                        println!(
                            "  {} Run {} to reclaim disk space.",
                            style("→").dim(),
                            style("git gc").cyan()
                        );
                    }
                }

                ContextCommands::Scope { name, purpose } => {
                    let full_name = if name.starts_with("scope/") {
                        name.clone()
                    } else {
                        format!("scope/{name}")
                    };
                    let purpose_text = if purpose.is_empty() {
                        format!("Subagent scope: {name}")
                    } else {
                        purpose.clone()
                    };
                    ctx::gcc_scope(workdir, &full_name, &purpose_text)?;
                    println!(
                        "{} Scope {} created and activated.",
                        SUCCESS,
                        style(&full_name).magenta().bold()
                    );
                    println!(
                        "  {} Merge findings back with {}",
                        style("→").dim(),
                        style(format!("h5i context merge {full_name}")).cyan()
                    );
                }

                ContextCommands::Ephemeral { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let text = ctx::read_ephemeral(workdir, branch.as_deref())?;
                    if text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).count() == 0 {
                        println!("{} No ephemeral traces (cleared on last context commit).", style("ℹ").blue());
                    } else {
                        println!("{}", style("── Ephemeral Traces (scratch, not persisted) ──────────────").dim());
                        for line in text.lines().filter(|l| !l.starts_with('#')) {
                            println!("  {}", style(line).dim());
                        }
                    }
                }

                ContextCommands::CachedPrefix { tail } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_cached_prefix(workdir, tail)?;
                }

                ContextCommands::Todo => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_todos(workdir)?;
                }

                ContextCommands::Knowledge => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_knowledge(workdir)?;
                }

                ContextCommands::Dag { branch } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    ctx::print_dag(workdir, branch.as_deref())?;
                }

                ContextCommands::Recap { session, since, dry_run } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }

                    let cutoff = match since {
                        Some(s) => Some(
                            s.parse::<chrono::DateTime<chrono::Utc>>()
                                .map_err(|e| anyhow::anyhow!("invalid --since timestamp: {e}"))?,
                        ),
                        None => None,
                    };

                    // Session-log discovery matches on absolute cwd, so resolve first.
                    let scan_dir = std::fs::canonicalize(workdir)
                        .unwrap_or_else(|_| workdir.to_path_buf());

                    let opts = h5i_core::recap::ImportOpts {
                        since: cutoff,
                        session_path: session,
                        dry_run,
                    };

                    let results = h5i_core::recap::import_recaps(&scan_dir, &opts)?;

                    let imported: Vec<_> = results.iter().filter(|r| !r.skipped).collect();
                    let skipped: Vec<_> = results.iter().filter(|r| r.skipped).collect();

                    if results.is_empty() {
                        println!("{} No recaps found in session log.", style("·").dim());
                    } else {
                        let verb = if dry_run { "would import" } else { "imported" };
                        println!(
                            "{} {} {} new recap(s){}",
                            SUCCESS,
                            style(verb).green().bold(),
                            style(imported.len()).cyan(),
                            if skipped.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} already imported", skipped.len())
                            },
                        );
                        for r in &imported {
                            let (summary, _) = h5i_core::recap::split_summary_detail(&r.recap.content);
                            let display = if summary.is_empty() {
                                r.recap.uuid.clone()
                            } else {
                                summary
                            };
                            let short = r.recap.uuid.get(..8).unwrap_or(&r.recap.uuid);
                            println!(
                                "  {} {}  {}",
                                style("✓").green(),
                                style(short).dim(),
                                display,
                            );
                        }
                    }
                }

                ContextCommands::Search { query, limit, history, json } => {
                    if !ctx::is_initialized(workdir) {
                        anyhow::bail!(".h5i-ctx/ not initialized. Run `h5i context init` first.");
                    }
                    let mut results = ctx::search(workdir, &query, limit)?;

                    // Enrich top results with git co-change data
                    if let Ok(repo) = H5iRepository::open(workdir) {
                        for r in results.iter_mut().take(5) {
                            if let Ok(cochanged) = repo.cochanged_files(&r.file, history, 5) {
                                r.cochanged_with = cochanged.into_iter().map(|(f, _)| f).collect();
                            }
                        }
                    }

                    if json {
                        let out: Vec<serde_json::Value> = results.iter().map(|r| {
                            serde_json::json!({
                                "file": r.file,
                                "score": r.score,
                                "signal": r.signal,
                                "snippets": r.snippets,
                                "cochanged_with": r.cochanged_with,
                            })
                        }).collect();
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    } else {
                        ctx::print_search_results(&results, &query);
                    }
                }
            }
        }

        Commands::Resolve { ours, theirs, file } => {
            let repo = H5iRepository::open(".")?;
            let our_oid = Oid::from_str(&ours)?;
            let their_oid = Oid::from_str(&theirs)?;

            println!(
                "{} {} for {}...",
                STEP,
                style("3-way text merge").cyan().bold(),
                style(&file).yellow()
            );
            let outcome = repo.merge_file_three_way(our_oid, their_oid, &file)?;

            println!("\n{}\n{}", style("--- Merge Result ---").dim(), outcome.content);
            if outcome.had_conflicts {
                eprintln!(
                    "\n{} Conflict markers were left in the output. Resolve them and `git add {}`.",
                    style("⚠").yellow(),
                    style(&file).bold()
                );
                std::process::exit(1);
            } else {
                println!(
                    "\n{} Tip: Use {} to stage the resolved content.",
                    style("💡").yellow(),
                    style(format!("git add {}", file)).bold()
                );
            }
        }

        Commands::Mcp => {
            let workdir = std::env::current_dir()?;
            eprintln!("h5i-mcp: listening on stdio (workdir: {})", workdir.display());
            h5i_core::mcp::run_stdio(workdir)?;
        }

        Commands::Doctor {
            repair,
            export,
            json,
        } => {
            let git_repo = git2::Repository::discover(".")?;
            let report = storage::doctor(&git_repo, repair, export.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_doctor_report(&report);
            }
            if !report.ok {
                std::process::exit(2);
            }
        }

        Commands::Vibe { limit, json } => {
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

        Commands::Policy { action } => {
            let workdir = std::env::current_dir()?;
            match action {
                PolicyCommands::Init => {
                    let path = h5i_core::policy::init_policy(&workdir)?;
                    println!(
                        "{} {} at {}",
                        SUCCESS,
                        style("Policy file created").green().bold(),
                        style(path.display()).yellow()
                    );
                    println!(
                        "  {} Edit {} to define your governance rules.",
                        style("→").dim(),
                        style(path.display()).cyan()
                    );
                }
                PolicyCommands::Check => {
                    let repo = H5iRepository::open(".")?;
                    match h5i_core::policy::load_policy(&workdir)? {
                        None => {
                            println!(
                                "{} No policy file found at {}",
                                WARN,
                                style(h5i_core::policy::policy_path(&workdir).display()).dim()
                            );
                            println!("  Run `h5i policy init` to create one.");
                        }
                        Some(cfg) => {
                            // Get staged files.
                            let staged_files: Vec<String> = {
                                let mut idx = repo.git().index()?;
                                idx.read(true)?;
                                idx.iter()
                                    .map(|e| String::from_utf8_lossy(&e.path).to_string())
                                    .collect()
                            };
                            let input = h5i_core::policy::CommitCheckInput {
                                message: "",
                                ai_meta: None,
                                staged_files: &staged_files,
                                audit_passed: false,
                            };
                            let violations = h5i_core::policy::check_commit(&cfg, &input);
                            if violations.is_empty() {
                                println!(
                                    "{} {}",
                                    SUCCESS,
                                    style("No policy violations in staged files.").green()
                                );
                            } else {
                                println!(
                                    "{} {} violation(s):",
                                    ERROR,
                                    style(violations.len()).red().bold()
                                );
                                h5i_core::policy::print_violations(&violations);
                            }
                        }
                    }
                }
                PolicyCommands::Show => {
                    let path = h5i_core::policy::policy_path(&workdir);
                    match h5i_core::policy::load_policy(&workdir)? {
                        None => {
                            println!(
                                "{} No policy file found at {}",
                                WARN,
                                style(path.display()).dim()
                            );
                            println!("  Run `h5i policy init` to create one.");
                        }
                        Some(cfg) => {
                            h5i_core::policy::print_policy(&cfg, &path);
                        }
                    }
                }
            }
        }

        Commands::Compliance {
            since,
            until,
            format,
            output,
            limit,
        } => {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            let policy_cfg = h5i_core::policy::load_policy(&workdir)?;

            println!(
                "{} {}",
                STEP,
                style("Scanning commits for compliance report…").cyan().bold()
            );

            let report = h5i_core::compliance::compute_compliance_report(
                &repo,
                since.as_deref(),
                until.as_deref(),
                policy_cfg.as_ref(),
                limit,
            )?;

            let content: String = match format.as_str() {
                "json" => h5i_core::compliance::to_json(&report)?,
                "html" => h5i_core::compliance::to_html(&report),
                _ => {
                    // Print text directly and return early.
                    h5i_core::compliance::print_compliance_text(&report);
                    if let Some(ref path) = output {
                        // Re-generate for file write.
                        let text = format!(
                            "h5i compliance report\n{} commits scanned · {} AI · {} policy violations\n",
                            report.total_commits, report.ai_commits, report.policy_violations
                        );
                        std::fs::write(path, text)?;
                        println!(
                            "{} Report written to {}",
                            SUCCESS,
                            style(path.display()).yellow()
                        );
                    }
                    return Ok(());
                }
            };

            if let Some(ref path) = output {
                std::fs::write(path, &content)?;
                println!(
                    "{} {} report written to {}",
                    SUCCESS,
                    style(format.to_uppercase()).cyan(),
                    style(path.display()).yellow()
                );
            } else {
                println!("{}", content);
            }
        }

        Commands::Resume { branch } => {
            let repo = H5iRepository::open(".")?;
            let workdir = repo
                .git()
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("Bare repository not supported"))?
                .to_path_buf();
            if let Some(ref b) = branch {
                println!(
                    "{} {} {}",
                    STEP,
                    style("Generating handoff briefing for branch").cyan().bold(),
                    style(b).yellow()
                );
            } else {
                println!(
                    "{} {}",
                    STEP,
                    style("Generating handoff briefing...").cyan().bold()
                );
            }
            match h5i_core::resume::generate_briefing(&repo, &workdir, branch.as_deref()) {
                Ok(briefing) => h5i_core::resume::print_briefing(&briefing),
                Err(e) => println!("{} Failed to generate briefing: {}", ERROR, style(e).red()),
            }
        }
    }

    Ok(())
}
