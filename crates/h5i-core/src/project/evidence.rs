//! Evidence copies: what a finding stands on, kept after its session is gone.
//!
//! A copy is the display version. Credential headers, secret-looking query
//! parameters and form or JSON fields are replaced before anything is written,
//! and bodies are capped. The digest of the original message files is kept so
//! a copy can be tied back to the store while the store exists.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Project, ProjectError, Result, bail, bounded, next_id, normalise_id, now, sha256_hex, write_private};
use crate::browser_session as bs;

pub const DIR: &str = "evidence";

/// The most of one body a copy keeps.
pub const MAX_BODY_CHARS: usize = 64 * 1024;

/// The largest file `evidence add --file` accepts.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

pub const REMOVED: &str = "[removed by h5i]";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// One stored HTTP message pair.
    Http,
    /// A log excerpt, a command's output, anything textual.
    Text,
    /// A screenshot or other image, stored beside the record.
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub kind: Kind,
    pub added: String,
    #[serde(default)]
    pub caption: String,
    // Http
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// sha256 of the original `<seq>.request.json` / `.response.json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_digests: Vec<(String, String)>,
    /// How many values were replaced on the way in.
    #[serde(default)]
    pub removed: usize,
    // Text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    // File
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

fn dir(project: &Project) -> std::path::PathBuf {
    project.path(DIR)
}

pub fn list(project: &Project) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir(project)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(record) = serde_json::from_str::<Record>(&text)
        {
            out.push(record);
        }
    }
    out.sort_by_key(|r| r.id.trim_start_matches("E-").parse::<u64>().unwrap_or(u64::MAX));
    out
}

pub fn get(project: &Project, id: &str) -> Result<Record> {
    let want = normalise_id("E", id)?;
    let path = dir(project).join(format!("{want}.json"));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(_) => bail!("project {} has no {want}", project.meta.name),
    }
}

/// The bytes of a file evidence, and its media type.
pub fn file_bytes(project: &Project, record: &Record) -> Option<(Vec<u8>, String)> {
    let name = record.file.as_deref()?;
    if name.contains('/') || name.contains("..") {
        return None;
    }
    let bytes = std::fs::read(dir(project).join(name)).ok()?;
    Some((bytes, record.media_type.clone().unwrap_or_else(|| "application/octet-stream".into())))
}

/// `E-1,E-2` and `3` normalised, each checked to exist.
pub fn check_ids(project: &Project, specs: &[String]) -> Result<Vec<String>> {
    let have: Vec<String> = list(project).into_iter().map(|r| r.id).collect();
    let mut out = Vec::new();
    for one in specs.iter().flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty()) {
        let id = normalise_id("E", one)?;
        if !have.contains(&id) {
            bail!(
                "project {} holds no {id}. Copy a message in first with \
                 `h5i project evidence add req_N --session <name>`",
                project.meta.name
            );
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn save(project: &Project, record: &Record) -> Result<()> {
    write_private(&dir(project).join(format!("{}.json", record.id)), serde_json::to_string_pretty(record)?.as_bytes())
}

fn fresh_id(project: &Project) -> String {
    let have = list(project);
    next_id("E", have.iter().map(|r| r.id.as_str()))
}

/// Copy one stored message out of a session. Copying the same message twice
/// returns the first copy.
pub fn add_http(project: &Project, browser_root: &Path, session: &bs::Session, seq: u64, caption: &str) -> Result<Record> {
    if let Some(existing) = list(project)
        .into_iter()
        .find(|r| r.session.as_deref() == Some(session.id.as_str()) && r.seq == Some(seq))
    {
        return Ok(existing);
    }
    if !bs::id_is_one_component(&session.id) {
        bail!("session record names `{}` as its id, which is not one h5i could have minted", session.id);
    }
    let sdir = bs::dir(browser_root, &session.id);
    let Some(view) = crate::message_view::message(&sdir, seq, false) else {
        bail!(
            "session {} holds no message {seq}. Only sessions opened with `--capture` keep messages, \
             and `h5i browser gc` reclaims them from ended sessions",
            session.name.as_deref().unwrap_or(&session.id)
        );
    };
    let store = sdir.join(bs::MESSAGES_DIR);
    let mut digests = Vec::new();
    for phase in ["request", "response"] {
        if let Ok(bytes) = std::fs::read(store.join(format!("{seq}.{phase}.json"))) {
            digests.push((phase.to_string(), format!("sha256:{}", sha256_hex(&bytes))));
        }
    }
    let mut value = serde_json::to_value(&view)?;
    let mut removed = 0usize;
    let mut request = value.get_mut("request").map(Value::take).filter(|v| !v.is_null());
    let mut response = value.get_mut("response").map(Value::take).filter(|v| !v.is_null());
    for half in [&mut request, &mut response].into_iter().flatten() {
        removed += scrub_half(half);
    }
    let field = |half: &Option<Value>, name: &str| half.as_ref().and_then(|h| h.get(name)).cloned();
    let record = Record {
        id: fresh_id(project),
        kind: Kind::Http,
        added: now(),
        caption: bounded("caption", caption)?,
        session: Some(session.id.clone()),
        session_name: session.name.clone(),
        seq: Some(seq),
        captured_at: field(&request, "at").or_else(|| field(&response, "at")).and_then(|v| v.as_str().map(str::to_string)),
        method: field(&request, "method").and_then(|v| v.as_str().map(str::to_string)),
        url: field(&request, "url").or_else(|| field(&response, "url")).and_then(|v| v.as_str().map(str::to_string)),
        status: field(&response, "status").and_then(|v| v.as_u64()).map(|s| s as u16),
        request,
        response,
        source_digests: digests,
        removed,
        text: None,
        file: None,
        media_type: None,
    };
    save(project, &record)?;
    Ok(record)
}

pub fn add_text(project: &Project, text: &str, caption: &str) -> Result<Record> {
    if text.trim().is_empty() {
        bail!("text evidence needs some text");
    }
    let record = Record {
        id: fresh_id(project),
        kind: Kind::Text,
        added: now(),
        caption: bounded("caption", caption)?,
        session: None,
        session_name: None,
        seq: None,
        captured_at: None,
        method: None,
        url: None,
        status: None,
        request: None,
        response: None,
        source_digests: Vec::new(),
        removed: 0,
        text: Some(cap(text)),
        file: None,
        media_type: None,
    };
    save(project, &record)?;
    Ok(record)
}

pub fn add_file(project: &Project, path: &Path, caption: &str) -> Result<Record> {
    let media = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => bail!("{} is not an image h5i keeps as evidence: use png, jpeg, gif or webp", path.display()),
    };
    let meta = std::fs::metadata(path).map_err(|e| ProjectError(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_FILE_BYTES {
        bail!("{} is {} bytes, and {MAX_FILE_BYTES} is the most one evidence file holds", path.display(), meta.len());
    }
    let bytes = std::fs::read(path)?;
    let id = fresh_id(project);
    let ext = media.rsplit('/').next().unwrap_or("bin");
    let name = format!("{id}.{ext}");
    write_private(&dir(project).join(&name), &bytes)?;
    let record = Record {
        id,
        kind: Kind::File,
        added: now(),
        caption: bounded("caption", caption)?,
        session: None,
        session_name: None,
        seq: None,
        captured_at: None,
        method: None,
        url: None,
        status: None,
        request: None,
        response: None,
        source_digests: vec![("file".into(), format!("sha256:{}", sha256_hex(&bytes)))],
        removed: 0,
        text: None,
        file: Some(name),
        media_type: Some(media.into()),
    };
    save(project, &record)?;
    Ok(record)
}

fn cap(text: &str) -> String {
    if text.len() <= MAX_BODY_CHARS {
        return text.to_string();
    }
    let mut end = MAX_BODY_CHARS;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[h5i kept the first {end} of {} bytes]", &text[..end], text.len())
}

/// Header names whose value is a credential or close enough to one.
pub fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    crate::message_view::is_secret(&n)
        || ["token", "secret", "password", "passwd", "api-key", "api_key", "apikey", "session", "csrf", "xsrf", "signature"]
            .iter()
            .any(|w| n.contains(w))
        || matches!(n.as_str(), "sid" | "auth" | "key" | "code" | "pass" | "pwd")
}

/// Mask one half of a message view in place. Returns how many values went.
fn scrub_half(half: &mut Value) -> usize {
    let mut removed = 0;
    if let Some(headers) = half.get_mut("headers").and_then(Value::as_array_mut) {
        for h in headers {
            let name = h.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let masked = h.get("masked").and_then(Value::as_bool).unwrap_or(false);
            if masked {
                removed += 1;
                h["value"] = Value::String(REMOVED.into());
            } else if secret_name(&name) {
                removed += 1;
                h["value"] = Value::String(REMOVED.into());
                h["masked"] = Value::Bool(true);
            }
        }
    }
    if let Some(url) = half.get("url").and_then(Value::as_str).map(str::to_string) {
        let (clean, n) = scrub_url(&url);
        removed += n;
        half["url"] = Value::String(clean);
    }
    let is_json = half
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|h| {
            h.get("name").and_then(Value::as_str).is_some_and(|n| n.eq_ignore_ascii_case("content-type"))
                && h.get("value").and_then(Value::as_str).is_some_and(|v| v.contains("json"))
        });
    if let Some(body) = half.get_mut("body")
        && let Some(text) = body.get("text").and_then(Value::as_str).map(str::to_string)
    {
        let (clean, n) = scrub_body(&text, is_json);
        removed += n;
        body["text"] = Value::String(cap(&clean));
    }
    removed
}

fn scrub_url(url: &str) -> (String, usize) {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return (url.to_string(), 0);
    };
    if parsed.query().is_none() {
        return (url.to_string(), 0);
    }
    let mut n = 0;
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(k, v)| {
            if secret_name(&k) {
                n += 1;
                (k.into_owned(), REMOVED.to_string())
            } else {
                (k.into_owned(), v.into_owned())
            }
        })
        .collect();
    if n == 0 {
        return (url.to_string(), 0);
    }
    parsed.query_pairs_mut().clear().extend_pairs(pairs);
    (parsed.to_string(), n)
}

fn scrub_body(text: &str, is_json: bool) -> (String, usize) {
    if is_json && let Ok(mut v) = serde_json::from_str::<Value>(text) {
        let n = scrub_json(&mut v);
        if n > 0 {
            return (serde_json::to_string_pretty(&v).unwrap_or_default(), n);
        }
        return (text.to_string(), 0);
    }
    // `a=1&password=x`: a form body. Anything else is left as it is.
    let looks_form = !text.contains(char::is_whitespace) && text.contains('=');
    if !looks_form {
        return (text.to_string(), 0);
    }
    let mut n = 0;
    let parts: Vec<String> = text
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if secret_name(&k.replace('+', " ")) => {
                n += 1;
                format!("{k}={REMOVED}")
            }
            _ => pair.to_string(),
        })
        .collect();
    (parts.join("&"), n)
}

fn scrub_json(v: &mut Value) -> usize {
    let mut n = 0;
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if secret_name(k) && (child.is_string() || child.is_number()) {
                    *child = Value::String(REMOVED.into());
                    n += 1;
                } else {
                    n += scrub_json(child);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                n += scrub_json(child);
            }
        }
        _ => {}
    }
    n
}

/// The message as HTTP text, for a report's code block.
pub fn http_text(record: &Record) -> String {
    let mut out = String::new();
    let headers = |half: &Value, out: &mut String| {
        for h in half.get("headers").and_then(Value::as_array).into_iter().flatten() {
            out.push_str(&format!(
                "{}: {}\n",
                h.get("name").and_then(Value::as_str).unwrap_or_default(),
                h.get("value").and_then(Value::as_str).unwrap_or_default()
            ));
        }
    };
    let body = |half: &Value, out: &mut String| {
        if let Some(text) = half.get("body").and_then(|b| b.get("text")).and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push('\n');
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
    };
    if let Some(req) = &record.request {
        out.push_str(&format!(
            "{} {}\n",
            req.get("method").and_then(Value::as_str).unwrap_or("?"),
            req.get("url").and_then(Value::as_str).unwrap_or("?")
        ));
        headers(req, &mut out);
        body(req, &mut out);
    }
    if let Some(res) = &record.response {
        if !out.is_empty() {
            out.push('\n');
        }
        match res.get("status").and_then(Value::as_u64) {
            Some(s) => out.push_str(&format!("HTTP/1.1 {s}\n")),
            None => out.push_str("(no response)\n"),
        }
        headers(res, &mut out);
        body(res, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_query_parameters_are_removed_and_others_kept() {
        let (url, n) = scrub_url("https://a.test/cb?code=abc&state=1&access_token=zz");
        assert_eq!(n, 2);
        assert!(!url.contains("abc") && !url.contains("zz"), "{url}");
        assert!(url.contains("state=1"));
    }

    #[test]
    fn form_and_json_passwords_are_removed() {
        let (form, n) = scrub_body("user=alice&password=hunter2", false);
        assert_eq!(n, 1);
        assert!(form.contains("user=alice") && !form.contains("hunter2"));
        let (json, n) = scrub_body(r#"{"user":"alice","auth":{"api_key":"k1"},"items":[{"csrf_token":"t"}]}"#, true);
        assert_eq!(n, 2);
        assert!(!json.contains("k1") && !json.contains("\"t\""), "{json}");
        assert!(json.contains("alice"));
    }

    #[test]
    fn credential_headers_are_removed_not_just_masked() {
        let mut half = serde_json::json!({
            "url": "https://a.test/",
            "headers": [
                {"name": "Cookie", "value": "*** (9 bytes)", "masked": true},
                {"name": "X-Auth-Token", "value": "abc", "masked": false},
                {"name": "Accept", "value": "*/*", "masked": false}
            ],
            "body": {"kind": "utf8", "text": ""}
        });
        assert_eq!(scrub_half(&mut half), 2);
        let text = half.to_string();
        assert!(!text.contains("abc") && !text.contains("9 bytes"), "{text}");
        assert!(text.contains("*/*"));
    }

    #[test]
    fn a_long_body_is_capped_on_a_character_boundary() {
        let long = "é".repeat(MAX_BODY_CHARS);
        let capped = cap(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("h5i kept the first"));
    }
}
