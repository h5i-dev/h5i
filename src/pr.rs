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

/// Minimum length of a consecutive mechanical-kind run before we collapse it
/// into a summary node. Two-in-a-row is just normal work; three or more is
/// usually `read a; read b; read c` noise worth folding.
const DAG_COMPRESS_RUN: usize = 3;

/// Hard cap on the label-text portion of a DAG node (after the `KIND ·` prefix).
/// Wide enough that THINK/NOTE reasoning is readable; narrow enough that the
/// graph still fits in a GitHub PR comment.
const DAG_LABEL_BUDGET: usize = 100;

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

/// One node-or-summary in the rendered DAG. A summary stands in for a run of
/// consecutive mechanical OBSERVE/ACT calls that were collapsed for readability.
#[derive(Debug, Clone)]
struct DagUnit {
    /// ID actually emitted into the Mermaid graph. For singletons this is the
    /// original `TraceNode::id`; for summaries it's `sum_<last_node_id>`.
    id: String,
    kind: String,
    /// Pre-rendered label text, *without* the kind prefix or sanitization
    /// (those happen inside [`render_dag_section`] via [`mermaid_label`]).
    content: String,
    /// Parent IDs in the original DAG; remapped to render-IDs at draw time.
    parent_ids: Vec<String>,
}

/// Compresses runs of consecutive mechanical same-kind nodes (OBSERVE or ACT)
/// of length ≥ [`DAG_COMPRESS_RUN`] into one summary node, while passing
/// THINK/NOTE/MERGE through unchanged. Also returns a remap table so callers
/// can rewrite edges pointing into a compressed run.
fn compress_dag_units(visible: &[&TraceNode]) -> (Vec<DagUnit>, HashMap<String, String>) {
    let mut units: Vec<DagUnit> = Vec::new();
    let mut remap: HashMap<String, String> = HashMap::new();

    let is_mechanical = |k: &str| matches!(k, "OBSERVE" | "ACT");

    let mut i = 0;
    while i < visible.len() {
        let n = visible[i];
        if is_mechanical(&n.kind) {
            // Greedily extend a run of same-kind mechanical nodes.
            let mut j = i + 1;
            while j < visible.len() && visible[j].kind == n.kind {
                j += 1;
            }
            let run_len = j - i;
            if run_len >= DAG_COMPRESS_RUN {
                let synth = format!("sum_{}", visible[j - 1].id);
                for node in &visible[i..j] {
                    remap.insert(node.id.clone(), synth.clone());
                }
                units.push(DagUnit {
                    id: synth,
                    kind: n.kind.clone(),
                    content: summarize_mechanical_run(visible, i, j),
                    parent_ids: visible[i].parent_ids.clone(),
                });
                i = j;
                continue;
            }
        }
        remap.insert(n.id.clone(), n.id.clone());
        units.push(DagUnit {
            id: n.id.clone(),
            kind: n.kind.clone(),
            content: n.content.clone(),
            parent_ids: n.parent_ids.clone(),
        });
        i += 1;
    }

    (units, remap)
}

/// Builds a human-readable summary line for a compressed mechanical run.
/// Looks like `"read 5 files: a.rs, b.rs, c.rs (+2 more)"`.
fn summarize_mechanical_run(visible: &[&TraceNode], start: usize, end: usize) -> String {
    let kind = visible[start].kind.as_str();
    let verb = match kind {
        "OBSERVE" => "read",
        "ACT" => "edited",
        _ => "touched",
    };
    let total = end - start;
    let mut files: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node in &visible[start..end] {
        // Hook-generated content is `"<verb> <path>"`; take the trailing token
        // as the path. For nonstandard content (e.g. multi-word notes), fall
        // back to the whole content trimmed.
        let c = node.content.trim();
        // Take the last whitespace-delimited token, which for hook-generated
        // entries is the file path.
        let token = c.split_whitespace().next_back().unwrap_or(c).to_string();
        if !token.is_empty() && seen.insert(token.clone()) {
            files.push(token);
        }
    }
    let unique = files.len();
    let preview: Vec<&str> = files.iter().take(3).map(String::as_str).collect();
    let preview_str = if unique <= 3 {
        preview.join(", ")
    } else {
        format!("{} (+{} more)", preview.join(", "), unique - 3)
    };
    let file_plural = if unique == 1 { "" } else { "s" };
    if unique == total {
        // Each call hit a distinct path — flat list.
        format!("{verb} {unique} file{file_plural}: {preview_str}")
    } else {
        // Same paths revisited — be honest about it.
        let op_label = match kind {
            "OBSERVE" => "reads",
            "ACT" => "edits",
            _ => "ops",
        };
        format!("{total} {op_label} across {unique} file{file_plural}: {preview_str}")
    }
}

fn render_dag_section(dag: &TraceDag) -> String {
    if dag.nodes.is_empty() {
        return String::new();
    }

    // Tail-truncate: keep the most recent N nodes — they reflect the work
    // most likely landing in this PR.
    let total = dag.nodes.len();
    let start = total.saturating_sub(DAG_NODE_LIMIT);
    let visible: Vec<&TraceNode> = dag.nodes.iter().skip(start).collect();
    let elided = total - visible.len();

    let (units, remap) = compress_dag_units(&visible);
    let unit_ids: std::collections::HashSet<&str> =
        units.iter().map(|u| u.id.as_str()).collect();

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<details><summary><b>🧠 Reasoning DAG</b> — {} node{} \
         ({} block{} after compression{})</summary>",
        total,
        if total == 1 { "" } else { "s" },
        units.len(),
        if units.len() == 1 { "" } else { "s" },
        if elided > 0 {
            format!(", latest {} only", visible.len())
        } else {
            String::new()
        },
    );
    s.push('\n');
    s.push_str("\n```mermaid\ngraph TD\n");
    if elided > 0 {
        let _ = writeln!(
            s,
            "  elided[\"… {} earlier node{} elided …\"]:::elided",
            elided,
            if elided == 1 { "" } else { "s" }
        );
    }
    for u in &units {
        let label = mermaid_label(&u.kind, &u.content);
        let class = mermaid_class(&u.kind);
        let _ = writeln!(s, "  {id}[\"{label}\"]:::{class}", id = mermaid_id(&u.id));
    }
    // Edges: remap each parent through the compression table, then drop edges
    // where parent or child fell off the visible window OR where a node points
    // at its own summary (which would create a self-loop after collapse).
    let mut seen_edges: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for u in &units {
        for p in &u.parent_ids {
            let mapped = remap.get(p).cloned().unwrap_or_else(|| p.clone());
            if mapped == u.id {
                continue;
            }
            if !unit_ids.contains(mapped.as_str()) {
                continue;
            }
            let key = (mapped.clone(), u.id.clone());
            if !seen_edges.insert(key) {
                continue;
            }
            let _ = writeln!(
                s,
                "  {p} --> {c}",
                p = mermaid_id(&mapped),
                c = mermaid_id(&u.id)
            );
        }
    }
    s.push_str(
        "  classDef o fill:#dbeafe,stroke:#1e3a8a,color:#0b1c4a;\n\
         \x20\x20classDef t fill:#fef3c7,stroke:#92400e,color:#3f2d05;\n\
         \x20\x20classDef a fill:#dcfce7,stroke:#166534,color:#0a2e16;\n\
         \x20\x20classDef n fill:#ede9fe,stroke:#5b21b6,color:#221251;\n\
         \x20\x20classDef m fill:#e5e7eb,stroke:#374151,color:#0b0f17;\n\
         \x20\x20classDef elided fill:#f3f4f6,stroke:#9ca3af,color:#6b7280,stroke-dasharray: 3 3;\n",
    );
    s.push_str("```\n\n</details>\n\n");
    s
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

fn mermaid_label(kind: &str, content: &str) -> String {
    // Mermaid double-quoted labels treat `"`, `\`, `<`, and `>` specially.
    // Newlines collapse to a single space so the node renders one row tall.
    let oneline = content.replace('\n', " ");
    let trimmed = truncate(&oneline, DAG_LABEL_BUDGET);
    let safe: String = trimmed
        .chars()
        .map(|c| match c {
            '"' => '\u{201D}', // right double quote
            '\\' => '/',
            '<' => '‹',
            '>' => '›',
            _ => c,
        })
        .collect();
    format!("{kind} · {safe}")
}

// ── Top-level render ─────────────────────────────────────────────────────────

/// Render the full Markdown body for the PR comment.
///
/// Layout (sections omit themselves when empty):
///   1. Header + badge row
///   2. 🔒 Credential-leak alert + table
///   3. 🔁 Duplicate-code alert + table
///   4. 🧠 Reasoning DAG (collapsible Mermaid)
///   5. 📜 Per-commit provenance (collapsible if >5 AI commits)
///   6. Footer
pub fn render_body(workdir: &Path, limit: usize) -> Result<String> {
    let _span = tracing::info_span!("pr_render_body", limit).entered();
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
    tracing::debug!(
        records = records.len(),
        review_points = review_points.len(),
        secrets = secret_rows.len(),
        duplicates = dup_rows.len(),
        dag_nodes = dag.nodes.len(),
        "pr_render_body aggregates",
    );

    let mut ai_count = 0usize;
    let mut total_tokens = 0usize;
    let mut tests_passing = 0usize;
    let mut tests_failing = 0usize;
    let mut flagged_count = 0usize;
    for r in &records {
        if r.ai_metadata.is_none() {
            continue;
        }
        ai_count += 1;
        if let Some(u) = r.ai_metadata.as_ref().and_then(|a| a.usage.as_ref()) {
            total_tokens = total_tokens.saturating_add(u.total_tokens);
        }
        if let Some(tm) = r.test_metrics.as_ref() {
            // Skip empty/placeholder metrics so the header badge doesn't
            // claim a failure for commits where no adapter actually ran.
            if tm.total > 0 || tm.passed + tm.failed > 0 {
                if tm.is_passing() {
                    tests_passing += 1;
                } else {
                    tests_failing += 1;
                }
            }
        }
        if by_oid
            .get(&r.git_oid)
            .map(|p| p.should_flag_in_pr())
            .unwrap_or(false)
        {
            flagged_count += 1;
        }
    }

    let mut body = String::new();
    body.push_str(MARKER);
    body.push('\n');
    body.push_str("## 🪙 h5i provenance\n\n");
    body.push_str(&render_badges(
        ai_count,
        total_tokens,
        secret_rows.len(),
        dup_rows.len(),
        tests_passing,
        tests_failing,
        flagged_count,
    ));
    body.push_str("\n\n");

    // Empty-state reassurance: when BOTH deterministic checks came back
    // clean, emit a single all-clear NOTE. When only one fired, the
    // section-level renderer adds a tail line about the other.
    if secret_rows.is_empty() && dup_rows.is_empty() {
        body.push_str(&render_checks_pass_note());
    }

    body.push_str(&render_secret_section(&secret_rows, dup_rows.len()));
    body.push_str(&render_duplicate_section(&dup_rows, secret_rows.len()));
    body.push_str(&render_dag_section(&dag));
    body.push_str(&render_per_commit_section(&records, &by_oid, &repo));

    body.push_str("---\n\n");
    body.push_str("<sub>Generated by <a href=\"https://github.com/Koukyosyumei/h5i\">h5i</a> · re-run <code>h5i share pr post</code> to refresh.</sub>\n");
    Ok(body)
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
        assert!(render_dag_section(&TraceDag::default()).is_empty());
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

    #[test]
    fn compress_run_collapses_three_or_more_same_kind_mechanical() {
        let n1 = make_node("aa", "OBSERVE", "read src/a.rs", &[]);
        let n2 = make_node("bb", "OBSERVE", "read src/b.rs", &["aa"]);
        let n3 = make_node("cc", "OBSERVE", "read src/c.rs", &["bb"]);
        let n4 = make_node("dd", "THINK", "decide", &["cc"]);
        let visible: Vec<&TraceNode> = vec![&n1, &n2, &n3, &n4];

        let (units, remap) = compress_dag_units(&visible);
        assert_eq!(units.len(), 2, "3 OBSERVE → 1 summary + 1 THINK");
        assert!(units[0].id.starts_with("sum_"));
        assert_eq!(units[0].kind, "OBSERVE");
        assert!(units[0].content.contains("read 3 files"));
        assert!(units[0].content.contains("src/a.rs"));
        assert!(units[0].content.contains("src/b.rs"));
        assert!(units[0].content.contains("src/c.rs"));
        // THINK must survive as itself.
        assert_eq!(units[1].id, "dd");
        // Remap: every collapsed node's id now points at the summary so edges
        // from THINK referring to "cc" get rewritten to the summary id.
        assert_eq!(remap.get("aa"), remap.get("bb"));
        assert_eq!(remap.get("bb"), remap.get("cc"));
        assert!(remap.get("aa").unwrap().starts_with("sum_"));
    }

    #[test]
    fn compress_run_keeps_runs_of_two_uncompressed() {
        let n1 = make_node("aa", "OBSERVE", "read src/a.rs", &[]);
        let n2 = make_node("bb", "OBSERVE", "read src/b.rs", &["aa"]);
        let visible: Vec<&TraceNode> = vec![&n1, &n2];
        let (units, _) = compress_dag_units(&visible);
        assert_eq!(units.len(), 2, "2 < DAG_COMPRESS_RUN — both stay singular");
    }

    #[test]
    fn compress_run_does_not_merge_across_kinds() {
        let n1 = make_node("aa", "OBSERVE", "read src/a.rs", &[]);
        let n2 = make_node("bb", "OBSERVE", "read src/b.rs", &["aa"]);
        let n3 = make_node("cc", "ACT", "edited src/c.rs", &["bb"]);
        let n4 = make_node("dd", "ACT", "edited src/d.rs", &["cc"]);
        let visible: Vec<&TraceNode> = vec![&n1, &n2, &n3, &n4];
        let (units, _) = compress_dag_units(&visible);
        // Neither run reaches 3; nothing collapses.
        assert_eq!(units.len(), 4);
    }

    #[test]
    fn compress_run_summarizes_many_unique_files_as_plus_more() {
        let nodes: Vec<TraceNode> = (0..7)
            .map(|i| make_node(&format!("n{i}"), "ACT", &format!("edited f{i}.rs"), &[]))
            .collect();
        let visible: Vec<&TraceNode> = nodes.iter().collect();
        let (units, _) = compress_dag_units(&visible);
        assert_eq!(units.len(), 1);
        assert!(
            units[0].content.contains("edited 7 files"),
            "got: {}",
            units[0].content
        );
        assert!(units[0].content.contains("(+4 more)"));
    }

    #[test]
    fn dag_section_renders_mermaid_with_classes() {
        let dag = TraceDag {
            nodes: vec![
                make_node("a1b2c3d4", "OBSERVE", "scanned secrets", &[]),
                make_node("e5f6a7b8", "THINK", "which to rotate?", &["a1b2c3d4"]),
                make_node("c9d0e1f2", "ACT", "rotated AWS key", &["e5f6a7b8"]),
            ],
        };
        let s = render_dag_section(&dag);
        assert!(s.starts_with("<details>"));
        assert!(s.contains("```mermaid"));
        assert!(s.contains("graph TD"));
        assert!(s.contains("n_a1b2c3d4"), "node id must be Mermaid-safe");
        assert!(s.contains("classDef o"), "OBSERVE class must be defined");
        assert!(s.contains("OBSERVE · scanned secrets"));
        assert!(s.contains("THINK · which to rotate?"));
        assert!(s.contains("ACT · rotated AWS key"));
        assert!(s.contains("n_a1b2c3d4 --> n_e5f6a7b8"));
        assert!(s.contains("n_e5f6a7b8 --> n_c9d0e1f2"));
        assert!(s.contains("</details>"));
    }

    #[test]
    fn dag_section_elides_old_nodes_when_over_limit() {
        let mut nodes = Vec::new();
        for i in 0..(DAG_NODE_LIMIT + 5) {
            // Each parents the previous, building a long linear chain.
            let id = format!("n{i:08x}");
            let parent_str: Vec<String> = if i == 0 {
                vec![]
            } else {
                vec![format!("n{:08x}", i - 1)]
            };
            nodes.push(TraceNode {
                id,
                parent_ids: parent_str,
                kind: "OBSERVE".into(),
                content: format!("step {i}"),
                timestamp: "t".into(),
            });
        }
        let dag = TraceDag { nodes };
        let s = render_dag_section(&dag);
        assert!(
            s.contains("earlier node"),
            "elision marker must be present when over limit"
        );
        // Edges that reference an elided node must not appear, otherwise
        // Mermaid declares an unstyled phantom node.
        assert!(
            !s.contains(&"n_n00000000 --> ".to_string()),
            "edges from elided nodes must be suppressed"
        );
    }

    #[test]
    fn dag_section_sanitizes_dangerous_label_chars() {
        let dag = TraceDag {
            nodes: vec![make_node(
                "a1b2c3d4",
                "NOTE",
                "weird \"quotes\" <html> and \\ backslashes",
                &[],
            )],
        };
        let s = render_dag_section(&dag);
        // No raw double-quotes inside the label, no raw `<` or `>` (would break Mermaid).
        // The label is wrapped in double-quotes, so look at the label substring.
        let label_start = s.find("NOTE ·").expect("label present");
        let label_end = s[label_start..].find("\"]").unwrap() + label_start;
        let label = &s[label_start..label_end];
        assert!(!label.contains('"'), "raw quote leaked into label: {label}");
        assert!(!label.contains('<'), "raw < leaked into label: {label}");
        assert!(!label.contains('>'), "raw > leaked into label: {label}");
        assert!(!label.contains('\\'), "raw \\ leaked into label: {label}");
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
}
