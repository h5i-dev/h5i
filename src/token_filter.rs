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
    /// The command argv that produced the output, when known. Enables the
    /// command-aware adapter layer (pytest/cargo/git) for higher-quality
    /// summaries; falls back to the generic scorer when no adapter matches.
    /// Only consulted when `kind` is `Auto` (an explicit `--kind` opts out).
    pub cmd: Option<Vec<String>>,
}

impl Default for FilterConfig {
    fn default() -> Self {
        FilterConfig {
            kind: OutputKind::Auto,
            head_lines: DEFAULT_HEAD,
            tail_lines: DEFAULT_TAIL,
            max_lines: DEFAULT_MAX_LINES,
            token_budget: None,
            cmd: None,
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
    // Sample a bounded prefix AND tail: a failure that only shows up late in a
    // huge log (the summary line, the final panic) must still drive
    // classification — head-only sampling would miss it.
    let all: Vec<&str> = text.lines().collect();
    let mut sample: Vec<&str> = all.iter().take(80).copied().collect();
    if all.len() > 80 {
        sample.extend(all.iter().skip(all.len().saturating_sub(80)).copied());
    }

    // Test output: pytest/cargo/jest/go vocabulary.
    let sample_lower = sample.join("\n").to_ascii_lowercase();
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
    let hits = test_markers
        .iter()
        .filter(|m| sample_lower.contains(**m))
        .count();
    if hits >= 2 {
        return OutputKind::Test;
    }
    // A critical failure anywhere in the sample, or several log-level lines,
    // makes this a log rather than opaque generic text. (Mid-log failures
    // outside the sample are still preserved by the scored summarizer, which
    // generic output also routes through — classification only sets the label.)
    let mut crit = 0;
    let mut log_hits = 0;
    for l in &sample {
        let s = line_score(l);
        if s >= 1.0 {
            crit += 1;
        }
        if s >= 0.7 {
            log_hits += 1;
        }
    }
    if crit >= 1 || log_hits >= 2 {
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
    // Command-aware adapters (pytest/cargo/git) produce materially better
    // summaries for known tools. They only run when the kind isn't forced, and
    // any adapter may decline (return None) to fall back to the generic scorer.
    if cfg.kind == OutputKind::Auto {
        if let Some(cmd) = &cfg.cmd {
            if let Some(res) = summarize_command(cmd, raw, cfg) {
                return res;
            }
        }
    }

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

// ── Command-aware adapters ──────────────────────────────────────────────────
//
// A thin layer that recognizes a handful of high-traffic commands and produces
// a semantic summary (e.g. "Pytest: 184 passed, 2 failed in 3.21s" + the failure
// blocks) rather than a generic head/tail+errors reduction. The contract is
// deliberately simple — each adapter is deterministic, dependency-light, and may
// decline by returning `None`, in which case the generic scorer takes over.
//
// This is phase 1.5 of the token-reduction feature: it captures the proven RTK
// idea of per-command parsers without adopting a config format yet. Adapters
// implemented: pytest, cargo (test/check/clippy/build), git diff. Deferred:
// git status/log, npm/jest/vitest.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CargoSub {
    Test,
    Check,
    Clippy,
    Build,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Pytest,
    Cargo(CargoSub),
    GitDiff,
}

/// Command-aware summary for known tools. Returns `None` to fall back to the
/// generic scorer. Mirrors the contract proposed for phase 1.5:
/// `summarize_command(cmd, stdout_stderr, cfg) -> Option<FilterResult>`.
pub fn summarize_command(cmd: &[String], output: &str, cfg: &FilterConfig) -> Option<FilterResult> {
    let tool = detect_tool(cmd)?;
    let cleaned = strip_ansi(output);
    let (summary, highlights, kind, kept_lines) = match tool {
        Tool::Pytest => pytest_summary(&cleaned, cfg)?,
        Tool::Cargo(sub) => cargo_summary(&cleaned, sub, cfg)?,
        Tool::GitDiff => git_diff_summary(&cleaned, cfg)?,
    };
    let raw_lines = cleaned.lines().count();
    let raw_tokens = count_tokens(&cleaned);
    let summary = apply_token_budget(summary, cfg.token_budget);
    let summary_tokens = count_tokens(&summary);
    Some(FilterResult {
        summary,
        kind,
        highlights,
        raw_lines,
        kept_lines,
        raw_tokens,
        summary_tokens,
    })
}

/// Tokenize an argv into bare words, splitting embedded shell strings
/// (`bash -c "pytest -q"`) and stripping path prefixes + surrounding quotes.
fn argv_words(cmd: &[String]) -> Vec<String> {
    cmd.iter()
        .flat_map(|a| a.split_whitespace())
        .map(|w| {
            let w = w.trim_matches(|c| c == '"' || c == '\'');
            // Basename: drop any path prefix (e.g. /usr/bin/cargo → cargo).
            w.rsplit('/').next().unwrap_or(w).to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Identify a known tool from the command argv, or `None`.
fn detect_tool(cmd: &[String]) -> Option<Tool> {
    let words = argv_words(cmd);
    if words.is_empty() {
        return None;
    }
    // pytest, however invoked (`pytest`, `python -m pytest`, `uv run pytest`).
    if words.iter().any(|w| w == "pytest" || w == "py.test") {
        return Some(Tool::Pytest);
    }
    // cargo <sub>.
    if let Some(i) = words.iter().position(|w| w == "cargo") {
        let sub = words.get(i + 1).map(|s| s.as_str()).unwrap_or("");
        let sub = match sub {
            "test" | "t" | "nextest" => CargoSub::Test,
            "check" | "c" => CargoSub::Check,
            "clippy" => CargoSub::Clippy,
            "build" | "b" => CargoSub::Build,
            "" => return None,
            _ => CargoSub::Other,
        };
        return Some(Tool::Cargo(sub));
    }
    // git diff / git show.
    if let Some(i) = words.iter().position(|w| w == "git") {
        if let Some(sub) = words.get(i + 1) {
            if sub == "diff" || sub == "show" {
                return Some(Tool::GitDiff);
            }
        }
    }
    None
}

/// Cap a list of kept lines to the configured budget, appending an explicit
/// elision marker when truncated.
fn cap_block(mut lines: Vec<String>, max: usize) -> Vec<String> {
    if lines.len() > max && max > 0 {
        let dropped = lines.len() - max;
        lines.truncate(max);
        lines.push(format!("… [{dropped} more line{} elided] …", plural(dropped)));
    }
    lines
}

/// Parse pytest's count summary line ("=== 2 failed, 184 passed in 3.2s ===")
/// into a compact "184 passed, 2 failed in 3.2s".
fn parse_pytest_counts(line: &str) -> String {
    let re = regex::Regex::new(
        r"(\d+)\s+(passed|failed|error|errors|skipped|deselected|xfailed|xpassed|warning|warnings)",
    )
    .unwrap();
    let mut parts: Vec<String> = Vec::new();
    for cap in re.captures_iter(line) {
        parts.push(format!("{} {}", &cap[1], &cap[2]));
    }
    if let Some(t) = regex::Regex::new(r"in\s+([0-9.]+)s")
        .unwrap()
        .captures(line)
        .map(|c| c[1].to_string())
    {
        if parts.is_empty() {
            return format!("finished in {t}s");
        }
        return format!("{} in {t}s", parts.join(", "));
    }
    parts.join(", ")
}

fn is_pytest_failure_header(t: &str) -> bool {
    // "_________ test_name _________"
    t.starts_with('_') && t.ends_with('_') && !t.trim_matches('_').trim().is_empty()
}

fn pytest_summary(
    text: &str,
    cfg: &FilterConfig,
) -> Option<(String, Vec<String>, OutputKind, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    // The summary line: a "=== … in <time>s ===" near the end mentioning a
    // pytest count keyword. Its presence is what confirms this is pytest output.
    let summary_line = lines.iter().rev().find(|l| {
        let t = l.trim().trim_matches('=').trim();
        (t.contains(" in ") || t.starts_with("no tests ran"))
            && (t.contains("passed")
                || t.contains("failed")
                || t.contains("error")
                || t.contains("skipped")
                || t.contains("no tests ran"))
    })?;

    let counts = parse_pytest_counts(summary_line.trim().trim_matches('=').trim());
    let headline = format!("Pytest: {counts}");

    let mut keep: Vec<String> = Vec::new();
    let mut highlights: Vec<String> = Vec::new();
    let mut failures = 0usize;
    let mut in_failures = false;
    const MAX_FAILURES: usize = 10;

    for l in &lines {
        let t = l.trim();
        if t.starts_with('=') && t.contains("FAILURES") {
            in_failures = true;
            continue;
        }
        if t.starts_with('=') && (t.contains("short test summary") || t.contains("warnings summary"))
        {
            in_failures = false;
        }
        // Short-summary lines are the highest-signal, keep them regardless.
        if t.starts_with("FAILED ")
            || t.starts_with("ERROR ")
            || t.starts_with("XFAIL")
            || t.starts_with("XPASS")
        {
            keep.push(t.to_string());
            highlights.push(t.to_string());
            continue;
        }
        if in_failures {
            if is_pytest_failure_header(t) {
                failures += 1;
                if failures <= MAX_FAILURES {
                    keep.push(t.to_string());
                }
                continue;
            }
            // Assertion error lines ("E   assert 1 == 2") and locations.
            if failures <= MAX_FAILURES && (t.starts_with("E   ") || t.starts_with("E\t")) {
                keep.push(t.to_string());
                if highlights.len() < 20 {
                    highlights.push(t.to_string());
                }
            }
        }
    }

    let mut summary = headline;
    if !keep.is_empty() {
        // Dedup consecutive identical lines, then cap.
        keep.dedup();
        let keep = cap_block(keep, cfg.max_lines);
        summary.push_str("\n\n");
        summary.push_str(&keep.join("\n"));
    }
    if failures > MAX_FAILURES {
        summary.push_str(&format!(
            "\n… [{} more failing test{} — see raw] …",
            failures - MAX_FAILURES,
            plural(failures - MAX_FAILURES)
        ));
    }
    let kept = summary.lines().count();
    Some((summary, highlights, OutputKind::Test, kept))
}

fn cargo_summary(
    text: &str,
    sub: CargoSub,
    cfg: &FilterConfig,
) -> Option<(String, Vec<String>, OutputKind, usize)> {
    // Cargo progress noise that carries no signal once a run is done.
    const NOISE: &[&str] = &[
        "Compiling",
        "Checking",
        "Downloading",
        "Downloaded",
        "Updating",
        "Finished",
        "Blocking",
        "Locking",
        "Installing",
        "Fresh",
        "Adding",
    ];
    let lines: Vec<&str> = text.lines().collect();
    let mut keep: Vec<String> = Vec::new();
    let mut highlights: Vec<String> = Vec::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut test_results: Vec<String> = Vec::new();
    let mut in_block = false;

    for l in &lines {
        let t = l.trim_end();
        let tl = t.trim_start();
        if NOISE.iter().any(|n| tl.starts_with(n)) {
            in_block = false;
            continue;
        }
        // Aggregate test result tallies.
        if tl.starts_with("test result:") {
            test_results.push(tl.to_string());
            in_block = false;
            continue;
        }
        // Cargo's trailing roll-up lines ("error: could not compile … due to N
        // previous errors", "error: test failed", "error: aborting …") are
        // summaries, not new diagnostics — keep them but don't double-count.
        if tl.starts_with("error: ")
            && (tl.contains("could not compile")
                || tl.contains("test failed")
                || tl.contains("build failed")
                || tl.contains("aborting")
                || tl.contains("due to"))
        {
            in_block = false;
            keep.push(t.to_string());
            highlights.push(tl.to_string());
            continue;
        }
        // rustc diagnostic block starts.
        if tl.starts_with("error[") || tl == "error" || tl.starts_with("error:") {
            errors += 1;
            in_block = true;
            keep.push(t.to_string());
            if highlights.len() < 20 {
                highlights.push(tl.to_string());
            }
            continue;
        }
        if tl.starts_with("warning:") {
            warnings += 1;
            in_block = true;
            keep.push(t.to_string());
            continue;
        }
        // Per-test failure listing.
        if tl.starts_with("---- ") && tl.ends_with("----") {
            keep.push(t.to_string());
            continue;
        }
        if in_block {
            if t.trim().is_empty() {
                in_block = false;
                keep.push(String::new()); // preserve the block separator
            } else {
                keep.push(t.to_string());
            }
        }
    }

    let sub_name = match sub {
        CargoSub::Test => "test",
        CargoSub::Check => "check",
        CargoSub::Clippy => "clippy",
        CargoSub::Build => "build",
        CargoSub::Other => "cargo",
    };
    let mut headline = format!("Cargo {sub_name}:");
    match (errors, warnings) {
        (0, 0) => headline.push_str(" ok"),
        (e, 0) => headline.push_str(&format!(" {e} error{}", plural(e))),
        (0, w) => headline.push_str(&format!(" {w} warning{}", plural(w))),
        (e, w) => headline.push_str(&format!(" {e} error{}, {w} warning{}", plural(e), plural(w))),
    }

    let mut summary = headline;
    for tr in &test_results {
        summary.push('\n');
        summary.push_str(tr);
    }
    if !keep.is_empty() {
        // Trim leading/trailing blank lines, collapse runs, and cap.
        while keep.first().map(|s| s.is_empty()).unwrap_or(false) {
            keep.remove(0);
        }
        while keep.last().map(|s| s.is_empty()).unwrap_or(false) {
            keep.pop();
        }
        let keep = cap_block(keep, cfg.max_lines);
        if !keep.is_empty() {
            summary.push_str("\n\n");
            summary.push_str(&keep.join("\n"));
        }
    }
    let kept = summary.lines().count();
    Some((summary, highlights, OutputKind::Test, kept))
}

fn git_diff_summary(
    text: &str,
    cfg: &FilterConfig,
) -> Option<(String, Vec<String>, OutputKind, usize)> {
    // Confirm this looks like a diff before claiming it.
    if !text.contains("diff --git") && !(text.contains("\n+++ ") || text.starts_with("+++ ")) {
        return None;
    }
    let mut files = 0usize;
    let mut adds = 0usize;
    let mut dels = 0usize;
    for l in text.lines() {
        if l.starts_with("diff --git") {
            files += 1;
        } else if l.starts_with('+') && !l.starts_with("+++") {
            adds += 1;
        } else if l.starts_with('-') && !l.starts_with("---") {
            dels += 1;
        }
    }
    let headline = format!(
        "git diff: {files} file{} changed, +{adds} -{dels}",
        plural(files)
    );
    // Reuse the structural diff reducer for the body.
    let (body, mut highlights, _kept) = summarize_diff(text, cfg);
    highlights.insert(0, headline.clone());
    let summary = format!("{headline}\n\n{body}");
    let kept = summary.lines().count();
    Some((summary, highlights, OutputKind::Diff, kept))
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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detect_tool_handles_wrappers_and_subcommands() {
        assert_eq!(detect_tool(&argv(&["pytest", "-q"])), Some(Tool::Pytest));
        assert_eq!(
            detect_tool(&argv(&["bash", "-c", "pytest -q tests/"])),
            Some(Tool::Pytest)
        );
        assert_eq!(
            detect_tool(&argv(&["python", "-m", "pytest"])),
            Some(Tool::Pytest)
        );
        assert_eq!(
            detect_tool(&argv(&["/usr/bin/cargo", "test", "--lib"])),
            Some(Tool::Cargo(CargoSub::Test))
        );
        assert_eq!(
            detect_tool(&argv(&["cargo", "clippy"])),
            Some(Tool::Cargo(CargoSub::Clippy))
        );
        assert_eq!(detect_tool(&argv(&["git", "diff", "--cached"])), Some(Tool::GitDiff));
        assert_eq!(detect_tool(&argv(&["ls", "-la"])), None);
        assert_eq!(detect_tool(&argv(&["echo", "hi"])), None);
    }

    #[test]
    fn pytest_all_pass_shrinks_to_one_line() {
        let mut raw = String::from("============ test session starts ============\n");
        for i in 0..184 {
            raw.push_str(&format!("tests/test_mod.py::test_{i} PASSED\n"));
        }
        raw.push_str("====================== 184 passed in 2.53s ======================\n");
        let res = summarize_command(&argv(&["pytest", "-q"]), &raw, &FilterConfig::default()).unwrap();
        assert!(res.summary.contains("Pytest: 184 passed in 2.53s"));
        // Aggressive: success collapses to a single semantic line.
        assert_eq!(res.summary.lines().count(), 1);
        assert!(res.raw_lines > 100);
    }

    #[test]
    fn pytest_keeps_buried_failures() {
        let mut raw = String::from("============ test session starts ============\n");
        for i in 0..300 {
            raw.push_str(&format!("tests/test_mod.py::test_{i} PASSED\n"));
        }
        raw.push_str("=================== FAILURES ===================\n");
        raw.push_str("__________________ test_thing __________________\n");
        raw.push_str("    assert add(1, 2) == 4\n");
        raw.push_str("E   assert 3 == 4\n");
        raw.push_str("=============== short test summary info ===============\n");
        raw.push_str("FAILED tests/test_mod.py::test_thing - assert 3 == 4\n");
        raw.push_str("============ 1 failed, 300 passed in 4.10s ============\n");
        let res = summarize_command(&argv(&["pytest"]), &raw, &FilterConfig::default()).unwrap();
        assert!(res.summary.contains("1 failed, 300 passed"));
        assert!(res.summary.contains("FAILED tests/test_mod.py::test_thing"));
        assert!(res.summary.contains("E   assert 3 == 4"));
        // Still a big reduction.
        assert!(res.summary.lines().count() < res.raw_lines / 5);
    }

    #[test]
    fn pytest_summary_line_required_else_none() {
        // Output without a pytest summary line should decline (fall back).
        let raw = "random text\nno pytest here\n";
        assert!(summarize_command(&argv(&["pytest"]), raw, &FilterConfig::default()).is_none());
    }

    #[test]
    fn cargo_all_pass_shrinks_but_not_empty() {
        let raw = "   Compiling foo v0.1.0\n    Checking foo v0.1.0\n    Finished test [unoptimized] target(s) in 3.2s\n     Running unittests src/lib.rs\ntest result: ok. 569 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let res = summarize_command(&argv(&["cargo", "test"]), raw, &FilterConfig::default()).unwrap();
        assert!(res.summary.contains("Cargo test: ok"));
        assert!(res.summary.contains("test result: ok. 569 passed"));
        assert!(!res.summary.trim().is_empty());
        assert!(res.summary.lines().count() <= 3);
    }

    #[test]
    fn cargo_keeps_error_blocks() {
        let raw = "   Compiling foo v0.1.0\nerror[E0382]: borrow of moved value: `x`\n  --> src/main.rs:10:5\n   |\n10 |     x;\n   |     ^ value borrowed here after move\n\nerror: could not compile `foo` due to previous error\n";
        let res = summarize_command(&argv(&["cargo", "build"]), raw, &FilterConfig::default()).unwrap();
        assert!(res.summary.contains("Cargo build: 1 error"));
        assert!(res.summary.contains("error[E0382]"));
        assert!(res.summary.contains("src/main.rs:10:5"));
        assert!(res.summary.contains("could not compile"));
        assert!(!res.summary.contains("Compiling foo"), "noise should be stripped");
    }

    #[test]
    fn git_diff_adds_file_count_header() {
        let raw = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,2 @@\n-old line\n+new line\n context\n";
        let res = summarize_command(&argv(&["git", "diff"]), raw, &FilterConfig::default()).unwrap();
        assert!(res.summary.starts_with("git diff: 1 file changed, +1 -1"));
        assert_eq!(res.kind, OutputKind::Diff);
    }

    #[test]
    fn filter_routes_through_adapter_when_cmd_set() {
        let raw = "============ 5 passed in 0.10s ============\n";
        let cfg = FilterConfig {
            cmd: Some(argv(&["pytest", "-q"])),
            ..Default::default()
        };
        let res = filter(raw, &cfg);
        assert!(res.summary.contains("Pytest: 5 passed"));
        // An explicit --kind opts out of adapters.
        let cfg2 = FilterConfig {
            cmd: Some(argv(&["pytest", "-q"])),
            kind: OutputKind::Generic,
            ..Default::default()
        };
        let res2 = filter(raw, &cfg2);
        assert!(!res2.summary.contains("Pytest:"));
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
