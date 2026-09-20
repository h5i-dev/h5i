//! One stored message, for the console's request and response pane.
//!
//! The capture store holds `Authorization`, `Cookie` and `Set-Cookie` in full.
//! Those are masked here, in the server, so the page never carries a secret it
//! was not asked for: revealing one is a second request with `reveal=1`, not a
//! client-side toggle over bytes that already crossed.

use std::path::Path;

use h5i_wire::message::{StoredRequest, StoredResponse};
use h5i_wire::read::{Text, body_text, read_json, sequences};
use serde::Serialize;

/// Headers whose value is a credential.
const SECRET: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
];

pub fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET.contains(&name.as_str())
}

/// One header, with the value replaced when it carries a credential.
#[derive(Serialize, Clone)]
pub struct HeaderRow {
    pub name: String,
    pub value: String,
    /// True when `value` is the mask rather than what was sent.
    pub masked: bool,
}

fn headers(raw: &[(String, String)], reveal: bool) -> Vec<HeaderRow> {
    raw.iter()
        .map(|(name, value)| {
            if !reveal && is_secret(name) {
                HeaderRow {
                    name: name.clone(),
                    // The length is not a secret and it is the one thing a
                    // reader checks before revealing: an empty header and a
                    // 900-byte JWT are different problems.
                    value: format!("*** ({} bytes)", value.len()),
                    masked: true,
                }
            } else {
                HeaderRow {
                    name: name.clone(),
                    value: value.clone(),
                    masked: false,
                }
            }
        })
        .collect()
}

#[derive(Serialize)]
pub struct BodyView {
    /// `utf8`, `cut`, `binary` or `missing`.
    pub kind: String,
    pub text: String,
    /// What the response carried, when that is more than `text` holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub of_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl From<Text> for BodyView {
    fn from(text: Text) -> Self {
        match text {
            Text::Utf8(text) => BodyView { kind: "utf8".into(), text, of_bytes: None, sha256: None },
            Text::Cut { text, of_bytes } => BodyView {
                kind: "cut".into(),
                text,
                of_bytes: Some(of_bytes),
                sha256: None,
            },
            Text::Binary { bytes, sha256, text } => BodyView {
                kind: "binary".into(),
                text,
                of_bytes: Some(bytes),
                sha256: Some(sha256),
            },
            Text::Missing(why) => {
                BodyView { kind: "missing".into(), text: why, of_bytes: None, sha256: None }
            }
        }
    }
}

#[derive(Serialize)]
pub struct RequestView {
    pub seq: u64,
    pub at: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<HeaderRow>,
    pub body: BodyView,
}

#[derive(Serialize)]
pub struct ResponseView {
    pub seq: u64,
    pub at: String,
    pub url: String,
    pub status: Option<u16>,
    pub headers: Vec<HeaderRow>,
    pub content_encoding: Option<String>,
    pub wire_bytes: Option<u64>,
    pub body: BodyView,
}

/// Both halves of one stored message.
#[derive(Serialize)]
pub struct MessageView {
    pub seq: u64,
    /// `true` when any header in either half is masked, so the page knows
    /// whether a reveal would show anything.
    pub has_secrets: bool,
    pub revealed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestView>,
    /// Absent when the fetch never got one: a refusal, a failed connection, or
    /// a process that died mid-fetch. That is a state, not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseView>,
}

/// Read one message out of a session's store.
///
/// `None` when the session kept no such message, which includes every session
/// opened without `--capture`.
pub fn message(dir: &Path, seq: u64, reveal: bool) -> Option<MessageView> {
    let store = dir.join(crate::browser_session::MESSAGES_DIR);
    let request: Option<StoredRequest> =
        read_json(&store.join(format!("{seq}.request.json"))).ok();
    let response: Option<StoredResponse> =
        read_json(&store.join(format!("{seq}.response.json"))).ok();
    if request.is_none() && response.is_none() {
        return None;
    }
    let has_secrets = request
        .iter()
        .flat_map(|r| r.headers.iter())
        .chain(response.iter().flat_map(|r| r.headers.iter()))
        .any(|(name, _)| is_secret(name));
    Some(MessageView {
        seq,
        has_secrets,
        revealed: reveal,
        request: request.map(|r| RequestView {
            seq: r.seq,
            at: r.at.clone(),
            method: r.method.clone(),
            url: r.url.clone(),
            headers: headers(&r.headers, reveal),
            body: body_text(&store, &r.body).into(),
        }),
        response: response.map(|r| ResponseView {
            seq: r.seq,
            at: r.at.clone(),
            url: r.url.clone(),
            status: r.status,
            headers: headers(&r.headers, reveal),
            content_encoding: r.content_encoding.clone(),
            wire_bytes: r.wire_bytes,
            body: body_text(&store, &r.body).into(),
        }),
    })
}

/// Which sequences this session has stored, so the pane can say what is there.
pub fn stored(dir: &Path) -> Vec<u64> {
    sequences(&dir.join(crate::browser_session::MESSAGES_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_headers_are_masked_and_their_length_survives() {
        let raw = vec![
            ("Authorization".to_string(), "Bearer abcdef".to_string()),
            ("Accept".to_string(), "*/*".to_string()),
        ];
        let masked = headers(&raw, false);
        assert!(masked[0].masked);
        assert_eq!(masked[0].value, "*** (13 bytes)");
        assert!(!masked[0].value.contains("abcdef"));
        assert!(!masked[1].masked);
        assert_eq!(masked[1].value, "*/*");
    }

    #[test]
    fn reveal_returns_what_was_sent() {
        let raw = vec![("cookie".to_string(), "sid=42".to_string())];
        let shown = headers(&raw, true);
        assert!(!shown[0].masked);
        assert_eq!(shown[0].value, "sid=42");
    }

    #[test]
    fn a_secret_header_is_recognised_whatever_its_case() {
        for name in ["Authorization", "SET-COOKIE", "cookie", "X-Api-Key"] {
            assert!(is_secret(name), "{name}");
        }
        assert!(!is_secret("content-type"));
    }

    #[test]
    fn a_session_that_stored_nothing_has_no_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(message(dir.path(), 1, false).is_none());
        assert!(stored(dir.path()).is_empty());
    }
}
