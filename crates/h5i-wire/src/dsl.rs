//! A bounded evaluator for the subset of Nuclei's DSL that appears in matchers.
//!
//! Nuclei's matcher DSL is not arbitrary code: it is a pure expression language
//! over the response, with no I/O, no exec and no network. That is why it can
//! live in the shareable `expect` layer (design-flow-and-verdict.md, source 2)
//! rather than an oracle. This module parses one expression and evaluates it
//! against a response, and it refuses anything outside its known variables and
//! functions rather than guess, so an expression it accepts is one it evaluates
//! faithfully and one that reads only the response in front of it.
//!
//! Deliberately excluded: every impure or side-effecting function (`rand_*`,
//! `unix_time`, out-of-band/interactsh helpers, gadget generators). Their
//! presence makes an expression non-deterministic or non-local, so the parser
//! refuses it, and the caller falls back to refusing the template.

use std::fmt::Write as _;

/// A parsed, validated DSL expression, ready to evaluate against a response.
#[derive(Debug, Clone)]
pub struct Program {
    root: Expr,
    /// The source, kept for a readable verdict line.
    src: String,
}

#[derive(Debug, Clone)]
enum Expr {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Var(String),
    Call(String, Vec<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// The response an expression may read.
pub struct Env<'a> {
    pub status: Option<u16>,
    pub headers: &'a [(String, String)],
    pub body: &'a str,
}

/// A runtime value.
#[derive(Debug, Clone)]
enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    fn as_string(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
        }
    }

    fn as_bool(&self) -> Result<bool, String> {
        match self {
            Value::Bool(b) => Ok(*b),
            other => Err(format!("expected a boolean, got {}", other.type_name())),
        }
    }

    fn as_f64(&self) -> Result<f64, String> {
        match self {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(f) => Ok(*f),
            other => Err(format!("expected a number, got {}", other.type_name())),
        }
    }

    fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::Bool(_) => "boolean",
        }
    }
}

impl Program {
    /// Parse and validate an expression. Errors on unknown syntax, an unknown
    /// variable, or an unsupported function.
    pub fn parse(src: &str) -> Result<Self, String> {
        let tokens = lex(src)?;
        let mut parser = Parser { tokens, pos: 0 };
        let root = parser.expression()?;
        parser.expect_end()?;
        validate(&root)?;
        Ok(Program {
            root,
            src: src.to_string(),
        })
    }

    /// Evaluate to a boolean. A matcher expression must yield a boolean; an
    /// expression that evaluates to anything else is an error, not a silent pass.
    pub fn eval(&self, env: &Env) -> Result<bool, String> {
        eval(&self.root, env)?.as_bool()
    }

    /// The source, for a verdict line.
    pub fn source(&self) -> &str {
        &self.src
    }
}

// ── the functions and variables this evaluator knows ────────────────────────

/// Every function name the evaluator implements. A call to anything else is
/// refused at parse time, which is what keeps an accepted expression pure.
fn known_function(name: &str) -> bool {
    matches!(
        name,
        "regex"
            | "contains"
            | "contains_all"
            | "contains_any"
            | "icontains"
            | "tolower"
            | "to_lower"
            | "toupper"
            | "to_upper"
            | "len"
            | "startswith"
            | "starts_with"
            | "endswith"
            | "ends_with"
            | "trim"
            | "trim_space"
            | "trim_left"
            | "trim_right"
            | "trim_prefix"
            | "trim_suffix"
            | "replace"
            | "concat"
            | "tostring"
            | "to_string"
            | "hex_encode"
            | "base64"
            | "base64_encode"
            | "base64_py"
            | "base64_decode"
            | "md5"
            | "sha1"
            | "sha256"
            | "mmh3"
            | "compare_versions"
    )
}

/// Every variable the evaluator binds from a response.
fn known_variable(name: &str) -> bool {
    matches!(
        name,
        "body" | "all_headers" | "header" | "status_code" | "content_length"
    )
}

fn validate(expr: &Expr) -> Result<(), String> {
    match expr {
        Expr::Str(_) | Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) => Ok(()),
        Expr::Var(name) => {
            if known_variable(name) {
                Ok(())
            } else {
                Err(format!("unknown DSL variable `{name}`"))
            }
        }
        Expr::Call(name, args) => {
            if !known_function(name) {
                return Err(format!("unsupported DSL function `{name}`"));
            }
            for arg in args {
                validate(arg)?;
            }
            Ok(())
        }
        Expr::Not(inner) | Expr::Neg(inner) => validate(inner),
        Expr::Bin(_, a, b) => {
            validate(a)?;
            validate(b)
        }
    }
}

// ── evaluation ──────────────────────────────────────────────────────────────

fn eval(expr: &Expr, env: &Env) -> Result<Value, String> {
    match expr {
        Expr::Str(s) => Ok(Value::Str(s.clone())),
        Expr::Int(n) => Ok(Value::Int(*n)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Var(name) => Ok(variable(name, env)),
        Expr::Not(inner) => Ok(Value::Bool(!eval(inner, env)?.as_bool()?)),
        Expr::Neg(inner) => match eval(inner, env)? {
            Value::Int(n) => Ok(Value::Int(-n)),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(format!("cannot negate {}", other.type_name())),
        },
        Expr::Call(name, args) => call(name, args, env),
        Expr::Bin(op, a, b) => binary(*op, a, b, env),
    }
}

fn variable(name: &str, env: &Env) -> Value {
    match name {
        "body" => Value::Str(env.body.to_string()),
        "all_headers" | "header" => Value::Str(header_block(env.headers)),
        "status_code" => Value::Int(env.status.map(i64::from).unwrap_or(0)),
        "content_length" => Value::Int(env.body.len() as i64),
        // validate() guarantees the name is known.
        _ => Value::Str(String::new()),
    }
}

fn header_block(headers: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        let _ = writeln!(out, "{name}: {value}");
    }
    out
}

fn binary(op: BinOp, a: &Expr, b: &Expr, env: &Env) -> Result<Value, String> {
    // Short-circuit the logical operators before evaluating the right side.
    match op {
        BinOp::And => return Ok(Value::Bool(eval(a, env)?.as_bool()? && eval(b, env)?.as_bool()?)),
        BinOp::Or => return Ok(Value::Bool(eval(a, env)?.as_bool()? || eval(b, env)?.as_bool()?)),
        _ => {}
    }
    let left = eval(a, env)?;
    let right = eval(b, env)?;
    match op {
        BinOp::Eq => Ok(Value::Bool(equal(&left, &right))),
        BinOp::Ne => Ok(Value::Bool(!equal(&left, &right))),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ok(Value::Bool(order(op, &left, &right)?)),
        BinOp::Add => {
            // `+` concatenates when either side is a string, else it adds.
            if matches!(left, Value::Str(_)) || matches!(right, Value::Str(_)) {
                Ok(Value::Str(format!("{}{}", left.as_string(), right.as_string())))
            } else {
                arithmetic(op, &left, &right)
            }
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => arithmetic(op, &left, &right),
        BinOp::And | BinOp::Or => unreachable!("handled above"),
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    if a.is_number() && b.is_number() {
        return a.as_f64().unwrap() == b.as_f64().unwrap();
    }
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => a.as_string() == b.as_string(),
    }
}

fn order(op: BinOp, a: &Value, b: &Value) -> Result<bool, String> {
    let ordering = if a.is_number() && b.is_number() {
        a.as_f64()?
            .partial_cmp(&b.as_f64()?)
            .ok_or_else(|| "cannot order these numbers".to_string())?
    } else {
        a.as_string().cmp(&b.as_string())
    };
    Ok(match op {
        BinOp::Lt => ordering.is_lt(),
        BinOp::Le => ordering.is_le(),
        BinOp::Gt => ordering.is_gt(),
        BinOp::Ge => ordering.is_ge(),
        _ => unreachable!(),
    })
}

fn arithmetic(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    // Integer arithmetic when both are integers, so `len(body) % 2` stays exact.
    if let (Value::Int(x), Value::Int(y)) = (a, b) {
        return Ok(Value::Int(match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div if *y != 0 => x / y,
            BinOp::Rem if *y != 0 => x % y,
            BinOp::Div | BinOp::Rem => return Err("division by zero".to_string()),
            _ => unreachable!(),
        }));
    }
    let (x, y) = (a.as_f64()?, b.as_f64()?);
    Ok(Value::Float(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
        BinOp::Rem => x % y,
        _ => unreachable!(),
    }))
}

fn call(name: &str, args: &[Expr], env: &Env) -> Result<Value, String> {
    let values: Vec<Value> = args.iter().map(|a| eval(a, env)).collect::<Result<_, _>>()?;
    let s = |i: usize| values[i].as_string();
    let arity = |min: usize| -> Result<(), String> {
        if values.len() < min {
            Err(format!("`{name}` needs at least {min} argument(s)"))
        } else {
            Ok(())
        }
    };
    match name {
        "regex" => {
            arity(2)?;
            let re = regex::Regex::new(&s(0)).map_err(|e| format!("`{}` is not a regex: {e}", s(0)))?;
            Ok(Value::Bool(re.is_match(&s(1))))
        }
        "contains" => {
            arity(2)?;
            Ok(Value::Bool(s(0).contains(&s(1))))
        }
        "icontains" => {
            arity(2)?;
            Ok(Value::Bool(s(0).to_lowercase().contains(&s(1).to_lowercase())))
        }
        "contains_all" => {
            arity(2)?;
            Ok(Value::Bool(values[1..].iter().all(|v| s(0).contains(&v.as_string()))))
        }
        "contains_any" => {
            arity(2)?;
            Ok(Value::Bool(values[1..].iter().any(|v| s(0).contains(&v.as_string()))))
        }
        "tolower" | "to_lower" => {
            arity(1)?;
            Ok(Value::Str(s(0).to_lowercase()))
        }
        "toupper" | "to_upper" => {
            arity(1)?;
            Ok(Value::Str(s(0).to_uppercase()))
        }
        "len" => {
            arity(1)?;
            Ok(Value::Int(s(0).len() as i64))
        }
        "startswith" | "starts_with" => {
            arity(2)?;
            Ok(Value::Bool(s(0).starts_with(&s(1))))
        }
        "endswith" | "ends_with" => {
            arity(2)?;
            Ok(Value::Bool(s(0).ends_with(&s(1))))
        }
        "trim" => {
            arity(2)?;
            let cutset: Vec<char> = s(1).chars().collect();
            Ok(Value::Str(s(0).trim_matches(cutset.as_slice()).to_string()))
        }
        "trim_left" => {
            arity(2)?;
            let cutset: Vec<char> = s(1).chars().collect();
            Ok(Value::Str(s(0).trim_start_matches(cutset.as_slice()).to_string()))
        }
        "trim_right" => {
            arity(2)?;
            let cutset: Vec<char> = s(1).chars().collect();
            Ok(Value::Str(s(0).trim_end_matches(cutset.as_slice()).to_string()))
        }
        "trim_prefix" => {
            arity(2)?;
            Ok(Value::Str(s(0).strip_prefix(&s(1)).unwrap_or(&s(0)).to_string()))
        }
        "trim_suffix" => {
            arity(2)?;
            Ok(Value::Str(s(0).strip_suffix(&s(1)).unwrap_or(&s(0)).to_string()))
        }
        "trim_space" => {
            arity(1)?;
            Ok(Value::Str(s(0).trim().to_string()))
        }
        "replace" => {
            arity(3)?;
            Ok(Value::Str(s(0).replace(&s(1), &s(2))))
        }
        "concat" => {
            arity(1)?;
            Ok(Value::Str(values.iter().map(Value::as_string).collect()))
        }
        "tostring" | "to_string" => {
            arity(1)?;
            Ok(Value::Str(s(0)))
        }
        "hex_encode" => {
            arity(1)?;
            Ok(Value::Str(hex(s(0).as_bytes())))
        }
        "base64" | "base64_encode" => {
            arity(1)?;
            use base64::Engine as _;
            Ok(Value::Str(base64::engine::general_purpose::STANDARD.encode(s(0).as_bytes())))
        }
        "base64_py" => {
            arity(1)?;
            use base64::Engine as _;
            // Python's base64.encodebytes: standard base64 wrapped at 76 columns
            // with a trailing newline. This is the exact input a favicon `mmh3`
            // hash is taken over, so the variant matters.
            let raw = base64::engine::general_purpose::STANDARD.encode(s(0).as_bytes());
            let mut out = String::new();
            for chunk in raw.as_bytes().chunks(76) {
                out.push_str(std::str::from_utf8(chunk).unwrap());
                out.push('\n');
            }
            Ok(Value::Str(out))
        }
        "base64_decode" => {
            arity(1)?;
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(s(0).as_bytes())
                .map_err(|e| format!("`base64_decode` got invalid base64: {e}"))?;
            Ok(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
        }
        "md5" => {
            arity(1)?;
            use md5::Digest as _;
            Ok(Value::Str(hex(&md5::Md5::digest(s(0).as_bytes()))))
        }
        "sha1" => {
            arity(1)?;
            use sha1::Digest as _;
            Ok(Value::Str(hex(&sha1::Sha1::digest(s(0).as_bytes()))))
        }
        "sha256" => {
            arity(1)?;
            use sha2::Digest as _;
            Ok(Value::Str(hex(&sha2::Sha256::digest(s(0).as_bytes()))))
        }
        "mmh3" => {
            arity(1)?;
            // Nuclei returns the 32-bit MurmurHash3 as a signed integer string,
            // which is what a favicon-hash matcher compares against.
            Ok(Value::Str((murmur3_32(s(0).as_bytes()) as i32).to_string()))
        }
        "compare_versions" => {
            arity(2)?;
            let version = s(0);
            let ok = values[1..]
                .iter()
                .map(|c| constraint_holds(&version, &c.as_string()))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .all(|held| held);
            Ok(Value::Bool(ok))
        }
        // validate() guarantees the name is known.
        _ => Err(format!("unsupported DSL function `{name}`")),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// MurmurHash3 x86 32-bit, seed 0: the variant Nuclei's `mmh3` uses.
fn murmur3_32(data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let mut h: u32 = 0;
    let blocks = data.len() / 4;
    for i in 0..blocks {
        let mut k = u32::from_le_bytes([data[i * 4], data[i * 4 + 1], data[i * 4 + 2], data[i * 4 + 3]]);
        k = k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k;
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    let tail = &data[blocks * 4..];
    let mut k1: u32 = 0;
    if tail.len() >= 3 {
        k1 ^= u32::from(tail[2]) << 16;
    }
    if tail.len() >= 2 {
        k1 ^= u32::from(tail[1]) << 8;
    }
    if !tail.is_empty() {
        k1 ^= u32::from(tail[0]);
        k1 = k1.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k1;
    }
    h ^= data.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// Whether a version satisfies one constraint such as `>=1.2.0` or `<2.0`.
fn constraint_holds(version: &str, constraint: &str) -> Result<bool, String> {
    let constraint = constraint.trim();
    let (op, want) = if let Some(rest) = constraint.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = constraint.strip_prefix("<=") {
        ("<=", rest)
    } else if let Some(rest) = constraint.strip_prefix("!=") {
        ("!=", rest)
    } else if let Some(rest) = constraint.strip_prefix("==") {
        ("==", rest)
    } else if let Some(rest) = constraint.strip_prefix('>') {
        (">", rest)
    } else if let Some(rest) = constraint.strip_prefix('<') {
        ("<", rest)
    } else if let Some(rest) = constraint.strip_prefix('=') {
        ("==", rest)
    } else {
        ("==", constraint)
    };
    let ordering = compare_version_strings(version, want.trim());
    Ok(match op {
        ">=" => ordering.is_ge(),
        "<=" => ordering.is_le(),
        ">" => ordering.is_gt(),
        "<" => ordering.is_lt(),
        "==" => ordering.is_eq(),
        "!=" => ordering.is_ne(),
        _ => return Err(format!("unknown version constraint `{constraint}`")),
    })
}

/// Compare two dotted versions field by field, numerically, padding the shorter
/// with zeros. Non-numeric fields compare as zero, which is enough for the
/// numeric version constraints Nuclei templates use.
fn compare_version_strings(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(v: &str) -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap_or(0))
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

// ── lexer ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Int(i64),
    Float(f64),
    Ident(String),
    Bool(bool),
    AndAnd,
    OrOr,
    Not,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '\'' | '"' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= chars.len() {
                        return Err("a string is never closed".to_string());
                    }
                    let ch = chars[i];
                    if ch == '\\' && i + 1 < chars.len() {
                        // Minimal escapes: the quote, a backslash, and the common
                        // whitespace escapes a pattern might carry.
                        let next = chars[i + 1];
                        s.push(match next {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            other => other,
                        });
                        i += 2;
                        continue;
                    }
                    if ch == quote {
                        i += 1;
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                out.push(Tok::Str(s));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                let mut float = false;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    if chars[i] == '.' {
                        float = true;
                    }
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if float {
                    out.push(Tok::Float(text.parse().map_err(|_| format!("`{text}` is not a number"))?));
                } else {
                    out.push(Tok::Int(text.parse().map_err(|_| format!("`{text}` is not a number"))?));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "true" => out.push(Tok::Bool(true)),
                    "false" => out.push(Tok::Bool(false)),
                    _ => out.push(Tok::Ident(word)),
                }
            }
            '&' if peek(&chars, i + 1) == Some('&') => {
                out.push(Tok::AndAnd);
                i += 2;
            }
            '|' if peek(&chars, i + 1) == Some('|') => {
                out.push(Tok::OrOr);
                i += 2;
            }
            '=' if peek(&chars, i + 1) == Some('=') => {
                out.push(Tok::EqEq);
                i += 2;
            }
            '!' if peek(&chars, i + 1) == Some('=') => {
                out.push(Tok::NotEq);
                i += 2;
            }
            '<' if peek(&chars, i + 1) == Some('=') => {
                out.push(Tok::Le);
                i += 2;
            }
            '>' if peek(&chars, i + 1) == Some('=') => {
                out.push(Tok::Ge);
                i += 2;
            }
            '!' => {
                out.push(Tok::Not);
                i += 1;
            }
            '<' => {
                out.push(Tok::Lt);
                i += 1;
            }
            '>' => {
                out.push(Tok::Gt);
                i += 1;
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            other => return Err(format!("unexpected character `{other}`")),
        }
    }
    Ok(out)
}

fn peek(chars: &[char], i: usize) -> Option<char> {
    chars.get(i).copied()
}

// ── parser (precedence climbing) ─────────────────────────────────────────────

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_end(&self) -> Result<(), String> {
        if self.pos == self.tokens.len() {
            Ok(())
        } else {
            Err("trailing characters after the expression".to_string())
        }
    }

    fn expression(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut left = self.and_expr()?;
        while self.eat(&Tok::OrOr) {
            let right = self.and_expr()?;
            left = Expr::Bin(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut left = self.equality()?;
        while self.eat(&Tok::AndAnd) {
            let right = self.equality()?;
            left = Expr::Bin(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Expr, String> {
        let mut left = self.comparison()?;
        loop {
            let op = match self.peek() {
                Some(Tok::EqEq) => BinOp::Eq,
                Some(Tok::NotEq) => BinOp::Ne,
                _ => break,
            };
            self.pos += 1;
            let right = self.comparison()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Ge) => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.additive()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.multiplicative()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(&Tok::Not) {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat(&Tok::Minus) {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::Int(n)) => Ok(Expr::Int(n)),
            Some(Tok::Float(f)) => Ok(Expr::Float(f)),
            Some(Tok::Bool(b)) => Ok(Expr::Bool(b)),
            Some(Tok::LParen) => {
                let inner = self.expression()?;
                if !self.eat(&Tok::RParen) {
                    return Err("a `(` is never closed".to_string());
                }
                Ok(inner)
            }
            Some(Tok::Ident(name)) => {
                if self.eat(&Tok::LParen) {
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.expression()?);
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            break;
                        }
                    }
                    if !self.eat(&Tok::RParen) {
                        return Err(format!("the call to `{name}` is never closed"));
                    }
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(status: Option<u16>, headers: &'a [(String, String)], body: &'a str) -> Env<'a> {
        Env {
            status,
            headers,
            body,
        }
    }

    fn run(src: &str, status: Option<u16>, headers: &[(String, String)], body: &str) -> bool {
        Program::parse(src)
            .expect("parse")
            .eval(&env(status, headers, body))
            .expect("eval")
    }

    #[test]
    fn status_and_body_predicates() {
        assert!(run("status_code == 200", Some(200), &[], ""));
        assert!(!run("status_code == 200", Some(404), &[], ""));
        assert!(run("contains(body, 'admin')", None, &[], "the admin page"));
        assert!(run("len(body) > 3", None, &[], "abcd"));
        assert!(!run("len(body) > 3", None, &[], "ab"));
    }

    #[test]
    fn the_git_config_shape_evaluates() {
        // The exact idiom that made git-config a dsl template.
        let src = "!contains(tolower(body), '<html') && !contains(tolower(body), '<body') && status_code == 200";
        assert!(run(src, Some(200), &[], "[core]\nrepositoryformatversion = 0"));
        assert!(!run(src, Some(200), &[], "<HTML><body>not found</body></HTML>"));
        assert!(!run(src, Some(404), &[], "[core]"));
    }

    #[test]
    fn boolean_operators_and_precedence() {
        assert!(run("status_code == 200 || status_code == 302", Some(302), &[], ""));
        assert!(run("true && (false || true)", None, &[], ""));
        assert!(!run("true && false || false", None, &[], ""));
    }

    #[test]
    fn regex_and_contains_family() {
        assert!(run("regex('v[0-9]+', body)", None, &[], "app v12"));
        assert!(run("contains_all(body, 'a', 'b')", None, &[], "a and b"));
        assert!(!run("contains_all(body, 'a', 'z')", None, &[], "a and b"));
        assert!(run("contains_any(body, 'z', 'b')", None, &[], "a and b"));
    }

    #[test]
    fn headers_are_readable() {
        let headers = vec![("Server".to_string(), "nginx".to_string())];
        assert!(run("contains(all_headers, 'Server: nginx')", None, &headers, ""));
    }

    #[test]
    fn an_unknown_function_is_refused_at_parse() {
        let err = Program::parse("rand_int(1, 9) == 5").unwrap_err();
        assert!(err.contains("unsupported DSL function `rand_int`"), "{err}");
    }

    #[test]
    fn an_unknown_variable_is_refused_at_parse() {
        let err = Program::parse("interactsh_protocol == 'dns'").unwrap_err();
        assert!(err.contains("unknown DSL variable `interactsh_protocol`"), "{err}");
    }

    #[test]
    fn a_non_boolean_expression_is_an_error() {
        let err = Program::parse("len(body)")
            .unwrap()
            .eval(&env(None, &[], "abc"))
            .unwrap_err();
        assert!(err.contains("expected a boolean"), "{err}");
    }

    #[test]
    fn hashing_and_encoding_functions() {
        // Known vectors.
        assert!(run("md5('abc') == '900150983cd24fb0d6963f7d28e17f72'", None, &[], ""));
        assert!(run(
            "sha256('abc') == 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'",
            None,
            &[],
            "",
        ));
        assert!(run("hex_encode('AB') == '4142'", None, &[], ""));
        assert!(run("base64('hi') == 'aGk='", None, &[], ""));
        assert!(run("base64_decode('aGk=') == 'hi'", None, &[], ""));
    }

    #[test]
    fn mmh3_matches_the_known_hello_vector() {
        // MurmurHash3 x86 32-bit, seed 0, of "hello" is 613153351.
        assert!(run("mmh3('hello') == '613153351'", None, &[], ""));
    }

    #[test]
    fn compare_versions_understands_constraints() {
        assert!(run("compare_versions('1.2.3', '>=1.0.0', '<2.0.0')", None, &[], ""));
        assert!(!run("compare_versions('2.5.0', '<2.0.0')", None, &[], ""));
        assert!(run("compare_versions('1.2.3', '==1.2.3')", None, &[], ""));
        assert!(run("compare_versions('1.2', '<1.10')", None, &[], ""));
    }

    #[test]
    fn syntax_errors_are_refused() {
        assert!(Program::parse("status_code ==").is_err());
        assert!(Program::parse("contains(body, 'x'").is_err());
        assert!(Program::parse("&& true").is_err());
    }
}
