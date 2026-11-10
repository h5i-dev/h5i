//! `application/x-www-form-urlencoded` pairs: a query (`Request::query`) or
//! a form body. Trusted, tested.
//!
//! ```ignore
//! let raw = req.query_param(b"unit").unwrap_or(b"c");
//! let mut buf = [0u8; 32];
//! let unit = form::decode(raw, &mut buf).ok_or(400u16)?;
//! ```
//!
//! [`param`] compares names byte for byte, undecoded, and answers with the
//! first match; [`unique`] refuses a name given twice, so two parsers cannot
//! pick different values.

/// The `name=value` pairs of `s`, in order; a pair without `=` has an empty
/// value; empty pairs (`a=1&&b=2`) are skipped.
pub fn pairs(s: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    s.split(|&c| c == b'&').filter(|p| !p.is_empty()).map(|p| match p.iter().position(|&c| c == b'=') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => (p, &p[..0]),
    })
}

/// The first value of `name`, undecoded.
pub fn param<'a>(s: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    pairs(s).find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// A name given more than once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Duplicate;

/// The value of `name`, undecoded: `Ok(None)` if absent, [`Duplicate`] if
/// given more than once.
pub fn unique<'a>(s: &'a [u8], name: &[u8]) -> Result<Option<&'a [u8]>, Duplicate> {
    let mut found = None;
    for (k, v) in pairs(s) {
        if k == name {
            if found.is_some() {
                return Err(Duplicate);
            }
            found = Some(v);
        }
    }
    Ok(found)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `raw` with `%XX` decoded and `+` read as a space, into `out`. `None` if a
/// `%` is not followed by two hex digits or `out` is too small. The result
/// may be any bytes, including NUL and invalid UTF-8.
pub fn decode<'o>(raw: &[u8], out: &'o mut [u8]) -> Option<&'o [u8]> {
    let mut i = 0;
    let mut n = 0;
    while i < raw.len() {
        let b = match raw[i] {
            b'%' => {
                let hi = hex(*raw.get(i + 1)?)?;
                let lo = hex(*raw.get(i + 2)?)?;
                i += 2;
                hi * 16 + lo
            }
            b'+' => b' ',
            c => c,
        };
        *out.get_mut(n)? = b;
        n += 1;
        i += 1;
    }
    Some(&out[..n])
}

/// A decimal `u32` (no sign, no leading `+`, at most 10 digits).
pub fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = b.iter().fold(0u64, |v, &c| v * 10 + (c - b'0') as u64);
    u32::try_from(v).ok()
}

/// A decimal `i32`: an optional `-`, then at most 10 digits.
pub fn parse_i32(b: &[u8]) -> Option<i32> {
    let (neg, digits) = match b.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, b),
    };
    if digits.is_empty() || digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = digits.iter().fold(0i64, |v, &c| v * 10 + (c - b'0') as i64);
    i32::try_from(if neg { -v } else { v }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params() {
        let q = b"a=1&b=&c&&a=2&d=x%20y";
        assert_eq!(param(q, b"a"), Some(&b"1"[..]));
        assert_eq!(param(q, b"b"), Some(&b""[..]));
        assert_eq!(param(q, b"c"), Some(&b""[..]));
        assert_eq!(param(q, b"z"), None);
        assert_eq!(unique(q, b"a"), Err(Duplicate));
        assert_eq!(unique(q, b"d"), Ok(Some(&b"x%20y"[..])));
        assert_eq!(unique(q, b"z"), Ok(None));
        assert_eq!(param(b"", b"a"), None);
    }

    #[test]
    fn decoding() {
        let mut out = [0u8; 16];
        assert_eq!(decode(b"x%20y+z", &mut out), Some(&b"x y z"[..]));
        assert_eq!(decode(b"%00%ff%Fe", &mut out), Some(&[0u8, 255, 254][..]));
        assert_eq!(decode(b"%2", &mut out), None);
        assert_eq!(decode(b"%zz", &mut out), None);
        assert_eq!(decode(b"%", &mut out), None);
        assert_eq!(decode(&[b'a'; 17], &mut out), None);
        assert_eq!(decode(&[b'a'; 16], &mut out), Some(&[b'a'; 16][..]));
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_u32(b"4294967295"), Some(u32::MAX));
        assert_eq!(parse_u32(b"4294967296"), None);
        assert_eq!(parse_u32(b"+1"), None);
        assert_eq!(parse_u32(b""), None);
        assert_eq!(parse_i32(b"-2147483648"), Some(i32::MIN));
        assert_eq!(parse_i32(b"2147483648"), None);
        assert_eq!(parse_i32(b"-"), None);
        assert_eq!(parse_i32(b"--1"), None);
        assert_eq!(parse_i32(b"-12"), Some(-12));
    }
}
