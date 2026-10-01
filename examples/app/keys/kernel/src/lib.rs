//! API keys. An admin issues keys, each with a list of permissions, and
//! revokes them. A client acts with a key, or opens a session with one and
//! acts through the session. Keys are byte strings.
//!
//! `transition_pre` trusts the key and permissions a session copied when it
//! was opened: the key is checked once and used later, so a request through
//! an old session still succeeds after the key is revoked. `transition`
//! looks the key up again on every request.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    Read,
    Write,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub secret: Vec<u8>,
    pub perms: Vec<Perm>,
}

/// A session remembers the key it was opened with and, for `transition_pre`,
/// that key's permissions at the time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub secret: Vec<u8>,
    pub perms: Vec<Perm>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub keys: Vec<Key>,
    /// Secrets revoked so far; never issued again.
    pub revoked: Vec<Vec<u8>>,
    pub sessions: Vec<Session>,
    pub next_session: u64,
}

/// How the caller authenticated (from the shell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cred {
    Admin,
    Key(Vec<u8>),
    Session(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Issue { secret: Vec<u8>, perms: Vec<Perm> },
    Revoke { secret: Vec<u8> },
    Open,
    Act { action: Action },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutKey(Key),
    DelKey(Vec<u8>),
    Revoked(Vec<u8>),
    PutSession(Session),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Done,
    Opened(u64),
    /// The action ran on behalf of this key.
    Acted(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotAdmin,
    Taken,
    NoKey,
    Denied,
    Full,
}

pub fn permits(p: Perm, a: Action) -> bool {
    p == Perm::All || (p == Perm::Read && a == Action::Read) || (p == Perm::Write && a == Action::Write)
}

/// Some permission in the list allows `a`.
pub fn grants(perms: &[Perm], a: Action) -> bool {
    for p in perms.iter() {
        if permits(*p, a) {
            return true;
        }
    }
    false
}

fn find_key<'a>(keys: &'a [Key], secret: &Vec<u8>) -> Option<&'a Key> {
    for k in keys.iter() {
        if k.secret == *secret {
            return Some(k);
        }
    }
    None
}

fn is_revoked(revoked: &[Vec<u8>], secret: &Vec<u8>) -> bool {
    for r in revoked.iter() {
        if *r == *secret {
            return true;
        }
    }
    false
}

fn find_session(sessions: &[Session], id: u64) -> Option<&Session> {
    for s in sessions.iter() {
        if s.id == id {
            return Some(s);
        }
    }
    None
}

fn taken(snap: &Snapshot, secret: &Vec<u8>) -> bool {
    match find_key(&snap.keys, secret) {
        Some(_) => true,
        None => is_revoked(&snap.revoked, secret),
    }
}

fn live(keys: &[Key], secret: &Vec<u8>) -> Option<Key> {
    match find_key(keys, secret) {
        Some(k) => Some(k.clone()),
        None => None,
    }
}

/// The key a credential acts for, with its permissions.
fn resolve(snap: &Snapshot, cred: &Cred, pre: bool) -> Option<Key> {
    match cred {
        Cred::Admin => None,
        Cred::Key(secret) => live(&snap.keys, secret),
        Cred::Session(id) => match find_session(&snap.sessions, *id) {
            None => None,
            Some(s) => {
                if pre {
                    Some(Key { secret: s.secret.clone(), perms: s.perms.clone() })
                } else {
                    live(&snap.keys, &s.secret)
                }
            }
        },
    }
}

fn step(actor: &Cred, snap: &Snapshot, cmd: &Command, pre: bool) -> Result<(Vec<Write>, Reply), Error> {
    match cmd {
        Command::Issue { secret, perms } => match actor {
            Cred::Admin => {
                if taken(snap, secret) {
                    Err(Error::Taken)
                } else {
                    let mut ws = Vec::new();
                    ws.push(Write::PutKey(Key { secret: secret.clone(), perms: perms.clone() }));
                    Ok((ws, Reply::Done))
                }
            }
            _ => Err(Error::NotAdmin),
        },
        Command::Revoke { secret } => match actor {
            Cred::Admin => {
                let mut ws = Vec::new();
                ws.push(Write::DelKey(secret.clone()));
                ws.push(Write::Revoked(secret.clone()));
                Ok((ws, Reply::Done))
            }
            _ => Err(Error::NotAdmin),
        },
        Command::Open => match resolve(snap, actor, pre) {
            None => Err(Error::NoKey),
            Some(k) => {
                if snap.next_session == u64::MAX {
                    Err(Error::Full)
                } else {
                    let id = snap.next_session;
                    let mut ws = Vec::new();
                    ws.push(Write::PutSession(Session { id, secret: k.secret, perms: k.perms }));
                    Ok((ws, Reply::Opened(id)))
                }
            }
        },
        Command::Act { action } => match resolve(snap, actor, pre) {
            None => Err(Error::NoKey),
            Some(k) => {
                if grants(&k.perms, *action) {
                    Ok((Vec::new(), Reply::Acted(k.secret)))
                } else {
                    Err(Error::Denied)
                }
            }
        },
    }
}

pub fn transition(actor: &Cred, snap: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, false)
}

/// The kernel that trusts a session's copy of its key.
pub fn transition_pre(actor: &Cred, snap: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, true)
}

fn without(keys: &[Key], secret: &Vec<u8>) -> Vec<Key> {
    let mut out = Vec::new();
    for k in keys.iter() {
        if k.secret != *secret {
            out.push(k.clone());
        }
    }
    out
}

/// Commit a write set.
pub fn apply(snap: &Snapshot, ws: &[Write]) -> Snapshot {
    let mut s = snap.clone();
    for w in ws.iter() {
        match w {
            Write::PutKey(k) => s.keys.push(k.clone()),
            Write::DelKey(secret) => s.keys = without(&s.keys, secret),
            Write::Revoked(secret) => s.revoked.push(secret.clone()),
            Write::PutSession(x) => {
                s.sessions.push(x.clone());
                s.next_session = x.id.wrapping_add(1);
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pre: bool, s: &mut Snapshot, a: &Cred, c: &Command) -> Result<Reply, Error> {
        let (ws, r) = step(a, s, c, pre)?;
        *s = apply(s, &ws);
        Ok(r)
    }

    #[test]
    fn revoked_session() {
        for pre in [false, true] {
            let mut s = Snapshot::default();
            let k = b"k".to_vec();
            run(pre, &mut s, &Cred::Admin, &Command::Issue { secret: k.clone(), perms: vec![Perm::Write] }).unwrap();
            assert_eq!(run(pre, &mut s, &Cred::Key(k.clone()), &Command::Open), Ok(Reply::Opened(0)));
            run(pre, &mut s, &Cred::Admin, &Command::Revoke { secret: k.clone() }).unwrap();
            let r = run(pre, &mut s, &Cred::Session(0), &Command::Act { action: Action::Write });
            if pre {
                assert_eq!(r, Ok(Reply::Acted(k.clone())));
            } else {
                assert_eq!(r, Err(Error::NoKey));
            }
        }
    }
}
