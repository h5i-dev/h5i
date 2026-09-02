//! What encoding a document is in, and what that changes.

use encoding_rs::Encoding;

/// How many bytes of a document to look at when hunting for a `<meta charset>`.
///
/// The HTML standard's prescan stops at 1024 bytes, and that bound is load
/// bearing rather than an optimisation: a `<meta charset>` further down cannot
/// be honoured, because by then the parser has already committed to an
/// encoding. Matching the bound is what makes this agree with a browser about
/// pages that declare their encoding too late.
const PRESCAN_LIMIT: usize = 1024;

/// Work out a document's encoding, in the order the HTML standard says.
///
/// A BOM wins outright, being unambiguous and overriding everything including a
/// `<meta>` that disagrees. Then the transport's `Content-Type`, because the
/// server is better informed than the markup. Then the markup's own declaration.
/// Then, for an undeclared document, UTF-8 if the bytes are valid UTF-8 and
/// windows-1252 if they are not. See the body for why that asymmetry is the
/// right way round.
pub fn sniff(bytes: &[u8], content_type: Option<&str>) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        return encoding;
    }
    if let Some(header) = content_type
        && let Some(label) = charset_from_content_type(header)
        && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
    {
        return encoding;
    }
    if let Some(encoding) = prescan(bytes) {
        return encoding;
    }

    // Nothing declared.
    let has_multibyte = bytes.iter().any(|byte| *byte >= 0x80);
    if has_multibyte && std::str::from_utf8(bytes).is_ok() {
        return encoding_rs::UTF_8;
    }
    encoding_rs::WINDOWS_1252
}

/// The `charset=` parameter of a `Content-Type`, if it has one.
fn charset_from_content_type(header: &str) -> Option<&str> {
    for part in header.split(';').skip(1) {
        let mut halves = part.splitn(2, '=');
        let name = halves.next()?.trim();
        if name.eq_ignore_ascii_case("charset") {
            let value = halves.next()?.trim();
            // Quoted forms are legal and common enough to matter.
            return Some(value.trim_matches('"').trim_matches('\''));
        }
    }
    None
}

/// The HTML prescan: look for a `<meta>` that declares an encoding.
///
/// Deliberately a scan over bytes rather than a parse. The whole point of this
/// step is that it runs *before* there is a parser, since the parser cannot
/// start until it knows how to decode the bytes it would be parsing.
///
/// Both spellings are recognised: the modern `<meta charset=…>` and the legacy
/// `<meta http-equiv="Content-Type" content="text/html; charset=…">`, which is
/// what the pages that need this most actually use.
fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    let window = &bytes[..bytes.len().min(PRESCAN_LIMIT)];
    // ASCII-lossy is safe here: every byte sequence that matters to this scan
    // is ASCII, and a multi-byte character in the prefix cannot spell `meta`.
    let text = String::from_utf8_lossy(window).to_ascii_lowercase();

    let mut at = 0;
    while let Some(found) = text[at..].find("<meta") {
        let start = at + found;
        let end = text[start..].find('>').map(|e| start + e).unwrap_or(text.len());
        let tag = &text[start..end];
        at = end.max(start + 1);

        // `<meta charset="euc-jp">`
        if let Some(label) = attribute(tag, "charset")
            && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
        {
            return Some(encoding);
        }
        // `<meta http-equiv="content-type" content="text/html; charset=euc-jp">`
        let is_content_type = attribute(tag, "http-equiv")
            .map(|value| value.trim().eq_ignore_ascii_case("content-type"))
            .unwrap_or(false);
        if is_content_type
            && let Some(content) = attribute(tag, "content")
            && let Some(label) = charset_from_content_type(&content)
            && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
        {
            return Some(encoding);
        }
    }
    None
}

/// One attribute out of a lowercased tag, quoted or bare.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = tag[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        // Must be preceded by whitespace, so `charset` does not match inside
        // another attribute's value.
        if at > 0 && !tag.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
            _ => rest.split(|c: char| c.is_ascii_whitespace()).next().unwrap_or(""),
        };
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Decode a document's bytes.
///
/// Never fails: a malformed byte becomes U+FFFD rather than an error, because a
/// page with one bad byte should still render, and refusing a document over an
/// encoding detail is not this engine's job.
pub fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Percent-encode a URL query using the document's encoding.
///
/// The URL Standard encodes a query with the document's encoding rather than
/// UTF-8, and a code point that encoding cannot represent becomes an HTML
/// numeric character reference, so `丂` in a euc-jp document comes out as
/// `%26%2319970%3B`, which is `&#19970;` with its own punctuation already
/// percent-encoded.
///
/// UTF-8 documents take the fast path.
pub fn encode_query(query: &str, encoding: &'static Encoding) -> String {
    if encoding == encoding_rs::UTF_8 {
        return query.to_string();
    }

    // The per-character encoder rather than the bulk `encode`, because the two
    // disagree about the very case this exists for. `encode` renders an
    // unmappable code point as the *literal* `&#19970;`, and `&`, `#` and `;`
    // are not in the query percent-encode set, so they would pass through and
    // the answer would be `&%2319970;`.
    //
    // The URL Standard appends the reference already percent-encoded, precisely
    // so a generated reference cannot be mistaken for a real separator. Only the
    // encoder API reports *which* code point was unmappable.
    let mut encoder = encoding.new_encoder();
    let mut out = String::with_capacity(query.len());
    let mut buffer = [0u8; 512];
    let mut rest = query;

    loop {
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(rest, &mut buffer, true);
        for byte in &buffer[..written] {
            push_query_byte(*byte, &mut out);
        }
        rest = &rest[read..];
        match result {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::OutputFull => continue,
            encoding_rs::EncoderResult::Unmappable(point) => {
                out.push_str("%26%23");
                out.push_str(&(point as u32).to_string());
                out.push_str("%3B");
            }
        }
    }
    out
}

fn push_query_byte(byte: u8, out: &mut String) {
    if in_query_percent_encode_set(byte) {
        out.push('%');
        out.push_str(&format!("{byte:02X}"));
    } else {
        out.push(byte as char);
    }
}

/// The URL Standard's query percent-encode set.
///
/// C0 controls, space, and `"`, `#`, `<`, `>`. Everything above 0x7E too, which
/// is where a legacy encoding's bytes land and the reason this matters at all.
fn in_query_percent_encode_set(byte: u8) -> bool {
    byte <= 0x20 || byte >= 0x7f || matches!(byte, b'"' | b'#' | b'<' | b'>')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declaration_a_document_makes_is_the_one_that_is_used() {
        assert_eq!(sniff(b"<meta charset=\"euc-jp\">", None), encoding_rs::EUC_JP);
        assert_eq!(sniff(b"<meta charset=shift_jis>", None), encoding_rs::SHIFT_JIS);
        assert_eq!(
            sniff(
                b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=euc-kr\">",
                None
            ),
            encoding_rs::EUC_KR
        );
        // The transport outranks the markup: the server knows what it sent.
        assert_eq!(
            sniff(b"<meta charset=\"euc-jp\">", Some("text/html; charset=utf-8")),
            encoding_rs::UTF_8
        );
        // A BOM outranks both, being unambiguous.
        assert_eq!(
            sniff(b"\xef\xbb\xbf<meta charset=\"euc-jp\">", Some("text/html; charset=shift_jis")),
            encoding_rs::UTF_8
        );
        // Undeclared bytes that demonstrate UTF-8 are taken as UTF-8, which is
        // the detection a browser performs.
        assert_eq!(sniff("<html><body>café 日本".as_bytes(), None), encoding_rs::UTF_8);
        // Undeclared and *not* valid UTF-8 is windows-1252, not UTF-8. Reading
        // those bytes as UTF-8 replaces every one of them with U+FFFD and the
        // text is gone; read as windows-1252 it is at worst mojibake, and it is
        // what a browser shows.
        assert_eq!(
            sniff(b"<html><body>caf\xe9 na\xefve", None),
            encoding_rs::WINDOWS_1252
        );
        // Pure ASCII decodes the same either way, so the label follows the
        // browser rather than the detector.
        assert_eq!(sniff(b"<html><body>hi", None), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn a_declaration_past_the_prescan_limit_is_not_honoured() {
        // A browser has already committed to an encoding by then, and agreeing
        // with the browser is the point.
        let mut late = vec![b' '; PRESCAN_LIMIT + 64];
        late.extend_from_slice(b"<meta charset=\"euc-jp\">");
        // Falls back rather than honouring it, and the fallback for ASCII is
        // windows-1252, the same answer a browser gives.
        assert_eq!(sniff(&late, None), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn a_legacy_document_decodes_as_itself() {
        // "日本" in euc-jp, which is mojibake if read as UTF-8.
        let bytes = [0xc6, 0xfc, 0xcb, 0xdc];
        assert_eq!(decode(&bytes, encoding_rs::EUC_JP), "日本");
        assert_ne!(decode(&bytes, encoding_rs::UTF_8), "日本");
    }

    #[test]
    fn a_query_is_encoded_in_the_documents_encoding() {
        // In the encoding: the euc-jp bytes, percent-encoded.
        assert_eq!(encode_query("日", encoding_rs::EUC_JP), "%C6%FC");
        // Not in the encoding: an HTML numeric character reference, which is
        // what makes `丂` come out as `%26%2319970%3B` rather than as UTF-8.
        assert_eq!(encode_query("丂", encoding_rs::EUC_JP), "%26%2319970%3B");
        // ASCII is untouched, and UTF-8 documents take the fast path.
        assert_eq!(encode_query("x=1", encoding_rs::EUC_JP), "x=1");
        assert_eq!(encode_query("日", encoding_rs::UTF_8), "日");
    }
}
