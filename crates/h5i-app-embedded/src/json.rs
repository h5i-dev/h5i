//! JSON for request and reply bodies, without an allocator.
//!
//! [`Writer`] writes into a handler's output buffer. Strings, keys and
//! numbers go through `h5i_app_json::slice`, which is extracted to Lean and
//! proven: a string's bytes cannot end its JSON string early. The commas
//! follow the rule of the proven `Vec` writer (`h5i_app_json::write`); a test
//! compares the two on random token sequences.
//!
//! [`Object`] reads a flat object (`{"k": v, ...}` whose values are strings,
//! numbers, `true`, `false` or `null`), strictly: one object and nothing
//! after it but whitespace, no nesting, no duplicate keys (so two parsers
//! cannot pick different values), valid escapes, no control bytes in strings,
//! valid UTF-8, at most [`MAX_KEYS`] keys. Trusted, tested.
//!
//! ```ignore
//! let obj = json::Object::parse(req.body).ok_or(400u16)?;
//! let dc = obj.get(b"setpoint_dc").and_then(|v| v.as_i64()).ok_or(400u16)?;
//!
//! let mut w = json::Writer::new(out);
//! w.open_obj();
//! w.key(b"setpoint_dc");
//! w.int(s.setpoint_dc as i64);
//! w.close_obj();
//! let len = w.finish()?;
//! ```

use h5i_app_json::slice;

/// Writes JSON tokens into a buffer. Every method is a no-op once the
/// buffer is full; [`Writer::finish`] then answers `None`.
pub struct Writer<'a> {
    out: &'a mut [u8],
    at: usize,
    ok: bool,
    /// The last token ended a value: the next one, unless it closes, needs a
    /// comma.
    after_value: bool,
}

impl<'a> Writer<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Writer { out, at: 0, ok: true, after_value: false }
    }

    fn tok(&mut self, closes: bool, ends_value: bool, f: impl FnOnce(&mut [u8], usize) -> Option<usize>) {
        if !self.ok {
            return;
        }
        let mut at = self.at;
        if self.after_value && !closes {
            match self.out.get_mut(at) {
                Some(b) => *b = b',',
                None => {
                    self.ok = false;
                    return;
                }
            }
            at += 1;
        }
        match f(self.out, at) {
            Some(p) => {
                self.at = p;
                self.after_value = ends_value;
            }
            None => self.ok = false,
        }
    }

    fn byte(c: u8) -> impl FnOnce(&mut [u8], usize) -> Option<usize> {
        move |out, at| {
            *out.get_mut(at)? = c;
            Some(at + 1)
        }
    }

    pub fn open_obj(&mut self) {
        self.tok(false, false, Self::byte(b'{'));
    }

    pub fn close_obj(&mut self) {
        self.tok(true, true, Self::byte(b'}'));
    }

    pub fn open_arr(&mut self) {
        self.tok(false, false, Self::byte(b'['));
    }

    pub fn close_arr(&mut self) {
        self.tok(true, true, Self::byte(b']'));
    }

    /// An object key and its colon.
    pub fn key(&mut self, k: &[u8]) {
        self.tok(false, false, |o, p| slice::put_key(o, p, k));
    }

    pub fn str(&mut self, s: &[u8]) {
        self.tok(false, true, |o, p| slice::put_str(o, p, s));
    }

    pub fn uint(&mut self, n: u64) {
        self.tok(false, true, |o, p| slice::put_dec(o, p, n));
    }

    /// A signed integer: `-` and the magnitude's digits.
    pub fn int(&mut self, n: i64) {
        self.tok(false, true, |o, p| {
            if n < 0 {
                *o.get_mut(p)? = b'-';
                slice::put_dec(o, p + 1, n.unsigned_abs())
            } else {
                slice::put_dec(o, p, n as u64)
            }
        });
    }

    pub fn bool(&mut self, b: bool) {
        let lit: &[u8] = if b { b"true" } else { b"false" };
        self.raw_value(lit);
    }

    pub fn null(&mut self) {
        self.raw_value(b"null");
    }

    fn raw_value(&mut self, lit: &'static [u8]) {
        self.tok(false, true, |o, p| {
            let end = p.checked_add(lit.len())?;
            o.get_mut(p..end)?.copy_from_slice(lit);
            Some(end)
        });
    }

    /// The length written, or `None` if it did not fit.
    pub fn finish(self) -> Option<usize> {
        if self.ok { Some(self.at) } else { None }
    }
}

/// The most keys an [`Object`] holds.
pub const MAX_KEYS: usize = 16;

/// A value of a flat object, as spans of the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    /// The string's bytes between the quotes, still escaped.
    Str(&'a [u8]),
    /// The number as written (JSON grammar checked).
    Num(&'a [u8]),
    Bool(bool),
    Null,
}

impl<'a> Value<'a> {
    /// The string, unescaped, when it has no escapes; see [`Value::unescape`].
    pub fn as_str(&self) -> Option<&'a [u8]> {
        match self {
            Value::Str(s) if !s.contains(&b'\\') => Some(s),
            _ => None,
        }
    }

    /// The string with its escapes decoded into `out` (UTF-8); `None` if it
    /// is not a string or does not fit.
    pub fn unescape<'o>(&self, out: &'o mut [u8]) -> Option<&'o [u8]> {
        let Value::Str(s) = self else { return None };
        let n = unescape_into(s, out)?;
        Some(&out[..n])
    }

    /// An integer with no fraction or exponent, within `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        let Value::Num(n) = self else { return None };
        let (neg, digits) = match n.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, *n),
        };
        if digits.is_empty() || digits.len() > 19 || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let v = digits.iter().fold(0i128, |v, &c| v * 10 + (c - b'0') as i128);
        i64::try_from(if neg { -v } else { v }).ok()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// A parsed flat object: its keys (still escaped) and values, in order.
pub struct Object<'a> {
    keys: [&'a [u8]; MAX_KEYS],
    vals: [Value<'a>; MAX_KEYS],
    n: usize,
}

impl<'a> Object<'a> {
    /// `None` unless `s` is exactly one flat object, as described in the
    /// module documentation.
    pub fn parse(s: &'a [u8]) -> Option<Object<'a>> {
        let mut p = Parser { s, i: 0 };
        let mut obj = Object { keys: [&s[..0]; MAX_KEYS], vals: [Value::Null; MAX_KEYS], n: 0 };
        p.ws();
        p.eat(b'{')?;
        p.ws();
        if p.peek() == Some(b'}') {
            p.i += 1;
        } else {
            loop {
                p.ws();
                let k = p.string()?;
                p.ws();
                p.eat(b':')?;
                p.ws();
                let v = p.value()?;
                if obj.n == MAX_KEYS || obj.keys[..obj.n].iter().any(|&o| same_key(o, k)) {
                    return None;
                }
                obj.keys[obj.n] = k;
                obj.vals[obj.n] = v;
                obj.n += 1;
                p.ws();
                match p.next()? {
                    b',' => continue,
                    b'}' => break,
                    _ => return None,
                }
            }
        }
        p.ws();
        if p.i != s.len() {
            return None;
        }
        Some(obj)
    }

    /// The value of `key` (compared after unescaping).
    pub fn get(&self, key: &[u8]) -> Option<Value<'a>> {
        (0..self.n).find(|&i| key_eq(self.keys[i], key)).map(|i| self.vals[i])
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Whether every key is one of `allowed`: refuse fields a client may
    /// expect to have an effect.
    pub fn only(&self, allowed: &[&[u8]]) -> bool {
        self.keys[..self.n].iter().all(|k| allowed.iter().any(|a| key_eq(k, a)))
    }
}

/// An escaped key equals the plain `want`.
fn key_eq(escaped: &[u8], want: &[u8]) -> bool {
    let mut it = Unescape { s: escaped, i: 0, pending: [0; 4], np: 0, pi: 0 };
    let mut j = 0;
    while let Some(b) = it.next_byte() {
        if want.get(j) != Some(&b) {
            return false;
        }
        j += 1;
    }
    j == want.len()
}

/// Two escaped keys denote the same string.
fn same_key(a: &[u8], b: &[u8]) -> bool {
    let mut x = Unescape { s: a, i: 0, pending: [0; 4], np: 0, pi: 0 };
    let mut y = Unescape { s: b, i: 0, pending: [0; 4], np: 0, pi: 0 };
    loop {
        match (x.next_byte(), y.next_byte()) {
            (None, None) => return true,
            (p, q) if p == q => {}
            _ => return false,
        }
    }
}

fn unescape_into(s: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut it = Unescape { s, i: 0, pending: [0; 4], np: 0, pi: 0 };
    let mut n = 0;
    while let Some(b) = it.next_byte() {
        *out.get_mut(n)? = b;
        n += 1;
    }
    Some(n)
}

/// The bytes of a validated, escaped string, decoded (UTF-8 out).
struct Unescape<'a> {
    s: &'a [u8],
    i: usize,
    pending: [u8; 4],
    np: usize,
    pi: usize,
}

impl Unescape<'_> {
    fn next_byte(&mut self) -> Option<u8> {
        if self.pi < self.np {
            self.pi += 1;
            return Some(self.pending[self.pi - 1]);
        }
        let c = *self.s.get(self.i)?;
        self.i += 1;
        if c != b'\\' {
            return Some(c);
        }
        // Validated by the parser: one of the escapes below.
        let e = self.s[self.i];
        self.i += 1;
        let b = match e {
            b'b' => 8,
            b'f' => 12,
            b'n' => 10,
            b'r' => 13,
            b't' => 9,
            b'u' => {
                let (cp, used) = code_point(&self.s[self.i - 2..])?;
                self.i += used - 2;
                let ch = char::from_u32(cp)?;
                let enc = ch.encode_utf8(&mut self.pending);
                self.np = enc.len();
                self.pi = 1;
                return Some(self.pending[0]);
            }
            other => other,
        };
        Some(b)
    }
}

fn hex4(s: &[u8]) -> Option<u32> {
    let mut v = 0u32;
    for &c in s.get(..4)? {
        v = v * 16 + (c as char).to_digit(16)?;
    }
    Some(v)
}

/// The code point of `\uXXXX` (or a surrogate pair `\uD83D\uDE00`) at the
/// start of `s`, and how many bytes it used.
fn code_point(s: &[u8]) -> Option<(u32, usize)> {
    if s.get(..2)? != b"\\u" {
        return None;
    }
    let hi = hex4(&s[2..])?;
    match hi {
        0xD800..=0xDBFF => {
            if s.get(6..8)? != b"\\u" {
                return None;
            }
            let lo = hex4(&s[8..])?;
            if !(0xDC00..=0xDFFF).contains(&lo) {
                return None;
            }
            Some((0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), 12))
        }
        0xDC00..=0xDFFF => None,
        _ => Some((hi, 6)),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        if self.next()? == c { Some(()) } else { None }
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, l: &[u8]) -> Option<()> {
        if self.s.get(self.i..self.i + l.len())? == l {
            self.i += l.len();
            Some(())
        } else {
            None
        }
    }

    /// A string; its bytes between the quotes.
    fn string(&mut self) -> Option<&'a [u8]> {
        self.eat(b'"')?;
        let start = self.i;
        loop {
            let c = self.next()?;
            match c {
                b'"' => break,
                b'\\' => match self.next()? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        let (_, used) = code_point(&self.s[self.i - 2..])?;
                        self.i += used - 2;
                    }
                    _ => return None,
                },
                0..=31 => return None,
                _ => {}
            }
        }
        let body = &self.s[start..self.i - 1];
        core::str::from_utf8(body).ok()?;
        Some(body)
    }

    /// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`
    fn number(&mut self) -> Option<&'a [u8]> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.next()? {
            b'0' => {}
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            self.digit1()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            self.digit1()?;
        }
        Some(&self.s[start..self.i])
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn digit1(&mut self) -> Option<()> {
        if !matches!(self.peek(), Some(b'0'..=b'9')) {
            return None;
        }
        self.digits();
        Some(())
    }

    fn value(&mut self) -> Option<Value<'a>> {
        match self.peek()? {
            b'"' => self.string().map(Value::Str),
            b't' => self.lit(b"true").map(|_| Value::Bool(true)),
            b'f' => self.lit(b"false").map(|_| Value::Bool(false)),
            b'n' => self.lit(b"null").map(|_| Value::Null),
            b'-' | b'0'..=b'9' => self.number().map(Value::Num),
            // Nested objects and arrays are refused.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;
    use proptest::prelude::*;

    #[test]
    fn writes_an_object() {
        let mut out = [0u8; 128];
        let mut w = Writer::new(&mut out);
        w.open_obj();
        w.key(b"a\"b");
        w.int(-12);
        w.key(b"list");
        w.open_arr();
        w.bool(true);
        w.null();
        w.uint(u64::MAX);
        w.int(i64::MIN);
        w.close_arr();
        w.key(b"s");
        w.str(b"x\ny");
        w.close_obj();
        let n = w.finish().unwrap();
        assert_eq!(
            &out[..n],
            br#"{"a\"b":-12,"list":[true,null,18446744073709551615,-9223372036854775808],"s":"x\u000ay"}"#
        );
        let mut small = [0u8; 5];
        let mut w = Writer::new(&mut small);
        w.open_obj();
        w.key(b"abc");
        w.int(1);
        assert_eq!(w.finish(), None);
    }

    #[derive(Clone, Debug)]
    enum T {
        Null,
        Bool(bool),
        Num(u64),
        Str(Vec<u8>),
        Key(Vec<u8>),
        ArrOpen,
        ArrClose,
        ObjOpen,
        ObjClose,
    }

    fn tok() -> impl Strategy<Value = T> {
        prop_oneof![
            Just(T::Null),
            any::<bool>().prop_map(T::Bool),
            any::<u64>().prop_map(T::Num),
            proptest::collection::vec(any::<u8>(), 0..8).prop_map(T::Str),
            proptest::collection::vec(any::<u8>(), 0..8).prop_map(T::Key),
            Just(T::ArrOpen),
            Just(T::ArrClose),
            Just(T::ObjOpen),
            Just(T::ObjClose),
        ]
    }

    proptest! {
        /// Same bytes as the proven `Vec` writer, on any token sequence.
        #[test]
        fn writer_matches_proven_writer(ts in proptest::collection::vec(tok(), 0..24)) {
            use h5i_app_json::Tok;
            let toks: Vec<Tok> = ts.iter().map(|t| match t {
                T::Null => Tok::Null,
                T::Bool(b) => Tok::Bool(*b),
                T::Num(n) => Tok::Num(*n),
                T::Str(s) => Tok::Str(s.clone()),
                T::Key(s) => Tok::Key(s.clone()),
                T::ArrOpen => Tok::ArrOpen,
                T::ArrClose => Tok::ArrClose,
                T::ObjOpen => Tok::ObjOpen,
                T::ObjClose => Tok::ObjClose,
            }).collect();
            let want = h5i_app_json::write(&toks);
            let mut out = std::vec![0u8; want.len()];
            let mut w = Writer::new(&mut out);
            for t in &ts {
                match t {
                    T::Null => w.null(),
                    T::Bool(b) => w.bool(*b),
                    T::Num(n) => w.uint(*n),
                    T::Str(s) => w.str(s),
                    T::Key(s) => w.key(s),
                    T::ArrOpen => w.open_arr(),
                    T::ArrClose => w.close_arr(),
                    T::ObjOpen => w.open_obj(),
                    T::ObjClose => w.close_obj(),
                }
            }
            prop_assert_eq!(w.finish(), Some(want.len()));
            prop_assert_eq!(&out, &want);
            if !want.is_empty() {
                let mut short = std::vec![0u8; want.len() - 1];
                let mut w = Writer::new(&mut short);
                for t in &ts {
                    match t {
                        T::Null => w.null(),
                        T::Bool(b) => w.bool(*b),
                        T::Num(n) => w.uint(*n),
                        T::Str(s) => w.str(s),
                        T::Key(s) => w.key(s),
                        T::ArrOpen => w.open_arr(),
                        T::ArrClose => w.close_arr(),
                        T::ObjOpen => w.open_obj(),
                        T::ObjClose => w.close_obj(),
                    }
                }
                prop_assert_eq!(w.finish(), None);
            }
        }

        /// Whatever the reader accepts, serde_json reads as the same flat object.
        #[test]
        fn reader_agrees_with_serde(s in "[ {}\":,a-c0-9.eE+\\-\\\\nultrfs]{0,40}") {
            let b = s.as_bytes();
            if let Some(obj) = Object::parse(b) {
                let v: serde_json::Value = serde_json::from_slice(b).expect("serde accepts what we accept");
                let m = v.as_object().expect("an object");
                prop_assert_eq!(m.len(), obj.len());
                for (k, sv) in m {
                    let ov = obj.get(k.as_bytes()).expect("same keys");
                    match ov {
                        Value::Null => prop_assert!(sv.is_null()),
                        Value::Bool(x) => prop_assert_eq!(sv.as_bool(), Some(x)),
                        Value::Num(_) => prop_assert!(sv.is_number()),
                        Value::Str(_) => {
                            let mut buf = [0u8; 64];
                            let got = ov.unescape(&mut buf).unwrap();
                            prop_assert_eq!(Some(got), sv.as_str().map(str::as_bytes));
                        }
                    }
                }
            }
        }

        #[test]
        fn reader_round_trips_strings(s in "\\PC{0,12}", n in any::<i64>()) {
            let doc = serde_json::to_vec(&serde_json::json!({ "s": s, "n": n })).unwrap();
            let obj = Object::parse(&doc).unwrap();
            let mut buf = [0u8; 64];
            prop_assert_eq!(obj.get(b"s").unwrap().unescape(&mut buf), Some(s.as_bytes()));
            prop_assert_eq!(obj.get(b"n").unwrap().as_i64(), Some(n));
        }
    }

    #[test]
    fn reader() {
        let o =
            Object::parse(br#" {"a": 1, "b":"x\"y", "c": true, "d": null, "e": -0.5e3, "\u0066": false} "#).unwrap();
        assert_eq!(o.get(b"a").unwrap().as_i64(), Some(1));
        assert_eq!(o.get(b"b").unwrap().as_str(), None);
        let mut buf = [0u8; 8];
        assert_eq!(o.get(b"b").unwrap().unescape(&mut buf), Some(&b"x\"y"[..]));
        assert_eq!(o.get(b"c").unwrap().as_bool(), Some(true));
        assert_eq!(o.get(b"d"), Some(Value::Null));
        assert_eq!(o.get(b"e").unwrap().as_i64(), None);
        assert_eq!(o.get(b"f").unwrap().as_bool(), Some(false));
        assert!(o.only(&[b"a", b"b", b"c", b"d", b"e", b"f"]));
        assert!(!o.only(&[b"a"]));
        assert!(Object::parse(b"{}").unwrap().is_empty());
        let bad: &[&[u8]] = &[
            b"",
            b"[]",
            b"{",
            b"{\"a\":1,}",
            b"{\"a\":1}x",
            b"{\"a\":1}{}",
            b"{\"a\":{}}",
            b"{\"a\":[1]}",
            b"{\"a\":1,\"a\":2}",
            b"{\"a\":1,\"\\u0061\":2}",
            b"{\"a\":01}",
            b"{\"a\":1.}",
            b"{\"a\":+1}",
            b"{\"a\":\"\x01\"}",
            b"{\"a\":\"\\x\"}",
            b"{\"a\":\"\\ud800\"}",
            b"{\"a\":\"\xff\"}",
            b"{a:1}",
            b"{\"a\" 1}",
            b"{\"a\":tru}",
        ];
        for b in bad {
            assert!(Object::parse(b).is_none(), "{:?}", core::str::from_utf8(b));
        }
        let mut many = std::string::String::from("{");
        for i in 0..=MAX_KEYS {
            many.push_str(&std::format!("\"k{i}\":{i},"));
        }
        many.pop();
        many.push('}');
        assert!(Object::parse(many.as_bytes()).is_none());
    }
}
