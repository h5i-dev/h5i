//! The same output into a caller's buffer, for `no_std` devices without an
//! allocator. Extracted to Lean and proven like the `Vec` writer: on
//! `Some(p)`, `out[pos..p]` is exactly `Spec.tokBytes` of the token (so
//! [`put_str`] writes a string that lexes back to itself), and the bytes
//! outside `pos..p` are unchanged. `None` when the bytes do not fit.

use crate::hex_digit;

/// One byte at `pos`.
fn put_byte(out: &mut [u8], pos: usize, b: u8) -> Option<usize> {
    if pos >= out.len() {
        return None;
    }
    out[pos] = b;
    Some(pos + 1)
}

/// One escaped byte of a string's body at `pos`.
pub fn put_escaped_byte(out: &mut [u8], pos: usize, c: u8) -> Option<usize> {
    if c == 34 || c == 92 {
        if pos > out.len() || out.len() - pos < 2 {
            return None;
        }
        out[pos] = 92;
        out[pos + 1] = c;
        Some(pos + 2)
    } else if c < 32 {
        if pos > out.len() || out.len() - pos < 6 {
            return None;
        }
        out[pos] = 92;
        out[pos + 1] = 117;
        out[pos + 2] = 48;
        out[pos + 3] = 48;
        out[pos + 4] = hex_digit(c / 16);
        out[pos + 5] = hex_digit(c % 16);
        Some(pos + 6)
    } else {
        put_byte(out, pos, c)
    }
}

/// The escaped body of a string (no quotes) at `pos`.
pub fn put_escaped(out: &mut [u8], pos: usize, s: &[u8]) -> Option<usize> {
    let mut at = pos;
    let mut i = 0;
    while i < s.len() {
        at = match put_escaped_byte(out, at, s[i]) {
            None => return None,
            Some(a) => a,
        };
        i += 1;
    }
    Some(at)
}

/// A JSON string: `"`, the escaped bytes of `s`, `"`.
pub fn put_str(out: &mut [u8], pos: usize, s: &[u8]) -> Option<usize> {
    let at = match put_byte(out, pos, 34) {
        None => return None,
        Some(a) => a,
    };
    let at = match put_escaped(out, at, s) {
        None => return None,
        Some(a) => a,
    };
    put_byte(out, at, 34)
}

/// An object key: [`put_str`], then `:`.
pub fn put_key(out: &mut [u8], pos: usize, s: &[u8]) -> Option<usize> {
    let at = match put_str(out, pos, s) {
        None => return None,
        Some(a) => a,
    };
    put_byte(out, at, 58)
}

/// `n` in decimal, without leading zeros.
pub fn put_dec(out: &mut [u8], pos: usize, n: u64) -> Option<usize> {
    let mut digits = [0u8; 20];
    let mut k = 0;
    let mut m = n;
    digits[0] = 48 + (m % 10) as u8;
    k += 1;
    m = m / 10;
    while m > 0 && k < 20 {
        digits[k] = 48 + (m % 10) as u8;
        k += 1;
        m = m / 10;
    }
    if pos > out.len() || out.len() - pos < k {
        return None;
    }
    let mut i = 0;
    while i < k {
        out[pos + i] = digits[k - 1 - i];
        i += 1;
    }
    Some(pos + k)
}
