//! The DOM instrument, as bytes the proxy injects and as a decoder/injector
//! the proxy needs to place it. The engine vehicle (an opt-in realm hook) reads
//! the same [`INSTRUMENT_JS`] so the two vehicles cannot drift.

use std::io::Read as _;

/// The instrument source, with `__H5I_*` placeholders still in it.
pub const INSTRUMENT_JS: &str = include_str!("script/instrument.js");

/// The property a probe leaves on `Object.prototype`, and the value it sets.
/// Kept in step with `h5i-websec`'s `dom` module, which seeds the same token.
pub const CANARY_PROP: &str = "h5ipp";
pub const CANARY_VALUE: &str = "reserved";

/// Where the injected script reports. The proxy MITMs this host (a forged cert
/// the session CA signs), so an https page can beacon to it without tripping
/// mixed-content, and the host short-circuits the request before policy.
pub const BEACON_HOST: &str = "h5i.invalid";
pub const BEACON_PATH: &str = "/__h5i_dom_report";
pub const BEACON_URL: &str = "https://h5i.invalid/__h5i_dom_report";

/// The instrument with its placeholders filled, ready to wrap in a `<script>`.
pub fn render_instrument() -> String {
    INSTRUMENT_JS
        .replace("__H5I_TOKEN__", CANARY_PROP)
        .replace("__H5I_VALUE__", CANARY_VALUE)
        .replace("__H5I_BEACON__", BEACON_URL)
}

/// The most a response body is decoded to before injection. A hostile target
/// can send a few KB of gzip that inflates to gigabytes; past this the response
/// is passed through unmodified rather than decoded into an OOM.
pub const MAX_DECODE_BYTES: u64 = 32 * 1024 * 1024;

/// Decode a response body for injection. `None` means "leave the original
/// alone": an encoding this cannot decode, a decode that failed, or output that
/// would exceed [`MAX_DECODE_BYTES`]. An absent or `identity` encoding returns
/// the bytes unchanged.
pub fn decode_body(raw: &[u8], content_encoding: Option<&str>) -> Option<Vec<u8>> {
    let encoding = content_encoding.unwrap_or("").trim();
    // One layer, matching the engine's own rule: a stacked `gzip, br` is not
    // decoded here, so the response is passed through untouched rather than
    // half-decoded.
    let mut layers = encoding
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("identity"));
    let name = layers.next().unwrap_or("");
    if layers.next().is_some() {
        return None;
    }
    // Every decoder is bounded to MAX_DECODE_BYTES + 1 so a bomb cannot grow the
    // buffer without limit; one byte over the cap means "too big, pass through".
    let cap = MAX_DECODE_BYTES;
    let mut out = Vec::new();
    let read = match name.to_ascii_lowercase().as_str() {
        "" => return Some(raw.to_vec()),
        "gzip" | "x-gzip" => flate2::read::GzDecoder::new(raw)
            .take(cap + 1)
            .read_to_end(&mut out),
        "deflate" => flate2::read::ZlibDecoder::new(raw)
            .take(cap + 1)
            .read_to_end(&mut out)
            .or_else(|_| {
                out.clear();
                flate2::read::DeflateDecoder::new(raw)
                    .take(cap + 1)
                    .read_to_end(&mut out)
            }),
        "br" => brotli::Decompressor::new(raw, 4096)
            .take(cap + 1)
            .read_to_end(&mut out),
        _ => return None,
    };
    read.ok()?;
    if out.len() as u64 > cap {
        return None;
    }
    Some(out)
}

/// Insert a `<script>` carrying `js` so it runs before the page's own scripts:
/// right after the opening `<head>`, or `<html>`, or at the very start.
pub fn inject_script(html: &[u8], js: &str) -> Vec<u8> {
    let tag = format!("<script>{js}</script>");
    let at = after_opening_tag(html, b"<head")
        .or_else(|| after_opening_tag(html, b"<html"))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(html.len() + tag.len());
    out.extend_from_slice(&html[..at]);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&html[at..]);
    out
}

/// The byte offset just past the first `>` of an opening tag whose name matches
/// `needle` (case-insensitive), or `None` if there is no such tag.
fn after_opening_tag(html: &[u8], needle: &[u8]) -> Option<usize> {
    let lower: Vec<u8> = html.iter().map(u8::to_ascii_lowercase).collect();
    let start = lower.windows(needle.len()).position(|w| w == needle)?;
    html[start..].iter().position(|&b| b == b'>').map(|gt| start + gt + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fills_every_placeholder() {
        let js = render_instrument();
        assert!(!js.contains("__H5I_"));
        assert!(js.contains("h5ipp"));
        assert!(js.contains(BEACON_URL));
    }

    #[test]
    fn injects_right_after_head() {
        let out = inject_script(b"<html><head><title>x</title></head><body>y</body></html>", "Z");
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "<html><head><script>Z</script><title>x</title></head><body>y</body></html>");
    }

    #[test]
    fn falls_back_to_html_then_start() {
        let out = inject_script(b"<html><body>y</body></html>", "Z");
        assert!(String::from_utf8(out).unwrap().starts_with("<html><script>Z</script>"));
        let out = inject_script(b"nodoctype", "Z");
        assert!(String::from_utf8(out).unwrap().starts_with("<script>Z</script>nodoctype"));
    }

    #[test]
    fn identity_passes_through_and_unknown_is_skipped() {
        assert_eq!(decode_body(b"hi", None), Some(b"hi".to_vec()));
        assert_eq!(decode_body(b"hi", Some("identity")), Some(b"hi".to_vec()));
        assert_eq!(decode_body(b"hi", Some("gzip, br")), None);
        assert_eq!(decode_body(b"hi", Some("zstd")), None);
    }

    #[test]
    fn gzip_round_trips() {
        use flate2::write::GzEncoder;
        use std::io::Write as _;
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"<html><head></head></html>").unwrap();
        let gz = e.finish().unwrap();
        assert_eq!(decode_body(&gz, Some("gzip")).unwrap(), b"<html><head></head></html>");
    }
}
