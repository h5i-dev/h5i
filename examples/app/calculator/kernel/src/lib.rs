//! Tutorial 1: a calculator with one memory per user.
//!
//! The kernel: Rust in the subset Aeneas translates to Lean (no `?`, no
//! `String`, `while` loops over indices). The proofs are about this code.

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

// One table, `memories`, keyed by user. `schema!` generates the structs,
// table operations and the server's table mapping.
h5i_app_schema::schema! {
    mapping calc_tables for calculator_kernel, lean "../proofs/generated/Schema.lean";

    /// One tenant's state: every user's memory.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        memories: Vec<Memory>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Memory in "memories" {
        key { user: u64 }
        value: u64,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Store `value` in my memory.
    Set { value: u64 },
    /// Replace my memory `m` with `m op arg`.
    Apply { op: Op, arg: u64 },
    /// Read my memory.
    Get,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Value(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Overflow,
    Underflow,
    DivByZero,
}

/// My memory; 0 if I never stored anything.
pub fn memory_of(ms: &Vec<Memory>, user: u64) -> u64 {
    let mut i = 0;
    while i < ms.len() {
        if ms[i].user == user {
            return ms[i].value;
        }
        i += 1;
    }
    0
}

/// `a op b` on natural numbers below 2^64, or why it has no such result.
pub fn compute(op: Op, a: u64, b: u64) -> Result<u64, Error> {
    match op {
        Op::Add => {
            if a > u64::MAX - b {
                Err(Error::Overflow)
            } else {
                Ok(a + b)
            }
        }
        Op::Sub => {
            if a < b {
                Err(Error::Underflow)
            } else {
                Ok(a - b)
            }
        }
        Op::Mul => {
            if b != 0 && a > u64::MAX / b {
                Err(Error::Overflow)
            } else {
                Ok(a * b)
            }
        }
        Op::Div => {
            if b == 0 {
                Err(Error::DivByZero)
            } else {
                Ok(a / b)
            }
        }
    }
}

/// The app logic: the write (at most one memory) and the reply, or a refusal
/// that commits nothing.
pub fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Option<Memory>, Reply), Error> {
    match cmd {
        Command::Set { value } => Ok((Some(Memory { user: actor.user, value: *value }), Reply::Value(*value))),
        Command::Apply { op, arg } => {
            let m = memory_of(&snap.memories, actor.user);
            match compute(*op, m, *arg) {
                Ok(v) => Ok((Some(Memory { user: actor.user, value: v }), Reply::Value(v))),
                Err(e) => Err(e),
            }
        }
        Command::Get => Ok((None, Reply::Value(memory_of(&snap.memories, actor.user)))),
    }
}

/// Commits a write set. `Memory::put` (from `schema!`) replaces the row with
/// `m`'s user, or appends it.
pub fn apply(snap: &Snapshot, w: &Option<Memory>) -> Snapshot {
    let mut s = snap.clone();
    match w {
        Some(m) => Memory::put(&mut s.memories, *m),
        None => {}
    }
    s
}

/// The table writes the server stores for a write set.
pub fn sql_writes(w: &Option<Memory>) -> Vec<h5i_app_sql::Write> {
    let mut out = Vec::new();
    match w {
        Some(m) => out.push(m.sql_put()),
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<u64, Error>) {
        match transition(&Principal { org: 1, user }, s, &c) {
            Ok((w, Reply::Value(v))) => (apply(s, &w), Ok(v)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    #[test]
    fn memories_are_per_user() {
        let s = Snapshot::default();
        let (s, _) = run(&s, 1, Command::Set { value: 6 });
        let (s, r) = run(&s, 1, Command::Apply { op: Op::Mul, arg: 7 });
        assert_eq!(r, Ok(42));
        assert_eq!(run(&s, 2, Command::Get).1, Ok(0));
        assert_eq!(run(&s, 1, Command::Apply { op: Op::Div, arg: 0 }).1, Err(Error::DivByZero));
        assert_eq!(run(&s, 1, Command::Apply { op: Op::Sub, arg: 43 }).1, Err(Error::Underflow));
    }
}
