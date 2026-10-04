//! Documents behind inherited roles and path patterns.
//!
//! A user holds roles directly (`members`), and a role inherits the roles it
//! points to (`inherits`). A role may read or write the paths a pattern
//! covers: `docs/*` covers every path under `docs/`. Every helper that
//! upstream code would take from `std` (a map lookup, recursion over the
//! inheritance graph, `str::trim`, prefix matching) comes from
//! `h5i-app-std`, whose specs are proven once.

use h5i_app_std::{bytes, graph, map, set};

/// The wildcard at the end of a pattern: `*`.
pub const STAR: u8 = 42;

/// More direct roles or inheritance edges than this is refused, which keeps
/// the reachability search inside `usize`.
pub const LIMIT: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub id: u64,
    pub path: Vec<u8>,
    pub body: Vec<u8>,
}

pub struct Principal {
    pub user: u64,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    /// Each user's direct roles.
    pub members: Vec<(u64, Vec<u64>)>,
    /// `(role, inherited)`: holding `role` also gives `inherited`.
    pub inherits: Vec<(u64, u64)>,
    /// `(role, pattern)`: the role reads the paths `pattern` covers.
    pub readers: Vec<(u64, Vec<u8>)>,
    /// `(role, pattern)`: the role writes the paths `pattern` covers.
    pub writers: Vec<(u64, Vec<u8>)>,
    pub docs: Vec<Doc>,
}

pub enum Command {
    Get {
        id: u64,
    },
    Put {
        id: u64,
        path: Vec<u8>,
        body: Vec<u8>,
    },
    List,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Doc(Doc),
    Docs(Vec<Doc>),
    Done,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Forbidden,
    NotFound,
    TooLarge,
}

/// Every role `user` holds, directly or by inheritance; `None` past `LIMIT`.
pub fn roles(s: &Snapshot, user: u64) -> Option<Vec<u64>> {
    let direct = match map::get(&s.members, &user) {
        Some(rs) => rs,
        None => Vec::new(),
    };
    if direct.len() >= LIMIT || s.inherits.len() >= LIMIT {
        return None;
    }
    Some(graph::reachable(&s.inherits, &direct))
}

/// Some entry of `table` is for one of `roles` and covers `path`.
pub fn may(table: &Vec<(u64, Vec<u8>)>, roles: &Vec<u64>, path: &Vec<u8>) -> bool {
    let mut i = 0;
    while i < table.len() {
        if set::contains(roles, &table[i].0) && bytes::star_match(&table[i].1, path, STAR) {
            return true;
        }
        i += 1;
    }
    false
}

fn find_doc(docs: &Vec<Doc>, id: u64) -> Option<Doc> {
    let mut i = 0;
    while i < docs.len() {
        if docs[i].id == id {
            return Some(docs[i].clone());
        }
        i += 1;
    }
    None
}

/// The documents `roles` may read.
pub fn readable(readers: &Vec<(u64, Vec<u8>)>, roles: &Vec<u64>, docs: &Vec<Doc>) -> Vec<Doc> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < docs.len() {
        if may(readers, roles, &docs[i].path) {
            out.push(docs[i].clone());
        }
        i += 1;
    }
    out
}

pub fn transition(
    actor: &Principal,
    s: &Snapshot,
    cmd: &Command,
) -> Result<(Vec<Doc>, Reply), Error> {
    let rs = match roles(s, actor.user) {
        Some(rs) => rs,
        None => return Err(Error::TooLarge),
    };
    match cmd {
        Command::Get { id } => match find_doc(&s.docs, *id) {
            Some(d) => {
                if may(&s.readers, &rs, &d.path) {
                    Ok((Vec::new(), Reply::Doc(d)))
                } else {
                    Err(Error::Forbidden)
                }
            }
            None => Err(Error::NotFound),
        },
        Command::Put { id, path, body } => {
            let p = bytes::trim(path);
            if may(&s.writers, &rs, &p) {
                let mut ws = Vec::new();
                ws.push(Doc {
                    id: *id,
                    path: p,
                    body: body.clone(),
                });
                Ok((ws, Reply::Done))
            } else {
                Err(Error::Forbidden)
            }
        }
        Command::List => Ok((Vec::new(), Reply::Docs(readable(&s.readers, &rs, &s.docs)))),
    }
}

/// Commit a write set: each document replaces the one with its id, or is added.
pub fn apply(s: &Snapshot, ws: &Vec<Doc>) -> Snapshot {
    let mut out = s.clone();
    for w in ws {
        out.docs.retain(|d| d.id != w.id);
        out.docs.push(w.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    /// Alice is an editor; editors inherit viewer. Viewers read `docs/*`,
    /// editors write `docs/drafts/*`.
    fn snap() -> Snapshot {
        Snapshot {
            members: vec![(1, vec![10]), (2, vec![11])],
            inherits: vec![(10, 11)],
            readers: vec![(11, b("docs/*"))],
            writers: vec![(10, b("docs/drafts/*"))],
            docs: vec![
                Doc {
                    id: 1,
                    path: b("docs/a"),
                    body: b("x"),
                },
                Doc {
                    id: 2,
                    path: b("hr/pay"),
                    body: b("y"),
                },
            ],
        }
    }

    fn run(user: u64, s: &mut Snapshot, c: Command) -> Result<Reply, Error> {
        let (ws, r) = transition(&Principal { user }, s, &c)?;
        *s = apply(s, &ws);
        Ok(r)
    }

    #[test]
    fn inherited_read_and_patterns() {
        let mut s = snap();
        assert!(matches!(
            run(2, &mut s, Command::Get { id: 1 }),
            Ok(Reply::Doc(_))
        ));
        assert_eq!(
            run(2, &mut s, Command::Get { id: 2 }),
            Err(Error::Forbidden)
        );
        // Alice reads through the inherited viewer role.
        assert!(matches!(
            run(1, &mut s, Command::Get { id: 1 }),
            Ok(Reply::Doc(_))
        ));
        // The path is trimmed before the check and the write.
        assert_eq!(
            run(
                1,
                &mut s,
                Command::Put {
                    id: 3,
                    path: b(" docs/drafts/n "),
                    body: b("z")
                }
            ),
            Ok(Reply::Done)
        );
        assert_eq!(
            run(
                2,
                &mut s,
                Command::Put {
                    id: 4,
                    path: b("docs/drafts/m"),
                    body: b("z")
                }
            ),
            Err(Error::Forbidden)
        );
        assert_eq!(s.docs.last().unwrap().path, b("docs/drafts/n"));
        match run(9, &mut s, Command::List) {
            Ok(Reply::Docs(ds)) => assert!(ds.is_empty()),
            other => panic!("{other:?}"),
        }
        match run(1, &mut s, Command::List) {
            Ok(Reply::Docs(ds)) => assert_eq!(ds.len(), 2),
            other => panic!("{other:?}"),
        }
    }
}
