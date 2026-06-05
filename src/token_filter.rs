//! Deterministic, dependency-free token-reduction filters.
//!
//! This module takes a (potentially huge) raw tool output and reduces it to a
//! small, faithful summary that an agent can read without burning its context
//! window. The full raw bytes are stored out-of-band by [`crate::objects`]; the
//! summary produced here is what travels in the git-tracked manifest.
//!
//! Design constraints (see `docs` and the project's TokenReduction study):
//!   - **Deterministic.** No model, no randomness — the same input always yields
//!     the same summary. This is what makes [`FILTER_VERSION`] meaningful: a
//!     stored summary can be regenerated and compared byte-for-byte.
//!   - **Lossless escape hatch.** The filter never *invents* text; elided
//!     regions are marked with an explicit count, and the raw is always
//!     retrievable via the object store.
//!   - **Cheap.** Single pass line scoring + a bounded selection. Borrowed
//!     ideas: RTK's per-kind line filtering + head/tail, Headroom's log line
//!     scoring + dedup, Context-Mode's byte-safe truncation.
//!
//! The summarization strategy, by [`OutputKind`]:
//!   - `Test` / `Log` — score each line (errors/panics/failures high, noise
//!     low), keep the head, the tail, and every high-score line, dedup runs of
//!     identical lines, then cap to a line budget.
//!   - `Json` — parse with serde_json and emit a structural skeleton (top-level
//!     shape, key types, array lengths) plus any error/status fields.
//!   - `Diff` — keep file headers, hunk headers, and a bounded window of changed
//!     lines per hunk.
//!   - `Generic` — head + tail with a byte/line budget.

use serde::{Deserialize, Serialize};

/// Bump when the summarization algorithm changes in a way that would alter the
/// `summary` text for the same input. Stored in each manifest so a reader knows
/// which algorithm produced a summary (and whether it can be regenerated).
pub const FILTER_VERSION: u32 = 1;

/// Default number of leading lines preserved verbatim.
pub const DEFAULT_HEAD: usize = 12;
/// Default number of trailing lines preserved verbatim.
pub const DEFAULT_TAIL: usize = 12;
/// Default upper bound on the number of lines in a summary.
pub const DEFAULT_MAX_LINES: usize = 80;

/// The model used for best-effort token counting. cl100k-family; only used to
/// annotate the manifest, never to gate behaviour.
const TOKEN_MODEL: &str = "gpt-4";

/// What kind of output we're summarizing. `Auto` asks the filter to classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    Auto,
    Test,
    Log,
    Json,
    Diff,
    Generic,
}

impl OutputKind {
    /// Parse a user-supplied `--kind` value. Unknown values fall back to `Auto`.
    pub fn parse(s: &str) -> OutputKind {
        match s.trim().to_ascii_lowercase().as_str() {
            "test" | "tests" => OutputKind::Test,
            "log" | "logs" => OutputKind::Log,
            "json" => OutputKind::Json,
            "diff" | "patch" => OutputKind::Diff,
            "generic" | "text" | "raw" => OutputKind::Generic,
            _ => OutputKind::Auto,
        }
    }

    /// The string written into the manifest's `kind` field.
    pub fn as_str(self) -> &'static str {
        match self {
            OutputKind::Auto => "generic",
            OutputKind::Test => "test",
            OutputKind::Log => "log",
            OutputKind::Json => "json",
            OutputKind::Diff => "diff",
            OutputKind::Generic => "generic",
        }
    }
}

/// Tunables for a single filter run.
#[derive(Debug, Clone)]
pub struct FilterConfig {
    pub kind: OutputKind,
    pub head_lines: usize,
    pub tail_lines: usize,
    pub max_lines: usize,
    /// Optional cap on summary tokens (best-effort; uses tiktoken when available).
    pub token_budget: Option<usize>,
}

impl Default for FilterConfig {
    fn default() -> Self {
        FilterConfig {
            kind: OutputKind::Auto,
            head_lines: DEFAULT_HEAD,
            tail_lines: DEFAULT_TAIL,
            max_lines: DEFAULT_MAX_LINES,
            token_budget: None,
        }
    }
}

/// The result of filtering: a small summary plus accounting metadata.
#[derive(Debug, Clone)]
pub struct FilterResult {
    /// The reduced text shown to the agent.
    pub summary: String,
    /// The kind that was actually used (after classification).
    pub kind: OutputKind,
    /// The highest-signal lines, extracted for quick scanning / manifest search.
    pub highlights: Vec<String>,
    pub raw_lines: usize,
    pub kept_lines: usize,
    /// Best-effort token counts (None when the tokenizer is unavailable).
    pub raw_tokens: Option<usize>,
    pub summary_tokens: Option<usize>,
}

/// Best-effort token count. Returns `None` rather than failing the whole capture
/// when the tokenizer can't be loaded.
pub fn count_tokens(text: &str) -> Option<usize> {
    crate::metadata::count_tokens(text, TOKEN_MODEL).ok()
}

/// Strip ANSI/VT100 escape sequences (colors, cursor moves). Hand-rolled to
/// avoid pulling in a regex just for this; handles the common CSI form
/// `ESC [ ... <final>` and the lone `ESC` + single char case.
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: consume until a final byte in 0x40..=0x7e.
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // skip the final byte
            } else {
                // Lone ESC or two-char sequence; drop the ESC and the next byte.
                i += 2;
            }
            continue;
        }
        // Carriage returns inside progress bars overwrite the line; emulate the
        // terminal by discarding the current (un-newlined) line so we keep only
        // the final state. A CRLF is left intact (real line break).
        if bytes[i] == b'\r' && !(i + 1 < bytes.len() && bytes[i + 1] == b'\n') {
            match out.rfind('\n') {
                Some(pos) => out.truncate(pos + 1),
                None => out.clear(),
            }
            i += 1;
            continue;
        }
        // Copy this UTF-8 scalar wholesale to stay on char boundaries.
        let ch_len = utf8_len(bytes[i]);
        let end = (i + ch_len).min(bytes.len());
        if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
            out.push_str(s);
        }
        i = end;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Classify raw output into an [`OutputKind`] using cheap structural heuristics.
pub fn classify(text: &str) -> OutputKind {
    let trimmed = text.trim_start();
    // JSON: starts with a brace/bracket and parses.
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return OutputKind::Json;
    }
    // Unified diff: a `diff --git` header or a `---`/`+++` file pair with `@@`.
    let head: String = text.lines().take(40).collect::<Vec<_>>().join("\n");
    if head.contains("diff --git")
        || (head.contains("\n--- ") && head.contains("\n+++ ") && head.contains("@@"))
        || head.starts_with("--- ") && head.contains("+++ ")
    {
        return OutputKind::Diff;
    }
    // Test output: pytest/cargo/jest/go vocabulary.
    let lower = head.to_ascii_lowercase();
    let test_markers = [
        "test result",
        "passed",
        "failed",
        "running ",
        "test session starts",
        "=== run",
        "--- fail",
        "ok ",
        "assertionerror",
        "tests passed",
        "failures:",
    ];
    let hits = test_markers.iter().filter(|m| lower.contains(**m)).count();
    if hits >= 2 {
        return OutputKind::Test;
    }
    // Logs: lines that look like log records (level tags / timestamps).
    let log_hits = text
        .lines()
        .take(80)
        .filter(|l| line_score(l) >= 0.7)
        .count();
    if log_hits >= 2 {
        return OutputKind::Log;
    }
    OutputKind::Generic
}

/// Score a single line 0.0..=1.0 by how important it is to keep. Higher means
/// "more likely an error / signal an agent must see".
pub fn line_score(line: &str) -> f32 {
    let l = line.trim();
    if l.is_empty() {
        return 0.0;
    }
    let lower = l.to_ascii_lowercase();

    // Highest signal: hard failures, panics, exceptions.
    const CRIT: &[&str] = &[
        "panic",
        "traceback (most recent call last)",
        "fatal",
        "segmentation fault",
        "assertionerror",
        "error[", // rustc / clang style
        "exception",
        "failed to compile",
    ];
    if CRIT.iter().any(|p| lower.contains(p)) {
        return 1.0;
    }

    // Failures and errors.
    const ERR: &[&str] = &[
        "error", "failed", "failure", " fail", "fail:", "✗", "✘", "✖", "❌", "denied", "cannot ",
        "could not", "unable to", "not found", "missing", "undefined",
    ];
    if ERR.iter().any(|p| lower.contains(p)) {
        return 0.9;
    }

    // Stack-trace frames (Python "File ...", Rust "  at ...", node "  at fn").
    if l.starts_with("File \"") || l.starts_with("at ") || l.starts_with("  at ") {
        return 0.85;
    }

    // Warnings.
    if lower.contains("warning") || lower.contains("warn ") || lower.starts_with("warn") {
        return 0.7;
    }

    // Summary / status lines (counts, results).
    const SUMMARY: &[&str] = &[
        "passed",
        "test result",
        "tests run",
        "summary",
        "result:",
        "===",
        "ok",
        "build succeeded",
        "finished",
        "exit code",
    ];
    if SUMMARY.iter().any(|p| lower.contains(p)) {
        return 0.6;
    }

    // File paths with line numbers are usually worth keeping.
    if looks_like_path_with_line(l) {
        return 0.5;
    }

    0.1
}

/// Heuristic: does the line contain a `path:line` or `path:line:col` reference?
fn looks_like_path_with_line(l: &str) -> bool {
    // Find a token containing ".<ext>:<digits>".
    l.split_whitespace().any(|tok| {
        if let Some(colon) = tok.find(':') {
            let (path, rest) = tok.split_at(colon);
            let rest = &rest[1..];
            path.contains('.')
                && rest
                    .split(':')
                    .next()
                    .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false)
        } else {
            false
        }
    })
}

/// Filter `raw` according to `cfg`, returning a small summary.
pub fn filter(raw: &str, cfg: &FilterConfig) -> FilterResult {
    let cleaned = strip_ansi(raw);
    let kind = match cfg.kind {
        OutputKind::Auto => classify(&cleaned),
        k => k,
    };
    let raw_lines = cleaned.lines().count();
    let raw_tokens = count_tokens(&cleaned);

    // Score-based summarization is the general case: it keeps head + tail *and*
    // every high-signal line, so an error buried deep in otherwise-noisy output
    // is never silently dropped. Only JSON and diffs get structure-aware paths.
    let (summary, highlights, kept_lines) = match kind {
        OutputKind::Json => summarize_json(&cleaned, cfg),
        OutputKind::Diff => summarize_diff(&cleaned, cfg),
        OutputKind::Test | OutputKind::Log | OutputKind::Generic | OutputKind::Auto => {
            summarize_scored(&cleaned, cfg)
        }
    };

    let summary = apply_token_budget(summary, cfg.token_budget);
    let summary_tokens = count_tokens(&summary);

    FilterResult {
        summary,
        kind,
        highlights,
        raw_lines,
        kept_lines,
        raw_tokens,
        summary_tokens,
    }
}

/// Score-based summarization for tests and logs: keep head + tail + every
/// high-signal line, dedup identical runs, cap to the line budget.
fn summarize_scored(text: &str, cfg: &FilterConfig) -> (String, Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let threshold = 0.6_f32;

    // Indices we must keep: head, tail, and every high-signal line.
    let mut keep = vec![false; n];
    let head_end = cfg.head_lines.min(n);
    keep[..head_end].fill(true);
    keep[n.saturating_sub(cfg.tail_lines)..].fill(true);
    let mut highlights = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line_score(line) >= threshold {
            keep[i] = true;
            if highlights.len() < 20 && line_score(line) >= 0.85 {
                let h = line.trim();
                if !h.is_empty() {
                    highlights.push(h.to_string());
                }
            }
        }
    }

    // If we're still over budget, drop the lowest-signal kept lines that are
    // neither head nor tail until we fit.
    let mut kept_idx: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
    if kept_idx.len() > cfg.max_lines {
        let tail_start = n.saturating_sub(cfg.tail_lines);
        // Candidates eligible for dropping (middle, lower score first).
        let mut middle: Vec<usize> = kept_idx
            .iter()
            .copied()
            .filter(|&i| i >= head_end && i < tail_start)
            .collect();
        middle.sort_by(|&a, &b| {
            line_score(lines[a])
                .partial_cmp(&line_score(lines[b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut to_drop = kept_idx.len().saturating_sub(cfg.max_lines);
        for &i in &middle {
            if to_drop == 0 {
                break;
            }
            keep[i] = false;
            to_drop -= 1;
        }
        kept_idx = (0..n).filter(|&i| keep[i]).collect();
    }

    // Emit, inserting elision markers for gaps and deduping identical runs.
    let out = render_with_gaps(&lines, &kept_idx);
    (out, highlights, kept_idx.len())
}

/// Generic summarization: head + tail with a gap marker.
fn summarize_generic(text: &str, cfg: &FilterConfig) -> (String, Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    if n <= cfg.head_lines + cfg.tail_lines {
        return (text.to_string(), Vec::new(), n);
    }
    let mut keep = Vec::new();
    for i in 0..cfg.head_lines {
        keep.push(i);
    }
    for i in n.saturating_sub(cfg.tail_lines)..n {
        keep.push(i);
    }
    let out = render_with_gaps(&lines, &keep);
    (out, Vec::new(), keep.len())
}

/// Render selected line indices, collapsing dropped spans into an explicit
/// `… [N lines elided] …` marker and consecutive identical kept lines into a
/// `(×N)` count.
fn render_with_gaps(lines: &[&str], kept_idx: &[usize]) -> String {
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    let mut i = 0;
    while i < kept_idx.len() {
        let idx = kept_idx[i];
        if let Some(p) = prev {
            let gap = idx - p - 1;
            if gap > 0 {
                out.push_str(&format!("… [{gap} line{} elided] …\n", plural(gap)));
            }
        }
        // Dedup a run of identical kept lines.
        let mut run = 1;
        while i + run < kept_idx.len()
            && kept_idx[i + run] == idx + run
            && lines[kept_idx[i + run]] == lines[idx]
        {
            run += 1;
        }
        out.push_str(lines[idx]);
        if run > 1 {
            out.push_str(&format!("  (×{run})"));
            prev = Some(idx + run - 1);
            i += run;
        } else {
            prev = Some(idx);
            i += 1;
        }
        out.push('\n');
    }
    // Trim trailing newline for a tidy block.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Structural JSON summary: top-level shape, key types, array lengths, and any
/// error/status fields. Falls back to generic if parsing fails.
fn summarize_json(text: &str, cfg: &FilterConfig) -> (String, Vec<String>, usize) {
    let value: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(_) => return summarize_generic(text, cfg),
    };
    let mut out = String::new();
    let mut highlights = Vec::new();
    describe_json(&value, 0, &mut out, &mut highlights);
    let lines = out.lines().count();
    (out.trim_end().to_string(), highlights, lines)
}

const JSON_MAX_KEYS: usize = 40;
const JSON_MAX_DEPTH: usize = 3;
/// Field names whose values are surfaced verbatim (truncated) in the summary.
const JSON_SIGNAL_KEYS: &[&str] = &[
    "error", "errors", "message", "status", "code", "exit_code", "ok", "success", "failed",
    "reason", "detail", "details",
];

fn describe_json(
    v: &serde_json::Value,
    depth: usize,
    out: &mut String,
    highlights: &mut Vec<String>,
) {
    let indent = "  ".repeat(depth);
    match v {
        serde_json::Value::Object(map) => {
            out.push_str(&format!("{indent}object · {} keys\n", map.len()));
            if depth >= JSON_MAX_DEPTH {
                return;
            }
            for (i, (k, val)) in map.iter().enumerate() {
                if i >= JSON_MAX_KEYS {
                    out.push_str(&format!(
                        "{indent}  … [{} more keys]\n",
                        map.len() - JSON_MAX_KEYS
                    ));
                    break;
                }
                let signal = JSON_SIGNAL_KEYS.contains(&k.to_ascii_lowercase().as_str());
                if signal {
                    let scalar = scalar_preview(val);
                    let hl = format!("{k}: {scalar}");
                    out.push_str(&format!("{indent}  {hl}\n"));
                    highlights.push(hl);
                } else {
                    out.push_str(&format!("{indent}  {k}: {}\n", type_name(val)));
                    if matches!(val, serde_json::Value::Object(_) | serde_json::Value::Array(_)) {
                        describe_json(val, depth + 2, out, highlights);
                    }
                }
            }
        }
        serde_json::Value::Array(arr) => {
            out.push_str(&format!("{indent}array · {} items\n", arr.len()));
            if let Some(first) = arr.first() {
                if depth < JSON_MAX_DEPTH {
                    out.push_str(&format!("{indent}  [0] ·\n"));
                    describe_json(first, depth + 2, out, highlights);
                }
            }
        }
        scalar => {
            out.push_str(&format!("{indent}{}\n", scalar_preview(scalar)));
        }
    }
}

fn type_name(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "bool".into(),
        serde_json::Value::Number(_) => "number".into(),
        serde_json::Value::String(s) => format!("string[{}]", s.len()),
        serde_json::Value::Array(a) => format!("array[{}]", a.len()),
        serde_json::Value::Object(o) => format!("object{{{}}}", o.len()),
    }
}

fn scalar_preview(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            let s = s.replace('\n', " ");
            if s.chars().count() > 120 {
                let t: String = s.chars().take(117).collect();
                format!("\"{t}…\"")
            } else {
                format!("\"{s}\"")
            }
        }
        serde_json::Value::Array(a) => format!("array[{}]", a.len()),
        serde_json::Value::Object(o) => format!("object{{{}}}", o.len()),
        other => other.to_string(),
    }
}

/// Diff summary: keep file + hunk headers and a bounded window of changed lines
/// per hunk, eliding long unchanged context.
fn summarize_diff(text: &str, cfg: &FilterConfig) -> (String, Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let mut keep = vec![false; n];
    let mut highlights = Vec::new();
    const CHANGED_WINDOW: usize = 6; // changed lines kept per hunk before eliding
    let mut changed_in_hunk = 0usize;

    for (i, line) in lines.iter().enumerate() {
        let is_header = line.starts_with("diff --git")
            || line.starts_with("@@")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("rename ");
        if is_header {
            keep[i] = true;
            if line.starts_with("@@") || line.starts_with("diff --git") {
                changed_in_hunk = 0;
                highlights.push((*line).to_string());
            }
            continue;
        }
        let changed = (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---");
        if changed {
            if changed_in_hunk < CHANGED_WINDOW {
                keep[i] = true;
            }
            changed_in_hunk += 1;
        }
    }
    let kept_idx: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
    let kept_idx = if kept_idx.len() > cfg.max_lines {
        kept_idx.into_iter().take(cfg.max_lines).collect()
    } else {
        kept_idx
    };
    let out = render_with_gaps(&lines, &kept_idx);
    (out, highlights, kept_idx.len())
}

/// Apply an optional token budget by trimming whole lines from the *middle*
/// until the summary fits. Best-effort: a no-op if tokenizing is unavailable.
fn apply_token_budget(summary: String, budget: Option<usize>) -> String {
    let Some(budget) = budget else {
        return summary;
    };
    if budget == 0 {
        return summary;
    }
    let Some(tokens) = count_tokens(&summary) else {
        return summary;
    };
    if tokens <= budget {
        return summary;
    }
    let lines: Vec<&str> = summary.lines().collect();
    let n = lines.len();
    if n < 4 {
        return summary;
    }
    // Keep shrinking the middle window until under budget.
    let mut keep_head = n / 2;
    let mut keep_tail = n - keep_head;
    loop {
        if keep_head + (n - keep_tail) >= n || keep_head == 0 {
            break;
        }
        let elided = keep_tail - keep_head;
        let mut candidate = String::new();
        for l in &lines[..keep_head] {
            candidate.push_str(l);
            candidate.push('\n');
        }
        candidate.push_str(&format!("… [{elided} line{} elided for token budget] …\n", plural(elided)));
        for l in &lines[keep_tail..] {
            candidate.push_str(l);
            candidate.push('\n');
        }
        let candidate = candidate.trim_end().to_string();
        match count_tokens(&candidate) {
            Some(t) if t <= budget => return candidate,
            _ => {
                // Shrink the kept window symmetrically.
                if keep_head > 1 {
                    keep_head -= 1;
                }
                if keep_tail < n - 1 {
                    keep_tail += 1;
                }
                if keep_head <= 1 && keep_tail >= n - 1 {
                    return candidate;
                }
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        let s = "\x1b[31mred\x1b[0m plain";
        assert_eq!(strip_ansi(s), "red plain");
    }

    #[test]
    fn strip_ansi_collapses_bare_cr() {
        assert_eq!(strip_ansi("progress 10%\rprogress 100%"), "progress 100%");
        assert_eq!(strip_ansi("keep\r\nnewline"), "keep\r\nnewline");
    }

    #[test]
    fn classify_detects_json() {
        assert_eq!(classify("{\"a\": 1, \"b\": [1,2,3]}"), OutputKind::Json);
    }

    #[test]
    fn classify_detects_diff() {
        let d = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(classify(d), OutputKind::Diff);
    }

    #[test]
    fn classify_detects_test() {
        let t = "running 3 tests\ntest foo ... ok\ntest bar ... FAILED\ntest result: FAILED. 2 passed; 1 failed";
        assert_eq!(classify(t), OutputKind::Test);
    }

    #[test]
    fn line_score_ranks_errors_above_noise() {
        assert!(line_score("thread panicked at 'boom'") > line_score("compiling foo v0.1"));
        assert!(line_score("error[E0382]: borrow of moved value") >= 0.9);
        assert!(line_score("warning: unused variable") >= 0.7);
        assert!(line_score("   ") == 0.0);
    }

    #[test]
    fn scored_filter_keeps_errors_and_elides_noise() {
        let mut raw = String::new();
        for i in 0..500 {
            raw.push_str(&format!("noise line {i}\n"));
        }
        raw.push_str("error: something broke at src/x.rs:10\n");
        for i in 0..500 {
            raw.push_str(&format!("more noise {i}\n"));
        }
        let cfg = FilterConfig::default();
        let res = filter(&raw, &cfg);
        // A buried error must survive even when the bulk classifies as generic.
        assert!(res.summary.contains("error: something broke"));
        assert!(res.summary.contains("elided"));
        assert!(res.kept_lines <= cfg.max_lines);
        assert!(res.raw_lines > res.kept_lines);
        assert!(res.highlights.iter().any(|h| h.contains("something broke")));
    }

    #[test]
    fn json_summary_surfaces_error_fields() {
        let j = r#"{"status":"error","code":500,"message":"db timeout","rows":[1,2,3,4,5]}"#;
        let res = filter(j, &FilterConfig { kind: OutputKind::Json, ..Default::default() });
        assert_eq!(res.kind, OutputKind::Json);
        assert!(res.summary.contains("db timeout"));
        assert!(res.summary.contains("status"));
        assert!(res.highlights.iter().any(|h| h.contains("db timeout")));
    }

    #[test]
    fn small_output_is_returned_intact() {
        let raw = "line a\nline b\nline c";
        let res = filter(raw, &FilterConfig::default());
        assert_eq!(res.summary, raw);
        assert_eq!(res.raw_lines, res.kept_lines);
    }

    #[test]
    fn dedup_collapses_identical_runs() {
        let mut raw = String::new();
        for _ in 0..50 {
            raw.push_str("repeated warning: disk slow\n");
        }
        let res = filter(&raw, &FilterConfig { kind: OutputKind::Log, ..Default::default() });
        assert!(res.summary.contains("(×"));
    }

    #[test]
    fn token_budget_trims_middle() {
        let mut raw = String::new();
        for i in 0..400 {
            raw.push_str(&format!("warning: line {i}\n"));
        }
        let res = filter(
            &raw,
            &FilterConfig { kind: OutputKind::Log, token_budget: Some(50), ..Default::default() },
        );
        if let Some(t) = res.summary_tokens {
            assert!(t <= 80, "expected near budget, got {t}");
        }
    }
}
