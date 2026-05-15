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
    },
    SecretRule {
        id: "GCP_API_KEY",
        description: "Google Cloud / Maps API key",
        pattern: r"\bAIza[0-9A-Za-z\-_]{35}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "GITHUB_PAT",
        description: "GitHub personal access token",
        // ghp_ / gho_ / ghu_ / ghs_ / ghr_ + 36 Base62 chars
        pattern: r"\bgh[pousr]_[A-Za-z0-9]{36}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "GITHUB_FINE_GRAINED_PAT",
        description: "GitHub fine-grained PAT",
        pattern: r"\bgithub_pat_[A-Za-z0-9_]{82}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "SLACK_TOKEN",
        description: "Slack token",
        pattern: r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "STRIPE_SECRET_KEY",
        description: "Stripe live or test secret key",
        pattern: r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{24,}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "ANTHROPIC_API_KEY",
        description: "Anthropic API key",
        pattern: r"\bsk-ant-[A-Za-z0-9_-]{80,}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "OPENAI_API_KEY",
        description: "OpenAI API key",
        // Both legacy sk- and project-scoped sk-proj-
        pattern: r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}T3BlbkFJ[A-Za-z0-9_-]{20,}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "JWT",
        description: "JSON Web Token",
        // header.payload.signature — three Base64url segments
        pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        entropy_group: None,
        min_entropy: 0.0,
    },
    SecretRule {
        id: "PRIVATE_KEY_PEM",
        description: "Private key PEM block",
        pattern: r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        entropy_group: None,
        min_entropy: 0.0,
    },

    // ── Generic high-entropy assignment (entropy-gated, catches the tail) ─
    //
    // Matches `<credential-keyword> [=:] "<value>"` where `<value>` has
    // enough characters and entropy to plausibly be a real secret. The
    // entropy gate keeps this from firing on `password = "config"`.
    SecretRule {
        id: "GENERIC_HIGH_ENTROPY",
        description: "high-entropy credential-like assignment",
        pattern: r#"(?i)(?:api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|password|passwd|private[_-]?key)\s*[:=]\s*['"]([A-Za-z0-9+/=_\-]{20,})['"]"#,
        entropy_group: Some(1),
        min_entropy: 3.5,
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
    "/h5i-py-parser",
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

/// True when `path` should be skipped entirely (lockfiles, binaries, …).
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
    false
}

fn line_is_stoplisted(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    STOPLIST.iter().any(|s| lower.contains(s))
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
    if !path.is_empty() && is_path_allowlisted(path) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (n, line) in lines {
        if line_is_stoplisted(line) {
            continue;
        }
        for (re, rule) in compiled_rules() {
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
    fn anthropic_key_fires() {
        // 80+ char tail
        let tail: String = std::iter::repeat('A').take(95).collect();
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
}
