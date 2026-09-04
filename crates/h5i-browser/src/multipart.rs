//! `multipart/form-data`, taken apart and put back together.
//!
//! The one body format an edit cannot treat as text. A multipart body is a
//! sequence of parts separated by a boundary the *header* declares, each with
//! its own headers, and three of the things a file-upload test wants to change
//! live in those per-part headers rather than in the payload: the filename, the
//! declared content type, and the field name. String surgery on the raw body
//! gets the boundary wrong, or the trailing `--`, or the CRLFs, and the server
//! answers 400 to a request the caller believes it sent.
//!
//! So it is parsed into parts, edited as parts, and serialised with a boundary
//! this engine chooses. What comes out is well formed by construction, and
//! `Content-Length` follows from it.
//!
//! Deliberately not a general MIME implementation. No nested multiparts, no
//! transfer encodings, no header continuations: those appear in mail, not in a
//! browser's form post, and a parser that accepted them would be a parser with
//! more surface than the thing it parses.

use std::fmt::Write as _;

/// One part of a multipart body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Part {
    /// The form field name, from `Content-Disposition`.
    pub name: String,
    /// The filename, when the part declares one. A part with a filename is a
    /// file upload; a part without is an ordinary field, and servers routinely
    /// treat the two differently.
    pub filename: Option<String>,
    /// The declared type. What an upload filter reads, and therefore what a
    /// test changes.
    pub content_type: Option<String>,
    /// Headers other than the two above, kept so a round trip does not drop
    /// them.
    pub extra: Vec<(String, String)>,
    pub data: Vec<u8>,
}

/// The boundary a `Content-Type` header declares.
///
/// `multipart/form-data; boundary=----abc`, quoted or not. Returns `None` for a
/// content type that is not multipart, which is how a caller tells "this body is
/// not multipart" from "this body is multipart and malformed".
pub fn boundary_of(content_type: &str) -> Option<String> {
    let lower = content_type.to_ascii_lowercase();
    if !lower.contains("multipart/") {
        return None;
    }
    let at = lower.find("boundary=")? + "boundary=".len();
    let rest = content_type[at..].trim();
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default(),
        None => rest.split(';').next().unwrap_or_default().trim(),
    };
    (!value.is_empty()).then(|| value.to_string())
}

/// A boundary no body will contain by accident.
///
/// The engine's own, always, rather than the one that arrived: re-using an
/// incoming boundary means a caller who pastes that boundary into a part's data
/// splits the message, which is a way to send something other than what was
/// asked for. Fresh and random costs nothing.
pub fn fresh_boundary() -> String {
    let mut bytes = [0u8; 16];
    // The same source the script realm's `crypto.getRandomValues` uses. A
    // predictable boundary is not a security hole here, but a colliding one is
    // a corrupt request, and this is the cheapest way to never collide.
    let _ = getrandom::getrandom(&mut bytes);
    let mut out = String::from("----h5i");
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Split a multipart body into its parts.
///
/// A body that does not match its declared boundary comes back as `None`
/// rather than as an empty list: "this is not the shape it claims" and "this
/// has no parts" are different facts, and an edit that silently rebuilt a
/// malformed body as an empty one would erase the request.
pub fn parse(body: &[u8], boundary: &str) -> Option<Vec<Part>> {
    let sep = format!("--{boundary}");
    let sep = sep.as_bytes();
    let mut parts = Vec::new();
    let mut at = find(body, sep, 0)?;
    at += sep.len();
    loop {
        // `--` after the boundary is the end of the body.
        if body[at..].starts_with(b"--") {
            break;
        }
        // Past the CRLF that ends the boundary line.
        at = match body[at..].iter().position(|b| *b == b'\n') {
            Some(offset) => at + offset + 1,
            None => break,
        };
        let headers_end = find(body, b"\r\n\r\n", at).or_else(|| find(body, b"\n\n", at))?;
        let gap = if body[headers_end..].starts_with(b"\r\n\r\n") {
            4
        } else {
            2
        };
        let head = String::from_utf8_lossy(&body[at..headers_end]).to_string();
        let next = find(body, sep, headers_end + gap)?;
        // The CRLF before the next boundary belongs to the framing, not to the
        // data. Dropping it is what makes a round trip byte-identical.
        //
        // Only when there is data for it to sit behind. A body whose boundary
        // follows its header terminator with nothing between them is malformed,
        // and a page can post one: the line break being stepped over is then
        // the terminator's own, `end` lands two bytes before the data starts,
        // and the slice below runs backwards and panics.
        let data_at = headers_end + gap;
        let mut end = next;
        if end >= data_at + 2 && body[..end].ends_with(b"\r\n") {
            end -= 2;
        } else if end >= data_at + 1 && body[..end].ends_with(b"\n") {
            end -= 1;
        }

        let mut part = Part {
            data: body[data_at..end].to_vec(),
            ..Default::default()
        };
        for line in head.lines().filter(|l| !l.trim().is_empty()) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name.eq_ignore_ascii_case("content-disposition") {
                part.name = quoted(value, "name").unwrap_or_default();
                part.filename = quoted(value, "filename");
            } else if name.eq_ignore_ascii_case("content-type") {
                part.content_type = Some(value.to_string());
            } else {
                part.extra.push((name.to_string(), value.to_string()));
            }
        }
        parts.push(part);
        at = next + sep.len();
    }
    Some(parts)
}

/// `name="value"` out of a `Content-Disposition`.
fn quoted(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let at = header.find(&needle)? + needle.len();
    let rest = &header[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Put the parts back together with a boundary of this engine's choosing.
pub fn serialize(parts: &[Part], boundary: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = &part.filename {
            let _ = write!(disposition, "; filename=\"{filename}\"");
        }
        out.extend_from_slice(disposition.as_bytes());
        out.extend_from_slice(b"\r\n");
        if let Some(kind) = &part.content_type {
            out.extend_from_slice(format!("Content-Type: {kind}\r\n").as_bytes());
        }
        for (name, value) in &part.extra {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&part.data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"------abc\r\n");
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"comment\"\r\n\r\n");
        body.extend_from_slice(b"hello\r\n");
        body.extend_from_slice(b"------abc\r\n");
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"avatar\"; filename=\"cat.png\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body.extend_from_slice(b"\x89PNG\r\n\x1a\n binary");
        body.extend_from_slice(b"\r\n------abc--\r\n");
        body
    }

    /// A part whose boundary follows the header terminator with nothing
    /// between them is malformed, and a page can post one through `fetch`. The
    /// CRLF the parser stepped over was then the terminator's own, so the data
    /// slice ran backwards and the process died on the next edit of that
    /// stored request.
    #[test]
    fn a_part_with_no_data_at_all_is_parsed_rather_than_panicked_on() {
        let mut body = Vec::new();
        body.extend_from_slice(b"------abc\r\n");
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"a\"\r\n\r\n");
        body.extend_from_slice(b"------abc--\r\n");
        let parts = parse(&body, "----abc").expect("parses");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "a");
        assert!(parts[0].data.is_empty(), "{:?}", parts[0].data);
    }

    /// The well-formed spelling of the same thing keeps working.
    #[test]
    fn an_empty_part_written_properly_is_still_empty() {
        let mut body = Vec::new();
        body.extend_from_slice(b"------abc\r\n");
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"a\"\r\n\r\n");
        body.extend_from_slice(b"\r\n------abc--\r\n");
        let parts = parse(&body, "----abc").expect("parses");
        assert_eq!(parts.len(), 1);
        assert!(parts[0].data.is_empty());
    }

    #[test]
    fn a_body_is_taken_apart_into_its_fields_and_its_files() {
        let parts = parse(&sample(), "----abc").expect("parses");
        assert_eq!(parts.len(), 2);

        assert_eq!(parts[0].name, "comment");
        assert_eq!(parts[0].filename, None, "a field is not a file");
        assert_eq!(parts[0].data, b"hello");

        assert_eq!(parts[1].name, "avatar");
        assert_eq!(parts[1].filename.as_deref(), Some("cat.png"));
        assert_eq!(parts[1].content_type.as_deref(), Some("image/png"));
        assert_eq!(
            parts[1].data,
            b"\x89PNG\r\n\x1a\n binary",
            "binary data survives, CRLFs included"
        );
    }

    #[test]
    fn taking_it_apart_and_putting_it_back_changes_nothing_but_the_boundary() {
        let parts = parse(&sample(), "----abc").expect("parses");
        let again = serialize(&parts, "----abc");
        assert_eq!(
            parse(&again, "----abc").expect("parses again"),
            parts,
            "a round trip is the identity on the parts"
        );
    }

    #[test]
    fn the_boundary_comes_out_of_the_header_quoted_or_not() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=----abc").as_deref(),
            Some("----abc")
        );
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"----abc\"; charset=utf-8").as_deref(),
            Some("----abc")
        );
        assert_eq!(boundary_of("application/json"), None, "not multipart at all");
        assert_eq!(
            boundary_of("multipart/form-data"),
            None,
            "multipart with no boundary is not usable"
        );
    }

    /// "Malformed" and "empty" are different answers.
    #[test]
    fn a_body_that_does_not_match_its_boundary_is_not_an_empty_body() {
        assert_eq!(parse(b"nothing like a multipart body", "----abc"), None);
    }

    /// A part whose data contains the boundary would split the message. The
    /// engine picks its own, so it cannot be steered into one that collides.
    #[test]
    fn a_fresh_boundary_is_not_one_a_caller_can_predict() {
        let first = fresh_boundary();
        let second = fresh_boundary();
        assert_ne!(first, second);
        assert!(first.starts_with("----h5i"));
        assert!(first.len() > 32);
    }

    #[test]
    fn a_filename_with_traversal_in_it_survives_the_round_trip() {
        let mut parts = parse(&sample(), "----abc").expect("parses");
        // The whole point of the feature: the filename is where the test lives.
        parts[1].filename = Some("../../etc/passwd".to_string());
        parts[1].content_type = Some("image/png".to_string());
        let body = serialize(&parts, "----xyz");
        let again = parse(&body, "----xyz").expect("parses");
        assert_eq!(again[1].filename.as_deref(), Some("../../etc/passwd"));
        assert_eq!(again[1].content_type.as_deref(), Some("image/png"));
    }
}
