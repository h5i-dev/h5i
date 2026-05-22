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

/// Maximum number of DAG nodes rendered in the Mermaid block. Anything older
/// is dropped with a "…earlier nodes elided…" head note so the diagram stays
/// readable on a PR.
const DAG_NODE_LIMIT: usize = 40;

/// Hard cap on the label-text portion of a DAG node (after the `KIND ·` prefix).
/// Wide enough that THINK/NOTE reasoning is readable; narrow enough that the
/// graph still fits in a GitHub PR comment.
const DAG_LABEL_BUDGET: usize = 100;

/// Maximum number of file lanes drawn in the swim-lane DAG before extras get
/// folded into an "Other" lane. Picked empirically: 6 distinct files in a
/// vertical stack is the most you can read at a glance on a PR screenshot.
const SWIMLANE_MAX_FILES: usize = 6;

/// Minimum run length before consecutive same-kind nodes within a single lane
/// get compressed into `READ × N` / `EDIT × N`. More aggressive than the
/// chain-DAG threshold because per-lane density matters more than per-graph.
const SWIMLANE_COMPRESS_RUN: usize = 2;

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

fn render_secret_section(rows: &[SecretRow], dup_count: usize) -> String {
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
    // Reassurance footer when the *other* deterministic check came back clean.
    // We only print it when the partner check actually has zero findings — if
    // both fired, both alerts already speak for themselves.
    if dup_count == 0 {
        s.push_str("\n_Other checks: ✓ no duplicate code introduced._\n");
    }
    s.push('\n');
    s
}

fn render_duplicate_section(rows: &[DupRow], secret_count: usize) -> String {
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
    if secret_count == 0 {
        s.push_str("\n_Other checks: ✓ no credential leaks detected._\n");
    }
    s.push('\n');
    s
}

/// All-clear banner emitted when every deterministic check passed. Surfaces
/// the negative result so reviewers know h5i actually looked — silently
/// rendering nothing looks like "no scan ran".
fn render_checks_pass_note() -> String {
    "> [!NOTE]\n> **h5i checks pass** — ✓ no credential leaks · ✓ no duplicate code blocks\n\n"
        .to_string()
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
    /// Receipt — scannable summary block: goal, milestones, AI/human ratio,
    /// tokens, top AI prompt. Optimised for the screenshot-able first viewport.
    Receipt,
    /// Detective — narrative: goal → considered/rejected → key insight → shipped.
    Detective,
    /// Replay — DAG promoted above the fold with milestone markers.
    Replay,
}

/// Pre-rolled aggregates derived from `h5i_log` + review points. All hero
/// renderers consume this so they never re-walk the records list.
struct Aggregates {
    ai_count: usize,
    human_count: usize,
    total_tokens: usize,
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
///        Receipt/Detective: collapsible, below the fold
///        Replay:           rendered inside the hero, expanded
///   5. 📜 Per-commit provenance (collapsible if >5 AI commits)
///   6. Footer
pub fn render_body_with_style(workdir: &Path, limit: usize, style: PrStyle) -> Result<String> {
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
        body.push_str(&render_secret_section(&secret_rows, dup_rows.len()));
    }

    match style {
        PrStyle::Receipt => body.push_str(&render_hero_receipt(&aggregates, &hero, &secret_rows, &dup_rows)),
        PrStyle::Detective => body.push_str(&render_hero_detective(&aggregates, &hero, &secret_rows, &dup_rows)),
        PrStyle::Replay => body.push_str(&render_hero_replay(&aggregates, &hero, &dag, &secret_rows, &dup_rows)),
    }

    // Empty-state reassurance: when BOTH deterministic checks came back
    // clean, emit a single all-clear NOTE. When only one fired, the
    // section-level renderer adds a tail line about the other.
    if secret_rows.is_empty() && dup_rows.is_empty() {
        body.push_str(&render_checks_pass_note());
    }

    // When secrets WEREN'T promoted to the top, emit the table here as before.
    if !secrets_present {
        body.push_str(&render_secret_section(&secret_rows, dup_rows.len()));
    }
    body.push_str(&render_duplicate_section(&dup_rows, secret_rows.len()));
    // Replay already drew the DAG above the fold; skip the collapsible copy.
    if !matches!(style, PrStyle::Replay) {
        body.push_str(&render_swimlane_section(&dag));
    }
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
        tests_passing: 0,
        tests_failing: 0,
        flagged_count: 0,
    };
    for r in records {
        if r.ai_metadata.is_none() {
            a.human_count += 1;
            continue;
        }
        a.ai_count += 1;
        if let Some(u) = r.ai_metadata.as_ref().and_then(|m| m.usage.as_ref()) {
            a.total_tokens = a.total_tokens.saturating_add(u.total_tokens);
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
    a
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

/// Single dense block, scannable at a glance — built to be the screenshot
/// people share. We use a single-row blockquote header so it stands out from
/// the audit tables below, and put the badges on the first line for parity
/// with the legacy layout (anyone screenshot-comparing won't see a regression).
fn render_hero_receipt(
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

    // The hero block proper — a single GFM blockquote so the whole receipt
    // visually clusters as one card on github.com.
    s.push_str("> **Receipt**\n");
    if !hero.branch_goal.is_empty() {
        let _ = writeln!(s, "> 🎯 **Goal:** {}", escape_md(&truncate(&hero.branch_goal, 200)));
    }
    let total_commits = agg.ai_count + agg.human_count;
    if total_commits > 0 {
        let ratio = if total_commits > 0 {
            (agg.ai_count as f64 / total_commits as f64 * 100.0).round() as usize
        } else {
            0
        };
        let _ = writeln!(
            s,
            "> 🤖 **{} AI** · 👤 **{} human** _( {}% AI )_",
            agg.ai_count, agg.human_count, ratio
        );
    }
    if agg.total_tokens > 0 {
        let _ = writeln!(s, "> 🧮 **{}** tokens consumed", format_tokens(agg.total_tokens));
    }
    let stats_line = format_dag_stats_inline(&hero.dag_stats);
    if !stats_line.is_empty() {
        let _ = writeln!(s, "> 📊 {}", stats_line);
    }
    if !hero.milestones.is_empty() {
        s.push_str("> 📍 **Milestones reached:**\n");
        // Latest 3, in original (oldest→newest) order so the trail reads forward.
        let start = hero.milestones.len().saturating_sub(3);
        for m in &hero.milestones[start..] {
            let _ = writeln!(s, "> &nbsp;&nbsp;✓ {}", escape_md(&truncate(m, 120)));
        }
    }
    if let Some(prompt) = &hero.top_prompt {
        let _ = writeln!(
            s,
            "> 💬 _Triggering prompt:_ \"{}\"",
            escape_md(&truncate(prompt, 180))
        );
    }
    s.push_str(">\n");
    s.push('\n');
    s
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

    // Act I — the goal.
    if !hero.branch_goal.is_empty() {
        s.push_str("### 🎯 Goal\n\n");
        let _ = writeln!(s, "> {}", escape_md(&truncate(&hero.branch_goal, 280)));
        s.push('\n');
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

    // Act IV — what shipped. Most-recent first because reviewers care about
    // the latest state of the branch, not its archaeology.
    if !hero.milestones.is_empty() {
        s.push_str("### 🚢 What shipped\n\n");
        let tail: Vec<&String> = hero.milestones.iter().rev().take(5).collect();
        for m in &tail {
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

    // Goal as a one-line header above the DAG so the graph has context.
    if !hero.branch_goal.is_empty() {
        let _ = writeln!(
            s,
            "> 🎯 **Goal:** {}",
            escape_md(&truncate(&hero.branch_goal, 220))
        );
        s.push('\n');
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
    if !hero.milestones.is_empty() {
        s.push_str("**Milestone trail:**\n\n");
        let tail: Vec<&String> = hero.milestones.iter().rev().take(6).collect();
        // Print in chronological order so the arrow chain reads left-to-right.
        let chrono: Vec<&&String> = tail.iter().rev().collect();
        let line = chrono
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
    let token = node.content.trim().split_whitespace().next_back()?;
    if token.contains('/') {
        return Some(token.to_string());
    }
    let lower = token.to_ascii_lowercase();
    if SWIMLANE_FILE_EXTS.iter().any(|e| lower.ends_with(e)) {
        return Some(token.to_string());
    }
    None
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
    /// Either an original `TraceNode::id` or a synthetic `srun_<last_id>` for
    /// a compressed run. Routed through [`mermaid_id`] before emission.
    id: String,
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
            // Use the last node's id as the synthetic anchor so any edges
            // pointing into the run from a future cross-lane renderer still
            // resolve to *something* visible.
            let id = format!("srun_{}", raw[j - 1].id);
            let verb = verb_for_kind(kind);
            let label = if run > 1 {
                format!("{verb} × {run}")
            } else {
                verb.to_string()
            };
            out.push(SwimNode { id, kind: kind.to_string(), label });
        } else {
            for n in &raw[i..j] {
                out.push(SwimNode {
                    id: n.id.clone(),
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
    let oneline = n.content.replace('\n', " ");
    let trimmed = truncate(&oneline, DAG_LABEL_BUDGET);
    let safe: String = trimmed
        .chars()
        .map(sanitize_mermaid_char)
        .collect();
    format!("{verb} · {safe}")
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
    s.push_str("\n```mermaid\nflowchart TB\n");
    for lane in lanes {
        let title: String = lane.title.chars().map(sanitize_mermaid_char).collect();
        let _ = writeln!(s, "  subgraph {key}[\"{title}\"]", key = lane.key);
        s.push_str("    direction LR\n");
        for node in &lane.nodes {
            let _ = writeln!(
                s,
                "    {id}[\"{label}\"]:::{class}",
                id = mermaid_id(&node.id),
                label = node.label,
                class = mermaid_class(&node.kind),
            );
        }
        // Intra-lane chronological arrows. Two or more nodes → one arrow per
        // consecutive pair so the eye can read the flow left-to-right.
        for w in lane.nodes.windows(2) {
            let _ = writeln!(
                s,
                "    {a} --> {b}",
                a = mermaid_id(&w[0].id),
                b = mermaid_id(&w[1].id),
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
        assert!(render_secret_section(&[], 0).is_empty());
        assert!(render_duplicate_section(&[], 0).is_empty());
        assert!(render_swimlane_section(&TraceDag::default()).is_empty());
    }

    #[test]
    fn checks_pass_note_uses_github_note_alert() {
        let s = render_checks_pass_note();
        assert!(s.starts_with("> [!NOTE]"), "must use GitHub NOTE alert: {s}");
        assert!(s.contains("h5i checks pass"));
        assert!(s.contains("no credential leaks"));
        assert!(s.contains("no duplicate code"));
    }

    #[test]
    fn secret_section_adds_passing_dup_footnote_when_alone() {
        let rows = vec![SecretRow {
            rule_id: "X".into(),
            description: "d".into(),
            file: None,
            line: 1,
            preview: "p".into(),
            short_oid: "abc12345".into(),
        }];
        let with_dups = render_secret_section(&rows, 3);
        assert!(
            !with_dups.contains("no duplicate code"),
            "must not claim duplicates passed when partner check fired: {with_dups}"
        );
        let without_dups = render_secret_section(&rows, 0);
        assert!(
            without_dups.contains("✓ no duplicate code"),
            "must surface that dup check came back clean: {without_dups}"
        );
    }

    #[test]
    fn duplicate_section_adds_passing_secret_footnote_when_alone() {
        let rows = vec![DupRow {
            file: "src/a.rs".into(),
            block_len: 8,
            first_line: 1,
            repeat_line: 50,
            short_oid: "abc12345".into(),
        }];
        let with_secrets = render_duplicate_section(&rows, 2);
        assert!(
            !with_secrets.contains("no credential leaks"),
            "must not claim secrets passed when partner check fired: {with_secrets}"
        );
        let without_secrets = render_duplicate_section(&rows, 0);
        assert!(
            without_secrets.contains("✓ no credential leaks"),
            "must surface that secret check came back clean: {without_secrets}"
        );
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
        let s = render_secret_section(&rows, 0);
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
        let s = render_secret_section(&one, 0);
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
        let s = render_secret_section(&two, 0);
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
        let s = render_duplicate_section(&rows, 0);
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
        let nodes = vec![
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
        let nodes = vec![
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
        assert!(s.contains("flowchart TB"), "outer direction is top-bottom");
        assert!(s.contains("subgraph lane_0"));
        assert!(s.contains("direction LR"), "each lane forces internal LR");
        assert!(s.contains("classDef o"), "OBSERVE class still defined");
        // Reasoning lane comes first (THINK), then the file lane.
        let reasoning_pos = s.find("💭 Reasoning").expect("reasoning lane title");
        let file_pos = s.find("📄 src/foo.rs").expect("file lane title");
        assert!(reasoning_pos < file_pos);
        // Intra-lane arrow between the two foo.rs ops.
        assert!(s.contains("n_a1 --> n_a2"));
        assert!(s.contains("</details>"));
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
    fn receipt_hero_includes_goal_ratio_and_milestones() {
        let body = render_hero_receipt(&sample_aggregates(), &sample_hero(), &[], &[]);
        assert!(body.contains("> **Receipt**"), "got: {body}");
        assert!(body.contains("🎯 **Goal:** Add retry logic"));
        assert!(body.contains("🤖 **4 AI**"));
        assert!(body.contains("👤 **1 human**"));
        assert!(body.contains("80% AI"), "ratio rounding wrong: {body}");
        assert!(body.contains("12.3k"), "tokens formatted: {body}");
        assert!(body.contains("Add timeout parameter"));
        assert!(body.contains("Triggering prompt"));
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
            tests_passing: 0,
            tests_failing: 0,
            flagged_count: 0,
        };
        let body = render_hero_receipt(&agg, &empty, &[], &[]);
        assert!(body.contains("> **Receipt**"));
        assert!(!body.contains("Goal:"));
        assert!(!body.contains("Milestones"));
        assert!(!body.contains("Triggering prompt"));
    }

    #[test]
    fn detective_hero_lays_out_four_acts() {
        let body = render_hero_detective(&sample_aggregates(), &sample_hero(), &[], &[]);
        assert!(body.contains("### 🎯 Goal"));
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
    fn receipt_hero_emits_stats_line() {
        let body = render_hero_receipt(&sample_aggregates(), &sample_hero(), &[], &[]);
        assert!(body.contains("📊"));
        assert!(body.contains("**3** files touched"));
        assert!(body.contains("READ:EDIT"));
        assert!(body.contains("1 THINK per"));
    }

    #[test]
    fn detective_hero_has_dedicated_stats_section() {
        let body = render_hero_detective(&sample_aggregates(), &sample_hero(), &[], &[]);
        assert!(body.contains("### 📊 By the numbers"));
        // The narrative ordering: Goal → By the numbers → Considered → Insight → Shipped.
        let positions: Vec<usize> = ["### 🎯 Goal", "### 📊 By the numbers", "### 🧭", "### 💡", "### 🚢"]
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
        assert!(body.contains("🎯 **Goal:**"), "goal must be labelled: {body}");
        assert!(body.contains("**📊 By the numbers:**"));
    }
}
