//! Kellnr's registry authorization (crates/registry/src/kellnr_api.rs) as a
//! pure kernel. `transition` is after PR #1243 (45043ee), `transition_pre1243`
//! before it (45043ee^); they differ only where the PR changed the code.
//!
//! Aeneas subset: no `?`, iterator adapters, or `String`. Names are ids.

/// How the request was authenticated. Kellnr builds `MaybeUser` differently
/// for each (crates/auth/src/maybe_user.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    Session,
    Token,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub user: u64,
    pub login: Login,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct User {
    pub id: u64,
    pub is_admin: bool,
    pub is_read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Krate {
    pub id: u64,
    pub restricted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub krate: u64,
    pub vers: u64,
    pub yanked: bool,
}

/// A row of a two-column relation: (crate, user) for owners and crate users,
/// (crate, group) for crate groups, (group, user) for group members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub a: u64,
    pub b: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub allow_ownerless_crates: bool,
    pub new_crates_restricted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub settings: Settings,
    pub users: Vec<User>,
    pub crates: Vec<Krate>,
    pub versions: Vec<Version>,
    pub owners: Vec<Pair>,
    pub crate_users: Vec<Pair>,
    pub crate_groups: Vec<Pair>,
    pub group_members: Vec<Pair>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    AddOwner(Pair),
    DelOwner(Pair),
    AddCrateUser(Pair),
    DelCrateUser(Pair),
    AddCrateGroup(Pair),
    DelCrateGroup(Pair),
    SetYanked(Version),
    AddCrate(Krate),
    AddVersion(Version),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    AddOwner { krate: u64, user: u64 },
    RemoveOwner { krate: u64, user: u64 },
    AddCrateUser { krate: u64, user: u64 },
    RemoveCrateUser { krate: u64, user: u64 },
    AddCrateGroup { krate: u64, group: u64 },
    RemoveCrateGroup { krate: u64, group: u64 },
    Yank { krate: u64, vers: u64 },
    Unyank { krate: u64, vers: u64 },
    Publish { krate: u64, vers: u64 },
    Download { krate: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Done,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthorized,
    ReadOnlyModify,
    NotOwner,
    LastOwner,
    CrateNotFound,
    CrateExists,
    NewCratesRestricted,
    DownloadUnauthorized,
    NotCrateUser,
}

/// Kellnr's `MaybeUser`: who acts, as the handler sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaybeUser {
    pub name: u64,
    pub is_admin: bool,
    pub is_read_only: bool,
}

pub fn find_user(users: &Vec<User>, id: u64) -> Option<User> {
    let mut i = 0;
    while i < users.len() {
        if users[i].id == id {
            return Some(users[i]);
        }
        i += 1;
    }
    None
}

pub fn find_crate(crates: &Vec<Krate>, id: u64) -> Option<Krate> {
    let mut i = 0;
    while i < crates.len() {
        if crates[i].id == id {
            return Some(crates[i]);
        }
        i += 1;
    }
    None
}

pub fn find_version(versions: &Vec<Version>, krate: u64, vers: u64) -> Option<Version> {
    let mut i = 0;
    while i < versions.len() {
        if versions[i].krate == krate && versions[i].vers == vers {
            return Some(versions[i]);
        }
        i += 1;
    }
    None
}

pub fn has_pair(v: &Vec<Pair>, a: u64, b: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].a == a && v[i].b == b {
            return true;
        }
        i += 1;
    }
    false
}

pub fn count_a(v: &Vec<Pair>, a: u64) -> u64 {
    let mut n = 0;
    let mut i = 0;
    while i < v.len() {
        if v[i].a == a {
            n += 1;
        }
        i += 1;
    }
    n
}

/// `db.is_crate_group_user`: some group granted on `krate` has `user`.
pub fn is_crate_group_user(s: &Snapshot, krate: u64, user: u64) -> bool {
    let mut i = 0;
    while i < s.crate_groups.len() {
        if s.crate_groups[i].a == krate && has_pair(&s.group_members, s.crate_groups[i].b, user) {
            return true;
        }
        i += 1;
    }
    false
}

/// `MaybeUser::from_session` and `from_token`. Before PR #1243 the session
/// path hardcoded `is_read_only: false` (maybe_user.rs, 45043ee^).
fn maybe_user(s: &Snapshot, p: &Principal, fixed: bool) -> Option<MaybeUser> {
    match find_user(&s.users, p.user) {
        None => None,
        Some(u) => {
            let read_only = match p.login {
                Login::Token => u.is_read_only,
                Login::Session => fixed && u.is_read_only,
            };
            Some(MaybeUser { name: u.id, is_admin: u.is_admin, is_read_only: read_only })
        }
    }
}

/// Token-only extractors (`token::Token`) reject session logins with 401.
fn token_user(s: &Snapshot, p: &Principal) -> Option<MaybeUser> {
    match p.login {
        Login::Session => None,
        Login::Token => maybe_user(s, p, true),
    }
}

/// kellnr_api.rs `check_ownership`.
pub fn check_ownership(s: &Snapshot, krate: u64, user: &MaybeUser) -> bool {
    user.is_admin || has_pair(&s.owners, krate, user.name)
}

/// kellnr_api.rs `check_can_modify`: fails when `!is_admin && is_read_only`.
/// Written in De Morgan form because Aeneas rejects the negated `&&`.
pub fn check_can_modify(user: &MaybeUser) -> bool {
    user.is_admin || !user.is_read_only
}

/// kellnr_api.rs `check_download_auth`. Download takes an optional token only.
fn check_download_auth(s: &Snapshot, krate: u64, p: &Principal) -> Result<(), Error> {
    let restricted = match find_crate(&s.crates, krate) {
        None => false,
        Some(k) => k.restricted,
    };
    if !restricted {
        return Ok(());
    }
    match token_user(s, p) {
        None => Err(Error::DownloadUnauthorized),
        Some(t) => {
            if t.is_admin
                || has_pair(&s.crate_users, krate, t.name)
                || is_crate_group_user(s, krate, t.name)
                || has_pair(&s.owners, krate, t.name)
            {
                Ok(())
            } else {
                Err(Error::NotCrateUser)
            }
        }
    }
}

fn one(w: Write) -> Vec<Write> {
    let mut v = Vec::new();
    v.push(w);
    v
}

/// Owner and ACL endpoints: `check_can_modify` (if `modify_check`) then
/// `check_ownership`.
fn guarded(s: &Snapshot, p: &Principal, fixed: bool, modify_check: bool, krate: u64) -> Result<MaybeUser, Error> {
    match maybe_user(s, p, fixed) {
        None => Err(Error::Unauthorized),
        Some(u) => {
            if modify_check && !check_can_modify(&u) {
                return Err(Error::ReadOnlyModify);
            }
            if !check_ownership(s, krate, &u) {
                return Err(Error::NotOwner);
            }
            Ok(u)
        }
    }
}

/// Same, for token-only endpoints (yank, unyank).
fn guarded_token(s: &Snapshot, p: &Principal, krate: u64) -> Result<MaybeUser, Error> {
    match token_user(s, p) {
        None => Err(Error::Unauthorized),
        Some(u) => {
            if !check_can_modify(&u) {
                return Err(Error::ReadOnlyModify);
            }
            if !check_ownership(s, krate, &u) {
                return Err(Error::NotOwner);
            }
            Ok(u)
        }
    }
}

fn run(p: &Principal, s: &Snapshot, cmd: &Command, fixed: bool) -> Result<(Vec<Write>, Reply), Error> {
    match cmd {
        Command::AddOwner { krate, user } => match guarded(s, p, fixed, true, *krate) {
            Err(e) => Err(e),
            Ok(_) => Ok((one(Write::AddOwner(Pair { a: *krate, b: *user })), Reply::Done)),
        },
        Command::RemoveOwner { krate, user } => match guarded(s, p, fixed, true, *krate) {
            Err(e) => Err(e),
            Ok(_) => {
                // remove_owner_single: never remove the last owner.
                if !s.settings.allow_ownerless_crates && count_a(&s.owners, *krate) <= 1 {
                    return Err(Error::LastOwner);
                }
                Ok((one(Write::DelOwner(Pair { a: *krate, b: *user })), Reply::Done))
            }
        },
        Command::AddCrateUser { krate, user } => match guarded(s, p, fixed, true, *krate) {
            Err(e) => Err(e),
            Ok(_) => Ok((one(Write::AddCrateUser(Pair { a: *krate, b: *user })), Reply::Done)),
        },
        Command::RemoveCrateUser { krate, user } => match guarded(s, p, fixed, true, *krate) {
            Err(e) => Err(e),
            Ok(_) => Ok((one(Write::DelCrateUser(Pair { a: *krate, b: *user })), Reply::Done)),
        },
        // Before PR #1243 the group endpoints skipped `check_can_modify`.
        Command::AddCrateGroup { krate, group } => match guarded(s, p, fixed, fixed, *krate) {
            Err(e) => Err(e),
            Ok(_) => {
                if has_pair(&s.crate_groups, *krate, *group) {
                    return Ok((Vec::new(), Reply::Done));
                }
                Ok((one(Write::AddCrateGroup(Pair { a: *krate, b: *group })), Reply::Done))
            }
        },
        Command::RemoveCrateGroup { krate, group } => match guarded(s, p, fixed, fixed, *krate) {
            Err(e) => Err(e),
            Ok(_) => Ok((one(Write::DelCrateGroup(Pair { a: *krate, b: *group })), Reply::Done)),
        },
        Command::Yank { krate, vers } => match guarded_token(s, p, *krate) {
            Err(e) => Err(e),
            Ok(_) => match find_version(&s.versions, *krate, *vers) {
                None => Err(Error::CrateNotFound),
                Some(v) => Ok((one(Write::SetYanked(Version { krate: v.krate, vers: v.vers, yanked: true })), Reply::Done)),
            },
        },
        Command::Unyank { krate, vers } => match guarded_token(s, p, *krate) {
            Err(e) => Err(e),
            Ok(_) => match find_version(&s.versions, *krate, *vers) {
                None => Err(Error::CrateNotFound),
                Some(v) => Ok((one(Write::SetYanked(Version { krate: v.krate, vers: v.vers, yanked: false })), Reply::Done)),
            },
        },
        Command::Publish { krate, vers } => match token_user(s, p) {
            None => Err(Error::Unauthorized),
            Some(u) => {
                if !check_can_modify(&u) {
                    return Err(Error::ReadOnlyModify);
                }
                let version = Version { krate: *krate, vers: *vers, yanked: false };
                match find_crate(&s.crates, *krate) {
                    Some(_) => {
                        if !check_ownership(s, *krate, &u) {
                            return Err(Error::NotOwner);
                        }
                        match find_version(&s.versions, *krate, *vers) {
                            Some(_) => Err(Error::CrateExists),
                            None => Ok((one(Write::AddVersion(version)), Reply::Done)),
                        }
                    }
                    None => {
                        if s.settings.new_crates_restricted && !u.is_admin {
                            return Err(Error::NewCratesRestricted);
                        }
                        // `db.add_crate` makes the publisher the first owner.
                        let mut ws = Vec::new();
                        ws.push(Write::AddCrate(Krate { id: *krate, restricted: false }));
                        ws.push(Write::AddVersion(version));
                        ws.push(Write::AddOwner(Pair { a: *krate, b: u.name }));
                        Ok((ws, Reply::Done))
                    }
                }
            }
        },
        Command::Download { krate } => match check_download_auth(s, *krate, p) {
            Err(e) => Err(e),
            Ok(()) => Ok((Vec::new(), Reply::File)),
        },
    }
}

/// Kellnr after PR #1243.
pub fn transition(p: &Principal, s: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    run(p, s, cmd, true)
}

/// Kellnr before PR #1243.
pub fn transition_pre1243(p: &Principal, s: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    run(p, s, cmd, false)
}

fn put_pair(v: &mut Vec<Pair>, x: Pair) {
    if !has_pair(v, x.a, x.b) {
        v.push(x);
    }
}

fn del_pair(v: &Vec<Pair>, x: Pair) -> Vec<Pair> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if !(v[i].a == x.a && v[i].b == x.b) {
            out.push(v[i]);
        }
        i += 1;
    }
    out
}

fn set_yanked(v: &mut Vec<Version>, x: Version) {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == x.krate && v[i].vers == x.vers {
            v[i] = x;
            return;
        }
        i += 1;
    }
}

/// What committing a write set means. Owner, crate-user and group rows are
/// added only if absent, so each pair is stored once.
pub fn apply(snap: &Snapshot, ws: &Vec<Write>) -> Snapshot {
    let mut s = snap.clone();
    let mut i = 0;
    while i < ws.len() {
        match ws[i] {
            Write::AddOwner(x) => put_pair(&mut s.owners, x),
            Write::DelOwner(x) => s.owners = del_pair(&s.owners, x),
            Write::AddCrateUser(x) => put_pair(&mut s.crate_users, x),
            Write::DelCrateUser(x) => s.crate_users = del_pair(&s.crate_users, x),
            Write::AddCrateGroup(x) => put_pair(&mut s.crate_groups, x),
            Write::DelCrateGroup(x) => s.crate_groups = del_pair(&s.crate_groups, x),
            Write::SetYanked(x) => set_yanked(&mut s.versions, x),
            Write::AddCrate(k) => s.crates.push(k),
            Write::AddVersion(v) => s.versions.push(v),
        }
        i += 1;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Snapshot {
        Snapshot {
            users: vec![
                User { id: 1, is_admin: false, is_read_only: true },
                User { id: 2, is_admin: false, is_read_only: false },
            ],
            crates: vec![Krate { id: 7, restricted: true }],
            versions: vec![Version { krate: 7, vers: 1, yanked: false }],
            owners: vec![Pair { a: 7, b: 1 }],
            ..Default::default()
        }
    }

    #[test]
    fn pr1243_session_bug() {
        let p = Principal { user: 1, login: Login::Session };
        let cmd = Command::AddOwner { krate: 7, user: 2 };
        assert!(transition_pre1243(&p, &state(), &cmd).is_ok());
        assert_eq!(transition(&p, &state(), &cmd), Err(Error::ReadOnlyModify));
    }

    #[test]
    fn pr1243_group_bug() {
        let p = Principal { user: 1, login: Login::Token };
        let cmd = Command::AddCrateGroup { krate: 7, group: 3 };
        assert!(transition_pre1243(&p, &state(), &cmd).is_ok());
        assert_eq!(transition(&p, &state(), &cmd), Err(Error::ReadOnlyModify));
    }

    #[test]
    fn apply_adds_owner_once() {
        let ws = vec![Write::AddOwner(Pair { a: 7, b: 1 }), Write::AddOwner(Pair { a: 7, b: 2 })];
        assert_eq!(apply(&state(), &ws).owners.len(), 2);
    }
}
