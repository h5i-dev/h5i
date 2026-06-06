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
    /// Tests specifically passed (test runners only).
    Passed,
    /// The command succeeded (exit 0, no problems) for non-test tools.
    Ok,
    /// Tests/lints/types failed (the tool ran and reported problems).
    Failed,
    /// Tool/parser/infrastructure error (e.g. usage error, couldn't run).
    Error,
    /// Generic fallback couldn't infer pass/fail.
    Unknown,
}

impl Status {
    /// Safe status for a **non-test** tool: success → `Ok`, never on nonzero exit.
    pub fn from_exit(exit_code: Option<i32>, has_failures: bool) -> Status {
        match exit_code {
            Some(0) if !has_failures => Status::Ok,
            Some(0) => Status::Failed, // findings despite exit 0 → surface them
            Some(_) if has_failures => Status::Failed,
            Some(_) => Status::Error, // nonzero, no parsed findings → tool/usage error
            None => Status::Unknown,
        }
    }

    /// Safe status for a **test runner**: clean success → `Passed`.
    pub fn from_test(exit_code: Option<i32>, failed: bool) -> Status {
        match exit_code {
            Some(0) if !failed => Status::Passed,
            Some(0) => Status::Failed,
            Some(_) if failed => Status::Failed,
            Some(_) => Status::Error,
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
    /// Span end (rustc/tsc/eslint/ruff/SARIF-style ranges). Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
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
    /// Suggested fixes/replacements (eslint/ruff/clippy). Pairs with `fixable`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
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
    /// Total bytes of per-finding `detail` clipped by [`MAX_DETAIL_BYTES`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub detail_bytes_omitted: usize,
    /// Whether the raw output is larger than what's represented here.
    pub raw: bool,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    /// Parser-specific structured data that doesn't fit `findings` — the escape
    /// hatch that lets any tool carry its own shape: install tallies, coverage
    /// %, diff `+/-` stats, benchmark numbers, a VCS file list, etc. Bounded by
    /// the caller; values are arbitrary JSON. Keeps the core schema small while
    /// supporting tools whose output isn't diagnostic-shaped.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            extra: serde_json::Map::new(),
        }
    }

    /// Cap findings to [`MAX_FINDINGS`] and per-finding detail to
    /// [`MAX_DETAIL_BYTES`], recording what was dropped in `truncated`.
    pub fn cap(&mut self) {
        let total = self.findings.len();
        if total > MAX_FINDINGS {
            self.findings.truncate(MAX_FINDINGS);
        }
        const ELLIPSIS: &str = "…"; // 3 bytes
        let mut omitted = 0usize;
        for f in &mut self.findings {
            if let Some(d) = &mut f.detail {
                if d.len() > MAX_DETAIL_BYTES {
                    let before = d.len();
                    // Reserve room for the marker so the final string is <= cap.
                    let mut end = MAX_DETAIL_BYTES - ELLIPSIS.len();
                    while end > 0 && !d.is_char_boundary(end) {
                        end -= 1;
                    }
                    d.truncate(end);
                    omitted += before - end;
                    d.push_str(ELLIPSIS);
                }
            }
        }
        self.truncated.findings_total = total;
        self.truncated.findings_shown = self.findings.len();
        self.truncated.detail_bytes_omitted = omitted;
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

/// Canonical JSON — **compact**. This is what's stored in the manifest and
/// returned over MCP, so it must be token/byte-frugal.
pub fn render_json(r: &ToolResult) -> String {
    serde_json::to_string(r).unwrap_or_default()
}

/// Pretty JSON for human CLI/debug display only (not the canonical form).
pub fn render_json_pretty(r: &ToolResult) -> String {
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
    if !r.extra.is_empty() {
        out.push_str("extra:\n");
        for (k, v) in &r.extra {
            // Compact JSON value on one line; keeps tool-specific data readable.
            out.push_str(&format!("  {k}: {}\n", serde_json::to_string(v).unwrap_or_default()));
        }
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
        .map(|w| {
            w.rsplit('/')
                .next()
                .unwrap_or(w)
                .trim_matches('"')
                .to_ascii_lowercase()
        })
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
    if let Some(i) = words.iter().position(|w| w == "go") {
        if words.get(i + 1).map(String::as_str) == Some("test") {
            return parse_go_test(output, exit_code);
        }
    }
    if has("tsc") {
        return parse_tsc(output, exit_code);
    }
    if has("eslint") {
        return parse_eslint(output, exit_code);
    }
    if has("ruff") {
        return parse_ruff(output, exit_code);
    }
    if has("mypy") {
        return parse_mypy(output, exit_code);
    }
    None
}

/// Build a diagnostic-style result (compilers/linters/type checkers): status
/// from exit + presence of error-severity findings (never passes on nonzero).
fn diag_result(
    tool: &str,
    kind: ResultKind,
    exit_code: Option<i32>,
    findings: Vec<Finding>,
) -> ToolResult {
    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count() as u64;
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count() as u64;
    let mut counts = std::collections::BTreeMap::new();
    if errors > 0 {
        counts.insert("error".to_string(), errors);
    }
    if warnings > 0 {
        counts.insert("warning".to_string(), warnings);
    }
    let mut r = ToolResult {
        schema_version: SCHEMA_VERSION,
        tool: tool.to_string(),
        kind,
        status: Status::from_exit(exit_code, errors > 0),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
        extra: serde_json::Map::new(),
    };
    r.cap();
    r
}

fn parse_go_test(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let lines: Vec<&str> = output.lines().collect();
    // Anchor: go test markers.
    if !lines.iter().any(|l| {
        let t = l.trim_start();
        t.starts_with("--- FAIL")
            || t.starts_with("--- PASS")
            || t.starts_with("ok ")
            || t.starts_with("ok\t")
            || t.starts_with("FAIL")
    }) {
        return None;
    }
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut findings = Vec::new();
    let mut saw_test_event = false;
    let loc_re = regex::Regex::new(r"^\s+([\w./-]+\.go):(\d+):").unwrap();
    let fail_re = regex::Regex::new(r"^--- FAIL: (\S+)").unwrap();
    // Non-indented "file.go:line:col?: message" → a build/compile diagnostic.
    let build_re = regex::Regex::new(r"^([\w./-]+\.go):(\d+):(?:(\d+):)?\s+(.+)$").unwrap();
    let mut i = 0;
    while i < lines.len() {
        let raw_line = lines[i];
        let t = raw_line.trim_start();
        // A `go build`/compile diagnostic (not indented under a test).
        if !raw_line.starts_with(char::is_whitespace) {
            if let Some(c) = build_re.captures(raw_line.trim_end()) {
                let loc = Location {
                    path: c[1].trim_start_matches("./").to_string(),
                    line: c[2].parse().ok(),
                    column: c.get(3).and_then(|m| m.as_str().parse().ok()),
                    ..Default::default()
                };
                let msg = c[4].to_string();
                findings.push(Finding {
                    kind: FindingKind::BuildError,
                    severity: Severity::Error,
                    id: None,
                    rule: None,
                    message: msg.clone(),
                    location: Some(loc.clone()),
                    locations: vec![],
                    expected: None,
                    actual: None,
                    detail: None,
                    fixable: false,
                    suggestions: vec![],
                    fingerprint: fingerprint("go", "build", &loc.shorthand(), &msg),
                });
                i += 1;
                continue;
            }
        }
        if t.starts_with("--- PASS") {
            passed += 1;
            saw_test_event = true;
        } else if t.starts_with("ok ") || t.starts_with("ok\t") {
            saw_test_event = true;
        } else if let Some(c) = fail_re.captures(t) {
            saw_test_event = true;
            failed += 1;
            let test = c[1].to_string();
            // The next indented "file.go:line:" line is the location/message.
            let (loc, msg) = lines.get(i + 1).and_then(|n| {
                loc_re.captures(n.trim_end()).map(|lc| {
                    let l = Location {
                        path: lc[1].to_string(),
                        line: lc[2].parse().ok(),
                        ..Default::default()
                    };
                    (Some(l), n.trim().to_string())
                })
            }).unwrap_or((None, test.clone()));
            findings.push(Finding {
                kind: FindingKind::TestFailure,
                severity: Severity::Failure,
                id: Some(test.clone()),
                rule: None,
                message: msg,
                location: loc.clone(),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                suggestions: vec![],
                fingerprint: fingerprint(
                    "go",
                    "",
                    &loc.map(|l| l.shorthand()).unwrap_or_else(|| test.clone()),
                    &test,
                ),
            });
        }
        i += 1;
    }
    // Only package-level FAIL with nothing useful extracted → decline so the
    // generic fallback shows the raw (don't emit a parsed-but-empty result).
    if !saw_test_event && findings.is_empty() {
        return None;
    }
    let has_build_error = findings.iter().any(|f| f.kind == FindingKind::BuildError);
    let mut counts = std::collections::BTreeMap::new();
    if passed > 0 {
        counts.insert("passed".to_string(), passed);
    }
    if failed > 0 {
        counts.insert("failed".to_string(), failed);
    }
    let mut r = ToolResult {
        schema_version: SCHEMA_VERSION,
        tool: "go".into(),
        kind: ResultKind::Test,
        status: Status::from_test(exit_code, failed > 0 || has_build_error),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
        extra: serde_json::Map::new(),
    };
    r.cap();
    Some(r)
}

fn parse_tsc(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let re = regex::Regex::new(r"^(.+?)\((\d+),(\d+)\): (error|warning) (TS\d+): (.+)$").unwrap();
    let mut findings = Vec::new();
    for l in output.lines() {
        if let Some(c) = re.captures(l.trim()) {
            let severity = if &c[4] == "warning" { Severity::Warning } else { Severity::Error };
            let loc = Location {
                path: c[1].to_string(),
                line: c[2].parse().ok(),
                column: c[3].parse().ok(),
                ..Default::default()
            };
            findings.push(Finding {
                kind: FindingKind::Diagnostic,
                severity,
                id: None,
                rule: Some(c[5].to_string()),
                message: c[6].to_string(),
                location: Some(loc.clone()),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                suggestions: vec![],
                fingerprint: fingerprint("tsc", &c[5], &loc.shorthand(), &c[6]),
            });
        }
    }
    let found = output.lines().any(|l| l.trim_start().starts_with("Found ") && l.contains("error"));
    if findings.is_empty() && !found {
        return None;
    }
    Some(diag_result("tsc", ResultKind::Typecheck, exit_code, findings))
}

fn parse_ruff(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    // "path.py:line:col: CODE message"  (CODE like F401, E711)
    let re = regex::Regex::new(r"^(.+?):(\d+):(\d+): ([A-Z]+\d+) (.+)$").unwrap();
    let mut findings = Vec::new();
    for l in output.lines() {
        if let Some(c) = re.captures(l.trim()) {
            let loc = Location {
                path: c[1].to_string(),
                line: c[2].parse().ok(),
                column: c[3].parse().ok(),
                ..Default::default()
            };
            let msg = c[5].to_string();
            let fixable = msg.contains("[*]");
            findings.push(Finding {
                kind: FindingKind::Diagnostic,
                severity: Severity::Error,
                id: None,
                rule: Some(c[4].to_string()),
                message: msg.clone(),
                location: Some(loc.clone()),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable,
                suggestions: vec![],
                fingerprint: fingerprint("ruff", &c[4], &loc.shorthand(), &msg),
            });
        }
    }
    let clean = output.lines().any(|l| l.trim() == "All checks passed!");
    if findings.is_empty() && !clean {
        return None;
    }
    Some(diag_result("ruff", ResultKind::Lint, exit_code, findings))
}

fn parse_mypy(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    // "path.py:line: error: message [code]"  (column optional)
    let re = regex::Regex::new(r"^(.+?):(\d+):(?:(\d+):)? (error|note|warning): (.+)$").unwrap();
    let mut findings = Vec::new();
    for l in output.lines() {
        if let Some(c) = re.captures(l.trim()) {
            let kind_word = &c[4];
            if kind_word == "note" {
                continue; // notes are context, not findings
            }
            let severity = if kind_word == "warning" { Severity::Warning } else { Severity::Error };
            let mut msg = c[5].to_string();
            // Pull a trailing "[code]" into rule.
            let rule = msg
                .rfind('[')
                .filter(|_| msg.ends_with(']'))
                .map(|b| {
                    let code = msg[b + 1..msg.len() - 1].to_string();
                    msg = msg[..b].trim_end().to_string();
                    code
                });
            let loc = Location {
                path: c[1].to_string(),
                line: c[2].parse().ok(),
                column: c.get(3).and_then(|m| m.as_str().parse().ok()),
                ..Default::default()
            };
            findings.push(Finding {
                kind: FindingKind::Diagnostic,
                severity,
                id: None,
                rule: rule.clone(),
                message: msg.clone(),
                location: Some(loc.clone()),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                suggestions: vec![],
                fingerprint: fingerprint("mypy", rule.as_deref().unwrap_or(""), &loc.shorthand(), &msg),
            });
        }
    }
    let resolved = output.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("Success:") || (t.starts_with("Found ") && t.contains("error"))
    });
    if findings.is_empty() && !resolved {
        return None;
    }
    Some(diag_result("mypy", ResultKind::Typecheck, exit_code, findings))
}

fn parse_eslint(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    // Stylish format: a file header line, then "  L:C  severity  message  rule".
    let row = regex::Regex::new(r"^(\d+):(\d+)\s+(error|warning)\s+(.+?)\s{2,}(\S+)$").unwrap();
    let mut findings = Vec::new();
    let mut current_file: Option<String> = None;
    for l in output.lines() {
        let t = l.trim_end();
        let tl = t.trim_start();
        if let Some(c) = row.captures(tl) {
            let severity = if &c[3] == "warning" { Severity::Warning } else { Severity::Error };
            let loc = current_file.clone().map(|p| Location {
                path: p,
                line: c[1].parse().ok(),
                column: c[2].parse().ok(),
                ..Default::default()
            });
            let msg = c[4].trim().to_string();
            let rule = c[5].to_string();
            findings.push(Finding {
                kind: FindingKind::Diagnostic,
                severity,
                id: None,
                rule: Some(rule.clone()),
                message: msg.clone(),
                location: loc.clone(),
                locations: vec![],
                expected: None,
                actual: None,
                detail: None,
                fixable: false,
                suggestions: vec![],
                fingerprint: fingerprint(
                    "eslint",
                    &rule,
                    &loc.map(|l| l.shorthand()).unwrap_or_default(),
                    &msg,
                ),
            });
        } else if !tl.is_empty() && !tl.starts_with('✖') && !tl.starts_with('✔') && row.captures(tl).is_none() {
            // A non-row, non-summary line is a file header (path).
            if tl.contains('/') || tl.contains('.') {
                current_file = Some(tl.to_string());
            }
        }
    }
    let summary = output.lines().any(|l| l.contains("problem") || l.trim_start().starts_with('✖'));
    if findings.is_empty() && !summary {
        return None;
    }
    Some(diag_result("eslint", ResultKind::Lint, exit_code, findings))
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
        Some(Location { path: path.to_string(), ..Default::default() })
    } else {
        None
    }
}

fn parse_pytest(output: &str, exit_code: Option<i32>) -> Option<ToolResult> {
    let lines: Vec<&str> = output.lines().collect();
    // Anchor: the count summary line, or an explicit "no tests ran". Absent → decline.
    let summary = lines.iter().rev().find(|l| {
        let t = l.trim().trim_matches('=').trim();
        t.contains("no tests ran")
            || (t.contains(" in ")
                && (t.contains("passed")
                    || t.contains("failed")
                    || t.contains("error")
                    || t.contains("skipped")))
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
                suggestions: vec![],
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
        status: Status::from_test(exit_code, failed > 0),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
        extra: serde_json::Map::new(),
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
            let loc = Location { path: path.clone(), line: Some(line), column: Some(col), ..Default::default() };
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
                suggestions: vec![],
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
        status: Status::from_test(exit_code, failed > 0 || !findings.is_empty()),
        exit_code,
        duration_ms: None,
        counts,
        parser_confidence: ParserConfidence::Parsed,
        raw_oid: None,
        findings,
        suppressed: Vec::new(),
        truncated: Truncated::default(),
        body: None,
        extra: serde_json::Map::new(),
    };
    r.cap();
    Some(r)
}

/// Collapse a message to a single line and cap its length.
fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let t: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    } else {
        flat
    }
}

/// One-line-per-finding render — token-minimal text (rtk-style) that keeps the
/// structured signal (status, counts, each finding's severity/rule/location/
/// message) WITHOUT the per-field YAML overhead. This is the default for
/// `capture run`: on diagnostic-dense output it is ~3× smaller than the full
/// YAML while staying scannable and parseable.
pub fn render_compact(r: &ToolResult) -> String {
    let mut out = String::new();
    // Header: "<tool> <kind> <status> · <counts> (exit N)"
    out.push_str(&format!("{} {} {}", r.tool, serde_plain(&r.kind), serde_plain(&r.status)));
    if !r.counts.is_empty() {
        let parts: Vec<String> = r.counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
        out.push_str(&format!(" · {}", parts.join(", ")));
    }
    if let Some(c) = r.exit_code {
        if c != 0 {
            out.push_str(&format!(" (exit {c})"));
        }
    }
    out.push('\n');

    // One line per finding: "  <sev> <location|id>  <rule> <message>[ (fix)]"
    for f in &r.findings {
        let sev = match f.severity {
            Severity::Error => "E",
            Severity::Warning => "W",
            Severity::Failure => "F",
        };
        // Primary locator: the id (test nodeid/target) when present, else the
        // location. Append `file:line` when an id is paired with a precise
        // location (so test failures show both the test name and where).
        let loc_short = f.location.as_ref().map(Location::shorthand).filter(|s| !s.is_empty());
        let primary = f.id.clone().or_else(|| loc_short.clone()).unwrap_or_default();
        let loc_suffix = match (&f.id, &f.location) {
            (Some(_), Some(l)) if l.line.is_some() => format!(" ({})", l.shorthand()),
            _ => String::new(),
        };
        let rule = f.rule.as_deref().map(|r| format!("{r} ")).unwrap_or_default();
        let fix = if f.fixable { " (fixable)" } else { "" };
        let msg = one_line(&f.message, 160);
        if primary.is_empty() {
            out.push_str(&format!("  {sev} {rule}{msg}{fix}\n"));
        } else {
            out.push_str(&format!("  {sev} {primary}{loc_suffix}  {rule}{msg}{fix}\n"));
        }
    }
    if r.truncated.findings_total > r.truncated.findings_shown {
        out.push_str(&format!(
            "  … +{} more findings ({} total)\n",
            r.truncated.findings_total - r.truncated.findings_shown,
            r.truncated.findings_total
        ));
    }

    // Generic results (no parser) carry the reduced text in `body`.
    if let Some(b) = &r.body {
        if !b.trim().is_empty() {
            out.push('\n');
            out.push_str(b.trim_end());
            out.push('\n');
        }
    }
    out.trim_end().to_string()
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
        // Non-test tools → Ok on success, never on nonzero exit.
        assert_eq!(Status::from_exit(Some(0), false), Status::Ok);
        assert_eq!(Status::from_exit(Some(0), true), Status::Failed);
        assert_eq!(Status::from_exit(Some(2), false), Status::Error);
        assert_eq!(Status::from_exit(Some(1), true), Status::Failed);
        assert_eq!(Status::from_exit(None, false), Status::Unknown);
        // Test runners → Passed on clean success, never on nonzero exit.
        assert_eq!(Status::from_test(Some(0), false), Status::Passed);
        assert_eq!(Status::from_test(Some(2), false), Status::Error);
        assert_eq!(Status::from_test(Some(1), true), Status::Failed);
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
                suggestions: vec![],
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
            location: Some(Location { path: "tests/test_auth.py".into(), line: Some(42), ..Default::default() }),
            locations: vec![],
            expected: Some("100".into()),
            actual: Some("0".into()),
            detail: None,
            fixable: false,
            suggestions: vec![],
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
    fn go_test_parser_extracts_failures() {
        let raw = "=== RUN   TestAdd\n--- PASS: TestAdd (0.00s)\nok  \tex/m\t0.01s\n=== RUN   TestSub\n--- FAIL: TestSub (0.00s)\n    sub_test.go:10: got 1, want 2\nFAIL\nFAIL\tex/m2\t0.00s\n";
        let r = parse(&argv(&["go", "test", "./..."]), raw, Some(1)).unwrap();
        assert_eq!(r.tool, "go");
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.counts.get("failed"), Some(&1));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].id.as_deref(), Some("TestSub"));
        assert_eq!(r.findings[0].location.as_ref().unwrap().shorthand(), "sub_test.go:10");
    }

    #[test]
    fn go_build_failure_surfaces_compiler_diagnostic() {
        // A compile failure has no "--- FAIL" test case; the diagnostic must
        // still reach the agent (as a build_error finding), not vanish.
        let raw = "# example/pkg\n./main.go:6:2: undefined: missing\nFAIL\texample/pkg [build failed]\n";
        let r = parse(&argv(&["go", "test", "./..."]), raw, Some(2)).unwrap();
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].kind, FindingKind::BuildError);
        assert_eq!(r.findings[0].location.as_ref().unwrap().shorthand(), "main.go:6:2");
        assert!(r.findings[0].message.contains("undefined: missing"));
    }

    #[test]
    fn go_bare_package_fail_declines_to_generic() {
        // Package-level FAIL with nothing extractable → decline (generic shows raw).
        assert!(parse(&argv(&["go", "test"]), "FAIL\texample/pkg [build failed]\n", Some(2)).is_none());
    }

    #[test]
    fn tsc_parser_extracts_diagnostics() {
        let raw = "src/a.ts(12,5): error TS2322: Type 'string' is not assignable.\nsrc/b.tsx(8,3): warning TS6133: 'x' is declared but never used.\nFound 1 error.\n";
        let r = parse(&argv(&["tsc", "--noEmit"]), raw, Some(2)).unwrap();
        assert_eq!(r.tool, "tsc");
        assert_eq!(r.kind, ResultKind::Typecheck);
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.findings.len(), 2);
        assert_eq!(r.findings[0].rule.as_deref(), Some("TS2322"));
        assert_eq!(r.findings[0].location.as_ref().unwrap().shorthand(), "src/a.ts:12:5");
        assert_eq!(r.findings[1].severity, Severity::Warning);
    }

    #[test]
    fn ruff_parser_extracts_diagnostics_and_fixable() {
        let raw = "app.py:1:1: F401 [*] `os` imported but unused\napp.py:5:5: E711 comparison to `None`\nFound 2 errors.\n";
        let r = parse(&argv(&["ruff", "check", "."]), raw, Some(1)).unwrap();
        assert_eq!(r.tool, "ruff");
        assert_eq!(r.kind, ResultKind::Lint);
        assert_eq!(r.findings.len(), 2);
        assert_eq!(r.findings[0].rule.as_deref(), Some("F401"));
        assert!(r.findings[0].fixable);
        assert_eq!(r.findings[1].rule.as_deref(), Some("E711"));
    }

    #[test]
    fn ruff_clean_is_ok() {
        let r = parse(&argv(&["ruff", "check", "."]), "All checks passed!\n", Some(0)).unwrap();
        assert_eq!(r.status, Status::Ok);
        assert!(r.findings.is_empty());
    }

    #[test]
    fn mypy_parser_extracts_errors_with_codes() {
        let raw = "src/a.py:12: error: Incompatible return value type [return-value]\nsrc/a.py:12: note: ignore this\nsrc/b.py:3:5: error: Name 'foo' is not defined [name-defined]\nFound 2 errors in 2 files\n";
        let r = parse(&argv(&["mypy", "src"]), raw, Some(1)).unwrap();
        assert_eq!(r.tool, "mypy");
        assert_eq!(r.findings.len(), 2, "notes are not findings");
        assert_eq!(r.findings[0].rule.as_deref(), Some("return-value"));
        assert_eq!(r.findings[1].location.as_ref().unwrap().shorthand(), "src/b.py:3:5");
        assert_eq!(r.findings[1].rule.as_deref(), Some("name-defined"));
    }

    #[test]
    fn eslint_parser_extracts_problems_with_file() {
        let raw = "/app/src/index.ts\n  1:7   error    'x' is assigned a value but never used  no-unused-vars\n  3:1   warning  Unexpected console statement              no-console\n\n✖ 2 problems (1 error, 1 warning)\n";
        let r = parse(&argv(&["eslint", "."]), raw, Some(1)).unwrap();
        assert_eq!(r.tool, "eslint");
        assert_eq!(r.findings.len(), 2);
        assert_eq!(r.findings[0].rule.as_deref(), Some("no-unused-vars"));
        assert_eq!(r.findings[0].location.as_ref().unwrap().path, "/app/src/index.ts");
        assert_eq!(r.findings[1].severity, Severity::Warning);
    }

    #[test]
    fn diagnostic_parsers_decline_on_garbage() {
        // No anchors → decline so the caller uses the generic fallback.
        assert!(parse(&argv(&["tsc"]), "hello world", Some(0)).is_none());
        assert!(parse(&argv(&["ruff", "check"]), "hello world", Some(0)).is_none());
        assert!(parse(&argv(&["mypy"]), "hello world", Some(0)).is_none());
        assert!(parse(&argv(&["eslint"]), "hello world", Some(0)).is_none());
        assert!(parse(&argv(&["go", "test"]), "hello world", Some(0)).is_none());
    }

    #[test]
    fn detail_cap_respects_byte_limit_and_records_omission() {
        let mut r = ToolResult::generic("x", Some(1));
        r.findings.push(Finding {
            kind: FindingKind::Diagnostic,
            severity: Severity::Error,
            id: None,
            rule: None,
            message: "m".into(),
            location: None,
            locations: vec![],
            expected: None,
            actual: None,
            detail: Some("d".repeat(MAX_DETAIL_BYTES * 2)),
            fixable: false,
            suggestions: vec![],
            fingerprint: "x".into(),
        });
        r.cap();
        let d = r.findings[0].detail.as_ref().unwrap();
        assert!(d.len() <= MAX_DETAIL_BYTES, "detail {} > cap {}", d.len(), MAX_DETAIL_BYTES);
        assert!(r.truncated.detail_bytes_omitted > 0, "should record omitted bytes");
    }

    #[test]
    fn pytest_no_tests_ran_is_handled_not_declined() {
        let r = parse(&argv(&["pytest"]), "no tests ran in 0.01s\n", Some(0)).unwrap();
        assert_eq!(r.tool, "pytest");
        assert!(r.findings.is_empty());
        // exit 0, no failures → Passed (test runner).
        assert_eq!(r.status, Status::Passed);
    }

    #[test]
    fn generic_success_is_ok_not_passed() {
        let r = ToolResult::generic("make", Some(0));
        assert_eq!(r.status, Status::Ok);
    }

    #[test]
    fn cargo_unknown_subcommand_declines() {
        assert!(parse(&argv(&["cargo", "metadata"]), "{\"x\":1}", Some(0)).is_none());
    }

    #[test]
    fn compact_render_is_one_line_per_finding_and_smaller() {
        let raw = "app/models.py:1:1: F401 [*] `os` imported but unused\napp/views.py:42:5: E711 comparison to `None`\nFound 2 errors.\n";
        let r = parse(&argv(&["ruff", "check", "."]), raw, Some(1)).unwrap();
        let compact = render_compact(&r);
        let yaml = render_yaml(&r);
        // Header + 2 finding lines (no blank-line padding, no per-field keys).
        let lines: Vec<&str> = compact.lines().collect();
        assert!(lines[0].starts_with("ruff lint failed"), "header: {}", lines[0]);
        assert_eq!(lines.iter().filter(|l| l.starts_with("  ")).count(), 2);
        assert!(compact.contains("F401") && compact.contains("app/models.py:1:1"));
        assert!(compact.contains("(fixable)")); // F401 had [*]
        // Compact is materially smaller than the full YAML for diagnostics.
        assert!(compact.len() < yaml.len() / 2, "compact {} vs yaml {}", compact.len(), yaml.len());
    }

    #[test]
    fn compact_render_shows_truncation_and_generic_body() {
        let mut r = ToolResult::generic("make", Some(0));
        r.body = Some("build line 1\nbuild line 2".into());
        let c = render_compact(&r);
        assert!(c.starts_with("make generic ok"));
        assert!(c.contains("build line 1"));
    }

    #[test]
    fn extra_carries_non_diagnostic_tool_data() {
        // The escape hatch: data that isn't a finding (coverage, diff stats, …).
        let mut r = ToolResult::generic("git", Some(0));
        r.kind = ResultKind::Vcs;
        r.extra.insert("files_changed".into(), serde_json::json!(3));
        r.extra.insert("insertions".into(), serde_json::json!(42));
        r.extra.insert("deletions".into(), serde_json::json!(7));
        let y = render_yaml(&r);
        assert!(y.contains("extra:"));
        assert!(y.contains("files_changed: 3"));
        // round-trips through canonical JSON
        let back: ToolResult = serde_json::from_str(&render_json(&r)).unwrap();
        assert_eq!(back.extra.get("insertions"), Some(&serde_json::json!(42)));
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
