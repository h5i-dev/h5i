//! A bounded, data-only verdict over one response.
//!
//! This is source 2 of `docs/design/design-flow-and-verdict.md`: the rung
//! between "the agent decides" and "an external oracle decides". It is the
//! Nuclei matcher surface and nothing past it, on purpose. It has leaves
//! (`status`, `body`, `header`) and the combinators `all`, `any`, `not`, and it
//! carries no variables, no arithmetic, no calls out. A clause that a matcher
//! cannot express does not go into the matcher; it goes to an oracle (for a
//! repository-owned test) or to the agent (for a live sequence).
//!
//! Because an `Expect` executes nothing the recipe author chose, a flow that
//! carries only this kind of verdict is safe to share and safe to import. That
//! is the whole reason it is data and not code.

use serde::{Deserialize, Deserializer};

/// How a leaf compares text: a literal substring, or a regular expression.
///
/// Same convention the extractors use (`src/cli/websec.rs`): a `regex:` prefix
/// means a pattern, and anything else is a literal the response must contain.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// The response text must contain this literally.
    Word(String),
    /// The response text must match this pattern somewhere.
    Regex(regex::Regex),
}

impl Pattern {
    fn parse(spec: &str) -> Result<Self, String> {
        Self::parse_with(spec, false)
    }

    /// Parse a pattern that will match case-insensitively. Used for the header
    /// block and whole-response leaves, because HTTP header names are
    /// case-insensitive and the engine stores them lowercased, so a caller
    /// writing `Server` must still match a stored `server`.
    fn parse_ci(spec: &str) -> Result<Self, String> {
        Self::parse_with(spec, true)
    }

    fn parse_with(spec: &str, case_insensitive: bool) -> Result<Self, String> {
        match spec.strip_prefix("regex:") {
            Some(rest) => regex::RegexBuilder::new(rest)
                .case_insensitive(case_insensitive)
                .build()
                .map(Pattern::Regex)
                .map_err(|e| format!("`{rest}` is not a regular expression: {e}")),
            // A word carries no case at parse time; `hits_ci` folds it at match.
            None => Ok(Pattern::Word(spec.to_string())),
        }
    }

    fn hits(&self, text: &str) -> bool {
        match self {
            Pattern::Word(word) => text.contains(word.as_str()),
            Pattern::Regex(re) => re.is_match(text),
        }
    }

    /// Like [`hits`], but a `Word` matches without regard to case. The regex
    /// variant already carries case-insensitivity from `parse_ci`.
    fn hits_ci(&self, text: &str) -> bool {
        match self {
            Pattern::Word(word) => text.to_ascii_lowercase().contains(&word.to_ascii_lowercase()),
            Pattern::Regex(re) => re.is_match(text),
        }
    }

    fn describe(&self) -> String {
        match self {
            Pattern::Word(word) => format!("contains {word:?}"),
            Pattern::Regex(re) => format!("matches /{}/", re.as_str()),
        }
    }
}

/// What a `header` leaf asks of one header.
#[derive(Debug, Clone)]
pub enum HeaderTest {
    /// The header is present, with any value.
    Present,
    /// The header is present and its value satisfies the pattern.
    Value(Pattern),
}

/// One clause of a verdict.
///
/// Built through `Deserialize`, which is where a malformed clause is refused
/// with a reason, so that by the time an `Expect` exists every regex in it has
/// compiled and every clause is exactly one shape.
#[derive(Debug, Clone)]
pub enum Expect {
    /// Every clause must hold.
    All(Vec<Expect>),
    /// At least one clause must hold.
    Any(Vec<Expect>),
    /// The clause must not hold.
    Not(Box<Expect>),
    /// The response status equals this.
    Status(u16),
    /// The response body satisfies the pattern.
    Body(Pattern),
    /// The named header satisfies the test.
    Header { name: String, test: HeaderTest },
    /// The serialized header block (each `name: value` on its own line)
    /// satisfies the pattern. This is the whole-header match, distinct from a
    /// single named header: it is what a caller reaches for when the header a
    /// string might be in is not known ahead of time.
    Headers(Pattern),
    /// The whole response, header block then a blank line then the body,
    /// satisfies the pattern.
    Response(Pattern),
    /// A Nuclei-style DSL expression over the response evaluates true. This is a
    /// bounded, pure expression language (see [`crate::dsl`]), not code: it reads
    /// only the response and calls only known pure functions, so it stays in the
    /// shareable layer.
    Dsl(crate::dsl::Program),
}

/// The header block as one text: `name: value`, one per line.
fn header_block(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The result of evaluating an `Expect`, with a one-line reason for a report.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Whether the verdict held.
    pub matched: bool,
    /// Why, in one line: the clause that decided it.
    pub because: String,
}

/// The parts of a response an `Expect` may read. Decoupled from `Body`/`Text`
/// so this module stays pure: each engine passes the body it already decoded.
pub struct Response<'a> {
    pub status: Option<u16>,
    pub headers: &'a [(String, String)],
    pub body: &'a str,
}

impl Expect {
    /// Evaluate against one response.
    pub fn evaluate(&self, response: &Response) -> Outcome {
        match self {
            Expect::All(clauses) => {
                for clause in clauses {
                    let outcome = clause.evaluate(response);
                    if !outcome.matched {
                        return Outcome {
                            matched: false,
                            because: format!("all: {}", outcome.because),
                        };
                    }
                }
                Outcome {
                    matched: true,
                    because: format!("all {} clauses held", clauses.len()),
                }
            }
            Expect::Any(clauses) => {
                for clause in clauses {
                    let outcome = clause.evaluate(response);
                    if outcome.matched {
                        return Outcome {
                            matched: true,
                            because: format!("any: {}", outcome.because),
                        };
                    }
                }
                Outcome {
                    matched: false,
                    because: format!("none of {} clauses held", clauses.len()),
                }
            }
            Expect::Not(clause) => {
                let inner = clause.evaluate(response);
                Outcome {
                    matched: !inner.matched,
                    because: format!("not ({})", inner.because),
                }
            }
            Expect::Status(want) => {
                let got = response.status;
                Outcome {
                    matched: got == Some(*want),
                    because: match got {
                        Some(got) => format!("status {got} vs {want}"),
                        None => format!("status is unknown vs {want}"),
                    },
                }
            }
            Expect::Body(pattern) => Outcome {
                matched: pattern.hits(response.body),
                because: format!("body {}", pattern.describe()),
            },
            Expect::Header { name, test } => {
                let value = response
                    .headers
                    .iter()
                    .find(|(header, _)| header.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.as_str());
                let matched = match (test, value) {
                    (HeaderTest::Present, value) => value.is_some(),
                    (HeaderTest::Value(pattern), Some(value)) => pattern.hits(value),
                    (HeaderTest::Value(_), None) => false,
                };
                let because = match test {
                    HeaderTest::Present => format!("header {name} present"),
                    HeaderTest::Value(pattern) => format!("header {name} {}", pattern.describe()),
                };
                Outcome { matched, because }
            }
            Expect::Headers(pattern) => {
                let block = header_block(response.headers);
                Outcome {
                    matched: pattern.hits_ci(&block),
                    because: format!("headers {}", pattern.describe()),
                }
            }
            Expect::Response(pattern) => {
                let whole = format!("{}\n\n{}", header_block(response.headers), response.body);
                Outcome {
                    matched: pattern.hits_ci(&whole),
                    because: format!("response {}", pattern.describe()),
                }
            }
            Expect::Dsl(program) => {
                let env = crate::dsl::Env {
                    status: response.status,
                    headers: response.headers,
                    body: response.body,
                };
                match program.eval(&env) {
                    Ok(matched) => Outcome {
                        matched,
                        because: format!("dsl `{}`", program.source()),
                    },
                    // An expression that cannot be evaluated is not a match; it is
                    // a verdict that could not be reached, reported as such.
                    Err(why) => Outcome {
                        matched: false,
                        because: format!("dsl `{}` could not evaluate: {why}", program.source()),
                    },
                }
            }
        }
    }
}

/// The wire shape: one object with at most a few keys, exactly one clause's
/// worth. All fields optional so the error can name what was wrong rather than
/// serde naming a missing field of a variant the author never meant.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    all: Option<Vec<Expect>>,
    #[serde(default)]
    any: Option<Vec<Expect>>,
    #[serde(default)]
    not: Option<Box<Expect>>,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    headers: Option<String>,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    dsl: Option<String>,
    #[serde(default)]
    contains: Option<String>,
    #[serde(default)]
    regex: Option<String>,
}

impl<'de> Deserialize<'de> for Expect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        let raw = Raw::deserialize(deserializer)?;
        // A header modifier without a header, or two modifiers at once, is a
        // mistake the author wants named specifically, so it is checked before
        // the general clause count (which would otherwise call `{"contains":..}`
        // an empty clause and hide the real error).
        if (raw.contains.is_some() || raw.regex.is_some()) && raw.header.is_none() {
            return Err(D::Error::custom(
                "`contains` and `regex` qualify a `header` clause and mean nothing without one",
            ));
        }
        if raw.contains.is_some() && raw.regex.is_some() {
            return Err(D::Error::custom(
                "a header takes `contains` or `regex`, not both",
            ));
        }
        // Count the top-level clause keys so exactly one is present. The header
        // modifiers `contains`/`regex` are not clause keys; they qualify
        // `header`.
        let clause_keys = [
            raw.all.is_some(),
            raw.any.is_some(),
            raw.not.is_some(),
            raw.status.is_some(),
            raw.body.is_some(),
            raw.header.is_some(),
            raw.headers.is_some(),
            raw.response.is_some(),
            raw.dsl.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if clause_keys != 1 {
            return Err(D::Error::custom(
                "an expect clause is exactly one of: all, any, not, status, body, header, \
                 headers, response, dsl",
            ));
        }

        if let Some(clauses) = raw.all {
            return Ok(Expect::All(clauses));
        }
        if let Some(clauses) = raw.any {
            return Ok(Expect::Any(clauses));
        }
        if let Some(clause) = raw.not {
            return Ok(Expect::Not(clause));
        }
        if let Some(status) = raw.status {
            return Ok(Expect::Status(status));
        }
        if let Some(spec) = raw.body {
            return Pattern::parse(&spec)
                .map(Expect::Body)
                .map_err(D::Error::custom);
        }
        if let Some(spec) = raw.headers {
            return Pattern::parse_ci(&spec)
                .map(Expect::Headers)
                .map_err(D::Error::custom);
        }
        if let Some(spec) = raw.response {
            return Pattern::parse_ci(&spec)
                .map(Expect::Response)
                .map_err(D::Error::custom);
        }
        if let Some(spec) = raw.dsl {
            return crate::dsl::Program::parse(&spec)
                .map(Expect::Dsl)
                .map_err(D::Error::custom);
        }
        let name = raw.header.expect("clause_keys guarantees one clause");
        let test = if let Some(word) = raw.contains {
            HeaderTest::Value(Pattern::Word(word))
        } else if let Some(pattern) = raw.regex {
            HeaderTest::Value(
                regex::Regex::new(&pattern)
                    .map(Pattern::Regex)
                    .map_err(|e| D::Error::custom(format!("`{pattern}` is not a regex: {e}")))?,
            )
        } else {
            HeaderTest::Present
        };
        Ok(Expect::Header { name, test })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Expect {
        serde_json::from_str(json).expect("a valid expect")
    }

    fn response<'a>(
        status: Option<u16>,
        headers: &'a [(String, String)],
        body: &'a str,
    ) -> Response<'a> {
        Response {
            status,
            headers,
            body,
        }
    }

    #[test]
    fn a_status_leaf_compares_the_status() {
        let expect = parse(r#"{"status": 200}"#);
        assert!(expect.evaluate(&response(Some(200), &[], "")).matched);
        assert!(!expect.evaluate(&response(Some(500), &[], "")).matched);
        assert!(!expect.evaluate(&response(None, &[], "")).matched);
    }

    #[test]
    fn a_body_leaf_is_a_substring_unless_it_says_regex() {
        let word = parse(r#"{"body": "database_password"}"#);
        assert!(word.evaluate(&response(None, &[], "x database_password y")).matched);
        assert!(!word.evaluate(&response(None, &[], "nothing here")).matched);

        let re = parse(r#"{"body": "regex:SQL error: (\\w+)"}"#);
        assert!(re.evaluate(&response(None, &[], "SQL error: syntax")).matched);
        assert!(!re.evaluate(&response(None, &[], "SQL error:")).matched);
    }

    #[test]
    fn a_header_leaf_tests_presence_contains_or_regex() {
        let headers = vec![("Content-Type".to_string(), "text/html; charset=utf-8".to_string())];
        let present = parse(r#"{"header": "content-type"}"#);
        assert!(present.evaluate(&response(None, &headers, "")).matched);
        let absent = parse(r#"{"header": "x-frame-options"}"#);
        assert!(!absent.evaluate(&response(None, &headers, "")).matched);
        let contains = parse(r#"{"header": "content-type", "contains": "text/html"}"#);
        assert!(contains.evaluate(&response(None, &headers, "")).matched);
        let re = parse(r#"{"header": "content-type", "regex": "charset=\\w+"}"#);
        assert!(re.evaluate(&response(None, &headers, "")).matched);
    }

    #[test]
    fn a_headers_leaf_matches_the_whole_header_block_case_insensitively() {
        // The engine stores header names lowercased, so a caller's canonical-case
        // name must still match.
        let headers = vec![
            ("server".to_string(), "nginx/1.25".to_string()),
            ("x-powered-by".to_string(), "PHP/8.1".to_string()),
        ];
        let word = parse(r#"{"headers": "X-Powered-By"}"#);
        assert!(word.evaluate(&response(None, &headers, "")).matched);
        let re = parse(r#"{"headers": "regex:Server: nginx"}"#);
        assert!(re.evaluate(&response(None, &headers, "")).matched);
        let absent = parse(r#"{"headers": "CamelCache"}"#);
        assert!(!absent.evaluate(&response(None, &headers, "")).matched);
    }

    #[test]
    fn a_response_leaf_matches_headers_and_body_together() {
        let headers = vec![("Content-Type".to_string(), "text/html".to_string())];
        let expect = parse(r#"{"response": "regex:text/html[\\s\\S]*<title>"}"#);
        assert!(expect.evaluate(&response(Some(200), &headers, "<title>hi</title>")).matched);
        // The body alone does not carry the header text.
        let body_only = parse(r#"{"body": "text/html"}"#);
        assert!(!body_only.evaluate(&response(Some(200), &headers, "<title>hi</title>")).matched);
    }

    #[test]
    fn combinators_compose() {
        let expect = parse(
            r#"{"all": [
                 {"status": 200},
                 {"any": [{"body": "admin"}, {"body": "root"}]},
                 {"not": {"body": "denied"}}
               ]}"#,
        );
        assert!(expect.evaluate(&response(Some(200), &[], "welcome root")).matched);
        assert!(!expect.evaluate(&response(Some(200), &[], "root but denied")).matched);
        assert!(!expect.evaluate(&response(Some(403), &[], "root")).matched);
    }

    #[test]
    fn a_dsl_leaf_evaluates_an_expression_over_the_response() {
        let expect = parse(
            r#"{"dsl": "status_code == 200 && !contains(tolower(body), '<html')"}"#,
        );
        assert!(expect.evaluate(&response(Some(200), &[], "[core]")).matched);
        assert!(!expect.evaluate(&response(Some(200), &[], "<HTML>")).matched);
        // A dsl leaf composes with the combinators like any other.
        let combined = parse(r#"{"all": [{"status": 200}, {"dsl": "len(body) > 2"}]}"#);
        assert!(combined.evaluate(&response(Some(200), &[], "abcd")).matched);
    }

    #[test]
    fn an_unsupported_dsl_is_refused_at_parse() {
        let err = serde_json::from_str::<Expect>(r#"{"dsl": "rand_int(1,9) == 5"}"#).unwrap_err();
        assert!(err.to_string().contains("unsupported DSL function"), "{err}");
    }

    #[test]
    fn an_empty_or_multi_key_clause_is_refused() {
        let empty = serde_json::from_str::<Expect>(r#"{}"#).unwrap_err();
        assert!(empty.to_string().contains("exactly one"), "{empty}");
        let two = serde_json::from_str::<Expect>(r#"{"status": 200, "body": "x"}"#).unwrap_err();
        assert!(two.to_string().contains("exactly one"), "{two}");
    }

    #[test]
    fn a_header_modifier_without_a_header_is_refused() {
        let orphan = serde_json::from_str::<Expect>(r#"{"contains": "x"}"#).unwrap_err();
        assert!(orphan.to_string().contains("qualify a `header`"), "{orphan}");
        let both =
            serde_json::from_str::<Expect>(r#"{"header": "h", "contains": "a", "regex": "b"}"#)
                .unwrap_err();
        assert!(both.to_string().contains("not both"), "{both}");
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let typo = serde_json::from_str::<Expect>(r#"{"stat": 200}"#).unwrap_err();
        assert!(typo.to_string().contains("unknown field"), "{typo}");
    }

    #[test]
    fn a_bad_regex_is_refused_at_parse_time() {
        let bad = serde_json::from_str::<Expect>(r#"{"body": "regex:("}"#).unwrap_err();
        assert!(bad.to_string().contains("not a regular expression"), "{bad}");
    }
}
