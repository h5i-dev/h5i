/// Minimal JSON parser — handles objects, arrays, strings, numbers, bool, null.
#[derive(Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

pub fn parse(input: &str) -> Result<Value, String> {
    let mut chars = input.trim().chars().peekable();
    parse_value(&mut chars)
}

fn parse_value(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<Value, String> {
    skip_ws(it);
    match it.peek().copied() {
        Some('"')       => parse_string(it).map(Value::Str),
        Some('{')       => parse_object(it),
        Some('[')       => parse_array(it),
        Some('t')       => expect(it, "true").map(|_| Value::Bool(true)),
        Some('f')       => expect(it, "false").map(|_| Value::Bool(false)),
        Some('n')       => expect(it, "null").map(|_| Value::Null),
        Some(c) if c == '-' || c.is_ascii_digit() => parse_number(it),
        other           => Err(format!("unexpected {:?}", other)),
    }
}

fn skip_ws(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) {
    while it.peek().map(|c| c.is_whitespace()).unwrap_or(false) { it.next(); }
}

fn expect(it: &mut std::iter::Peekable<impl Iterator<Item = char>>, s: &str) -> Result<(), String> {
    for ch in s.chars() {
        if it.next() != Some(ch) { return Err(format!("expected '{}'", s)); }
    }
    Ok(())
}

fn parse_string(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<String, String> {
    it.next(); // consume '"'
    let mut s = String::new();
    loop {
        match it.next() {
            Some('"') => break,
            Some('\\') => { s.push(it.next().ok_or("eof in escape")?); }
            Some(c)   => s.push(c),
            None      => return Err("unterminated string".into()),
        }
    }
    Ok(s)
}

fn parse_number(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<Value, String> {
    let mut buf = String::new();
    while it.peek().map(|c| matches!(c, '-'|'+'|'.'|'e'|'E') || c.is_ascii_digit()).unwrap_or(false) {
        buf.push(it.next().unwrap());
    }
    buf.parse::<f64>().map(Value::Number).map_err(|e| e.to_string())
}

fn parse_array(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<Value, String> {
    it.next(); // '['
    let mut items = vec![];
    skip_ws(it);
    if it.peek() == Some(&']') { it.next(); return Ok(Value::Array(items)); }
    loop {
        items.push(parse_value(it)?);
        skip_ws(it);
        match it.next() { Some(']') => break, Some(',') => {}, _ => return Err("expected , or ]".into()) }
    }
    Ok(Value::Array(items))
}

fn parse_object(it: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<Value, String> {
    it.next(); // '{'
    let mut pairs = vec![];
    skip_ws(it);
    if it.peek() == Some(&'}') { it.next(); return Ok(Value::Object(pairs)); }
    loop {
        skip_ws(it);
        let key = parse_string(it)?;
        skip_ws(it);
        if it.next() != Some(':') { return Err("expected ':'".into()); }
        let val = parse_value(it)?;
        pairs.push((key, val));
        skip_ws(it);
        match it.next() { Some('}') => break, Some(',') => {}, _ => return Err("expected , or }".into()) }
    }
    Ok(Value::Object(pairs))
}
