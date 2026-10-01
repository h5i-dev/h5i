//! Token bytes: `v1.<tenant>.<user>.<expiry>.<hex signature>`.
//!
//! Pure and extracted to Lean (see `proofs/`). The signature covers the
//! payload: every byte before the last dot. Parsing is strict, so each
//! payload has exactly one reading.

pub const DOT: u8 = 46;

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub tenant: u64,
    pub user: u64,
    pub exp: u64,
    pub payload: Vec<u8>,
    pub sig: Vec<u8>,
}

/// Appends `n` in decimal, without leading zeros.
pub fn push_dec(out: &mut Vec<u8>, n: u64) {
    let mut rev: Vec<u8> = Vec::new();
    let mut m = n;
    let d = (m % 10) as u8;
    rev.push(48 + d);
    m = m / 10;
    while m > 0 {
        let d = (m % 10) as u8;
        rev.push(48 + d);
        m = m / 10;
    }
    let mut i = rev.len();
    while i > 0 {
        i -= 1;
        out.push(rev[i]);
    }
}

pub fn encode_payload(tenant: u64, user: u64, exp: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(118);
    out.push(49);
    out.push(DOT);
    push_dec(&mut out, tenant);
    out.push(DOT);
    push_dec(&mut out, user);
    out.push(DOT);
    push_dec(&mut out, exp);
    out
}

fn hex_digit(n: u8) -> u8 {
    if n < 10 {
        48 + n
    } else {
        87 + n
    }
}

/// `payload ++ "." ++ lowercase hex of sig`.
pub fn join(payload: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        out.push(payload[i]);
        i += 1;
    }
    out.push(DOT);
    let mut j = 0;
    while j < sig.len() {
        let b = sig[j];
        out.push(hex_digit(b / 16));
        out.push(hex_digit(b % 16));
        j += 1;
    }
    out
}

/// First dot at or after `start`, or `s.len()`.
fn next_dot(s: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < s.len() {
        if s[i] == DOT {
            return i;
        }
        i += 1;
    }
    s.len()
}

/// Strict decimal in `s[start..end]`: digits only, no leading zero, fits u64.
fn parse_dec(s: &[u8], start: usize, end: usize) -> Option<u64> {
    if start >= end || end > s.len() {
        return None;
    }
    if s[start] == 48 && end - start > 1 {
        return None;
    }
    let mut acc: u64 = 0;
    let mut i = start;
    while i < end {
        let c = s[i];
        if c < 48 || c > 57 {
            return None;
        }
        let d = (c - 48) as u64;
        if acc > 1844674407370955161 || (acc == 1844674407370955161 && d > 5) {
            return None;
        }
        acc = acc * 10 + d;
        i += 1;
    }
    Some(acc)
}

/// Value of a lowercase hex digit, or 16.
fn hex_value(c: u8) -> u8 {
    if c >= 48 && c <= 57 {
        c - 48
    } else if c >= 97 && c <= 102 {
        c - 87
    } else {
        16
    }
}

/// Strict lowercase hex in `s[start..end]`, even length.
fn parse_hex(s: &[u8], start: usize, end: usize) -> Option<Vec<u8>> {
    if start > end || end > s.len() || (end - start) % 2 != 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let hi = hex_value(s[i]);
        let lo = hex_value(s[i + 1]);
        if hi > 15 || lo > 15 {
            return None;
        }
        out.push(hi * 16 + lo);
        i += 2;
    }
    Some(out)
}

fn copy_prefix(s: &[u8], end: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < end && i < s.len() {
        out.push(s[i]);
        i += 1;
    }
    out
}

pub fn parse(tok: &[u8]) -> Option<Parsed> {
    let n = tok.len();
    let d0 = next_dot(tok, 0);
    if d0 != 2 || tok[0] != 118 || tok[1] != 49 {
        return None;
    }
    let d1 = next_dot(tok, d0 + 1);
    if d1 == n {
        return None;
    }
    let d2 = next_dot(tok, d1 + 1);
    if d2 == n {
        return None;
    }
    let d3 = next_dot(tok, d2 + 1);
    if d3 == n {
        return None;
    }
    if next_dot(tok, d3 + 1) != n {
        return None;
    }
    let tenant = match parse_dec(tok, d0 + 1, d1) {
        Some(v) => v,
        None => return None,
    };
    let user = match parse_dec(tok, d1 + 1, d2) {
        Some(v) => v,
        None => return None,
    };
    let exp = match parse_dec(tok, d2 + 1, d3) {
        Some(v) => v,
        None => return None,
    };
    let sig = match parse_hex(tok, d3 + 1, n) {
        Some(v) => v,
        None => return None,
    };
    Some(Parsed { tenant, user, exp, payload: copy_prefix(tok, d3), sig })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(p: &[u8], sig: &[u8]) -> Vec<u8> {
        join(p, sig)
    }

    #[test]
    fn round_trip() {
        for (t, u, e) in [(0, 0, 0), (1, 22, 333), (u64::MAX, 10, 100), (7, u64::MAX, 1)] {
            let p = encode_payload(t, u, e);
            let sig = [0u8, 1, 0xab, 0xff];
            let got = parse(&tok(&p, &sig)).unwrap();
            assert_eq!(got, Parsed { tenant: t, user: u, exp: e, payload: p, sig: sig.to_vec() });
        }
        assert_eq!(encode_payload(12, 0, 3), b"v1.12.0.3".to_vec());
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            &b""[..], b"v1", b"v1.", b"v2.1.2.3.aa", b"v1.01.2.3.aa", b"v1.1.2.3.4.aa", b"v1.1.2.aa",
            b"v1.1.2.3.AA", b"v1.1.2.3.a", b"v1.1.2.3.zz", b"v1.+1.2.3.aa", b"v1..2.3.aa",
            b"v1.18446744073709551616.2.3.aa", b"x1.1.2.3.aa",
        ] {
            assert_eq!(parse(bad), None, "{:?}", std::str::from_utf8(bad));
        }
        assert!(parse(b"v1.18446744073709551615.2.3.").is_some());
    }
}
