//! Saved filters. An admin stores filter templates such as `owner='$'`; a
//! user searches records through one. `substitute` writes the caller's name
//! where `$` is, escaped, and `parse` reads the filter back. Names are any
//! bytes, so a search shows only the caller's records exactly when `parse`
//! undoes the escaping `substitute` does.
//!
//! `substitute_pre` escapes backslashes but not quotes, the kind of mismatch
//! between a template substituter and a filter parser that lets a name
//! rewrite the filter.
//!
//! Text is `Vec<u8>`: Aeneas has no model of `String`.

pub const QUOTE: u8 = b'\'';
pub const BSLASH: u8 = b'\\';
pub const BAR: u8 = b'|';
pub const EQ: u8 = b'=';
pub const HOLE: u8 = b'$';
/// Longest template and name a search accepts.
pub const MAX_LEN: usize = 4096;

/// `key='val'`: a record matches if its field `key` equals `val`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    pub key: Vec<u8>,
    pub val: Vec<u8>,
}

/// The authenticated caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub name: Vec<u8>,
    pub is_admin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: u64,
    pub owner: Vec<u8>,
    pub tag: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: u64,
    pub template: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub rules: Vec<Rule>,
    pub records: Vec<Record>,
    pub next_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Records matching rule `rule` with the caller's name substituted.
    Search { rule: u64 },
    /// A record owned by the caller.
    Add { tag: Vec<u8> },
    /// Store a template (admins only).
    SetRule { id: u64, template: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Record(Record),
    Rule(Rule),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Records(Vec<Record>),
    Added(u64),
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoRule,
    BadFilter,
    NotAdmin,
    Full,
    TooLong,
}

/// Append `v`, escaping `\` and `'` with a backslash.
pub fn escape_into(out: &mut Vec<u8>, v: &[u8]) {
    for b in v.iter() {
        if *b == BSLASH || *b == QUOTE {
            out.push(BSLASH);
        }
        out.push(*b);
    }
}

/// Before the fix: escapes `\` but not `'`.
pub fn escape_into_pre(out: &mut Vec<u8>, v: &[u8]) {
    for b in v.iter() {
        if *b == BSLASH {
            out.push(BSLASH);
        }
        out.push(*b);
    }
}

/// The template with each `$` replaced by `name`, escaped.
pub fn substitute(template: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in template.iter() {
        if *b == HOLE {
            escape_into(&mut out, name);
        } else {
            out.push(*b);
        }
    }
    out
}

pub fn substitute_pre(template: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in template.iter() {
        if *b == HOLE {
            escape_into_pre(&mut out, name);
        } else {
            out.push(*b);
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum St {
    Key,
    Open,
    Val,
    Esc,
    Closed,
}

/// `clause ('|' clause)*` or nothing, where `clause` is `key='val'`, `key`
/// has no `=`, `'`, `|` or `\`, and `\` in `val` takes the next byte as is.
pub fn parse(s: &[u8]) -> Option<Vec<Clause>> {
    let mut out: Vec<Clause> = Vec::new();
    let mut key: Vec<u8> = Vec::new();
    let mut val: Vec<u8> = Vec::new();
    let mut st = St::Key;
    for b in s.iter() {
        let b = *b;
        match st {
            St::Key => {
                if b == EQ {
                    st = St::Open;
                } else if b == QUOTE || b == BAR || b == BSLASH {
                    return None;
                } else {
                    key.push(b);
                }
            }
            St::Open => {
                if b == QUOTE {
                    st = St::Val;
                } else {
                    return None;
                }
            }
            St::Val => {
                if b == BSLASH {
                    st = St::Esc;
                } else if b == QUOTE {
                    out.push(Clause { key, val });
                    key = Vec::new();
                    val = Vec::new();
                    st = St::Closed;
                } else {
                    val.push(b);
                }
            }
            St::Esc => {
                val.push(b);
                st = St::Val;
            }
            St::Closed => {
                if b == BAR {
                    st = St::Key;
                } else {
                    return None;
                }
            }
        }
    }
    match st {
        St::Closed => Some(out),
        St::Key => {
            if out.len() == 0 && key.len() == 0 {
                Some(out)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn field<'a>(r: &'a Record, key: &[u8]) -> Option<&'a Vec<u8>> {
    if key == b"owner".as_slice() {
        Some(&r.owner)
    } else if key == b"tag".as_slice() {
        Some(&r.tag)
    } else {
        None
    }
}

/// A record matches a filter if it matches one of its clauses.
pub fn matches(r: &Record, filter: &[Clause]) -> bool {
    for c in filter.iter() {
        match field(r, &c.key) {
            Some(v) => {
                if *v == c.val {
                    return true;
                }
            }
            None => {}
        }
    }
    false
}

pub fn select(records: &[Record], filter: &[Clause]) -> Vec<Record> {
    let mut out = Vec::new();
    for r in records.iter() {
        if matches(r, filter) {
            out.push(r.clone());
        }
    }
    out
}

fn find_rule(rules: &[Rule], id: u64) -> Option<&Rule> {
    for r in rules.iter() {
        if r.id == id {
            return Some(r);
        }
    }
    None
}

fn too_long(template: &[u8], name: &[u8]) -> bool {
    template.len() > MAX_LEN || name.len() > MAX_LEN
}

fn run_rule(name: &[u8], records: &[Record], template: &[u8], pre: bool) -> Result<(Vec<Write>, Reply), Error> {
    if too_long(template, name) {
        return Err(Error::TooLong);
    }
    let text = if pre { substitute_pre(template, name) } else { substitute(template, name) };
    match parse(&text) {
        None => Err(Error::BadFilter),
        Some(filter) => Ok((Vec::new(), Reply::Records(select(records, &filter)))),
    }
}

fn search(actor: &Principal, snap: &Snapshot, rule: u64, pre: bool) -> Result<(Vec<Write>, Reply), Error> {
    match find_rule(&snap.rules, rule) {
        None => Err(Error::NoRule),
        Some(r) => run_rule(&actor.name, &snap.records, &r.template, pre),
    }
}

fn step(actor: &Principal, snap: &Snapshot, cmd: &Command, pre: bool) -> Result<(Vec<Write>, Reply), Error> {
    match cmd {
        Command::Search { rule } => search(actor, snap, *rule, pre),
        Command::Add { tag } => {
            if snap.next_id == u64::MAX {
                return Err(Error::Full);
            }
            let rec = Record { id: snap.next_id, owner: actor.name.clone(), tag: tag.clone() };
            let mut ws = Vec::new();
            ws.push(Write::Record(rec));
            Ok((ws, Reply::Added(snap.next_id)))
        }
        Command::SetRule { id, template } => {
            if !actor.is_admin {
                return Err(Error::NotAdmin);
            }
            let mut ws = Vec::new();
            ws.push(Write::Rule(Rule { id: *id, template: template.clone() }));
            Ok((ws, Reply::Done))
        }
    }
}

pub fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, false)
}

/// The kernel with `substitute_pre`.
pub fn transition_pre(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, owner: &[u8]) -> Record {
        Record { id, owner: owner.to_vec(), tag: b"t".to_vec() }
    }

    #[test]
    fn round_trip() {
        let name = b"a\\'|owner='bob".to_vec();
        let text = substitute(b"owner='$'", &name);
        assert_eq!(parse(&text), Some(vec![Clause { key: b"owner".to_vec(), val: name }]));
    }

    #[test]
    fn pre_leaks() {
        let snap = Snapshot { rules: vec![Rule { id: 1, template: b"owner='$'".to_vec() }], records: vec![rec(1, b"bob")], next_id: 2 };
        let eve = Principal { name: b"x'|owner='bob".to_vec(), is_admin: false };
        let cmd = Command::Search { rule: 1 };
        assert_eq!(transition_pre(&eve, &snap, &cmd), Ok((vec![], Reply::Records(vec![rec(1, b"bob")]))));
        assert_eq!(transition(&eve, &snap, &cmd), Ok((vec![], Reply::Records(vec![]))));
    }
}
