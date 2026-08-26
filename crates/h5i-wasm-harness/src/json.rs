//! Minimal JSON parse/serialize for no_std+alloc. Zero dependencies because
//! this crate must compile against hand-built core/alloc rlibs for wasm32.
//! Objects keep insertion order (Vec of pairs); numbers are f64.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => {
                // f64::fract/abs live in std, not core; integer check by cast.
                let n = *n;
                if !n.is_finite() {
                    // NaN/inf cannot be represented in JSON; never emit them.
                    out.push_str("null");
                } else if n > -9e15 && n < 9e15 && n == (n as i64) as f64 {
                    out.push_str(&format!("{}", n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            Value::Str(s) => write_escaped(s, out),
            Value::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Recursion cap: the parser is recursive, and a model (or a hostile
/// endpoint) can send thousands of open brackets — without a cap that is a
/// stack overflow, which in wasm is an unrecoverable trap.
const MAX_DEPTH: u32 = 128;

pub fn parse(input: &str) -> Result<Value, String> {
    let mut p = Parser { bytes: input.as_bytes(), pos: 0, depth: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(format!("trailing data at byte {}", p.pos));
    }
    Ok(v)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", b as char, self.pos))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(format!("unexpected input at byte {}", self.pos)),
        }
    }

    fn nested(
        &mut self,
        f: fn(&mut Self) -> Result<Value, String>,
    ) -> Result<Value, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!("nesting deeper than {} levels", MAX_DEPTH));
        }
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn literal(&mut self, lit: &str, v: Value) -> Result<Value, String> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.pos))
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.pos += 1;
        }
        let text = core::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
        let n: f64 = text
            .parse()
            .map_err(|_| format!("bad number '{}' at byte {}", text, start))?;
        // "1e999" parses to inf, which would serialize back as invalid JSON.
        if !n.is_finite() {
            return Err(format!("number '{}' out of f64 range at byte {}", text, start));
        }
        Ok(Value::Num(n))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'u') => {
                            let cp = self.hex4()?;
                            // Surrogate pair handling for characters above the BMP.
                            let c = if (0xD800..0xDC00).contains(&cp) {
                                if self.bytes[self.pos + 1..].starts_with(b"\\u") {
                                    self.pos += 2;
                                    let lo = self.hex4()?;
                                    // The second escape must be a LOW surrogate;
                                    // \uD800\uD800 would otherwise underflow.
                                    if (0xDC00..0xE000).contains(&lo) {
                                        char::from_u32(
                                            0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00),
                                        )
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                char::from_u32(cp)
                            };
                            out.push(c.ok_or_else(|| "bad \\u escape".to_string())?);
                            self.pos += 1;
                            continue;
                        }
                        _ => return Err(format!("bad escape at byte {}", self.pos)),
                    }
                    self.pos += 1;
                }
                Some(_) => {
                    // Consume one UTF-8 encoded char.
                    let rest = core::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| "invalid utf-8".to_string())?;
                    let c = rest.chars().next().unwrap();
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// Reads 4 hex digits following `\u`; leaves pos on the last digit.
    fn hex4(&mut self) -> Result<u32, String> {
        let start = self.pos + 1;
        if start + 4 > self.bytes.len() {
            return Err("truncated \\u escape".to_string());
        }
        let hex = core::str::from_utf8(&self.bytes[start..start + 4])
            .map_err(|_| "bad \\u escape".to_string())?;
        let cp = u32::from_str_radix(hex, 16).map_err(|_| "bad \\u escape".to_string())?;
        self.pos = start + 3;
        Ok(cp)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = vec![];
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Arr(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut pairs = vec![];
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Obj(pairs));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.value()?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Obj(pairs));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":[1,2.5,-3],"b":"hi\n\"there\"","c":{"d":null,"e":true},"f":[]}"#;
        let v = parse(src).unwrap();
        assert_eq!(parse(&v.dump()).unwrap(), v);
        assert_eq!(v.get("b").unwrap().as_str().unwrap(), "hi\n\"there\"");
        assert_eq!(v.get("a").unwrap().as_arr().unwrap()[0].as_f64().unwrap(), 1.0);
    }

    #[test]
    fn unicode_escapes() {
        let v = parse(r#""é 😀""#).unwrap();
        assert_eq!(v.as_str().unwrap(), "é 😀");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("{\"a\" 1}").is_err());
        assert!(parse("1 2").is_err());
    }

    // Adversarial vectors from the design-thread review (forum post 14).

    #[test]
    fn deep_nesting_errors_instead_of_overflowing() {
        let deep = "[".repeat(10_000);
        assert!(parse(&deep).unwrap_err().contains("nesting"));
        let deep_obj = "{\"a\":".repeat(10_000);
        assert!(parse(&deep_obj).unwrap_err().contains("nesting"));
        // 100 levels is fine.
        let ok = "[".repeat(100) + &"]".repeat(100);
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn lone_surrogates_error_not_panic() {
        assert!(parse(r#""\uD800""#).is_err());
        assert!(parse(r#""\uDC00""#).is_err());
        assert!(parse(r#""\uD800\uD800""#).is_err());
        // A valid pair still works.
        assert_eq!(parse(r#""😀""#).unwrap().as_str().unwrap(), "😀");
    }

    #[test]
    fn number_edge_cases() {
        assert!(parse("1e999").is_err(), "overflow to inf must be rejected");
        assert_eq!(parse("-0").unwrap().dump(), "0");
        // > 2^53: precision may be lost but must not panic or emit garbage.
        let v = parse("9007199254740993").unwrap();
        assert!(parse(&v.dump()).is_ok());
        // Non-finite injected programmatically never serializes as inf/nan.
        assert_eq!(Value::Num(f64::INFINITY).dump(), "null");
        assert_eq!(Value::Num(f64::NAN).dump(), "null");
    }

    #[test]
    fn backslash_pileups_roundtrip() {
        for s in [r"\\", r#"\""#, r"\\\", "a\\\"b\\\\c", "\\u0041 literal"] {
            let v = Value::str(s);
            assert_eq!(parse(&v.dump()).unwrap(), v, "pileup {:?}", s);
        }
    }

    #[test]
    fn control_chars_escaped_on_output() {
        for c in (0u8..0x20).map(|b| b as char) {
            let dumped = Value::str(&c.to_string()).dump();
            assert!(
                dumped.bytes().all(|b| b >= 0x20),
                "raw control byte {:#x} leaked into output {:?}",
                c as u32,
                dumped
            );
            assert_eq!(parse(&dumped).unwrap().as_str().unwrap(), c.to_string());
        }
    }
}
