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
    /// Nuclei's executable protocols. These are not read; their presence is the
    /// point. A template that runs code (`code`), a browser (`headless`),
    /// JavaScript (`javascript`, and `flow`, which is a JS orchestration block)
    /// is code, not data, and importing "just its http part" would quietly drop
    /// the behavior the template is actually about. F9: a recipe is data or it is
    /// refused; it is never silently reduced to data.
    #[serde(default)]
    code: Option<serde_yaml::Value>,
    #[serde(default)]
    javascript: Option<serde_yaml::Value>,
    #[serde(default)]
    headless: Option<serde_yaml::Value>,
    #[serde(default)]
    flow: Option<serde_yaml::Value>,
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
    /// A raw HTTP request: request line, headers, blank line, body. A single one
    /// is parsed into a request template; more than one in a block is a
    /// multi-request chain whose per-response matcher semantics do not map to one
    /// verdict, so it is refused.
    #[serde(default)]
    raw: Vec<String>,
    /// Nuclei's `unsafe: true`: send the bytes exactly, malformed on purpose (CRLF
    /// injection, smuggling). A request template cannot preserve that, so a raw
    /// request marked unsafe is refused rather than silently normalised.
    #[serde(default, rename = "unsafe")]
    unsafe_request: bool,
    /// Nuclei's `req-condition: true`: the matchers read across the requests'
    /// responses (`body_1`, `status_code_2`). This makes a multi-request block a
    /// stateful chain rather than a list of variants.
    #[serde(default, rename = "req-condition")]
    req_condition: bool,
    #[serde(default, rename = "matchers-condition")]
    matchers_condition: Option<String>,
    #[serde(default)]
    matchers: Vec<Matcher>,
    #[serde(default)]
    extractors: Vec<Extractor>,
    /// Named payload lists for a fuzzing sweep. A value may be an inline list;
    /// a file reference is not portable and is refused.
    #[serde(default)]
    payloads: BTreeMap<String, serde_yaml::Value>,
    /// `batteringram` | `pitchfork` | `clusterbomb`.
    #[serde(default)]
    attack: Option<String>,
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
    dsl: Vec<String>,
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
    /// Nuclei's `internal: true`: this value is reused by a later request. Only
    /// these become bindings. An extractor without it is an output extractor:
    /// cosmetic, shown in results, and irrelevant to the verdict, so it is
    /// dropped rather than allowed to block the import.
    #[serde(default)]
    internal: bool,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    variants: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    extract: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expect: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sweep: Option<SweepOut>,
}

impl StepOut {
    fn send(id: String) -> Self {
        StepOut {
            send: id,
            variants: Vec::new(),
            extract: BTreeMap::new(),
            expect: None,
            sweep: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct SweepOut {
    payloads: BTreeMap<String, Vec<String>>,
    attack: String,
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
    for (name, present) in [
        ("code", template.code.is_some()),
        ("javascript", template.javascript.is_some()),
        ("headless", template.headless.is_some()),
        ("flow", template.flow.is_some()),
    ] {
        if present {
            anyhow::bail!(
                "template `{}` has a `{name}` block: it runs code, so it cannot be imported as \
                 data. Importing only its http part would drop what the template does",
                template.id
            );
        }
    }

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

        if entry.unsafe_request {
            anyhow::bail!(
                "template `{}` request {n} is an unsafe raw request, whose exact malformed \
                 bytes a request template cannot preserve",
                template.id
            );
        }
        // A raw block with several requests is a chain: emit each as a request,
        // then either "match if any" (a variant list) or a verdict that reads
        // across them (`req-condition`).
        if entry.raw.len() > 1 {
            handle_multi_raw(&template.id, n, &entry, &mut requests, &mut flow)?;
            continue;
        }

        // A request's method, path, headers and body come from either a raw
        // request or the structured fields; from there on the two are identical.
        let (method, path_spec, headers, body) = if !entry.raw.is_empty() {
            let parsed = parse_raw(&entry.raw[0]).map_err(|e| {
                anyhow::anyhow!("template `{}` request {n} raw request {e}", template.id)
            })?;
            (parsed.method, parsed.path, parsed.headers, parsed.body)
        } else {
            let path = entry
                .path
                .first()
                .ok_or_else(|| anyhow::anyhow!("template `{}` request {n} has no path", template.id))?
                .clone();
            (
                entry.method.clone().unwrap_or_else(|| "GET".to_string()),
                path,
                entry.headers,
                entry.body,
            )
        };

        // A fuzzing sweep: turn Nuclei's `{{name}}`/`§name§` payload markers into
        // the engine's `${name}`, so the request template takes each value. Only
        // the payload names are rewritten; any other `{{var}}` is left to be
        // refused below, because the importer cannot resolve it.
        let sweep = build_sweep(&template.id, n, &entry.payloads, &entry.attack)?;
        let (path_spec, headers, body) = if let Some(sweep) = &sweep {
            let names: Vec<&str> = sweep.payloads.keys().map(String::as_str).collect();
            let path_spec = rewrite_markers(&path_spec, &names);
            let headers = headers
                .into_iter()
                .map(|(k, v)| (k, rewrite_markers(&v, &names)))
                .collect::<BTreeMap<_, _>>();
            let body = body.map(|b| rewrite_markers(&b, &names));
            (path_spec, headers, body)
        } else {
            (path_spec, headers, body)
        };

        let path = relative_path(&path_spec).ok_or_else(|| {
            anyhow::anyhow!(
                "template `{}` request {n} path `{path_spec}` uses a Nuclei variable or absolute \
                 URL this importer does not model; only a `{{{{BaseURL}}}}`-relative path imports",
                template.id
            )
        })?;
        for (name, value) in &headers {
            if has_interpolation(value) {
                anyhow::bail!(
                    "template `{}` request {n} header `{name}` uses a Nuclei variable this \
                     importer does not model",
                    template.id
                );
            }
        }
        if let Some(body) = &body
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
                method,
                path,
                headers,
                body,
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

        // A sweep needs a verdict to hold for at least one payload.
        if sweep.is_some() && expect.is_none() {
            anyhow::bail!(
                "template `{}` request {n} has payloads but no matcher, so a fuzz sweep would \
                 conclude nothing",
                template.id
            );
        }
        flow.push(StepOut {
            send: id,
            variants: Vec::new(),
            extract,
            expect,
            sweep,
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

/// Build a sweep from a template's payloads, or `None` when there are none.
fn build_sweep(
    template: &str,
    n: usize,
    payloads: &BTreeMap<String, serde_yaml::Value>,
    attack: &Option<String>,
) -> anyhow::Result<Option<SweepOut>> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let mut lists = BTreeMap::new();
    for (name, value) in payloads {
        let list = match value {
            serde_yaml::Value::Sequence(items) => items
                .iter()
                .map(|item| match item {
                    serde_yaml::Value::String(s) => Ok(s.clone()),
                    serde_yaml::Value::Number(num) => Ok(num.to_string()),
                    serde_yaml::Value::Bool(b) => Ok(b.to_string()),
                    _ => Err(anyhow::anyhow!(
                        "template `{template}` request {n} payload `{name}` has a non-scalar value"
                    )),
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            // A string payload is a wordlist file path, which is not portable.
            serde_yaml::Value::String(_) => anyhow::bail!(
                "template `{template}` request {n} payload `{name}` is a file reference, which \
                 does not travel with the template"
            ),
            _ => anyhow::bail!(
                "template `{template}` request {n} payload `{name}` is not a list of values"
            ),
        };
        lists.insert(name.clone(), list);
    }
    // Nuclei's default attack is batteringram, which takes one list. With more
    // than one list and no declared attack the intent is ambiguous, so it is
    // refused rather than guessed.
    let attack = match attack.as_deref() {
        Some("batteringram") | Some("pitchfork") | Some("clusterbomb") => {
            attack.clone().unwrap()
        }
        Some(other) => anyhow::bail!(
            "template `{template}` request {n} uses attack `{other}`, which has no equivalent"
        ),
        None if lists.len() == 1 => "batteringram".to_string(),
        None => anyhow::bail!(
            "template `{template}` request {n} has several payloads and no `attack`, so how they \
             combine is ambiguous"
        ),
    };
    Ok(Some(SweepOut {
        payloads: lists,
        attack,
    }))
}

/// Rewrite `{{name}}` and `§name§` payload markers to the engine's `${name}`,
/// for the given payload names only.
fn rewrite_markers(text: &str, names: &[&str]) -> String {
    let mut out = text.to_string();
    for name in names {
        out = out.replace(&format!("{{{{{name}}}}}"), &format!("${{{name}}}"));
        out = out.replace(&format!("\u{a7}{name}\u{a7}"), &format!("${{{name}}}"));
    }
    out
}

/// What a single raw HTTP request parses into.
struct RawParsed {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
}

/// Parse one Nuclei raw request: an optional run of `@directive` lines, a request
/// line, headers, a blank line, then a body.
///
/// The `Host` header is dropped because the engine sets it from the target, and
/// `Content-Length` because the engine recomputes it; carrying either literally
/// would send a request that describes a different host or a wrong length.
fn parse_raw(raw: &str) -> anyhow::Result<RawParsed> {
    let mut lines = raw.lines().map(|l| l.strip_suffix('\r').unwrap_or(l));

    // The request line is the first line that is neither blank nor an @directive.
    let request_line = loop {
        match lines.next() {
            Some(line) if line.trim().is_empty() => continue,
            Some(line) if line.trim_start().starts_with('@') => continue,
            Some(line) => break line,
            None => anyhow::bail!("is empty"),
        }
    };
    let mut fields = request_line.split_whitespace();
    let method = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("has no method"))?
        .to_string();
    let rest: Vec<&str> = fields.collect();
    // Drop a trailing `HTTP/x.y`; whatever is between method and version is the
    // path (joined, so a stray space in a malformed path survives rather than
    // being silently cut).
    let path = if rest.last().is_some_and(|t| t.starts_with("HTTP/")) {
        rest[..rest.len() - 1].join(" ")
    } else {
        rest.join(" ")
    };
    if path.is_empty() {
        anyhow::bail!("has no path");
    }

    let mut headers = BTreeMap::new();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break; // end of headers; the body follows
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("has a header line without a colon: `{line}`"))?;
        let name = name.trim();
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        headers.insert(name.to_string(), value.trim().to_string());
    }

    let body: String = lines.collect::<Vec<_>>().join("\n");
    let body = body.trim_end_matches('\n').to_string();
    let body = if body.is_empty() { None } else { Some(body) };

    Ok(RawParsed {
        method,
        path,
        headers,
        body,
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
    // An absolute URL in the request line names its own host, which a
    // target-relative request template cannot honour.
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
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

/// Import a raw block that holds several requests: each becomes a request, and
/// the block becomes either a variant list (match if any) or, when it reads
/// across the responses, a chain with one cross-request verdict on the last step.
fn handle_multi_raw(
    template: &str,
    n: usize,
    entry: &HttpRequest,
    requests: &mut BTreeMap<String, RequestOut>,
    flow: &mut Vec<StepOut>,
) -> anyhow::Result<()> {
    if !entry.payloads.is_empty() {
        anyhow::bail!(
            "template `{template}` request {n} combines a payload sweep with a multi-request \
             raw block, which this importer does not model"
        );
    }
    let mut ids = Vec::new();
    for (k, raw) in entry.raw.iter().enumerate() {
        let id = format!("req_{n}_{}", k + 1);
        let parsed = parse_raw(raw)
            .map_err(|e| anyhow::anyhow!("template `{template}` request {n}.{} raw {e}", k + 1))?;
        let path = relative_path(&parsed.path).ok_or_else(|| {
            anyhow::anyhow!(
                "template `{template}` request {n}.{} path `{}` uses a Nuclei variable this \
                 importer does not model",
                k + 1,
                parsed.path
            )
        })?;
        for value in parsed.headers.values() {
            if has_interpolation(value) {
                anyhow::bail!(
                    "template `{template}` request {n}.{} header uses a Nuclei variable",
                    k + 1
                );
            }
        }
        if let Some(body) = &parsed.body
            && has_interpolation(body)
        {
            anyhow::bail!(
                "template `{template}` request {n}.{} body uses a Nuclei variable",
                k + 1
            );
        }
        requests.insert(
            id.clone(),
            RequestOut {
                method: parsed.method,
                path,
                headers: parsed.headers,
                body: parsed.body,
            },
        );
        ids.push(id);
    }

    if entry.matchers.is_empty() {
        anyhow::bail!(
            "template `{template}` request {n} is a multi-request block with no matcher"
        );
    }

    // A chain whose matchers read another request's response (`req-condition`, or
    // an indexed part or dsl variable) is one verdict over the whole flow; a
    // block that does not is a list of variants, matched if any one holds.
    let is_chain = entry.req_condition || references_other_response(entry);
    let verdict = if is_chain {
        matchers_to_dsl(template, n, &entry.matchers, &entry.matchers_condition)?
    } else {
        matchers_to_expect(template, n, &entry.matchers, &entry.matchers_condition)?
            .ok_or_else(|| anyhow::anyhow!("template `{template}` request {n} has no verdict"))?
    };
    serde_json::from_value::<h5i_wire::Expect>(verdict.clone()).map_err(|e| {
        anyhow::anyhow!("template `{template}` request {n} produced an invalid verdict: {e}")
    })?;

    if is_chain {
        // Send each request as its own step; the last carries the verdict, which
        // reads `body_1`, `body_2`, … across the responses.
        let last = ids.len() - 1;
        for (k, id) in ids.into_iter().enumerate() {
            let mut step = StepOut::send(id);
            if k == last {
                step.expect = Some(verdict.clone());
            }
            flow.push(step);
        }
    } else {
        // One step that tries each request; the verdict holds if any one matches.
        let mut ids = ids.into_iter();
        let mut step = StepOut::send(ids.next().expect("at least two requests"));
        step.variants = ids.collect();
        step.expect = Some(verdict);
        flow.push(step);
    }
    Ok(())
}

/// Whether any matcher reads another request's response: an indexed `part`
/// (`body_2`) or a dsl expression naming an indexed variable.
fn references_other_response(entry: &HttpRequest) -> bool {
    entry.matchers.iter().any(|m| {
        m.part.as_deref().is_some_and(is_indexed_part)
            || m.dsl.iter().any(|expr| dsl_names_index(expr))
    })
}

/// A rough check for a `<name>_<digits>` token in a dsl expression, enough to
/// tell a cross-request matcher from a single-response one.
fn dsl_names_index(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if w[0] == b'_' && w[1].is_ascii_digit() {
            // Preceded by a letter, so it is `body_2`, not a bare `_2`.
            if i > 0 && (bytes[i - 1].is_ascii_alphanumeric()) {
                return true;
            }
        }
    }
    false
}

/// Lower a set of matchers to one `dsl` leaf, so a cross-request verdict can name
/// `body_1`, `status_code_2` and so on through the shared evaluator.
fn matchers_to_dsl(
    template: &str,
    n: usize,
    matchers: &[Matcher],
    condition: &Option<String>,
) -> anyhow::Result<Value> {
    let mut parts = Vec::new();
    for matcher in matchers {
        parts.push(matcher_to_dsl(template, n, matcher)?);
    }
    let joiner = match condition.as_deref().unwrap_or("or") {
        "and" => " && ",
        "or" => " || ",
        other => anyhow::bail!("template `{template}` request {n} uses condition `{other}`"),
    };
    let expr = parts
        .iter()
        .map(|p| format!("({p})"))
        .collect::<Vec<_>>()
        .join(joiner);
    Ok(json!({ "dsl": expr }))
}

fn matcher_to_dsl(template: &str, n: usize, matcher: &Matcher) -> anyhow::Result<String> {
    let joiner = match matcher.condition.as_deref().unwrap_or("or") {
        "and" => " && ",
        "or" => " || ",
        other => anyhow::bail!("template `{template}` request {n} uses condition `{other}`"),
    };
    let expr = match matcher.kind.as_str() {
        "status" => matcher
            .status
            .iter()
            .map(|s| format!("status_code == {s}"))
            .collect::<Vec<_>>()
            .join(" || "),
        "word" => {
            let var = part_to_var(template, n, &matcher.part)?;
            matcher
                .words
                .iter()
                .map(|w| format!("contains({var}, '{}')", esc(w)))
                .collect::<Vec<_>>()
                .join(joiner)
        }
        "regex" => {
            let var = part_to_var(template, n, &matcher.part)?;
            matcher
                .regex
                .iter()
                .map(|r| format!("regex('{}', {var})", esc(r)))
                .collect::<Vec<_>>()
                .join(joiner)
        }
        "dsl" => {
            for expr in &matcher.dsl {
                refuse_byte_exact_dsl(template, n, expr)?;
            }
            matcher
                .dsl
                .iter()
                .map(|e| format!("({e})"))
                .collect::<Vec<_>>()
                .join(joiner)
        }
        other => anyhow::bail!(
            "template `{template}` request {n} uses a `{other}` matcher in a chain, which has no \
             data-only equivalent"
        ),
    };
    if expr.is_empty() {
        anyhow::bail!("template `{template}` request {n} has an empty matcher");
    }
    let wrapped = format!("({expr})");
    Ok(if matcher.negative {
        format!("!{wrapped}")
    } else {
        wrapped
    })
}

/// Map a Nuclei matcher `part` to a dsl variable, carrying a `_N` request index.
fn part_to_var(template: &str, n: usize, part: &Option<String>) -> anyhow::Result<String> {
    let part = part.as_deref().unwrap_or("body");
    let (base, index) = match part.rsplit_once('_') {
        Some((base, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (base, Some(tail))
        }
        _ => (part, None),
    };
    let var_base = match base {
        "body" => "body",
        "header" | "all" | "response" => "all_headers",
        "content_type" => "content_type",
        "location" => "location",
        "server" => "server",
        "set_cookie" => "set_cookie",
        other => anyhow::bail!(
            "template `{template}` request {n} matches part `{other}`, which has no dsl variable"
        ),
    };
    Ok(match index {
        Some(i) => format!("{var_base}_{i}"),
        None => var_base.to_string(),
    })
}

/// Escape a literal for embedding in a dsl single-quoted string.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
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

/// Refuse a dsl expression that hashes binary content. `mmh3` and `base64_py`
/// are the favicon-hash idiom, taken over the raw response bytes; the test
/// runtime reads a body as text (lossy for binary), so the hash would be wrong.
/// Refusing is honest where a silent misjudgement would not be. Text hashes
/// (`md5`, `sha256`) stay: a text body survives the round trip exactly.
fn refuse_byte_exact_dsl(template: &str, n: usize, expr: &str) -> anyhow::Result<()> {
    for func in ["mmh3", "base64_py"] {
        if expr.contains(func) {
            anyhow::bail!(
                "template `{template}` request {n} hashes the response bytes with `{func}` \
                 (a favicon hash); the test runtime reads a body as text, so the hash would \
                 not match"
            );
        }
    }
    Ok(())
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
            let leaves: Vec<Value> = matcher
                .words
                .iter()
                .map(|w| part_leaf(template, n, "word", &matcher.part, w, false))
                .collect::<anyhow::Result<_>>()?;
            // Nuclei's default word `condition` is `or`.
            combine_leaves(leaves, matcher.condition.as_deref().unwrap_or("or"), template, n, "word")?
        }
        "regex" => {
            let leaves: Vec<Value> = matcher
                .regex
                .iter()
                .map(|r| part_leaf(template, n, "regex", &matcher.part, r, true))
                .collect::<anyhow::Result<_>>()?;
            combine_leaves(leaves, matcher.condition.as_deref().unwrap_or("or"), template, n, "regex")?
        }
        "dsl" => {
            // Each expression becomes a `dsl` leaf. Validation (below, and the
            // grammar's own parse) refuses an expression that uses a function or
            // variable the bounded evaluator does not implement, so the whole
            // template is refused rather than half-imported.
            for expr in &matcher.dsl {
                refuse_byte_exact_dsl(template, n, expr)?;
            }
            let leaves: Vec<Value> = matcher.dsl.iter().map(|e| json!({ "dsl": e })).collect();
            // Nuclei's default matcher `condition` is `or`, dsl included.
            combine_leaves(leaves, matcher.condition.as_deref().unwrap_or("or"), template, n, "dsl")?
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

/// One word/regex leaf, placed on the part Nuclei named.
///
/// `body` and the unset default read the body. `header` reads the whole header
/// block, `all`/`response` the whole response. Any other part is a specific
/// header name (`content_type` -> `Content-Type`), which maps to the named-header
/// leaf. The parts that do not map are the ones that are not a response at all:
/// `interactsh_*` is an out-of-band callback, and a `_N` suffix names another
/// request's response in a chain this single step does not hold.
fn part_leaf(
    template: &str,
    n: usize,
    kind: &str,
    part: &Option<String>,
    value: &str,
    is_regex: bool,
) -> anyhow::Result<Value> {
    // A word is a literal; a regex leaf carries the `regex:` prefix the grammar
    // reads.
    let spec = if is_regex { format!("regex:{value}") } else { value.to_string() };
    let part = part.as_deref().unwrap_or("body");
    let clause = match part {
        "body" => json!({ "body": spec }),
        "header" => json!({ "headers": spec }),
        "all" | "response" => json!({ "response": spec }),
        other if other.starts_with("interactsh") => anyhow::bail!(
            "template `{template}` request {n} has a {kind} matcher on part `{other}`, an \
             out-of-band callback with no in-response equivalent"
        ),
        other if is_indexed_part(other) => anyhow::bail!(
            "template `{template}` request {n} has a {kind} matcher on part `{other}`, which \
             names another request's response in a chain"
        ),
        header_name => {
            let name = header_name.replace('_', "-");
            if is_regex {
                json!({ "header": name, "regex": value })
            } else {
                json!({ "header": name, "contains": value })
            }
        }
    };
    Ok(clause)
}

/// A `_N` suffix (`body_2`, `header_3`) names the Nth response in a multi-request
/// chain, not a header called `body-2`.
fn is_indexed_part(part: &str) -> bool {
    part.rsplit_once('_')
        .is_some_and(|(_, tail)| !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()))
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
        // An output extractor does not feed a later step and is not part of the
        // verdict, so it is dropped: it must not block an import or clutter the
        // test. Only an `internal` extractor becomes a binding.
        if !extractor.internal {
            continue;
        }
        if extractor.kind != "regex" {
            anyhow::bail!(
                "template `{template}` request {n} has an internal `{}` extractor; only a \
                 `regex` extractor imports as a binding",
                extractor.kind
            );
        }
        if !matches!(extractor.part.as_deref(), None | Some("body")) {
            anyhow::bail!(
                "template `{template}` request {n} has an internal regex extractor on part \
                 `{}`; only `body` imports as a binding",
                extractor.part.as_deref().unwrap_or("")
            );
        }
        let name = extractor.name.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "template `{template}` request {n} has an internal regex extractor with no name"
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
    fn an_internal_regex_extractor_becomes_a_binding() {
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
        internal: true
        regex:
          - "tok=([a-f0-9]+)"
"#,
        )
        .expect("import");
        assert_eq!(out.flow[0].extract.get("token").unwrap(), "regex:tok=([a-f0-9]+)");
    }

    #[test]
    fn output_extractors_are_dropped_not_refused() {
        // An unnamed extractor, and a non-regex one, are output-only: they must
        // not block the import, and they leave no binding behind.
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
        regex: ["version: ([0-9.]+)"]
      - type: kval
        kval: ["server"]
"#,
        )
        .expect("import");
        assert!(out.flow[0].extract.is_empty());
        assert_eq!(out.flow[0].expect.as_ref().unwrap(), &json!({"status": 200}));
    }

    #[test]
    fn a_supported_dsl_matcher_becomes_a_dsl_leaf() {
        let out = convert_yaml(
            r#"
id: git-config
http:
  - path: ["{{BaseURL}}/.git/config"]
    matchers:
      - type: dsl
        condition: and
        dsl:
          - "!contains(tolower(body), '<html')"
          - "status_code == 200"
"#,
        )
        .expect("import");
        assert_eq!(
            out.flow[0].expect.as_ref().unwrap(),
            &json!({"all": [
                {"dsl": "!contains(tolower(body), '<html')"},
                {"dsl": "status_code == 200"},
            ]})
        );
    }

    #[test]
    fn a_dsl_matcher_using_an_unsupported_function_is_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: dsl
        dsl:
          - "unix_time() > 100"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported DSL function"), "{error}");
    }

    #[test]
    fn a_header_part_word_maps_to_the_header_block_leaf() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        part: header
        words:
          - X-Jenkins
"#,
        )
        .expect("import");
        assert_eq!(out.flow[0].expect.as_ref().unwrap(), &json!({"headers": "X-Jenkins"}));
    }

    #[test]
    fn a_named_header_part_maps_to_the_named_header_leaf() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        part: content_type
        words:
          - application/json
"#,
        )
        .expect("import");
        assert_eq!(
            out.flow[0].expect.as_ref().unwrap(),
            &json!({"header": "content-type", "contains": "application/json"})
        );
    }

    #[test]
    fn a_response_part_regex_maps_to_the_response_leaf() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: regex
        part: response
        regex:
          - "Set-Cookie:.*admin"
"#,
        )
        .expect("import");
        assert_eq!(
            out.flow[0].expect.as_ref().unwrap(),
            &json!({"response": "regex:Set-Cookie:.*admin"})
        );
    }

    #[test]
    fn an_interactsh_part_is_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        part: interactsh_protocol
        words:
          - dns
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("out-of-band"), "{error}");
    }

    #[test]
    fn an_indexed_part_is_refused_as_a_chain_reference() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: word
        part: body_2
        words:
          - admin
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("another request's response"), "{error}");
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
    fn an_executable_protocol_is_refused_not_reduced_to_its_http_part() {
        // A template with both http and a code block must not import as if the
        // code were not there.
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: status
        status: [200]
code:
  - engine: [sh]
    source: "id"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("it runs code"), "{error}");
    }

    #[test]
    fn a_flow_block_is_refused() {
        let error = convert_yaml(
            r#"
id: t
flow: "http(1) && http(2)"
http:
  - path: ["{{BaseURL}}/"]
    matchers:
      - type: status
        status: [200]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot be imported as data"), "{error}");
    }

    #[test]
    fn a_single_raw_request_becomes_a_request_template() {
        let out = convert_yaml(
            r#"
id: odoo-detect
info:
  name: Odoo
http:
  - raw:
      - |
        POST /web/webclient/version_info HTTP/1.1
        Host: {{Hostname}}
        Content-Type: application/json
        Content-Length: 2

        {}
    matchers:
      - type: word
        words:
          - server_version
"#,
        )
        .expect("import");
        let req = out.requests.get("req_1").expect("a request");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/web/webclient/version_info");
        // Host and Content-Length are dropped; the engine sets them.
        assert_eq!(req.headers.get("Content-Type").map(String::as_str), Some("application/json"));
        assert!(!req.headers.contains_key("Host"));
        assert!(!req.headers.contains_key("Content-Length"));
        assert_eq!(req.body.as_deref(), Some("{}"));
        assert_eq!(out.flow[0].expect.as_ref().unwrap(), &json!({"body": "server_version"}));
    }

    #[test]
    fn a_raw_request_skips_leading_directives() {
        let out = convert_yaml(
            r#"
id: t
http:
  - raw:
      - |
        @timeout: 20s
        GET /status HTTP/1.1
        Host: {{Hostname}}
    matchers:
      - type: status
        status: [200]
"#,
        )
        .expect("import");
        assert_eq!(out.requests.get("req_1").unwrap().method, "GET");
        assert_eq!(out.requests.get("req_1").unwrap().path, "/status");
    }

    #[test]
    fn a_variant_multi_raw_becomes_one_step_with_variants() {
        // No cross-response reference: try both, match if either holds.
        let out = convert_yaml(
            r#"
id: t
http:
  - raw:
      - |
        POST /x HTTP/1.1
        Host: {{Hostname}}

        file:///etc/passwd
      - |
        POST /x HTTP/1.1
        Host: {{Hostname}}

        file:///c:/windows/win.ini
    stop-at-first-match: true
    matchers-condition: and
    matchers:
      - type: regex
        regex: ["root:.*:0:0:", "for 16-bit app support"]
        condition: or
      - type: status
        status: [200]
"#,
        )
        .expect("import");
        assert_eq!(out.requests.len(), 2);
        assert!(out.requests.contains_key("req_1_1"));
        assert_eq!(out.flow.len(), 1);
        assert_eq!(out.flow[0].send, "req_1_1");
        assert_eq!(out.flow[0].variants, vec!["req_1_2".to_string()]);
        assert!(out.flow[0].expect.is_some());
    }

    #[test]
    fn a_req_condition_multi_raw_becomes_a_chain_with_a_cross_request_verdict() {
        let out = convert_yaml(
            r#"
id: t
http:
  - raw:
      - |
        GET /?lang=../../../etc/passwd HTTP/1.1
        Host: {{Hostname}}
      - |
        GET / HTTP/1.1
        Host: {{Hostname}}
      - |
        GET /?config-create HTTP/1.1
        Host: {{Hostname}}
    matchers-condition: or
    matchers:
      - type: word
        part: body_3
        words: ["CONFIGURATION", "Successfully created"]
        condition: and
"#,
        )
        .expect("import");
        assert_eq!(out.flow.len(), 3);
        // Only the last step carries the verdict.
        assert!(out.flow[0].expect.is_none());
        assert!(out.flow[1].expect.is_none());
        let verdict = out.flow[2].expect.as_ref().unwrap();
        let dsl = verdict.get("dsl").and_then(|v| v.as_str()).expect("a dsl verdict");
        assert!(dsl.contains("contains(body_3, 'CONFIGURATION')"), "{dsl}");
        assert!(dsl.contains("&&"), "{dsl}");
        // The emitted dsl must parse under the shared grammar.
        serde_json::from_value::<h5i_wire::Expect>(verdict.clone()).expect("valid verdict");
    }

    #[test]
    fn an_unsafe_raw_request_is_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - unsafe: true
    raw:
      - |
        GET /../../etc/passwd HTTP/1.1
        Host: {{Hostname}}
    matchers:
      - type: status
        status: [200]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsafe raw request"), "{error}");
    }

    #[test]
    fn a_raw_request_with_a_payload_variable_in_the_path_is_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - raw:
      - |
        GET /search?q={{payload}} HTTP/1.1
        Host: {{Hostname}}
    matchers:
      - type: status
        status: [200]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not model"), "{error}");
    }

    #[test]
    fn payloads_become_a_sweep_and_markers_become_bindings() {
        let out = convert_yaml(
            r#"
id: t
http:
  - method: GET
    path: ["{{BaseURL}}/item?id={{inj}}"]
    payloads:
      inj:
        - "1'"
        - "1 OR 1=1"
    attack: pitchfork
    matchers:
      - type: word
        words: ["SQL syntax"]
"#,
        )
        .expect("import");
        assert_eq!(out.requests.get("req_1").unwrap().path, "/item?id=${inj}");
        let sweep = out.flow[0].sweep.as_ref().expect("a sweep");
        assert_eq!(sweep.attack, "pitchfork");
        assert_eq!(sweep.payloads.get("inj").unwrap(), &vec!["1'".to_string(), "1 OR 1=1".to_string()]);
    }

    #[test]
    fn a_single_payload_defaults_to_batteringram() {
        let out = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/?q={{p}}"]
    payloads:
      p: ["a", "b"]
    matchers:
      - type: status
        status: [500]
"#,
        )
        .expect("import");
        assert_eq!(out.flow[0].sweep.as_ref().unwrap().attack, "batteringram");
    }

    #[test]
    fn a_file_payload_is_refused_as_unportable() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/?q={{p}}"]
    payloads:
      p: "helpers/wordlists/sqli.txt"
    matchers:
      - type: status
        status: [500]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not travel"), "{error}");
    }

    #[test]
    fn payloads_without_a_matcher_are_refused() {
        let error = convert_yaml(
            r#"
id: t
http:
  - path: ["{{BaseURL}}/?q={{p}}"]
    payloads:
      p: ["a"]
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("conclude nothing"), "{error}");
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
