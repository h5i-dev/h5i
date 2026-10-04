//! `str` operations over byte strings.
//!
//! Each matches the `str` method it is named after when the text is ASCII.
//! Matching (`starts_with`, `contains`, `split_once`, ...) is exact for any
//! UTF-8, since `str` compares bytes there too. Only the whitespace and case
//! functions differ: they see ASCII alone, where `str::trim` and
//! `str::to_lowercase` also act on non-ASCII characters.

/// `a == b`.
pub fn eq(a: &Vec<u8>, b: &Vec<u8>) -> bool {
    a == b
}

/// `a[ao..ao + n] == b[bo..bo + n]`. Panics if a range is out of bounds.
pub fn range_eq(a: &Vec<u8>, ao: usize, b: &Vec<u8>, bo: usize, n: usize) -> bool {
    let mut i = 0;
    while i < n {
        if a[ao + i] != b[bo + i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `s.starts_with(p)`.
pub fn starts_with(s: &Vec<u8>, p: &Vec<u8>) -> bool {
    if p.len() > s.len() {
        return false;
    }
    range_eq(s, 0, p, 0, p.len())
}

/// `s.ends_with(p)`.
pub fn ends_with(s: &Vec<u8>, p: &Vec<u8>) -> bool {
    if p.len() > s.len() {
        return false;
    }
    range_eq(s, s.len() - p.len(), p, 0, p.len())
}

/// `s[lo..hi].to_vec()`. Panics unless `lo <= hi <= s.len()`.
pub fn slice(s: &Vec<u8>, lo: usize, hi: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = lo;
    while i < hi {
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `s.strip_prefix(p).map(|r| r.to_vec())`.
pub fn strip_prefix(s: &Vec<u8>, p: &Vec<u8>) -> Option<Vec<u8>> {
    if starts_with(s, p) {
        Some(slice(s, p.len(), s.len()))
    } else {
        None
    }
}

/// `s.strip_suffix(p).map(|r| r.to_vec())`.
pub fn strip_suffix(s: &Vec<u8>, p: &Vec<u8>) -> Option<Vec<u8>> {
    if ends_with(s, p) {
        Some(slice(s, 0, s.len() - p.len()))
    } else {
        None
    }
}

/// `s.contains(needle)` for a byte string `needle`.
pub fn contains(s: &Vec<u8>, needle: &Vec<u8>) -> bool {
    if needle.len() == 0 {
        return true;
    }
    if needle.len() > s.len() {
        return false;
    }
    let last = s.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if range_eq(s, i, needle, 0, needle.len()) {
            return true;
        }
        i += 1;
    }
    false
}

/// `s.iter().position(|&b| b == c)`.
pub fn find_byte(s: &Vec<u8>, c: u8) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        if s[i] == c {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `s.contains(&c)`.
pub fn contains_byte(s: &Vec<u8>, c: u8) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == c {
            return true;
        }
        i += 1;
    }
    false
}

/// `s.split_once(c)` for a one-byte separator.
pub fn split_once(s: &Vec<u8>, c: u8) -> Option<(Vec<u8>, Vec<u8>)> {
    match find_byte(s, c) {
        Some(i) => Some((slice(s, 0, i), slice(s, i + 1, s.len()))),
        None => None,
    }
}

/// `s.split(c)` for a one-byte separator: always at least one piece, empty
/// pieces kept.
pub fn split(s: &Vec<u8>, c: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] == c {
            out.push(cur);
            cur = Vec::new();
        } else {
            cur.push(s[i]);
        }
        i += 1;
    }
    out.push(cur);
    out
}

/// `char::is_whitespace` on an ASCII byte: `\t`, `\n`, `\x0B`, `\x0C`, `\r`, ` `.
pub fn is_whitespace(c: u8) -> bool {
    c == 32 || (c >= 9 && c <= 13)
}

/// `s.trim()`, for ASCII whitespace.
pub fn trim(s: &Vec<u8>) -> Vec<u8> {
    let mut lo = 0;
    while lo < s.len() && is_whitespace(s[lo]) {
        lo += 1;
    }
    let mut hi = s.len();
    while hi > lo && is_whitespace(s[hi - 1]) {
        hi -= 1;
    }
    slice(s, lo, hi)
}

/// `s.split_whitespace()`, for ASCII whitespace: no empty pieces.
pub fn split_whitespace(s: &Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if is_whitespace(s[i]) {
            if cur.len() > 0 {
                out.push(cur);
                cur = Vec::new();
            }
        } else {
            cur.push(s[i]);
        }
        i += 1;
    }
    if cur.len() > 0 {
        out.push(cur);
    }
    out
}

/// `c.to_ascii_lowercase()`.
pub fn lower(c: u8) -> u8 {
    if c >= 65 && c <= 90 { c + 32 } else { c }
}

/// `s.to_ascii_lowercase()`.
pub fn to_lowercase(s: &Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        out.push(lower(s[i]));
        i += 1;
    }
    out
}

/// `a.eq_ignore_ascii_case(b)`.
pub fn eq_ignore_case(a: &Vec<u8>, b: &Vec<u8>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if lower(a[i]) != lower(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// `[a, b].concat()`.
pub fn concat(a: &Vec<u8>, b: &Vec<u8>) -> Vec<u8> {
    let mut out = a.clone();
    let mut i = 0;
    while i < b.len() {
        out.push(b[i]);
        i += 1;
    }
    out
}

/// A pattern that is either exact or ends in `star`, which then matches any
/// rest: `pattern == s`, or `pattern == p ++ [star]` and `s.starts_with(p)`.
/// `user:*` with `star = b'*'` matches `user:` and `user:read`.
pub fn star_match(pattern: &Vec<u8>, s: &Vec<u8>, star: u8) -> bool {
    if pattern == s {
        return true;
    }
    let n = pattern.len();
    if n == 0 || pattern[n - 1] != star {
        return false;
    }
    if n - 1 > s.len() {
        return false;
    }
    range_eq(s, 0, pattern, 0, n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    fn bs(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| b(s)).collect()
    }

    const TEXT: [&str; 12] = ["", " ", "a", "ab", "abc", " a b ", "a,b,,c", ",", "\tx\n", "user:*", "user:read", "Ab C"];

    #[test]
    fn matches_str() {
        for s in TEXT {
            for p in TEXT {
                let (sv, pv) = (b(s), b(p));
                assert_eq!(eq(&sv, &pv), s == p);
                assert_eq!(starts_with(&sv, &pv), s.starts_with(p));
                assert_eq!(ends_with(&sv, &pv), s.ends_with(p));
                assert_eq!(contains(&sv, &pv), s.contains(p));
                assert_eq!(strip_prefix(&sv, &pv), s.strip_prefix(p).map(b));
                assert_eq!(strip_suffix(&sv, &pv), s.strip_suffix(p).map(b));
                assert_eq!(eq_ignore_case(&sv, &pv), s.eq_ignore_ascii_case(p));
                assert_eq!(concat(&sv, &pv), b(&format!("{s}{p}")));
                let glob = match p.strip_suffix('*') {
                    Some(q) => s.starts_with(q),
                    None => false,
                };
                assert_eq!(star_match(&pv, &sv, b'*'), s == p || glob);
            }
            let sv = b(s);
            assert_eq!(trim(&sv), b(s.trim()));
            assert_eq!(split_whitespace(&sv), bs(&s.split_whitespace().collect::<Vec<_>>()));
            assert_eq!(split(&sv, b','), bs(&s.split(',').collect::<Vec<_>>()));
            assert_eq!(split_once(&sv, b','), s.split_once(',').map(|(x, y)| (b(x), b(y))));
            assert_eq!(find_byte(&sv, b','), s.find(','));
            assert_eq!(contains_byte(&sv, b','), s.contains(','));
            assert_eq!(to_lowercase(&sv), b(&s.to_ascii_lowercase()));
        }
        for c in 0..=255u8 {
            assert_eq!(is_whitespace(c), (c as char).is_whitespace() && c < 128);
            assert_eq!(lower(c), c.to_ascii_lowercase());
        }
    }
}
