//! Prompt-injection detection for `h5i context scan` and `h5i compliance`.
//!
//! Scans arbitrary text (context trace, session thinking blocks, etc.) against
//! a set of regex rules that flag common prompt-injection patterns.  Each rule
//! produces zero or more [`Hit`]s; the overall [`ScanResult`] aggregates hits
//! into a 0.0–1.0 risk score.

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

// ── Severity ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    /// Weight used when computing the aggregate risk score.
    pub fn weight(self) -> f64 {
        match self {
            Severity::High => 0.5,
            Severity::Medium => 0.25,
            Severity::Low => 0.1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::High => "HIGH",
            Severity::Medium => "MEDIUM",
            Severity::Low => "LOW",
        }
    }
}

// ── Rule ──────────────────────────────────────────────────────────────────────

pub struct Rule {
    pub name: &'static str,
    pub description: &'static str,
    pub severity: Severity,
    pub pattern: Regex,
}

// ── Hit / ScanResult ─────────────────────────────────────────────────────────

/// A single pattern match within the scanned text.
#[derive(Debug, Serialize)]
pub struct Hit {
    /// Short rule identifier (e.g. `"override_instructions"`).
    pub rule: &'static str,
    pub severity: Severity,
    /// 1-indexed line number in the scanned text.
    pub line_no: usize,
    /// The substring that matched the pattern.
    pub matched: String,
    /// The full line containing the match (truncated to 200 chars).
    pub line: String,
}

/// Aggregate result of scanning a block of text.
#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub hits: Vec<Hit>,
    /// Risk score in [0.0, 1.0].  0 = no signals; 1 = saturated.
    pub risk_score: f64,
    pub lines_scanned: usize,
}

impl ScanResult {
    pub fn is_clean(&self) -> bool {
        self.hits.is_empty()
    }
}

// ── Built-in rules ────────────────────────────────────────────────────────────

static RULES: OnceLock<Vec<Rule>> = OnceLock::new();

fn rules() -> &'static Vec<Rule> {
    RULES.get_or_init(|| {
        vec![
            Rule {
                name: "override_instructions",
                description: "Attempts to override, ignore, or forget prior instructions/context",
                severity: Severity::High,
                pattern: Regex::new(
                    r"(?i)(ignore|disregard|forget)\s+(all\s+)?(previous|above|prior|earlier)?\s*(instructions?|rules?|context|constraints?|guidelines?)",
                ).unwrap(),
            },
            Rule {
                name: "role_hijack",
                description: "Tries to redefine the model's role or identity",
                severity: Severity::High,
                pattern: Regex::new(
                    r"(?i)(you\s+are|act\s+as|pretend\s+to\s+be|role\s*:\s*)(now\s+)?(system|developer|assistant|admin|dan|root|god\s*mode|jailbreak)",
                ).unwrap(),
            },
            Rule {
                name: "exfiltration_attempt",
                description: "Requests disclosure of system prompt, secrets, or credentials",
                severity: Severity::High,
                pattern: Regex::new(
                    r"(?i)(show|reveal|print|dump|expose|output|display|repeat|echo)\s*.{0,30}(system\s*prompt|hidden\s*instructions?|secret|api[\s_-]?key|credentials?|password|token)",
                ).unwrap(),
            },
            Rule {
                name: "bypass_safety",
                description: "Attempts to disable safety measures, policies, or guardrails",
                severity: Severity::High,
                pattern: Regex::new(
                    r"(?i)(override|bypass|disable|ignore|circumvent|remove|turn\s+off)\s*.{0,20}(polic(y|ies)|safety|restriction|guardrail|filter|limit|moderation)",
                ).unwrap(),
            },
            Rule {
                name: "indirect_injection_marker",
                description: "Common structural markers used to embed injections in data/tool outputs",
                severity: Severity::Medium,
                pattern: Regex::new(
                    r"(?i)(--\s*system\s*--|<\s*system\s*>|\[system\]|\[\[instructions?\]\]|###\s*new\s*instructions?|begin\s+new\s+prompt|end\s+of\s+user\s+input)",
                ).unwrap(),
            },
            Rule {
                name: "hidden_command",
                description: "Text designed to be invisible to humans but processed by the model",
                severity: Severity::Medium,
                pattern: Regex::new(
                    r"(?i)(this\s+(text|message|content)\s+is\s+(invisible|hidden|not\s+shown)|white\s+text\s+on\s+white|font.{0,10}color.{0,10}white|opacity\s*:\s*0)",
                ).unwrap(),
            },
            Rule {
                name: "prompt_delimiter_escape",
                description: "Attempts to break out of the current prompt context using delimiters",
                severity: Severity::Medium,
                pattern: Regex::new(
                    r#"(?i)(human\s*:|assistant\s*:|<\|im_start\|>|<\|im_end\|>|\[/INST\]|\[INST\]|<<SYS>>|<</SYS>>)"#,
                ).unwrap(),
            },
            Rule {
                name: "credential_request",
                description: "Requests that the model send or use credentials/keys",
                severity: Severity::Low,
                pattern: Regex::new(
                    r"(?i)(send|transmit|post|upload|curl|fetch|wget).{0,40}(api[\s_-]?key|secret|token|bearer|authorization|auth[\s_-]?header)",
                ).unwrap(),
            },
        ]
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Scan `text` against all built-in injection rules.
///
/// Risk score formula: `min(1.0, Σ hit.severity.weight())`.
/// Duplicate matches on the same line by the same rule are collapsed.
pub fn scan(text: &str) -> ScanResult {
    let rules = rules();
    let mut hits: Vec<Hit> = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let line_no = line_idx + 1;
        for rule in rules {
            if let Some(m) = rule.pattern.find(line) {
                // Collapse duplicate rule+line hits.
                let already = hits.iter().any(|h| h.rule == rule.name && h.line_no == line_no);
                if !already {
                    hits.push(Hit {
                        rule: rule.name,
                        severity: rule.severity,
                        line_no,
                        matched: m.as_str().to_string(),
                        line: line.chars().take(200).collect(),
                    });
                }
            }
        }
    }

    let raw_score: f64 = hits.iter().map(|h| h.severity.weight()).sum();
    let risk_score = raw_score.min(1.0);
    let lines_scanned = text.lines().count();

    ScanResult { hits, risk_score, lines_scanned }
}

/// Convenience: scan multiple text fragments and merge results.
pub fn scan_many(texts: &[&str]) -> ScanResult {
    let mut merged_hits: Vec<Hit> = Vec::new();
    let mut total_lines = 0usize;

    for &text in texts {
        let mut r = scan(text);
        // Offset line numbers by total so far to avoid collision in dedup.
        for h in &mut r.hits {
            h.line_no += total_lines;
        }
        merged_hits.extend(r.hits);
        total_lines += r.lines_scanned;
    }

    let raw_score: f64 = merged_hits.iter().map(|h| h.severity.weight()).sum();
    ScanResult {
        risk_score: raw_score.min(1.0),
        hits: merged_hits,
        lines_scanned: total_lines,
    }
}

/// Return the description of a rule by name.
pub fn rule_description(name: &str) -> &'static str {
    rules()
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.description)
        .unwrap_or("unknown rule")
}

// ── Text formatter ────────────────────────────────────────────────────────────

pub fn print_scan_result(result: &ScanResult, source_label: &str) {
    use console::style;

    let bar_len = (result.risk_score * 10.0).round() as usize;
    let bar = format!(
        "{}{}",
        "█".repeat(bar_len),
        "░".repeat(10usize.saturating_sub(bar_len))
    );

    let score_styled = if result.risk_score >= 0.5 {
        style(format!("{:.2}", result.risk_score)).red().bold()
    } else if result.risk_score >= 0.2 {
        style(format!("{:.2}", result.risk_score)).yellow().bold()
    } else {
        style(format!("{:.2}", result.risk_score)).green().bold()
    };

    println!(
        "\n{} {}",
        style("── h5i context scan ──────────────────────────────").dim(),
        style(source_label).cyan()
    );
    println!(
        "  risk score  {}  {}  ({} lines scanned, {} hit(s))",
        score_styled,
        style(bar).dim(),
        result.lines_scanned,
        result.hits.len()
    );

    if result.hits.is_empty() {
        println!("  {}", style("No injection signals detected.").green());
    } else {
        println!();
        for hit in &result.hits {
            let sev = match hit.severity {
                Severity::High => style(hit.severity.label()).red().bold(),
                Severity::Medium => style(hit.severity.label()).yellow().bold(),
                Severity::Low => style(hit.severity.label()).dim(),
            };
            println!(
                "  {} line {:>4}  [{}]  {}",
                sev,
                style(hit.line_no).dim(),
                style(hit.rule).magenta(),
                style(&hit.matched).italic()
            );
            println!(
                "           {}",
                style(hit.line.trim()).dim()
            );
        }
    }
    println!();
}
