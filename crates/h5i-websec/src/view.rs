//! The text view: what every verb prints unless `--json` is asked for.
//!
//! An HTTP message is already a format a model reads fluently, so the view is
//! the message itself under one line of ids and numbers, with the body bounded.
//! The JSON envelope stays one flag away for a caller that parses.

use h5i_wire::read::{Text, printable};
use serde_json::Value;

/// How much of a body the text view prints. The same bound the engine puts on
/// a replay's preview, so `show` and `replay` cut in the same place.
pub const TEXT_BODY_BYTES: usize = 4096;

/// Up to `TEXT_BODY_BYTES` of a body, inert, cut on a character boundary, and
/// how many bytes of it that was.
fn head(text: &str) -> (String, usize) {
    let mut end = text.len().min(TEXT_BODY_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (printable_lines(&text[..end]), end)
}

/// Escape control characters but keep the line breaks, which `printable` alone
/// would turn into `\n`.
fn printable_lines(text: &str) -> String {
    text.lines().map(printable).collect::<Vec<_>>().join("\n")
}

/// A body as the text view prints it; `id` is what the pointer to the rest names.
pub fn push_body(out: &mut String, body: &Text, id: &str) {
    match body {
        Text::Utf8(text) if text.is_empty() => {}
        Text::Utf8(text) => {
            let (shown, end) = head(text);
            out.push_str(&shown);
            out.push('\n');
            if end < text.len() {
                out.push_str(&format!(
                    "[{end} of {} bytes shown; the rest: `h5i websec show {id} --raw`]\n",
                    text.len()
                ));
            }
        }
        Text::Cut { text, of_bytes } => {
            let (shown, end) = head(text);
            out.push_str(&shown);
            out.push('\n');
            out.push_str(&format!(
                "[{end} of {of_bytes} bytes shown; the store kept only {}]\n",
                text.len()
            ));
        }
        Text::Binary { bytes, sha256, .. } => out.push_str(&format!(
            "[{bytes} bytes, not text, sha256 {sha256}; the bytes: `h5i websec show {id} --body-to PATH`]\n"
        )),
        Text::Missing(why) => out.push_str(&format!("[no body: {why}]\n")),
    }
}

/// A message head (start line and headers) with each line made inert.
pub fn push_head(out: &mut String, raw_head: &str) {
    for line in raw_head.lines() {
        if line.is_empty() {
            break;
        }
        out.push_str(&printable(line));
        out.push('\n');
    }
    out.push('\n');
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

/// One line of an edit: `target=value (was old)`.
fn applied_line(edit: &Value) -> String {
    let target = str_at(edit, "target").unwrap_or("?");
    let mut line = match str_at(edit, "value") {
        Some(value) => format!("{target}={}", clip(value, 120)),
        None => format!("{target} (removed)"),
    };
    if let Some(encoded) = str_at(edit, "encoded") {
        line.push_str(&format!(" (sent as {})", clip(encoded, 120)));
    }
    if let Some(was) = str_at(edit, "was") {
        line.push_str(&format!(" (was {})", clip(was, 120)));
    }
    if edit.get("created").and_then(Value::as_bool) == Some(true) {
        line.push_str(" (created)");
    }
    line
}

fn clip(text: &str, max: usize) -> String {
    let shown: String = text.chars().take(max).collect();
    if shown.len() < text.len() {
        format!("{}…", printable(&shown))
    } else {
        printable(&shown)
    }
}

fn status_of(value: &Value) -> String {
    u64_at(value, "status").map_or_else(|| "no answer".to_string(), |s| s.to_string())
}

/// A body preview on one line, for a table row.
fn snippet(preview: &str) -> String {
    clip(&preview.split_whitespace().collect::<Vec<_>>().join(" "), 100)
}

/// `replay`'s answer as text. `Err` carries the refusal for stderr.
pub fn replay(answer: &Value) -> Result<String, String> {
    let response = answer.get("response");
    if response.is_none() {
        let message = str_at(answer, "message").unwrap_or("the replay was refused");
        return Err(match str_at(answer, "code") {
            Some(code) => format!("{message} ({code})"),
            None => message.to_string(),
        });
    }
    let response = response.expect("checked above");
    let samples = answer
        .get("samples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sent = answer.get("sent").cloned().unwrap_or(Value::Null);
    let method = str_at(&sent, "method").unwrap_or("?");
    let sent_url = str_at(&sent, "url").unwrap_or("?");
    let mut out = String::new();

    if samples.len() <= 1 {
        let id = answer
            .get("seq")
            .and_then(Value::as_u64)
            .map(|seq| format!("res_{seq} ← req_{seq}"))
            .unwrap_or_else(|| "not stored".to_string());
        let mut line = format!(
            "{id} · {} · {} bytes",
            status_of(response),
            u64_at(response, "bytes").unwrap_or(0)
        );
        if let Some(ms) = samples.first().and_then(|s| u64_at(s, "total_ms")) {
            line.push_str(&format!(" · {ms} ms"));
        }
        out.push_str(&line);
        out.push('\n');
        out.push_str(&format!("sent  {method} {}\n", printable(sent_url)));
    } else {
        let mut line = format!("{} sends of {method}", samples.len());
        if let Some(timing) = answer.get("timing") {
            for key in ["ttfb_ms", "total_ms"] {
                if let Some(stat) = timing.get(key) {
                    line.push_str(&format!(
                        " · {} median {} ms (±{})",
                        key.trim_end_matches("_ms"),
                        u64_at(stat, "median").unwrap_or(0),
                        u64_at(stat, "deviation").unwrap_or(0)
                    ));
                }
            }
        }
        out.push_str(&line);
        out.push('\n');
        // A walk sends a different URL each time, and `sent` is the last.
        let last = if samples.iter().any(|s| s.get("value").is_some()) {
            " (the last send)"
        } else {
            ""
        };
        out.push_str(&format!("sent  {method} {}{last}\n", printable(sent_url)));
    }
    for edit in answer
        .get("applied")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push_str(&format!("set   {}\n", applied_line(edit)));
    }
    if let Some(dropped) = answer.get("credentials_dropped").and_then(Value::as_array) {
        let names: Vec<&str> = dropped.iter().filter_map(Value::as_str).collect();
        out.push_str(&format!("dropped  {}\n", names.join(", ")));
    }
    if let Some(error) = str_at(response, "error") {
        out.push_str(&format!("error {}\n", printable(error)));
    }

    if samples.len() > 1 {
        let previews: Vec<&str> = samples
            .iter()
            .map(|s| str_at(s, "body_preview").unwrap_or(""))
            .collect();
        let same = previews.windows(2).all(|pair| pair[0] == pair[1]);
        out.push('\n');
        for sample in &samples {
            let id = u64_at(sample, "seq").map_or_else(|| "-".to_string(), |s| format!("res_{s}"));
            let mut row = format!(
                "{id:<8} {:>3}  {:>6} B  {:>4} ms",
                status_of(sample),
                u64_at(sample, "bytes").unwrap_or(0),
                u64_at(sample, "total_ms").unwrap_or(0)
            );
            if let Some(value) = str_at(sample, "value") {
                row.push_str(&format!("  {}", clip(value, 60)));
            }
            if !same {
                row.push_str(&format!("  {}", snippet(str_at(sample, "body_preview").unwrap_or(""))));
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        if same {
            out.push_str("every body: ");
            out.push_str(&snippet(previews.first().copied().unwrap_or("")));
            out.push('\n');
        }
        return Ok(out);
    }

    if let Some(final_url) = str_at(response, "url")
        && final_url != sent_url
    {
        out.push_str(&format!("final {}\n", printable(final_url)));
    }
    out.push('\n');
    match u64_at(response, "status") {
        Some(status) => out.push_str(&format!("HTTP/1.1 {status}\n")),
        None => out.push_str("HTTP/1.1 (no status: the request did not complete)\n"),
    }
    for pair in response
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = pair.get(0).and_then(Value::as_str).unwrap_or("");
        let value = pair.get(1).and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("{}: {}\n", printable(name), printable(value)));
    }
    out.push('\n');
    let preview = str_at(response, "body_preview").unwrap_or("");
    let bytes = u64_at(response, "bytes").unwrap_or(preview.len() as u64);
    // The preview is a lossy decode, so a replacement character is the sign
    // of a body that is not text.
    let seq = u64_at(answer, "seq").map_or_else(String::new, |s| format!("res_{s}"));
    if preview.contains('\u{FFFD}') {
        out.push_str(&format!(
            "[{bytes} bytes, not text; the bytes: `h5i websec show {seq} --body-to PATH`]\n"
        ));
    } else if !preview.is_empty() {
        out.push_str(&printable_lines(preview));
        out.push('\n');
    }
    if response.get("body_truncated").and_then(Value::as_bool) == Some(true) {
        out.push_str(&format!(
            "[{} of {bytes} bytes shown; the rest: `h5i websec show {seq} --raw`]\n",
            preview.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one() -> Value {
        json!({
            "ok": true, "seq": 7, "request_id": "req_7", "response_id": "res_7",
            "applied": [{"target": "header.X-Test", "value": "1", "created": true}],
            "sent": {"method": "GET", "url": "http://t/api", "header_names": [], "body_bytes": 0},
            "samples": [{"seq": 7, "status": 200, "ttfb_ms": 1, "total_ms": 2, "bytes": 5,
                          "body_preview": "hello", "body_truncated": false}],
            "response": {"status": 200, "url": "http://t/api",
                         "headers": [["content-type", "text/plain"]],
                         "bytes": 5, "body_preview": "hello", "body_truncated": false, "error": null}
        })
    }

    #[test]
    fn a_single_replay_reads_as_ids_then_the_message() {
        let text = replay(&one()).unwrap();
        assert_eq!(
            text,
            "res_7 ← req_7 · 200 · 5 bytes · 2 ms\n\
             sent  GET http://t/api\n\
             set   header.X-Test=1 (created)\n\
             \n\
             HTTP/1.1 200\n\
             content-type: text/plain\n\
             \n\
             hello\n"
        );
    }

    #[test]
    fn a_refusal_is_an_error_not_a_view() {
        let refused = json!({"ok": false, "code": "no-such-request", "message": "no request 9"});
        assert_eq!(replay(&refused).unwrap_err(), "no request 9 (no-such-request)");
    }

    #[test]
    fn a_cut_preview_says_where_the_rest_is() {
        let mut answer = one();
        answer["response"]["bytes"] = json!(9000);
        answer["response"]["body_truncated"] = json!(true);
        let text = replay(&answer).unwrap();
        assert!(text.ends_with("[5 of 9000 bytes shown; the rest: `h5i websec show res_7 --raw`]\n"), "{text}");
    }

    #[test]
    fn control_characters_in_a_response_are_made_inert() {
        let mut answer = one();
        answer["response"]["headers"] = json!([["x-evil", "a\u{1b}[2Jb"]]);
        let text = replay(&answer).unwrap();
        assert!(!text.contains('\u{1b}'), "{text}");
    }

    #[test]
    fn several_sends_are_one_row_each_and_a_shared_body_is_printed_once() {
        let mut answer = one();
        let sample = answer["samples"][0].clone();
        let mut second = sample.clone();
        second["seq"] = json!(8);
        answer["samples"] = json!([sample, second]);
        answer["timing"] = json!({"ttfb_ms": {"median": 1, "deviation": 0},
                                  "total_ms": {"median": 2, "deviation": 0}});
        let text = replay(&answer).unwrap();
        assert!(text.starts_with("2 sends of GET · ttfb median 1 ms (±0)"), "{text}");
        assert!(text.contains("res_7 ") && text.contains("res_8 "), "{text}");
        assert_eq!(text.matches("hello").count(), 1, "{text}");
    }

    #[test]
    fn different_bodies_each_get_a_snippet() {
        let mut answer = one();
        let a = answer["samples"][0].clone();
        let mut b = a.clone();
        b["seq"] = json!(8);
        b["value"] = json!("admin");
        b["body_preview"] = json!("denied");
        answer["samples"] = json!([a, b]);
        let text = replay(&answer).unwrap();
        assert!(text.contains("admin  denied"), "{text}");
        assert!(!text.contains("every body"), "{text}");
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary() {
        let body = "é".repeat(TEXT_BODY_BYTES);
        let mut out = String::new();
        push_body(&mut out, &Text::Utf8(body.clone()), "res_1");
        assert!(out.contains(&format!("of {} bytes shown", body.len())), "{out}");
    }
}
