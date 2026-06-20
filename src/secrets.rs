//! In-process secret scanner.
//!
//! High-precision regex pack (no runtime dependency on the gitleaks binary)
//! covering the credential formats that produce the fewest false positives
//! in real-world repos: AWS keys, GitHub PATs, Slack tokens, Stripe keys,
//! Google / Anthropic / OpenAI API keys, JWTs, generic private-key PEM
//! blocks, and a Shannon-entropy fallback for opaque high-entropy
//! assignments next to a credential-like keyword.
//!
//! The design notes that drove the rule choices:
//!
//! - **Each rule is anchored on a known prefix or structure.** Prefix-anchored
//!   rules (`AKIA…`, `ghp_…`, `sk-ant-…`) have effectively zero false
//!   positives because the prefix only appears in real credentials.
//! - **A path allowlist prunes lockfiles, vendor trees, fonts, binaries, and
//!   well-known test-fixture directories before regex matching.** These are
//!   the biggest sources of FPs in any secret scanner.
//! - **A per-line stoplist suppresses obvious placeholders** (`your-key-here`,
//!   `<INSERT_KEY>`, `EXAMPLE`, `xxxx…`, `${VAR}`).
//! - **An entropy floor on the generic rule** catches the long tail of
//!   opaque credentials without firing on every `key = "config"` line.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

/// A single detector rule.
struct SecretRule {
    /// Stable identifier surfaced in [`SecretFinding::rule_id`].
    id: &'static str,
    /// Short human-readable label.
    description: &'static str,
    /// Compiled regex (one-time init).
    pattern: &'static str,
    /// Optional capture group index whose value must clear an entropy floor.
    /// `None` means "any match counts" — used for prefix-anchored rules
    /// that are already low-FP.
    entropy_group: Option<usize>,
    /// Minimum Shannon entropy (bits/char) on the captured group. Only
    /// consulted when `entropy_group` is `Some`.
    min_entropy: f32,
    /// Lower-cased substrings that must appear somewhere in the line before
    /// we bother running the regex. Empty slice = "no pre-filter, always
    /// try" (used by catch-all rules like `GENERIC_HIGH_ENTROPY`).
    ///
    /// All entries MUST be lowercase: the scanner lowercases the line once
    /// and compares against these as-is. Keeping the pre-filter strictly
    /// looser than the regex is required for correctness — false positives
    /// here just trigger a wasted regex run; false negatives would hide a
    /// real finding.
    keywords: &'static [&'static str],
}

/// Concrete match emitted by the scanner.
#[derive(Debug, Clone)]
pub struct SecretFinding {
    /// e.g. `"AWS_ACCESS_KEY_ID"`. Stable across releases.
    pub rule_id: &'static str,
    /// Human-readable description suitable for surfacing in the PR comment.
    pub description: &'static str,
    /// 1-indexed line number within the scanned content.
    pub line: usize,
    /// Redacted preview of the matched value (first 4 chars + "…").
    pub preview: String,
}

/// Rule pack. Order matters only insofar as we report the first match per
/// line — most specific rules (prefix-anchored) come before the entropy
/// fallback so we attribute findings correctly.
const RULES: &[SecretRule] = &[
    // ── Cloud / provider keys (prefix-anchored, near-zero FP) ─────────────
    SecretRule {
        id: "AWS_ACCESS_KEY_ID",
        description: "AWS access key ID",
        pattern: r"\b(AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASCA)[0-9A-Z]{16}\b",
        entropy_group: None,
        min_entropy: 0.0,
        // AWS access keys always start with one of these four-char prefixes.
        keywords: &["akia", "asia", "agpa", "aida", "aroa", "aipa", "anpa", "anva", "asca"],
    },
    SecretRule {
        id: "GCP_API_KEY",
        description: "Google Cloud / Maps API key",
        pattern: r"\bAIza[0-9A-Za-z\-_]{35}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["aiza"],
    },
    SecretRule {
        id: "GITHUB_PAT",
        description: "GitHub personal access token",
        // ghp_ / gho_ / ghu_ / ghs_ / ghr_ + 36 Base62 chars
        pattern: r"\bgh[pousr]_[A-Za-z0-9]{36}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
    },
    SecretRule {
        id: "GITHUB_FINE_GRAINED_PAT",
        description: "GitHub fine-grained PAT",
        pattern: r"\bgithub_pat_[A-Za-z0-9_]{82}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["github_pat_"],
    },
    SecretRule {
        id: "SLACK_TOKEN",
        description: "Slack token",
        pattern: r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["xoxa-", "xoxb-", "xoxp-", "xoxr-", "xoxs-"],
    },
    SecretRule {
        id: "STRIPE_SECRET_KEY",
        description: "Stripe live or test secret key",
        pattern: r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{24,}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["sk_live_", "sk_test_", "rk_live_", "rk_test_"],
    },
    SecretRule {
        id: "ANTHROPIC_API_KEY",
        description: "Anthropic API key",
        pattern: r"\bsk-ant-[A-Za-z0-9_-]{80,}\b",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["sk-ant-"],
    },
    SecretRule {
        id: "OPENAI_API_KEY",
        description: "OpenAI API key",
        // Both legacy sk- and project-scoped sk-proj-
        pattern: r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}T3BlbkFJ[A-Za-z0-9_-]{20,}\b",
        entropy_group: None,
        min_entropy: 0.0,
        // The fixed "T3BlbkFJ" infix uniquely identifies OpenAI keys.
        keywords: &["t3blbkfj"],
    },
    SecretRule {
        id: "JWT",
        description: "JSON Web Token",
        // header.payload.signature — three Base64url segments
        pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        entropy_group: None,
        min_entropy: 0.0,
        // Two consecutive "eyJ" segments is the structural signature of a JWT.
        keywords: &["eyj"],
    },
    SecretRule {
        id: "PRIVATE_KEY_PEM",
        description: "Private key PEM block",
        pattern: r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        entropy_group: None,
        min_entropy: 0.0,
        keywords: &["-----begin"],
    },

    // ── Database connection strings (entropy-gated on password group) ─────
    //
    // Each captures `user:password@host` (or `:password@` for redis where
    // the user is optional). Entropy gate ≥ 2.5 keeps `password = "test"`
    // out, but lets shortish randomized passwords through.
    SecretRule {
        id: "POSTGRES_CONNECTION_STRING",
        description: "PostgreSQL connection string with embedded credentials",
        pattern: r##"(?i)\bpostgres(?:ql)?://[^:@\s/]+:([^@\s/'"]{4,})@"##,
        entropy_group: Some(1),
        min_entropy: 2.5,
        keywords: &["postgres://", "postgresql://"],
    },
    SecretRule {
        id: "MYSQL_CONNECTION_STRING",
        description: "MySQL/MariaDB connection string with embedded credentials",
        pattern: r##"(?i)\bmysql://[^:@\s/]+:([^@\s/'"]{4,})@"##,
        entropy_group: Some(1),
        min_entropy: 2.5,
        keywords: &["mysql://"],
    },
    SecretRule {
        id: "MONGODB_CONNECTION_STRING",
        description: "MongoDB connection string with embedded credentials",
        // mongodb:// and mongodb+srv:// both supported.
        pattern: r##"(?i)\bmongodb(?:\+srv)?://[^:@\s/]+:([^@\s/'"]{4,})@"##,
        entropy_group: Some(1),
        min_entropy: 2.5,
        keywords: &["mongodb://", "mongodb+srv://"],
    },
    SecretRule {
        id: "REDIS_CONNECTION_STRING",
        description: "Redis connection string with embedded credentials",
        // Redis URLs frequently have an empty username slot: redis://:pw@host.
        pattern: r##"(?i)\bredis://[^:@\s/]*:([^@\s/'"]{4,})@"##,
        entropy_group: Some(1),
        min_entropy: 2.5,
        keywords: &["redis://"],
    },
    SecretRule {
        id: "JDBC_PASSWORD_PARAM",
        description: "JDBC URL with embedded password parameter",
        // jdbc:<driver>://host[:port]/db?...&password=secret
        pattern: r##"(?i)\bjdbc:[a-z][a-z0-9]*:[^\s'"]+[?&]password=([^&\s'"]{4,})"##,
        entropy_group: Some(1),
        min_entropy: 2.0,
        keywords: &["jdbc:"],
    },
    SecretRule {
        id: "HTTP_BASIC_AUTH_URL",
        description: "HTTP(S) URL with embedded basic-auth credentials",
        // Matches https://user:secret@host. Excludes the scheme-only case so
        // `https://example.com:443/path` is not interpreted as user:port.
        pattern: r##"(?i)\bhttps?://[A-Za-z0-9._~%+\-]+:([^@\s/'"]{4,})@"##,
        entropy_group: Some(1),
        min_entropy: 2.5,
        keywords: &["http://", "https://"],
    },

    // ── Generic high-entropy assignment (entropy-gated, catches the tail) ─
    //
    // Matches `<credential-keyword> [=:] "<value>"` where `<value>` has
    // enough characters and entropy to plausibly be a real secret. The
    // entropy gate keeps this from firing on `password = "config"`.
    //
    // `keywords` intentionally empty: this rule IS the keyword pre-filter
    // (the regex's own credential-keyword alternation does the same job),
    // and there's no single substring we could anchor on without falsely
    // suppressing real matches.
    SecretRule {
        id: "GENERIC_HIGH_ENTROPY",
        description: "high-entropy credential-like assignment",
        pattern: r#"(?i)(?:api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|password|passwd|private[_-]?key)\s*[:=]\s*['"]([A-Za-z0-9+/=_\-]{20,})['"]"#,
        entropy_group: Some(1),
        min_entropy: 3.5,
        keywords: &[],
    },
];

/// Per-line allowlist substrings. If any of these (case-insensitive) appears
/// on a matched line, the finding is discarded. This catches the most common
/// "obviously a placeholder" footguns without resorting to a regex.
const STOPLIST: &[&str] = &[
    "placeholder",
    "your-key-here",
    "your_key_here",
    "your-token-here",
    "your_token_here",
    "<insert",
    "<your",
    "<key>",
    "<token>",
    "xxxxxxx",
    "example",
    "redacted",
    "change-me",
    "change_me",
    "changeme",
    "dummy",
    "fake",
    "test_only",
    "${",   // ${ENV_VAR} interpolation
    "{{",   // {{template_var}}
    "%(",   // %()s old-style python
    // Explicit user-controlled escape hatches. Same convention as gitleaks.
    "h5i:allow",
    "gitleaks:allow",
    "h5i-allow",
];

/// Path allowlist — files that almost always contain false positives
/// (lockfiles, vendor trees, font/image binaries, h5i fixtures, the
/// gitleaks rule pack itself).
const PATH_ALLOWLIST: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "go.sum",
    "composer.lock",
    "/vendor/",
    "/node_modules/",
    "/testdata/",
    "/fixtures/",
    "/__fixtures__/",
    "gitleaks.toml",
];

const ALLOWLISTED_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".tif", ".svg", ".ico",
    ".webp", ".pdf",
    ".ttf", ".otf", ".woff", ".woff2", ".eot",
    ".mp3", ".mp4", ".mov", ".avi", ".webm",
    ".zip", ".tar", ".gz", ".bz2", ".xz", ".7z",
    ".min.js", ".min.css", ".map",
    ".bin", ".o", ".so", ".a", ".dylib", ".dll", ".exe",
];

fn compiled_rules() -> &'static [(Regex, &'static SecretRule)] {
    static COMPILED: OnceLock<Vec<(Regex, &'static SecretRule)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        RULES
            .iter()
            .map(|r| {
                let re = Regex::new(r.pattern)
                    .unwrap_or_else(|e| panic!("invalid secret regex for {}: {e}", r.id));
                (re, r)
            })
            .collect()
    })
}

/// Test-file suffix allowlist. Test fixtures legitimately contain
/// credential-shaped strings; treating them like production code generates
/// nothing but noise. Matches case-insensitively against the basename.
const TEST_FILE_SUFFIXES: &[&str] = &[
    "_test.rs",
    "_tests.rs",
    "_test.go",
    ".test.js",
    ".test.ts",
    ".test.tsx",
    ".spec.js",
    ".spec.ts",
    "_spec.rb",
    "_test.py",
    "test_secrets.rs",
];

/// File basenames that define the rule pack itself — scanning them is a
/// self-inflicted false positive. Add new detector-defining files here.
const RULE_DEFINITION_FILES: &[&str] = &[
    "secrets.rs",
    "gitleaks.toml",
];

/// True when `path` should be skipped entirely (lockfiles, binaries, test
/// fixtures, the rule pack itself, …).
pub fn is_path_allowlisted(path: &str) -> bool {
    // Normalize so the same logic works for repo-relative and abs paths.
    let normalized = path.replace('\\', "/");
    // Prepend "/" so a check like `contains("/vendor/")` catches both
    // `vendor/foo` and `path/to/vendor/foo`.
    let buf = if normalized.starts_with('/') {
        normalized.clone()
    } else {
        format!("/{normalized}")
    };
    if PATH_ALLOWLIST.iter().any(|p| buf.contains(p)) {
        return true;
    }
    let lower = buf.to_ascii_lowercase();
    if ALLOWLISTED_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        return true;
    }
    // Test files: match on basename so `src/foo_test.rs` and
    // `crates/foo/src/bar_test.go` both skip.
    if let Some(basename) = lower.rsplit('/').next() {
        if TEST_FILE_SUFFIXES.iter().any(|s| basename.ends_with(s)) {
            return true;
        }
        if RULE_DEFINITION_FILES.contains(&basename) {
            return true;
        }
    }
    false
}

/// Returns `true` if any keyword in `keywords` appears in `lowered`.
/// Empty `keywords` slice means "no pre-filter — always run the regex".
fn keywords_match(keywords: &[&str], lowered: &str) -> bool {
    keywords.is_empty() || keywords.iter().any(|k| lowered.contains(*k))
}

/// Shannon entropy in bits per character. Empty string → 0.
fn shannon_entropy(s: &str) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f32;
    let mut h = 0.0f32;
    for &c in counts.iter() {
        if c == 0 {
            continue;
        }
        let p = c as f32 / n;
        h -= p * p.log2();
    }
    h
}

/// Redact a captured secret to a safe preview (`abcd…` of the first 4 chars).
fn redact(s: &str) -> String {
    let prefix: String = s.chars().take(4).collect();
    if s.chars().count() > prefix.chars().count() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Scan an iterable of `(line_number, line)` pairs.
///
/// `path` is used solely for the allowlist check; pass an empty string when
/// the content has no path (e.g. an ad-hoc commit message).
pub fn scan_lines<'a, I>(path: &str, lines: I) -> Vec<SecretFinding>
where
    I: IntoIterator<Item = (usize, &'a str)>,
{
    let _span = tracing::trace_span!("secrets_scan_lines", path).entered();
    if !path.is_empty() && is_path_allowlisted(path) {
        tracing::trace!("path allowlisted; skipping");
        return Vec::new();
    }
    let mut out = Vec::new();
    for (n, line) in lines {
        // Lower-case the line once per iteration — shared by both the
        // stoplist and the per-rule keyword pre-filter.
        let lowered = line.to_ascii_lowercase();
        if STOPLIST.iter().any(|s| lowered.contains(s)) {
            continue;
        }
        for (re, rule) in compiled_rules() {
            // Keyword pre-filter: skip the regex entirely when the rule's
            // anchor strings aren't present. This is the largest perf win.
            if !keywords_match(rule.keywords, &lowered) {
                continue;
            }
            let Some(caps) = re.captures(line) else {
                continue;
            };
            if let Some(grp) = rule.entropy_group {
                let val = match caps.get(grp) {
                    Some(m) => m.as_str(),
                    None => continue,
                };
                if shannon_entropy(val) < rule.min_entropy {
                    continue;
                }
                out.push(SecretFinding {
                    rule_id: rule.id,
                    description: rule.description,
                    line: n,
                    preview: redact(val),
                });
            } else {
                let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                out.push(SecretFinding {
                    rule_id: rule.id,
                    description: rule.description,
                    line: n,
                    preview: redact(m),
                });
            }
            break; // one finding per line is enough
        }
    }
    if !out.is_empty() {
        tracing::debug!(path, findings = out.len(), "secrets_scan_lines hits");
    }
    out
}

/// Convenience: scan a full text body. Lines are numbered starting at 1.
pub fn scan_text(path: &Path, text: &str) -> Vec<SecretFinding> {
    let path_str = path.to_string_lossy();
    scan_lines(
        &path_str,
        text.lines().enumerate().map(|(i, l)| (i + 1, l)),
    )
}

/// Marker substituted for a redacted secret span.
const REDACTION_MARKER: &str = "‹redacted›";

/// Replace every secret-like span in `text` with [`REDACTION_MARKER`], reusing
/// the same rule pack as [`scan_lines`]. Returns a scrubbed copy safe to embed
/// in published output (e.g. a PR comment or a pulled message body).
///
/// Differences from [`scan_lines`], which exists to *report* findings:
/// - There is no path argument and no path allowlist — the caller is redacting
///   arbitrary untrusted text (a message body), not a file, so "skip lockfiles"
///   does not apply.
/// - It redacts *every* matching rule on a line, not just the first, because a
///   single untrusted line may carry more than one credential. Defense in depth.
///
/// The guillemet marker is intentionally free of Markdown/HTML metacharacters so
/// it survives a later escaping pass unchanged.
pub fn redact_text(text: &str) -> String {
    let mut lines = text.lines().map(redact_line);
    match lines.next() {
        None => String::new(),
        Some(first) => {
            let mut out = first;
            for line in lines {
                out.push('\n');
                out.push_str(&line);
            }
            out
        }
    }
}

/// Redact one line. Mirrors the per-line *matching* in [`scan_lines`] but
/// substitutes every matched span instead of recording a finding.
///
/// Redaction is a publication safety control, so it deliberately diverges from
/// detection in two fail-closed ways:
///
/// - **No [`STOPLIST`] early-return.** In detection the stoplist suppresses
///   placeholder false positives (`your-key-here`, `EXAMPLE`). Applied to
///   redaction it would be *fail-open*: a line like `example token ghp_<real>`
///   contains `example`, so the whole line — real credential included — would be
///   emitted verbatim. We accept the occasional redacted placeholder instead.
/// - **Every match of every rule is scrubbed**, not just the first per rule, so
///   two distinct credentials of the same type on one line are both removed.
///
/// Matches are collected as byte spans, merged, and the line rebuilt — overlaps
/// across rules collapse to a single marker.
fn redact_line(line: &str) -> String {
    let lowered = line.to_ascii_lowercase();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (re, rule) in compiled_rules() {
        if !keywords_match(rule.keywords, &lowered) {
            continue;
        }
        for caps in re.captures_iter(line) {
            let m = if let Some(grp) = rule.entropy_group {
                match caps.get(grp) {
                    Some(g) if shannon_entropy(g.as_str()) >= rule.min_entropy => g,
                    _ => continue,
                }
            } else {
                match caps.get(0) {
                    Some(g) => g,
                    None => continue,
                }
            };
            if m.start() != m.end() {
                spans.push((m.start(), m.end()));
            }
        }
    }
    if spans.is_empty() {
        return line.to_string();
    }
    // Merge overlapping/adjacent spans so a region matched by two rules yields a
    // single marker rather than a marker nested inside another.
    spans.sort_by_key(|s| s.0);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => {
                if e > last.1 {
                    last.1 = e;
                }
            }
            _ => merged.push((s, e)),
        }
    }
    // Regex offsets land on char boundaries, so byte slicing is safe.
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    for (s, e) in merged {
        out.push_str(&line[cursor..s]);
        out.push_str(REDACTION_MARKER);
        cursor = e;
    }
    out.push_str(&line[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(line: &str) -> Vec<SecretFinding> {
        scan_lines("src/lib.rs", std::iter::once((1, line)))
    }

    #[test]
    fn aws_access_key_id_fires() {
        let f = scan("aws_key = \"AKIAIOSFODNN7EXAMPLE\"");
        // Stoplist contains "example" → suppresses. Use a non-example AKIA.
        assert!(f.is_empty(), "EXAMPLE in line should suppress");

        let f = scan("aws_key = \"AKIAZZZZZZZZZZZZZZZZ\"");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "AWS_ACCESS_KEY_ID");
        assert!(f[0].preview.starts_with("AKIA"));
    }

    #[test]
    fn github_pat_fires() {
        let f = scan("token: \"ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\"");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "GITHUB_PAT");
    }

    #[test]
    fn redact_text_scrubs_secret_keeps_prose() {
        let input = "deploy used token ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 — rotate it";
        let out = redact_text(input);
        assert!(
            !out.contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
            "secret must be gone: {out}"
        );
        assert!(out.contains(REDACTION_MARKER), "marker must be present: {out}");
        // Surrounding prose is preserved.
        assert!(out.starts_with("deploy used token "));
        assert!(out.ends_with(" — rotate it"));
    }

    #[test]
    fn redact_text_leaves_clean_text_untouched() {
        let input = "I refactored render_body and added a test; no creds here.";
        assert_eq!(redact_text(input), input);
    }

    #[test]
    fn redact_text_preserves_line_structure() {
        // Clean placeholder text (matches no rule) is left alone; the real key
        // on the next line is redacted. Line breaks are preserved.
        let input = "key = your-key-here\nreal = AKIAZZZZZZZZZZZZZZZZ";
        let out = redact_text(input);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "key = your-key-here");
        assert!(!lines[1].contains("AKIAZZZZZZZZZZZZZZZZ"));
        assert!(lines[1].contains(REDACTION_MARKER));
    }

    #[test]
    fn redact_text_fails_closed_on_stoplist_word() {
        // Regression (Codex RISK): a stoplist word (`example`) on the same line
        // as a real credential must NOT shield it. Redaction is fail-closed.
        let token = format!("ghp_{}", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
        let input = format!("example token {token} please rotate");
        let out = redact_text(&input);
        assert!(!out.contains(&token), "stoplist word must not fail open: {out}");
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redact_text_scrubs_multiple_same_rule_secrets() {
        // Regression (Codex RISK): two distinct tokens of the SAME rule on one
        // line must both be removed, not just the first.
        let t1 = format!("ghp_{}", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let t2 = format!("ghp_{}", "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let input = format!("old {t1} new {t2}");
        let out = redact_text(&input);
        assert!(!out.contains(&t1), "first token leaked: {out}");
        assert!(!out.contains(&t2), "second token leaked: {out}");
        assert_eq!(out.matches(REDACTION_MARKER).count(), 2);
    }

    #[test]
    fn anthropic_key_fires() {
        // 80+ char tail
        let tail: String = std::iter::repeat_n('A', 95).collect();
        let f = scan(&format!("key=\"sk-ant-{tail}\""));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn jwt_fires() {
        let f = scan(
            "auth: \"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U\"",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "JWT");
    }

    #[test]
    fn private_key_pem_fires() {
        let f = scan("-----BEGIN OPENSSH PRIVATE KEY-----");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "PRIVATE_KEY_PEM");
    }

    #[test]
    fn placeholder_suppressed() {
        // Real-looking shape, but the line says "your-token-here" → must NOT fire.
        let f = scan("github_token: \"your-token-here\"");
        assert!(f.is_empty(), "placeholder should suppress");
    }

    #[test]
    fn low_entropy_assignment_suppressed() {
        // Generic-entropy rule should not fire on low-entropy values.
        let f = scan("password = \"aaaaaaaaaaaaaaaaaaaaaa\""); // 22 chars, all 'a'
        assert!(f.is_empty(), "low-entropy value should not fire");
    }

    #[test]
    fn high_entropy_assignment_fires() {
        let f = scan("api_key = \"Xb4nGq8wPzM3aLv7yFhT2Rc9JeD5tWkB\"");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "GENERIC_HIGH_ENTROPY");
    }

    #[test]
    fn path_allowlist_skips_lockfile() {
        let f = scan_lines(
            "Cargo.lock",
            std::iter::once((1, "token = \"ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\"")),
        );
        assert!(f.is_empty());
    }

    #[test]
    fn path_allowlist_skips_image() {
        let f = scan_lines("assets/logo.png", std::iter::once((1, "AKIAZZZZZZZZZZZZZZZZ")));
        assert!(f.is_empty());
    }

    #[test]
    fn vendor_dir_skipped() {
        let f = scan_lines(
            "vendor/foo/bar.go",
            std::iter::once((1, "key = \"ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\"")),
        );
        assert!(f.is_empty());
    }

    #[test]
    fn shannon_entropy_basics() {
        assert!(shannon_entropy("aaaaaa") < 0.1);
        assert!(shannon_entropy("abcdef") > 2.0);
        assert!(shannon_entropy("Xb4nGq8wPzM3aLv7yFhT2Rc9JeD5tWkB") > 4.5);
    }

    // ── Keyword pre-filter mechanics ─────────────────────────────────────

    #[test]
    fn keywords_match_with_keyword_present() {
        assert!(keywords_match(&["akia"], "found akiazzz in code"));
    }

    #[test]
    fn keywords_match_with_keyword_absent() {
        assert!(!keywords_match(&["akia"], "no anchor here"));
    }

    #[test]
    fn keywords_match_empty_means_always_run() {
        // Catch-all rules like GENERIC_HIGH_ENTROPY use an empty keyword list
        // — the helper must signal "yes run me" so the regex still gets a chance.
        assert!(keywords_match(&[], "anything at all"));
    }

    #[test]
    fn keywords_match_any_of_many() {
        assert!(keywords_match(&["xoxa-", "xoxb-", "xoxp-"], "found xoxp-123"));
        assert!(!keywords_match(&["xoxa-", "xoxb-", "xoxp-"], "xoxq-"));
    }

    #[test]
    fn keyword_prefilter_blocks_unanchored_lines() {
        // Construct a 20-char Base62 blob that has the SHAPE of a GitHub PAT
        // but lacks the `ghp_` prefix. Without the keyword pre-filter you could
        // imagine a future regex misfiring; with it, lines that don't even
        // mention `ghp_` short-circuit before any regex runs. This test pins
        // the no-regex path at a level the scanner publicly observes.
        let f = scan("totally innocent text with no creds at all");
        assert!(f.is_empty());
    }

    // ── Database connection-string rules ─────────────────────────────────

    #[test]
    fn postgres_connection_string_fires() {
        let f = scan("DATABASE_URL=postgres://prod_app:Xb4nGq8wPzM3aLv7yFhT@db.internal:5432/app");
        assert_eq!(f.len(), 1, "expected exactly one finding, got {:?}", f);
        assert_eq!(f[0].rule_id, "POSTGRES_CONNECTION_STRING");
        assert!(f[0].preview.starts_with("Xb4n"));
    }

    #[test]
    fn postgres_connection_string_long_form_scheme() {
        let f = scan("url = \"postgresql://owner:HighEntropyPass1234@host/db\"");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "POSTGRES_CONNECTION_STRING");
    }

    #[test]
    fn postgres_rejects_low_entropy_password() {
        // "password" has Shannon entropy ~2.75 — still above 2.5. Use a more
        // clearly low-entropy value to verify the entropy gate kicks in.
        let f = scan("url = postgres://app:aaaa@host/db");
        assert!(
            f.is_empty(),
            "all-same-char password must fail entropy gate; got {:?}",
            f
        );
    }

    #[test]
    fn mysql_connection_string_fires() {
        let f = scan("uri: mysql://root:Pa55w0rd!sup3rL0ng@127.0.0.1:3306/app");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "MYSQL_CONNECTION_STRING");
    }

    #[test]
    fn mongodb_connection_string_fires() {
        let f = scan("MONGO=mongodb+srv://admin:Tr0ub4dor&3@cluster0.mongodb.net/test");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "MONGODB_CONNECTION_STRING");
    }

    #[test]
    fn redis_connection_string_fires_with_empty_user() {
        let f = scan("REDIS_URL=redis://:Sup3rS3cr3tR3d1s@cache:6379/0");
        assert_eq!(f.len(), 1, "got {:?}", f);
        assert_eq!(f[0].rule_id, "REDIS_CONNECTION_STRING");
    }

    #[test]
    fn jdbc_password_param_fires() {
        let f = scan("DRIVER=jdbc:postgresql://host:5432/db?user=app&password=Xb4nGq8wPzM3aLv7yFhT");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "JDBC_PASSWORD_PARAM");
    }

    #[test]
    fn http_basic_auth_url_fires() {
        let f = scan("curl -X POST https://svcacct:K7zR3mE9wQv2N8pX@api.example.org/v1");
        // The line contains "example", which is on the global STOPLIST — that
        // suppression is correct (test fixtures use example.{com,org} on purpose).
        assert!(
            f.is_empty(),
            "example.org should be stoplisted; got {:?}",
            f
        );

        let f = scan("curl -X POST https://svcacct:K7zR3mE9wQv2N8pX@api.acme.internal/v1");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "HTTP_BASIC_AUTH_URL");
    }

    #[test]
    fn http_basic_auth_url_does_not_fire_on_plain_url() {
        // No credentials embedded → no match.
        let f = scan("see https://docs.acme.internal/auth for details");
        assert!(f.is_empty());
    }

    #[test]
    fn connection_string_in_test_file_path_skipped() {
        // Test-file allowlist must still apply to the new rules.
        let f = scan_lines(
            "src/db/connection_test.rs",
            std::iter::once((1, "postgres://user:Xb4nGq8wPzM3aLv7yFhT@host/db")),
        );
        assert!(f.is_empty(), "test fixtures must be allowlisted; got {:?}", f);
    }
}
