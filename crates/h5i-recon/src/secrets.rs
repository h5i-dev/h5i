//! Credentials, keys, connection strings and sourcemaps a bundle discloses
//! (design-recon.md N8).
//!
//! A disclosure, not a verdict: every match names its rule, the value is
//! redacted (the store holds the bytes), and whether a leaked key matters is
//! the agent's call. A regex pass, unlike [`crate::js`], because a key in a
//! comment is still a key: syntactic position is exactly what to ignore here.

use std::sync::LazyLock;

use regex::Regex;
use url::Url;

/// The most disclosures one document may contribute, so a generated bundle
/// cannot fill the report on its own.
pub const MAX_PER_DOCUMENT: usize = 200;

/// One value a document disclosed, before the caller decides what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// The family: `credential`, `publishable`, `private-key`,
    /// `connection-string` or `sourcemap`. Coarse on purpose; `rule` is the
    /// specific claim. `publishable` is a token meant to ship to the client
    /// (a Mapbox `pk.` or a Stripe `pk_`), surfaced but not alarmed over.
    pub kind: &'static str,
    /// Which pattern matched, e.g. `aws-access-key-id` or `sourcemap`.
    pub rule: &'static str,
    /// The match, redacted for a credential and shown whole for a place.
    pub preview: String,
    /// Byte offset of the disclosed value in the source, for the reader who
    /// wants to see it in the stored body.
    pub at: usize,
    /// How many like it were folded here. A config that embeds a token array
    /// is one disclosure with a count, not a screenful of near-identical rows.
    pub count: usize,
    /// A place this disclosure points at, when it is one: the resolved sourcemap
    /// URL, or the host a connection string names. `None` for a bare secret.
    pub url: Option<Url>,
}

/// A named pattern whose whole match is the disclosed value.
struct Rule {
    kind: &'static str,
    rule: &'static str,
    re: Regex,
}

/// Patterns anchored on a vendor prefix or fixed marker, so a match is a strong
/// claim. The labelled-secret rule below has no such anchor and earns its keep
/// with an entropy check instead.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let r = |kind, rule, pattern: &str| Rule {
        kind,
        rule,
        re: Regex::new(pattern).expect("a static pattern compiles"),
    };
    vec![
        r("credential", "aws-access-key-id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        r("credential", "google-api-key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
        r("credential", "slack-token", r"\bxox[baprs]-[0-9A-Za-z-]{10,48}\b"),
        r("credential", "github-token", r"\bgh[pousr]_[0-9A-Za-z]{36,}\b"),
        r("credential", "stripe-secret-key", r"\b(?:sk|rk)_live_[0-9A-Za-z]{16,}\b"),
        // Meant to ship to the client, so surfaced under `publishable` and never
        // alarmed over: the reader learns it is there, not that it is a leak.
        r("publishable", "stripe-publishable-key", r"\bpk_(?:live|test)_[0-9A-Za-z]{16,}\b"),
        r("publishable", "mapbox-public-token", r"\bpk\.eyJ[A-Za-z0-9_\-]{20,}\b"),
        r(
            "private-key",
            "pem-private-key",
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----",
        ),
    ]
});

/// A JWT: three base64url segments, the first two starting `eyJ` (i.e. `{"`).
/// Kept apart from [`RULES`] so an array of them folds by header (§ [`scan`]).
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b")
        .expect("a static pattern compiles")
});

/// `scheme://user:password@host…`, captured so the host can be surfaced and the
/// password redacted.
static CONNECTION_STRING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(?:mongodb(?:\+srv)?|postgres(?:ql)?|mysql|redis|amqps?)://[^\s:@/"']+:[^\s:@/"']+@[^\s/"'#?]+"#,
    )
    .expect("a static pattern compiles")
});

/// `//# sourceMappingURL=…` (and the older `//@`), whose argument is the map.
static SOURCEMAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"//[#@]\s*sourceMappingURL=(\S+)").expect("a static pattern compiles"));

/// A `name = "value"` whose name looks like a secret's. No anchor to trust, so
/// the value must be quoted, long, high-entropy and not a placeholder (a
/// declared filter, applied in [`scan`], not a judgement that it matters).
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:api[_-]?key|access[_-]?key|secret(?:[_-]?key)?|client[_-]?secret|auth[_-]?token|api[_-]?secret|passwd|password)"?\s*[:=]\s*"([^"]{12,120})""#,
    )
    .expect("a static pattern compiles")
});

/// Read one document for disclosed secrets and sourcemaps.
///
/// `base` is the response's final URL, used to resolve a relative sourcemap the
/// way the browser would. Matches are returned in the order they appear, capped
/// at [`MAX_PER_DOCUMENT`].
pub fn scan(base: &Url, source: &str) -> Vec<Disclosure> {
    let mut out: Vec<Disclosure> = Vec::new();

    for rule in RULES.iter() {
        for m in rule.re.find_iter(source) {
            out.push(Disclosure {
                kind: rule.kind,
                rule: rule.rule,
                preview: redact(m.as_str()),
                at: m.start(),
                count: 1,
                url: None,
            });
        }
    }

    // JWTs fold by header segment (the part before the first `.`, identical for
    // every token with the same `alg`): a config that embeds fifty context
    // tokens is one row with a count, not fifty. The first is kept whole.
    let mut jwt_first: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for m in JWT.find_iter(source) {
        let header = m.as_str().split('.').next().unwrap_or_default();
        match jwt_first.get(header) {
            Some(&idx) => out[idx].count += 1,
            None => {
                jwt_first.insert(header, out.len());
                out.push(Disclosure {
                    kind: "credential",
                    rule: "json-web-token",
                    preview: redact(m.as_str()),
                    at: m.start(),
                    count: 1,
                    url: None,
                });
            }
        }
    }

    for m in CONNECTION_STRING.find_iter(source) {
        let (preview, url) = match Url::parse(m.as_str()) {
            // Keep the useful half (scheme and host), drop the password.
            Ok(parsed) => {
                let shown = format!(
                    "{}://{}:\u{2022}\u{2022}\u{2022}@{}",
                    parsed.scheme(),
                    parsed.username(),
                    parsed.host_str().unwrap_or("")
                );
                (shown, Some(parsed))
            }
            Err(_) => (redact(m.as_str()), None),
        };
        out.push(Disclosure {
            kind: "connection-string",
            rule: "connection-string",
            preview,
            at: m.start(),
            count: 1,
            url,
        });
    }

    for caps in SOURCEMAP.captures_iter(source) {
        let whole = caps.get(0).expect("group 0 exists");
        let token = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        // A place, not a secret: shown whole and resolved. An inline `data:` map
        // is in the bundle already, so there is nothing to chase.
        let url = if token.starts_with("data:") {
            None
        } else {
            base.join(token).ok()
        };
        out.push(Disclosure {
            kind: "sourcemap",
            rule: if token.starts_with("data:") { "sourcemap-inline" } else { "sourcemap" },
            preview: url.as_ref().map(Url::to_string).unwrap_or_else(|| preview_place(token)),
            at: whole.start(),
            count: 1,
            url,
        });
    }

    for caps in ASSIGNMENT.captures_iter(source) {
        let value = caps.get(1).expect("the value group is required by the pattern");
        let v = value.as_str();
        if is_placeholder(v) || bits_per_char(v) < 3.0 {
            continue;
        }
        out.push(Disclosure {
            kind: "credential",
            rule: "labelled-secret",
            preview: redact(v),
            at: value.start(),
            count: 1,
            url: None,
        });
    }

    // A JWT and a `token = "…"` around it are one leak seen twice.
    out.sort_by(|a, b| a.at.cmp(&b.at).then(a.rule.cmp(b.rule)));
    out.dedup_by(|a, b| a.at == b.at && a.preview == b.preview);
    out.truncate(MAX_PER_DOCUMENT);
    out
}

/// First and last few characters: enough to tell two leaks apart without
/// reprinting the secret the store already holds.
fn redact(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return "\u{2022}".repeat(n);
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[n - 2..].iter().collect();
    format!("{head}\u{2026}({} more)\u{2026}{tail}", n - 6)
}

/// A place worth showing but too long to show whole.
fn preview_place(token: &str) -> String {
    if token.len() <= 96 {
        token.to_string()
    } else {
        format!("{}\u{2026}", &token[..96])
    }
}

/// Shannon entropy in bits per character, to keep the labelled-secret rule off
/// `password = "not set here"`.
fn bits_per_char(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    let mut total = 0usize;
    for b in value.bytes() {
        counts[b as usize] += 1;
        total += 1;
    }
    let total = total as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / total;
            -p * p.log2()
        })
        .sum()
}

/// The values that are the shape of a secret and the substance of a stand-in.
fn is_placeholder(value: &str) -> bool {
    if value.contains("${") || value.contains("{{") || value.contains('<') {
        return true;
    }
    // All one character ("xxxxxxxxxxxx", "000000000000").
    if value.bytes().all(|b| b == value.as_bytes()[0]) {
        return true;
    }
    let low = value.to_ascii_lowercase();
    const STANDINS: &[&str] = &[
        "your", "example", "changeme", "change-me", "placeholder", "todo", "test", "dummy",
        "sample", "redacted", "xxxx", "process.env", "insert", "none",
    ];
    STANDINS.iter().any(|s| low.starts_with(s) || low.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://app.test/static/main.min.js").unwrap()
    }

    fn rules(found: &[Disclosure]) -> Vec<&str> {
        found.iter().map(|d| d.rule).collect()
    }

    #[test]
    fn an_aws_key_is_disclosed_and_redacted() {
        let src = r#"const cfg = {key: "AKIAIOSFODNN7EXAMPLE"};"#;
        let found = scan(&base(), src);
        assert_eq!(rules(&found), vec!["aws-access-key-id"]);
        let d = &found[0];
        assert_eq!(d.kind, "credential");
        // The value is not reprinted whole.
        assert!(!d.preview.contains("AKIAIOSFODNN7EXAMPLE"), "{}", d.preview);
        assert!(d.preview.starts_with("AKIA") && d.preview.ends_with("LE"), "{}", d.preview);
    }

    #[test]
    fn a_key_in_a_comment_is_still_a_leak() {
        // The endpoint reader ignores comments; the disclosure reader must not.
        let src = "// old google key AIza012345678901234567890123456789abcde left in\n";
        assert_eq!(rules(&scan(&base(), src)), vec!["google-api-key"]);
    }

    #[test]
    fn a_jwt_is_recognised_by_shape() {
        let src = "auth=eyJhbGciOiJub25lIn0.eyJ1cG4iOiJhZG1pbiJ9.signaturehere123";
        let found = scan(&base(), src);
        assert_eq!(rules(&found), vec!["json-web-token"]);
        assert_eq!(found[0].count, 1);
    }

    #[test]
    fn an_array_of_jwts_folds_to_one_row_with_a_count() {
        // A config embedding many tokens that share one header is one
        // disclosure, not a screenful (the travel.crypto.com case).
        let h = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!("\"T{i}\":\"{h}.eyJzdWIiOiJLTEEiLCJuIjp{i}9.sig{i}abcdefgh\","));
        }
        let found = scan(&base(), &src);
        let jwts: Vec<&Disclosure> = found.iter().filter(|d| d.rule == "json-web-token").collect();
        assert_eq!(jwts.len(), 1, "one row for the whole array");
        assert_eq!(jwts[0].count, 40, "and it counts every token it folded");
    }

    #[test]
    fn publishable_tokens_are_surfaced_but_not_alarmed() {
        for (src, rule) in [
            (r#"mapboxgl.accessToken = "pk.eyJ1Ijoib25lIiwiYSI6ImNrMTIzNDU2Nzg5In0.abcDEF";"#, "mapbox-public-token"),
            (r#"const stripe = Stripe("pk_live_51H8xYz0123456789abcdef");"#, "stripe-publishable-key"),
        ] {
            let found = scan(&base(), src);
            assert_eq!(rules(&found), vec![rule], "{src}");
            assert_eq!(found[0].kind, "publishable", "not a credential to alarm over: {src}");
        }
    }

    #[test]
    fn a_connection_string_shows_the_host_and_hides_the_password() {
        let src = r#"MONGO="mongodb://svc:s3cr3tp4ss@db.internal:27017/app""#;
        let found = scan(&base(), src);
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.kind, "connection-string");
        assert!(d.preview.contains("db.internal"), "the host is the useful half: {}", d.preview);
        assert!(!d.preview.contains("s3cr3tp4ss"), "the password is not: {}", d.preview);
        assert_eq!(d.url.as_ref().unwrap().host_str(), Some("db.internal"));
    }

    #[test]
    fn a_sourcemap_is_resolved_against_the_bundle_and_shown_whole() {
        let src = "console.log(1)\n//# sourceMappingURL=main.min.js.map\n";
        let found = scan(&base(), src);
        assert_eq!(rules(&found), vec!["sourcemap"]);
        let d = &found[0];
        assert_eq!(d.url.as_ref().unwrap().as_str(), "https://app.test/static/main.min.js.map");
        assert!(d.preview.ends_with("main.min.js.map"));
    }

    #[test]
    fn an_inline_sourcemap_has_nothing_to_chase() {
        let src = "//# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozfQ==\n";
        let found = scan(&base(), src);
        assert_eq!(rules(&found), vec!["sourcemap-inline"]);
        assert!(found[0].url.is_none());
    }

    #[test]
    fn a_labelled_secret_needs_entropy_and_is_not_a_placeholder() {
        let real = r#"api_key = "9f8c2a1be74d40b3a6f10c2d5e8b7a44""#;
        assert_eq!(rules(&scan(&base(), real)), vec!["labelled-secret"]);

        // Stand-ins are the shape of a secret and the substance of nothing.
        for stand_in in [
            r#"api_key = "your-api-key-here""#,
            r#"password = "changeme-please""#,
            r#"secret = "xxxxxxxxxxxxxxxx""#,
            r#"apiKey = "${PROCESS_ENV_KEY}""#,
        ] {
            assert!(
                scan(&base(), stand_in).is_empty(),
                "a placeholder is not a disclosure: {stand_in}"
            );
        }
    }

    #[test]
    fn a_pem_private_key_header_is_a_disclosure() {
        let src = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n";
        assert_eq!(rules(&scan(&base(), src)), vec!["pem-private-key"]);
    }

    #[test]
    fn ordinary_code_discloses_nothing() {
        let src = "function add(a, b) { return a + b; } const url = '/api/users';";
        assert!(scan(&base(), src).is_empty());
    }

    #[test]
    fn a_document_cannot_flood_the_report() {
        let one = "key: \"AKIAIOSFODNN7EXAMPLE\"\n";
        let many = one.repeat(MAX_PER_DOCUMENT + 50);
        assert_eq!(scan(&base(), &many).len(), MAX_PER_DOCUMENT);
    }
}
