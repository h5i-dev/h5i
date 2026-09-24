//! Import a Nuclei template as an h5i test whose verdict is data.
//!
//! A Nuclei template is the largest free corpus of source-2 verdicts there is: a
//! request, a set of matchers, a boolean. This turns one into an `h5i.test/v1`
//! file whose verdict is an `expect` and never an oracle, because an imported
//! recipe is data, not code (design-flow-and-verdict.md F7, F9). A construct that
//! cannot be expressed as data is refused with a reason rather than lowered into
//! a script, because a skipped template is a gap and a smuggled oracle is a
//! shared executable.
//!
//! The mapping is faithful to Nuclei's defaults, which are worth stating because
//! they are permissive: an unset `matchers-condition` is `or`, and an unset
//! per-matcher `condition` (over a list of words or patterns) is `or`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── the Nuclei template, the part of it this reads ──────────────────────────

/// Unknown fields are allowed: a template carries severity, tags, references and
/// much else this importer does not model, and refusing them would refuse almost
/// every real template. What is refused is a construct that *changes the verdict*
/// and cannot be expressed as data, which is caught by value, not by shape.
#[derive(Debug, Deserialize)]
struct Template {
    id: String,
    #[serde(default)]
    info: Info,
    /// Nuclei v3 spells the block `http`; older templates spell it `requests`.
    #[serde(default)]
    http: Vec<HttpRequest>,
    #[serde(default)]
    requests: Vec<HttpRequest>,
}

#[derive(Debug, Default, Deserialize)]
struct Info {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HttpRequest {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Vec<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    /// A raw HTTP request. Not modelled: it carries its own request line and
    /// header block, which is not a request template.
    #[serde(default)]
    raw: Vec<String>,
    #[serde(default, rename = "matchers-condition")]
    matchers_condition: Option<String>,
    #[serde(default)]
    matchers: Vec<Matcher>,
    #[serde(default)]
    extractors: Vec<Extractor>,
}

#[derive(Debug, Deserialize)]
struct Matcher {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    words: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    status: Vec<u16>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    negative: bool,
}

#[derive(Debug, Deserialize)]
struct Extractor {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    regex: Vec<String>,
}

// ── the h5i test this writes ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct TestOut {
    version: &'static str,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    requests: BTreeMap<String, RequestOut>,
    flow: Vec<StepOut>,
}

#[derive(Debug, Serialize)]
struct RequestOut {
    method: String,
    path: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Debug, Serialize)]
struct StepOut {
    send: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    extract: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expect: Option<Value>,
}

/// `h5i websec import-nuclei <file>`: read a template, print an h5i test.
pub fn import(file: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("{} could not be read: {e}", file.display()))?;
    let template: Template = serde_yaml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not a Nuclei template: {e}", file.display()))?;
    let out = convert(template)?;
    let yaml = serde_yaml::to_string(&out)?;
    print!("{yaml}");
    Ok(())
}

fn convert(template: Template) -> anyhow::Result<TestOut> {
    let entries = if !template.http.is_empty() {
        template.http
    } else {
        template.requests
    };
    if entries.is_empty() {
        anyhow::bail!(
            "template `{}` has no http requests to import",
            template.id
        );
    }

    let mut requests = BTreeMap::new();
    let mut flow = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let n = index + 1;
        let id = format!("req_{n}");
        if !entry.raw.is_empty() {
            anyhow::bail!(
                "template `{}` request {n} is a raw request, which is not a request template",
                template.id
            );
        }
        let path_spec = entry.path.first().ok_or_else(|| {
            anyhow::anyhow!("template `{}` request {n} has no path", template.id)
        })?;
        let path = relative_path(path_spec).ok_or_else(|| {
            anyhow::anyhow!(
                "template `{}` request {n} path `{path_spec}` uses a Nuclei variable this \
                 importer does not model; only a `{{{{BaseURL}}}}`-relative path imports",
                template.id
            )
        })?;
        for (name, value) in &entry.headers {
            if has_interpolation(value) {
                anyhow::bail!(
                    "template `{}` request {n} header `{name}` uses a Nuclei variable this \
                     importer does not model",
                    template.id
                );
            }
        }
        if let Some(body) = &entry.body
            && has_interpolation(body)
        {
            anyhow::bail!(
                "template `{}` request {n} body uses a Nuclei variable this importer does \
                 not model",
                template.id
            );
        }

        requests.insert(
            id.clone(),
            RequestOut {
                method: entry.method.clone().unwrap_or_else(|| "GET".to_string()),
                path,
                headers: entry.headers,
                body: entry.body,
            },
        );

        let expect = matchers_to_expect(&template.id, n, &entry.matchers, &entry.matchers_condition)?;
        // Guarantee the verdict is valid under the shared grammar before it is
        // written, so an import never emits an expect the engines would reject.
        if let Some(value) = &expect {
            serde_json::from_value::<h5i_wire::Expect>(value.clone()).map_err(|e| {
                anyhow::anyhow!("template `{}` request {n} produced an invalid expect: {e}", template.id)
            })?;
        }
        let extract = extractors_to_bindings(&template.id, n, &entry.extractors)?;

        flow.push(StepOut {
            send: id,
            extract,
            expect,
        });
    }

    Ok(TestOut {
        version: "h5i.test/v1",
        id: sanitise_id(&template.id),
        name: template.info.name,
        requests,
        flow,
    })
}

/// A `{{BaseURL}}`/`{{RootURL}}`-relative path, or `None` if the path carries any
/// other Nuclei variable this importer does not resolve.
fn relative_path(spec: &str) -> Option<String> {
    let trimmed = spec
        .trim()
        .strip_prefix("{{BaseURL}}")
        .or_else(|| spec.trim().strip_prefix("{{RootURL}}"))
        .unwrap_or_else(|| spec.trim());
    if has_interpolation(trimmed) {
        return None;
    }
    if trimmed.is_empty() {
        Some("/".to_string())
    } else if trimmed.starts_with('/') {
        Some(trimmed.to_string())
    } else {
        Some(format!("/{trimmed}"))
    }
}

fn has_interpolation(text: &str) -> bool {
    text.contains("{{")
}

/// Nuclei ids allow characters an h5i test id does not; keep the safe ones.
fn sanitise_id(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect();
    if safe.is_empty() { "imported".to_string() } else { safe }
}

fn matchers_to_expect(
    template: &str,
    n: usize,
    matchers: &[Matcher],
    condition: &Option<String>,
) -> anyhow::Result<Option<Value>> {
    if matchers.is_empty() {
        return Ok(None);
    }
    let mut clauses = Vec::new();
    for matcher in matchers {
        clauses.push(one_matcher(template, n, matcher)?);
    }
    // Nuclei's default matchers-condition is `or`.
    let combined = combine(clauses, condition.as_deref().unwrap_or("or"), template, n)?;
    Ok(Some(combined))
}

fn one_matcher(template: &str, n: usize, matcher: &Matcher) -> anyhow::Result<Value> {
    let clause = match matcher.kind.as_str() {
        "status" => {
            let leaves: Vec<Value> = matcher.status.iter().map(|s| json!({ "status": s })).collect();
            match leaves.len() {
                0 => anyhow::bail!("template `{template}` request {n} has a status matcher with no status"),
                1 => leaves.into_iter().next().expect("one"),
                _ => json!({ "any": leaves }),
            }
        }
        "word" => {
            body_part(template, n, "word", &matcher.part)?;
            let leaves: Vec<Value> = matcher.words.iter().map(|w| json!({ "body": w })).collect();
            // Nuclei's default word `condition` is `or`.
            combine_leaves(leaves, matcher.condition.as_deref().unwrap_or("or"), template, n, "word")?
        }
        "regex" => {
            body_part(template, n, "regex", &matcher.part)?;
            let leaves: Vec<Value> = matcher
                .regex
                .iter()
                .map(|r| json!({ "body": format!("regex:{r}") }))
                .collect();
            combine_leaves(leaves, matcher.condition.as_deref().unwrap_or("or"), template, n, "regex")?
        }
        other => anyhow::bail!(
            "template `{template}` request {n} uses a `{other}` matcher, which has no data-only \
             equivalent; a repository-owned test can express it with an oracle instead"
        ),
    };
    if matcher.negative {
        Ok(json!({ "not": clause }))
    } else {
        Ok(clause)
    }
}

/// A word or regex matcher only imports when it reads the body. Nuclei's
/// `header`/`all`/`raw` parts match a text block this grammar has no leaf for
/// (its `header` leaf takes a name and a value), so they are refused rather than
/// mis-mapped.
fn body_part(template: &str, n: usize, kind: &str, part: &Option<String>) -> anyhow::Result<()> {
    match part.as_deref() {
        None | Some("body") => Ok(()),
        Some(other) => anyhow::bail!(
            "template `{template}` request {n} has a {kind} matcher on part `{other}`; only \
             `body` imports"
        ),
    }
}

fn combine_leaves(
    leaves: Vec<Value>,
    condition: &str,
    template: &str,
    n: usize,
    kind: &str,
) -> anyhow::Result<Value> {
    if leaves.is_empty() {
        anyhow::bail!("template `{template}` request {n} has a {kind} matcher with no {kind}s");
    }
    combine(leaves, condition, template, n)
}

fn combine(mut clauses: Vec<Value>, condition: &str, template: &str, n: usize) -> anyhow::Result<Value> {
    if clauses.len() == 1 {
        return Ok(clauses.pop().expect("one"));
    }
    match condition {
        "and" => Ok(json!({ "all": clauses })),
        "or" => Ok(json!({ "any": clauses })),
        other => anyhow::bail!(
            "template `{template}` request {n} uses condition `{other}`; only `and` and `or` \
             are understood"
        ),
    }
}

fn extractors_to_bindings(
    template: &str,
    n: usize,
    extractors: &[Extractor],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for extractor in extractors {
        if extractor.kind != "regex" {
            anyhow::bail!(
                "template `{template}` request {n} has a `{}` extractor; only a `regex` \
                 extractor imports as a binding",
                extractor.kind
            );
        }
        body_part(template, n, "regex extractor", &extractor.part)?;
        let name = extractor.name.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "template `{template}` request {n} has a regex extractor with no name to bind to"
            )
        })?;
        let pattern = extractor.regex.first().ok_or_else(|| {
            anyhow::anyhow!("template `{template}` request {n} extractor `{name}` has no pattern")
        })?;
        bindings.insert(name, format!("regex:{pattern}"));
    }
    Ok(bindings)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn convert_yaml(yaml: &str) -> anyhow::Result<TestOut> {
        convert(serde_yaml::from_str(yaml).expect("a template"))
    }

    #[test]
    fn a_single_request_template_becomes_a_step_with_a_verdict() {
        let out = convert_yaml(
            r#"
id: exposed-config
info:
  name: Exposed config
http:
  - method: GET
    path:
      - "{{BaseURL}}/admin/config.php"
    matchers-condition: and
    matchers:
      - type: status
        status:
          - 200
      - type: word
        words:
          - database_password
"#,
        )
        .expect("import");
        assert_eq!(out.version, "h5i.test/v1");
        assert_eq!(out.id, "exposed-config");
        let req = out.requests.get("req_1").expect("a request");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/admin/config.php");
        let step = &out.flow[0];
        assert_eq!(step.send, "req_1");
        let expect = step.expect.as_ref().expect("a verdict");
        assert_eq!(
            expect,
            &json!({"all": [{"status": 200}, {"body": "database_password"}]})
        );
        serde_json::from_value::<h5i_wire::Expect>(expect.clone()).expect("valid expect");
    }

    #[test]
    fn multiple_words_default_to_or() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        words:
          - alpha
          - beta
"#,
        )
        .expect("import");
        assert_eq!(
            out.flow[0].expect.as_ref().unwrap(),
            &json!({"any": [{"body": "alpha"}, {"body": "beta"}]})
        );
    }

    #[test]
    fn a_negative_matcher_becomes_not() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        negative: true
        words:
          - denied
"#,
        )
        .expect("import");
        assert_eq!(
            out.flow[0].expect.as_ref().unwrap(),
            &json!({"not": {"body": "denied"}})
        );
    }

    #[test]
    fn a_regex_extractor_becomes_a_binding() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: status
        status: [200]
    extractors:
      - type: regex
        name: token
        regex:
          - "tok=([a-f0-9]+)"
"#,
        )
        .expect("import");
        assert_eq!(out.flow[0].extract.get("token").unwrap(), "regex:tok=([a-f0-9]+)");
    }

    #[test]
    fn a_dsl_matcher_is_refused_not_lowered_to_a_script() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: dsl
        dsl:
          - "len(body) > 10"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no data-only equivalent"), "{error}");
    }

    #[test]
    fn a_header_part_word_is_refused_rather_than_mismapped() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        part: header
        words:
          - Server
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("only `body` imports"), "{error}");
    }

    #[test]
    fn a_path_with_an_unmodelled_variable_is_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/{{path}}"]
    matchers:
      - type: status
        status: [200]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not model"), "{error}");
    }

    #[test]
    fn multiple_requests_become_multiple_steps() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/a"]
    matchers:
      - type: status
        status: [200]
  - path: ["{{BaseURL}}/b"]
    matchers:
      - type: status
        status: [404]
"#,
        )
        .expect("import");
        assert_eq!(out.flow.len(), 2);
        assert_eq!(out.flow[0].send, "req_1");
        assert_eq!(out.flow[1].send, "req_2");
        assert_eq!(out.requests.get("req_2").unwrap().path, "/b");
    }
}
