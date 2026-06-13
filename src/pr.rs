//! GitHub pull-request integration for h5i.
//!
//! Renders a sticky PR comment that surfaces h5i provenance for every commit
//! on the current branch vs. the PR's base. Uses the `gh` CLI under the hood.
//!
//! The comment is identified by an HTML marker tag so re-running `h5i pr post`
//! upserts in place rather than spamming new comments.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::ctx::{self, TraceDag, TraceNode};
use crate::metadata::H5iCommitRecord;
use crate::repository::H5iRepository;
use crate::review::{ReviewPoint, REVIEW_THRESHOLD};

/// HTML marker we put at the top of the comment. `gh` returns comment bodies
/// verbatim, so we can find ours by prefix-matching this string.
pub const MARKER: &str = "<!-- h5i:pr-comment v1 -->";

/// Maximum number of DAG nodes rendered in the Mermaid block. GitHub scales
/// large Mermaid diagrams down to fit the PR column, so a smaller visible
/// window is more readable at normal zoom than a complete-but-tiny graph.
const DAG_NODE_LIMIT: usize = 24;

/// Hard cap on the label-text portion of a DAG node (after the `KIND ·` prefix).
/// Wide enough that THINK/NOTE reasoning is useful; narrow enough that the
/// graph does not force GitHub to shrink every node.
const DAG_LABEL_BUDGET: usize = 72;

/// Maximum number of file lanes drawn in the swim-lane DAG before extras get
/// folded into an "Other" lane. Four file lanes plus Reasoning/Other keeps
/// the graph legible in GitHub's normal PR comment column.
const SWIMLANE_MAX_FILES: usize = 4;

/// Minimum run length before consecutive same-kind nodes within a single lane
/// get compressed into `READ × N` / `EDIT × N`. More aggressive than the
/// chain-DAG threshold because per-lane density matters more than per-graph.
const SWIMLANE_COMPRESS_RUN: usize = 2;

/// Default cap on coordination threads rendered in the PR body. Enough to show
/// the shape of a collaboration without letting a chatty branch dominate the
/// comment; surplus threads are noted as elided.
const MSG_THREAD_LIMIT: usize = 12;

/// Character budget for a default (review-typed) message excerpt — the first
/// non-empty line, trimmed to roughly a sentence.
const MSG_EXCERPT_BUDGET: usize = 200;

/// Character budget for a `--msg-bodies` full body (newlines folded to spaces).
const MSG_FULL_BUDGET: usize = 1000;

/// Controls whether — and how much of — the i5h coordination history is folded
/// into the PR body. Defaults to the disclosure-safe shape agreed for the
/// feature: include branch-scoped threads, but only review-typed messages get a
/// one-line (secret-redacted) excerpt; everything else is metadata-only.
#[derive(Debug, Clone, Copy)]
pub struct MsgOptions {
    /// Render the coordination section at all (`--no-msg` sets this false).
    pub include: bool,
    /// Show a (still redacted + sanitized) excerpt for *every* message kind,
    /// not just review-typed ones (`--msg-bodies`).
    pub full_bodies: bool,
    /// Cap on threads rendered before eliding.
    pub max_threads: usize,
}

impl Default for MsgOptions {
    fn default() -> Self {
        Self { include: true, full_bodies: false, max_threads: MSG_THREAD_LIMIT }
    }
}

// ── Detail-string parsers ────────────────────────────────────────────────────

/// Parsed `CREDENTIAL_LEAK` finding row, suitable for table rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretRow {
    rule_id: String,
    description: String,
    file: Option<String>,
    line: usize,
    preview: String,
    short_oid: String,
}

/// Parsed `DUPLICATED_CODE` finding row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DupRow {
    file: String,
    block_len: usize,
    first_line: usize,
    repeat_line: usize,
    short_oid: String,
}

/// Parses a `CREDENTIAL_LEAK` trigger detail produced by `rules.rs`. Two
/// shapes are recognised:
///   "<desc> matched in <path> (line <N>, preview `<preview>`)"   ← per-file
///   "<desc> matched (line <N>, preview `<preview>`)"             ← fallback
fn parse_secret_detail(detail: &str, rule_id: &str, short_oid: &str) -> Option<SecretRow> {
    // Walk from the right since `desc` can contain " matched in " phrases.
    let preview_close = detail.rfind("`)")?;
    let preview_open = detail[..preview_close].rfind("preview `")?;
    let preview = detail[preview_open + "preview `".len()..preview_close].to_string();

    let after_line = detail[..preview_open].trim_end_matches(", ");
    let line_open = after_line.rfind("(line ")?;
    let line_str = &after_line[line_open + "(line ".len()..];
    let line: usize = line_str.trim().parse().ok()?;

    let head = detail[..line_open].trim_end_matches(' ');
    let (description, file) = if let Some(idx) = head.rfind(" matched in ") {
        let desc = head[..idx].to_string();
        let f = head[idx + " matched in ".len()..].trim().to_string();
        (desc, Some(f))
    } else if let Some(idx) = head.rfind(" matched") {
        (head[..idx].to_string(), None)
    } else {
        (head.to_string(), None)
    };

    Some(SecretRow {
        rule_id: rule_id.to_string(),
        description,
        file,
        line,
        preview,
        short_oid: short_oid.to_string(),
    })
}

/// Parses a `DUPLICATED_CODE` trigger detail. Expected shape:
/// "<N> duplicated lines in '<path>': block first seen at line <A>, repeated at line <B>"
fn parse_duplicate_detail(detail: &str, short_oid: &str) -> Option<DupRow> {
    let (count_part, rest) = detail.split_once(" duplicated lines in '")?;
    let block_len: usize = count_part.trim().parse().ok()?;
    let (path, rest) = rest.split_once("': block first seen at line ")?;
    let (first_str, rest) = rest.split_once(", repeated at line ")?;
    let first_line: usize = first_str.trim().parse().ok()?;
    let repeat_line: usize = rest.trim().parse().ok()?;
    Some(DupRow {
        file: path.to_string(),
        block_len,
        first_line,
        repeat_line,
        short_oid: short_oid.to_string(),
    })
}

// ── Aggregation across commits ───────────────────────────────────────────────

fn collect_secret_rows(
    records: &[H5iCommitRecord],
    by_oid: &HashMap<String, &ReviewPoint>,
) -> Vec<SecretRow> {
    let mut out = Vec::new();
    for r in records {
        let Some(rp) = by_oid.get(&r.git_oid).copied() else {
            continue;
        };
        let short = &r.git_oid[..r.git_oid.len().min(8)];
        for t in rp.quality_triggers() {
            if t.rule_id != "CREDENTIAL_LEAK" {
                continue;
            }
            if let Some(row) = parse_secret_detail(&t.detail, &t.rule_id, short) {
                out.push(row);
            }
        }
    }
    out
}

fn collect_duplicate_rows(
    records: &[H5iCommitRecord],
    by_oid: &HashMap<String, &ReviewPoint>,
) -> Vec<DupRow> {
    let mut out = Vec::new();
    for r in records {
        let Some(rp) = by_oid.get(&r.git_oid).copied() else {
            continue;
        };
        let short = &r.git_oid[..r.git_oid.len().min(8)];
        for t in rp.quality_triggers() {
            if t.rule_id != "DUPLICATED_CODE" {
                continue;
            }
            if let Some(row) = parse_duplicate_detail(&t.detail, short) {
                out.push(row);
            }
        }
    }
    out
}

// ── Section renderers ────────────────────────────────────────────────────────

fn render_secret_section(rows: &[SecretRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let commits: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.short_oid.as_str()).collect();
    let mut s = String::new();
    let _ = writeln!(
        s,
        "> [!CAUTION]\n> **{n} credential leak{plural} across {c} commit{cplural}** \
         — rotate the exposed secrets before merging.",
        n = rows.len(),
        plural = if rows.len() == 1 { "" } else { "s" },
        c = commits.len(),
        cplural = if commits.len() == 1 { "" } else { "s" },
    );
    s.push('\n');
    s.push_str("| Rule | File | Line | Preview | Commit |\n");
    s.push_str("|---|---|---:|---|---|\n");
    for r in rows {
        let _ = writeln!(
            s,
            "| `{}` | {} | {} | `{}` | `{}` |",
            escape_md(&r.rule_id),
            r.file
                .as_deref()
                .map(|f| format!("`{}`", escape_md(f)))
                .unwrap_or_else(|| "_unknown_".to_string()),
            r.line,
            escape_md(&r.preview),
            r.short_oid,
        );
    }
    s.push('\n');
    s
}

fn render_duplicate_section(rows: &[DupRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    // Group by file so the table reads "what's duplicated where" rather than
    // a flat per-finding firehose.
    let mut by_file: BTreeMap<&str, Vec<&DupRow>> = BTreeMap::new();
    for r in rows {
        by_file.entry(r.file.as_str()).or_default().push(r);
    }

    let mut s = String::new();
    let _ = writeln!(
        s,
        "> [!WARNING]\n> **Duplicate code introduced in {} file{}** \
         — consider extracting a helper before this lands.",
        by_file.len(),
        if by_file.len() == 1 { "" } else { "s" },
    );
    s.push('\n');
    s.push_str("| File | Block | First → Repeat | Commit |\n");
    s.push_str("|---|---:|---|---|\n");
    for (file, group) in &by_file {
        for r in group {
            let _ = writeln!(
                s,
                "| `{}` | {} lines | L{} → L{} | `{}` |",
                escape_md(file),
                r.block_len,
                r.first_line,
                r.repeat_line,
                r.short_oid,
            );
        }
    }
    s.push('\n');
    s
}

/// Standalone `> [!TIP]` callout (green stripe on github.com) emitted whenever
/// the credential scan came back clean. Promoted out of the inline `_Other
/// checks: …_` footer so the security-pass signal is *louder* than the
/// duplicate alert sitting next to it — a clean scan is a positive trust
/// signal and deserves its own visual lane.
///
/// Composition note: when paired with the duplicate-pass note (both checks
/// clean), the two callouts stack visually. We don't collapse them into a
/// single "h5i checks pass" line anymore because doing so demoted the
/// security signal to a bullet — and security findings (positive or
/// negative) should always read as the marquee result.
fn render_secret_pass_callout() -> String {
    "> [!TIP]\n\
     > ### ✅ Security scan clean\n\
     > **No credentials leaked** in this branch's diff. h5i scanned every \
     added line against the secret rule pack and found nothing to rotate.\n\n"
        .to_string()
}

/// Smaller note for the duplicate-code-pass result. Uses `> [!NOTE]` (white,
/// neutral) rather than `[!TIP]` because duplicate code is a craft signal,
/// not a security signal — visually subordinate to the security callout.
fn render_duplicate_pass_note() -> String {
    "> [!NOTE]\n> **Duplicate-code scan clean** — no copy-paste blocks introduced.\n\n"
        .to_string()
}

/// 🪙 Token-reduction summary: how many tokens of raw tool output `h5i capture
/// run` kept out of the agent's context on this branch (from `refs/h5i/objects`).
/// Self-omits when there were no captures on the branch or no net saving.
fn render_token_reduction_section(git: &git2::Repository, branch: &str) -> String {
    token_reduction_section_from(&crate::objects::read_manifests(git), branch)
}

/// Pure core of [`render_token_reduction_section`] — takes the manifests
/// directly so it can be unit-tested without a git repo.
fn token_reduction_section_from(manifests: &[crate::objects::Manifest], branch: &str) -> String {
    let mut n = 0usize;
    let mut raw: u64 = 0;
    let mut sum: u64 = 0;
    // tool → (raw, summary, count)
    let mut by_tool: std::collections::BTreeMap<String, (u64, u64, usize)> = Default::default();
    for m in manifests {
        // Only captures taken on this branch.
        if m.branch.as_deref() != Some(branch) {
            continue;
        }
        // Use the DEFAULT agent-facing token count (compact render when
        // structured), so "kept out of context" matches what the agent saw.
        let (Some(r), Some(s)) = (m.raw_tokens, m.agent_facing_tokens()) else {
            continue;
        };
        n += 1;
        raw += r as u64;
        sum += s as u64;
        let tool = m
            .structured
            .as_ref()
            .map(|t| t.tool.clone())
            .unwrap_or_else(|| m.kind.clone());
        let e = by_tool.entry(tool).or_insert((0, 0, 0));
        e.0 += r as u64;
        e.1 += s as u64;
        e.2 += 1;
    }
    // Nothing captured, or no net reduction → omit (don't advertise a loss).
    if n == 0 || sum >= raw {
        return String::new();
    }
    let saved = raw - sum;
    let pct = saved * 100 / raw;

    let mut s = String::new();
    s.push_str(&format!(
        "> [!NOTE]\n> 🪙 **Token reduction** — {n} captured tool output{} kept out of context: \
         {raw} → {sum} tokens (**{pct}% saved**, {saved} tokens). \
         Full output is recoverable with `h5i recall object`.\n\n",
        if n == 1 { "" } else { "s" }
    ));
    if by_tool.len() > 1 {
        s.push_str("<details><summary>By tool</summary>\n\n");
        s.push_str("| Tool | Captures | Raw | Summary | Saved |\n|---|---:|---:|---:|---:|\n");
        let mut rows: Vec<(&String, &(u64, u64, usize))> = by_tool.iter().collect();
        rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.0.saturating_sub(v.1)));
        for (tool, (tr, ts, tc)) in rows {
            let tsaved = tr.saturating_sub(*ts);
            let tpct = if *tr > 0 { tsaved * 100 / tr } else { 0 };
            // Tool names come from argv/basename or manifest kind — untrusted,
            // so escape like all other PR comment cells.
            let tool = escape_md(tool);
            s.push_str(&format!("| {tool} | {tc} | {tr} | {ts} | {tpct}% |\n"));
        }
        s.push_str("\n</details>\n\n");
    }
    s
}

/// Top-of-comment banner emitted when a credential leak was detected. Lives
/// **above** the hero block so it lands in the first screenshot a reviewer
/// or social-share takes — leaks must be impossible to miss. The full
/// finding table (rule, file, line, preview) renders immediately after
/// via [`render_secret_section`].
fn render_secret_alert_banner(n: usize) -> String {
    format!(
        "> [!CAUTION]\n\
         > # 🚨 BLOCK MERGE — {n} credential leak{plural} detected\n\
         > **Rotate the exposed secrets and remove them from history before merging.** \
         The h5i audit found {n} finding{plural} in this branch's diff. \
         Full details in the table below.\n\n",
        n = n,
        plural = if n == 1 { "" } else { "s" },
    )
}

fn mermaid_id(node_id: &str) -> String {
    // Mermaid node identifiers must be `[A-Za-z_][A-Za-z0-9_]*`. Our DAG IDs
    // are hex digests, so prefix with `n_` to guarantee a letter start.
    let mut out = String::with_capacity(node_id.len() + 2);
    out.push_str("n_");
    for c in node_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

fn mermaid_class(kind: &str) -> &'static str {
    match kind {
        "OBSERVE" => "o",
        "THINK" => "t",
        "ACT" => "a",
        "NOTE" => "n",
        "MERGE" => "m",
        _ => "n",
    }
}

// ── Top-level render ─────────────────────────────────────────────────────────

/// Layout for the hero block at the top of the comment.
///
/// The audit sections below the fold (secrets, duplicates, per-commit) stay
/// the same across styles — only the first viewport changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStyle {
    /// Receipt — H1 headline + Stripe-style stat card + prompt + milestones.
    /// Default. Optimised for the screenshot-able first viewport and
    /// social-share virality; uses `~$X` cost callouts and a centered HTML
    /// table for the stat row. Pair with `Minimal` for terse internal PRs.
    Receipt,
    /// Detective — narrative: goal → considered/rejected → key insight → shipped.
    Detective,
    /// Replay — DAG promoted above the fold with milestone markers.
    Replay,
    /// Minimal — quiet variant for internal PRs that want h5i provenance
    /// without the marketing flourish: a single-line headline, the goal,
    /// the swim-lane DAG, and the audit sections. No HTML tables, no
    /// dollar figures, no IMPORTANT callout.
    Minimal,
    /// Review — reviewer-first triage brief. Leads with merge status,
    /// review focus, evidence, and a short checklist; keeps provenance
    /// details below the fold.
    Review,
}

/// Pre-rolled aggregates derived from `h5i_log` + review points. All hero
/// renderers consume this so they never re-walk the records list.
struct Aggregates {
    ai_count: usize,
    human_count: usize,
    total_tokens: usize,
    /// Best-effort sum of per-commit compute cost in USD, derived from each
    /// `AiMetadata::usage` via the public-list-price table in [`model_price`].
    /// `None` only when every AI commit on the branch lacked a recognised
    /// `usage.model` — at that point we can't honestly attach a dollar figure.
    estimated_cost_usd: Option<f64>,
    tests_passing: usize,
    tests_failing: usize,
    flagged_count: usize,
}

/// Strings extracted from the context workspace + commit decisions to feed
/// the narrative-style renderers. Each field may be empty if the underlying
/// signal is missing (no ctx workspace, no decisions recorded, …).
struct HeroInputs {
    branch_goal: String,
    milestones: Vec<String>,
    /// Most-recent THINK trace entry — the "key insight" for Detective style.
    top_think: Option<String>,
    /// Most-recent OBSERVE trace entry — context backdrop for Receipt style.
    top_observe: Option<String>,
    /// All Decision records flattened across the branch's AI commits.
    decisions: Vec<DecisionEntry>,
    /// First non-empty AI prompt on the branch — the "trigger" for Receipt.
    top_prompt: Option<String>,
    /// Process-shape stats derived from the DAG (files, READ:EDIT, THINK
    /// density). Always present — degenerate cases (empty DAG) yield a
    /// stats struct whose [`format_dag_stats_inline`] returns an empty
    /// string, so heroes can render unconditionally and skip the line
    /// when there's nothing useful to say.
    dag_stats: DagStats,
}

#[derive(Debug, Clone)]
struct DecisionEntry {
    location: String,
    choice: String,
    alternatives: Vec<String>,
    reason: String,
    short_oid: String,
}

/// Backward-compatible entry point — keeps the old `render_body(workdir, limit)`
/// signature for callers (and our test suite) that don't pass a style. Equivalent
/// to `render_body_with_style(..., PrStyle::Receipt)`.
pub fn render_body(workdir: &Path, limit: usize) -> Result<String> {
    render_body_with_style(workdir, limit, PrStyle::Receipt)
}

/// Render the full Markdown body for the PR comment.
///
/// Layout (sections omit themselves when empty):
///   1. Hero block (style-dependent)
///   2. 🔒 Credential-leak alert + table
///   3. 🔁 Duplicate-code alert + table
///   4. 🧠 Reasoning DAG — placement depends on style:
///      Receipt/Detective/Minimal/Review: collapsible, below the fold
///      Replay:                           rendered inside the hero, expanded
///   5. 💬 Agent coordination — branch-scoped i5h message threads (collapsible)
///   6. 📜 Per-commit provenance (collapsible if >5 AI commits)
///   7. Footer
///
/// Backward-compatible 3-arg entry point: renders with the default
/// [`MsgOptions`] (coordination section on, review-typed excerpts). Callers that
/// need to honour `--no-msg` / `--msg-bodies` / `--msg-limit` use
/// [`render_body_with_options`].
pub fn render_body_with_style(workdir: &Path, limit: usize, style: PrStyle) -> Result<String> {
    render_body_with_options(workdir, limit, style, &MsgOptions::default())
}

/// Options-aware variant of [`render_body_with_style`]. See that function for
/// the section layout; `msg_opts` controls the 💬 Agent coordination section.
pub fn render_body_with_options(
    workdir: &Path,
    limit: usize,
    style: PrStyle,
    msg_opts: &MsgOptions,
) -> Result<String> {
    let _span = tracing::info_span!("pr_render_body", limit, ?style).entered();
    let repo = H5iRepository::open(workdir)?;
    let records = repo
        .h5i_log(limit)
        .context("failed to read h5i log for PR body")?;

    let review_points = repo
        .suggest_review_points(limit, REVIEW_THRESHOLD)
        .unwrap_or_default();
    let by_oid: HashMap<String, &ReviewPoint> = review_points
        .iter()
        .map(|p| (p.commit_oid.clone(), p))
        .collect();

    // Pre-roll all aggregates so the header can summarise without re-walking.
    let secret_rows = collect_secret_rows(&records, &by_oid);
    let dup_rows = collect_duplicate_rows(&records, &by_oid);
    let dag = ctx::dag_for_branch(workdir, None).unwrap_or_default();
    let aggregates = compute_aggregates(&records, &by_oid);
    let hero = collect_hero_inputs(workdir, &records, &dag);
    tracing::debug!(
        records = records.len(),
        review_points = review_points.len(),
        secrets = secret_rows.len(),
        duplicates = dup_rows.len(),
        dag_nodes = dag.nodes.len(),
        milestones = hero.milestones.len(),
        decisions = hero.decisions.len(),
        "pr_render_body aggregates",
    );

    let mut body = String::new();
    body.push_str(MARKER);
    body.push('\n');

    // When secrets are present, promote the alert above the hero so the
    // screenshot-able first viewport is the security finding, not the goal
    // card. Reviewers can't miss a leak, and the dedicated banner is louder
    // than a chip in the badge row.
    let secrets_present = !secret_rows.is_empty();
    if secrets_present {
        body.push_str(&render_secret_alert_banner(secret_rows.len()));
        body.push_str(&render_secret_section(&secret_rows));
    }

    match style {
        PrStyle::Receipt => body.push_str(&render_hero_receipt(&aggregates, &hero, &secret_rows, &dup_rows)),
        PrStyle::Detective => body.push_str(&render_hero_detective(&aggregates, &hero, &secret_rows, &dup_rows)),
        PrStyle::Replay => body.push_str(&render_hero_replay(&aggregates, &hero, &dag, &secret_rows, &dup_rows)),
        PrStyle::Minimal => body.push_str(&render_hero_minimal(&aggregates, &hero, &secret_rows, &dup_rows)),
        PrStyle::Review => body.push_str(&render_hero_review(&aggregates, &hero, &dag, &secret_rows, &dup_rows)),
    }

    // Pass callouts. Security gets its own prominent `[!TIP]` callout
    // whenever the scan was clean — this is the marquee positive signal in
    // the comment and must be loud enough to be screenshot-able. Duplicate-
    // code pass gets a quieter `[!NOTE]` line for symmetry without competing
    // for attention. Either callout fires whether or not the *other* check
    // fired — they're independent signals.
    if secret_rows.is_empty() {
        body.push_str(&render_secret_pass_callout());
    }
    if dup_rows.is_empty() {
        body.push_str(&render_duplicate_pass_note());
    }

    // When secrets WEREN'T promoted to the top, emit the table here as before.
    if !secrets_present {
        body.push_str(&render_secret_section(&secret_rows));
    }
    body.push_str(&render_duplicate_section(&dup_rows));
    // Replay already drew the DAG above the fold; every other style keeps the
    // same collapsed click-to-expand DAG below the audit sections.
    if !matches!(style, PrStyle::Replay) {
        body.push_str(&render_swimlane_section(&dag));
    }
    // 💬 Agent coordination — branch-scoped i5h message threads. Sibling to the
    // reasoning DAG (collaboration context, not a headline), so it sits between
    // the DAG and the per-commit provenance. Self-omits when there's nothing
    // branch-relevant or when `--no-msg` is set.
    let branch = ctx::current_git_branch(workdir);
    if msg_opts.include {
        body.push_str(&render_coordination_section(repo.git(), &branch, msg_opts));
    }
    // 🪙 Token reduction — how much raw tool output `h5i capture run` kept out of
    // the agent's context on this branch. Self-omits when there were no captures
    // (or no net saving).
    body.push_str(&render_token_reduction_section(repo.git(), &branch));
    body.push_str(&render_per_commit_section(&records, &by_oid, &repo));

    body.push_str("---\n\n");
    body.push_str("<sub>Generated by <a href=\"https://github.com/Koukyosyumei/h5i\">h5i</a> · re-run <code>h5i share pr post</code> to refresh.</sub>\n");
    Ok(body)
}

/// Walk the records once to derive every counter the hero blocks need.
fn compute_aggregates(
    records: &[H5iCommitRecord],
    by_oid: &HashMap<String, &ReviewPoint>,
) -> Aggregates {
    let mut a = Aggregates {
        ai_count: 0,
        human_count: 0,
        total_tokens: 0,
        estimated_cost_usd: None,
        tests_passing: 0,
        tests_failing: 0,
        flagged_count: 0,
    };
    // We accumulate cost into a separate variable rather than directly into
    // `a.estimated_cost_usd` so we can distinguish "we never priced anything"
    // (→ None, honest) from "we priced zero dollars of work" (→ Some(0.0),
    // misleading: makes free-tier work look uncounted).
    let mut cost_total: f64 = 0.0;
    let mut cost_seen: bool = false;
    for r in records {
        let Some(meta) = r.ai_metadata.as_ref() else {
            a.human_count += 1;
            continue;
        };
        a.ai_count += 1;
        if let Some(u) = meta.usage.as_ref() {
            a.total_tokens = a.total_tokens.saturating_add(u.total_tokens);
            if let Some(c) = estimate_commit_cost(&meta.model_name, u) {
                cost_total += c;
                cost_seen = true;
            }
        }
        if let Some(tm) = r.test_metrics.as_ref() {
            if tm.total > 0 || tm.passed + tm.failed > 0 {
                if tm.is_passing() {
                    a.tests_passing += 1;
                } else {
                    a.tests_failing += 1;
                }
            }
        }
        if by_oid
            .get(&r.git_oid)
            .map(|p| p.should_flag_in_pr())
            .unwrap_or(false)
        {
            a.flagged_count += 1;
        }
    }
    if cost_seen {
        a.estimated_cost_usd = Some(cost_total);
    }
    a
}

/// Per-million-token list prices (USD) for the public Claude families. We
/// store them as `(input, output)` pairs and keep the table small —
/// estimating cost is a marketing flourish, not an accountancy guarantee,
/// so an unknown model degrades to `None` rather than mis-attributing
/// to an off-by-a-tier price.
fn model_price(model: &str) -> Option<(f64, f64)> {
    let m = model.to_ascii_lowercase();
    // Order matters: most-specific tier first, then family fallback.
    if m.contains("opus") {
        Some((15.0, 75.0))
    } else if m.contains("sonnet") {
        Some((3.0, 15.0))
    } else if m.contains("haiku") {
        Some((0.8, 4.0))
    } else {
        None
    }
}

/// Estimate the USD cost of a single commit's compute. Returns `None` if the
/// model name doesn't map to a known price tier — we'd rather print no figure
/// than a wrong one. The `TokenUsage` field names are misleading: `prompt`
/// = input, `content` = output (we keep them as-is for serde compatibility).
fn estimate_commit_cost(model: &str, usage: &crate::metadata::TokenUsage) -> Option<f64> {
    let (in_rate, out_rate) = model_price(if usage.model.is_empty() { model } else { &usage.model })?;
    let mtok = 1_000_000.0;
    Some(usage.prompt_tokens as f64 * in_rate / mtok + usage.content_tokens as f64 * out_rate / mtok)
}

/// Render a USD figure with the right resolution for the magnitude: pennies
/// get two decimals (`$0.04`), dollars get two decimals (`$1.32`), large
/// figures get a leading tilde and rounded cents (`~$12.50`). Centralised
/// so every style renders the same shape — consistency reads as polish.
fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        // Sub-penny work is "free" in any practical sense. Show "<$0.01"
        // rather than `$0.00` to make the smallness explicit.
        "<$0.01".to_string()
    } else if usd < 1.0 {
        format!("${:.2}", usd)
    } else if usd < 100.0 {
        format!("~${:.2}", usd)
    } else {
        format!("~${:.0}", usd)
    }
}

/// Pull together every soft-signal the narrative hero blocks reference.
/// Each field degrades gracefully — missing ctx workspace = empty milestones,
/// no decisions recorded = empty list, etc.
fn collect_hero_inputs(
    workdir: &Path,
    records: &[H5iCommitRecord],
    dag: &TraceDag,
) -> HeroInputs {
    let branch = ctx::current_git_branch(workdir);
    let branch_goal = ctx::git_branch_goal(workdir, &branch).unwrap_or_default();

    // Milestones from the cross-branch project context. Most recent last; the
    // hero renderers reverse-slice so the freshest line shows on top.
    let milestones = ctx::gcc_context(workdir, &ctx::ContextOpts::default())
        .map(|c| c.milestones)
        .unwrap_or_default();

    // Walk the DAG from newest to oldest for the THINK / OBSERVE picks. We use
    // the *latest* meaningful entry rather than scoring by length because
    // "most recent" is the strongest proxy for "what the PR is actually about".
    let top_think = dag
        .nodes
        .iter()
        .rev()
        .find(|n| n.kind == "THINK" && !n.content.trim().is_empty())
        .map(|n| n.content.clone());
    let top_observe = dag
        .nodes
        .iter()
        .rev()
        .find(|n| n.kind == "OBSERVE" && !n.content.trim().is_empty())
        .map(|n| n.content.clone());

    // Flatten decisions across all AI commits. Annotate each entry with the
    // commit it came from so the Detective renderer can deep-link.
    let mut decisions: Vec<DecisionEntry> = Vec::new();
    for r in records {
        if r.ai_metadata.is_none() {
            continue;
        }
        let short = r.git_oid[..r.git_oid.len().min(8)].to_string();
        for d in &r.decisions {
            decisions.push(DecisionEntry {
                location: d.location.clone(),
                choice: d.choice.clone(),
                alternatives: d.alternatives.clone(),
                reason: d.reason.clone(),
                short_oid: short.clone(),
            });
        }
    }

    // First non-empty prompt — records come back newest-first, so reverse to
    // pick the *earliest* prompt, which best captures the trigger of the work.
    let top_prompt = records
        .iter()
        .rev()
        .filter_map(|r| r.ai_metadata.as_ref().map(|m| m.prompt.trim().to_string()))
        .find(|p| !p.is_empty());

    HeroInputs {
        branch_goal,
        milestones,
        top_think,
        top_observe,
        decisions,
        top_prompt,
        dag_stats: compute_dag_stats(dag),
    }
}

// ── Style: Receipt ───────────────────────────────────────────────────────────
//
// The default layout. Optimised for the *screenshot* — the part of the
// comment a reviewer or social-share captures in one image. Three moves
// distinguish it from a stock GitHub comment:
//
//   1. H1 headline collapses the punchline into one line ("60% AI-authored ·
//      12.3k tokens · ~$0.04"). This is the only line guaranteed to be in any
//      screenshot, so it has to carry the whole story.
//   2. The goal lives in a native `> [!IMPORTANT]` callout (blue stripe on
//      github.com) — visually distinct from the `> [!CAUTION]` (red) we use
//      for leaks and `> [!NOTE]` (white) for checks-pass.
//   3. Stats use an HTML <table> instead of bullet points. Tables render
//      centred on github.com which gives the Stripe-receipt aesthetic;
//      bullet points would just look like a status comment.
//
// Compare to `Minimal` (escape hatch for internal PRs) which keeps the same
// data shape but rolls everything into a single blockquote with no big H1,
// no cost callouts, and no <table>.

fn render_hero_receipt(
    agg: &Aggregates,
    hero: &HeroInputs,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
) -> String {
    let mut s = String::new();

    // (1) Headline. Build only the parts that have data so a fresh PR with
    // no AI commits still produces something coherent.
    let mut headline_parts: Vec<String> = Vec::new();
    let total_commits = agg.ai_count + agg.human_count;
    if total_commits > 0 && agg.ai_count > 0 {
        let pct = (agg.ai_count as f64 / total_commits as f64 * 100.0).round() as usize;
        headline_parts.push(format!("{}% AI-authored", pct));
    }
    if agg.total_tokens > 0 {
        headline_parts.push(format!("{} tokens", format_tokens(agg.total_tokens)));
    }
    if let Some(usd) = agg.estimated_cost_usd {
        headline_parts.push(format_cost(usd));
    }
    if hero.dag_stats.files_touched > 0 {
        headline_parts.push(format!(
            "{} file{}",
            hero.dag_stats.files_touched,
            plural_s(hero.dag_stats.files_touched)
        ));
    }
    if headline_parts.is_empty() {
        // No AI commits at all — fall back to a neutral title so we never emit
        // a bare `# 🪙` with nothing after it (looks broken).
        s.push_str("# 🪙 h5i provenance\n\n");
    } else {
        let _ = writeln!(s, "# 🪙 {}", headline_parts.join(" · "));
        s.push('\n');
    }

    // Existing badge row — kept so the chip-style readout stays for power
    // users who want to scan secrets/tests/flagged counts at a glance.
    s.push_str(&render_badges(
        agg.ai_count,
        agg.total_tokens,
        secret_rows.len(),
        dup_rows.len(),
        agg.tests_passing,
        agg.tests_failing,
        agg.flagged_count,
    ));
    s.push_str("\n\n");

    // (2) Goal as a GitHub [!IMPORTANT] callout — distinct color from leaks.
    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "> [!IMPORTANT]\n> 🎯 **Goal:** {}\n",
            escape_md(&truncate(&hero.branch_goal, 240)),
        );
    }

    // (3) HTML stat card — six cells, fits one row at GitHub-comfortable width.
    s.push_str(&render_stat_card(agg, hero));

    // The prompt that triggered the work. Promoted out of the receipt blockquote
    // (where it was buried at the bottom) into its own labelled section because
    // it's the single most relatable artifact to a non-engineer reading the PR.
    if let Some(prompt) = &hero.top_prompt {
        s.push_str("### 💬 The ask\n\n");
        let _ = writeln!(
            s,
            "> \"{}\"\n",
            escape_md(&truncate(prompt, 240))
        );
    }

    // What shipped. We pull clean milestones first (human-written checkpoints);
    // if every milestone got filtered out as auto-trace noise, we leave the
    // section out rather than printing a header with no body.
    let clean = clean_milestones(&hero.milestones, 5);
    if !clean.is_empty() {
        s.push_str("### 📍 What shipped\n\n");
        for m in &clean {
            let _ = writeln!(s, "- ✓ {}", escape_md(&truncate(m, 140)));
        }
        s.push('\n');
    }

    s
}

/// Renders the centered six-cell stat card used by Receipt style. Pure HTML
/// because GFM tables don't centre-align reliably and the receipt aesthetic
/// depends on the symmetric grid. Cells are dropped if their underlying
/// signal is zero; a sparse card with two cells still reads as intentional.
fn render_stat_card(agg: &Aggregates, hero: &HeroInputs) -> String {
    let total_commits = agg.ai_count + agg.human_count;
    let mut cells: Vec<(String, &'static str)> = Vec::new();
    if total_commits > 0 {
        cells.push((format!("{} / {}", agg.ai_count, total_commits), "AI commits"));
    }
    if agg.total_tokens > 0 {
        cells.push((format_tokens(agg.total_tokens).to_string(), "tokens"));
    }
    if let Some(usd) = agg.estimated_cost_usd {
        cells.push((format_cost(usd), "est. cost"));
    }
    if hero.dag_stats.files_touched > 0 {
        cells.push((hero.dag_stats.files_touched.to_string(), "files touched"));
    }
    if let Some(r) = hero.dag_stats.read_to_edit {
        let cell = if r >= 1.0 {
            format!("{:.1} : 1", r)
        } else {
            format!("1 : {:.1}", 1.0 / r)
        };
        cells.push((cell, "READ : EDIT"));
    }
    if let Some(opt) = hero.dag_stats.ops_per_think {
        cells.push((
            format!("1 / {}", opt.round() as usize),
            "THINKs / ops",
        ));
    }
    if cells.is_empty() {
        return String::new();
    }

    let mut s = String::new();
    s.push_str("<table align=\"center\"><tr>");
    for (val, _) in &cells {
        let _ = write!(s, "<td align=\"center\"><strong>{}</strong></td>", val);
    }
    s.push_str("</tr><tr>");
    for (_, label) in &cells {
        let _ = write!(s, "<td align=\"center\"><sub>{}</sub></td>", label);
    }
    s.push_str("</tr></table>\n\n");
    s
}

/// Filters auto-trace noise out of the milestone list. The hook that writes
/// `h5i context commit` from `ACT` events produces strings like
/// `"edited src/pr.rs; edited src/pr.rs; edited src/pr.rs"` and
/// `"session ended (auto-checkpoint)"` — useful in the full trace, but pure
/// noise in a screenshot. We strip a leading `[x] ` (the rendered checkbox
/// the `gcc_context` extractor leaves on each line) before pattern-matching
/// so the filter sees the actual body text. Returns up to `take` cleaned
/// entries in newest-first order, capping at the cleanest few.
fn clean_milestones(raw: &[String], take: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Walk newest-first since reviewers care about the latest state of the
    // branch; the auto-trace tends to dominate the tail, so going from the
    // newest end lets us pick up human-written checkpoints early.
    for m in raw.iter().rev() {
        let body = m.trim().trim_start_matches("[x]").trim().trim_start_matches("[ ]").trim();
        if body.is_empty() {
            continue;
        }
        let lower = body.to_ascii_lowercase();
        // Auto-checkpoint emitted on session-end by the hook.
        if lower.starts_with("session ended") {
            continue;
        }
        // `edited X; edited Y; …` pattern — N "edited "s separated by `; `.
        // One "edited" is fine; two or more is the auto-trace concatenation.
        if body.matches("edited ").count() >= 2 || body.matches("wrote ").count() >= 2 {
            continue;
        }
        // Single-file mechanical entries: "edited src/foo.rs", "wrote a.py",
        // "deleted README.md". One verb + one path-shaped token, nothing
        // else. Prose like "edited authentication flow to use OAuth2"
        // survives because it has > 2 tokens.
        let tokens: Vec<&str> = body.split_whitespace().collect();
        if tokens.len() == 2
            && matches!(tokens[0].to_ascii_lowercase().as_str(), "edited" | "wrote" | "deleted")
        {
            continue;
        }
        out.push(body.to_string());
        if out.len() >= take {
            break;
        }
    }
    out.reverse(); // Restore chronological order (oldest of the kept entries first).
    out
}

// ── Style: Minimal ───────────────────────────────────────────────────────────

/// Quiet escape hatch from Receipt's marketing flourish. Same data, no H1
/// headline, no stat <table>, no IMPORTANT callout, no dollar figures.
/// Use when h5i provenance is informational rather than the point of the PR.
fn render_hero_minimal(
    agg: &Aggregates,
    hero: &HeroInputs,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
) -> String {
    let mut s = String::new();
    s.push_str("## 🪙 h5i provenance\n\n");
    s.push_str(&render_badges(
        agg.ai_count,
        agg.total_tokens,
        secret_rows.len(),
        dup_rows.len(),
        agg.tests_passing,
        agg.tests_failing,
        agg.flagged_count,
    ));
    s.push_str("\n\n");

    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "**🎯 Goal:** {}\n",
            escape_md(&truncate(&hero.branch_goal, 200)),
        );
    }
    let stats_line = format_dag_stats_inline(&hero.dag_stats);
    if !stats_line.is_empty() {
        let _ = writeln!(s, "**📊 By the numbers:** {}\n", stats_line);
    }
    // Minimal intentionally drops the prompt, milestones, and decisions —
    // the swim-lane DAG below the hero already shows the work; anyone who
    // wants narrative detail can switch to `--style detective`.
    s
}

// ── Style: Review ───────────────────────────────────────────────────────────

/// Reviewer-first triage brief. This intentionally avoids the marketing
/// signals from Receipt (cost, token headline) and leads with what a maintainer
/// needs to decide where to spend attention.
fn render_hero_review(
    agg: &Aggregates,
    hero: &HeroInputs,
    dag: &TraceDag,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
) -> String {
    let mut s = String::new();
    s.push_str("## h5i review brief\n\n");

    let _ = writeln!(
        s,
        "**Merge status:** {}\n",
        review_merge_status(secret_rows, dup_rows)
    );

    let focus = review_focus_files(dag, dup_rows, 3);
    if !focus.is_empty() {
        let _ = writeln!(
            s,
            "**Review focus:** {}\n",
            focus
                .iter()
                .map(|f| format!("`{}`", escape_md(f)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let evidence = review_evidence_line(agg, hero, dag);
    if !evidence.is_empty() {
        let _ = writeln!(s, "**Evidence:** {}\n", evidence);
    }

    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "> 🎯 **Goal:** {}\n",
            escape_md(&truncate(&hero.branch_goal, 240)),
        );
    }

    let checklist = review_checklist(agg, hero, dag, secret_rows, dup_rows, &focus);
    if !checklist.is_empty() {
        s.push_str("### Reviewer checklist\n\n");
        for item in checklist {
            let _ = writeln!(s, "- {}", item);
        }
        s.push('\n');
    }

    let highlights = review_reasoning_highlights(dag, 3);
    if !highlights.is_empty() {
        s.push_str("### Reasoning highlights\n\n");
        s.push_str("| Signal | Trace |\n");
        s.push_str("|---|---|\n");
        for h in highlights {
            let _ = writeln!(
                s,
                "| `{}` | {} |",
                h.kind,
                escape_md(&truncate(&h.content, 180))
            );
        }
        s.push('\n');
    }

    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReasoningHighlight {
    kind: String,
    content: String,
}

fn review_reasoning_highlights(dag: &TraceDag, limit: usize) -> Vec<ReasoningHighlight> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for preferred_kind in ["THINK", "NOTE"] {
        for n in dag.nodes.iter().rev() {
            if n.kind != preferred_kind {
                continue;
            }
            let content = n.content.trim().replace('\n', " ");
            if content.is_empty() || !seen.insert(content.clone()) {
                continue;
            }
            out.push(ReasoningHighlight {
                kind: n.kind.clone(),
                content,
            });
            if out.len() >= limit {
                return out;
            }
        }
    }

    out
}

fn review_merge_status(secret_rows: &[SecretRow], dup_rows: &[DupRow]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if secret_rows.is_empty() && dup_rows.is_empty() {
        parts.push("✅ ready for normal review".to_string());
    } else if secret_rows.is_empty() {
        parts.push("🟡 review needed".to_string());
    } else {
        parts.push(format!(
            "🛑 block merge: {} credential leak{}",
            secret_rows.len(),
            plural_s(secret_rows.len())
        ));
    }

    if secret_rows.is_empty() {
        parts.push("🔐 security clean".to_string());
    }
    if !dup_rows.is_empty() {
        let files: std::collections::BTreeSet<&str> =
            dup_rows.iter().map(|r| r.file.as_str()).collect();
        parts.push(format!(
            "🧬 {} duplicate-code finding{} in {} file{}",
            dup_rows.len(),
            plural_s(dup_rows.len()),
            files.len(),
            plural_s(files.len())
        ));
    }
    parts.join(" · ")
}

fn review_focus_files(dag: &TraceDag, dup_rows: &[DupRow], limit: usize) -> Vec<String> {
    let total = dag.nodes.len();
    let start = total.saturating_sub(DAG_NODE_LIMIT);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in dag.nodes.iter().skip(start) {
        if let Some(file) = extract_swimlane_file(n) {
            *counts.entry(file).or_default() += 1;
        }
    }
    for r in dup_rows {
        *counts.entry(r.file.clone()).or_default() += 1000;
    }

    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(limit).map(|(f, _)| f).collect()
}

fn review_evidence_line(agg: &Aggregates, hero: &HeroInputs, dag: &TraceDag) -> String {
    let mut parts: Vec<String> = Vec::new();
    if agg.ai_count > 0 {
        parts.push(format!("{} AI commit{}", agg.ai_count, plural_s(agg.ai_count)));
    }
    if hero.dag_stats.files_touched > 0 {
        parts.push(format!(
            "{} file{} touched",
            hero.dag_stats.files_touched,
            plural_s(hero.dag_stats.files_touched)
        ));
    }
    if !dag.nodes.is_empty() {
        parts.push(format!("{} trace node{}", dag.nodes.len(), plural_s(dag.nodes.len())));
    }
    if !hero.decisions.is_empty() {
        parts.push(format!(
            "{} decision{} recorded",
            hero.decisions.len(),
            plural_s(hero.decisions.len())
        ));
    }
    if agg.tests_passing > 0 {
        parts.push(format!(
            "{} commit{} with passing test evidence",
            agg.tests_passing,
            plural_s(agg.tests_passing)
        ));
    }
    parts.join(" · ")
}

fn review_checklist(
    agg: &Aggregates,
    hero: &HeroInputs,
    dag: &TraceDag,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
    focus: &[String],
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    if !secret_rows.is_empty() {
        items.push("Rotate exposed credentials and remove them from history before merge.".to_string());
    }
    if !dup_rows.is_empty() {
        let files: std::collections::BTreeSet<&str> =
            dup_rows.iter().map(|r| r.file.as_str()).collect();
        items.push(format!(
            "Inspect duplicate-code findings in {}.",
            files
                .iter()
                .map(|f| format!("`{}`", escape_md(f)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !focus.is_empty() {
        items.push(format!(
            "Start review with {}.",
            focus
                .iter()
                .map(|f| format!("`{}`", escape_md(f)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if agg.flagged_count > 0 {
        items.push(format!(
            "Open the per-commit provenance for {} flagged commit{}.",
            agg.flagged_count,
            plural_s(agg.flagged_count)
        ));
    }
    if hero.decisions.is_empty() {
        items.push("Confirm the implementation choices manually; no explicit decisions were recorded.".to_string());
    } else {
        items.push("Skim recorded decisions for rejected alternatives and reviewer intent.".to_string());
    }
    if dag.nodes.is_empty() {
        items.push("No reasoning trace was recorded; rely on commit provenance and code review.".to_string());
    }
    items.truncate(5);
    items
}

// ── Style: Detective ─────────────────────────────────────────────────────────

/// Narrative arc — reads like a mini blog post. Goal → considered → key
/// insight → shipped. Each section omits itself when its data is empty so a
/// fresh branch with no decisions/milestones still produces a coherent block.
fn render_hero_detective(
    agg: &Aggregates,
    hero: &HeroInputs,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
) -> String {
    let mut s = String::new();
    s.push_str("## 🪙 h5i provenance · _the story_\n\n");
    s.push_str(&render_badges(
        agg.ai_count,
        agg.total_tokens,
        secret_rows.len(),
        dup_rows.len(),
        agg.tests_passing,
        agg.tests_failing,
        agg.flagged_count,
    ));
    s.push_str("\n\n");

    // Act I — the goal, in a native [!IMPORTANT] callout so it pops visually
    // (blue stripe on github.com, distinct from the red [!CAUTION] and white
    // [!NOTE] we use elsewhere).
    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "> [!IMPORTANT]\n> 🎯 **Goal:** {}\n",
            escape_md(&truncate(&hero.branch_goal, 280)),
        );
    }

    // Process-shape stats — a tight one-liner between the goal and the
    // narrative so reviewers can scan the AI's working style without
    // scrolling to the DAG.
    let stats_line = format_dag_stats_inline(&hero.dag_stats);
    if !stats_line.is_empty() {
        s.push_str("### 📊 By the numbers\n\n");
        let _ = writeln!(s, "{}", stats_line);
        s.push('\n');
    }

    // Act II — what was considered. We surface up to 3 decisions; recording
    // every alternative would drown the screenshot, but the deep-link to each
    // commit lets reviewers expand on demand.
    if !hero.decisions.is_empty() {
        s.push_str("### 🧭 What we considered\n\n");
        for d in hero.decisions.iter().take(3) {
            let alts = if d.alternatives.is_empty() {
                "none recorded".to_string()
            } else {
                d.alternatives
                    .iter()
                    .map(|a| escape_md(a))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            let _ = writeln!(
                s,
                "- **{}** at `{}` (vs. {}){}  — `{}`",
                escape_md(&d.choice),
                escape_md(&d.location),
                alts,
                if d.reason.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n  - _Why:_ {}", escape_md(&truncate(&d.reason, 200)))
                },
                d.short_oid,
            );
        }
        if hero.decisions.len() > 3 {
            let _ = writeln!(s, "- _… and {} more — see per-commit section._", hero.decisions.len() - 3);
        }
        s.push('\n');
    }

    // Act III — the insight that unlocked the work.
    if let Some(think) = &hero.top_think {
        s.push_str("### 💡 Key insight\n\n");
        let _ = writeln!(s, "> {}", escape_md(&truncate(think, 320)));
        s.push('\n');
    } else if let Some(observe) = &hero.top_observe {
        s.push_str("### 💡 What we found\n\n");
        let _ = writeln!(s, "> {}", escape_md(&truncate(observe, 320)));
        s.push('\n');
    }

    // Act IV — what shipped. We use `clean_milestones` to drop auto-trace
    // noise; if every entry got filtered out we skip the section entirely
    // rather than emit an empty header.
    let clean = clean_milestones(&hero.milestones, 5);
    if !clean.is_empty() {
        s.push_str("### 🚢 What shipped\n\n");
        for m in &clean {
            let _ = writeln!(s, "- ✓ {}", escape_md(&truncate(m, 140)));
        }
        s.push('\n');
    }

    s
}

// ── Style: Replay ────────────────────────────────────────────────────────────

/// DAG-as-hero. We promote the existing reasoning-DAG renderer above the fold
/// (expanded, not collapsed) and annotate it with the goal + milestone trail
/// so the screenshot leads with the visually distinctive Mermaid graph.
fn render_hero_replay(
    agg: &Aggregates,
    hero: &HeroInputs,
    dag: &TraceDag,
    secret_rows: &[SecretRow],
    dup_rows: &[DupRow],
) -> String {
    let mut s = String::new();
    s.push_str("## 🪙 h5i provenance · _the replay_\n\n");
    s.push_str(&render_badges(
        agg.ai_count,
        agg.total_tokens,
        secret_rows.len(),
        dup_rows.len(),
        agg.tests_passing,
        agg.tests_failing,
        agg.flagged_count,
    ));
    s.push_str("\n\n");

    // Goal in a native [!IMPORTANT] callout, matching Receipt and Detective
    // for visual consistency across styles.
    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "> [!IMPORTANT]\n> 🎯 **Goal:** {}\n",
            escape_md(&truncate(&hero.branch_goal, 220)),
        );
    }

    // Process-shape stats above the DAG so the screenshot leads with the
    // graph plus a one-liner of context, before any narrative chrome.
    let stats_line = format_dag_stats_inline(&hero.dag_stats);
    if !stats_line.is_empty() {
        let _ = writeln!(s, "**📊 By the numbers:** {}", stats_line);
        s.push('\n');
    }

    // The DAG itself — rendered expanded (no <details> wrapper) so it lands
    // above the fold. Empty DAG → fallback note so the section never looks
    // like a render bug.
    if dag.nodes.is_empty() {
        s.push_str("_No reasoning trace recorded on this branch yet. Run `h5i context trace ...` while working to populate the replay._\n\n");
    } else {
        s.push_str("### 🧠 Reasoning by file\n\n");
        s.push_str(&render_swimlane_section_expanded(dag));
    }

    // Milestone trail beneath the graph, so reviewers can read the narrative
    // in markdown if the Mermaid block doesn't render (some clients block it).
    // We pull cleaned milestones so the arrow chain isn't dominated by
    // auto-trace `edited X; edited Y;` concatenations.
    let trail = clean_milestones(&hero.milestones, 6);
    if !trail.is_empty() {
        s.push_str("**Milestone trail:**\n\n");
        let line = trail
            .iter()
            .map(|m| format!("`{}`", escape_md(&truncate(m, 60))))
            .collect::<Vec<_>>()
            .join(" → ");
        s.push_str(&line);
        s.push_str("\n\n");
    }

    s
}

// ── Swim-lane DAG renderer ───────────────────────────────────────────────────
//
// The previous chain renderer showed causal order — useful when you're
// auditing why a particular decision happened, but visually it always collapsed
// to a single vertical sausage since real reasoning is mostly linear. The
// swim-lane renderer below trades causal edges for *file* density: one
// horizontal lane per file the AI touched, plus a "Reasoning" lane for
// THINK/NOTE/MERGE nodes that aren't bound to any file. The resulting
// silhouette tells a different story at a glance — "the AI revisited
// mcp.rs four times during the rewrite" — and it stays a DAG, so we keep the
// technical credibility of the chain view.

/// File-extension allowlist for path extraction from trace content. We can't
/// rely on slashes alone (root-level files exist; `Cargo.toml` is real) and we
/// can't rely on dots alone (numbers like `1.0` would false-positive). The
/// union gets us the right answer for every language we ship support for.
const SWIMLANE_FILE_EXTS: &[&str] = &[
    ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".java", ".kt",
    ".swift", ".c", ".h", ".cpp", ".cc", ".hpp", ".rb", ".php", ".css",
    ".scss", ".sass", ".html", ".htm", ".json", ".toml", ".yaml", ".yml",
    ".md", ".sh", ".sql", ".proto", ".graphql", ".lock", ".env",
];

/// Pulls a probable file path from a trace node's content. Returns `None` for
/// THINK/NOTE/MERGE (no file binding) and for OBSERVE/ACT whose content
/// doesn't end with something path-shaped. Heuristic mirrors how hook-emitted
/// traces are formatted (`"<verb> <path>"`) without false-positiving on prose
/// like `"Updated to 1.0"`.
fn extract_swimlane_file(node: &TraceNode) -> Option<String> {
    if !matches!(node.kind.as_str(), "OBSERVE" | "ACT") {
        return None;
    }
    let token = node.content.split_whitespace().next_back()?;
    if token.contains('/') {
        return Some(shorten_swimlane_path(token));
    }
    let lower = token.to_ascii_lowercase();
    if SWIMLANE_FILE_EXTS.iter().any(|e| lower.ends_with(e)) {
        return Some(token.to_string());
    }
    None
}

fn shorten_swimlane_path(path: &str) -> String {
    if !path.starts_with('/') {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for anchor in ["src", "tests", "web", "docs", "man", "assets", "scripts", "examples", ".github"] {
        if let Some(i) = parts.iter().position(|p| *p == anchor) {
            return parts[i..].join("/");
        }
    }
    parts.last().copied().unwrap_or(path).to_string()
}

/// Process-shape stats derived from a `TraceDag`. Used by the hero renderers
/// to surface "the AI's working style" in a single line of pull-quote facts:
/// did it read before editing? did it stop to think? how broad was the scope?
///
/// All counts cap at the visible window applied by [`DAG_NODE_LIMIT`]; this
/// matches what reviewers actually see in the rendered diagram so the
/// "📊 By the numbers" line never contradicts the swim-lane shape above it.
#[derive(Debug, Clone, Default)]
struct DagStats {
    /// Distinct files touched by an OBSERVE or ACT. THINK/NOTE never
    /// contribute (they're file-agnostic).
    files_touched: usize,
    observe_count: usize,
    act_count: usize,
    think_count: usize,
    /// `OBSERVE` count divided by `ACT` count. `None` when either side is
    /// zero (degenerate ratio); rendered as `"all read"` / `"all edit"` in
    /// that case.
    read_to_edit: Option<f64>,
    /// Number of ops (OBSERVE+ACT) per THINK. `None` when no THINK fired —
    /// "no reasoning recorded" is more honest than "∞ ops per THINK".
    ops_per_think: Option<f64>,
}

fn compute_dag_stats(dag: &TraceDag) -> DagStats {
    let total = dag.nodes.len();
    if total == 0 {
        return DagStats::default();
    }
    let start = total.saturating_sub(DAG_NODE_LIMIT);
    let visible: Vec<&TraceNode> = dag.nodes.iter().skip(start).collect();

    let mut stats = DagStats::default();
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for n in &visible {
        match n.kind.as_str() {
            "OBSERVE" => {
                stats.observe_count += 1;
                if let Some(f) = extract_swimlane_file(n) {
                    files.insert(f);
                }
            }
            "ACT" => {
                stats.act_count += 1;
                if let Some(f) = extract_swimlane_file(n) {
                    files.insert(f);
                }
            }
            "THINK" => stats.think_count += 1,
            _ => {}
        }
    }
    stats.files_touched = files.len();
    stats.read_to_edit = if stats.observe_count > 0 && stats.act_count > 0 {
        Some(stats.observe_count as f64 / stats.act_count as f64)
    } else {
        None
    };
    let total_ops = stats.observe_count + stats.act_count;
    stats.ops_per_think = if stats.think_count > 0 && total_ops > 0 {
        Some(total_ops as f64 / stats.think_count as f64)
    } else {
        None
    };
    stats
}

/// Format the stats as a compact one-line pull-quote, dropping any individual
/// signal whose underlying data was zero so the line never reads as a
/// rendering glitch (e.g. "0 files touched · 0 ops per THINK").
fn format_dag_stats_inline(stats: &DagStats) -> String {
    let mut parts: Vec<String> = Vec::new();
    if stats.files_touched > 0 {
        parts.push(format!(
            "**{}** file{} touched",
            stats.files_touched,
            plural_s(stats.files_touched)
        ));
    }
    match stats.read_to_edit {
        Some(r) if r >= 1.0 => parts.push(format!("READ:EDIT **{:.1}:1**", r)),
        Some(r) => parts.push(format!("READ:EDIT **1:{:.1}**", 1.0 / r)),
        None if stats.observe_count > 0 => parts.push("**read-only** (no edits)".into()),
        None if stats.act_count > 0 => parts.push("**all edits** (no reads first)".into()),
        _ => {}
    }
    if let Some(opt) = stats.ops_per_think {
        // Round to nearest integer for the "1 THINK per N ops" reading; high
        // density (N small) reads as careful work, low density (N large) as
        // execution-mode.
        parts.push(format!("1 THINK per **{}** ops", opt.round() as usize));
    }
    parts.join(" · ")
}

#[derive(Debug, Clone)]
struct SwimNode {
    /// Drives the mermaid `classDef` (`o`/`t`/`a`/`n`/`m`).
    kind: String,
    /// Pre-formatted label text. For file lanes this is just the verb
    /// (`READ`, `EDIT × 3`); for the reasoning lane it includes the truncated
    /// content so reviewers can see the actual thought.
    label: String,
}

#[derive(Debug, Clone)]
struct SwimLane {
    /// Mermaid-safe subgraph id (e.g. `lane_0`, `lane_reasoning`).
    key: String,
    /// Human-readable lane title shown above the row.
    title: String,
    nodes: Vec<SwimNode>,
}

/// Group visible nodes into swim lanes (one per touched file + a reasoning
/// lane + an overflow lane). Compresses consecutive same-kind nodes within
/// each lane so a lane with 6 reads renders as a single `READ × 6` box. The
/// reasoning lane is *not* compressed — every THINK / NOTE deserves its own
/// box since the content text is what reviewers came to read.
fn build_swimlanes(visible: &[&TraceNode]) -> Vec<SwimLane> {
    // Phase 1 — bucket nodes by lane key, preserving chronological order
    // within each bucket. We use a `Vec<(key, idx)>` so we can recover the
    // first-touch order for stable file-lane sorting later (ties get broken
    // by who appeared first, not by HashMap iteration order).
    let mut buckets: BTreeMap<String, Vec<&TraceNode>> = BTreeMap::new();
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    for (i, n) in visible.iter().enumerate() {
        let key = if matches!(n.kind.as_str(), "OBSERVE" | "ACT") {
            extract_swimlane_file(n).unwrap_or_else(|| "_other".to_string())
        } else {
            "_reasoning".to_string()
        };
        first_seen.entry(key.clone()).or_insert(i);
        buckets.entry(key).or_default().push(*n);
    }

    // Phase 2 — pick the top N file lanes by node count. Reasoning + Other
    // are special (always last/first respectively in the rendered output)
    // and don't count against the file cap.
    let mut file_lanes: Vec<(String, Vec<&TraceNode>)> = buckets
        .iter()
        .filter(|(k, _)| *k != "_reasoning" && *k != "_other")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    file_lanes.sort_by(|a, b| {
        // Primary: more touches first (heatmap effect — busy files lead).
        // Secondary: stable first-seen order so equal-count lanes stay in
        // the order they appeared in the trace.
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| first_seen.get(&a.0).cmp(&first_seen.get(&b.0)))
    });

    // Overflow files fold into the _other bucket so we never silently drop
    // trace nodes — the user wrote them, we render them somewhere.
    let mut overflow_into_other: Vec<&TraceNode> = Vec::new();
    if file_lanes.len() > SWIMLANE_MAX_FILES {
        for (_, nodes) in file_lanes.drain(SWIMLANE_MAX_FILES..) {
            overflow_into_other.extend(nodes);
        }
    }
    let mut other_bucket = buckets.remove("_other").unwrap_or_default();
    other_bucket.extend(overflow_into_other);

    // Phase 3 — materialise lanes in display order: Reasoning, then files
    // (count-desc), then Other. Skip any bucket that ended up empty.
    let mut lanes: Vec<SwimLane> = Vec::new();
    let mut lane_idx = 0usize;
    let push_lane =
        |lanes: &mut Vec<SwimLane>, lane_idx: &mut usize, title: String, raw: Vec<&TraceNode>, is_file_lane: bool| {
            if raw.is_empty() {
                return;
            }
            let nodes = compress_swim_run(&raw, is_file_lane);
            lanes.push(SwimLane {
                key: format!("lane_{}", *lane_idx),
                title,
                nodes,
            });
            *lane_idx += 1;
        };

    if let Some(r) = buckets.remove("_reasoning") {
        let title = format!("💭 Reasoning · {} step{}", r.len(), plural_s(r.len()));
        push_lane(&mut lanes, &mut lane_idx, title, r, false);
    }
    for (file, nodes) in file_lanes {
        let title = format!("📄 {} · {} op{}", file, nodes.len(), plural_s(nodes.len()));
        push_lane(&mut lanes, &mut lane_idx, title, nodes, true);
    }
    let other_title = format!("🗂 Other · {} node{}", other_bucket.len(), plural_s(other_bucket.len()));
    push_lane(&mut lanes, &mut lane_idx, other_title, other_bucket, false);

    lanes
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Compresses consecutive same-kind nodes within a single lane. In a file lane
/// (where the file is implicit from the lane title) the compressed label is
/// just the verb plus a count, e.g. `READ × 3`. In the reasoning / other
/// lanes the label keeps the content so the box stays informative.
fn compress_swim_run(raw: &[&TraceNode], file_lane: bool) -> Vec<SwimNode> {
    let mut out: Vec<SwimNode> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let kind = raw[i].kind.as_str();
        let mut j = i + 1;
        while j < raw.len() && raw[j].kind == kind {
            j += 1;
        }
        let run = j - i;
        if file_lane && run >= SWIMLANE_COMPRESS_RUN {
            let verb = verb_for_kind(kind);
            let label = if run > 1 {
                format!("{verb} × {run}")
            } else {
                verb.to_string()
            };
            out.push(SwimNode { kind: kind.to_string(), label });
        } else {
            for n in &raw[i..j] {
                out.push(SwimNode {
                    kind: n.kind.clone(),
                    label: swim_label(n, file_lane),
                });
            }
        }
        i = j;
    }
    out
}

fn verb_for_kind(kind: &str) -> &'static str {
    match kind {
        "OBSERVE" => "READ",
        "ACT" => "EDIT",
        "THINK" => "THINK",
        "NOTE" => "NOTE",
        "MERGE" => "MERGE",
        _ => "STEP",
    }
}

/// Build the label for an uncompressed swim-lane node. File lanes drop the
/// content (it would just repeat the lane title); reasoning/other lanes show
/// the content because that's the whole point of the lane.
fn swim_label(n: &TraceNode, file_lane: bool) -> String {
    let verb = verb_for_kind(&n.kind);
    if file_lane {
        return verb.to_string();
    }
    let oneline = shorten_paths_in_trace_content(&n.content.replace('\n', " "));
    let trimmed = truncate(&oneline, DAG_LABEL_BUDGET);
    let safe: String = trimmed
        .chars()
        .map(sanitize_mermaid_char)
        .collect();
    format!("{verb} · {safe}")
}

fn shorten_paths_in_trace_content(content: &str) -> String {
    content
        .split_whitespace()
        .map(|part| {
            if part.starts_with('/') && part.contains('/') {
                shorten_swimlane_path(part)
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace characters that break mermaid double-quoted labels with safe
/// look-alikes. Centralised so the chain renderer and swim-lane renderer
/// can't drift apart on what they consider dangerous.
fn sanitize_mermaid_char(c: char) -> char {
    match c {
        '"' => '\u{201D}', // right double quote
        '\\' => '/',
        '<' => '‹',
        '>' => '›',
        _ => c,
    }
}

/// Render the swim-lane DAG as a collapsible Mermaid block. Public surface
/// matches [`render_dag_section`] so callers can swap one for the other.
fn render_swimlane_section(dag: &TraceDag) -> String {
    if dag.nodes.is_empty() {
        return String::new();
    }
    let total = dag.nodes.len();
    let start = total.saturating_sub(DAG_NODE_LIMIT);
    let visible: Vec<&TraceNode> = dag.nodes.iter().skip(start).collect();
    let elided = total - visible.len();
    let lanes = build_swimlanes(&visible);
    if lanes.is_empty() {
        return String::new();
    }

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<details><summary><b>🧠 Reasoning by file</b> — {} node{} across {} lane{}{}</summary>",
        total,
        plural_s(total),
        lanes.len(),
        plural_s(lanes.len()),
        if elided > 0 {
            format!(", latest {} only", visible.len())
        } else {
            String::new()
        },
    );
    s.push('\n');
    s.push_str(&render_swimlane_mermaid(&lanes));
    s.push_str("</details>\n\n");
    s
}

/// Same content as [`render_swimlane_section`] but without the `<details>`
/// wrapper — used by the Replay hero which wants the diagram open by default.
fn render_swimlane_section_expanded(dag: &TraceDag) -> String {
    if dag.nodes.is_empty() {
        return String::new();
    }
    let total = dag.nodes.len();
    let start = total.saturating_sub(DAG_NODE_LIMIT);
    let visible: Vec<&TraceNode> = dag.nodes.iter().skip(start).collect();
    let lanes = build_swimlanes(&visible);
    if lanes.is_empty() {
        return String::new();
    }
    render_swimlane_mermaid(&lanes)
}

/// Pure mermaid emitter shared by the collapsed and expanded variants. Outer
/// graph direction is `TB` so lanes stack vertically; each subgraph forces
/// `direction LR` so nodes within a lane flow left-to-right (the swim-lane
/// shape). Edges are intra-lane only — cross-lane causal edges quickly turn
/// the graph into spaghetti and the user's whole motivation for switching
/// away from the chain view was visual clarity.
fn render_swimlane_mermaid(lanes: &[SwimLane]) -> String {
    let mut s = String::new();
    s.push_str(
        "\n```mermaid\n\
         %%{init: {\"flowchart\": {\"nodeSpacing\": 42, \"rankSpacing\": 48, \"diagramPadding\": 14}, \"themeVariables\": {\"fontSize\": \"18px\"}} }%%\n\
         flowchart TB\n",
    );
    for lane in lanes {
        let title: String = lane.title.chars().map(sanitize_mermaid_char).collect();
        let _ = writeln!(s, "  subgraph {key}[\"{title}\"]", key = lane.key);
        s.push_str("    direction LR\n");
        for (idx, node) in lane.nodes.iter().enumerate() {
            let _ = writeln!(
                s,
                "    {id}[\"{label}\"]:::{class}",
                id = mermaid_id(&format!("{}_{}", lane.key, idx)),
                label = node.label,
                class = mermaid_class(&node.kind),
            );
        }
        // Intra-lane chronological arrows. Two or more nodes → one arrow per
        // consecutive pair so the eye can read the flow left-to-right.
        for i in 0..lane.nodes.len().saturating_sub(1) {
            let _ = writeln!(
                s,
                "    {a} --> {b}",
                a = mermaid_id(&format!("{}_{}", lane.key, i)),
                b = mermaid_id(&format!("{}_{}", lane.key, i + 1)),
            );
        }
        s.push_str("  end\n");
    }
    s.push_str(
        "  classDef o fill:#dbeafe,stroke:#1e3a8a,color:#0b1c4a;\n\
         \x20\x20classDef t fill:#fef3c7,stroke:#92400e,color:#3f2d05;\n\
         \x20\x20classDef a fill:#dcfce7,stroke:#166534,color:#0a2e16;\n\
         \x20\x20classDef n fill:#ede9fe,stroke:#5b21b6,color:#221251;\n\
         \x20\x20classDef m fill:#e5e7eb,stroke:#374151,color:#0b0f17;\n",
    );
    s.push_str("```\n\n");
    s
}

/// Human-friendly token count: 12345 → "12.3k". Below 1000 stays as integer.
fn format_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn render_badges(
    ai_count: usize,
    total_tokens: usize,
    secrets: usize,
    duplicates: usize,
    tests_passing: usize,
    tests_failing: usize,
    flagged: usize,
) -> String {
    let tokens_label = if total_tokens >= 1000 {
        format!("{:.1}k", total_tokens as f64 / 1000.0)
    } else {
        total_tokens.to_string()
    };
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "`{ai_count} AI commit{}`",
        if ai_count == 1 { "" } else { "s" }
    ));
    if total_tokens > 0 {
        parts.push(format!("`{tokens_label} tokens`"));
    }
    if secrets > 0 {
        parts.push(format!("`🔒 {secrets} secret{}`", if secrets == 1 { "" } else { "s" }));
    }
    if duplicates > 0 {
        parts.push(format!(
            "`🔁 {duplicates} duplicate{}`",
            if duplicates == 1 { "" } else { "s" }
        ));
    }
    if tests_passing + tests_failing > 0 {
        parts.push(format!("`tests {tests_passing}✅ / {tests_failing}❌`"));
    }
    if flagged > 0 {
        parts.push(format!(
            "`🚩 {flagged} flagged`"
        ));
    }
    parts.join(" · ")
}

// ── 💬 Agent coordination (i5h message threads) ──────────────────────────────

/// i5h kinds whose bodies are review-relevant enough to excerpt by default.
/// These are authored to be shared (a review ask, a risk callout, a handoff)
/// and their typed replies; everything else (FYI, free-text) is metadata-only,
/// which keeps casual internal chatter out of a published PR body.
fn kind_gets_excerpt(kind: &str) -> bool {
    matches!(
        kind,
        "REVIEW_REQUEST" | "RISK" | "HANDOFF" | "ASK" | "ACK" | "DONE" | "DECLINE" | "BLOCKED"
    )
}

/// The i5h kind vocabulary. A `kind` from a pulled message is untrusted, so it
/// is checked against this set: a known kind renders as its readable label
/// (no entity-escaping noise like `REVIEW&#95;REQUEST`), anything else is
/// truncated and escaped so a hostile `kind` can't break out.
const KNOWN_KINDS: &[&str] = &[
    "FYI", "ASK", "REVIEW_REQUEST", "RISK", "BLOCKED", "HANDOFF", "ACK", "DONE", "DECLINE",
    "BROADCAST", "NOT_UNDERSTOOD",
];

/// Render a message kind label: readable for known kinds, escaped for untrusted
/// unknown ones. (The lone `_` in names like `REVIEW_REQUEST` is inert in
/// Markdown — a single underscore isn't emphasis — so it's safe to emit raw.)
fn render_kind(kind: &str) -> String {
    if KNOWN_KINDS.contains(&kind) {
        kind.to_string()
    } else {
        // Untrusted unknown kind: sanitize (drop control/newline bytes) before
        // truncate + escape, same fail-closed order as every other field.
        md_escape(&truncate_chars(&crate::msg::sanitize_display(kind), 24))
    }
}

/// Status glyph for a thread: ✅ once a `DONE`/`DECLINE` lands, 🟡 while a
/// request-type root is still open, • otherwise (informational).
fn thread_glyph(thread: &crate::msg::PrThread) -> &'static str {
    let closed = thread
        .messages
        .iter()
        .any(|m| matches!(m.effective_kind().as_str(), "DONE" | "DECLINE"));
    if closed {
        return "✅";
    }
    let root_kind = thread
        .messages
        .first()
        .map(|m| m.effective_kind())
        .unwrap_or_default();
    if matches!(
        root_kind.as_str(),
        "ASK" | "REVIEW_REQUEST" | "RISK" | "HANDOFF" | "BLOCKED"
    ) {
        "🟡"
    } else {
        "•"
    }
}

/// Truncate to at most `budget` characters, appending `…` when cut.
fn truncate_chars(s: &str, budget: usize) -> String {
    if s.chars().count() <= budget {
        return s.to_string();
    }
    let mut out: String = s.chars().take(budget.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escape the Markdown/HTML metacharacters that would let an untrusted string
/// break out of its rendered context in a PR comment — close the `<details>`,
/// inject a `<script>`, forge a table column, or trigger emphasis/links. Paired
/// with [`crate::msg::sanitize_display`] (control chars / newlines) and
/// [`crate::secrets::redact_text`] (credentials), this is the third layer.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '`' => out.push_str("&#96;"),
            '|' => out.push_str("&#124;"),
            '*' => out.push_str("&#42;"),
            '_' => out.push_str("&#95;"),
            '[' => out.push_str("&#91;"),
            ']' => out.push_str("&#93;"),
            '\\' => out.push_str("&#92;"),
            c => out.push(c),
        }
    }
    out
}

/// Strip control/escape bytes but keep line breaks (`\t`/`\r` fold to a space).
/// Used before redaction on multi-line bodies: dropping the `ESC` etc. *first*
/// means an ANSI-split credential is reassembled into contiguous text that the
/// redactor can then catch — sanitizing *after* redaction would instead
/// reconstruct a token the only redaction pass never saw (Codex RISK #4).
fn strip_controls_keep_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' | '\r' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Render the safe, display-ready text for one untrusted single-line field.
///
/// Order is fail-closed: sanitize (drop control/escape bytes, fold newlines)
/// FIRST so a token split by control bytes can't be reassembled *after* the
/// redaction pass; then redact secrets; then truncate (redact-before-truncate
/// so a secret near the cut can't be half-emitted); then Markdown/HTML-escape.
fn safe_field(raw: &str, budget: usize) -> String {
    let sanitized = crate::msg::sanitize_display(raw);
    let redacted = crate::secrets::redact_text(&sanitized);
    let truncated = truncate_chars(&redacted, budget);
    md_escape(&truncated)
}

/// Excerpt a message body with the same fail-closed ordering as [`safe_field`],
/// but newline-aware: control bytes are stripped *keeping* line breaks, then the
/// first non-empty line is taken (or, under `full`, newlines fold to spaces —
/// space, not nothing, so a token can't bridge two lines), THEN redaction runs.
fn excerpt_body(body: &str, full: bool) -> String {
    let cleaned = strip_controls_keep_newlines(body);
    let (line_src, budget) = if full {
        (cleaned.replace('\n', " "), MSG_FULL_BUDGET)
    } else {
        let first = cleaned
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        (first, MSG_EXCERPT_BUDGET)
    };
    let redacted = crate::secrets::redact_text(&line_src);
    let truncated = truncate_chars(&redacted, budget);
    md_escape(&truncated)
}

/// `2026-05-30T18:15:40.123Z` → `2026-05-30 18:15`. Absolute (not relative) so
/// the rendered comment doesn't rot; falls back to the raw value if unparseable.
fn short_ts(ts: &str) -> String {
    match (ts.get(0..10), ts.get(11..16)) {
        (Some(date), Some(time)) if ts.as_bytes().get(10) == Some(&b'T') => {
            format!("{date} {time}")
        }
        _ => ts.to_string(),
    }
}

/// One message line: `KIND · from → to · 2026-05-30 18:15`, plus focus/risk/
/// priority chips and (for review-typed kinds, or always under `--msg-bodies`)
/// an excerpt. All untrusted fields pass through [`safe_field`]/[`excerpt_body`].
fn render_message_line(m: &crate::msg::Message, full_bodies: bool, indent: bool) -> String {
    let mut s = String::new();
    let lead = if indent { "  - ↳ " } else { "- " };
    let kind = m.effective_kind();
    let _ = write!(
        s,
        "{lead}**{}** · {} → {} · {}",
        render_kind(&kind),
        safe_field(&m.from, 40),
        safe_field(&m.to, 40),
        // `ts` is untrusted: `short_ts` falls back to the raw value when it
        // can't parse, so the result must still be sanitized + escaped before it
        // lands in the comment (Codex RISK #3b).
        md_escape(&crate::msg::sanitize_display(&short_ts(&m.ts))),
    );
    if let Some(p) = m.priority.as_deref() {
        if matches!(p, "high" | "urgent") {
            let _ = write!(s, " · `prio:{}`", md_escape(p));
        }
    }
    if !m.focus.is_empty() {
        let chips: Vec<String> = m.focus.iter().take(3).map(|f| safe_field(f, 60)).collect();
        let _ = write!(s, " · 🎯 {}", chips.join(", "));
    }
    if let Some(r) = m.risk.as_deref() {
        if !r.is_empty() {
            let _ = write!(s, " · ⚠ {}", safe_field(r, 120));
        }
    }

    let show_excerpt = full_bodies || kind_gets_excerpt(&kind);
    if show_excerpt {
        let ex = excerpt_body(&m.body, full_bodies);
        if !ex.is_empty() {
            let _ = write!(s, " — {ex}");
        }
    } else {
        s.push_str(" _(metadata only)_");
    }
    s.push('\n');
    s
}

/// Render the collapsible 💬 Agent coordination section for `branch`. Returns
/// the empty string when there are no branch-relevant threads (self-omitting,
/// like the secret/duplicate sections).
fn render_coordination_section(
    repo: &git2::Repository,
    branch: &str,
    opts: &MsgOptions,
) -> String {
    let max = opts.max_threads.max(1);
    let (threads, total_threads) = crate::msg::threads_for_branch(repo, branch, max);
    if threads.is_empty() {
        return String::new();
    }
    let msg_count: usize = threads.iter().map(|t| t.messages.len()).sum();

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<details><summary><b>💬 Agent coordination</b> — {} message{} across {} thread{}{}</summary>",
        msg_count,
        plural_s(msg_count),
        threads.len(),
        plural_s(threads.len()),
        if total_threads > threads.len() {
            format!(", latest {} of {}", threads.len(), total_threads)
        } else {
            String::new()
        },
    );
    s.push('\n');

    // Git-proof + scope line: where the data came from and how to suppress it.
    let stats = crate::msg::stats(repo);
    let tip = stats.tip.unwrap_or_else(|| "—".into());
    let _ = writeln!(
        s,
        "<sub>From <code>refs/h5i/msg</code> @ <code>{tip}</code> · branch <code>{}</code>{} · <code>--no-msg</code> to omit{}.</sub>\n",
        md_escape(branch),
        if opts.full_bodies { "" } else { " · review-typed excerpts only" },
        if opts.full_bodies { "" } else { ", <code>--msg-bodies</code> for full" },
    );

    for t in &threads {
        let _ = write!(s, "{} ", thread_glyph(t));
        // First message is the thread root; the rest are indented replies.
        let mut msgs = t.messages.iter();
        if let Some(root) = msgs.next() {
            s.push_str(render_message_line(root, opts.full_bodies, false).trim_start_matches("- "));
        }
        for reply in msgs {
            s.push_str(&render_message_line(reply, opts.full_bodies, true));
        }
        s.push('\n');
    }

    s.push_str("</details>\n\n");
    s
}

fn render_per_commit_section(
    records: &[H5iCommitRecord],
    by_oid: &HashMap<String, &ReviewPoint>,
    repo: &H5iRepository,
) -> String {
    let ai_records: Vec<&H5iCommitRecord> = records
        .iter()
        .filter(|r| r.ai_metadata.is_some())
        .collect();
    if ai_records.is_empty() {
        return String::new();
    }
    let collapsible = ai_records.len() > 5;
    let mut s = String::new();
    if collapsible {
        let _ = writeln!(
            s,
            "<details><summary><b>📜 Per-commit provenance</b> — {} AI-authored commits</summary>\n",
            ai_records.len(),
        );
    } else {
        s.push_str("### 📜 Per-commit provenance\n\n");
    }

    for r in &ai_records {
        let short = &r.git_oid[..r.git_oid.len().min(8)];
        let ai = r.ai_metadata.as_ref().expect("filtered above");
        let _ = writeln!(
            s,
            "#### `{}` {}\n",
            short,
            escape_md(&first_line(&r.git_oid, repo))
        );
        let _ = writeln!(s, "- **prompt** — _{}_", escape_md(&truncate(&ai.prompt, 280)));
        let mut line = format!("- **model** `{}` · **agent** `{}`", ai.model_name, ai.agent_id);
        if let Some(u) = ai.usage.as_ref() {
            let _ = write!(line, " · **{}** tokens", u.total_tokens);
        }
        let _ = writeln!(s, "{}", line);
        if let Some(tm) = r.test_metrics.as_ref() {
            // Suppress empty test metrics — when `--tests` was passed but
            // no adapter produced counts, rendering "❌ 0/0 in 0.00s" is
            // worse than silence.
            if tm.total > 0 || tm.passed + tm.failed > 0 {
                let status = if tm.is_passing() { "✅" } else { "❌" };
                let _ = writeln!(
                    s,
                    "- **tests** — {status} {} passed / {} failed ({} total, {:.2}s)",
                    tm.passed, tm.failed, tm.total, tm.duration_secs
                );
            }
        }
        if !r.decisions.is_empty() {
            s.push_str("- **decisions**\n");
            for d in &r.decisions {
                let alts = if d.alternatives.is_empty() {
                    "no alternatives recorded".to_string()
                } else {
                    d.alternatives
                        .iter()
                        .map(|a| escape_md(a))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let _ = writeln!(
                    s,
                    "  - `{}` — {} _(vs. {})_",
                    escape_md(&d.location),
                    escape_md(&d.choice),
                    alts,
                );
            }
        }
        if let Some(p) = by_oid.get(&r.git_oid).copied() {
            if p.should_flag_in_pr() {
                let quality_rules: Vec<String> =
                    p.quality_triggers().map(|t| t.rule_id.clone()).collect();
                let _ = writeln!(
                    s,
                    "- 🚩 **review signals** — score {:.2}: {}",
                    p.quality_score,
                    escape_md(&quality_rules.join(", "))
                );
                let shape_rules: Vec<String> =
                    p.shape_triggers().map(|t| t.rule_id.clone()).collect();
                if !shape_rules.is_empty() {
                    let _ = writeln!(
                        s,
                        "  - _shape signals (informational):_ {}",
                        escape_md(&shape_rules.join(", "))
                    );
                }
            }
        }
        s.push('\n');
    }

    if collapsible {
        s.push_str("</details>\n\n");
    }
    s
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{TraceDag, TraceNode};

    // ── parse_secret_detail ───────────────────────────────────────────────

    #[test]
    fn parses_secret_detail_with_path() {
        let row = parse_secret_detail(
            "AWS access key ID matched in src/cfg.py (line 42, preview `AKIA…`)",
            "CREDENTIAL_LEAK",
            "a3f8c12e",
        )
        .expect("should parse");
        assert_eq!(row.rule_id, "CREDENTIAL_LEAK");
        assert_eq!(row.description, "AWS access key ID");
        assert_eq!(row.file.as_deref(), Some("src/cfg.py"));
        assert_eq!(row.line, 42);
        assert_eq!(row.preview, "AKIA…");
        assert_eq!(row.short_oid, "a3f8c12e");
    }

    #[test]
    fn parses_secret_detail_without_path() {
        let row = parse_secret_detail(
            "Stripe key matched (line 7, preview `sk_li…`)",
            "CREDENTIAL_LEAK",
            "deadbeef",
        )
        .expect("should parse fallback shape");
        assert_eq!(row.description, "Stripe key");
        assert!(row.file.is_none());
        assert_eq!(row.line, 7);
        assert_eq!(row.preview, "sk_li…");
    }

    #[test]
    fn rejects_secret_detail_garbage() {
        assert!(parse_secret_detail("totally malformed", "CREDENTIAL_LEAK", "x").is_none());
        assert!(parse_secret_detail("", "CREDENTIAL_LEAK", "x").is_none());
    }

    // ── parse_duplicate_detail ────────────────────────────────────────────

    #[test]
    fn parses_duplicate_detail() {
        let row = parse_duplicate_detail(
            "12 duplicated lines in 'src/foo/bar.rs': block first seen at line 30, repeated at line 88",
            "deadbeef",
        )
        .expect("should parse");
        assert_eq!(row.block_len, 12);
        assert_eq!(row.file, "src/foo/bar.rs");
        assert_eq!(row.first_line, 30);
        assert_eq!(row.repeat_line, 88);
        assert_eq!(row.short_oid, "deadbeef");
    }

    #[test]
    fn rejects_duplicate_detail_garbage() {
        assert!(parse_duplicate_detail("12 duplicated lines but no path here", "x").is_none());
        assert!(parse_duplicate_detail("", "x").is_none());
    }

    // ── Section rendering ─────────────────────────────────────────────────

    #[test]
    fn empty_sections_render_to_empty_string() {
        assert!(render_secret_section(&[]).is_empty());
        assert!(render_duplicate_section(&[]).is_empty());
        assert!(render_swimlane_section(&TraceDag::default()).is_empty());
    }

    #[test]
    fn secret_pass_callout_uses_tip_alert_and_shouts_about_security() {
        let s = render_secret_pass_callout();
        assert!(s.starts_with("> [!TIP]"), "must use GitHub TIP (green) alert: {s}");
        // The h3 heading inside the callout gives it visual weight — the
        // signal must read as a marquee result, not a footnote.
        assert!(s.contains("### ✅ Security scan clean"), "got: {s}");
        assert!(s.contains("No credentials leaked"));
    }

    #[test]
    fn duplicate_pass_note_is_quieter_than_security_pass() {
        let s = render_duplicate_pass_note();
        // Duplicate-code pass is a craft signal, not a security signal —
        // [!NOTE] (white) is intentionally less prominent than [!TIP] (green).
        assert!(s.starts_with("> [!NOTE]"), "must use GitHub NOTE alert: {s}");
        assert!(s.contains("Duplicate-code scan clean"));
        // No h3 — heading would put it on visual par with the security callout.
        assert!(!s.contains("###"), "must not use h3 heading: {s}");
    }

    #[test]
    fn secret_section_uses_caution_alert_and_table() {
        let rows = vec![SecretRow {
            rule_id: "AWS_ACCESS_KEY_ID".into(),
            description: "AWS access key".into(),
            file: Some("src/cfg.py".into()),
            line: 42,
            preview: "AKIA…".into(),
            short_oid: "a3f8c12e".into(),
        }];
        let s = render_secret_section(&rows);
        assert!(s.contains("> [!CAUTION]"), "must use GitHub CAUTION alert");
        assert!(s.contains("credential leak"));
        assert!(s.contains("| `AWS_ACCESS_KEY_ID` | `src/cfg.py` | 42 | `AKIA…` | `a3f8c12e` |"));
    }

    #[test]
    fn secret_section_pluralizes_correctly() {
        let one = vec![SecretRow {
            rule_id: "X".into(),
            description: "d".into(),
            file: None,
            line: 1,
            preview: "p".into(),
            short_oid: "abc12345".into(),
        }];
        let s = render_secret_section(&one);
        assert!(s.contains("1 credential leak across 1 commit"), "got: {s}");

        let two = vec![
            SecretRow {
                rule_id: "X".into(),
                description: "d".into(),
                file: None,
                line: 1,
                preview: "p".into(),
                short_oid: "abc12345".into(),
            },
            SecretRow {
                rule_id: "Y".into(),
                description: "d".into(),
                file: None,
                line: 2,
                preview: "p".into(),
                short_oid: "def67890".into(),
            },
        ];
        let s = render_secret_section(&two);
        assert!(s.contains("2 credential leaks across 2 commits"), "got: {s}");
    }

    #[test]
    fn duplicate_section_uses_warning_alert_and_groups_files() {
        let rows = vec![
            DupRow {
                file: "src/a.rs".into(),
                block_len: 8,
                first_line: 10,
                repeat_line: 88,
                short_oid: "aaaaaaaa".into(),
            },
            DupRow {
                file: "src/b.rs".into(),
                block_len: 12,
                first_line: 4,
                repeat_line: 30,
                short_oid: "bbbbbbbb".into(),
            },
        ];
        let s = render_duplicate_section(&rows);
        assert!(s.contains("> [!WARNING]"));
        assert!(s.contains("Duplicate code introduced in 2 files"));
        assert!(s.contains("`src/a.rs`"));
        assert!(s.contains("L10 → L88"));
        assert!(s.contains("`src/b.rs`"));
        assert!(s.contains("L4 → L30"));
    }

    // ── Mermaid DAG rendering ─────────────────────────────────────────────

    fn make_node(id: &str, kind: &str, content: &str, parents: &[&str]) -> TraceNode {
        TraceNode {
            id: id.to_string(),
            parent_ids: parents.iter().map(|s| s.to_string()).collect(),
            kind: kind.to_string(),
            content: content.to_string(),
            timestamp: "2026-05-15T10:00:00Z".to_string(),
        }
    }

    // ── Swim-lane DAG ─────────────────────────────────────────────────────

    #[test]
    fn extract_file_pulls_path_from_observe_act() {
        assert_eq!(
            extract_swimlane_file(&make_node("a", "OBSERVE", "read src/foo.rs", &[])),
            Some("src/foo.rs".into())
        );
        assert_eq!(
            extract_swimlane_file(&make_node(
                "abs",
                "ACT",
                "edited /home/user/project/src/foo.rs",
                &[]
            )),
            Some("src/foo.rs".into())
        );
        assert_eq!(
            extract_swimlane_file(&make_node(
                "root",
                "ACT",
                "edited /home/user/project/MANUAL.md",
                &[]
            )),
            Some("MANUAL.md".into())
        );
        assert_eq!(
            extract_swimlane_file(&make_node("b", "ACT", "edited Cargo.toml", &[])),
            Some("Cargo.toml".into())
        );
        // Bare extension without slash still counts.
        assert_eq!(
            extract_swimlane_file(&make_node("c", "ACT", "edited foo.py", &[])),
            Some("foo.py".into())
        );
    }

    #[test]
    fn extract_file_returns_none_for_non_file_traces() {
        // THINK/NOTE never bind to a file even if content happens to mention one.
        assert!(extract_swimlane_file(&make_node("a", "THINK", "src/foo.rs is buggy", &[])).is_none());
        assert!(extract_swimlane_file(&make_node("b", "NOTE", "TODO: fix bar.rs", &[])).is_none());
        // Prose with no path-shaped token.
        assert!(extract_swimlane_file(&make_node("c", "OBSERVE", "thought about it", &[])).is_none());
        // The "1.0" trap — a dot doesn't make something a path.
        assert!(extract_swimlane_file(&make_node("d", "ACT", "bumped version to 1.0", &[])).is_none());
    }

    #[test]
    fn swimlanes_bucket_by_file_with_reasoning_first() {
        let nodes = [
            make_node("a1", "OBSERVE", "read src/foo.rs", &[]),
            make_node("a2", "THINK", "consider split", &["a1"]),
            make_node("a3", "ACT", "edited src/foo.rs", &["a2"]),
            make_node("a4", "OBSERVE", "read src/bar.rs", &["a3"]),
            make_node("a5", "NOTE", "TODO: tests", &["a4"]),
        ];
        let visible: Vec<&TraceNode> = nodes.iter().collect();
        let lanes = build_swimlanes(&visible);
        // Reasoning lane is always first, then files by node count.
        assert!(lanes[0].title.contains("Reasoning"));
        assert_eq!(lanes[0].nodes.len(), 2, "THINK + NOTE land in reasoning");
        // foo.rs has 2 ops (read + edit); bar.rs has 1.
        assert!(lanes[1].title.contains("src/foo.rs"));
        assert!(lanes[1].title.contains("2 op"));
        assert!(lanes[2].title.contains("src/bar.rs"));
    }

    #[test]
    fn swimlanes_compress_consecutive_same_kind_in_file_lane() {
        // 4 consecutive READs on the same file → one `READ × 4` node.
        let nodes: Vec<TraceNode> = (0..4)
            .map(|i| make_node(&format!("r{i}"), "OBSERVE", "read src/foo.rs", &[]))
            .collect();
        let visible: Vec<&TraceNode> = nodes.iter().collect();
        let lanes = build_swimlanes(&visible);
        assert_eq!(lanes.len(), 1, "only the foo.rs lane");
        assert_eq!(lanes[0].nodes.len(), 1, "compressed into 1 node");
        assert_eq!(lanes[0].nodes[0].label, "READ × 4");
    }

    #[test]
    fn swimlanes_compress_keeps_distinct_kinds_separate() {
        // READ → EDIT → READ on the same file must NOT collapse — kind alternates.
        let nodes = [
            make_node("r1", "OBSERVE", "read src/foo.rs", &[]),
            make_node("e1", "ACT", "edited src/foo.rs", &["r1"]),
            make_node("r2", "OBSERVE", "read src/foo.rs", &["e1"]),
        ];
        let visible: Vec<&TraceNode> = nodes.iter().collect();
        let lanes = build_swimlanes(&visible);
        assert_eq!(lanes[0].nodes.len(), 3, "kinds alternate; no compression");
        let labels: Vec<&str> = lanes[0].nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["READ", "EDIT", "READ"]);
    }

    #[test]
    fn swimlanes_overflow_into_other_lane() {
        // SWIMLANE_MAX_FILES + 2 distinct files → top N file lanes + Other.
        let mut nodes: Vec<TraceNode> = Vec::new();
        for i in 0..(SWIMLANE_MAX_FILES + 2) {
            // First file gets the most ops so file-count sort is unambiguous.
            let count = if i == 0 { 5 } else { 1 };
            for j in 0..count {
                nodes.push(make_node(
                    &format!("n{i}_{j}"),
                    "ACT",
                    &format!("edited src/f{i}.rs"),
                    &[],
                ));
            }
        }
        let visible: Vec<&TraceNode> = nodes.iter().collect();
        let lanes = build_swimlanes(&visible);
        let titles: Vec<&str> = lanes.iter().map(|l| l.title.as_str()).collect();
        // No reasoning lane (all ACT). MAX file lanes + Other.
        assert_eq!(lanes.len(), SWIMLANE_MAX_FILES + 1);
        assert!(titles.last().unwrap().contains("Other"));
        // The first lane (most ops) must be f0.rs at the top of the heatmap.
        assert!(lanes[0].title.contains("src/f0.rs"));
    }

    #[test]
    fn swimlane_mermaid_emits_subgraphs_and_lane_arrows() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a1", "OBSERVE", "read src/foo.rs", &[]),
                make_node("a2", "ACT", "edited src/foo.rs", &["a1"]),
                make_node("a3", "THINK", "key insight", &["a2"]),
            ],
        };
        let s = render_swimlane_section(&dag);
        assert!(s.starts_with("<details>"));
        assert!(s.contains("fontSize"), "Mermaid readability config should stay present");
        assert!(s.contains("nodeSpacing"), "Mermaid spacing config should stay present");
        assert!(s.contains("flowchart TB"), "outer direction is top-bottom");
        assert!(s.contains("subgraph lane_0"));
        assert!(s.contains("direction LR"), "each lane forces internal LR");
        assert!(s.contains("classDef o"), "OBSERVE class still defined");
        // Reasoning lane comes first (THINK), then the file lane.
        let reasoning_pos = s.find("💭 Reasoning").expect("reasoning lane title");
        let file_pos = s.find("📄 src/foo.rs").expect("file lane title");
        assert!(reasoning_pos < file_pos);
        // Intra-lane arrow between the two foo.rs ops. Rendered IDs are
        // lane-local so duplicate trace IDs cannot collide in Mermaid.
        assert!(s.contains("n_lane_1_0 --> n_lane_1_1"));
        assert!(s.contains("</details>"));
    }

    #[test]
    fn swimlane_labels_shorten_absolute_paths() {
        let label = swim_label(
            &make_node("a", "NOTE", "edited /home/user/project/src/foo.rs", &[]),
            false,
        );
        assert!(label.contains("src/foo.rs"), "got: {label}");
        assert!(!label.contains("/home/user/project"), "got: {label}");
    }

    #[test]
    fn swimlane_expanded_drops_details_wrapper() {
        let dag = TraceDag {
            nodes: vec![make_node("a1", "ACT", "edited src/foo.rs", &[])],
        };
        let s = render_swimlane_section_expanded(&dag);
        assert!(!s.contains("<details>"), "expanded variant must not collapse: {s}");
        assert!(s.contains("flowchart TB"));
    }

    #[test]
    fn swimlane_empty_dag_renders_nothing() {
        assert!(render_swimlane_section(&TraceDag::default()).is_empty());
        assert!(render_swimlane_section_expanded(&TraceDag::default()).is_empty());
    }

    #[test]
    fn swimlane_sanitizes_dangerous_chars_in_labels_and_titles() {
        // A "file" path with quotes/brackets must round-trip through the
        // lane title and the node label without breaking Mermaid.
        let dag = TraceDag {
            nodes: vec![
                make_node("a1", "ACT", "edited weird\"path<x>.rs", &[]),
                make_node("a2", "THINK", "weird \"thought\" <html>", &["a1"]),
            ],
        };
        let s = render_swimlane_section(&dag);
        // No raw double-quotes inside subgraph titles (would close the title early).
        for line in s.lines().filter(|l| l.contains("subgraph")) {
            let after_bracket = line.find('[').unwrap();
            let inner = &line[after_bracket + 1..line.rfind(']').unwrap()];
            // The wrapping quotes are at the boundary; anything in between
            // must be a smart quote, not a raw `"`.
            assert!(!inner[1..inner.len() - 1].contains('"'), "raw quote in title: {line}");
            assert!(!inner.contains('<'), "raw < in title: {line}");
            assert!(!inner.contains('>'), "raw > in title: {line}");
        }
    }

    // ── Aggregation ───────────────────────────────────────────────────────

    fn fake_review_point(short: &str, oid: &str, rule_id: &str, detail: &str) -> ReviewPoint {
        use crate::review::{ReviewTrigger, Tier};
        ReviewPoint {
            commit_oid: oid.to_string(),
            short_oid: short.to_string(),
            message: "msg".into(),
            author: "a".into(),
            timestamp: chrono::Utc::now(),
            score: 1.0,
            quality_score: 1.0,
            shape_score: 0.0,
            triggers: vec![ReviewTrigger {
                rule_id: rule_id.into(),
                weight: 0.5,
                detail: detail.into(),
                tier: Tier::Quality,
            }],
        }
    }

    fn fake_record(oid: &str) -> H5iCommitRecord {
        H5iCommitRecord {
            git_oid: oid.into(),
            parent_oid: None,
            ai_metadata: None,
            test_metrics: None,
            ast_hashes: None,
            timestamp: chrono::Utc::now(),
            caused_by: Vec::new(),
            decisions: Vec::new(),
            env_provenance: None,
        }
    }

    #[test]
    fn collect_secret_rows_only_picks_credential_leak_triggers() {
        let oid = "abc123de00000000";
        let rp = fake_review_point(
            "abc123de",
            oid,
            "CREDENTIAL_LEAK",
            "AWS access key matched in src/cfg.py (line 42, preview `AKIA…`)",
        );
        let by_oid: HashMap<String, &ReviewPoint> =
            std::iter::once((oid.to_string(), &rp)).collect();
        let records = vec![fake_record(oid)];
        let rows = collect_secret_rows(&records, &by_oid);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule_id, "CREDENTIAL_LEAK");
        assert_eq!(rows[0].file.as_deref(), Some("src/cfg.py"));
    }

    #[test]
    fn collect_duplicate_rows_only_picks_duplicated_code_triggers() {
        let oid = "abc123de00000000";
        let rp = fake_review_point(
            "abc123de",
            oid,
            "DUPLICATED_CODE",
            "12 duplicated lines in 'src/a.rs': block first seen at line 30, repeated at line 88",
        );
        let by_oid: HashMap<String, &ReviewPoint> =
            std::iter::once((oid.to_string(), &rp)).collect();
        let records = vec![fake_record(oid)];
        let rows = collect_duplicate_rows(&records, &by_oid);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, "src/a.rs");
        assert_eq!(rows[0].block_len, 12);
    }

    // ── Badges ────────────────────────────────────────────────────────────

    #[test]
    fn badges_omit_optional_segments_when_zero() {
        let s = render_badges(3, 4200, 0, 0, 0, 0, 0);
        assert!(s.contains("`3 AI commits`"));
        assert!(s.contains("`4.2k tokens`"));
        assert!(!s.contains("secret"));
        assert!(!s.contains("duplicate"));
        assert!(!s.contains("tests"));
        assert!(!s.contains("flagged"));
    }

    #[test]
    fn badges_pluralize_and_format_thousands() {
        let s = render_badges(1, 850, 1, 2, 5, 1, 1);
        assert!(s.contains("`1 AI commit`"), "got: {s}");
        assert!(s.contains("`850 tokens`"));
        assert!(s.contains("`🔒 1 secret`"));
        assert!(s.contains("`🔁 2 duplicates`"));
        assert!(s.contains("`tests 5✅ / 1❌`"));
        assert!(s.contains("`🚩 1 flagged`"));
    }

    // ── hero renderers ────────────────────────────────────────────────────

    fn sample_aggregates() -> Aggregates {
        Aggregates {
            ai_count: 4,
            human_count: 1,
            total_tokens: 12_345,
            estimated_cost_usd: Some(0.0432),
            tests_passing: 2,
            tests_failing: 0,
            flagged_count: 0,
        }
    }

    fn sample_hero() -> HeroInputs {
        HeroInputs {
            branch_goal: "Add retry logic to the HTTP client".into(),
            milestones: vec![
                "Read existing client".into(),
                "Implement retry loop".into(),
                "Add timeout parameter".into(),
            ],
            top_think: Some("Exponential backoff with jitter is safest".into()),
            top_observe: Some("HttpClient::send has no retry logic".into()),
            decisions: vec![DecisionEntry {
                location: "src/http.rs:88".into(),
                choice: "exponential backoff with jitter".into(),
                alternatives: vec!["fixed delay".into(), "linear backoff".into()],
                reason: "reduces thundering herd under high load".into(),
                short_oid: "a3f8c12e".into(),
            }],
            top_prompt: Some("Add exponential backoff to the HTTP client".into()),
            dag_stats: DagStats {
                files_touched: 3,
                observe_count: 6,
                act_count: 3,
                think_count: 1,
                read_to_edit: Some(2.0),
                ops_per_think: Some(9.0),
            },
        }
    }

    #[test]
    fn receipt_hero_headlines_and_card() {
        let body = render_hero_receipt(&sample_aggregates(), &sample_hero(), &[], &[]);
        // (1) H1 headline rolls up the punchline stats.
        assert!(body.starts_with("# 🪙 "), "must lead with H1 headline: {body}");
        assert!(body.contains("80% AI-authored"), "ratio: {body}");
        assert!(body.contains("12.3k tokens"));
        assert!(body.contains("$0.04"), "cost in headline: {body}");
        assert!(body.contains("3 files"), "files in headline: {body}");
        // (2) Goal in an IMPORTANT callout, not buried in the receipt blockquote.
        assert!(body.contains("> [!IMPORTANT]\n> 🎯 **Goal:** Add retry logic"));
        // (3) Stat card present and centred.
        assert!(body.contains("<table align=\"center\">"));
        assert!(body.contains("<strong>4 / 5</strong>"), "AI/total cell: {body}");
        assert!(body.contains("<sub>est. cost</sub>"));
        // (4) The ask now has its own section, prompt promoted out of the card.
        assert!(body.contains("### 💬 The ask"));
        assert!(body.contains("\"Add exponential backoff"));
        // (5) Milestones section — cleaned, no auto-trace junk in the fixture
        // so all three sample milestones survive.
        assert!(body.contains("### 📍 What shipped"));
        assert!(body.contains("Add timeout parameter"));
    }

    #[test]
    fn receipt_hero_omits_blank_signals_gracefully() {
        let empty = HeroInputs {
            branch_goal: String::new(),
            milestones: vec![],
            top_think: None,
            top_observe: None,
            decisions: vec![],
            top_prompt: None,
            dag_stats: DagStats::default(),
        };
        let agg = Aggregates {
            ai_count: 0,
            human_count: 0,
            total_tokens: 0,
            estimated_cost_usd: None,
            tests_passing: 0,
            tests_failing: 0,
            flagged_count: 0,
        };
        let body = render_hero_receipt(&agg, &empty, &[], &[]);
        // Empty data → neutral H1 fallback so we never emit a bare emoji header.
        assert!(body.contains("# 🪙 h5i provenance"));
        assert!(!body.contains("Goal:"));
        assert!(!body.contains("Milestones"));
        assert!(!body.contains("Triggering prompt"));
    }

    #[test]
    fn detective_hero_lays_out_four_acts() {
        let body = render_hero_detective(&sample_aggregates(), &sample_hero(), &[], &[]);
        // Goal is now in an [!IMPORTANT] callout, not a section header.
        assert!(body.contains("> [!IMPORTANT]\n> 🎯 **Goal:** Add retry logic"));
        assert!(body.contains("### 🧭 What we considered"));
        assert!(body.contains("### 💡 Key insight"));
        assert!(body.contains("### 🚢 What shipped"));
        // Decision payload reaches the rendered output.
        assert!(body.contains("exponential backoff with jitter"));
        assert!(body.contains("fixed delay"));
        assert!(body.contains("`a3f8c12e`"));
        // Key insight quotes the THINK, not the OBSERVE.
        assert!(body.contains("Exponential backoff with jitter is safest"));
        assert!(!body.contains("has no retry logic"));
    }

    #[test]
    fn detective_hero_falls_back_to_observe_when_no_think() {
        let mut hero = sample_hero();
        hero.top_think = None;
        let body = render_hero_detective(&sample_aggregates(), &hero, &[], &[]);
        assert!(body.contains("### 💡 What we found"));
        assert!(body.contains("HttpClient::send has no retry logic"));
    }

    #[test]
    fn replay_hero_renders_goal_then_dag_then_trail() {
        let dag = TraceDag {
            nodes: vec![TraceNode {
                id: "abc12345".into(),
                parent_ids: vec![],
                kind: "THINK".into(),
                content: "use exponential backoff".into(),
                timestamp: "2026-05-22T00:00:00Z".into(),
            }],
        };
        let body = render_hero_replay(&sample_aggregates(), &sample_hero(), &dag, &[], &[]);
        assert!(body.contains("🪙 h5i provenance · _the replay_"));
        assert!(body.contains("🎯 **Goal:** Add retry logic"));
        assert!(body.contains("### 🧠 Reasoning by file"));
        assert!(body.contains("```mermaid"));
        // Replay must promote the DAG above the fold: no <details>, and the
        // swim-lane outer direction is TB (lanes stack) with internal LR.
        assert!(body.contains("flowchart TB"));
        assert!(
            !body.contains("<details>"),
            "replay hero must render DAG expanded, got: {body}"
        );
        assert!(body.contains("**Milestone trail:**"));
        // Trail uses arrow separators between backticked items.
        assert!(body.contains("→"));
    }

    #[test]
    fn replay_hero_emits_fallback_when_dag_empty() {
        let body = render_hero_replay(
            &sample_aggregates(),
            &sample_hero(),
            &TraceDag::default(),
            &[],
            &[],
        );
        assert!(body.contains("No reasoning trace recorded"));
        assert!(!body.contains("```mermaid"));
    }

    #[test]
    fn format_tokens_thousands_breakpoint() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(12_345), "12.3k");
    }

    // ── DAG stats ─────────────────────────────────────────────────────────

    #[test]
    fn dag_stats_count_files_kinds_and_ratios() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a", "OBSERVE", "read src/foo.rs", &[]),
                make_node("b", "OBSERVE", "read src/foo.rs", &["a"]),
                make_node("c", "OBSERVE", "read src/bar.rs", &["b"]),
                make_node("d", "ACT", "edited src/foo.rs", &["c"]),
                make_node("e", "THINK", "consider split", &["d"]),
                make_node("f", "ACT", "edited src/bar.rs", &["e"]),
            ],
        };
        let s = compute_dag_stats(&dag);
        assert_eq!(s.files_touched, 2, "foo.rs + bar.rs");
        assert_eq!(s.observe_count, 3);
        assert_eq!(s.act_count, 2);
        assert_eq!(s.think_count, 1);
        assert_eq!(s.read_to_edit, Some(1.5));
        assert_eq!(s.ops_per_think, Some(5.0));
    }

    #[test]
    fn dag_stats_handle_zero_divisions() {
        // All ACT, no OBSERVE → ratio is None ("all edits").
        let dag = TraceDag {
            nodes: vec![make_node("a", "ACT", "edited src/foo.rs", &[])],
        };
        let s = compute_dag_stats(&dag);
        assert_eq!(s.read_to_edit, None);
        assert_eq!(s.ops_per_think, None, "no THINK → no density");

        // All OBSERVE.
        let dag = TraceDag {
            nodes: vec![make_node("a", "OBSERVE", "read src/foo.rs", &[])],
        };
        let s = compute_dag_stats(&dag);
        assert_eq!(s.read_to_edit, None);
    }

    #[test]
    fn dag_stats_inline_omits_empty_signals() {
        let s = DagStats::default();
        assert_eq!(format_dag_stats_inline(&s), "");

        let s = DagStats {
            files_touched: 4,
            observe_count: 10,
            act_count: 5,
            think_count: 1,
            read_to_edit: Some(2.0),
            ops_per_think: Some(15.0),
        };
        let line = format_dag_stats_inline(&s);
        assert!(line.contains("**4** files touched"));
        assert!(line.contains("READ:EDIT **2.0:1**"));
        assert!(line.contains("1 THINK per **15** ops"));
    }

    #[test]
    fn dag_stats_inline_inverts_ratio_when_edits_dominate() {
        // 3 reads, 9 edits → ratio 0.33 → render as "1:3.0"
        let s = DagStats {
            files_touched: 2,
            observe_count: 3,
            act_count: 9,
            think_count: 0,
            read_to_edit: Some(3.0 / 9.0),
            ops_per_think: None,
        };
        let line = format_dag_stats_inline(&s);
        assert!(line.contains("READ:EDIT **1:3.0**"), "got: {line}");
        // No THINK density line when none recorded.
        assert!(!line.contains("THINK per"));
    }

    // ── Security banner ───────────────────────────────────────────────────

    #[test]
    fn secret_banner_screams_block_merge() {
        let s = render_secret_alert_banner(3);
        assert!(s.starts_with("> [!CAUTION]"), "must be a CAUTION alert: {s}");
        assert!(s.contains("🚨"));
        assert!(s.contains("BLOCK MERGE"));
        assert!(s.contains("3 credential leaks"));
        assert!(s.contains("Rotate the exposed secrets"));
    }

    #[test]
    fn secret_banner_pluralizes_for_single_leak() {
        let s = render_secret_alert_banner(1);
        assert!(s.contains("1 credential leak detected"));
        assert!(!s.contains("leaks "), "got plural for one leak: {s}");
    }

    // ── Hero integration ──────────────────────────────────────────────────

    #[test]
    fn receipt_hero_stat_card_carries_all_signals() {
        let body = render_hero_receipt(&sample_aggregates(), &sample_hero(), &[], &[]);
        // Six-cell stat card. We check that each label and its value reach
        // the rendered output; exact placement is loosely coupled so we
        // don't lock in a column order future designers might want to tweak.
        assert!(body.contains("<sub>AI commits</sub>"));
        assert!(body.contains("<sub>tokens</sub>"));
        assert!(body.contains("<sub>est. cost</sub>"));
        assert!(body.contains("<sub>files touched</sub>"));
        assert!(body.contains("<sub>READ : EDIT</sub>"));
        assert!(body.contains("<sub>THINKs / ops</sub>"));
        assert!(body.contains("<strong>4 / 5</strong>"));
        assert!(body.contains("<strong>$0.04</strong>"));
        assert!(body.contains("<strong>2.0 : 1</strong>"));
    }

    #[test]
    fn detective_hero_has_dedicated_stats_section() {
        let body = render_hero_detective(&sample_aggregates(), &sample_hero(), &[], &[]);
        assert!(body.contains("### 📊 By the numbers"));
        // The narrative ordering: Goal callout → By the numbers → Considered → Insight → Shipped.
        let positions: Vec<usize> = ["> [!IMPORTANT]", "### 📊 By the numbers", "### 🧭", "### 💡", "### 🚢"]
            .iter()
            .map(|h| body.find(h).unwrap_or_else(|| panic!("missing section {h} in:\n{body}")))
            .collect();
        for w in positions.windows(2) {
            assert!(w[0] < w[1], "sections out of order: {positions:?}");
        }
    }

    #[test]
    fn replay_hero_emits_goal_label_and_stats() {
        let dag = TraceDag {
            nodes: vec![make_node("a1", "ACT", "edited src/foo.rs", &[])],
        };
        let body = render_hero_replay(&sample_aggregates(), &sample_hero(), &dag, &[], &[]);
        // Goal in [!IMPORTANT] callout, same as Receipt/Detective.
        assert!(body.contains("> [!IMPORTANT]\n> 🎯 **Goal:**"), "goal callout: {body}");
        assert!(body.contains("**📊 By the numbers:**"));
    }

    // ── Cost estimation ───────────────────────────────────────────────────

    #[test]
    fn model_price_picks_tier_by_family_substring() {
        assert_eq!(model_price("claude-opus-4-7"), Some((15.0, 75.0)));
        assert_eq!(model_price("claude-sonnet-4-6"), Some((3.0, 15.0)));
        assert_eq!(model_price("claude-haiku-4-5-20251001"), Some((0.8, 4.0)));
        // Unknown / nonsense → None so we never mis-attribute a price tier.
        assert_eq!(model_price("gpt-4o"), None);
        assert_eq!(model_price(""), None);
    }

    #[test]
    fn estimate_commit_cost_uses_input_and_output_rates() {
        use crate::metadata::TokenUsage;
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            content_tokens: 1_000_000,
            total_tokens: 2_000_000,
            model: String::new(),
        };
        // Opus: $15 + $75 = $90 for a clean 1M-in / 1M-out commit.
        let c = estimate_commit_cost("claude-opus-4-7", &usage).unwrap();
        assert!((c - 90.0).abs() < 0.01, "got: {c}");
        // Sonnet: $3 + $15 = $18.
        let c = estimate_commit_cost("claude-sonnet-4-6", &usage).unwrap();
        assert!((c - 18.0).abs() < 0.01, "got: {c}");
        // Unknown family → None.
        assert!(estimate_commit_cost("mystery-model", &usage).is_none());
    }

    #[test]
    fn estimate_commit_cost_prefers_usage_model_over_outer_name() {
        use crate::metadata::TokenUsage;
        // The Anthropic SDK reports `model` on the usage block; fall back to
        // the outer `AiMetadata.model_name` only when usage.model is empty.
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            content_tokens: 0,
            total_tokens: 1_000_000,
            model: "claude-opus-4-7".to_string(),
        };
        let c = estimate_commit_cost("claude-sonnet-4-6", &usage).unwrap();
        assert!((c - 15.0).abs() < 0.01, "usage.model wins: {c}");
    }

    #[test]
    fn format_cost_scales_resolution_with_magnitude() {
        assert_eq!(format_cost(0.0), "<$0.01");
        assert_eq!(format_cost(0.001), "<$0.01");
        assert_eq!(format_cost(0.04), "$0.04");
        assert_eq!(format_cost(0.99), "$0.99");
        assert_eq!(format_cost(1.0), "~$1.00");
        assert_eq!(format_cost(12.5), "~$12.50");
        assert_eq!(format_cost(150.0), "~$150");
    }

    // ── Milestone cleaning ────────────────────────────────────────────────

    #[test]
    fn clean_milestones_drops_autotrace_and_session_end() {
        let raw = vec![
            "[x] Production hardening pass".to_string(),
            "[x] edited src/pr.rs; edited src/pr.rs; edited src/pr.rs".to_string(),
            "[x] session ended (auto-checkpoint)".to_string(),
            "[x] wrote .github/workflows/test.yaml; wrote src/foo.rs".to_string(),
            "[x] Implement retry loop".to_string(),
        ];
        let out = clean_milestones(&raw, 5);
        // Cleaned output preserves chronological order of the *kept* entries.
        assert_eq!(
            out,
            vec![
                "Production hardening pass".to_string(),
                "Implement retry loop".to_string(),
            ]
        );
    }

    #[test]
    fn clean_milestones_caps_at_take_and_walks_newest_first() {
        // 7 valid checkpoints. We ask for 3, expect the 3 newest (last 3
        // in input) in chronological order.
        let raw: Vec<String> = (0..7).map(|i| format!("[x] checkpoint {i}")).collect();
        let out = clean_milestones(&raw, 3);
        assert_eq!(
            out,
            vec![
                "checkpoint 4".to_string(),
                "checkpoint 5".to_string(),
                "checkpoint 6".to_string(),
            ]
        );
    }

    #[test]
    fn clean_milestones_drops_single_file_mechanical_entries() {
        // Single-verb-plus-path is auto-trace noise; multi-word prose that
        // happens to start with the same verb must survive.
        let raw = vec![
            "[x] edited src/main.rs".to_string(),
            "[x] wrote tests/foo.rs".to_string(),
            "[x] deleted README.md".to_string(),
            "[x] edited authentication flow to use OAuth2".to_string(),
            "[x] Production hardening pass".to_string(),
        ];
        let out = clean_milestones(&raw, 5);
        assert_eq!(
            out,
            vec![
                "edited authentication flow to use OAuth2".to_string(),
                "Production hardening pass".to_string(),
            ]
        );
    }

    #[test]
    fn clean_milestones_handles_unchecked_boxes_and_blanks() {
        let raw = vec![
            "[ ] not yet done".to_string(),
            "".to_string(),
            "[x]   ".to_string(),
            "[x] a real one".to_string(),
        ];
        let out = clean_milestones(&raw, 5);
        assert_eq!(out, vec!["not yet done".to_string(), "a real one".to_string()]);
    }

    // ── Minimal style ─────────────────────────────────────────────────────

    #[test]
    fn minimal_hero_omits_marketing_flourish() {
        let body = render_hero_minimal(&sample_aggregates(), &sample_hero(), &[], &[]);
        // Has the data:
        assert!(body.contains("## 🪙 h5i provenance"));
        assert!(body.contains("**🎯 Goal:** Add retry logic"));
        assert!(body.contains("**📊 By the numbers:**"));
        // Drops the marketing flourish:
        assert!(!body.starts_with("# 🪙 "), "no H1 headline: {body}");
        assert!(!body.contains("<table"), "no HTML stat card: {body}");
        assert!(!body.contains("[!IMPORTANT]"), "no IMPORTANT callout: {body}");
        assert!(!body.contains("$0.04"), "no dollar figures: {body}");
        assert!(!body.contains("The ask"), "no prompt section: {body}");
    }

    // ── Review style ──────────────────────────────────────────────────────

    #[test]
    fn review_hero_leads_with_reviewer_triage() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a", "OBSERVE", "read src/http.rs", &[]),
                make_node("b", "ACT", "edited src/http.rs", &["a"]),
                make_node("c", "ACT", "edited src/retry.rs", &["b"]),
                make_node("d", "THINK", "retry policy should avoid thundering herd", &["c"]),
            ],
        };
        let dup_rows = vec![DupRow {
            file: "src/retry.rs".into(),
            block_len: 12,
            first_line: 10,
            repeat_line: 88,
            short_oid: "a3f8c12e".into(),
        }];
        let body = render_hero_review(&sample_aggregates(), &sample_hero(), &dag, &[], &dup_rows);

        assert!(body.starts_with("## h5i review brief"), "got: {body}");
        assert!(body.contains("**Merge status:** 🟡 review needed · 🔐 security clean · 🧬 1 duplicate-code finding in 1 file"));
        assert!(body.contains("**Review focus:** `src/retry.rs`, `src/http.rs`"));
        assert!(body.contains("**Evidence:** 4 AI commits · 3 files touched · 4 trace nodes · 1 decision recorded · 2 commits with passing test evidence"));
        assert!(body.contains("> 🎯 **Goal:** Add retry logic"));
        assert!(body.contains("### Reviewer checklist"));
        assert!(body.contains("### Reasoning highlights"));
        assert!(body.contains("| `THINK` | retry policy should avoid thundering herd |"));
        assert!(body.contains("Inspect duplicate-code findings in `src/retry.rs`."));
        let checklist_pos = body.find("### Reviewer checklist").unwrap();
        let reasoning_pos = body.find("### Reasoning highlights").unwrap();
        assert!(checklist_pos < reasoning_pos, "reasoning highlights should come last: {body}");
        assert!(!body.contains("est. cost"), "review style must not market cost: {body}");
        assert!(!body.contains("tokens"), "review style should not foreground tokens: {body}");
        assert!(!body.contains("```mermaid"), "review hero must stay text-first: {body}");
    }

    #[test]
    fn review_focus_prioritizes_duplicate_files_before_hot_files() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a", "ACT", "edited src/hot.rs", &[]),
                make_node("b", "ACT", "edited src/hot.rs", &["a"]),
                make_node("c", "ACT", "edited src/hot.rs", &["b"]),
                make_node("d", "ACT", "edited src/dup.rs", &["c"]),
            ],
        };
        let dup_rows = vec![DupRow {
            file: "src/dup.rs".into(),
            block_len: 10,
            first_line: 1,
            repeat_line: 20,
            short_oid: "deadbeef".into(),
        }];
        let focus = review_focus_files(&dag, &dup_rows, 2);
        assert_eq!(focus, vec!["src/dup.rs".to_string(), "src/hot.rs".to_string()]);
    }

    #[test]
    fn review_reasoning_highlights_prefer_think_then_note() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a", "NOTE", "TODO: revisit timeout defaults", &[]),
                make_node("b", "THINK", "choose explicit review style over default receipt", &["a"]),
                make_node("c", "NOTE", "RISK: Mermaid can shrink in GitHub comments", &["b"]),
                make_node("d", "THINK", "choose compact table over exposing full DAG", &["c"]),
            ],
        };
        let highlights = review_reasoning_highlights(&dag, 3);
        assert_eq!(
            highlights,
            vec![
                ReasoningHighlight {
                    kind: "THINK".into(),
                    content: "choose compact table over exposing full DAG".into(),
                },
                ReasoningHighlight {
                    kind: "THINK".into(),
                    content: "choose explicit review style over default receipt".into(),
                },
                ReasoningHighlight {
                    kind: "NOTE".into(),
                    content: "RISK: Mermaid can shrink in GitHub comments".into(),
                },
            ]
        );
    }

    // ── 💬 Agent coordination ─────────────────────────────────────────────

    use crate::msg::Message;

    fn msg(kind: &str, from: &str, to: &str, body: &str, ts: &str) -> Message {
        Message {
            id: ts.into(),
            ts: ts.into(),
            from: from.into(),
            to: to.into(),
            body: body.into(),
            kind: Some(kind.into()),
            version: 1,
            ..Default::default()
        }
    }

    #[test]
    fn md_escape_neutralizes_html_and_table_injection() {
        let out = md_escape("</summary><script>alert(1)</script> | col | `code`");
        assert!(!out.contains('<'), "raw < must be gone: {out}");
        assert!(!out.contains('>'), "raw > must be gone: {out}");
        assert!(!out.contains('|'), "raw | must be gone: {out}");
        assert!(!out.contains('`'), "raw backtick must be gone: {out}");
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn truncate_chars_appends_ellipsis_only_when_cut() {
        assert_eq!(truncate_chars("short", 10), "short");
        let t = truncate_chars("abcdefghij", 5);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn short_ts_formats_rfc3339() {
        assert_eq!(short_ts("2026-05-30T18:15:40.123Z"), "2026-05-30 18:15");
        // Unparseable input falls back to the raw value.
        assert_eq!(short_ts("whenever"), "whenever");
    }

    #[test]
    fn excerpt_uses_first_nonempty_line_and_redacts() {
        // Build the token from parts so the credential literal never appears on
        // a single source line (keeps the in-repo secret scanner quiet — this
        // file isn't on its test-fixture allowlist the way `secrets.rs` is).
        let token = format!("ghp_{}", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
        let body = format!("\n  summary line with {token}\nsecond line");
        let ex = excerpt_body(&body, false);
        assert!(ex.starts_with("summary line"), "first non-empty line: {ex}");
        assert!(!ex.contains("second line"), "later lines dropped in excerpt mode");
        assert!(!ex.contains(&token), "secret redacted: {ex}");
    }

    #[test]
    fn review_typed_kinds_get_excerpt_fyi_does_not() {
        assert!(kind_gets_excerpt("REVIEW_REQUEST"));
        assert!(kind_gets_excerpt("RISK"));
        assert!(kind_gets_excerpt("DONE"));
        assert!(!kind_gets_excerpt("FYI"));
        assert!(!kind_gets_excerpt("BROADCAST"));
    }

    #[test]
    fn message_line_shows_review_excerpt() {
        let m = msg("REVIEW_REQUEST", "codex", "claude", "please review the redactor", "2026-05-30T18:15:40Z");
        let line = render_message_line(&m, false, false);
        // Known kinds render readably (no entity-escaping noise).
        assert!(line.contains("**REVIEW_REQUEST**"), "{line}");
        assert!(line.contains("codex → claude"));
        assert!(line.contains("please review the redactor"));
        assert!(!line.contains("metadata only"));
    }

    #[test]
    fn fyi_internal_chatter_hidden_by_default() {
        // An FYI body that mentions an internal path / error must NOT leak into
        // the default render — metadata only.
        let m = msg("FYI", "claude", "all", "stack trace at /home/me/secrets/notes.txt blew up", "2026-05-30T12:00:00Z");
        let line = render_message_line(&m, false, false);
        assert!(line.contains("_(metadata only)_"));
        assert!(!line.contains("/home/me/secrets/notes.txt"), "FYI body must not appear: {line}");
    }

    #[test]
    fn msg_bodies_flag_reveals_fyi_body() {
        let m = msg("FYI", "claude", "all", "heads up: rebased onto main", "2026-05-30T12:00:00Z");
        let line = render_message_line(&m, true, false);
        assert!(line.contains("heads up: rebased onto main"));
        assert!(!line.contains("metadata only"));
    }

    #[test]
    fn unknown_kind_is_escaped() {
        // A hostile pulled message with a breakout `kind` must be neutralised.
        let m = msg("</summary><script>", "x", "y", "hi", "2026-05-30T12:00:00Z");
        let line = render_message_line(&m, false, false);
        assert!(!line.contains("<script>"), "unknown kind must be escaped: {line}");
        assert!(line.contains("&lt;"));
    }

    #[test]
    fn unknown_kind_strips_control_bytes() {
        // Regression (Codex follow-up): an unknown kind with newline/control
        // bytes must be sanitized, not just escaped — no raw controls or
        // injected Markdown structure leak through.
        let m = msg("EVIL\nKIND\u{1b}[31m", "x", "y", "hi", "2026-05-30T12:00:00Z");
        let line = render_message_line(&m, false, false);
        // Ignore the single trailing line terminator; the kind itself must carry
        // no embedded newline or control byte.
        assert!(!line.trim_end().contains('\n'), "kind newline must be folded: {:?}", line);
        assert!(!line.contains('\u{1b}'), "control byte must be gone: {:?}", line);
    }

    #[test]
    fn malformed_ts_is_escaped() {
        // Regression (Codex RISK #3b): an unparseable ts falls back to raw, so it
        // must still be escaped — no Markdown/HTML injection via the timestamp.
        let m = msg("ASK", "x", "y", "hi", "</summary><img src=x onerror=alert(1)>");
        let line = render_message_line(&m, false, false);
        assert!(!line.contains("<img"), "raw ts must not inject: {line}");
        assert!(line.contains("&lt;"));
    }

    #[test]
    fn excerpt_redacts_token_split_by_control_bytes() {
        // Regression (Codex RISK #4): a credential split by an ANSI/control byte
        // must not survive — control bytes are stripped BEFORE redaction, so the
        // reassembled token is caught rather than reconstructed afterwards.
        let token = format!("ghp_{}", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
        // Insert an ESC (0x1b) mid-token: redacting the raw string wouldn't match.
        let split = format!("ghp_\u{1b}{}", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
        let m = msg("ASK", "x", "y", &split, "2026-05-30T12:00:00Z");
        let line = render_message_line(&m, false, false);
        assert!(!line.contains(&token), "reassembled token leaked: {line}");
        assert!(!line.contains('\u{1b}'), "control byte must be gone: {line}");
    }

    // ── Token-reduction section ────────────────────────────────────────────────
    fn manifest_json(branch: &str, kind: &str, raw: u64, summary: u64) -> crate::objects::Manifest {
        // structured=None → agent_facing_tokens() == summary_tokens (deterministic,
        // no tokenizer needed); tool falls back to `kind`.
        serde_json::from_value(serde_json::json!({
            "id": "deadbeefdeadbeef",
            "kind": kind,
            "branch": branch,
            "timestamp": "2026-06-05T00:00:00Z",
            "raw_oid": "sha256:abc",
            "raw_size": 10,
            "raw_lines": 1,
            "filter_version": 1,
            "summary": "s",
            "store": "local",
            "codec": "none",
            "raw_tokens": raw,
            "summary_tokens": summary,
        }))
        .unwrap()
    }

    #[test]
    fn token_section_omits_when_no_captures_or_no_saving() {
        // No manifests on the branch → omit.
        assert_eq!(token_reduction_section_from(&[], "feat"), "");
        // Captures exist but on another branch → omit.
        let other = vec![manifest_json("main", "pytest", 1000, 20)];
        assert_eq!(token_reduction_section_from(&other, "feat"), "");
        // On-branch but no net saving (summary >= raw) → omit (don't advertise a loss).
        let loss = vec![manifest_json("feat", "ruff", 40, 80)];
        assert_eq!(token_reduction_section_from(&loss, "feat"), "");
    }

    #[test]
    fn token_section_reports_branch_savings() {
        let ms = vec![
            manifest_json("feat", "pytest", 1000, 20),
            manifest_json("feat", "cargo", 600, 30),
            manifest_json("main", "pytest", 9999, 1), // other branch — excluded
        ];
        let out = token_reduction_section_from(&ms, "feat");
        assert!(out.contains("2 captured tool outputs"), "{out}");
        assert!(out.contains("1600 → 50 tokens"), "{out}"); // only the feat captures
        assert!(out.contains("96% saved"), "{out}");
        assert!(out.contains("<details><summary>By tool</summary>"));
    }

    #[test]
    fn token_section_escapes_tool_names_in_table() {
        // Tool names are untrusted (argv/kind) — a pipe must not break the table.
        let ms = vec![
            manifest_json("feat", "ev|il`x`", 1000, 10),
            manifest_json("feat", "pytest", 500, 10),
        ];
        let out = token_reduction_section_from(&ms, "feat");
        assert!(out.contains("ev\\|il"), "pipe must be escaped in table: {out}");
        // Every table data row keeps exactly the 5 columns (6 pipes), so an
        // unescaped pipe can't smuggle an extra cell.
        for row in out.lines().filter(|l| l.starts_with("| ") && !l.contains("---")) {
            assert_eq!(row.matches('|').count() - row.matches("\\|").count(), 6, "row: {row}");
        }
    }
}
