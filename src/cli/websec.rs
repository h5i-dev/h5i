//! Reading a session's stored messages, and comparing two of them.
//!
//! The half of the workbench that does not touch the wire. `resend` sends;
//! these two verbs read what was sent and say how two answers differ, which is
//! what an agent spends most of its turns on.
//!
//! Read here, on h5i's side, rather than through a session verb, and that is a
//! security decision rather than a convenience. The store holds `Authorization`
//! headers and session cookies in full. A verb's reply travels out through the
//! renderer, which is the process that parses untrusted pages and is the one
//! half of the engine deliberately kept away from credentials. Asking it to
//! relay a stored credential would undo, on request, exactly what the broker
//! split is for. The files are h5i's to read, so h5i reads them.
//!
//! The cost is honest and already precedented: a boxed session whose filesystem
//! this machine cannot see answers "this machine cannot read that store" rather
//! than pretending the session captured nothing. `browser_session::Sources`
//! makes the same distinction about the same directory.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use h5i_browser::capture::{Body, StoredRequest, StoredResponse};
use h5i_core::browser_session as bs;
use serde_json::{json, Value};

/// Which half of a message to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Request,
    Response,
    Both,
}

/// Where a session's messages are, or why they cannot be read.
fn store_dir(root: &Path, selector: Option<&str>) -> anyhow::Result<(bs::Session, PathBuf)> {
    let session = match bs::resolve(root, selector) {
        Ok(session) => session,
        Err(bs::SessionGone::Ended { id, .. }) => bs::read(root, &id)?,
        Err(gone) => anyhow::bail!("{gone}"),
    };
    let dir = bs::dir(root, &session.id).join(bs::MESSAGES_DIR);
    if !dir.exists() {
        let placed = match &session.placement {
            bs::Placement::Box { name } => format!(
                "\n\n  This session runs in box `{name}`. If that box keeps its /tmp inside \
                 its image, its store is not on a filesystem this machine can read, and \
                 nothing here is missing."
            ),
            bs::Placement::Host => String::new(),
        };
        anyhow::bail!(
            "session {} kept no messages: it was opened without `--capture`, so only the \
             request log exists. `h5i browser requests` shows what it sent; \
             `h5i browser open <url> --capture` starts one that also keeps the messages.{placed}",
            session.id
        );
    }
    Ok((session, dir))
}

/// Every sequence number the store holds, in order.
fn sequences(dir: &Path) -> Vec<u64> {
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some((seq, _)) = name.split_once('.')
                && let Ok(seq) = seq.parse::<u64>()
            {
                seen.insert(seq);
            }
        }
    }
    seen.into_iter().collect()
}

fn read_json<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("{} could not be read: {e}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// A body, as text where it is text.
///
/// Never a lossy string: a body that is not UTF-8 is reported as its size and
/// its hash rather than as mojibake, because a workbench that silently mangled
/// a binary body would produce a diff nobody could trust.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// Decoded, and safe to compare line by line.
    Utf8(String),
    /// There, and not text.
    Binary { bytes: u64, sha256: String },
    /// Not in the store, and why.
    Missing(String),
}

impl Text {
    fn as_str(&self) -> &str {
        match self {
            Text::Utf8(text) => text,
            _ => "",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Text::Utf8(text) => json!({"kind": "text", "text": text}),
            Text::Binary { bytes, sha256 } => {
                json!({"kind": "binary", "bytes": bytes, "sha256": sha256})
            }
            Text::Missing(why) => json!({"kind": "absent", "why": why}),
        }
    }
}

/// Pull a body out of the store.
fn body_text(dir: &Path, body: &Body) -> Text {
    match body {
        Body::Empty => Text::Utf8(String::new()),
        Body::Skipped { reason, bytes } => Text::Missing(match (reason, bytes) {
            (reason, Some(bytes)) => format!("{reason:?} ({bytes} bytes)").to_lowercase(),
            (reason, None) => format!("{reason:?}").to_lowercase(),
        }),
        Body::Stored { sha256, bytes, .. } => {
            let path = dir.join("bodies").join(sha256);
            match std::fs::read(&path) {
                Err(e) => Text::Missing(format!("the stored body could not be read: {e}")),
                Ok(raw) => match String::from_utf8(raw) {
                    Ok(text) => Text::Utf8(text),
                    Err(_) => Text::Binary {
                        bytes: *bytes,
                        sha256: sha256.clone(),
                    },
                },
            }
        }
    }
}

/// Render a request the way it went out.
fn raw_request(stored: &StoredRequest, body: &Text) -> String {
    let mut out = String::new();
    let target = url::Url::parse(&stored.url)
        .map(|u| {
            let mut target = u.path().to_string();
            if let Some(query) = u.query() {
                target.push('?');
                target.push_str(query);
            }
            target
        })
        .unwrap_or_else(|_| stored.url.clone());
    out.push_str(&format!("{} {} HTTP/1.1\n", stored.method, target));
    if let Ok(url) = url::Url::parse(&stored.url)
        && let Some(host) = url.host_str()
    {
        // The client computes this one, so it is not in the stored set; showing
        // the message without it would be showing something that is not a
        // request.
        match url.port() {
            Some(port) => out.push_str(&format!("host: {host}:{port}\n")),
            None => out.push_str(&format!("host: {host}\n")),
        }
    }
    for (name, value) in &stored.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push('\n');
    push_body(&mut out, body);
    out
}

/// Render a response the way it arrived.
fn raw_response(stored: &StoredResponse, body: &Text) -> String {
    let mut out = String::new();
    match stored.status {
        Some(status) => out.push_str(&format!("HTTP/1.1 {status}\n")),
        None => out.push_str("HTTP/1.1 (no status: the request did not complete)\n"),
    }
    for (name, value) in &stored.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push('\n');
    push_body(&mut out, body);
    out
}

fn push_body(out: &mut String, body: &Text) {
    match body {
        Text::Utf8(text) => out.push_str(text),
        Text::Binary { bytes, sha256 } => {
            out.push_str(&format!("[{bytes} bytes, not text — sha256 {sha256}]"));
        }
        Text::Missing(why) => out.push_str(&format!("[no body: {why}]")),
    }
}

/// `h5i browser message <seq>`.
pub fn show(
    root: &Path,
    selector: Option<&str>,
    seq: u64,
    part: Part,
    raw: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let (session, dir) = store_dir(root, selector)?;

    let request: Option<StoredRequest> = match part {
        Part::Response => None,
        _ => Some(read_json(&dir.join(format!("{seq}.request.json"))).map_err(|_| {
            let have = sequences(&dir);
            anyhow::anyhow!(
                "session {} has no stored request {seq}. It holds: {}",
                session.id,
                if have.is_empty() {
                    "nothing yet".to_string()
                } else {
                    have.iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        })?),
    };
    let response: Option<StoredResponse> = match part {
        Part::Request => None,
        // A request with no response is a real state: the connection failed, or
        // the process died mid-fetch. Reported as absence rather than as an
        // error, because the request half is still the evidence.
        _ => read_json(&dir.join(format!("{seq}.response.json"))).ok(),
    };

    let request_body = request.as_ref().map(|r| body_text(&dir, &r.body));
    let response_body = response.as_ref().map(|r| body_text(&dir, &r.body));

    if json_out {
        let mut value = json!({"seq": seq, "session": session.id});
        if let (Some(request), Some(body)) = (&request, &request_body) {
            value["request"] = json!({
                "at": request.at,
                "method": request.method,
                "url": request.url,
                "headers": request.headers,
                "body": body.to_json(),
            });
        }
        if let (Some(response), Some(body)) = (&response, &response_body) {
            value["response"] = json!({
                "at": response.at,
                "url": response.url,
                "status": response.status,
                "headers": response.headers,
                "content_encoding": response.content_encoding,
                "wire_bytes": response.wire_bytes,
                "body": body.to_json(),
            });
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if let (Some(request), Some(body)) = (&request, &request_body) {
        if raw {
            print!("{}", raw_request(request, body));
        } else {
            println!("  request  : {} {}", request.method, request.url);
            println!("  at       : {}", request.at);
            for (name, value) in &request.headers {
                println!("    {name}: {value}");
            }
            summarise_body(body);
        }
        if matches!(part, Part::Both) {
            println!();
        }
    }
    if let (Some(response), Some(body)) = (&response, &response_body) {
        if raw {
            print!("{}", raw_response(response, body));
        } else {
            match response.status {
                Some(status) => println!("  response : {status}"),
                None => println!("  response : (none: the request did not complete)"),
            }
            for (name, value) in &response.headers {
                println!("    {name}: {value}");
            }
            summarise_body(body);
        }
    } else if matches!(part, Part::Response) {
        println!("  response : not stored. The request half is at `--part request`.");
    }
    Ok(())
}

fn summarise_body(body: &Text) {
    match body {
        Text::Utf8(text) if text.is_empty() => println!("  body     : empty"),
        Text::Utf8(text) => {
            println!("  body     : {} bytes", text.len());
            for line in text.lines().take(20) {
                println!("    {line}");
            }
            if text.lines().count() > 20 {
                println!("    … {} more lines", text.lines().count() - 20);
            }
        }
        Text::Binary { bytes, sha256 } => {
            println!("  body     : {bytes} bytes, not text (sha256 {sha256})")
        }
        Text::Missing(why) => println!("  body     : not stored ({why})"),
    }
}

/// How two responses differ, in the layers an agent branches on.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Difference {
    /// Nothing differs: same status, same headers that matter, same body.
    pub same: bool,
    pub status: (Option<u16>, Option<u16>),
    pub status_changed: bool,
    /// Body length, and the difference between them.
    pub bytes: (u64, u64),
    pub length_delta: i64,
    /// 0.0 to 1.0 over the body. The number a blind-injection loop thresholds
    /// on, and the reason this verb is not just a printed diff: reading two
    /// HTML pages per candidate character through a model is the expensive way
    /// to answer "true page or false page".
    pub similarity: f64,
    pub headers_added: Vec<String>,
    pub headers_removed: Vec<String>,
    pub headers_changed: Vec<String>,
    /// Changed body fields, when both bodies are JSON. Keyed by dotted path.
    pub json_changes: Vec<JsonChange>,
    /// Changed lines, when they are not.
    pub line_changes: Vec<LineChange>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JsonChange {
    pub path: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LineChange {
    pub side: &'static str,
    pub line: usize,
    pub text: String,
}

/// Headers whose values differ on every response and mean nothing.
///
/// Comparing them makes every pair of responses "different", which is the same
/// as making the verb useless. Named rather than guessed at, so a header that
/// matters is never dropped for looking noisy.
fn header_is_noise(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "date" | "age" | "expires" | "last-modified" | "x-request-id" | "x-trace-id"
    )
}

/// How many lines to carry out of a body diff.
const MAX_LINE_CHANGES: usize = 60;

/// How many JSON fields to name.
const MAX_JSON_CHANGES: usize = 60;

/// Compare two stored responses.
pub fn compare(left: (&StoredResponse, &Text), right: (&StoredResponse, &Text)) -> Difference {
    let (a, a_body) = left;
    let (b, b_body) = right;

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let find = |set: &[(String, String)], name: &str| {
        set.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    for (name, value) in &b.headers {
        if header_is_noise(name) {
            continue;
        }
        match find(&a.headers, name) {
            None => added.push(name.clone()),
            Some(before) if &before != value => changed.push(name.clone()),
            Some(_) => {}
        }
    }
    for (name, _) in &a.headers {
        if !header_is_noise(name) && find(&b.headers, name).is_none() {
            removed.push(name.clone());
        }
    }

    let left_text = a_body.as_str();
    let right_text = b_body.as_str();
    let (json_changes, line_changes) = body_changes(a, b, left_text, right_text);

    let bytes = (left_text.len() as u64, right_text.len() as u64);
    Difference {
        same: a.status == b.status
            && added.is_empty()
            && removed.is_empty()
            && changed.is_empty()
            && left_text == right_text,
        status: (a.status, b.status),
        status_changed: a.status != b.status,
        bytes,
        length_delta: bytes.1 as i64 - bytes.0 as i64,
        similarity: similarity(left_text, right_text),
        headers_added: added,
        headers_removed: removed,
        headers_changed: changed,
        json_changes,
        line_changes,
    }
}

fn is_json(response: &StoredResponse) -> bool {
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .is_some_and(|(_, value)| value.to_ascii_lowercase().contains("json"))
}

fn body_changes(
    a: &StoredResponse,
    b: &StoredResponse,
    left: &str,
    right: &str,
) -> (Vec<JsonChange>, Vec<LineChange>) {
    // Both sides have to be JSON *and* parse. A body that claims JSON and is
    // not (a truncated answer, an error page served with the wrong type) falls
    // through to the line diff rather than reporting no changes at all.
    if is_json(a)
        && is_json(b)
        && let (Ok(left), Ok(right)) = (
            serde_json::from_str::<Value>(left),
            serde_json::from_str::<Value>(right),
        )
    {
        let mut changes = Vec::new();
        walk_json("", &left, &right, &mut changes);
        changes.truncate(MAX_JSON_CHANGES);
        return (changes, Vec::new());
    }
    (Vec::new(), line_changes(left, right))
}

/// Field-by-field, so a re-ordered object is not a difference.
fn walk_json(path: &str, left: &Value, right: &Value, out: &mut Vec<JsonChange>) {
    if out.len() >= MAX_JSON_CHANGES {
        return;
    }
    let render = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for name in names {
                let next = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                match (a.get(name), b.get(name)) {
                    (Some(l), Some(r)) => walk_json(&next, l, r, out),
                    (Some(l), None) => out.push(JsonChange {
                        path: next,
                        from: Some(render(l)),
                        to: None,
                    }),
                    (None, Some(r)) => out.push(JsonChange {
                        path: next,
                        from: None,
                        to: Some(render(r)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                let next = format!("{path}.{index}");
                match (a.get(index), b.get(index)) {
                    (Some(l), Some(r)) => walk_json(&next, l, r, out),
                    (Some(l), None) => out.push(JsonChange {
                        path: next,
                        from: Some(render(l)),
                        to: None,
                    }),
                    (None, Some(r)) => out.push(JsonChange {
                        path: next,
                        from: None,
                        to: Some(render(r)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (l, r) if l != r => out.push(JsonChange {
            path: path.to_string(),
            from: Some(render(l)),
            to: Some(render(r)),
        }),
        _ => {}
    }
}

/// Lines present on one side and not the other.
///
/// Set-based rather than a true longest-common-subsequence: what an agent asks
/// of a diff here is "what appeared and what vanished", and a page that moved a
/// line without changing it is not a finding. An LCS would also be O(n·m) over
/// two HTML documents, which is the wrong cost for a loop.
fn line_changes(left: &str, right: &str) -> Vec<LineChange> {
    let before: BTreeSet<&str> = left.lines().collect();
    let after: BTreeSet<&str> = right.lines().collect();
    let mut out = Vec::new();
    for (index, line) in right.lines().enumerate() {
        if !before.contains(line) {
            out.push(LineChange {
                side: "added",
                line: index + 1,
                text: line.chars().take(400).collect(),
            });
        }
    }
    for (index, line) in left.lines().enumerate() {
        if !after.contains(line) {
            out.push(LineChange {
                side: "removed",
                line: index + 1,
                text: line.chars().take(400).collect(),
            });
        }
    }
    out.truncate(MAX_LINE_CHANGES);
    out
}

/// How alike two bodies are, 0.0 to 1.0.
///
/// Token overlap (Jaccard over whitespace-separated tokens), which is cheap,
/// order-insensitive and good enough for the question it answers: is this the
/// same page with a different value in it, or a different page. Identical
/// bodies are 1.0 and two empty bodies are 1.0, because "nothing changed" is
/// the honest answer there.
pub fn similarity(left: &str, right: &str) -> f64 {
    if left == right {
        return 1.0;
    }
    let a: BTreeSet<&str> = left.split_whitespace().collect();
    let b: BTreeSet<&str> = right.split_whitespace().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let shared = a.intersection(&b).count() as f64;
    let total = a.union(&b).count() as f64;
    if total == 0.0 { 0.0 } else { shared / total }
}

/// `h5i browser diff <a> <b>`.
pub fn diff(
    root: &Path,
    selector: Option<&str>,
    left: u64,
    right: u64,
    json_out: bool,
) -> anyhow::Result<()> {
    let (session, dir) = store_dir(root, selector)?;
    let read = |seq: u64| -> anyhow::Result<(StoredResponse, Text)> {
        let stored: StoredResponse = read_json(&dir.join(format!("{seq}.response.json")))
            .map_err(|_| {
                anyhow::anyhow!(
                    "session {} has no stored response {seq}. It holds: {}",
                    session.id,
                    sequences(&dir)
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let body = body_text(&dir, &stored.body);
        Ok((stored, body))
    };
    let (a, a_body) = read(left)?;
    let (b, b_body) = read(right)?;
    let difference = compare((&a, &a_body), (&b, &b_body));

    if json_out {
        println!("{}", serde_json::to_string_pretty(&difference)?);
        return Ok(());
    }

    if difference.same {
        println!("  {left} and {right} are the same response.");
        return Ok(());
    }
    println!(
        "  status   : {} → {}",
        difference.status.0.map_or("none".to_string(), |s| s.to_string()),
        difference.status.1.map_or("none".to_string(), |s| s.to_string()),
    );
    println!(
        "  bytes    : {} → {} ({:+})",
        difference.bytes.0, difference.bytes.1, difference.length_delta
    );
    println!("  alike    : {:.3}", difference.similarity);
    for name in &difference.headers_added {
        println!("  + header : {name}");
    }
    for name in &difference.headers_removed {
        println!("  - header : {name}");
    }
    for name in &difference.headers_changed {
        println!("  ~ header : {name}");
    }
    for change in &difference.json_changes {
        let from = change.from.as_deref().unwrap_or("(absent)");
        let to = change.to.as_deref().unwrap_or("(absent)");
        println!("  ~ {} : {from} → {to}", change.path);
    }
    for change in &difference.line_changes {
        let mark = if change.side == "added" { '+' } else { '-' };
        println!("  {mark} {}", change.text);
    }
    Ok(())
}


/// The exit code a matcher answers with when nothing matched.
///
/// `grep`'s convention, not the sysexits scheme the rest of h5i uses for
/// session failures: 0 matched, 1 did not, 2 something went wrong. This verb is
/// a grep, it will be read in `if` and `&&` a thousand times more often than it
/// is read by a person, and a shell author already knows what 1 means from a
/// matcher. A distinct code is the whole point: "did not match" and "could not
/// look" must never be the same answer.
pub const EXIT_NO_MATCH: i32 = 1;

/// The exit code for a question that could not be asked at all.
///
/// A pattern that does not compile, a body that was never stored, a session
/// with no store. Distinct from [`EXIT_NO_MATCH`] on purpose and it is the
/// whole discipline of this verb: a loop that reads "did not match" when the
/// truth is "could not look" draws a conclusion from an absence, which is the
/// one mistake a workbench must not help anyone make.
pub const EXIT_CANNOT_LOOK: i32 = 2;

/// One thing a caller is asking about a response.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// A regular expression over the body.
    Regex(String),
    /// A literal substring of the body. The common case, and it needs no
    /// escaping, which matters when the thing being looked for is a payload.
    Contains(String),
    /// A dotted path into a JSON body, as `edits` spells them. Matches when the
    /// path exists; with a value, when it equals that value.
    Json { path: String, value: Option<String> },
    /// A header, by name, and optionally by value.
    Header { name: String, value: Option<String> },
    /// The status code.
    Status(u16),
    /// The body is longer than this many bytes.
    LongerThan(u64),
    /// ...or shorter.
    ShorterThan(u64),
}

/// What one condition found.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Found {
    pub kind: &'static str,
    pub expr: String,
    pub matched: bool,
    /// What the expression captured, when it captures. A regex hands back its
    /// groups, a JSON path its value, a header its value. This is the half of
    /// the verb that feeds the next request: an agent extracting a CSRF token
    /// is running a match and reading this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<String>,
}

fn evaluate(condition: &Condition, response: &StoredResponse, body: &Text) -> Found {
    let text = body.as_str();
    match condition {
        Condition::Regex(pattern) => match regex::Regex::new(pattern) {
            // A pattern that does not compile is not a response that does not
            // match. It comes back as an unmatched condition carrying the
            // parser's complaint, and `matches` turns that into an error exit
            // rather than a "no".
            Err(e) => Found {
                kind: "regex",
                expr: format!("{pattern} (not a regular expression: {e})"),
                matched: false,
                captures: Vec::new(),
            },
            Ok(re) => {
                let found = re.captures(text);
                Found {
                    kind: "regex",
                    expr: pattern.clone(),
                    matched: found.is_some(),
                    captures: found
                        .map(|caps| {
                            caps.iter()
                                .skip(1)
                                .flatten()
                                .map(|m| m.as_str().to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            }
        },
        Condition::Contains(needle) => Found {
            kind: "contains",
            expr: needle.clone(),
            matched: text.contains(needle.as_str()),
            captures: Vec::new(),
        },
        Condition::Json { path, value } => {
            let found = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|document| json_at(&document, path).cloned());
            let rendered = found.as_ref().map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            let matched = match (&rendered, value) {
                (Some(_), None) => true,
                (Some(have), Some(want)) => have == want,
                (None, _) => false,
            };
            Found {
                kind: "json",
                expr: match value {
                    Some(value) => format!("{path}={value}"),
                    None => path.clone(),
                },
                matched,
                captures: rendered.into_iter().collect(),
            }
        }
        Condition::Header { name, value } => {
            let have = response
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone());
            let matched = match (&have, value) {
                (Some(_), None) => true,
                (Some(have), Some(want)) => have == want,
                (None, _) => false,
            };
            Found {
                kind: "header",
                expr: match value {
                    Some(value) => format!("{name}={value}"),
                    None => name.clone(),
                },
                matched,
                captures: have.into_iter().collect(),
            }
        }
        Condition::Status(want) => Found {
            kind: "status",
            expr: want.to_string(),
            matched: response.status == Some(*want),
            captures: response.status.map(|s| s.to_string()).into_iter().collect(),
        },
        Condition::LongerThan(bytes) => Found {
            kind: "longer-than",
            expr: bytes.to_string(),
            matched: text.len() as u64 > *bytes,
            captures: vec![text.len().to_string()],
        },
        Condition::ShorterThan(bytes) => Found {
            kind: "shorter-than",
            expr: bytes.to_string(),
            matched: (text.len() as u64) < *bytes,
            captures: vec![text.len().to_string()],
        },
    }
}

/// Walk a dotted path, the way `edits` does. Kept to the same spelling so a
/// path that names a field for an edit names the same field for a match.
fn json_at<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    let mut at = document;
    for segment in path.trim_start_matches("$.").split('.').filter(|s| !s.is_empty()) {
        at = match at {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(at)
}

/// `h5i browser match <seq>`.
///
/// Every condition has to hold. That is the useful default for the loop this
/// serves ("status 200 *and* the body has the flag"), and an `or` is a second
/// call in a shell that already has `||`.
pub fn matches(
    root: &Path,
    selector: Option<&str>,
    seq: u64,
    conditions: &[Condition],
    json_out: bool,
) -> anyhow::Result<()> {
    // The three answers are kept apart here rather than by the caller: `look`
    // says yes or no, and anything that stopped it from looking arrives as an
    // error and leaves by a different door.
    match look(root, selector, seq, conditions, json_out) {
        Ok(true) => Ok(()),
        Ok(false) => std::process::exit(EXIT_NO_MATCH),
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(EXIT_CANNOT_LOOK)
        }
    }
}

fn look(
    root: &Path,
    selector: Option<&str>,
    seq: u64,
    conditions: &[Condition],
    json_out: bool,
) -> anyhow::Result<bool> {
    if conditions.is_empty() {
        anyhow::bail!(
            "match needs something to look for: --regex, --contains, --json, --header, \
             --status, --longer-than or --shorter-than"
        );
    }
    let (session, dir) = store_dir(root, selector)?;
    let stored: StoredResponse =
        read_json(&dir.join(format!("{seq}.response.json"))).map_err(|_| {
            anyhow::anyhow!(
                "session {} has no stored response {seq}. It holds: {}",
                session.id,
                sequences(&dir)
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let body = body_text(&dir, &stored.body);

    // A body that was never stored cannot be matched against, and answering
    // "no" would be a claim about the response rather than about the store.
    if let Text::Missing(why) = &body
        && conditions.iter().any(|c| {
            matches!(
                c,
                Condition::Regex(_)
                    | Condition::Contains(_)
                    | Condition::Json { .. }
                    | Condition::LongerThan(_)
                    | Condition::ShorterThan(_)
            )
        })
    {
        anyhow::bail!(
            "response {seq}'s body is not in the store ({why}), so a body condition cannot \
             be answered. Header and status conditions still can"
        );
    }

    let found: Vec<Found> = conditions
        .iter()
        .map(|condition| evaluate(condition, &stored, &body))
        .collect();
    let bad_pattern = found
        .iter()
        .any(|f| f.kind == "regex" && f.expr.contains("not a regular expression"));
    let matched = found.iter().all(|f| f.matched);

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "seq": seq,
                "matched": matched,
                "conditions": found,
            }))?
        );
    } else {
        for one in &found {
            println!(
                "  {} {} : {}",
                if one.matched { "✔" } else { "✘" },
                one.kind,
                one.expr
            );
            for capture in &one.captures {
                println!("      {capture}");
            }
        }
    }
    if bad_pattern {
        anyhow::bail!("a condition could not be evaluated; see the report above");
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, kind: &str) -> StoredResponse {
        StoredResponse {
            seq: 0,
            at: "2026-09-02T00:00:00.000000Z".to_string(),
            url: "https://app.test/api".to_string(),
            status: Some(status),
            headers: vec![
                ("content-type".to_string(), kind.to_string()),
                ("date".to_string(), "whenever".to_string()),
            ],
            content_encoding: None,
            wire_bytes: None,
            body: Body::Empty,
        }
    }

    #[test]
    fn one_changed_json_field_is_named_and_the_rest_is_not() {
        let a = response(200, "application/json");
        let b = response(200, "application/json");
        let left = Text::Utf8(r#"{"id":1,"name":"alice","role":"user"}"#.to_string());
        let right = Text::Utf8(r#"{"id":2,"name":"alice","role":"user"}"#.to_string());
        let difference = compare((&a, &left), (&b, &right));
        assert!(!difference.same);
        assert_eq!(difference.json_changes.len(), 1);
        assert_eq!(difference.json_changes[0].path, "id");
        assert_eq!(difference.json_changes[0].from.as_deref(), Some("1"));
        assert_eq!(difference.json_changes[0].to.as_deref(), Some("2"));
    }

    /// The header that changes on every response must not make every pair of
    /// responses differ, or the verb answers "different" always and says
    /// nothing.
    #[test]
    fn a_clock_header_is_not_a_difference() {
        let mut a = response(200, "text/html");
        let mut b = response(200, "text/html");
        a.headers[1].1 = "Mon, 01 Jan 2026 00:00:00 GMT".to_string();
        b.headers[1].1 = "Tue, 02 Jan 2026 00:00:00 GMT".to_string();
        let body = Text::Utf8("<p>hello</p>".to_string());
        let difference = compare((&a, &body), (&b, &body));
        assert!(difference.same, "{difference:?}");
    }

    #[test]
    fn a_status_change_is_the_headline() {
        let a = response(200, "text/html");
        let b = response(403, "text/html");
        let body = Text::Utf8("<p>hello</p>".to_string());
        let difference = compare((&a, &body), (&b, &body));
        assert!(difference.status_changed);
        assert_eq!(difference.status, (Some(200), Some(403)));
        assert!(!difference.same);
    }

    /// The number a blind-injection loop reads instead of the body.
    ///
    /// The property that matters is the *ordering*, not any particular value:
    /// the same page scores above a page with one value changed, which scores
    /// above a different page. Thresholds belong to the caller, who knows how
    /// long its pages are, because this is Jaccard over unique tokens and a
    /// short body moves a long way per word.
    #[test]
    fn similarity_orders_the_same_page_above_a_changed_one_above_a_different_one() {
        let page = "<html><body><h1>Welcome back</h1><p>You have 3 new messages</p></body></html>";
        let changed = "<html><body><h1>Welcome back</h1><p>You have 4 new messages</p></body></html>";
        let other = "<html><body><h1>Login required</h1><p>Please sign in</p></body></html>";

        assert_eq!(similarity(page, page), 1.0, "identical is exactly 1.0");
        let near = similarity(page, changed);
        let far = similarity(page, other);
        assert!(near > far, "one value changed ({near}) must read closer than another page ({far})");
        assert!(near > 0.6, "the true/false pair of a blind test stays recognisable: {near}");
        assert!(far < 0.4, "a different page is plainly different: {far}");
    }

    /// A short body is sharper than a long one, and a caller thresholding on
    /// this number should know that rather than discover it.
    #[test]
    fn a_short_body_moves_further_per_word() {
        let short = similarity("the quick brown fox", "the quick brown cat");
        let long = similarity(
            "the quick brown fox jumps over the lazy dog again and again and again",
            "the quick brown cat jumps over the lazy dog again and again and again",
        );
        assert!(short < long, "short {short} should be further from 1.0 than long {long}");
    }

    #[test]
    fn a_body_that_is_not_text_is_reported_rather_than_mangled() {
        let a = response(200, "image/png");
        let body = Text::Binary {
            bytes: 12,
            sha256: "beef".to_string(),
        };
        let difference = compare((&a, &body), (&a, &body));
        assert!(difference.same, "identical binary bodies are identical");
        assert!(difference.line_changes.is_empty());
    }

    fn json_response(body: &str) -> (StoredResponse, Text) {
        (
            response(200, "application/json"),
            Text::Utf8(body.to_string()),
        )
    }

    #[test]
    fn a_regex_hands_back_what_it_captured() {
        let (stored, body) = json_response(r#"{"csrf":"tok_9f8e","user":"alice"}"#);
        let found = evaluate(
            &Condition::Regex("\"csrf\":\"([a-z0-9_]+)\"".to_string()),
            &stored,
            &body,
        );
        assert!(found.matched);
        assert_eq!(found.captures, vec!["tok_9f8e".to_string()]);
    }

    /// The binding an agent chains into the next request.
    #[test]
    fn a_json_path_captures_the_value_it_found() {
        let (stored, body) = json_response(r#"{"session":{"token":"abc123"}}"#);
        let found = evaluate(
            &Condition::Json {
                path: "session.token".to_string(),
                value: None,
            },
            &stored,
            &body,
        );
        assert!(found.matched);
        assert_eq!(found.captures, vec!["abc123".to_string()]);

        let wrong = evaluate(
            &Condition::Json {
                path: "session.token".to_string(),
                value: Some("nope".to_string()),
            },
            &stored,
            &body,
        );
        assert!(!wrong.matched, "a value that differs does not match");
        assert_eq!(wrong.captures, vec!["abc123".to_string()], "and still says what it found");
    }

    /// A pattern that does not compile is not a response that does not match.
    #[test]
    fn a_broken_pattern_is_not_a_negative_answer() {
        let (stored, body) = json_response("{}");
        let found = evaluate(&Condition::Regex("([unclosed".to_string()), &stored, &body);
        assert!(!found.matched);
        assert!(
            found.expr.contains("not a regular expression"),
            "the caller has to be able to tell this from a clean miss: {}",
            found.expr
        );
    }

    #[test]
    fn status_and_length_read_off_the_stored_response() {
        let (stored, body) = json_response(r#"{"a":1}"#);
        assert!(evaluate(&Condition::Status(200), &stored, &body).matched);
        assert!(!evaluate(&Condition::Status(403), &stored, &body).matched);
        assert!(evaluate(&Condition::LongerThan(3), &stored, &body).matched);
        assert!(!evaluate(&Condition::LongerThan(100), &stored, &body).matched);
        assert!(evaluate(&Condition::ShorterThan(100), &stored, &body).matched);
    }

    #[test]
    fn a_raw_request_reads_as_an_http_message() {
        let stored = StoredRequest {
            seq: 3,
            at: "2026-09-02T00:00:00.000000Z".to_string(),
            method: "POST".to_string(),
            url: "https://app.test/login?next=/home".to_string(),
            headers: vec![("cookie".to_string(), "session=abc".to_string())],
            body: Body::Empty,
        };
        let rendered = raw_request(&stored, &Text::Utf8("user=alice".to_string()));
        assert!(rendered.starts_with("POST /login?next=/home HTTP/1.1\n"), "{rendered}");
        assert!(rendered.contains("host: app.test\n"), "{rendered}");
        assert!(rendered.contains("cookie: session=abc\n"), "{rendered}");
        assert!(rendered.ends_with("\n\nuser=alice"), "{rendered}");
    }
}
