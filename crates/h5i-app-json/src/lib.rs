//! JSON output for replies. The writer over tokens is extracted to Lean and
//! proven (see `proofs/`): a string's bytes cannot end its JSON string early.
//!
//! Escaping: `"` and `\` get a backslash, bytes below 0x20 become `\u00XX`,
//! and all other bytes pass through. Valid UTF-8 in gives valid JSON out.

mod value;
pub use value::Value;

/// One JSON token. A document is a sequence of them; commas and colons are
/// added by `write`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Null,
    Bool(bool),
    Num(u64),
    Str(Vec<u8>),
    /// An object key; `write` adds the colon.
    Key(Vec<u8>),
    ArrOpen,
    ArrClose,
    ObjOpen,
    ObjClose,
}

fn hex_digit(n: u8) -> u8 {
    if n < 10 {
        48 + n
    } else {
        87 + n
    }
}

/// Appends one escaped byte.
fn push_escaped_byte(out: &mut Vec<u8>, c: u8) {
    if c == 34 {
        out.push(92);
        out.push(34);
    } else if c == 92 {
        out.push(92);
        out.push(92);
    } else if c < 32 {
        out.push(92);
        out.push(117);
        out.push(48);
        out.push(48);
        out.push(hex_digit(c / 16));
        out.push(hex_digit(c % 16));
    } else {
        out.push(c);
    }
}

/// Appends the escaped body of a JSON string (no quotes).
pub fn push_escaped(out: &mut Vec<u8>, s: &Vec<u8>) {
    let mut i = 0;
    while i < s.len() {
        push_escaped_byte(out, s[i]);
        i += 1;
    }
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

fn ends_value(t: &Tok) -> bool {
    match t {
        Tok::Null => true,
        Tok::Bool(_) => true,
        Tok::Num(_) => true,
        Tok::Str(_) => true,
        Tok::ArrClose => true,
        Tok::ObjClose => true,
        _ => false,
    }
}

fn is_close(t: &Tok) -> bool {
    match t {
        Tok::ArrClose => true,
        Tok::ObjClose => true,
        _ => false,
    }
}

fn push_tok(out: &mut Vec<u8>, t: &Tok) {
    match t {
        Tok::Null => {
            out.push(110);
            out.push(117);
            out.push(108);
            out.push(108);
        }
        Tok::Bool(b) => {
            if *b {
                out.push(116);
                out.push(114);
                out.push(117);
                out.push(101);
            } else {
                out.push(102);
                out.push(97);
                out.push(108);
                out.push(115);
                out.push(101);
            }
        }
        Tok::Num(n) => push_dec(out, *n),
        Tok::Str(s) => {
            out.push(34);
            push_escaped(out, s);
            out.push(34);
        }
        Tok::Key(s) => {
            out.push(34);
            push_escaped(out, s);
            out.push(34);
            out.push(58);
        }
        Tok::ArrOpen => out.push(91),
        Tok::ArrClose => out.push(93),
        Tok::ObjOpen => out.push(123),
        Tok::ObjClose => out.push(125),
    }
}

/// Writes tokens as JSON text. A comma goes before any token that follows a
/// complete value, except a closing bracket.
pub fn write(toks: &Vec<Tok>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut after_value = false;
    let mut i = 0;
    while i < toks.len() {
        if after_value && !is_close(&toks[i]) {
            out.push(44);
        }
        push_tok(&mut out, &toks[i]);
        after_value = ends_value(&toks[i]);
        i += 1;
    }
    out
}
