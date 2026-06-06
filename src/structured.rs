//! Structured tool results — a normalized, AI-friendly schema for command output.
//!
//! Instead of a per-tool free-text summary, a [`ToolResult`] gives an agent one
//! predictable shape across test runners, compilers, linters, and type checkers:
//! an envelope (`tool`/`kind`/`status`/`exit_code`/`counts`) plus a unified list
//! of [`Finding`]s — a test failure, a compile error, and a lint diagnostic are
//! all the same shape.
//!
//! Design (converged with the codex advisor):
//!   - **JSON is canonical** (stored in the manifest, returned over MCP); the CLI
//!     renders a compact YAML-subset for readability. Both come from the typed
//!     struct, so there's a single source of truth.
//!   - **Never claim success it can't see.** [`Status::from_exit`] never returns
//!     `passed`/`ok` when `exit_code != 0`.
//!   - **No false precision.** Each result records [`ParserConfidence`]; a parser
//!     declines (returns `None`) when its anchors are missing, falling back to a
//!     `generic` result rather than inventing structure.
//!   - **Lossless.** `raw_oid` sits in the envelope; findings/detail are capped
//!     ([`MAX_FINDINGS`], [`MAX_DETAIL_BYTES`]) with the full output one
//!     `h5i recall object` away.

use serde::{Deserialize, Serialize};

/// Bump when the schema changes in a breaking way (tracked separately from the
/// text filter version).
pub const SCHEMA_VERSION: u32 = 1;
/// Max findings retained in a result; the rest are counted in `truncated`.
pub const MAX_FINDINGS: usize = 20;
/// Max bytes of free-form detail per finding.
pub const MAX_DETAIL_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Test,
    Lint,
    Typecheck,
    Build,
    Vcs,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Succeeded (exit 0, no error/failure findings).
    Passed,
    /// Tests/lints/types failed (the tool ran and reported problems).
    Failed,
    /// Tool/parser/infrastructure error (e.g. usage error, couldn't run).
    Error,
    /// Generic fallback couldn't infer pass/fail.
    Unknown,
}

impl Status {
    /// Decide a *safe* status: never `Passed` when the process failed.
    pub fn from_exit(exit_code: Option<i32>, has_failures: bool) -> Status {
        match exit_code {
            Some(0) if !has_failures => Status::Passed,
            Some(0) => Status::Failed, // findings despite exit 0 → surface them
            Some(_) if has_failures => Status::Failed,
            Some(_) => Status::Error, // nonzero, no parsed findings → tool/usage error
            None => Status::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    TestFailure,
    Diagnostic,
    BuildError,
    Panic,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Failure,
}

/// How much to trust the structure: a real parser, a heuristic, or the generic
/// exit-code-only fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParserConfidence {
    Parsed,
    Heuristic,
    Generic,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl Location {
    /// `path:line:col` shorthand.
    pub fn shorthand(&self) -> String {
        match (self.line, self.column) {
            (Some(l), Some(c)) => format!("{}:{}:{}", self.path, l, c),
            (Some(l), None) => format!("{}:{}", self.path, l),
            _ => self.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    /// Stable test id / target, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Exception type / lint rule / error code (e.g. `AssertionError`, `TS2322`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixable: bool,
    /// Deterministic dedupe/query id: tool + rule + normalized location + message.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suppressed {
    pub kind: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Truncated {
    pub findings_total: usize,
    pub findings_shown: usize,
    /// Whether the raw output is larger than what's represented here.
    pub raw: bool,
}

/// One normalized tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub schema_version: u32,
    pub tool: String,
    pub kind: ResultKind,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub counts: std::collections::BTreeMap<String, u64>,
    pub parser_confidence: ParserConfidence,
    /// Full content address of the raw output, e.g. `sha256:<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<Suppressed>,
    #[serde(default)]
    pub truncated: Truncated,
    /// Reduced free-text body — used for the generic/rule path (no structured
    /// parser) and as a supplement. The full raw is always via `raw_oid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl ToolResult {
    /// A bare envelope with no findings (the generic/rule fallback fills `body`).
    pub fn generic(tool: &str, exit_code: Option<i32>) -> ToolResult {
        ToolResult {
            schema_version: SCHEMA_VERSION,
            tool: tool.to_string(),
            kind: ResultKind::Generic,
            status: Status::from_exit(exit_code, false),
            exit_code,
            duration_ms: None,
            counts: Default::default(),
            parser_confidence: ParserConfidence::Generic,
            raw_oid: None,
            findings: Vec::new(),
            suppressed: Vec::new(),
            truncated: Truncated::default(),
            body: None,
        }
    }

    /// Cap findings to [`MAX_FINDINGS`] and per-finding detail to
    /// [`MAX_DETAIL_BYTES`], recording what was dropped in `truncated`.
    pub fn cap(&mut self) {
        let total = self.findings.len();
        if total > MAX_FINDINGS {
            self.findings.truncate(MAX_FINDINGS);
        }
        for f in &mut self.findings {
            if let Some(d) = &mut f.detail {
                if d.len() > MAX_DETAIL_BYTES {
                    let mut end = MAX_DETAIL_BYTES;
                    while end > 0 && !d.is_char_boundary(end) {
                        end -= 1;
                    }
                    d.truncate(end);
                    d.push('…');
                }
            }
        }
        self.truncated.findings_total = total;
        self.truncated.findings_shown = self.findings.len();
    }
}

/// Deterministic fingerprint for a finding: `tool|rule|norm(location)|message`,
/// digits normalized so line shifts don't churn it. First 12 hex of sha256.
pub fn fingerprint(tool: &str, rule: &str, location: &str, message: &str) -> String {
    let norm = |s: &str| s.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>();
    let key = format!("{tool}|{rule}|{}|{}", norm(location), message.trim());
    let hex = crate::objects::sha256_hex(key.as_bytes());
    hex[..12].to_string()
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Canonical JSON (pretty). Used for the manifest store and MCP responses.
pub fn render_json(r: &ToolResult) -> String {
    serde_json::to_string_pretty(r).unwrap_or_default()
}

fn yaml_scalar(s: &str) -> String {
    // Quote only when the scalar could actually be misread as YAML. A bare colon
    // inside a token (e.g. `path:42`) is fine; `: ` (colon-space) is not.
    let needs_quote = s.is_empty()
        || s.contains('"')
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains(" #")
        || s.starts_with([
            '#', ' ', '-', '?', '*', '&', '!', '|', '>', '@', '`', '[', '{', ',', '\'',
        ])
        || s.ends_with(' ')
        || matches!(s, "true" | "false" | "null" | "yes" | "no");
    if s.contains('\n') {
        // Block scalar.
        let indented: String = s.lines().map(|l| format!("    {l}\n")).collect();
        format!("|\n{}", indented.trim_end_matches('\n'))
    } else if needs_quote {
        format!("{:?}", s) // Rust debug = double-quoted with escapes
    } else {
        s.to_string()
    }
}

/// Compact YAML-subset render for the CLI. Deterministic; not a general YAML
/// emitter — it renders exactly the [`ToolResult`] shape.
pub fn render_yaml(r: &ToolResult) -> String {
    let mut out = String::new();
    let kind = serde_plain(&r.kind);
    let status = serde_plain(&r.status);
    out.push_str(&format!("tool: {}\n", yaml_scalar(&r.tool)));
    out.push_str(&format!("kind: {kind}\n"));
    out.push_str(&format!("status: {status}\n"));
    if let Some(code) = r.exit_code {
        out.push_str(&format!("exit_code: {code}\n"));
    }
    if let Some(ms) = r.duration_ms {
        out.push_str(&format!("duration_ms: {ms}\n"));
    }
    if !r.counts.is_empty() {
        let parts: Vec<String> = r.counts.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        out.push_str(&format!("counts: {{ {} }}\n", parts.join(", ")));
    }
    out.push_str(&format!("parser_confidence: {}\n", serde_plain(&r.parser_confidence)));
    if let Some(oid) = &r.raw_oid {
        out.push_str(&format!("raw_oid: {oid}\n"));
    }
    if !r.findings.is_empty() {
        out.push_str("findings:\n");
        for f in &r.findings {
            out.push_str(&format!(
                "  - kind: {}\n    severity: {}\n",
                serde_plain(&f.kind),
                serde_plain(&f.severity)
            ));
            if let Some(id) = &f.id {
                out.push_str(&format!("    id: {}\n", yaml_scalar(id)));
            }
            if let Some(rule) = &f.rule {
                out.push_str(&format!("    rule: {}\n", yaml_scalar(rule)));
            }
            out.push_str(&format!("    message: {}\n", yaml_scalar(&f.message)));
            if let Some(loc) = &f.location {
                out.push_str(&format!("    location: {}\n", yaml_scalar(&loc.shorthand())));
            }
            if let Some(e) = &f.expected {
                out.push_str(&format!("    expected: {}\n", yaml_scalar(e)));
            }
            if let Some(a) = &f.actual {
                out.push_str(&format!("    actual: {}\n", yaml_scalar(a)));
            }
            if let Some(d) = &f.detail {
                if d.contains('\n') {
                    // Block scalar indented under the finding field (6 spaces).
                    out.push_str("    detail: |\n");
                    for l in d.lines() {
                        out.push_str(&format!("      {l}\n"));
                    }
                } else {
                    out.push_str(&format!("    detail: {}\n", yaml_scalar(d)));
                }
            }
            if f.fixable {
                out.push_str("    fixable: true\n");
            }
            out.push_str(&format!("    fingerprint: {}\n", f.fingerprint));
        }
    }
    if r.truncated.findings_total > r.truncated.findings_shown {
        out.push_str(&format!(
            "truncated: {{ findings_total: {}, findings_shown: {} }}\n",
            r.truncated.findings_total, r.truncated.findings_shown
        ));
    }
    if !r.suppressed.is_empty() {
        let parts: Vec<String> = r
            .suppressed
            .iter()
            .map(|s| format!("{}: {}", s.kind, s.count))
            .collect();
        out.push_str(&format!("suppressed: {{ {} }}\n", parts.join(", ")));
    }
    if let Some(body) = &r.body {
        if !body.trim().is_empty() {
            let indented: String = body.lines().map(|l| format!("  {l}\n")).collect();
            out.push_str("body: |\n");
            out.push_str(indented.trim_end_matches('\n'));
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

// ── Parsers ──────────────────────────────────────────────────────────────────
//
// Each parser declines (returns `None`) when its anchors are missing, so the
// caller falls back to a generic result rather than inventing structure.

/// Dispatch to a structured parser based on the command, or `None`.
pub fn parse(cmd: &[String], output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let words: Vec<String> = cmd
        .iter()
        .flat_map(|a| a.split_whitespace())
        .map(|w| w.rsplit('/').next().unwrap_or(w).trim_matches('"').to_string())
        .collect();
    let has = |t: &str| words.iter().any(|w| w == t);

    if has("pytest") || has("py.test") {
        return parse_pytest(output, exit_code);
    }
    if let Some(i) = words.iter().position(|w| w == "cargo") {
        if matches!(words.get(i + 1).map(String::as_str), Some("test" | "t" | "nextest")) {
            return parse_cargo_test(output, exit_code);
        }
    }
    None
}

fn count_kv(line: &str) -> std::collections::BTreeMap<String, u64> {
    let re = regex::Regex::new(
        r"(\d+)\s+(passed|failed|error|errors|skipped|deselected|xfailed|xpassed|ignored|measured)",
    )
    .unwrap();
    let mut m = std::collections::BTreeMap::new();
    for c in re.captures_iter(line) {
        let n: u64 = c[1].parse().unwrap_or(0);
        let key = c[2].trim_end_matches('s').to_string(); // errors→error
        *m.entry(key).or_insert(0) += n;
    }
    m
}

fn loc_from_pytest_id(id: &str) -> Option<Location> {
    // "tests/test_auth.py::test_x" → tests/test_auth.py
    let path = id.split("::").next()?;
    if path.contains('.') {
        Some(Location { path: path.to_string(), line: None, column: None })
    } else {
        None
    }
}

fn parse_pytest(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let lines: Vec<&str> = output.lines().collect();
    // Anchor: the count summary line. Absent → decline.
    let summary = lines.iter().rev().find(|l| {
        let t = l.trim().trim_matches('=').trim();
        (t.contains(" in ") || t.starts_with("no tests ran"))
            && (t.contains("passed") || t.contains("failed") || t.contains("error") || t.contains("skipped"))
    })?;
    let counts = count_kv(summary);
    let failed = counts.get("failed").copied().unwrap_or(0) + counts.get("error").copied().unwrap_or(0);

    let mut findings = Vec::new();
    for l in &lines {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("FAILED ").or_else(|| t.strip_prefix("ERROR ")) {
            let (id, reason) = match rest.split_once(" - ") {
                Some((i, r)) => (i.trim(), r.trim()),
                None => (rest.trim(), ""),
            };
            let rule = reason.split([':', ' ']).next().filter(|s| s.ends_with("Error")).map(str::to_string);
            let loc = loc_from_pytest_id(id);
            findings.push(Finding {
                kind: FindingKind::TestFailure,
                severity: Severity::Failure,
                id: Some(id.to_string()),
                rule: rule.clone(),
                message: if reason.is_empty() { id.to_string() } else { reason.to_string() },
                location: loc.clone(),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                fingerprint: fingerprint(
                    "pytest",
                    rule.as_deref().unwrap_or(""),
                    &loc.map(|l| l.shorthand()).unwrap_or_else(|| id.to_string()),
                    reason,
                ),
            });
        }
    }

    let mut r = ToolResult {
        schema_version: SCHEMA_VERSION,
        tool: "pytest".into(),
        kind: ResultKind::Test,
        status: Status::from_exit(exit_code, failed > 0),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
    };
    r.cap();
    Some(r)
}

fn parse_cargo_test(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let lines: Vec<&str> = output.lines().collect();
    // Anchor: a "test result:" tally or a panic. Absent → decline.
    let has_result = lines.iter().any(|l| l.trim_start().starts_with("test result:"));
    let has_panic = lines.iter().any(|l| l.trim_start().starts_with("thread '"));
    if !has_result && !has_panic {
        return None;
    }
    let mut counts = std::collections::BTreeMap::new();
    for l in &lines {
        if l.trim_start().starts_with("test result:") {
            for (k, v) in count_kv(l) {
                *counts.entry(k).or_insert(0) += v;
            }
        }
    }
    let failed = counts.get("failed").copied().unwrap_or(0);

    // Findings: panic blocks (thread '<test>' panicked at <loc>: …).
    let mut findings = Vec::new();
    let panic_re = regex::Regex::new(r"thread '([^']+)' panicked at ([^:]+):(\d+):(\d+):").unwrap();
    let mut i = 0;
    while i < lines.len() {
        if let Some(c) = panic_re.captures(lines[i].trim()) {
            let test = c[1].to_string();
            let path = c[2].to_string();
            let line: u32 = c[3].parse().unwrap_or(0);
            let col: u32 = c[4].parse().unwrap_or(0);
            // Pull the next few non-empty lines as the assertion detail.
            let mut detail_lines = Vec::new();
            let mut expected = None;
            let mut actual = None;
            let mut j = i + 1;
            while j < lines.len() && j < i + 8 {
                let t = lines[j].trim();
                if t.is_empty() || t.starts_with("note:") {
                    break;
                }
                if let Some(v) = t.strip_prefix("left: ") {
                    actual = Some(v.to_string());
                }
                if let Some(v) = t.strip_prefix("right: ") {
                    expected = Some(v.to_string());
                }
                detail_lines.push(t.to_string());
                j += 1;
            }
            let loc = Location { path: path.clone(), line: Some(line), column: Some(col) };
            findings.push(Finding {
                kind: FindingKind::Panic,
                severity: Severity::Failure,
                id: Some(test.clone()),
                rule: Some("panic".into()),
                message: detail_lines.first().cloned().unwrap_or_else(|| "panicked".into()),
                location: Some(loc.clone()),
                locations: vec![],
                expected,
                actual,
                detail: if detail_lines.len() > 1 { Some(detail_lines.join("\n")) } else { None },
                fixable: false,
                fingerprint: fingerprint("cargo", "panic", &loc.shorthand(), &test),
            });
            i = j;
        } else {
            i += 1;
        }
    }

    let mut r = ToolResult {
        schema_version: SCHEMA_VERSION,
        tool: "cargo".into(),
        kind: ResultKind::Test,
        status: Status::from_exit(exit_code, failed > 0 || !findings.is_empty()),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
    };
    r.cap();
    Some(r)
}

/// Lowercase variant name (mirrors the serde rename_all = "snake_case").
fn serde_plain<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_never_passes_on_nonzero_exit() {
        assert_eq!(Status::from_exit(Some(0), false), Status::Passed);
        assert_eq!(Status::from_exit(Some(0), true), Status::Failed);
        assert_eq!(Status::from_exit(Some(2), false), Status::Error);
        assert_eq!(Status::from_exit(Some(1), true), Status::Failed);
        assert_eq!(Status::from_exit(None, false), Status::Unknown);
    }

    #[test]
    fn fingerprint_is_stable_across_line_shifts() {
        let a = fingerprint("pytest", "AssertionError", "tests/x.py:42", "assert 0 == 1");
        let b = fingerprint("pytest", "AssertionError", "tests/x.py:99", "assert 0 == 1");
        assert_eq!(a, b, "line number should be normalized out");
        let c = fingerprint("pytest", "AssertionError", "tests/y.py:42", "assert 0 == 1");
        assert_ne!(a, c, "different file should differ");
        assert_eq!(a.len(), 12);
    }

    #[test]
    fn cap_limits_findings_and_records_total() {
        let mut r = ToolResult::generic("x", Some(1));
        for i in 0..50 {
            r.findings.push(Finding {
                kind: FindingKind::Diagnostic,
                severity: Severity::Error,
                id: None,
                rule: None,
                message: format!("m{i}"),
                location: None,
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                fingerprint: "x".into(),
            });
        }
        r.cap();
        assert_eq!(r.findings.len(), MAX_FINDINGS);
        assert_eq!(r.truncated.findings_total, 50);
        assert_eq!(r.truncated.findings_shown, MAX_FINDINGS);
    }

    #[test]
    fn yaml_render_round_trips_through_json() {
        let mut r = ToolResult::generic("pytest", Some(1));
        r.kind = ResultKind::Test;
        r.status = Status::Failed;
        r.counts.insert("passed".into(), 120);
        r.counts.insert("failed".into(), 2);
        r.parser_confidence = ParserConfidence::Parsed;
        r.raw_oid = Some("sha256:abc".into());
        r.findings.push(Finding {
            kind: FindingKind::TestFailure,
            severity: Severity::Failure,
            id: Some("tests/test_auth.py::test_x".into()),
            rule: Some("AssertionError".into()),
            message: "assert 0 == 100".into(),
            location: Some(Location { path: "tests/test_auth.py".into(), line: Some(42), column: None }),
            locations: vec![],
            expected: Some("100".into()),
            actual: Some("0".into()),
            detail: None,
            fixable: false,
            fingerprint: fingerprint("pytest", "AssertionError", "tests/test_auth.py:42", "assert 0 == 100"),
        });
        let yaml = render_yaml(&r);
        assert!(yaml.contains("tool: pytest"));
        assert!(yaml.contains("status: failed"));
        assert!(yaml.contains("counts: { failed: 2, passed: 120 }"));
        assert!(yaml.contains("location: tests/test_auth.py:42"));
        assert!(yaml.contains("fingerprint: "));
        // JSON canonical round-trips.
        let json = render_json(&r);
        let back: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool, "pytest");
        assert_eq!(back.findings.len(), 1);
        assert_eq!(back.status, Status::Failed);
    }

    fn argv(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pytest_parser_extracts_findings_and_status() {
        let raw = "=== FAILURES ===\nFAILED tests/test_auth.py::test_pay - assert 0 == 100\nFAILED tests/test_auth.py::test_ref - AssertionError: nope\n=== 2 failed, 120 passed in 4.12s ===\n";
        let r = parse(&argv(&["pytest", "-q"]), raw, Some(1)).unwrap();
        assert_eq!(r.tool, "pytest");
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.counts.get("passed"), Some(&120));
        assert_eq!(r.counts.get("failed"), Some(&2));
        assert_eq!(r.findings.len(), 2);
        assert_eq!(r.findings[0].id.as_deref(), Some("tests/test_auth.py::test_pay"));
        assert_eq!(r.findings[0].location.as_ref().unwrap().path, "tests/test_auth.py");
        assert_eq!(r.findings[1].rule.as_deref(), Some("AssertionError"));
        assert_eq!(r.parser_confidence, ParserConfidence::Parsed);
    }

    #[test]
    fn pytest_parser_declines_without_summary_line() {
        // No count summary → decline so the caller falls back to generic.
        assert!(parse(&argv(&["pytest"]), "random noise\nno summary here", Some(1)).is_none());
    }

    #[test]
    fn pytest_all_pass_is_passed_status() {
        let raw = "=== 300 passed in 5.0s ===\n";
        let r = parse(&argv(&["pytest"]), raw, Some(0)).unwrap();
        assert_eq!(r.status, Status::Passed);
        assert_eq!(r.counts.get("passed"), Some(&300));
        assert!(r.findings.is_empty());
    }

    #[test]
    fn cargo_test_parser_extracts_panic_with_assertion() {
        let raw = "running 88 tests\ntest mod::auth ... FAILED\n\n---- mod::auth stdout ----\nthread 'mod::auth' panicked at src/auth.rs:55:9:\nassertion `left == right` failed\n  left: 401\n  right: 200\n\ntest result: FAILED. 87 passed; 1 failed; 0 ignored\n";
        let r = parse(&argv(&["cargo", "test"]), raw, Some(101)).unwrap();
        assert_eq!(r.tool, "cargo");
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.counts.get("passed"), Some(&87));
        assert_eq!(r.findings.len(), 1);
        let f = &r.findings[0];
        assert_eq!(f.kind, FindingKind::Panic);
        assert_eq!(f.location.as_ref().unwrap().shorthand(), "src/auth.rs:55:9");
        assert_eq!(f.actual.as_deref(), Some("401"));
        assert_eq!(f.expected.as_deref(), Some("200"));
    }

    #[test]
    fn cargo_unknown_subcommand_declines() {
        assert!(parse(&argv(&["cargo", "metadata"]), "{\"x\":1}", Some(0)).is_none());
    }

    #[test]
    fn yaml_quotes_risky_scalars() {
        // a colon-bearing message must be quoted so YAML stays valid.
        let mut r = ToolResult::generic("x", Some(1));
        r.body = Some("line one\nline two".into());
        let y = render_yaml(&r);
        assert!(y.contains("body: |"));
        assert!(y.contains("  line one"));
    }
}
